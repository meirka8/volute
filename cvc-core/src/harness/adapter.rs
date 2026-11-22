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
}

/// A hook payload this harness cannot use.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PayloadError {
    #[error("hook payload is not the JSON {0} delivers on stdin")]
    NotTheShape(&'static str),
    #[error("hook payload has no {0}")]
    Missing(&'static str),
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
    /// The start is in the record's first entry (Codex `session_meta.cwd`).
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
    /// Classifies the caller from the payload's top-level keys (never nested
    /// ones: a tool's input can carry any key) and the environment. Runs
    /// before the repository is looked for.
    fn classify(&self, payload: &Map<String, Value>, env: &dyn HookEnv) -> CallerVerdict;
    /// The request a payload of this harness makes.
    fn request(&self, payload: Map<String, Value>) -> Result<HookRequest, PayloadError>;
    /// Where the session started.
    fn start(&self, request: &HookRequest, env: &dyn HookEnv) -> StartLocation;
    /// Whether a run for this request records anything. Claude Code: not a
    /// subagent's payload, whose parent's run ingests the family.
    fn wants(&self, request: &HookRequest) -> bool;
    fn finality(&self, event: Option<&str>) -> Finality;
    /// How long a run for `event` may take, from the process's start. The
    /// engine does not enforce it yet (#149).
    fn budget(&self, event: Option<&str>) -> Duration;
    fn contract(&self, event: Option<&str>) -> OutputContract;
    /// What follows a `consent-required` message: how to stop the hooks.
    fn stop_hint(&self) -> &'static str;
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
/// end of the record, as every ingest does until batching (#149).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimit {
    pub bytes: Option<u64>,
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
    /// A session by the agent's id, where the adapter can find one.
    fn locate_session(
        &self,
        id: &str,
        layout: &RepositoryLayout,
    ) -> Result<Located<Self::Source>, Self::Error>;
    /// Every session the adapter can find for this repository.
    fn scan(&self, layout: &RepositoryLayout) -> Result<Vec<Located<Self::Source>>, Self::Error>;

    /// Is this record this harness's? Bounded and cheap. Runs before the
    /// store is opened.
    fn sniff(&self, source: &Self::Source) -> Sniff;

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
