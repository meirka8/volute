//! `cvc ingest`: record an agent-harness session transcript locally.
//!
//! Not to be confused with sync ingestion (`cvc pull`), which imports another
//! machine's already-shared projection: this is first-party local capture.
use crate::terminal_text::sanitize_for_terminal;
use anyhow::{anyhow, bail, Context, Result};
use cvc_core::claude_code::{
    ingest_with, sniff_first_entry, FirstEntry, IngestError, IngestMode, IngestOptions,
    IngestReport,
};
use cvc_core::db::CvcStore;
use cvc_core::privacy::PreparedPolicy;
use cvc_core::repository::{RepositoryLayout, RepositoryLayoutError};
use git2::ErrorCode;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};

const MAX_HOOK_PAYLOAD_BYTES: u64 = 1024 * 1024;
/// An oversize payload is still read to here, into nowhere, before the hook
/// decides: Claude Code writes the whole payload to the hook's stdin, and a
/// process that exits with part of it unread hands the writer a broken pipe.
const MAX_DRAINED_HOOK_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;
/// Claude Code exports this to every hook it runs: the directory the session
/// was started in.
const PROJECT_DIR_ENV: &str = "CLAUDE_PROJECT_DIR";
/// Cursor exports this to the hooks it runs. Claude Code run in Cursor's
/// integrated terminal, and so its hooks, may inherit it too (not verified).
const CURSOR_VERSION_ENV: &str = "CURSOR_VERSION";
/// The top-level key every Cursor hook input carries, per Cursor's
/// documentation, and Claude Code does not send.
const CURSOR_VERSION_KEY: &str = "cursor_version";
/// Cursor's conversation id. Taken for Cursor's only in a payload without
/// Claude Code's `session_id` (see [`classify_hook_caller`]).
const CURSOR_CONVERSATION_KEY: &str = "conversation_id";
/// Top-level keys of Antigravity's hook input that Claude Code does not send:
/// the three its changelog documents (`hookEventName`, `toolCall`,
/// `lastUserInput`) and three more in its camelCase style. An explicit list,
/// never a rule such as "any camelCase key", which a future Claude Code key
/// could match, silently ending all capture.
const ANTIGRAVITY_PAYLOAD_KEYS: [&str; 6] = [
    "hookEventName",
    "toolCall",
    "lastUserInput",
    "conversationId",
    "transcriptPath",
    "workspacePaths",
];

/// The subset of Claude Code's hook payload this command relies on.
#[derive(Deserialize)]
struct HookPayload {
    session_id: Option<String>,
    transcript_path: Option<String>,
    cwd: Option<String>,
    hook_event_name: Option<String>,
    agent_id: Option<String>,
}

/// Who ran the hook, as far as its payload and environment tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookCaller {
    /// Another agent that loads Claude Code's hook configuration, told by a
    /// top-level key that Claude Code does not send. Not CVC's Claude Code
    /// channel: ignored.
    Foreign(ForeignAgent),
    /// Claude Code, as far as the payload tells. `cursor_env` is
    /// `CURSOR_VERSION` in the environment, which a Claude Code session in
    /// Cursor's integrated terminal may inherit too, so it never turns a
    /// well-formed Claude Code run into a no-op: it only silences what would
    /// otherwise be an error about a payload or transcript that is not Claude
    /// Code's, or a no-op (see [`run_hook`]).
    ClaudeCode { cursor_env: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForeignAgent {
    Cursor,
    Antigravity,
}

/// Classifies the caller of a hook from the payload's top-level keys (never
/// nested ones: a tool's input can carry any key) and the value of
/// `CURSOR_VERSION`. `CLAUDE_PROJECT_DIR` is not looked at: Cursor sets it too.
///
/// `cursor_version` alone means Cursor. `conversation_id` means Cursor only in
/// a payload without `session_id`, so that a key of that name Claude Code may
/// one day add never silences its own runs.
fn classify_hook_caller(
    payload: &Map<String, Value>,
    cursor_version: Option<&OsStr>,
) -> HookCaller {
    if payload.contains_key(CURSOR_VERSION_KEY)
        || (payload.contains_key(CURSOR_CONVERSATION_KEY) && !payload.contains_key("session_id"))
    {
        return HookCaller::Foreign(ForeignAgent::Cursor);
    }
    if ANTIGRAVITY_PAYLOAD_KEYS
        .iter()
        .any(|key| payload.contains_key(*key))
    {
        return HookCaller::Foreign(ForeignAgent::Antigravity);
    }
    HookCaller::ClaudeCode {
        cursor_env: cursor_version.is_some_and(|value| !value.is_empty()),
    }
}

/// Who runs an ingest, which decides what a transcript that is not Claude
/// Code's, or not yet anyone's, comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Caller {
    /// `--hook`: another agent's transcript is passed over in silence when
    /// that is corroborated (see [`first_entry_verdict`]), and is an error
    /// otherwise.
    Hook { cursor_env: bool },
    /// `--transcript`: another agent's transcript is an error.
    Operator,
}

/// What a run does with a transcript, given how it begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Ingest it: the parser's own checks report whatever they find.
    Ingest,
    /// Do nothing, in silence, without opening the store.
    PassOver,
    /// Fail with [`IngestError::NotClaudeCode`], without opening the store.
    Refuse,
}

pub async fn claude_code(
    transcript: Option<PathBuf>,
    session: Option<String>,
    hook: bool,
    rescan: bool,
) -> Result<()> {
    if hook {
        return run_hook();
    }
    let Some(transcript) = transcript else {
        bail!("pass --transcript <path> (optionally --session <id>), or --hook to read a Claude Code hook payload from stdin");
    };
    let current_dir = env::current_dir()?;
    // An operator-driven read treats the transcript as complete: it is the
    // catch-up path for finished sessions and for sessions recorded before
    // the hooks were installed.
    let report = run(
        &current_dir,
        &transcript,
        session.as_deref(),
        IngestMode::Final,
        rescan,
    )?;
    for line in report_lines(&report) {
        println!("{line}");
    }
    Ok(())
}

/// What `cvc ingest claude-code --transcript` prints of a run. The session id
/// passed the capture policy's identifier rule, but the block types are
/// whatever the transcript's `type` fields say, so both are shown with every
/// control and format character replaced by `?`, each on the line it belongs
/// to.
fn report_lines(report: &IngestReport) -> Vec<String> {
    let mut lines = vec![format!(
        "CVC: Claude Code session {}: {} new thought(s), {} already recorded, {} pending, {} linked to session commits; cursor at byte {} of {}",
        sanitize_for_terminal(&report.session_id),
        report.inserted,
        report.already_present,
        report.pending_responses,
        report.linked_from_commits,
        report.cursor,
        report.transcript_len
    )];
    if report.subagent_sessions > 0 {
        lines.push(format!(
            "  plus {} subagent session(s): {} new thought(s), {} linked to commits",
            report.subagent_sessions, report.subagent_inserted, report.subagent_linked
        ));
    }
    if report.unrecorded_blocks > 0 {
        let types: Vec<_> = report
            .unrecorded_block_types
            .iter()
            .map(|kind| sanitize_for_terminal(kind))
            .collect();
        lines.push(format!(
            "  {} content block(s) of a type this version does not record were kept as placeholders: {}",
            report.unrecorded_blocks,
            types.join(", ")
        ));
    }
    lines.extend(left_out_lines(report));
    lines
}

/// What the capture boundary could not keep, as lines to print, so that a
/// response recorded as a stand-in, or left out, is never silent. None of the
/// text of such a response is in them, only the limit that was hit.
fn left_out_lines(report: &IngestReport) -> Vec<String> {
    let mut lines = Vec::new();
    if report.withheld > 0 || report.lost > 0 {
        let mut parts = Vec::new();
        if report.withheld > 0 {
            parts.push(format!(
                "{} recorded as a stand-in that says its response was withheld",
                report.withheld
            ));
        }
        if report.lost > 0 {
            parts.push(format!("{} not recorded at all", report.lost));
        }
        lines.push(format!(
            "  could not scrub {} response(s) within the limits: {} ({})",
            report.withheld + report.lost,
            parts.join(", "),
            sanitize_for_terminal(&report.withheld_reasons.join("; "))
        ));
    }
    if report.omitted_tool_calls > 0 || report.omitted_context_items > 0 {
        lines.push(format!(
            "  left out of the responses recorded now: {} tool call(s) beyond the 255 a response records, {} context item(s) (a path that cannot be scrubbed, or beyond the 256 a response records); each such response ends with a cvc-omitted record that says so",
            report.omitted_tool_calls, report.omitted_context_items
        ));
    }
    lines
}

/// Hook mode reads Claude Code's JSON payload from stdin and writes nothing to
/// stdout, which Claude Code would interpret as hook output. Failures exit
/// non-zero so they are visible in the session, and never with status 2, the
/// only exit status Claude Code treats as blocking.
///
/// The same command is run by the checkout's own hook entries and by the cvc
/// Claude Code plugin, which is installed for the user and so fires in every
/// project they open, CVC-initialized or not. Other agents run it too: Cursor
/// loads Claude Code's hook settings by default, and Antigravity's CLI can
/// import Claude Code plugins, and each hands it its own payload. It therefore
/// decides, in this order:
///
/// 1. The payload is read from stdin, bounded at 1 MiB. Nothing in it is
///    validated yet. A larger one is read on, unused, up to a cap, so that the
///    writer does not meet a closed pipe.
/// 2. A payload that is a JSON object with a top-level key that Claude Code
///    does not send is another agent's run (see [`classify_hook_caller`]):
///    Cursor's `cursor_version` (or its `conversation_id` without a
///    `session_id`), or one of Antigravity's `hookEventName`, `toolCall`,
///    `lastUserInput`, `conversationId`, `transcriptPath`, `workspacePaths`.
///    Exit 0 with nothing on stdout or stderr, before any repository is looked
///    for or any file read.
/// 3. The repository is chosen (see [`recording_repository`]): a session is
///    recorded, in full, in the repository it was started in, which Claude
///    Code exports to every hook as `CLAUDE_PROJECT_DIR`. The hook's working
///    directory (the payload's `cwd`) only picks which of that repository's
///    worktrees a run is attributed to, so a session that has since moved to
///    another folder or repository still ends up whole where it began. Without
///    the variable (an older Claude Code, a manual run) the working directory
///    chooses the repository; with it empty, the session was not started in
///    any repository that can be named.
/// 4. Unless that repository can be established as CVC-initialized, the hook
///    does nothing: exit 0 with nothing on stdout or stderr, whatever the
///    payload held. "Established" means the directory exists and is a Git
///    working tree with a CVC directory; one that does not exist, is in no
///    repository, is in a bare one, is in one Git itself refuses as owned by
///    someone else, or is in one without CVC is simply not CVC's business (see
///    [`is_not_cvcs_business`]). Any other discovery failure (an unreadable or
///    corrupt repository, a `cvc` entry that is not a directory) may hide
///    capture loss in an initialized repository, and fails visibly.
/// 5. Only then the payload must be valid (read whole within the bound, the
///    JSON Claude Code sends, with a session id and a transcript path), or the
///    hook fails, unless `CURSOR_VERSION` is set, when a payload that is not
///    is taken for one of Cursor's and the hook does nothing.
///    Subagent payloads are ignored, since the parent session's hooks ingest
///    that transcript; and capture must be acknowledged, or the hook fails
///    with `consent-required`.
/// 6. Before the CVC database is opened, the transcript is read from its
///    start up to its first complete line (at most 1 MiB; see
///    [`first_entry_verdict`]). A first entry with another agent's shape
///    (Cursor's, Antigravity's, Codex's) is passed over in silence when that
///    is corroborated: `CURSOR_VERSION` is set, or the transcript's file name
///    does not name the payload's session, as Claude Code's `<session>.jsonl`
///    does. Otherwise the hook fails, `not a Claude Code transcript`, so that
///    a foreign line put at the top of a session's own transcript cannot
///    silence it. With `CURSOR_VERSION` set, a transcript with no complete
///    entry yet is passed over too; the next event reads it. Anything else
///    (a Claude Code entry of a version this cvc does not support, a first
///    line that is damaged or of nobody known, a transcript that cannot be
///    read) goes on to the parser, which reports it as before.
/// 7. The transcript is ingested, under the session's lock. A hook that is
///    still waiting for another run on the session when the wait ends does
///    nothing, as in 4: that run is making progress, and the next event
///    catches up from the cursor.
fn run_hook() -> Result<()> {
    let input = read_payload(io::stdin().lock());
    // Parsed leniently here: a payload that is not usable is reported below,
    // once the repository is known to be CVC's.
    let object = (input.read.is_ok() && !input.oversize)
        .then(|| serde_json::from_slice::<Map<String, Value>>(&input.raw));
    // Read whatever the payload is, so that it also covers one that could not
    // be read or parsed (see step 5).
    let cursor_version = env::var_os(CURSOR_VERSION_ENV);
    let cursor_env = cursor_version
        .as_deref()
        .is_some_and(|value| !value.is_empty());
    if let Some(Ok(object)) = object.as_ref() {
        if let HookCaller::Foreign(_) = classify_hook_caller(object, cursor_version.as_deref()) {
            return Ok(());
        }
    }
    let parsed = object.map(|object| {
        object.and_then(|object| serde_json::from_value::<HookPayload>(Value::Object(object)))
    });

    let payload_cwd = parsed
        .as_ref()
        .and_then(|parsed| parsed.as_ref().ok())
        .and_then(|payload| payload.cwd.as_deref())
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from);
    let Some(layout) = recording_repository(env::var_os(PROJECT_DIR_ENV), payload_cwd)? else {
        return Ok(());
    };

    // Under Cursor, a payload that could not be read whole, or that is not the
    // JSON Claude Code sends, is taken for Cursor's own (a `PostToolUse` whose
    // tool output went past the bound, say). Nothing could be ingested from
    // it in any case.
    if cursor_env && (input.read.is_err() || input.oversize || !matches!(parsed, Some(Ok(_)))) {
        return Ok(());
    }
    input
        .read
        .context("could not read the hook payload from stdin")?;
    if input.oversize {
        bail!("hook payload exceeds 1 MiB");
    }
    let payload = parsed
        .ok_or_else(|| anyhow!("hook payload was not read"))?
        .context("hook payload is not the JSON Claude Code delivers on stdin")?;
    if payload.agent_id.is_some() {
        // Subagent hooks receive the parent session's transcript path; the
        // parent session's own hooks ingest it.
        return Ok(());
    }
    let session = payload.session_id.filter(|value| !value.is_empty());
    let transcript = payload
        .transcript_path
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let (session, transcript) = match (session, transcript) {
        (Some(session), Some(transcript)) => (session, transcript),
        // Claude Code always sends both; under Cursor, a payload without them
        // is taken for Cursor's own.
        _ if cursor_env => return Ok(()),
        (None, _) => bail!("hook payload has no session_id"),
        (Some(_), None) => bail!("hook payload has no transcript_path"),
    };
    let mode = match payload.hook_event_name.as_deref() {
        Some("Stop") | Some("SessionEnd") => IngestMode::Final,
        _ => IngestMode::Incremental,
    };
    // The hook reads from the cursor, always: only an operator rescans.
    match ingest_into(
        &layout,
        &transcript,
        Some(&session),
        mode,
        false,
        Caller::Hook { cursor_env },
    ) {
        Ok(_) => Ok(()),
        // Another run holds this session's lock and has held it for the whole
        // wait. It is making progress, and whatever it does not reach, the next
        // event's run reads from the cursor; a hook that failed here would only
        // report a delay as an error.
        Err(error) if session_is_busy(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

/// What was read of a hook's stdin.
struct HookInput {
    raw: Vec<u8>,
    /// How reading went; `raw` is whatever was read before a failure.
    read: io::Result<()>,
    /// There was more than [`MAX_HOOK_PAYLOAD_BYTES`]; `raw` holds one byte
    /// beyond that and nothing after it.
    oversize: bool,
}

/// Reads at most [`MAX_HOOK_PAYLOAD_BYTES`] + 1 bytes of `input` and, when
/// there is more, reads and discards the rest up to
/// [`MAX_DRAINED_HOOK_PAYLOAD_BYTES`] in all.
fn read_payload(mut input: impl Read) -> HookInput {
    let mut raw = Vec::new();
    let read = input
        .by_ref()
        .take(MAX_HOOK_PAYLOAD_BYTES + 1)
        .read_to_end(&mut raw)
        .map(drop);
    let oversize = raw.len() as u64 > MAX_HOOK_PAYLOAD_BYTES;
    if oversize && read.is_ok() {
        // Best effort: what cannot be read is no more use than what was.
        let _ = io::copy(
            &mut input
                .by_ref()
                .take(MAX_DRAINED_HOOK_PAYLOAD_BYTES - raw.len() as u64),
            &mut io::sink(),
        );
    }
    HookInput {
        raw,
        read,
        oversize,
    }
}

/// Whether `error` is an ingest that gave up waiting for another run on the
/// same session (see [`cvc_core::claude_code::IngestError::SessionBusy`]).
fn session_is_busy(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<IngestError>(),
        Some(IngestError::SessionBusy { .. })
    )
}

/// The CVC-initialized repository `directory` is in, or `None` when it can be
/// established that there is none. `Err` is a failure to tell, which hook mode
/// must not mistake for "no": see [`run_hook`].
fn initialized_repository_at(directory: &Path) -> Result<Option<RepositoryLayout>> {
    // Git discovery would take a path that does not exist, or a file, for a
    // path it merely cannot find a repository from, and, worse, walk up from
    // it into an ancestor's repository.
    match fs::metadata(directory) {
        Ok(metadata) if !metadata.is_dir() => return Ok(None),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        // Whatever else is wrong with it, discovery says so below.
        Ok(_) | Err(_) => {}
    }
    let layout = match RepositoryLayout::discover(directory) {
        Ok(layout) => layout,
        Err(error) if is_not_cvcs_business(&error) => return Ok(None),
        Err(error) => {
            return Err(anyhow::Error::new(error).context("Failed to discover Git repository"))
        }
    };
    // A bare repository has no working tree, so no session works in it.
    if layout.worktree_root().is_err() {
        return Ok(None);
    }
    let cvc_dir = layout.cvc_dir();
    // Not `fs::metadata`: a link there is as much something that is not a
    // directory as a file is, and the store refuses to open it.
    match fs::symlink_metadata(&cvc_dir) {
        Ok(metadata) if metadata.is_dir() => Ok(Some(layout)),
        // Something is where CVC's directory belongs, so this may well be an
        // initialized repository whose capture would be lost in silence.
        Ok(_) => bail!(
            "CVC cannot record here: {} exists but is not a directory; repair or remove it (it is a file or a link where CVC's directory belongs), then run `cvc init`",
            cvc_dir.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("cannot read this repository's CVC directory"),
    }
}

/// Whether failing to discover a repository means the directory is simply not
/// one CVC records into, rather than a repository whose state hides a problem.
///
/// Besides "there is no repository", that is a repository libgit2 refuses
/// because another user owns it (`ErrorCode::Owner`, the "dubious ownership"
/// check). Git itself refuses such a repository too, for every command, until
/// its `safe.directory` setting lists it, and when it does libgit2 opens it
/// normally and this never sees the error. So the refusal is not a fault of
/// CVC's or of the repository, and a user-scoped plugin that failed here would
/// report an error in every session in someone else's checkout.
///
/// Everything else is a failure to tell, which hook mode reports: for example
/// `.git` metadata Git cannot open (a gitfile pointing nowhere), which a CVC
/// repository may have.
fn is_not_cvcs_business(error: &RepositoryLayoutError) -> bool {
    match error {
        RepositoryLayoutError::NotRepository => true,
        RepositoryLayoutError::Git(error) => error.code() == ErrorCode::Owner,
        RepositoryLayoutError::Metadata(_) | RepositoryLayoutError::BareRepository => false,
    }
}

/// The CVC-initialized repository a hook records its session in, laid out for
/// the worktree the run is attributed to, or `None` when there is none to
/// record in.
///
/// A session is recorded, in full, in the repository it was started in. Claude
/// Code loads a checkout's own settings only for sessions started in it, but
/// the user-scoped plugin's hooks fire in every session, and a session moves:
/// into a folder added with `/add-dir`, into another repository, into a nested
/// one. Choosing the repository by where the session happens to be would take
/// the part of a transcript that follows it into a repository it was never
/// started in, and leave the one it was started in without its end. So the
/// repository is the one `CLAUDE_PROJECT_DIR`, the directory the session was
/// started in, is in, judged as [`initialized_repository_at`] judges any
/// directory. Its worktree is the project directory's own, unless the hook's
/// working directory (`cwd`) is in the same repository (the same canonical
/// common Git directory, which makes a linked worktree and a subdirectory the
/// same repository): then it is the working directory's worktree, as a
/// session that works in a linked worktree is attributed to it. The working
/// directory never chooses the repository.
///
/// With the variable unset (an older Claude Code, a manual run) there is no
/// start to go by, and the working directory, else the process's own
/// directory, is the one in question. With it set and empty, the session was
/// not started in any repository that can be named, and nothing is recorded.
fn recording_repository(
    project_dir: Option<OsString>,
    payload_cwd: Option<PathBuf>,
) -> Result<Option<RepositoryLayout>> {
    let Some(project_dir) = project_dir else {
        let directory = match payload_cwd {
            Some(cwd) => cwd,
            None => match env::current_dir() {
                Ok(directory) => directory,
                // The process's own directory was removed: no repository.
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error).context("cannot determine the current directory"),
            },
        };
        return initialized_repository_at(&directory);
    };
    if project_dir.is_empty() {
        return Ok(None);
    }
    let Some(project) = initialized_repository_at(Path::new(&project_dir))? else {
        return Ok(None);
    };
    Ok(Some(attributed_to(project, payload_cwd.as_deref())))
}

/// `project`, or the layout of `working_directory` when that is in the same
/// repository. Only for attribution, so a working directory that cannot be
/// resolved, or is elsewhere, is simply not used.
fn attributed_to(project: RepositoryLayout, working_directory: Option<&Path>) -> RepositoryLayout {
    let Some(directory) = working_directory else {
        return project;
    };
    // As in `initialized_repository_at`: discovery alone would take a path
    // that does not exist for one it merely cannot find a repository from.
    if !fs::metadata(directory).is_ok_and(|metadata| metadata.is_dir()) {
        return project;
    }
    match RepositoryLayout::discover(directory) {
        Ok(candidate)
            if candidate.common_git_dir() == project.common_git_dir()
                && candidate.worktree_root().is_ok() =>
        {
            candidate
        }
        _ => project,
    }
}

fn run(
    repository_path: &Path,
    transcript: &Path,
    session: Option<&str>,
    mode: IngestMode,
    rescan: bool,
) -> Result<IngestReport> {
    let layout = super::harness::discover_initialized_at(repository_path)?;
    // An operator's run passes nothing over: see `first_entry_verdict`.
    ingest_into(&layout, transcript, session, mode, rescan, Caller::Operator)?
        .ok_or_else(|| anyhow!("the transcript was passed over"))
}

/// Ingests `transcript` into `layout`'s store, or returns `None` when a hook
/// passed it over (see [`first_entry_verdict`]).
fn ingest_into(
    layout: &RepositoryLayout,
    transcript: &Path,
    session: Option<&str>,
    mode: IngestMode,
    rescan: bool,
    caller: Caller,
) -> Result<Option<IngestReport>> {
    super::harness::require_claude_code_capture_covered(layout)?;
    // Before the store is opened. What is not a regular file (a link, a FIFO,
    // a directory) is refused here, with the error ingestion gives it; a
    // transcript that cannot be opened or read is left to the ingest below,
    // which reports it as before.
    match sniff_first_entry(transcript) {
        Ok(first) => match first_entry_verdict(caller, first, transcript, session) {
            Verdict::Ingest => {}
            Verdict::PassOver => return Ok(None),
            Verdict::Refuse => {
                return Err(IngestError::NotClaudeCode(transcript.to_path_buf()).into())
            }
        },
        Err(refused @ IngestError::NotRegularFile(_)) => return Err(refused.into()),
        Err(_) => {}
    }
    let policy = PreparedPolicy::load(layout.policy_root()?)
        .map_err(|error| anyhow!("CVC capture blocked by .thoughtignore: {error}"))?;
    // An operator's run also finishes the store's one-time upgrade; a hook
    // never runs it, whatever the store's size.
    let store = match caller {
        Caller::Operator => CvcStore::open_initialized_upgrading(layout.db_path())?,
        Caller::Hook { .. } => CvcStore::open_initialized(layout.db_path())?,
    };
    Ok(Some(ingest_with(
        layout,
        &store,
        &policy,
        transcript,
        session,
        mode,
        IngestOptions {
            rescan,
            ..IngestOptions::default()
        },
    )?))
}

/// What a run by `caller` does with `transcript`, for `session`, given that
/// it begins as `first` says.
///
/// An operator's run refuses another agent's transcript and ingests anything
/// else. A hook passes another agent's transcript over in silence only when
/// something besides its first line says the run is not Claude Code's own:
/// `CURSOR_VERSION` is set, or the file is not named for the session, as
/// Claude Code names its transcripts `<session_id>.jsonl`. Otherwise it
/// refuses it, visibly, so that a foreign-shaped line put at the top of a
/// session's own transcript cannot silence that session. Under Cursor, a hook
/// also passes over a transcript with no complete entry yet, which the next
/// event reads. Everything else is ingested, so that the parser's own checks
/// (the version pinning among them) report what they find.
fn first_entry_verdict(
    caller: Caller,
    first: FirstEntry,
    transcript: &Path,
    session: Option<&str>,
) -> Verdict {
    match (caller, first) {
        (Caller::Operator, FirstEntry::NotClaudeCode) => Verdict::Refuse,
        (Caller::Operator, _) => Verdict::Ingest,
        (Caller::Hook { cursor_env }, FirstEntry::NotClaudeCode) => {
            if cursor_env || !named_for_session(transcript, session) {
                Verdict::PassOver
            } else {
                Verdict::Refuse
            }
        }
        (Caller::Hook { cursor_env: true }, FirstEntry::NoCompleteEntry) => Verdict::PassOver,
        (Caller::Hook { .. }, _) => Verdict::Ingest,
    }
}

/// Whether `transcript` is named `<session>.<extension>`, as Claude Code names
/// a session's transcript.
fn named_for_session(transcript: &Path, session: Option<&str>) -> bool {
    session.is_some_and(|session| transcript.file_stem() == Some(OsStr::new(session)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::ErrorClass;

    fn git_error(code: ErrorCode) -> RepositoryLayoutError {
        RepositoryLayoutError::Git(git2::Error::new(code, ErrorClass::Repository, "stand-in"))
    }

    /// CI cannot chown a repository to another user, so the classification is
    /// pinned on the error libgit2 raises for one (`GIT_EOWNER`).
    #[test]
    fn a_repository_owned_by_someone_else_is_not_cvcs_business() {
        assert!(is_not_cvcs_business(&git_error(ErrorCode::Owner)));
        assert!(is_not_cvcs_business(&RepositoryLayoutError::NotRepository));
    }

    #[test]
    fn every_other_failure_to_discover_a_repository_stays_visible() {
        for code in [
            ErrorCode::GenericError,
            ErrorCode::NotFound,
            ErrorCode::Invalid,
            ErrorCode::Locked,
            ErrorCode::Certificate,
            ErrorCode::Ambiguous,
            ErrorCode::BareRepo,
            ErrorCode::UnbornBranch,
            ErrorCode::Directory,
        ] {
            assert!(!is_not_cvcs_business(&git_error(code)), "{code:?}");
        }
        assert!(!is_not_cvcs_business(&RepositoryLayoutError::Metadata(
            "Git cannot open the repository at /x/.git: gone".into()
        )));
        assert!(!is_not_cvcs_business(
            &RepositoryLayoutError::BareRepository
        ));
    }

    /// A reader of `length` spaces that counts what is taken from it.
    struct Counting<R> {
        inner: R,
        taken: u64,
    }

    impl<R: Read> Read for Counting<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = self.inner.read(buffer)?;
            self.taken += count as u64;
            Ok(count)
        }
    }

    fn spaces(length: u64) -> Counting<impl Read> {
        Counting {
            inner: io::repeat(b' ').take(length),
            taken: 0,
        }
    }

    #[test]
    fn a_payload_up_to_the_bound_is_read_whole() {
        let input = read_payload(&b"{\"cwd\":\"/x\"}"[..]);
        assert!(input.read.is_ok());
        assert!(!input.oversize);
        assert_eq!(input.raw, b"{\"cwd\":\"/x\"}");

        let mut source = spaces(MAX_HOOK_PAYLOAD_BYTES);
        let input = read_payload(&mut source);
        assert!(!input.oversize, "exactly the bound is not over it");
        assert_eq!(input.raw.len() as u64, MAX_HOOK_PAYLOAD_BYTES);
        assert_eq!(source.taken, MAX_HOOK_PAYLOAD_BYTES);
    }

    #[test]
    fn an_oversize_payload_is_kept_to_the_bound_and_read_to_the_end() {
        for length in [MAX_HOOK_PAYLOAD_BYTES + 1, 8 * 1024 * 1024] {
            let mut source = spaces(length);
            let input = read_payload(&mut source);
            assert!(input.read.is_ok());
            assert!(input.oversize, "{length}");
            // Only the bound and one byte more are kept...
            assert_eq!(input.raw.len() as u64, MAX_HOOK_PAYLOAD_BYTES + 1);
            // ...and the writer's remaining bytes are taken off the pipe.
            assert_eq!(source.taken, length, "{length}");
        }
    }

    #[test]
    fn what_is_read_off_the_pipe_has_a_cap() {
        let mut source = spaces(MAX_DRAINED_HOOK_PAYLOAD_BYTES + 1024 * 1024);
        let input = read_payload(&mut source);
        assert!(input.oversize);
        assert_eq!(source.taken, MAX_DRAINED_HOOK_PAYLOAD_BYTES);
    }

    #[test]
    fn a_failing_pipe_is_reported_and_a_failing_drain_is_not() {
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("closed"))
            }
        }
        let input = read_payload(Failing);
        assert!(input.read.is_err());
        assert!(!input.oversize);

        // The payload was complete as far as it mattered when the pipe broke.
        let input = read_payload(io::repeat(b' ').take(2 * 1024 * 1024).chain(Failing));
        assert!(input.read.is_ok());
        assert!(input.oversize);
    }

    fn report(session_id: &str, unrecorded_block_types: &[&str]) -> IngestReport {
        IngestReport {
            session_id: session_id.into(),
            inserted: 2,
            already_present: 1,
            pending_responses: 0,
            linked_from_commits: 0,
            cursor: 10,
            transcript_len: 10,
            subagent_sessions: 0,
            subagent_inserted: 0,
            subagent_linked: 0,
            unrecorded_blocks: unrecorded_block_types.len(),
            unrecorded_block_types: unrecorded_block_types
                .iter()
                .map(|kind| (*kind).to_owned())
                .collect(),
            withheld: 0,
            lost: 0,
            withheld_reasons: Vec::new(),
            omitted_tool_calls: 0,
            omitted_context_items: 0,
        }
    }

    #[test]
    fn the_report_shows_the_session_id_and_the_block_types_as_plain_text() {
        let hostile = report(
            "s\u{1b}[2J\u{202e}1",
            &[
                "mys\u{1b}[2Jtery\u{1b}]52;c;SGVsbG8=\u{7}",
                "x\u{202e}y\u{2028}z",
            ],
        );
        assert_eq!(
            report_lines(&hostile),
            [
                "CVC: Claude Code session s?[2J?1: 2 new thought(s), 1 already recorded, 0 pending, 0 linked to session commits; cursor at byte 10 of 10",
                "  2 content block(s) of a type this version does not record were kept as placeholders: mys?[2Jtery?]52;c;SGVsbG8=?, x?y?z",
            ]
        );
        // What a run of a well-formed transcript reports is printed as it is.
        assert_eq!(
            report_lines(&report("7c1e9f5a-2b2f", &["mystery"]))[1],
            "  1 content block(s) of a type this version does not record were kept as placeholders: mystery"
        );
        assert_eq!(report_lines(&report("7c1e9f5a-2b2f", &[])).len(), 1);
    }

    #[test]
    fn only_a_session_that_waited_in_vain_is_busy() {
        let busy = anyhow::Error::new(IngestError::SessionBusy {
            session: "s".into(),
            waited_secs: 20,
        });
        assert!(session_is_busy(&busy));
        // The context a caller adds does not hide it.
        assert!(session_is_busy(&busy.context("while ingesting")));
        assert!(!session_is_busy(&anyhow!("session busy")));
        assert!(!session_is_busy(&anyhow::Error::new(
            IngestError::UnknownSession
        )));
    }

    fn clean_report() -> IngestReport {
        IngestReport {
            session_id: "s".into(),
            inserted: 3,
            already_present: 0,
            pending_responses: 0,
            linked_from_commits: 0,
            cursor: 10,
            transcript_len: 10,
            subagent_sessions: 0,
            subagent_inserted: 0,
            subagent_linked: 0,
            unrecorded_blocks: 0,
            unrecorded_block_types: Vec::new(),
            withheld: 0,
            lost: 0,
            withheld_reasons: Vec::new(),
            omitted_tool_calls: 0,
            omitted_context_items: 0,
        }
    }

    /// The family rescue links the session's own responses and its subagents'
    /// apart, and a rescan that records nothing new still says what it linked.
    #[test]
    fn links_to_the_session_and_to_its_subagents_are_reported_apart() {
        let lines = report_lines(&IngestReport {
            inserted: 0,
            already_present: 12,
            linked_from_commits: 2,
            subagent_sessions: 3,
            subagent_inserted: 0,
            subagent_linked: 7,
            ..clean_report()
        });
        assert_eq!(
            lines,
            [
                "CVC: Claude Code session s: 0 new thought(s), 12 already recorded, 0 pending, 2 linked to session commits; cursor at byte 10 of 10",
                "  plus 3 subagent session(s): 0 new thought(s), 7 linked to commits",
            ]
        );
    }

    #[test]
    fn a_clean_ingest_prints_nothing_about_what_was_left_out() {
        assert!(left_out_lines(&clean_report()).is_empty());
    }

    #[test]
    fn withheld_lost_and_omitted_responses_are_said_with_their_reasons_and_no_text() {
        let lines = left_out_lines(&IngestReport {
            withheld: 2,
            lost: 1,
            withheld_reasons: vec![
                "capture collection exceeds limit".into(),
                "too many secret matches".into(),
            ],
            omitted_tool_calls: 45,
            omitted_context_items: 44,
            ..clean_report()
        });
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(
            lines[0],
            "  could not scrub 3 response(s) within the limits: 2 recorded as a stand-in that says its response was withheld, 1 not recorded at all (capture collection exceeds limit; too many secret matches)"
        );
        assert!(lines[1].contains("45 tool call(s)"), "{lines:?}");
        assert!(lines[1].contains("44 context item(s)"), "{lines:?}");
        assert!(lines[1].contains("cvc-omitted"), "{lines:?}");
        // Only counts and limits: never any text of a response.
        let only_omitted = left_out_lines(&IngestReport {
            omitted_context_items: 1,
            ..clean_report()
        });
        assert_eq!(only_omitted.len(), 1);
    }

    fn object(json: Value) -> Map<String, Value> {
        match json {
            Value::Object(object) => object,
            other => panic!("not an object: {other}"),
        }
    }

    /// A `PostToolUse` payload as Claude Code sends it; ids are made up.
    fn claude_code_payload() -> Value {
        serde_json::json!({
            "session_id": "00000000-0000-4000-8000-0000000000a1",
            "transcript_path": "/home/someone/.claude/projects/-x/00000000-0000-4000-8000-0000000000a1.jsonl",
            "cwd": "/work/example",
            "permission_mode": "default",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "true"},
            "tool_response": {"stdout": ""},
            "tool_use_id": "tool-use-1"
        })
    }

    const NO_CURSOR_ENV: Option<&OsStr> = None;

    #[test]
    fn a_payload_with_cursors_keys_is_cursors() {
        // `cursor_version`, which every Cursor hook input carries, alone.
        let mut payload = object(claude_code_payload());
        payload.insert("cursor_version".into(), Value::String("1.0.0".into()));
        assert_eq!(
            classify_hook_caller(&payload, NO_CURSOR_ENV),
            HookCaller::Foreign(ForeignAgent::Cursor)
        );
        // `conversation_id` in a payload without `session_id`.
        let mut payload = object(claude_code_payload());
        payload.remove("session_id");
        payload.insert("conversation_id".into(), Value::String("c1".into()));
        assert_eq!(
            classify_hook_caller(&payload, NO_CURSOR_ENV),
            HookCaller::Foreign(ForeignAgent::Cursor)
        );
        // Present is enough, whatever the value.
        let payload = object(serde_json::json!({
            "conversation_id": null,
            "hook_event_name": "stop",
            "transcript_path": "/home/someone/.cursor/projects/x/agent-transcripts/y.jsonl"
        }));
        assert_eq!(
            classify_hook_caller(&payload, Some(OsStr::new("1.0.0"))),
            HookCaller::Foreign(ForeignAgent::Cursor)
        );
    }

    #[test]
    fn a_camel_case_payload_is_antigravitys() {
        for key in ANTIGRAVITY_PAYLOAD_KEYS {
            let payload = object(serde_json::json!({ key: "x" }));
            assert_eq!(
                classify_hook_caller(&payload, NO_CURSOR_ENV),
                HookCaller::Foreign(ForeignAgent::Antigravity),
                "{key}"
            );
        }
        for payload in [
            serde_json::json!({
                "conversationId": "00000000-0000-4000-8000-0000000000b1",
                "transcriptPath": "/x/y.jsonl",
                "workspacePaths": ["/work/example"]
            }),
            // Only the keys Antigravity's changelog documents.
            serde_json::json!({
                "hookEventName": "PostToolUse",
                "cwd": "/work/example",
                "toolCall": {"name": "run_command", "args": {"command": "true"}}
            }),
            serde_json::json!({"hookEventName": "Stop", "lastUserInput": "Say hello"}),
        ] {
            assert_eq!(
                classify_hook_caller(&object(payload.clone()), NO_CURSOR_ENV),
                HookCaller::Foreign(ForeignAgent::Antigravity),
                "{payload}"
            );
        }
    }

    /// A tool's input and output are the model's and the tool's to fill, so a
    /// key below the top level says nothing about the caller.
    #[test]
    fn keys_below_the_top_level_do_not_count() {
        let mut payload = object(claude_code_payload());
        payload.insert(
            "tool_input".into(),
            serde_json::json!({
                "conversationId": "x",
                "transcriptPath": "/x",
                "workspacePaths": ["/x"],
                "cursor_version": "1",
                "conversation_id": "x",
                "nested": {"conversationId": "x"}
            }),
        );
        payload.insert(
            "tool_response".into(),
            serde_json::json!([{"cursor_version": "1"}]),
        );
        assert_eq!(
            classify_hook_caller(&payload, NO_CURSOR_ENV),
            HookCaller::ClaudeCode { cursor_env: false }
        );
    }

    #[test]
    fn a_claude_code_payload_is_claude_codes_whatever_the_environment_says() {
        let payload = object(claude_code_payload());
        assert_eq!(
            classify_hook_caller(&payload, None),
            HookCaller::ClaudeCode { cursor_env: false }
        );
        // Claude Code run in Cursor's integrated terminal may inherit the
        // variable (not verified): that alone never makes its run another
        // agent's.
        assert_eq!(
            classify_hook_caller(&payload, Some(OsStr::new("2.0.0"))),
            HookCaller::ClaudeCode { cursor_env: true }
        );
        assert_eq!(
            classify_hook_caller(&payload, Some(OsStr::new(""))),
            HookCaller::ClaudeCode { cursor_env: false }
        );
        // Nor does it with a payload that names nothing at all (the plugin's
        // probe sends only a working directory).
        let probe = object(serde_json::json!({"cwd": "/nonexistent/cvc-plugin-probe"}));
        assert_eq!(
            classify_hook_caller(&probe, Some(OsStr::new("2.0.0"))),
            HookCaller::ClaudeCode { cursor_env: true }
        );
        assert_eq!(
            classify_hook_caller(&Map::new(), None),
            HookCaller::ClaudeCode { cursor_env: false }
        );
    }

    const SESSION_ID: &str = "00000000-0000-4000-8000-0000000000a1";

    #[test]
    fn a_hook_passes_another_agents_transcript_over_only_when_corroborated() {
        let own = Path::new("/x/00000000-0000-4000-8000-0000000000a1.jsonl");
        let elsewhere =
            Path::new("/x/agent-transcripts/00000000-0000-4000-8000-0000000000c1.jsonl");
        let hook = Caller::Hook { cursor_env: false };
        let under_cursor = Caller::Hook { cursor_env: true };
        let verdict =
            |caller, first, path| first_entry_verdict(caller, first, path, Some(SESSION_ID));

        // Corroborated: the variable, or a file not named for the session.
        assert_eq!(
            verdict(hook, FirstEntry::NotClaudeCode, elsewhere),
            Verdict::PassOver
        );
        assert_eq!(
            verdict(under_cursor, FirstEntry::NotClaudeCode, elsewhere),
            Verdict::PassOver
        );
        assert_eq!(
            verdict(under_cursor, FirstEntry::NotClaudeCode, own),
            Verdict::PassOver
        );
        // The session's own file, with nothing else to go by: visible.
        assert_eq!(
            verdict(hook, FirstEntry::NotClaudeCode, own),
            Verdict::Refuse
        );
        assert_eq!(
            verdict(
                hook,
                FirstEntry::NotClaudeCode,
                Path::new("/x/00000000-0000-4000-8000-0000000000a1")
            ),
            Verdict::Refuse,
            "a stem with no extension"
        );

        for caller in [hook, under_cursor] {
            for path in [own, elsewhere] {
                // A Claude Code transcript, and one only the parser can judge,
                // reach the parser, whose version pinning must stay loud.
                assert_eq!(
                    verdict(caller, FirstEntry::ClaudeCode, path),
                    Verdict::Ingest
                );
                assert_eq!(
                    verdict(caller, FirstEntry::Undetermined, path),
                    Verdict::Ingest
                );
            }
        }
        assert_eq!(
            verdict(hook, FirstEntry::NoCompleteEntry, own),
            Verdict::Ingest
        );
        assert_eq!(
            verdict(under_cursor, FirstEntry::NoCompleteEntry, own),
            Verdict::PassOver
        );

        for first in [
            FirstEntry::ClaudeCode,
            FirstEntry::NoCompleteEntry,
            FirstEntry::Undetermined,
        ] {
            for path in [own, elsewhere] {
                assert_eq!(
                    first_entry_verdict(Caller::Operator, first, path, None),
                    Verdict::Ingest,
                    "{first:?}"
                );
            }
        }
        for session in [None, Some(SESSION_ID)] {
            assert_eq!(
                first_entry_verdict(
                    Caller::Operator,
                    FirstEntry::NotClaudeCode,
                    elsewhere,
                    session
                ),
                Verdict::Refuse
            );
        }
    }

    /// Every top-level key Claude Code's hook input documents, all at once and
    /// each alone: none of them makes a run another agent's.
    #[test]
    fn claude_codes_documented_payload_keys_are_not_another_agents() {
        let keys = [
            "session_id",
            "transcript_path",
            "cwd",
            "permission_mode",
            "hook_event_name",
            "tool_name",
            "tool_input",
            "tool_response",
            "tool_use_id",
            "prompt",
            "stop_hook_active",
            "source",
            "reason",
            "trigger",
            "custom_instructions",
            "message",
            "agent_id",
            "agent_type",
            "agent_transcript_path",
            "model",
        ];
        let all: Map<String, Value> = keys
            .iter()
            .map(|key| ((*key).to_owned(), Value::String("x".into())))
            .collect();
        assert_eq!(
            classify_hook_caller(&all, NO_CURSOR_ENV),
            HookCaller::ClaudeCode { cursor_env: false }
        );
        for key in keys {
            let one: Map<String, Value> =
                [(key.to_owned(), Value::Bool(true))].into_iter().collect();
            assert_eq!(
                classify_hook_caller(&one, NO_CURSOR_ENV),
                HookCaller::ClaudeCode { cursor_env: false },
                "{key}"
            );
        }
    }

    /// `conversation_id` is Cursor's only where Claude Code's `session_id` is
    /// missing; `cursor_version` is Cursor's whatever else is there.
    #[test]
    fn a_conversation_id_beside_a_session_id_is_not_cursors() {
        let mut payload = object(claude_code_payload());
        payload.insert("conversation_id".into(), Value::String("c1".into()));
        assert_eq!(
            classify_hook_caller(&payload, NO_CURSOR_ENV),
            HookCaller::ClaudeCode { cursor_env: false }
        );
        payload.insert("cursor_version".into(), Value::String("1.0.0".into()));
        assert_eq!(
            classify_hook_caller(&payload, NO_CURSOR_ENV),
            HookCaller::Foreign(ForeignAgent::Cursor)
        );
    }
}
