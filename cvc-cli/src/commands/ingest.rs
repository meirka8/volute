//! `cvc ingest`: record an agent-harness session transcript locally.
//!
//! Not to be confused with sync ingestion (`cvc pull`), which imports another
//! machine's already-shared projection: this is first-party local capture.
use anyhow::{anyhow, bail, Context, Result};
use cvc_core::claude_code::{ingest, IngestError, IngestMode, IngestReport};
use cvc_core::db::CvcStore;
use cvc_core::privacy::PreparedPolicy;
use cvc_core::repository::{RepositoryLayout, RepositoryLayoutError};
use git2::ErrorCode;
use serde::Deserialize;
use std::env;
use std::ffi::OsString;
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

/// The subset of Claude Code's hook payload this command relies on.
#[derive(Deserialize)]
struct HookPayload {
    session_id: Option<String>,
    transcript_path: Option<String>,
    cwd: Option<String>,
    hook_event_name: Option<String>,
    agent_id: Option<String>,
}

pub async fn claude_code(
    transcript: Option<PathBuf>,
    session: Option<String>,
    hook: bool,
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
    )?;
    println!(
        "CVC: Claude Code session {}: {} new thought(s), {} already recorded, {} pending, {} linked to session commits; cursor at byte {} of {}",
        report.session_id,
        report.inserted,
        report.already_present,
        report.pending_responses,
        report.linked_from_commits,
        report.cursor,
        report.transcript_len
    );
    if report.subagent_sessions > 0 {
        println!(
            "  plus {} subagent session(s): {} new thought(s), {} linked to commits",
            report.subagent_sessions, report.subagent_inserted, report.subagent_linked
        );
    }
    if report.unrecorded_blocks > 0 {
        println!(
            "  {} content block(s) of a type this version does not record were kept as placeholders: {}",
            report.unrecorded_blocks,
            report.unrecorded_block_types.join(", ")
        );
    }
    Ok(())
}

/// Hook mode reads Claude Code's JSON payload from stdin and writes nothing to
/// stdout, which Claude Code would interpret as hook output. Failures exit
/// non-zero so they are visible in the session, and never with status 2, the
/// only exit status Claude Code treats as blocking.
///
/// The same command is run by the checkout's own hook entries and by the cvc
/// Claude Code plugin, which is installed for the user and so fires in every
/// project they open, CVC-initialized or not. It therefore decides, in this
/// order:
///
/// 1. The payload is read from stdin, bounded at 1 MiB. Nothing in it is
///    validated yet. A larger one is read on, unused, up to a cap, so that the
///    writer does not meet a closed pipe.
/// 2. The repository is chosen (see [`recording_repository`]): a session is
///    recorded, in full, in the repository it was started in, which Claude
///    Code exports to every hook as `CLAUDE_PROJECT_DIR`. The hook's working
///    directory (the payload's `cwd`) only picks which of that repository's
///    worktrees a run is attributed to, so a session that has since moved to
///    another folder or repository still ends up whole where it began. Without
///    the variable (an older Claude Code, a manual run) the working directory
///    chooses the repository; with it empty, the session was not started in
///    any repository that can be named.
/// 3. Unless that repository can be established as CVC-initialized, the hook
///    does nothing: exit 0 with nothing on stdout or stderr, whatever the
///    payload held. "Established" means the directory exists and is a Git
///    working tree with a CVC directory; one that does not exist, is in no
///    repository, is in a bare one, is in one Git itself refuses as owned by
///    someone else, or is in one without CVC is simply not CVC's business (see
///    [`is_not_cvcs_business`]). Any other discovery failure (an unreadable or
///    corrupt repository, a `cvc` entry that is not a directory) may hide
///    capture loss in an initialized repository, and fails visibly.
/// 4. Only then the payload must be valid (a session id and a transcript
///    path), or the hook fails; subagent payloads are ignored, since the
///    parent session's hooks ingest that transcript; capture must be
///    acknowledged, or the hook fails with `consent-required`; and the
///    transcript is ingested, under the session's lock. A hook that is still
///    waiting for another run on the session when the wait ends does nothing,
///    as in 3: that run is making progress, and the next event catches up
///    from the cursor.
fn run_hook() -> Result<()> {
    let input = read_payload(io::stdin().lock());
    // Parsed leniently here: a payload that is not usable is reported below,
    // once the repository is known to be CVC's.
    let parsed = (input.read.is_ok() && !input.oversize)
        .then(|| serde_json::from_slice::<HookPayload>(&input.raw));

    let payload_cwd = parsed
        .as_ref()
        .and_then(|parsed| parsed.as_ref().ok())
        .and_then(|payload| payload.cwd.as_deref())
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from);
    let Some(layout) = recording_repository(env::var_os(PROJECT_DIR_ENV), payload_cwd)? else {
        return Ok(());
    };

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
    let session = payload
        .session_id
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("hook payload has no session_id"))?;
    let transcript = payload
        .transcript_path
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("hook payload has no transcript_path"))?;
    let mode = match payload.hook_event_name.as_deref() {
        Some("Stop") | Some("SessionEnd") => IngestMode::Final,
        _ => IngestMode::Incremental,
    };
    match ingest_into(&layout, &transcript, Some(&session), mode) {
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
) -> Result<IngestReport> {
    let layout = super::harness::discover_initialized_at(repository_path)?;
    ingest_into(&layout, transcript, session, mode)
}

fn ingest_into(
    layout: &RepositoryLayout,
    transcript: &Path,
    session: Option<&str>,
    mode: IngestMode,
) -> Result<IngestReport> {
    super::harness::require_capture_acknowledged(layout)?;
    let policy = PreparedPolicy::load(layout.policy_root()?)
        .map_err(|error| anyhow!("CVC capture blocked by .thoughtignore: {error}"))?;
    let store = CvcStore::open_initialized(layout.db_path())?;
    Ok(ingest(layout, &store, &policy, transcript, session, mode)?)
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
}
