//! The hook-mode shell: one code path for `cvc ingest <harness> --hook`,
//! whichever harness runs it.
//!
//! A hook must never block or steer the agent that runs it. So the shell
//! never exits with status 2, which every supported agent treats as blocking
//! (a failure is status 1, and the cvc Claude Code plugin's wrapper turns a 2
//! into a 1 as well), and writes on stdout exactly what the harness's
//! [`OutputContract`] says, on every path of the shell, a panic included:
//! nothing, `{}`, or the harness's "continue" JSON. Claude Code's contract is
//! nothing at all. A panic is reported by cvc's source location only, never
//! by its message, which can carry what was being worked on.
//!
//! The same command is run by a checkout's own hook entries and by user-scoped
//! hooks and plugins, which fire in every project the user opens,
//! CVC-initialized or not. Other agents run it too: Cursor loads Claude Code's
//! hook settings by default, and Antigravity's CLI can import Claude Code
//! plugins, each handing it its own payload. A run therefore decides, in this
//! order (#189, made generic):
//!
//! 1. The payload is read from stdin, bounded at 1 MiB. Nothing in it is
//!    validated yet. A larger one is read on, unused, up to a cap, so that the
//!    writer does not meet a closed pipe.
//! 2. The output contract for the payload's event is written and flushed,
//!    before any work, so that a run killed at its timeout still leaves valid
//!    output.
//! 3. The adapter classifies the caller ([`HookCaller::classify`]) from the
//!    payload's top-level keys and the environment. Another agent's run exits
//!    0 with nothing on stderr, before any repository is looked for or any
//!    file read.
//! 4. The repository is chosen from where the session started
//!    ([`HookCaller::start`]; Claude Code: `CLAUDE_PROJECT_DIR`): a session is
//!    recorded, in full, in the repository it was started in. The hook's
//!    working directory (the payload's `cwd`) only picks which of that
//!    repository's worktrees a run is attributed to. When the harness does
//!    not say where the session started, the working directory, else the
//!    process's own directory, is the one in question; when it says the
//!    session started nowhere that can be named, nothing is recorded. For a
//!    harness whose start is in its record's first entry
//!    ([`StartLocation::FromRecordStart`]; Claude Code's never is), the
//!    shell reads that one line, at most 4 MiB of a regular file, in memory,
//!    for the adapter to read the start from, and drops it: the only read of
//!    a record before the repository is known.
//! 5. Unless that repository can be established as CVC-initialized, the hook
//!    does nothing: exit 0 with nothing on stderr, whatever the payload held
//!    (see [`initialized_repository_at`]).
//! 6. Only then the payload must be valid (read whole within the bound, the
//!    JSON the harness sends, with what a run needs), or the hook fails,
//!    unless the environment is another agent's too (Claude Code under
//!    Cursor's `CURSOR_VERSION`), when a payload that is not usable is taken
//!    for that agent's and the hook does nothing. A payload the harness's own
//!    parent run ingests (a Claude Code subagent's) is ignored.
//! 7. The capture acknowledgement must cover the harness's own channel, or
//!    the hook fails with `consent-required` and the adapter's hint on how to
//!    stop the hooks.
//! 8. Before the CVC database is opened, the adapter locates and screens the
//!    record the payload names ([`cvc_core::harness::adapter::RecordSource::admit_hook`]): Claude Code
//!    opens the transcript once and reads it up to its first complete line.
//!    A record that is not the harness's is passed over in silence when that
//!    is corroborated, and refused visibly otherwise.
//! 9. The policy is loaded, the store opened, the adapter records the event
//!    itself where the payload is its record ([`cvc_core::harness::adapter::RecordSource::record_event`],
//!    with the coverage grant held), and the engine ingests, by the deadline
//!    of the run's start plus the harness's budget. A hook that is still
//!    waiting for another run on the session when the lock wait ends does
//!    nothing, as in 5: that run is making progress, and the next event
//!    catches up from the cursor.
use super::{explain, load_policy};
use anyhow::{anyhow, bail, Context, Result};
use cvc_core::db::CvcStore;
use cvc_core::harness::adapter::{
    hook_request, json_type, Admission, CallerVerdict, HarnessAdapter, HookCaller, HookEnv,
    HookEvent, HookRequest, OutputContract, PayloadError, ProcessEnv, StartLocation,
};
use cvc_core::harness::candidate::Finality;
use cvc_core::harness::engine::{self, EngineError, EngineOptions};
use cvc_core::harness::{self, AdapterVisitor, HarnessId, SESSION_LOCK_WAIT};
use cvc_core::privacy::CoverageGrant;
use cvc_core::repository::{RepositoryLayout, RepositoryLayoutError};
use git2::ErrorCode;
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::env;
use std::fs;
use std::io::{self, BufRead, ErrorKind, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Instant;

const MAX_HOOK_PAYLOAD_BYTES: u64 = 1024 * 1024;
/// An oversize payload is still read to here, into nowhere, before the hook
/// decides: an agent writes the whole payload to the hook's stdin, and a
/// process that exits with part of it unread hands the writer a broken pipe.
const MAX_DRAINED_HOOK_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// `cvc ingest <harness> --hook`, for a harness this build has an adapter for.
pub(crate) fn run_hook(harness: HarnessId) -> Result<()> {
    let started = crate::started();
    harness::with_adapter(harness, Hook { started }).unwrap_or_else(|| {
        Err(refuse_unavailable(
            harness,
            HookIo {
                stdin: &mut io::stdin().lock(),
                stdout: &mut io::stdout(),
            },
        ))
    })
}

/// `cvc ingest <harness> --hook` for a harness this build has no adapter
/// for: the payload is read off the pipe with the usual bound, the harness's
/// static contract ([`HarnessId::hook_contract`]) is written, and the
/// refusal is returned, for exit status 1.
pub(crate) fn refuse_unavailable(harness: HarnessId, io: HookIo<'_>) -> anyhow::Error {
    let HookIo { stdin, stdout } = io;
    drop(read_payload(stdin));
    ContractOut {
        out: stdout,
        written: false,
    }
    .emit(harness.hook_contract());
    super::super::harness::not_available(harness)
}

struct Hook {
    started: Instant,
}

impl AdapterVisitor for Hook {
    type Output = Result<()>;

    fn visit<A: HarnessAdapter>(self, adapter: &A) -> Result<()> {
        run_hook_with(
            adapter,
            A::ID,
            &EngineCapture,
            HookIo {
                stdin: &mut io::stdin().lock(),
                stdout: &mut io::stdout(),
            },
            &ProcessEnv,
            self.started,
        )
    }
}

/// Where a hook run reads its payload and writes its output.
pub(crate) struct HookIo<'a> {
    pub(crate) stdin: &'a mut dyn Read,
    pub(crate) stdout: &'a mut dyn Write,
}

/// What a run does once the acknowledgement covers the harness's channel:
/// locate and screen the record, open the store, record the event, run the
/// engine. A trait so that the shell's own decisions can be tested with an
/// adapter the engine would not record for.
pub(crate) trait Capture<C: ?Sized> {
    fn capture(&self, caller: &C, run: CaptureRun<'_>) -> Result<Captured>;
}

/// Everything [`Capture::capture`] runs with.
pub(crate) struct CaptureRun<'a> {
    pub(crate) harness: HarnessId,
    pub(crate) request: &'a HookRequest,
    pub(crate) layout: &'a RepositoryLayout,
    /// The coverage the shell found, held through the adapter's own write
    /// ([`cvc_core::harness::adapter::RecordSource::record_event`]). The
    /// engine asks for its own.
    pub(crate) grant: CoverageGrant,
    pub(crate) foreign_env: Option<HarnessId>,
    pub(crate) finality: Finality,
    /// The run's start plus the harness's budget for the event.
    pub(crate) deadline: Instant,
}

/// How a run that got past the consent check ended without failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Captured {
    Recorded,
    /// The record is not the harness's, as far as the run can corroborate.
    PassedOver,
    /// Another run held the session's lock for the whole wait.
    Busy,
}

/// The real thing: the adapter's own screening and event, then the engine.
pub(crate) struct EngineCapture;

impl<A: HarnessAdapter> Capture<A> for EngineCapture {
    fn capture(&self, adapter: &A, run: CaptureRun<'_>) -> Result<Captured> {
        // The adapter's one write before the engine is made only under a
        // grant for its own channel.
        anyhow::ensure!(
            run.harness == A::ID && run.grant.channel() == A::ID.channel(),
            "the capture acknowledgement was checked for another channel; nothing was recorded"
        );
        let located = match adapter.admit_hook(run.request, run.layout, run.foreign_env)? {
            Admission::Ingest(located) => located,
            Admission::PassOver => return Ok(Captured::PassedOver),
        };
        let policy = load_policy(run.layout)?;
        // A hook's open runs the store's one-time capture-source upgrade only
        // where it costs nothing or where cvc 0.7.x would run it in the same
        // open; a populated 0.7.x store waits for an operator command.
        let store = CvcStore::open_initialized(run.layout.db_path())?;
        adapter.record_event(
            run.request,
            &HookEvent {
                layout: run.layout,
                store: &store,
                policy: &policy,
                grant: &run.grant,
            },
        )?;
        drop(run.grant);
        match engine::ingest_until(
            adapter,
            run.layout,
            &store,
            &policy,
            located,
            EngineOptions {
                finality: run.finality,
                // The hook reads from the cursor, always: only an operator
                // rescans.
                rescan: false,
                lock_wait: SESSION_LOCK_WAIT,
            },
            Some(run.deadline),
        ) {
            Ok(_) => Ok(Captured::Recorded),
            // Another run holds this session's lock and has held it for the
            // whole wait. It is making progress, and whatever it does not
            // reach, the next event's run reads from the cursor; a hook that
            // failed here would only report a delay as an error.
            Err(EngineError::Busy { .. }) => Ok(Captured::Busy),
            Err(error) => Err(explain(error, &store, run.harness, adapter.stop_hint())),
        }
    }
}

/// The hook's stdout: the contract is written once, whatever happens.
struct ContractOut<'a> {
    out: &'a mut dyn Write,
    written: bool,
}

impl ContractOut<'_> {
    /// Writes `contract` and flushes, unless a contract was written already.
    /// A stdout that cannot be written to has no reader left to tell.
    fn emit(&mut self, contract: OutputContract) {
        if self.written {
            return;
        }
        self.written = true;
        let text = match contract {
            OutputContract::Silent => return,
            OutputContract::EmptyObject => "{}",
            OutputContract::Literal(text) => text,
        };
        let _ = self.out.write_all(text.as_bytes());
        let _ = self.out.flush();
    }
}

thread_local! {
    /// This thread is inside a hook run's catch: a panic is recorded by its
    /// location only, and printed by no one.
    static CATCHING: Cell<bool> = const { Cell::new(false) };
    /// Where the last panic caught by a hook run on this thread was raised.
    static PANICKED_AT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Installs, once per process, the panic hook of hook mode. A panic's message
/// can carry what was being worked on (a slice of captured text, a transcript
/// path in a `Debug` value), and an agent shows a hook's stderr to its user.
/// Inside a hook run the hook records only cvc's source location of the
/// panic and prints nothing; elsewhere it hands the panic to the hook it
/// replaced.
fn install_panic_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if CATCHING.with(Cell::get) {
                let at = info
                    .location()
                    .map(|location| format!("{}:{}", location.file(), location.line()));
                PANICKED_AT.with(|panicked| *panicked.borrow_mut() = at);
            } else {
                previous(info);
            }
        }));
    });
}

/// Marks this thread as catching for as long as it lives.
struct Catching;

impl Catching {
    fn start() -> Self {
        install_panic_hook();
        PANICKED_AT.with(|panicked| panicked.borrow_mut().take());
        CATCHING.with(|catching| catching.set(true));
        Catching
    }
}

impl Drop for Catching {
    fn drop(&mut self) {
        CATCHING.with(|catching| catching.set(false));
    }
}

/// One hook run with `caller` (the adapter of `harness`) and `capture`, from
/// `started`. `Ok` is exit status 0 and `Err` is 1: there is no other
/// outcome, so the run never exits 2. A panic anywhere in the run is caught,
/// the contract is still written, and the run fails with status 1, saying
/// only where in cvc's source it stopped.
pub(crate) fn run_hook_with<C, K>(
    caller: &C,
    harness: HarnessId,
    capture: &K,
    io: HookIo<'_>,
    env: &dyn HookEnv,
    started: Instant,
) -> Result<()>
where
    C: HookCaller,
    K: Capture<C>,
{
    let HookIo { stdin, stdout } = io;
    let mut out = ContractOut {
        out: stdout,
        written: false,
    };
    let catching = Catching::start();
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        decide(caller, harness, capture, stdin, &mut out, env, started)
    }));
    // A panic before the contract was written: the contract of no event, or
    // nothing at all if even that cannot be had.
    if !out.written {
        let contract = panic::catch_unwind(AssertUnwindSafe(|| caller.contract(None)))
            .unwrap_or(OutputContract::Silent);
        out.emit(contract);
    }
    let panicked_at = PANICKED_AT.with(|panicked| panicked.borrow_mut().take());
    drop(catching);
    match outcome {
        Ok(result) => result,
        Err(_) => Err(anyhow!(
            "cvc stopped on an internal error at {}; this hook run recorded nothing more",
            panicked_at.as_deref().unwrap_or("an unknown place")
        )),
    }
}

/// The payload's top level, which must be a JSON object. What is wrong with
/// it is said without quoting any of it.
fn top_level(harness: HarnessId, raw: &[u8]) -> Result<Map<String, Value>, PayloadError> {
    let detail = match serde_json::from_slice::<Value>(raw) {
        Ok(Value::Object(object)) => return Ok(object),
        Ok(other) => format!("it is {}, not an object", json_type(&other)),
        Err(error) => format!(
            "it is not JSON (line {}, column {})",
            error.line(),
            error.column()
        ),
    };
    Err(PayloadError::Malformed {
        harness: harness.display(),
        detail,
    })
}

fn decide<C, K>(
    caller: &C,
    harness: HarnessId,
    capture: &K,
    stdin: &mut dyn Read,
    out: &mut ContractOut<'_>,
    env: &dyn HookEnv,
    started: Instant,
) -> Result<()>
where
    C: HookCaller,
    K: Capture<C>,
{
    let input = read_payload(stdin);
    // Parsed leniently here: a payload that is not usable is reported below,
    // once the repository is known to be CVC's. A payload that could not be
    // read or parsed is classified by the environment alone, so that it is
    // covered too (see step 6). The caller is classified before the payload
    // is handed to the request, which keeps it.
    let parsed = (input.read.is_ok() && !input.oversize).then(|| top_level(harness, &input.raw));
    let (verdict, request) = match parsed {
        Some(Ok(object)) => {
            let verdict = caller.classify(&object, env);
            (verdict, Some(hook_request(caller, object)))
        }
        Some(Err(error)) => (caller.classify(&Map::new(), env), Some(Err(error))),
        None => (caller.classify(&Map::new(), env), None),
    };
    let readable = request.as_ref().and_then(|request| request.as_ref().ok());
    out.emit(caller.contract(readable.and_then(|request| request.event.as_deref())));
    let foreign_env = match verdict {
        CallerVerdict::Foreign(_) => return Ok(()),
        CallerVerdict::Ours { foreign_env } => foreign_env,
    };

    let unknown = HookRequest::default();
    let start = match caller.start(readable.unwrap_or(&unknown), env) {
        // Read from the record only for an adapter whose start is there.
        StartLocation::FromRecordStart(record) => match first_line_of(&record) {
            Some(line) => match caller.start_from_first_entry(&line) {
                StartLocation::FromRecordStart(_) => StartLocation::Unknown,
                start => start,
            },
            None => StartLocation::Unknown,
        },
        start => start,
    };
    let cwd = readable.and_then(|request| request.cwd.clone());
    let Some(layout) = recording_repository(start, cwd)? else {
        return Ok(());
    };

    // Under another agent's environment, a payload that could not be read
    // whole, or that is not the JSON this harness sends, is taken for that
    // agent's own (a Cursor `PostToolUse` whose tool output went past the
    // bound, say). Nothing could be ingested from it in any case.
    if foreign_env.is_some() && readable.is_none() {
        return Ok(());
    }
    input
        .read
        .context("could not read the hook payload from stdin")?;
    if input.oversize {
        bail!("hook payload exceeds 1 MiB");
    }
    let request = match request {
        Some(Ok(request)) => request,
        Some(Err(error)) => return Err(error.into()),
        None => bail!("hook payload was not read"),
    };
    if !caller.wants(&request) {
        return Ok(());
    }
    match caller.check_complete(&request) {
        Ok(()) => {}
        // The harness always sends it; under another agent's environment, a
        // payload without it is taken for that agent's own.
        Err(_) if foreign_env.is_some() => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let event = request.event.as_deref();
    let finality = caller.finality(event);
    let deadline = started + caller.budget(event);
    let grant =
        super::super::harness::require_capture_covered(&layout, harness, caller.stop_hint())?;
    capture.capture(
        caller,
        CaptureRun {
            harness,
            request: &request,
            layout: &layout,
            grant,
            foreign_env,
            finality,
            deadline,
        },
    )?;
    Ok(())
}

/// The most of a record's first line a start is read from.
const MAX_FIRST_LINE_BYTES: u64 = 4 * 1024 * 1024;

/// The first complete line of the record at `path`, for
/// [`HookCaller::start_from_first_entry`], or `None` when there is none to
/// read: the path is not a regular file (a link, a FIFO, a directory), it
/// cannot be read, or its first line is longer than
/// [`MAX_FIRST_LINE_BYTES`] or not finished yet. This is the one read of a
/// record before the repository is known: bounded, kept in memory, and
/// dropped by the caller as soon as the start is known.
fn first_line_of(path: &Path) -> Option<Vec<u8>> {
    let file = open_record(path)?;
    let mut line = Vec::new();
    io::BufReader::new(file.take(MAX_FIRST_LINE_BYTES))
        .read_until(b'\n', &mut line)
        .ok()?;
    line.ends_with(b"\n").then_some(line)
}

/// `path` opened for reading if it is a regular file: on Unix without
/// following a link, without blocking (a FIFO opens at once instead of
/// waiting for a writer) and without becoming a controlling terminal, and
/// checked on the handle opened.
#[cfg(unix)]
fn open_record(path: &Path) -> Option<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

#[cfg(not(unix))]
fn open_record(path: &Path) -> Option<fs::File> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    file.metadata().ok()?.is_file().then_some(file)
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

/// The CVC-initialized repository `directory` is in, or `None` when it can be
/// established that there is none. `Err` is a failure to tell, which hook mode
/// must not mistake for "no".
///
/// "Established" means the directory exists and is a Git working tree with a
/// CVC directory; one that does not exist, is in no repository, is in a bare
/// one, is in one Git itself refuses as owned by someone else, or is in one
/// without CVC is simply not CVC's business (see [`is_not_cvcs_business`]).
/// Any other discovery failure (an unreadable or corrupt repository, a `cvc`
/// entry that is not a directory) may hide capture loss in an initialized
/// repository, and fails visibly.
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
/// A session is recorded, in full, in the repository it was started in. A
/// checkout's own settings load only for sessions started in it, but
/// user-scoped hooks fire in every session, and a session moves: into a
/// folder added with `/add-dir`, into another repository, into a nested one.
/// Choosing the repository by where the session happens to be would take the
/// part of a record that follows it into a repository it was never started
/// in, and leave the one it was started in without its end. So the repository
/// is the one the start directory is in, judged as
/// [`initialized_repository_at`] judges any directory. Its worktree is the
/// start directory's own, unless the hook's working directory (`cwd`) is in
/// the same repository (the same canonical common Git directory, which makes a
/// linked worktree and a subdirectory the same repository): then it is the
/// working directory's worktree, as a session that works in a linked worktree
/// is attributed to it. The working directory never chooses the repository.
///
/// With no start to go by ([`StartLocation::Unknown`]: an older Claude Code
/// without `CLAUDE_PROJECT_DIR`, a manual run), the working directory, else
/// the process's own directory, is the one in question. A start that cannot
/// be named ([`StartLocation::NotNameable`]: `CLAUDE_PROJECT_DIR` set and
/// empty) records nothing.
fn recording_repository(
    start: StartLocation,
    payload_cwd: Option<PathBuf>,
) -> Result<Option<RepositoryLayout>> {
    let directory = match start {
        StartLocation::NotNameable => return Ok(None),
        StartLocation::Unknown => {
            let directory = match payload_cwd {
                Some(cwd) => cwd,
                None => match env::current_dir() {
                    Ok(directory) => directory,
                    // The process's own directory was removed: no repository.
                    Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                    Err(error) => {
                        return Err(error).context("cannot determine the current directory")
                    }
                },
            };
            return initialized_repository_at(&directory);
        }
        StartLocation::Directory(directory) | StartLocation::FromRecordStart(directory) => {
            directory
        }
    };
    if directory.as_os_str().is_empty() {
        return Ok(None);
    }
    let Some(project) = initialized_repository_at(&directory)? else {
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

#[cfg(test)]
mod tests;
