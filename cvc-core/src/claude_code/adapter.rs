//! Claude Code on the harness registry: what is particular to Claude Code,
//! and nothing else. The consent check, the lock, scrub-then-bound, the
//! cursor rules and the commit rescue are the shared ones
//! ([`crate::harness::engine`]).
//!
//! * The record is the session's JSONL transcript, opened once
//!   ([`Source::open`]): the same handle is sniffed and read, so the file
//!   read is the file that was checked.
//! * The cursor is a [`JsonlCursor`]. Claude Code reads where to resume from
//!   its 0.7.x row (`harness_ingest_cursors`), exactly as 0.7.x does, and
//!   writes that row in every batch; the typed row is its copy, which never
//!   moves backwards within an epoch. Two things mean the transcript was
//!   rewritten, and the cursor that commits opens the next epoch: the 0.7.x
//!   row is behind its typed copy (reading resumes from the 0.7.x offset,
//!   as 0.7.x would), or the 0.7.x offset no longer lands on a line
//!   boundary (the transcript is then read again from its start; the
//!   deterministic ids make that harmless).
//! * A typed row of another codec kind or version (a newer cvc wrote it) is
//!   left as it is: the batch writes the 0.7.x row alone, so the session's
//!   cursor keeps advancing.
//! * The family is the session and the subagent transcripts beside it,
//!   `<session>/subagents/agent-<id>.jsonl`, each in the conversation
//!   `<session>:agent-<id>`. No relation rows are written: the relation is
//!   the id.
//! * The planner is [`super::transcript::plan`], unchanged; its output
//!   converts losslessly into candidates.
use super::ingest::{open_transcript_start, read_to_first_entry, IngestError};
use super::settings;
use super::transcript::{self, FirstEntry, ParsedLine, TitleSource, HARNESS};
use crate::db::{CvcStore, DbError, StoredHarnessCursor};
use crate::harness::adapter::{
    json_type, Admission, CallerVerdict, FamilyRead, HarnessStatus, HookCaller, HookEnv, HookEvent,
    HookRequest, InstallAction, InstallContext, InstallOutcome, InstallScope, Installer,
    LegacyCursor, Located, OutputContract, PayloadError, Planner, Read, ReadContext, RecordSource,
    Resume, Sniff, StartLocation, UninstallOutcome,
};
use crate::harness::candidate::{
    CaptureCandidate, ConversationRole, Finality, Plan, PlanInput, TitleHint, TitleRank,
};
use crate::harness::cursor::{CursorCodec, EpochReason, JsonlCursor};
use crate::harness::drift::{Drift, DriftRule};
use crate::harness::family::Family;
use crate::harness::HarnessId;
use crate::privacy;
use crate::repository::RepositoryLayout;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One read never buffers more than this; a transcript beyond it is treated as
/// corruption rather than an ingest target.
const MAX_READ_BYTES: u64 = 512 * 1024 * 1024;
/// The most of a subagent's `.meta.json` that is read. Claude Code writes a
/// few hundred bytes there; a larger file gives no title seed.
const MAX_META_BYTES: u64 = 64 * 1024;

/// Claude Code exports this to every hook it runs: the directory the session
/// was started in.
pub const PROJECT_DIR_ENV: &str = "CLAUDE_PROJECT_DIR";
/// Cursor exports this to the hooks it runs. Claude Code run in Cursor's
/// integrated terminal, and so its hooks, may inherit it too.
pub const CURSOR_VERSION_ENV: &str = "CURSOR_VERSION";
/// The top-level key every Cursor hook input carries and Claude Code does not
/// send.
const CURSOR_VERSION_KEY: &str = "cursor_version";
/// Cursor's conversation id: Cursor's only in a payload without Claude Code's
/// `session_id`.
const CURSOR_CONVERSATION_KEY: &str = "conversation_id";
/// Top-level keys of Antigravity's hook input that Claude Code does not send.
/// An explicit list, never a rule such as "any camelCase key", which a future
/// Claude Code key could match, silently ending all capture.
const ANTIGRAVITY_PAYLOAD_KEYS: [&str; 6] = [
    "hookEventName",
    "toolCall",
    "lastUserInput",
    "conversationId",
    "transcriptPath",
    "workspacePaths",
];
/// What follows a `consent-required` message for Claude Code.
pub const STOP_HINT: &str = "(or stop the hooks: `/plugin disable cvc@volute` for the Claude Code plugin, `cvc harness uninstall claude-code` for this checkout's hooks)";
/// How long a hook run may take, from the process's start: below the hooks'
/// own timeout ([`settings::HOOK_TIMEOUT_SECS`]).
const HOOK_BUDGET: Duration = Duration::from_secs(20);

/// The Claude Code adapter.
#[derive(Debug, Default, Clone, Copy)]
pub struct Adapter;

/// A Claude Code transcript to read.
#[derive(Debug)]
pub struct Source {
    /// As located, never canonicalised: the 0.7.x row records it so.
    path: PathBuf,
    /// The session id every entry must carry: the session's own, also in a
    /// subagent's file.
    file_session: String,
    /// A subagent transcript is made entirely of sidechain entries.
    include_sidechain: bool,
    handle: Handle,
}

#[derive(Debug)]
enum Handle {
    Open(fs::File),
    /// Opening failed; the error is reported by the first read.
    Failed(Option<IngestError>),
    /// Opened when it is first read (a subagent's file).
    Unopened,
}

impl Source {
    /// Opens the transcript at `path` once: on Unix without following a
    /// link, without blocking and without becoming a controlling terminal,
    /// and refused unless the handle is a regular file. A failure is kept
    /// and reported by the first read, where 0.7.x reported it.
    pub fn open(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            file_session: String::new(),
            include_sidechain: false,
            handle: match open_transcript_start(path) {
                Ok(file) => Handle::Open(file),
                Err(error) => Handle::Failed(Some(error)),
            },
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A path that holds something other than a regular file (a link, a
    /// FIFO, a directory) is refused when it is opened, before anything else
    /// is decided about it: the error the open recorded is returned here
    /// rather than by the first read. Any other failure to open is still
    /// left to the read.
    fn refuse_non_regular(&mut self) -> Result<(), IngestError> {
        if let Handle::Failed(error @ Some(IngestError::NotRegularFile(_))) = &mut self.handle {
            return Err(error.take().expect("matched above"));
        }
        Ok(())
    }

    fn file(&mut self) -> Result<&mut fs::File, IngestError> {
        if matches!(self.handle, Handle::Unopened) {
            self.handle = match open_transcript_start(&self.path) {
                Ok(file) => Handle::Open(file),
                Err(error) => Handle::Failed(Some(error)),
            };
        }
        match &mut self.handle {
            Handle::Open(file) => Ok(file),
            Handle::Failed(error) => Err(error.take().unwrap_or_else(|| {
                IngestError::Io(std::io::Error::other("the transcript could not be opened"))
            })),
            Handle::Unopened => unreachable!("opened above"),
        }
    }
}

/// One read of a transcript.
#[derive(Debug)]
pub struct Window {
    lines: transcript::Window,
    /// The byte offset the read started at.
    start: u64,
    /// Transcript `cwd` spellings that resolve inside the worktree.
    aliases: Vec<(PathBuf, PathBuf)>,
    /// The cursor the read resumed from.
    from: JsonlCursor,
}

impl Adapter {
    /// The located root of an opened transcript: the session is `session_id`
    /// or the file's stem, which is how Claude Code names session files, and
    /// every entry must carry it.
    pub fn locate_opened(
        &self,
        mut source: Source,
        session_id: Option<&str>,
    ) -> Result<Located<Source>, IngestError> {
        let session = session_id
            .map(str::to_owned)
            .or_else(|| {
                source
                    .path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned)
            })
            .ok_or(IngestError::UnknownSession)?;
        if !privacy::is_safe_identifier(&session) {
            return Err(IngestError::InvalidSession(session));
        }
        source.file_session = session.clone();
        Ok(Located {
            conversation_id: session.clone(),
            family_root: session,
            role: ConversationRole::Root,
            source,
            title_seed: None,
        })
    }
}

/// The keys of Claude Code's hook payload the adapter reads, each a string
/// or null when present, in the order a wrong one is reported.
const HOOK_PAYLOAD_KEYS: [&str; 5] = [
    "session_id",
    "transcript_path",
    "cwd",
    "hook_event_name",
    "agent_id",
];

impl HookCaller for Adapter {
    /// `cursor_version` alone means Cursor. `conversation_id` means Cursor only
    /// in a payload without `session_id`, so that a key of that name Claude
    /// Code may one day add never silences its own runs. `CLAUDE_PROJECT_DIR`
    /// is not looked at: Cursor sets it too.
    fn classify(&self, payload: &Map<String, Value>, env: &dyn HookEnv) -> CallerVerdict {
        if payload.contains_key(CURSOR_VERSION_KEY)
            || (payload.contains_key(CURSOR_CONVERSATION_KEY)
                && !payload.contains_key("session_id"))
        {
            return CallerVerdict::Foreign(Some(HarnessId::Cursor));
        }
        if ANTIGRAVITY_PAYLOAD_KEYS
            .iter()
            .any(|key| payload.contains_key(*key))
        {
            return CallerVerdict::Foreign(Some(HarnessId::Antigravity));
        }
        let cursor_env = env
            .var_os(CURSOR_VERSION_ENV)
            .is_some_and(|value| !value.is_empty());
        CallerVerdict::Ours {
            foreign_env: cursor_env.then_some(HarnessId::Cursor),
        }
    }

    /// Every key it reads must be a string or null where present. A wrong
    /// one is reported by its name and its JSON type, never its value.
    fn request(&self, mut payload: Map<String, Value>) -> Result<HookRequest, PayloadError> {
        let mut text = Vec::with_capacity(HOOK_PAYLOAD_KEYS.len());
        for key in HOOK_PAYLOAD_KEYS {
            text.push(match payload.remove(key) {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) => Some(text),
                Some(other) => {
                    return Err(PayloadError::Malformed {
                        harness: "Claude Code",
                        detail: format!("`{key}` is {}, not a string", json_type(&other)),
                    })
                }
            });
        }
        let [session, record, cwd, event, subagent]: [Option<String>; 5] =
            text.try_into().expect("one value per key");
        let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
        Ok(HookRequest {
            event,
            session: non_empty(session),
            record: non_empty(record).map(PathBuf::from),
            cwd: non_empty(cwd).map(PathBuf::from),
            subagent,
            roots: Vec::new(),
            extra: Map::new(),
        })
    }

    /// Claude Code always sends both; the session's id first, as 0.7.x
    /// reported it.
    fn check_complete(&self, request: &HookRequest) -> Result<(), PayloadError> {
        if request.session.is_none() {
            return Err(PayloadError::Missing("session_id"));
        }
        if request.record.is_none() {
            return Err(PayloadError::Missing("transcript_path"));
        }
        Ok(())
    }

    /// The directory the session was started in. Unset (an older Claude
    /// Code, a manual run): not known. Set and empty: the session was not
    /// started in any repository that can be named.
    fn start(&self, _request: &HookRequest, env: &dyn HookEnv) -> StartLocation {
        match env.var_os(PROJECT_DIR_ENV) {
            None => StartLocation::Unknown,
            Some(directory) if directory.is_empty() => StartLocation::NotNameable,
            Some(directory) => StartLocation::Directory(PathBuf::from(directory)),
        }
    }

    /// Subagent hooks receive the parent session's transcript path; the
    /// parent session's own hooks ingest it.
    fn wants(&self, request: &HookRequest) -> bool {
        request.subagent.is_none()
    }

    fn finality(&self, event: Option<&str>) -> Finality {
        match event {
            Some("Stop") | Some("SessionEnd") => Finality::Final,
            _ => Finality::Incremental,
        }
    }

    fn budget(&self, _event: Option<&str>) -> Duration {
        HOOK_BUDGET
    }

    fn contract(&self, _event: Option<&str>) -> OutputContract {
        OutputContract::Silent
    }

    fn stop_hint(&self) -> &'static str {
        STOP_HINT
    }
}

impl RecordSource for Adapter {
    const ID: HarnessId = HarnessId::ClaudeCode;
    type Source = Source;
    type Cursor = JsonlCursor;
    type Window = Window;
    type Error = IngestError;

    fn locate_hook(
        &self,
        request: &HookRequest,
        _layout: &RepositoryLayout,
    ) -> Result<Located<Source>, IngestError> {
        let session = request
            .session
            .as_deref()
            .ok_or(PayloadError::Missing("session_id"))?;
        let record = request
            .record
            .as_deref()
            .ok_or(PayloadError::Missing("transcript_path"))?;
        self.locate_file(record, Some(session))
    }

    fn locate_file(&self, path: &Path, id: Option<&str>) -> Result<Located<Source>, IngestError> {
        self.locate_opened(Source::open(path), id)
    }

    fn locate_session(
        &self,
        _id: &str,
        _layout: &RepositoryLayout,
    ) -> Result<Located<Source>, IngestError> {
        Err(IngestError::Unsupported("--session without --transcript"))
    }

    fn scan(&self, _layout: &RepositoryLayout) -> Result<Vec<Located<Source>>, IngestError> {
        Err(IngestError::Unsupported("--scan"))
    }

    /// A session is found by its transcript only.
    fn locates_sessions(&self) -> bool {
        false
    }

    fn scans(&self) -> bool {
        false
    }

    /// The first complete entry of the opened transcript (see
    /// [`transcript::first_entry`]). A transcript that could not be opened or
    /// read is undetermined here, and its read reports why.
    fn sniff(&self, source: &Source) -> Sniff {
        let Handle::Open(file) = &source.handle else {
            return Sniff::Undetermined;
        };
        let mut file = file;
        if file.seek(SeekFrom::Start(0)).is_err() {
            return Sniff::Undetermined;
        }
        match read_to_first_entry(file).map(|start| transcript::first_entry(&start)) {
            Ok(FirstEntry::ClaudeCode) => Sniff::Ours,
            Ok(FirstEntry::NotClaudeCode) => Sniff::NotOurs,
            Ok(FirstEntry::NoCompleteEntry) => Sniff::NotYet,
            Ok(FirstEntry::Undetermined) | Err(_) => Sniff::Undetermined,
        }
    }

    /// The transcript is opened once, here: what is not a regular file is
    /// refused, then its first complete entry decides (see [`hook_verdict`]),
    /// and only then is the session id checked. The same handle is read by
    /// the engine.
    fn admit_hook(
        &self,
        request: &HookRequest,
        _layout: &RepositoryLayout,
        foreign_env: Option<HarnessId>,
    ) -> Result<Admission<Source>, IngestError> {
        let session = request
            .session
            .as_deref()
            .ok_or(PayloadError::Missing("session_id"))?;
        let record = request
            .record
            .as_deref()
            .ok_or(PayloadError::Missing("transcript_path"))?;
        let mut source = Source::open(record);
        source.refuse_non_regular()?;
        match hook_verdict(self.sniff(&source), foreign_env.is_some(), record, session) {
            HookVerdict::Ingest => {}
            HookVerdict::PassOver => return Ok(Admission::PassOver),
            HookVerdict::Refuse => return Err(IngestError::NotClaudeCode(record.to_path_buf())),
        }
        Ok(Admission::Ingest(
            self.locate_opened(source, Some(session))?,
        ))
    }

    /// As [`Self::admit_hook`], for an operator: another agent's transcript
    /// is always refused, and nothing is passed over.
    fn admit_file(&self, path: &Path, id: Option<&str>) -> Result<Located<Source>, IngestError> {
        let mut source = Source::open(path);
        source.refuse_non_regular()?;
        if self.sniff(&source) == Sniff::NotOurs {
            return Err(IngestError::NotClaudeCode(path.to_path_buf()));
        }
        self.locate_opened(source, id)
    }

    /// Claude Code's own transcript is the record: a hook event adds nothing
    /// to it, so nothing is written before the engine.
    fn record_event(
        &self,
        _request: &HookRequest,
        _event: &HookEvent<'_>,
    ) -> Result<(), IngestError> {
        Ok(())
    }

    fn initial_cursor(&self, located: &Located<Source>) -> JsonlCursor {
        JsonlCursor::start(located.source.path.to_string_lossy())
    }

    /// The offset is the 0.7.x row's, as 0.7.x reads it; the epoch and the
    /// rest are the typed copy's. A 0.7.x row behind its typed copy was moved
    /// back for a rewritten transcript (by an older binary, or by the mirror
    /// rule): the cursor opens the next epoch.
    fn resume(
        &self,
        located: &Located<Source>,
        stored: Option<&StoredHarnessCursor>,
        store: &CvcStore,
    ) -> Result<Resume<JsonlCursor>, DbError> {
        let saved = store
            .harness_ingest_cursor(HARNESS, &located.conversation_id)?
            .map(|cursor| cursor.byte_offset)
            .unwrap_or(0);
        let path = located.source.path.to_string_lossy().into_owned();
        let Some(stored) = stored else {
            return Ok(Resume {
                from: JsonlCursor::start(path).at(saved),
                replaced: None,
            });
        };
        Ok(match JsonlCursor::decode_stored(stored) {
            Ok(typed) => Resume {
                replaced: (saved < typed.byte_offset).then_some(EpochReason::Rewritten),
                from: JsonlCursor {
                    path,
                    byte_offset: saved,
                    ..typed
                },
            },
            // Another codec's row, which the store keeps as it is.
            Err(_) => Resume {
                from: JsonlCursor {
                    epoch: stored.epoch,
                    ..JsonlCursor::start(path).at(saved)
                },
                replaced: None,
            },
        })
    }

    fn read(
        &self,
        source: &mut Source,
        from: &JsonlCursor,
        context: &ReadContext<'_>,
    ) -> Result<Read<Window, JsonlCursor>, IngestError> {
        let file_session = source.file_session.clone();
        let include_sidechain = source.include_sidechain;
        let file = source.file()?;
        let len = file.metadata()?.len();
        let saved = from.byte_offset;
        // A rewritten or truncated transcript invalidates the cursor; the
        // deterministic ids make a full re-read harmless.
        let trusted = if cursor_is_consistent(file, saved, len)? {
            saved
        } else {
            0
        };
        // A rescan reads it all again regardless: the recorded responses are
        // skipped by id, and the commits they reported are collected again.
        let offset = if context.rescan { 0 } else { trusted };
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(MAX_READ_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_READ_BYTES {
            return Err(IngestError::TooLarge);
        }
        let record_len = offset + bytes.len() as u64;
        let lines = transcript::parse_window(&bytes, offset, &file_session, include_sidechain)?;
        let aliases = path_aliases(&lines.lines, context.worktree_root);
        Ok(Read {
            window: Window {
                lines,
                start: offset,
                aliases,
                from: from.clone(),
            },
            trusted: from.at(trusted),
            replaced: (trusted != saved).then_some(EpochReason::Rewritten),
            record_len: Some(record_len),
            // A Claude Code transcript holds the session's own history only.
            own_history_start: None,
        })
    }

    /// The subagent transcripts beside the session's, each its own
    /// conversation keyed off the session so it never collides with the
    /// session or another agent. Every one found is in the family before any
    /// is read, so one that fails (or that is not reached) is still part of
    /// the family whose responses a commit claims; only the ones found beside
    /// this session's transcript are, never another session's.
    fn family(
        &self,
        root: &Located<Source>,
        _store: &CvcStore,
    ) -> Result<FamilyRead<Source>, IngestError> {
        let session = &root.conversation_id;
        let mut family = Family::new(HarnessId::ClaudeCode, session.clone());
        let mut members = Vec::new();
        for subagent in discover_subagents(&root.source.path)? {
            let conversation_id = format!("{session}:{}", subagent.agent_stem);
            if !privacy::is_safe_identifier(&conversation_id) {
                continue;
            }
            family.add_member(conversation_id.clone());
            members.push(Located {
                conversation_id,
                family_root: session.clone(),
                role: ConversationRole::Subagent,
                source: Source {
                    path: subagent.path,
                    file_session: session.clone(),
                    include_sidechain: true,
                    handle: Handle::Unopened,
                },
                title_seed: subagent.title_seed,
            });
        }
        Ok(FamilyRead { family, members })
    }

    fn legacy_cursor(&self, cursor: &JsonlCursor) -> Option<LegacyCursor> {
        Some(LegacyCursor {
            transcript_path: cursor.path.clone(),
            byte_offset: cursor.byte_offset,
        })
    }
}

impl Planner for Adapter {
    const DRIFT: DriftRule = transcript::DRIFT;
    const DEFAULT_TITLE: &'static str = "Claude Code session";

    fn plan(&self, window: &Window, input: &PlanInput<'_>) -> Plan<JsonlCursor> {
        let planned = transcript::plan(
            &window.lines.lines,
            window.start,
            &transcript::PlanInput {
                session_id: input.conversation_id,
                worktree_root: input.worktree_root,
                path_aliases: &window.aliases,
                final_mode: input.finality == Finality::Final,
                subagent: input.role == ConversationRole::Subagent,
            },
        );
        // The planner already prefers a title the person set.
        let user_set = window.lines.lines.iter().any(|line| {
            matches!(
                line,
                ParsedLine::Title {
                    source: TitleSource::Custom,
                    ..
                }
            )
        });
        let candidates: Vec<CaptureCandidate> = planned
            .interactions
            .into_iter()
            .map(CaptureCandidate::from)
            .collect();
        let mut drift = Drift::default();
        for candidate in &candidates {
            for kind in &candidate.unrecorded {
                drift.count_placeholder(Self::DRIFT.placeholders, kind);
            }
        }
        Plan {
            candidates,
            pending: planned.pending_responses,
            resume: window.from.at(planned.cursor_offset),
            title: planned.title.map(|text| TitleHint {
                text,
                rank: if user_set {
                    TitleRank::UserSet
                } else {
                    TitleRank::Generated
                },
            }),
            first_prompt: planned.first_human_prompt,
            children: Vec::new(),
            parent: None,
            drift,
        }
    }
}

impl Installer for Adapter {
    fn scopes(&self) -> &'static [InstallScope] {
        &[InstallScope::Checkout]
    }

    fn install(
        &self,
        context: &InstallContext<'_>,
        scope: InstallScope,
        binary: &Path,
    ) -> anyhow::Result<InstallOutcome> {
        only_checkout(scope)?;
        let outcome = settings::install(context.layout, binary)?;
        let mut warnings = Vec::new();
        if outcome.exclude == settings::ExcludeAction::NotIgnored {
            warnings.push(format!(
                "Git does not ignore {}; it holds a machine-specific path and must not be committed",
                settings::SETTINGS_RELATIVE_PATH
            ));
        }
        Ok(InstallOutcome {
            scope,
            location: outcome.settings_path,
            action: match outcome.action {
                settings::InstallAction::Created => InstallAction::Created,
                settings::InstallAction::Updated => InstallAction::Updated,
                settings::InstallAction::AlreadyPresent => InstallAction::AlreadyPresent,
            },
            warnings,
        })
    }

    fn uninstall(
        &self,
        context: &InstallContext<'_>,
        scope: InstallScope,
    ) -> anyhow::Result<UninstallOutcome> {
        only_checkout(scope)?;
        let outcome = settings::uninstall(context.layout)?;
        Ok(UninstallOutcome {
            scope,
            location: outcome.settings_path,
            removed: outcome.removed,
            deleted_file: outcome.deleted_file,
        })
    }

    fn status(&self, context: &InstallContext<'_>) -> anyhow::Result<HarnessStatus> {
        let mut status = HarnessStatus::default();
        if settings::installed(context.layout) {
            status.installed.push(InstallScope::Checkout);
        }
        match super::harness_record::read_record(context.layout) {
            Ok(binary) => {
                if let Some(problem) = binary.as_deref().and_then(settings::binary_problem) {
                    status.problems.push(problem);
                }
                status.binary = binary;
            }
            Err(error) => status.problems.push(error.to_string()),
        }
        Ok(status)
    }
}

/// What a hook run does with a transcript, given how it begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookVerdict {
    /// Ingest it: the parser's own checks report whatever they find.
    Ingest,
    /// Do nothing, in silence, without opening the store.
    PassOver,
    /// Fail with [`IngestError::NotClaudeCode`], without opening the store.
    Refuse,
}

/// What a hook run does with `transcript`, for `session`, given that it
/// begins as `first` says (#189).
///
/// A hook passes another agent's transcript over in silence only when
/// something besides its first line says the run is not Claude Code's own:
/// `CURSOR_VERSION` is set (`cursor_env`), or the file is not named for the
/// session, as Claude Code names its transcripts `<session_id>.jsonl`.
/// Otherwise it refuses it, visibly, so that a foreign-shaped line put at the
/// top of a session's own transcript cannot silence that session. Under
/// Cursor, a hook also passes over a transcript with no complete entry yet,
/// which the next event reads. Everything else is ingested, so that the
/// parser's own checks (the version pinning among them) report what they
/// find.
fn hook_verdict(first: Sniff, cursor_env: bool, transcript: &Path, session: &str) -> HookVerdict {
    match first {
        Sniff::NotOurs if cursor_env || !named_for_session(transcript, session) => {
            HookVerdict::PassOver
        }
        Sniff::NotOurs => HookVerdict::Refuse,
        Sniff::NotYet if cursor_env => HookVerdict::PassOver,
        Sniff::Ours | Sniff::NotYet | Sniff::Undetermined => HookVerdict::Ingest,
    }
}

/// Whether `transcript` is named `<session>.<extension>`, as Claude Code names
/// a session's transcript.
fn named_for_session(transcript: &Path, session: &str) -> bool {
    transcript.file_stem() == Some(std::ffi::OsStr::new(session))
}

fn only_checkout(scope: InstallScope) -> anyhow::Result<()> {
    if scope != InstallScope::Checkout {
        anyhow::bail!("Claude Code's hooks are installed per checkout only");
    }
    Ok(())
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

/// A subagent transcript discovered beside the main session file.
struct Subagent {
    path: PathBuf,
    /// The file stem, e.g. `agent-a0bd78e0`, used to form the conversation id.
    agent_stem: String,
    /// What the sibling `.meta.json` says the subagent is for, raw.
    title_seed: Option<String>,
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
        let title_seed = subagent_title_seed(&path.with_extension("meta.json"));
        subagents.push(Subagent {
            path: path.clone(),
            agent_stem: stem.to_owned(),
            title_seed,
        });
    }
    // Deterministic order so cursors and logs are stable.
    subagents.sort_by(|a, b| a.agent_stem.cmp(&b.agent_stem));
    Ok(subagents)
}

/// What a subagent's `.meta.json` says it is (`agentType`: `description`), from
/// which its title is derived.
///
/// The file is read the way its transcript is: opened once, on Unix without
/// following a link, without blocking and without becoming a controlling
/// terminal, used only when the handle is a regular file, and read to at
/// most [`MAX_META_BYTES`]. Anything else (no file, a link, a FIFO, a file
/// over the bound, text that is not UTF-8 or not the expected JSON) gives no
/// title seed, and the subagent is ingested all the same.
fn subagent_title_seed(meta_path: &Path) -> Option<String> {
    let file = open_transcript_start(meta_path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_META_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_META_BYTES {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let agent_type = value.get("agentType").and_then(|v| v.as_str());
    let description = value.get("description").and_then(|v| v.as_str());
    match (agent_type, description) {
        (Some(kind), Some(desc)) if !desc.trim().is_empty() => Some(format!("{kind}: {desc}")),
        (Some(kind), _) => Some(format!("subagent ({kind})")),
        (None, Some(desc)) if !desc.trim().is_empty() => Some(desc.to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, OsString> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), OsString::from(value)))
            .collect()
    }

    fn payload(json: &str) -> Map<String, Value> {
        serde_json::from_str(json).unwrap()
    }

    /// The same vectors as the CLI's own classification (#189).
    #[test]
    fn callers_are_classified_by_top_level_keys_and_the_environment() {
        let adapter = Adapter;
        let none = env(&[]);
        let ours = CallerVerdict::Ours { foreign_env: None };
        assert_eq!(
            adapter.classify(
                &payload(r#"{"session_id":"s","transcript_path":"/t"}"#),
                &none
            ),
            ours
        );
        assert_eq!(
            adapter.classify(
                &payload(r#"{"session_id":"s","cursor_version":"1"}"#),
                &none
            ),
            CallerVerdict::Foreign(Some(HarnessId::Cursor))
        );
        assert_eq!(
            adapter.classify(&payload(r#"{"conversation_id":"c"}"#), &none),
            CallerVerdict::Foreign(Some(HarnessId::Cursor))
        );
        // Claude Code's own session_id keeps a conversation_id from silencing it.
        assert_eq!(
            adapter.classify(
                &payload(r#"{"session_id":"s","conversation_id":"c"}"#),
                &none
            ),
            ours
        );
        for key in ANTIGRAVITY_PAYLOAD_KEYS {
            assert_eq!(
                adapter.classify(
                    &payload(&format!(r#"{{"session_id":"s","{key}":1}}"#)),
                    &none
                ),
                CallerVerdict::Foreign(Some(HarnessId::Antigravity)),
                "{key}"
            );
        }
        // Nested keys never count.
        assert_eq!(
            adapter.classify(&payload(r#"{"tool_input":{"cursor_version":"1"}}"#), &none),
            ours
        );
        // CURSOR_VERSION only silences errors; it never makes a run foreign.
        assert_eq!(
            adapter.classify(
                &payload(r#"{"session_id":"s"}"#),
                &env(&[("CURSOR_VERSION", "2")])
            ),
            CallerVerdict::Ours {
                foreign_env: Some(HarnessId::Cursor)
            }
        );
        assert_eq!(
            adapter.classify(
                &payload(r#"{"session_id":"s"}"#),
                &env(&[("CURSOR_VERSION", "")])
            ),
            ours
        );
    }

    #[test]
    fn a_hook_request_is_read_as_the_cli_reads_it() {
        let adapter = Adapter;
        let request = adapter
            .request(payload(
                r#"{"session_id":"s1","transcript_path":"/p/s1.jsonl","cwd":"","hook_event_name":"Stop","extra":1}"#,
            ))
            .unwrap();
        assert_eq!(request.session.as_deref(), Some("s1"));
        assert_eq!(request.record.as_deref(), Some(Path::new("/p/s1.jsonl")));
        assert_eq!(request.cwd, None);
        assert!(adapter.wants(&request));
        assert_eq!(adapter.finality(request.event.as_deref()), Finality::Final);
        assert_eq!(adapter.finality(Some("PostToolUse")), Finality::Incremental);
        assert_eq!(adapter.contract(Some("Stop")), OutputContract::Silent);
        assert!(adapter.budget(None) < Duration::from_secs(settings::HOOK_TIMEOUT_SECS));

        let subagent = adapter
            .request(payload(
                r#"{"agent_id":"a1","transcript_path":"/p/s1.jsonl"}"#,
            ))
            .unwrap();
        assert!(!adapter.wants(&subagent));
        // What the decoder said is kept, as 0.7.x printed it after the
        // context.
        assert_eq!(
            adapter.request(payload(r#"{"session_id":7}"#)),
            Err(PayloadError::Malformed {
                harness: "Claude Code",
                detail: "`session_id` is an integer, not a string".into()
            })
        );
        assert_eq!(
            adapter
                .request(payload(r#"{"session_id":7}"#))
                .unwrap_err()
                .to_string(),
            "hook payload is not the JSON Claude Code delivers on stdin: `session_id` is an integer, not a string"
        );
        // The value itself never reaches the message, whatever its type.
        for (json, kind) in [
            (r#"{"cwd":{"secret-marker":1}}"#, "`cwd` is an object"),
            (
                r#"{"transcript_path":["secret-marker"]}"#,
                "`transcript_path` is an array",
            ),
            (
                r#"{"hook_event_name":true}"#,
                "`hook_event_name` is a boolean",
            ),
            (r#"{"agent_id":1.5}"#, "`agent_id` is a number"),
        ] {
            let text = adapter.request(payload(json)).unwrap_err().to_string();
            assert!(text.ends_with(&format!("{kind}, not a string")), "{text}");
            assert!(!text.contains("secret-marker"), "{text}");
        }
        // Null is an absent value, as serde read it.
        let request = adapter
            .request(payload(r#"{"session_id":null,"agent_id":null}"#))
            .unwrap();
        assert_eq!(request.session, None);
        assert!(adapter.wants(&request));
        assert_eq!(
            PayloadError::NotTheShape("Claude Code").to_string(),
            "hook payload is not the JSON Claude Code delivers on stdin"
        );
        assert_eq!(
            PayloadError::Missing("session_id").to_string(),
            "hook payload has no session_id"
        );
    }

    /// A session id and a transcript path, in that order, as 0.7.x reported
    /// their absence; and no payload key reaches `extra`, since Claude Code
    /// allow-lists none.
    #[test]
    fn a_hook_request_needs_a_session_and_a_transcript_and_keeps_no_other_key() {
        let adapter = Adapter;
        let complete = |json: &str| {
            let request = crate::harness::adapter::hook_request(&adapter, payload(json)).unwrap();
            assert!(request.extra.is_empty(), "{json}");
            adapter.check_complete(&request)
        };
        assert_eq!(
            complete(r#"{"session_id":"s","transcript_path":"/t","user_email":"x"}"#),
            Ok(())
        );
        for (json, missing) in [
            (r#"{"transcript_path":"/t"}"#, "session_id"),
            (r#"{"session_id":"","transcript_path":"/t"}"#, "session_id"),
            (r#"{}"#, "session_id"),
            (r#"{"session_id":"s"}"#, "transcript_path"),
            (
                r#"{"session_id":"s","transcript_path":""}"#,
                "transcript_path",
            ),
        ] {
            assert_eq!(
                complete(json),
                Err(PayloadError::Missing(missing)),
                "{json}"
            );
        }
    }

    /// #189's rule, moved here from the CLI with its vectors.
    #[test]
    fn a_hook_passes_another_agents_transcript_over_only_when_corroborated() {
        const SESSION_ID: &str = "00000000-0000-4000-8000-0000000000a1";
        let own = Path::new("/x/00000000-0000-4000-8000-0000000000a1.jsonl");
        let elsewhere =
            Path::new("/x/agent-transcripts/00000000-0000-4000-8000-0000000000c1.jsonl");
        let verdict = |cursor_env, first, path| hook_verdict(first, cursor_env, path, SESSION_ID);

        // Corroborated: the variable, or a file not named for the session.
        assert_eq!(
            verdict(false, Sniff::NotOurs, elsewhere),
            HookVerdict::PassOver
        );
        assert_eq!(
            verdict(true, Sniff::NotOurs, elsewhere),
            HookVerdict::PassOver
        );
        assert_eq!(verdict(true, Sniff::NotOurs, own), HookVerdict::PassOver);
        // The session's own file, with nothing else to go by: visible.
        assert_eq!(verdict(false, Sniff::NotOurs, own), HookVerdict::Refuse);
        assert_eq!(
            verdict(
                false,
                Sniff::NotOurs,
                Path::new("/x/00000000-0000-4000-8000-0000000000a1")
            ),
            HookVerdict::Refuse,
            "a stem with no extension"
        );
        for cursor_env in [false, true] {
            for path in [own, elsewhere] {
                // A Claude Code transcript, and one only the parser can judge,
                // reach the parser, whose version pinning must stay loud.
                assert_eq!(verdict(cursor_env, Sniff::Ours, path), HookVerdict::Ingest);
                assert_eq!(
                    verdict(cursor_env, Sniff::Undetermined, path),
                    HookVerdict::Ingest
                );
            }
        }
        assert_eq!(verdict(false, Sniff::NotYet, own), HookVerdict::Ingest);
        assert_eq!(verdict(true, Sniff::NotYet, own), HookVerdict::PassOver);
    }

    /// The transcript is opened once and screened before anything is read
    /// past its first entry: a path that is not a regular file is refused,
    /// another agent's transcript is refused for an operator and, for a
    /// hook, passed over only when corroborated; the session id is checked
    /// last.
    #[cfg(unix)]
    #[test]
    fn records_are_screened_before_the_store_is_opened() {
        let temp = tempfile::TempDir::new().unwrap();
        let session = "00000000-0000-4000-8000-0000000000a1";
        let own = temp.path().join(format!("{session}.jsonl"));
        let cursor_line = "{\"role\":\"user\",\"message\":{\"content\":\"hi\"}}\n";
        fs::write(&own, cursor_line).unwrap();
        let adapter = Adapter;
        let layout_dir = tempfile::TempDir::new().unwrap();
        git2::Repository::init(layout_dir.path()).unwrap();
        let layout = RepositoryLayout::discover(layout_dir.path()).unwrap();
        let request = |record: &Path, session: &str| HookRequest {
            session: Some(session.into()),
            record: Some(record.to_path_buf()),
            ..HookRequest::default()
        };

        assert!(matches!(
            adapter.admit_file(&own, None),
            Err(IngestError::NotClaudeCode(path)) if path == own
        ));
        assert!(matches!(
            adapter.admit_hook(&request(&own, session), &layout, None),
            Err(IngestError::NotClaudeCode(_))
        ));
        assert!(matches!(
            adapter.admit_hook(&request(&own, session), &layout, Some(HarnessId::Cursor)),
            Ok(Admission::PassOver)
        ));
        // Passed over before the session id is looked at.
        assert!(matches!(
            adapter.admit_hook(&request(&own, "not safe"), &layout, Some(HarnessId::Cursor)),
            Ok(Admission::PassOver)
        ));

        let link = temp.path().join("link.jsonl");
        std::os::unix::fs::symlink(&own, &link).unwrap();
        for result in [
            adapter.admit_file(&link, None).map(drop),
            adapter
                .admit_hook(&request(&link, session), &layout, Some(HarnessId::Cursor))
                .map(drop),
        ] {
            assert!(
                matches!(&result, Err(IngestError::NotRegularFile(path)) if *path == link),
                "{result:?}"
            );
        }

        // A Claude Code transcript is located, with its session checked.
        let claude = temp.path().join("claude.jsonl");
        fs::write(&claude, "{\"type\":\"ai-title\"}\n").unwrap();
        assert!(matches!(
            adapter.admit_hook(&request(&claude, "not safe"), &layout, None),
            Err(IngestError::InvalidSession(_))
        ));
        match adapter.admit_hook(&request(&claude, session), &layout, None) {
            Ok(Admission::Ingest(located)) => assert_eq!(located.conversation_id, session),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            adapter.admit_file(&claude, None).unwrap().conversation_id,
            "claude"
        );
        // A missing transcript is left to the read, which reports it.
        assert!(adapter
            .admit_file(&temp.path().join("absent.jsonl"), None)
            .is_ok());
    }

    #[test]
    fn the_start_is_the_project_directory() {
        let adapter = Adapter;
        let request = HookRequest::default();
        assert_eq!(adapter.start(&request, &env(&[])), StartLocation::Unknown);
        assert_eq!(
            adapter.start(&request, &env(&[(PROJECT_DIR_ENV, "")])),
            StartLocation::NotNameable
        );
        assert_eq!(
            adapter.start(&request, &env(&[(PROJECT_DIR_ENV, "/w")])),
            StartLocation::Directory(PathBuf::from("/w"))
        );
    }
}
