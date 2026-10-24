//! Idempotent application of a transcript plan to the local store.
//!
//! Every run reads the transcript from the session's stored cursor, plans the
//! closed responses in that window, skips the ones already recorded (by their
//! deterministic ids), and commits the new captures together with the advanced
//! cursor in one transaction. A run that fails changes nothing.
use super::links;
use super::session_lock::{SessionLock, SESSION_LOCK_WAIT};
use super::transcript::{self, ParsedLine, PlanInput, TranscriptError, HARNESS};
use crate::db::{CvcStore, DbError, HarnessCursorUpdate};
use crate::models::{Conversation, Interaction, InteractionId};
use crate::privacy::{self, ClaudeCodeCapture, PreparedPolicy};
use crate::repository::{RepositoryLayout, RepositoryLayoutError};
use chrono::Utc;
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

/// One read never buffers more than this; a transcript beyond it is treated as
/// corruption rather than an ingest target.
const MAX_READ_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DERIVED_TITLE_BYTES: usize = 80;
const DEFAULT_TITLE: &str = "Claude Code session";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestMode {
    /// Hook-driven mid-session read: only closed responses are recorded and
    /// a streaming or result-awaiting response stays pending.
    Incremental,
    /// Stop/SessionEnd or operator-driven read: every response is complete.
    Final,
}

#[derive(Debug, Error)]
pub enum IngestError {
    #[error(transparent)]
    Transcript(#[from] TranscriptError),
    #[error("transcript {0:?} is not a regular file")]
    NotRegularFile(PathBuf),
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    pub session_id: String,
    pub inserted: usize,
    pub already_present: usize,
    pub pending_responses: usize,
    /// Floating responses linked to a commit the session created, by exact
    /// transcript evidence rather than the time-window linker.
    pub linked_from_commits: usize,
    pub cursor: u64,
    pub transcript_len: u64,
    /// Subagent (Agent tool) transcripts ingested alongside this session, each
    /// its own conversation. Their inserts and links are summed here.
    pub subagent_sessions: usize,
    pub subagent_inserted: usize,
    pub subagent_linked: usize,
    /// Content blocks of a type this version does not record, in the
    /// responses this run newly saw. Each stays in the captured text as a
    /// bracketed placeholder naming the type, never its content.
    pub unrecorded_blocks: usize,
    /// The distinct block types behind `unrecorded_blocks`, sorted.
    pub unrecorded_block_types: Vec<String>,
}

/// One transcript file to ingest: the file, the session id every entry must
/// carry, and the conversation id under which its interactions are stored.
/// For the main session these coincide; a subagent file carries the parent
/// session id but is stored under its own per-agent conversation.
struct Target<'a> {
    transcript_path: &'a Path,
    file_session: &'a str,
    conversation_id: &'a str,
    include_sidechain: bool,
    title_override: Option<String>,
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
pub fn ingest(
    layout: &RepositoryLayout,
    store: &CvcStore,
    policy: &PreparedPolicy,
    transcript_path: &Path,
    session_id: Option<&str>,
    mode: IngestMode,
) -> Result<IngestReport, IngestError> {
    ingest_waiting(
        layout,
        store,
        policy,
        transcript_path,
        session_id,
        mode,
        SESSION_LOCK_WAIT,
    )
}

/// [`ingest`] with an explicit bound on the wait for the session's lock.
pub fn ingest_waiting(
    layout: &RepositoryLayout,
    store: &CvcStore,
    policy: &PreparedPolicy,
    transcript_path: &Path,
    session_id: Option<&str>,
    mode: IngestMode,
    lock_wait: Duration,
) -> Result<IngestReport, IngestError> {
    let session = session_id
        .map(str::to_owned)
        .or_else(|| {
            transcript_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned)
        })
        .ok_or(IngestError::UnknownSession)?;
    if !privacy::is_safe_identifier(&session) {
        return Err(IngestError::InvalidSession(session));
    }

    // Held until this function returns: the cursors of the session and of its
    // subagents are read and advanced under it.
    let _lock = match SessionLock::acquire(&layout.cvc_dir(), HARNESS, &session, lock_wait) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return Err(IngestError::SessionBusy {
                session,
                waited_secs: lock_wait.as_secs(),
            })
        }
        Err(source) => return Err(IngestError::SessionLock { session, source }),
    };

    let mut report = ingest_target(
        layout,
        store,
        policy,
        &Target {
            transcript_path,
            file_session: &session,
            conversation_id: &session,
            include_sidechain: false,
            title_override: None,
        },
        mode,
    )?;

    // Subagents run inside the same session and worktree; Claude Code stores
    // each in `<session>/subagents/agent-<id>.jsonl` beside the main file. Each
    // becomes its own conversation, keyed off the parent session so it never
    // collides with the parent or another agent.
    for subagent in discover_subagents(transcript_path)? {
        let conversation_id = format!("{session}:{}", subagent.agent_stem);
        if !privacy::is_safe_identifier(&conversation_id) {
            continue;
        }
        let target_report = ingest_target(
            layout,
            store,
            policy,
            &Target {
                transcript_path: &subagent.path,
                file_session: &session,
                conversation_id: &conversation_id,
                include_sidechain: true,
                title_override: subagent.title,
            },
            mode,
        )?;
        report.subagent_sessions += 1;
        report.subagent_inserted += target_report.inserted;
        report.subagent_linked += target_report.linked_from_commits;
        report.unrecorded_blocks += target_report.unrecorded_blocks;
        for kind in target_report.unrecorded_block_types {
            if !report.unrecorded_block_types.contains(&kind) {
                report.unrecorded_block_types.push(kind);
            }
        }
        report.unrecorded_block_types.sort();
    }

    Ok(report)
}

/// Ingests one transcript file into one conversation.
fn ingest_target(
    layout: &RepositoryLayout,
    store: &CvcStore,
    policy: &PreparedPolicy,
    target: &Target<'_>,
    mode: IngestMode,
) -> Result<IngestReport, IngestError> {
    let metadata = fs::symlink_metadata(target.transcript_path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(IngestError::NotRegularFile(
            target.transcript_path.to_path_buf(),
        ));
    }
    let worktree_root = layout.worktree_root()?.to_path_buf();
    let capture_worktree = layout.worktree_origin()?;

    let mut file = fs::File::open(target.transcript_path)?;
    let mut offset = store
        .harness_ingest_cursor(HARNESS, target.conversation_id)?
        .map(|cursor| cursor.byte_offset)
        .unwrap_or(0);
    if !cursor_is_consistent(&mut file, offset, metadata.len())? {
        // A rewritten or truncated transcript invalidates the cursor; the
        // deterministic ids make a full re-read harmless.
        offset = 0;
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_READ_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_READ_BYTES {
        return Err(IngestError::TooLarge);
    }
    let transcript_len = offset + bytes.len() as u64;

    let window = transcript::parse_window(
        &bytes,
        offset,
        target.file_session,
        target.include_sidechain,
    )?;
    let path_aliases = path_aliases(&window.lines, &worktree_root);
    let plan = transcript::plan(
        &window.lines,
        offset,
        &PlanInput {
            session_id: target.conversation_id,
            worktree_root: &worktree_root,
            path_aliases: &path_aliases,
            final_mode: mode == IngestMode::Final,
        },
    );

    let existing = store.get_conversation(target.conversation_id)?;
    let title = target
        .title_override
        .clone()
        .or_else(|| plan.title.clone())
        .or_else(|| {
            existing
                .as_ref()
                .map(|conversation| conversation.title.clone())
                .filter(|title| !title.trim().is_empty())
        })
        .or_else(|| plan.first_human_prompt.as_deref().and_then(derive_title))
        .unwrap_or_else(|| DEFAULT_TITLE.to_owned());
    let created_at = existing
        .as_ref()
        .map(|conversation| conversation.created_at)
        .or_else(|| plan.interactions.first().map(|planned| planned.timestamp))
        .unwrap_or_else(Utc::now);
    let conversation = Conversation {
        id: target.conversation_id.to_owned(),
        title,
        created_at,
    };

    let mut captures = Vec::new();
    let mut batch_ids: HashSet<InteractionId> = HashSet::new();
    let mut already_present = 0usize;
    let mut unrecorded_blocks = 0usize;
    let mut unrecorded_block_types: BTreeSet<String> = BTreeSet::new();
    let mut commit_bearing: Vec<links::CommitBearingResponse> = Vec::new();
    for planned in plan.interactions {
        if !planned.commit_candidates.is_empty() {
            commit_bearing.push(links::CommitBearingResponse {
                interaction_id: planned.id.clone(),
                timestamp: planned.timestamp,
                candidates: planned.commit_candidates.clone(),
            });
        }
        if store.interaction_exists(&planned.id)? {
            already_present += 1;
            continue;
        }
        unrecorded_blocks += planned.unrecorded_blocks.len();
        unrecorded_block_types.extend(planned.unrecorded_blocks.iter().cloned());
        // A parent outside this batch and outside the store (suppressed, or
        // never recorded) breaks the chain rather than failing the batch.
        let parent_id = match planned.parent_id {
            Some(parent) if batch_ids.contains(&parent) || store.interaction_exists(&parent)? => {
                Some(parent)
            }
            _ => None,
        };
        let interaction = Interaction {
            id: planned.id.clone(),
            conversation_id: target.conversation_id.to_owned(),
            parent_id,
            timestamp: planned.timestamp,
            author: planned.author,
            user_prompt: planned.user_prompt,
            model_name: planned.model_name,
            model_cot: planned.model_cot,
            model_response: planned.model_response,
            source_request_id: Some(planned.response_key),
        };
        batch_ids.insert(planned.id);
        captures.push(ClaudeCodeCapture::new(
            conversation.clone(),
            interaction,
            planned.context_items,
            planned.tool_executions,
            policy.clone(),
            capture_worktree.clone(),
        ));
    }

    let outcome = store.capture_claude_code_batch(
        captures,
        HarnessCursorUpdate {
            harness: HARNESS,
            session_id: target.conversation_id,
            transcript_path: &target.transcript_path.to_string_lossy(),
            byte_offset: plan.cursor_offset,
        },
    )?;

    // Rescue the responses the time-window linker cannot reach: those that
    // created a commit and the reasoning before them. Attribution records who
    // linked, matching the linker, not the historical commit author.
    let linked_by = layout
        .repository()
        .signature()
        .ok()
        .and_then(|signature| signature.email().map(str::to_owned));
    let linked_from_commits = links::link_transcript_commits(
        layout.repository(),
        store,
        target.conversation_id,
        &capture_worktree,
        &commit_bearing,
        linked_by.as_deref(),
    )?;

    Ok(IngestReport {
        session_id: target.conversation_id.to_owned(),
        inserted: outcome.inserted,
        already_present: already_present + outcome.suppressed,
        pending_responses: plan.pending_responses,
        linked_from_commits,
        cursor: plan.cursor_offset,
        transcript_len,
        subagent_sessions: 0,
        subagent_inserted: 0,
        subagent_linked: 0,
        unrecorded_blocks,
        unrecorded_block_types: unrecorded_block_types.into_iter().collect(),
    })
}

/// A subagent transcript discovered beside the main session file.
struct Subagent {
    path: PathBuf,
    /// The file stem, e.g. `agent-a0bd78e0`, used to form the conversation id.
    agent_stem: String,
    /// Title from the sibling `.meta.json`, if present.
    title: Option<String>,
}

/// Finds `<session>/subagents/agent-*.jsonl` beside `<session>.jsonl`. A missing
/// directory yields nothing; symlinks and non-files are ignored.
fn discover_subagents(transcript_path: &Path) -> Result<Vec<Subagent>, IngestError> {
    let directory = transcript_path.with_extension("").join("subagents");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut subagents = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
            || !stem.starts_with("agent-")
        {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let title = subagent_title(&path.with_extension("meta.json"));
        subagents.push(Subagent {
            path: path.clone(),
            agent_stem: stem.to_owned(),
            title,
        });
    }
    // Deterministic order so cursors and logs are stable.
    subagents.sort_by(|a, b| a.agent_stem.cmp(&b.agent_stem));
    Ok(subagents)
}

/// A short title from a subagent's `.meta.json` (`agentType`: `description`).
fn subagent_title(meta_path: &Path) -> Option<String> {
    let text = fs::read_to_string(meta_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let agent_type = value.get("agentType").and_then(|v| v.as_str());
    let description = value.get("description").and_then(|v| v.as_str());
    let combined = match (agent_type, description) {
        (Some(kind), Some(desc)) if !desc.trim().is_empty() => format!("{kind}: {desc}"),
        (Some(kind), _) => format!("subagent ({kind})"),
        (None, Some(desc)) if !desc.trim().is_empty() => desc.to_owned(),
        _ => return None,
    };
    Some(derive_title(&combined).unwrap_or(combined))
}

/// The stored cursor is trusted only when it still lands on a line boundary
/// of the current file.
fn cursor_is_consistent(file: &mut fs::File, offset: u64, len: u64) -> std::io::Result<bool> {
    if offset == 0 {
        return Ok(true);
    }
    if offset > len {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(offset - 1))?;
    let mut previous = [0u8; 1];
    file.read_exact(&mut previous)?;
    Ok(previous[0] == b'\n')
}

/// Transcript `cwd` spellings that resolve inside the worktree, so tool paths
/// written through a symlinked or non-canonical checkout path still attribute.
fn path_aliases(lines: &[ParsedLine], worktree_root: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut seen = BTreeSet::new();
    let mut aliases = Vec::new();
    for line in lines {
        let ParsedLine::Entry(entry) = line else {
            continue;
        };
        let Some(cwd) = entry.cwd.as_deref() else {
            continue;
        };
        if !seen.insert(cwd.to_owned()) {
            continue;
        }
        let spelled = PathBuf::from(cwd);
        if spelled == worktree_root {
            continue;
        }
        if let Ok(canonical) = fs::canonicalize(&spelled) {
            if canonical.starts_with(worktree_root) {
                aliases.push((spelled, canonical));
            }
        }
    }
    aliases
}

fn derive_title(prompt: &str) -> Option<String> {
    let line = prompt.lines().find(|line| !line.trim().is_empty())?.trim();
    if line.len() <= MAX_DERIVED_TITLE_BYTES {
        return Some(line.to_owned());
    }
    let mut end = MAX_DERIVED_TITLE_BYTES;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!("{}…", &line[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Repository;
    use std::time::Instant;
    use tempfile::TempDir;

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

        fn lock(&self, session: &str) -> SessionLock {
            SessionLock::acquire(&self.layout.cvc_dir(), HARNESS, session, SHORT)
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
            &PlanInput {
                session_id: SESSION,
                worktree_root: root,
                path_aliases: &[],
                final_mode: true,
            },
        );
        plan.interactions[0].id.clone()
    }
}
