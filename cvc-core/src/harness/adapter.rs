//! The adapter traits each harness implements.
//!
//! An adapter is split four ways, so that each part can be built and tested
//! on its own:
//!
//! * [`HookCaller`]: everything a hook run decides before any record is read.
//!   Pure: the environment is injected, and no file is opened.
//! * [`RecordSource`]: finding and reading the agent's own record. It does
//!   I/O, read-only, on the files the adapter allow-lists.
//! * [`Planner`]: turning a window of the record into [`CaptureCandidate`]s.
//!   Pure: no filesystem, no database, no clock except a fallback timestamp.
//! * [`Installer`]: installing, removing and reporting the agent's hooks.
//!
//! Everything else a capture needs is shared and lives beside this module:
//! the consent check and the one transaction per batch
//! ([`crate::harness::engine`]), scrub-then-bound ([`crate::harness::seal`]),
//! the lock ([`crate::harness::lock`]), the family commit rescue
//! ([`crate::harness::family`]) and the drift rule
//! ([`crate::harness::drift`]).
//!
//! The traits have associated types, so they are not object safe; dispatch is
//! a generic call through the registry ([`crate::harness::with_adapter`]).
//!
//! [`CaptureCandidate`]: crate::harness::candidate::CaptureCandidate
use crate::db::{CvcStore, DbError, StoredHarnessCursor};
use crate::harness::candidate::{ConversationRole, Finality, Plan, PlanInput, SourcePosition};
use crate::harness::cursor::{CursorCodec, EpochReason};
use crate::harness::drift::DriftRule;
use crate::harness::family::Family;
use crate::harness::HarnessId;
use crate::privacy::{CoverageGrant, PreparedPolicy};
use crate::repository::RepositoryLayout;
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

// ---- the hook -----------------------------------------------------------------------

/// The environment a hook runs in, injected so that [`HookCaller`] stays pure.
pub trait HookEnv {
    fn var_os(&self, name: &str) -> Option<OsString>;
}

/// The process's own environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl HookEnv for ProcessEnv {
    fn var_os(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }
}

impl HookEnv for std::collections::HashMap<String, OsString> {
    fn var_os(&self, name: &str) -> Option<OsString> {
        self.get(name).cloned()
    }
}

/// Who ran a hook, as far as its payload and environment tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerVerdict {
    /// This harness. `foreign_env` names another harness whose environment
    /// the run also has (Claude Code in Cursor's integrated terminal inherits
    /// `CURSOR_VERSION`): it never turns a well-formed run into a no-op, it
    /// only silences what would otherwise be an error about a payload or a
    /// record that is not this harness's.
    Ours { foreign_env: Option<HarnessId> },
    /// Another agent that loads this harness's hook configuration, told by
    /// its payload. The run does nothing but honour the output contract.
    Foreign(Option<HarnessId>),
}

/// What a hook payload says, in the terms every harness shares. Fields a
/// harness does not send stay empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookRequest {
    /// The hook event (`Stop`, `SessionEnd`, ...).
    pub event: Option<String>,
    /// The agent's id for the session, thread or conversation.
    pub session: Option<String>,
    /// The record the payload names (a transcript path).
    pub record: Option<PathBuf>,
    /// The hook's working directory.
    pub cwd: Option<PathBuf>,
    /// Set when the payload is a subagent's own (Claude Code `agent_id`).
    pub subagent: Option<String>,
    /// The workspace roots a multi-root agent names, in its order.
    pub roots: Vec<PathBuf>,
    /// The adapter's own slice of the payload: exactly the top-level keys its
    /// [`HookCaller::PAYLOAD_KEYS`] allow-lists, with their values as sent,
    /// and nothing else. Filled by [`hook_request`], never by the adapter, so
    /// a key no adapter needs (Cursor's `user_email`, say) is never kept.
    /// Values are not scrubbed here: an adapter that stores any of them
    /// (Cursor's spool, through [`RecordSource::record_event`]) scrubs and
    /// bounds them on write.
    pub extra: Map<String, Value>,
}

/// A hook payload this harness cannot use.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PayloadError {
    #[error("hook payload is not the JSON {0} delivers on stdin")]
    NotTheShape(&'static str),
    /// [`PayloadError::NotTheShape`] with what is wrong with it, such as
    /// ``"`session_id` is an integer, not a string"``. Hook errors reach the
    /// agent's user, so `detail` is built from the payload's key names and
    /// JSON types only ([`json_type`]) and must never contain a payload
    /// value.
    #[error("hook payload is not the JSON {harness} delivers on stdin: {detail}")]
    Malformed {
        harness: &'static str,
        detail: String,
    },
    #[error("hook payload has no {0}")]
    Missing(&'static str),
}

/// The JSON type of `value`, as an error names it ("an integer", "a
/// string"), never the value itself.
pub fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "an integer",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The request `payload` makes of `caller`: [`HookCaller::request`], with
/// [`HookRequest::extra`] set to the keys the caller allow-lists
/// ([`HookCaller::PAYLOAD_KEYS`]) and no others. Every hook run reads its
/// payload through this.
pub fn hook_request<C: HookCaller + ?Sized>(
    caller: &C,
    payload: Map<String, Value>,
) -> Result<HookRequest, PayloadError> {
    let extra: Map<String, Value> = C::PAYLOAD_KEYS
        .iter()
        .filter_map(|key| {
            payload
                .get(*key)
                .map(|value| ((*key).to_owned(), value.clone()))
        })
        .collect();
    let mut request = caller.request(payload)?;
    request.extra = extra;
    Ok(request)
}

/// Where a session started, which chooses the repository it is recorded in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartLocation {
    /// A directory the harness names (Claude Code `CLAUDE_PROJECT_DIR`,
    /// Cursor `workspace_roots[0]`, Antigravity `workspacePaths[0]`,
    /// OpenCode `session.directory`).
    Directory(PathBuf),
    /// The harness says the session started nowhere that can be named:
    /// nothing is recorded.
    NotNameable,
    /// The harness does not say: the payload's working directory, else the
    /// process's own.
    Unknown,
    /// The start is in the first entry of the record at this path (Codex:
    /// `session_meta.cwd` in the rollout's first line). The shell reads that
    /// line, bounded (at most 4 MiB, from a regular file opened without
    /// following a link), in memory, before the repository is known, and
    /// hands it to [`HookCaller::start_from_first_entry`]; the bytes are then
    /// discarded. Only an adapter whose start is there returns this. It fails
    /// closed: when the line cannot be read (unfinished, over the bound, a
    /// link, not a regular file, missing) or gives no directory, the start
    /// cannot be named and the event records nothing, rather than being
    /// filed by the payload's working directory under another repository; a
    /// later event catches up from the cursor.
    FromRecordStart(PathBuf),
}

/// What a hook prints on stdout, on every path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputContract {
    Silent,
    /// `{}`.
    EmptyObject,
    Literal(&'static str),
}

/// Everything a hook run needs before any record is read. Pure: no file I/O,
/// and the environment is injected. No method ever leads to exit status 2,
/// which every agent treats as blocking.
pub trait HookCaller {
    /// The top-level payload keys this adapter needs beyond the shared
    /// fields of [`HookRequest`], copied into [`HookRequest::extra`] by
    /// [`hook_request`] (Antigravity: `modelName`; Cursor: `generation_id`
    /// and the event's own fields). Anything not listed is dropped.
    const PAYLOAD_KEYS: &'static [&'static str] = &[];

    /// Classifies the caller from the payload's top-level keys (never nested
    /// ones: a tool's input can carry any key) and the environment. Runs
    /// before the repository is looked for.
    fn classify(&self, payload: &Map<String, Value>, env: &dyn HookEnv) -> CallerVerdict;
    /// The request a payload of this harness makes. It names what the
    /// payload holds and refuses only a payload that is not this harness's
    /// shape; a field a run needs but the payload lacks is
    /// [`Self::check_complete`]'s to report. Leaves [`HookRequest::extra`]
    /// empty: [`hook_request`] fills it.
    fn request(&self, payload: Map<String, Value>) -> Result<HookRequest, PayloadError>;
    /// Whether `request` has what a run needs ([`PayloadError::Missing`]
    /// otherwise). Runs once the repository is known to be CVC's and before
    /// the consent check. The default asks for nothing.
    fn check_complete(&self, _request: &HookRequest) -> Result<(), PayloadError> {
        Ok(())
    }
    /// Where the session started.
    fn start(&self, request: &HookRequest, env: &dyn HookEnv) -> StartLocation;
    /// Whether a run for this request records anything. Claude Code: not a
    /// subagent's payload, whose parent's run ingests the family.
    fn wants(&self, request: &HookRequest) -> bool;
    fn finality(&self, event: Option<&str>) -> Finality;
    /// How long a run for `event` may take, from the process's start: well
    /// below the timeout the harness kills a hook at, so that a run stops
    /// between batches on its own terms ([`crate::harness::engine`]).
    fn budget(&self, event: Option<&str>) -> Duration;
    fn contract(&self, event: Option<&str>) -> OutputContract;
    /// What follows a `consent-required` message: how to stop the hooks.
    fn stop_hint(&self) -> &'static str;
    /// Where the session started, read from the record's first complete
    /// line, for an adapter whose [`Self::start`] is
    /// [`StartLocation::FromRecordStart`]. Pure: `first_line` is all it may
    /// use. The default does not know.
    fn start_from_first_entry(&self, _first_line: &[u8]) -> StartLocation {
        StartLocation::Unknown
    }
}

// ---- the record ---------------------------------------------------------------------

/// A located record: what to read, and which conversation it is.
#[derive(Debug)]
pub struct Located<S> {
    /// The conversation the record is stored under.
    pub conversation_id: String,
    /// The conversation whose family this one is in, and whose lock a run
    /// takes. The conversation itself for a root.
    pub family_root: String,
    pub role: ConversationRole,
    pub source: S,
    /// Raw text a title is derived from, as from a prompt, ahead of the
    /// record's own title (Claude Code: a subagent's `.meta.json`).
    pub title_seed: Option<String>,
}

/// What a bounded read is allowed to take. [`ReadLimit::WHOLE`] reads to the
/// end of the record, as an operator's run does; a hook's run reads in
/// batches ([`crate::harness::engine`]).
///
/// A read that honours a limit takes whole records only (whole lines of a
/// JSONL file), as many as fit, and says when it stopped at the limit before
/// the end of the record ([`Read::limited`]). When not even one record fits,
/// it returns none: the engine then reads again with a larger limit. An
/// adapter whose records are not counted in bytes maps `bytes` onto its own
/// records' encoded size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimit {
    /// The most bytes of the record one read takes, from where it starts.
    pub bytes: Option<u64>,
    /// The most records one read takes.
    pub units: Option<usize>,
}

impl ReadLimit {
    pub const WHOLE: ReadLimit = ReadLimit {
        bytes: None,
        units: None,
    };
}

/// What one read needs beyond the source and the cursor.
#[derive(Debug, Clone, Copy)]
pub struct ReadContext<'a> {
    /// The canonical root of the worktree the capture is attributed to.
    pub worktree_root: &'a Path,
    /// Read from the start of the current epoch's record, ignoring the
    /// cursor. The cursor is still never moved back.
    pub rescan: bool,
    pub limit: ReadLimit,
}

/// Where reading resumes, and whether that is in a replaced record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resume<C> {
    pub from: C,
    /// Exact evidence that the record was replaced since the stored cursor
    /// was written: the cursor that commits opens the next epoch.
    pub replaced: Option<EpochReason>,
}

/// One read of a record.
#[derive(Debug)]
pub struct Read<W, C> {
    pub window: W,
    /// The position the read found the cursor valid at: `from`, or the start
    /// of the record when `from` no longer fits it. A rescan's cursor never
    /// commits behind it.
    pub trusted: C,
    /// Exact evidence, found by this read, that the record was replaced.
    pub replaced: Option<EpochReason>,
    /// How long the record is, as far as this read saw, for reports.
    pub record_len: Option<u64>,
    /// The read stopped at its [`ReadContext::limit`] before the end of the
    /// record as it stood: more of it can be read now. `false` for a read
    /// that reached the end, and for every read of an adapter that does not
    /// honour limits. The engine plans a limited window as
    /// [`Finality::Incremental`] whatever the run's finality, so that a
    /// response the limit cut is not closed early.
    pub limited: bool,
    /// How many bytes of the record the window holds, which the engine times
    /// its batches by (see [`ReadLimit`]); `None` for a record not counted in
    /// bytes, whose batches are estimated at the engine's floor rate.
    pub window_bytes: Option<u64>,
    /// Where the conversation's own history begins in the record, when the
    /// reader found that the record starts with history inherited from
    /// another conversation (Codex `history_base` or `forked_from_id`, a
    /// revert file): the planner skips what lies before it.
    pub own_history_start: Option<SourcePosition>,
}

/// What a record's start says about who wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniff {
    Ours,
    /// Positively another agent's.
    NotOurs,
    /// Nothing complete to tell from yet.
    NotYet,
    /// Neither: the parser, which reports what it finds, decides.
    Undetermined,
}

/// What a hook run does with the record its payload names, decided before the
/// store is opened ([`RecordSource::admit_hook`]).
#[derive(Debug)]
pub enum Admission<S> {
    /// Ingest it.
    Ingest(Located<S>),
    /// Do nothing, in silence: the record is not this harness's, as far as
    /// the run can corroborate.
    PassOver,
}

/// What [`RecordSource::record_event`] runs with: the repository, its store,
/// its `.thoughtignore` snapshot and the coverage grant the hook obtained for
/// the harness's channel.
#[derive(Clone, Copy)]
pub struct HookEvent<'a> {
    pub layout: &'a RepositoryLayout,
    pub store: &'a CvcStore,
    pub policy: &'a PreparedPolicy,
    /// Held for the whole step: the event is recorded only while the
    /// acknowledgement covers the harness's channel here.
    pub grant: &'a CoverageGrant,
}

/// A family as one run sees it: who is in it, for the commit rescue, and the
/// members this run ingests after the root, in order.
#[derive(Debug)]
pub struct FamilyRead<S> {
    pub family: Family,
    pub members: Vec<Located<S>>,
}

/// Claude Code's 0.7.x cursor row, which every Claude Code batch also writes
/// for older binaries sharing the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyCursor {
    pub transcript_path: String,
    pub byte_offset: u64,
}

/// Finding and reading the agent's own record. Read-only.
pub trait RecordSource {
    /// Which harness this is: its capture source, consent channel, cursor
    /// rows and lock files are this harness's.
    const ID: HarnessId;
    /// A located record: file(s), database rows, a spool key.
    type Source;
    type Cursor: CursorCodec;
    /// What [`Planner::plan`] consumes.
    type Window;
    type Error: std::error::Error + From<DbError> + Send + Sync + 'static;

    /// The record a hook payload names.
    fn locate_hook(
        &self,
        request: &HookRequest,
        layout: &RepositoryLayout,
    ) -> Result<Located<Self::Source>, Self::Error>;
    /// An operator's record file, with an optional id override.
    fn locate_file(
        &self,
        path: &Path,
        id: Option<&str>,
    ) -> Result<Located<Self::Source>, Self::Error>;
    /// A session by the agent's id, where the adapter can find one
    /// ([`Self::locates_sessions`]). Nothing screens what this returns
    /// before the store opens, so an adapter that offers it screens here as
    /// [`Self::admit_file`] does.
    fn locate_session(
        &self,
        id: &str,
        layout: &RepositoryLayout,
    ) -> Result<Located<Self::Source>, Self::Error>;
    /// Every session the adapter can find for this repository, where it
    /// can ([`Self::scans`]). As for [`Self::locate_session`], an adapter
    /// that offers it screens what it returns here.
    fn scan(&self, layout: &RepositoryLayout) -> Result<Vec<Located<Self::Source>>, Self::Error>;

    /// Whether [`Self::locate_session`] can find a session by its id, so
    /// that `--session` alone is refused before anything is looked at when
    /// it cannot. The default can.
    fn locates_sessions(&self) -> bool {
        true
    }

    /// Whether [`Self::scan`] can find sessions, as for
    /// [`Self::locates_sessions`].
    fn scans(&self) -> bool {
        true
    }

    /// Is this record this harness's? Bounded and cheap. Runs before the
    /// store is opened.
    fn sniff(&self, source: &Self::Source) -> Sniff;

    /// The record a hook payload names, located and screened: runs after the
    /// consent check and before the store is opened, and reads no more of
    /// the record than [`Self::sniff`] does. `foreign_env` is the
    /// `foreign_env` of [`CallerVerdict::Ours`].
    ///
    /// The default locates with [`Self::locate_hook`] and passes the record
    /// over when another harness's environment corroborates that it is not
    /// this harness's ([`Sniff::NotOurs`]) or not anyone's yet
    /// ([`Sniff::NotYet`]); everything else is ingested, so that the
    /// parser's own checks report what they find.
    fn admit_hook(
        &self,
        request: &HookRequest,
        layout: &RepositoryLayout,
        foreign_env: Option<HarnessId>,
    ) -> Result<Admission<Self::Source>, Self::Error> {
        let located = self.locate_hook(request, layout)?;
        Ok(match self.sniff(&located.source) {
            Sniff::NotOurs | Sniff::NotYet if foreign_env.is_some() => Admission::PassOver,
            _ => Admission::Ingest(located),
        })
    }

    /// An operator's record file, located and screened before the store is
    /// opened: [`Self::locate_file`], and a record that is positively not
    /// this harness's is refused. The default only locates, leaving that to
    /// the parser.
    fn admit_file(
        &self,
        path: &Path,
        id: Option<&str>,
    ) -> Result<Located<Self::Source>, Self::Error> {
        self.locate_file(path, id)
    }

    /// The one write an adapter makes before the engine: it records the hook
    /// event itself, when the payload is (part of) the agent's record (Cursor
    /// spools each event under its conversation and generation). It runs in
    /// hook mode only, after [`Self::admit_hook`], once the policy is loaded
    /// and the store is open, while the hook holds the coverage grant for the
    /// harness's channel; whatever it stores is scrubbed with `event.policy`
    /// and bounded on write. Nothing else an adapter does before
    /// [`crate::harness::engine`] writes. The default records nothing.
    fn record_event(
        &self,
        _request: &HookRequest,
        _event: &HookEvent<'_>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// The cursor of a record never read.
    fn initial_cursor(&self, located: &Located<Self::Source>) -> Self::Cursor;

    /// Where reading `located` resumes, given its stored typed cursor. The
    /// default reads the stored cursor with [`Self::Cursor`]'s codec, and
    /// starts at [`Self::initial_cursor`] when there is none or it is of
    /// another kind or version: the store then keeps a cursor it cannot read
    /// rather than overwrite it.
    fn resume(
        &self,
        located: &Located<Self::Source>,
        stored: Option<&StoredHarnessCursor>,
        _store: &CvcStore,
    ) -> Result<Resume<Self::Cursor>, DbError> {
        let from = match stored.map(<Self::Cursor as CursorCodec>::decode_stored) {
            Some(Ok(cursor)) => cursor,
            _ => self.initial_cursor(located),
        };
        Ok(Resume {
            from,
            replaced: None,
        })
    }

    /// Reads from `from`. A record of an unsupported major version fails
    /// loudly, and nothing of it is recorded (see [`crate::harness::drift`]).
    fn read(
        &self,
        source: &mut Self::Source,
        from: &Self::Cursor,
        context: &ReadContext<'_>,
    ) -> Result<Read<Self::Window, Self::Cursor>, Self::Error>;

    /// The family of `root`, once `root` is ingested. Never a prefix match.
    /// The default is the root and its recorded subagents
    /// ([`Family::from_relations`]), none of them read by this run.
    fn family(
        &self,
        root: &Located<Self::Source>,
        store: &CvcStore,
    ) -> Result<FamilyRead<Self::Source>, Self::Error> {
        Ok(FamilyRead {
            family: Family::from_relations(store, Self::ID, &root.family_root)?,
            members: Vec::new(),
        })
    }

    /// Claude Code only: the 0.7.x row `cursor` stands for.
    fn legacy_cursor(&self, _cursor: &Self::Cursor) -> Option<LegacyCursor> {
        None
    }
}

/// Pure planning.
pub trait Planner: RecordSource {
    /// The format the parser is pinned to and how it treats drift.
    const DRIFT: DriftRule;
    /// The title a conversation gets when nothing better is known.
    const DEFAULT_TITLE: &'static str;

    fn plan(&self, window: &Self::Window, input: &PlanInput<'_>) -> Plan<Self::Cursor>;
}

// ---- the hooks' installation ----------------------------------------------------------

/// Where hooks are installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstallScope {
    /// One checkout (Claude Code's `.claude/settings.local.json`).
    Checkout,
    /// The project, shared by its checkouts.
    Project,
    /// The user: every repository the agent opens.
    User,
}

#[derive(Clone, Copy)]
pub struct InstallContext<'a> {
    pub layout: &'a RepositoryLayout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallAction {
    Created,
    Updated,
    AlreadyPresent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub scope: InstallScope,
    /// The file the hooks are in.
    pub location: PathBuf,
    pub action: InstallAction,
    /// What the person who installed should be told, one sentence each.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallOutcome {
    pub scope: InstallScope,
    pub location: PathBuf,
    pub removed: usize,
    pub deleted_file: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessStatus {
    pub installed: Vec<InstallScope>,
    /// The binary the hooks run, where the adapter records one.
    pub binary: Option<PathBuf>,
    /// What is wrong with the installation, one sentence each.
    pub problems: Vec<String>,
}

/// Installing a harness's hooks is a separate, explicit act: it starts
/// nothing by itself, and capture still needs the acknowledgement to cover
/// the harness's channel.
pub trait Installer {
    fn scopes(&self) -> &'static [InstallScope];
    fn install(
        &self,
        context: &InstallContext<'_>,
        scope: InstallScope,
        binary: &Path,
    ) -> anyhow::Result<InstallOutcome>;
    fn uninstall(
        &self,
        context: &InstallContext<'_>,
        scope: InstallScope,
    ) -> anyhow::Result<UninstallOutcome>;
    fn status(&self, context: &InstallContext<'_>) -> anyhow::Result<HarnessStatus>;
}

/// A complete adapter: the registry dispatches to these.
pub trait HarnessAdapter: HookCaller + Planner + Installer + Sync + 'static {}

impl<T: HookCaller + Planner + Installer + Sync + 'static> HarnessAdapter for T {}
