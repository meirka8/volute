//! Claude Code transcript ingestion: the 0.7.x-compatible entry points.
//!
//! Every run reads the transcript from the session's stored cursor, plans the
//! closed responses in that window, skips the ones already recorded (by their
//! deterministic ids), and commits the new captures together with the advanced
//! cursor in one transaction. A run that fails changes nothing.
//!
//! A session and its subagents are one family: each transcript is ingested into
//! its own conversation, and once all of them are, one rescue links the
//! family's floating responses to the commits any of them created (see
//! [`super::links`]).
//!
//! All of it runs on the shared harness engine with the Claude Code adapter
//! ([`super::Adapter`]). These entry points keep 0.7.x's contract: the
//! transcript is opened once, sniffed and read through the same handle, and
//! the capture decision is the caller's. They are deprecated:
//! [`crate::harness::engine::ingest`] with the same adapter is the gated
//! path, which asks for coverage itself, and is what cvc's own CLI runs.
use super::adapter::{Adapter, Source};
use super::transcript::{self, FirstEntry, TranscriptError, FIRST_ENTRY_BYTES};
use crate::db::{CvcStore, DbError, HARNESS_REASONS_SHOWN};
use crate::harness::adapter::{PayloadError, RecordSource, Sniff};
use crate::harness::candidate::Finality;
use crate::harness::cursor::JsonlCursor;
use crate::harness::engine::{self, EngineError, EngineOptions, EngineReport};
use crate::harness::{HarnessId, SESSION_LOCK_WAIT};
use crate::privacy::{PreparedPolicy, PrivacyError};
use crate::repository::{RepositoryLayout, RepositoryLayoutError};
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestMode {
    /// Hook-driven mid-session read: only closed responses are recorded and
    /// a streaming or result-awaiting response stays pending.
    Incremental,
    /// Stop/SessionEnd or operator-driven read: every response is complete.
    Final,
}

/// How one ingest run differs from the default hook-driven read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestOptions {
    /// Ignore the saved cursors of the session and its subagents and read every
    /// transcript from byte 0. Recorded responses are skipped by their
    /// deterministic ids (a locally deleted one stays deleted), and the commit
    /// links of every commit in the transcripts are derived again, which repairs
    /// a session captured before its subagents' responses were linked. A rescan
    /// never moves a cursor backwards.
    pub rescan: bool,
    /// How long to wait for the session's lock.
    pub lock_wait: Duration,
}

impl Default for IngestOptions {
    fn default() -> Self {
        Self {
            rescan: false,
            lock_wait: SESSION_LOCK_WAIT,
        }
    }
}

#[derive(Debug, Error)]
pub enum IngestError {
    #[error(transparent)]
    Transcript(#[from] TranscriptError),
    #[error("transcript {0:?} is not a regular file")]
    NotRegularFile(PathBuf),
    /// The transcript's first complete entry is in another agent's format
    /// (see [`transcript::is_other_agents_entry`]): the file is that agent's.
    /// Unlike [`TranscriptError`], which is a Claude Code transcript this
    /// parser does not support or a damaged one, nothing in it is read past
    /// that entry.
    #[error("not a Claude Code transcript: {0:?} begins with an entry in another agent's format")]
    NotClaudeCode(PathBuf),
    #[error("transcript exceeds the supported size")]
    TooLarge,
    #[error("session id {0:?} is not a safe identifier")]
    InvalidSession(String),
    #[error("transcript file name does not name a session; pass the session id explicitly")]
    UnknownSession,
    #[error(transparent)]
    Layout(#[from] RepositoryLayoutError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("transcript I/O: {0}")]
    Io(#[from] std::io::Error),
    /// The per-session lock could not be taken for a reason other than another
    /// run holding it (the CVC directory is a link or someone else's, say).
    #[error("cannot lock Claude Code session {session} for ingestion: {source}")]
    SessionLock {
        session: String,
        source: std::io::Error,
    },
    /// Another run was still ingesting this session when the wait for it ended.
    /// That run is making progress, so nothing is lost: whatever it does not
    /// reach, the next ingest of the session picks up from the cursor.
    #[error("another cvc process is still ingesting Claude Code session {session} and did not finish within {waited_secs} s; run this again once it has")]
    SessionBusy { session: String, waited_secs: u64 },
    /// A hook payload this adapter cannot use.
    #[error(transparent)]
    Payload(#[from] PayloadError),
    /// A way of locating sessions Claude Code's adapter does not offer.
    #[error("{0} is not supported for claude-code; pass --transcript")]
    Unsupported(&'static str),
    /// The gated path only ([`crate::harness::engine::ingest`]): the capture
    /// acknowledgement does not cover Claude Code here, or its state could
    /// not be read.
    #[error(transparent)]
    Privacy(#[from] PrivacyError),
    /// The engine refused an adapter that is not the registered one.
    #[error("this adapter is not the one registered for {0}; it records nothing")]
    Unregistered(HarnessId),
}

impl From<IngestMode> for Finality {
    fn from(mode: IngestMode) -> Self {
        match mode {
            IngestMode::Incremental => Finality::Incremental,
            IngestMode::Final => Finality::Final,
        }
    }
}

/// The engine's failures in this module's terms: a busy or unusable lock is
/// the session's, as in 0.7.x.
impl From<EngineError<IngestError>> for IngestError {
    fn from(error: EngineError<IngestError>) -> Self {
        match error {
            EngineError::Adapter(error) => error,
            EngineError::Db(error) => IngestError::Db(error),
            EngineError::Layout(error) => IngestError::Layout(error),
            EngineError::Privacy(error) => IngestError::Privacy(error),
            EngineError::Busy {
                family_root,
                waited,
                ..
            } => IngestError::SessionBusy {
                session: family_root,
                waited_secs: waited.as_secs(),
            },
            EngineError::Lock {
                family_root,
                source,
                ..
            } => IngestError::SessionLock {
                session: family_root,
                source,
            },
            EngineError::Unregistered(harness) => IngestError::Unregistered(harness),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    pub session_id: String,
    pub inserted: usize,
    pub already_present: usize,
    pub pending_responses: usize,
    /// Floating responses of the session's own conversation linked to a commit
    /// the session family created, by exact transcript evidence rather than the
    /// time-window linker.
    pub linked_from_commits: usize,
    pub cursor: u64,
    pub transcript_len: u64,
    /// Subagent (Agent tool) transcripts ingested alongside this session, each
    /// its own conversation. Their inserts are summed here, and `subagent_linked`
    /// counts the links made to their responses by the family's commit rescue.
    pub subagent_sessions: usize,
    pub subagent_inserted: usize,
    pub subagent_linked: usize,
    /// Content blocks of a type this version does not record, in the
    /// responses this run newly saw. Each stays in the captured text as a
    /// bracketed placeholder naming the type, never its content.
    pub unrecorded_blocks: usize,
    /// The distinct block types behind `unrecorded_blocks`, sorted.
    pub unrecorded_block_types: Vec<String>,
    /// Of `inserted`, the responses recorded as a stand-in because their capture
    /// could not be scrubbed within the limits: the stand-in keeps the stimulus,
    /// says its response was withheld, and holds no tool calls or context items.
    /// Without it, one such response would keep the cursor from moving past it.
    pub withheld: usize,
    /// Responses that could not be recorded at all, not even as a stand-in
    /// (a conversation id or a title the capture boundary refuses). They are
    /// skipped so that the responses after them are recorded.
    pub lost: usize,
    /// Why responses were withheld or lost: the limit that was hit, each once,
    /// never any of the text.
    pub withheld_reasons: Vec<String>,
    /// Tool calls a response made beyond the 255 it records, in the responses
    /// this run newly saw. A response that left calls or context items out ends
    /// with a `cvc-omitted` record that says how many.
    pub omitted_tool_calls: usize,
    /// Context items (worktree paths a call read or edited) a response left out:
    /// past the 256 it records, or a path that could not be scrubbed.
    pub omitted_context_items: usize,
}

/// A run's report, in 0.7.x's figures: the session's own, with its
/// subagents' summed.
impl From<EngineReport<JsonlCursor>> for IngestReport {
    fn from(run: EngineReport<JsonlCursor>) -> Self {
        let root = run.root;
        let mut report = IngestReport {
            session_id: root.conversation_id,
            inserted: root.inserted,
            already_present: root.already_present,
            pending_responses: root.pending,
            linked_from_commits: run.linked.root,
            cursor: root.cursor.byte_offset,
            transcript_len: root.record_len.unwrap_or(0),
            subagent_sessions: 0,
            subagent_inserted: 0,
            subagent_linked: run.linked.members,
            unrecorded_blocks: root.unrecorded.len(),
            unrecorded_block_types: root
                .unrecorded
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            withheld: root.withheld,
            lost: root.lost,
            withheld_reasons: root.withheld_reasons,
            omitted_tool_calls: root.omitted_tool_calls,
            omitted_context_items: root.omitted_context_items,
        };
        for member in run.members {
            report.subagent_sessions += 1;
            report.subagent_inserted += member.inserted;
            report.unrecorded_blocks += member.unrecorded.len();
            for kind in member.unrecorded {
                if !report.unrecorded_block_types.contains(&kind) {
                    report.unrecorded_block_types.push(kind);
                }
            }
            report.unrecorded_block_types.sort();
            report.withheld += member.withheld;
            report.lost += member.lost;
            for reason in member.withheld_reasons {
                if report.withheld_reasons.len() < HARNESS_REASONS_SHOWN
                    && !report.withheld_reasons.contains(&reason)
                {
                    report.withheld_reasons.push(reason);
                }
            }
            report.omitted_tool_calls += member.omitted_tool_calls;
            report.omitted_context_items += member.omitted_context_items;
        }
        report
    }
}

/// Ingests `transcript_path` for the worktree described by `layout`, then any
/// subagent transcripts stored beside it.
///
/// `session_id` defaults to the transcript's file stem, which is how Claude
/// Code names session files; every entry must agree with it. The policy is the
/// exact `.thoughtignore` snapshot handed to persistence.
///
/// Runs on the same session in the same repository are serialized by a lock in
/// the repository's CVC directory, held from reading the cursor to committing
/// the advanced one, subagent transcripts included. A run that finds it held
/// waits up to [`SESSION_LOCK_WAIT`], then fails with
/// [`IngestError::SessionBusy`].
#[deprecated(note = "use harness::engine::ingest")]
#[allow(deprecated)]
pub fn ingest(
    layout: &RepositoryLayout,
    store: &CvcStore,
    policy: &PreparedPolicy,
    transcript_path: &Path,
    session_id: Option<&str>,
    mode: IngestMode,
) -> Result<IngestReport, IngestError> {
    ingest_with(
        layout,
        store,
        policy,
        transcript_path,
        session_id,
        mode,
        IngestOptions::default(),
    )
}

/// [`ingest`] with an explicit bound on the wait for the session's lock.
#[deprecated(note = "use harness::engine::ingest")]
#[allow(deprecated)]
pub fn ingest_waiting(
    layout: &RepositoryLayout,
    store: &CvcStore,
    policy: &PreparedPolicy,
    transcript_path: &Path,
    session_id: Option<&str>,
    mode: IngestMode,
    lock_wait: Duration,
) -> Result<IngestReport, IngestError> {
    ingest_with(
        layout,
        store,
        policy,
        transcript_path,
        session_id,
        mode,
        IngestOptions {
            lock_wait,
            ..IngestOptions::default()
        },
    )
}

/// [`ingest`] with [`IngestOptions`].
///
/// The session and its subagents are ingested first, the session before the
/// subagents, each into its own conversation. Then one rescue links the family's
/// floating responses to the commits that any of those transcripts created (see
/// [`super::links::link_transcript_commits`]). If a subagent fails, the rescue still
/// runs over what was ingested, the session included, because the session's
/// cursor is already past its committing response and that response would
/// never be read again; the subagent's error is then returned.
#[deprecated(note = "use harness::engine::ingest")]
pub fn ingest_with(
    layout: &RepositoryLayout,
    store: &CvcStore,
    policy: &PreparedPolicy,
    transcript_path: &Path,
    session_id: Option<&str>,
    mode: IngestMode,
    options: IngestOptions,
) -> Result<IngestReport, IngestError> {
    let adapter = Adapter;
    // Opened once: this handle is sniffed, then read.
    let source = Source::open(transcript_path);
    // Before anything else, the lock included, so that another agent's file
    // leaves nothing behind. A transcript that cannot be opened or read here
    // is reported by the read, as before.
    if adapter.sniff(&source) == Sniff::NotOurs {
        return Err(IngestError::NotClaudeCode(transcript_path.to_path_buf()));
    }
    let located = adapter.locate_opened(source, session_id)?;
    let report = engine::ingest_claude_code_caller_decided(
        &adapter,
        layout,
        store,
        policy,
        located,
        EngineOptions {
            lock_wait: options.lock_wait,
            ..EngineOptions::operator(mode.into(), options.rescan)
        },
    )?;
    Ok(report.into())
}

/// Classifies the first complete entry of the transcript at `path` (see
/// [`transcript::first_entry`]), reading from its start only up to its first
/// non-blank line, and never more than [`FIRST_ENTRY_BYTES`] + 1 bytes.
///
/// The file is opened once, and what was opened is inspected: on Unix without
/// following a link, without blocking (a FIFO opens at once instead of waiting
/// for a writer) and without becoming a controlling terminal, then refused
/// unless the handle is a regular file; elsewhere, a link or anything but a
/// regular file at `path` is refused before it is opened. A refusal is
/// [`IngestError::NotRegularFile`], the error ingestion itself gives such a
/// path; any other failure to open or read is [`IngestError::Io`].
pub fn sniff_first_entry(path: &Path) -> Result<FirstEntry, IngestError> {
    let file = open_transcript_start(path)?;
    let start = read_to_first_entry(file)?;
    Ok(transcript::first_entry(&start))
}

#[cfg(unix)]
pub(crate) fn open_transcript_start(path: &Path) -> Result<fs::File, IngestError> {
    use std::os::unix::fs::OpenOptionsExt;
    let opened = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path);
    let file = match opened {
        Ok(file) => file,
        // `O_NOFOLLOW` refuses a link as the last component with `ELOOP`,
        // which a loop of links earlier in the path also gives: only the
        // first is a link where the transcript belongs.
        Err(error)
            if error.raw_os_error() == Some(libc::ELOOP)
                && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink()) =>
        {
            return Err(IngestError::NotRegularFile(path.to_path_buf()))
        }
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        return Err(IngestError::NotRegularFile(path.to_path_buf()));
    }
    Ok(file)
}

#[cfg(not(unix))]
pub(crate) fn open_transcript_start(path: &Path) -> Result<fs::File, IngestError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(IngestError::NotRegularFile(path.to_path_buf()));
    }
    Ok(fs::File::open(path)?)
}

/// The start of `source`, up to and including its first non-blank complete
/// line, or up to [`FIRST_ENTRY_BYTES`] + 1 bytes, or to its end, whichever
/// comes first: what [`transcript::first_entry`] needs, and no more.
pub(crate) fn read_to_first_entry(source: impl Read) -> std::io::Result<Vec<u8>> {
    let mut reader = BufReader::new(source.take(FIRST_ENTRY_BYTES as u64 + 1));
    let mut start = Vec::new();
    loop {
        let line_start = start.len();
        if reader.read_until(b'\n', &mut start)? == 0 {
            return Ok(start);
        }
        let line = &start[line_start..];
        let complete = line.ends_with(b"\n");
        if !complete || !line.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(start);
        }
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use crate::harness::lock::FamilyLock;
    use crate::harness::HarnessId;
    use crate::models::InteractionId;
    use git2::Repository;
    use std::time::Instant;
    use tempfile::TempDir;

    const HARNESS: &str = transcript::HARNESS;

    const BASIC: &str = include_str!("../../tests/fixtures/claude-code/v2.1.260/basic.jsonl");
    const SESSION: &str = "7c1e9f5a-2b2f-4f4e-9c3b-0f2e7a1d5b60";
    const SHORT: Duration = Duration::from_millis(100);

    struct Fixture {
        _temp: TempDir,
        layout: RepositoryLayout,
        store: CvcStore,
        policy: PreparedPolicy,
        transcript: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            Self::build(true)
        }

        /// `on_disk`: the store is the repository's own, in its CVC directory;
        /// otherwise it is in memory and the repository has no CVC directory.
        fn build(on_disk: bool) -> Self {
            let temp = TempDir::new().unwrap();
            Repository::init(temp.path()).unwrap();
            let layout = RepositoryLayout::discover(temp.path()).unwrap();
            let root = layout.worktree_root().unwrap().to_path_buf();
            let store = if on_disk {
                CvcStore::open(layout.db_path()).unwrap()
            } else {
                CvcStore::open(":memory:").unwrap()
            };
            let policy = PreparedPolicy::load(&root).unwrap();
            let transcript = root.join(format!("{SESSION}.jsonl"));
            fs::write(
                &transcript,
                BASIC.replace("__WORKTREE__", root.to_str().unwrap()),
            )
            .unwrap();
            Self {
                _temp: temp,
                layout,
                store,
                policy,
                transcript,
            }
        }

        fn ingest(&self, wait: Duration) -> Result<IngestReport, IngestError> {
            ingest_waiting(
                &self.layout,
                &self.store,
                &self.policy,
                &self.transcript,
                None,
                IngestMode::Final,
                wait,
            )
        }

        fn lock(&self, session: &str) -> FamilyLock {
            FamilyLock::acquire(
                &self.layout.cvc_dir(),
                HarnessId::ClaudeCode.name(),
                session,
                SHORT,
            )
            .unwrap()
            .expect("nothing else holds it")
        }

        fn cursor(&self) -> Option<u64> {
            self.store
                .harness_ingest_cursor(HARNESS, SESSION)
                .unwrap()
                .map(|cursor| cursor.byte_offset)
        }
    }

    /// A run that cannot get the session's lock in time records nothing, says
    /// which session and for how long, and leaves the cursor where it was: the
    /// holder is making progress and the next ingest resumes from the cursor.
    #[test]
    fn a_run_that_waits_in_vain_for_the_sessions_lock_changes_nothing() {
        let fixture = Fixture::new();
        let held = fixture.lock(SESSION);

        let started = Instant::now();
        let error = fixture.ingest(SHORT).unwrap_err();
        assert!(started.elapsed() >= SHORT, "it waited for its time");
        match &error {
            IngestError::SessionBusy {
                session,
                waited_secs,
            } => {
                assert_eq!(session, SESSION);
                assert_eq!(*waited_secs, 0, "a sub-second wait reads as 0 s");
            }
            other => panic!("{other:?}"),
        }
        let text = error.to_string();
        assert!(
            text.contains(SESSION) && text.contains("another cvc process"),
            "{text}"
        );
        assert_eq!(fixture.cursor(), None);
        assert!(!fixture
            .store
            .interaction_exists(&first_id(&fixture))
            .unwrap());

        // Released, the same call goes through and records the session.
        drop(held);
        let report = fixture.ingest(SHORT).unwrap();
        assert_eq!(report.inserted, 7);
        assert!(fixture.cursor().is_some());
    }

    #[test]
    fn the_wait_is_for_the_same_session_only() {
        let fixture = Fixture::new();
        let _other = fixture.lock("another-session");
        let report = fixture.ingest(SHORT).unwrap();
        assert_eq!(report.inserted, 7);
    }

    /// The default wait is below the hooks' own timeout (30 s), so a waiting
    /// hook gives up on its terms rather than being killed mid-wait.
    #[test]
    fn the_default_wait_ends_before_the_hooks_timeout() {
        assert!(SESSION_LOCK_WAIT < Duration::from_secs(super::super::settings::HOOK_TIMEOUT_SECS));
        assert!(SESSION_LOCK_WAIT >= Duration::from_secs(10));
    }

    #[test]
    fn a_lock_that_cannot_be_taken_for_another_reason_is_a_different_error() {
        let fixture = Fixture::build(false);
        assert!(!fixture.layout.cvc_dir().exists());
        let error = fixture.ingest(SHORT).unwrap_err();
        assert!(
            matches!(error, IngestError::SessionLock { ref session, .. } if session == SESSION),
            "{error:?}"
        );
    }

    /// The id of the first interaction the fixture's transcript produces.
    fn first_id(fixture: &Fixture) -> InteractionId {
        let bytes = fs::read(&fixture.transcript).unwrap();
        let window = transcript::parse_window(&bytes, 0, SESSION, false).unwrap();
        let root = fixture.layout.worktree_root().unwrap();
        let plan = transcript::plan(
            &window.lines,
            0,
            &transcript::PlanInput {
                session_id: SESSION,
                worktree_root: root,
                path_aliases: &[],
                final_mode: true,
                subagent: false,
            },
        );
        plan.interactions[0].id.clone()
    }

    /// A reader that counts what is taken from it.
    struct Counting<R> {
        inner: R,
        taken: u64,
    }

    impl<R: Read> Read for Counting<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let count = self.inner.read(buffer)?;
            self.taken += count as u64;
            Ok(count)
        }
    }

    #[test]
    fn the_sniff_reads_to_the_first_non_blank_line_and_no_further() {
        let first = b"\n\r\n{\"type\":\"ai-title\"}\n";
        let mut source = Counting {
            inner: first.chain(std::io::repeat(b'x').take(8 * 1024 * 1024)),
            taken: 0,
        };
        let start = read_to_first_entry(&mut source).unwrap();
        assert_eq!(start, first);
        // One buffer's worth past the line, not the bound.
        assert!(source.taken <= 64 * 1024, "{}", source.taken);
        assert_eq!(transcript::first_entry(&start), FirstEntry::ClaudeCode);

        // Without a line end, the bound and one byte more.
        let mut source = Counting {
            inner: std::io::repeat(b'x').take(8 * 1024 * 1024),
            taken: 0,
        };
        let start = read_to_first_entry(&mut source).unwrap();
        assert_eq!(start.len(), FIRST_ENTRY_BYTES + 1);
        assert_eq!(source.taken, FIRST_ENTRY_BYTES as u64 + 1);
        assert_eq!(transcript::first_entry(&start), FirstEntry::Undetermined);

        // A short file is read to its end.
        assert_eq!(read_to_first_entry(&b"\n{\"ty"[..]).unwrap(), b"\n{\"ty");
        assert_eq!(read_to_first_entry(&b""[..]).unwrap(), b"");
    }

    #[cfg(unix)]
    #[test]
    fn the_sniff_follows_no_link_and_waits_for_no_writer() {
        let temp = TempDir::new().unwrap();
        let cursor = temp.path().join("cursor.jsonl");
        fs::write(
            &cursor,
            "{\"role\":\"user\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();
        assert_eq!(
            sniff_first_entry(&cursor).unwrap(),
            FirstEntry::NotClaudeCode
        );

        let refused = |path: &Path| match sniff_first_entry(path) {
            Err(IngestError::NotRegularFile(named)) => assert_eq!(named, path),
            other => panic!("{path:?}: {other:?}"),
        };
        let link = temp.path().join("link.jsonl");
        std::os::unix::fs::symlink(&cursor, &link).unwrap();
        refused(&link);

        let fifo = temp.path().join("fifo.jsonl");
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: `name` is a valid, NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        refused(&fifo);
        assert!(started.elapsed() < Duration::from_secs(5));

        refused(temp.path());
        // A failure to open is not a refusal: ingestion reports it.
        assert!(matches!(
            sniff_first_entry(&temp.path().join("absent")),
            Err(IngestError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
        ));
        // Nor is a loop of links before the last component.
        let looped = temp.path().join("loop");
        std::os::unix::fs::symlink(&looped, &looped).unwrap();
        assert!(matches!(
            sniff_first_entry(&looped.join("x.jsonl")),
            Err(IngestError::Io(_))
        ));
    }
}
