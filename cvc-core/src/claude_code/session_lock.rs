//! An exclusive, per-session lock around the ingestion of one transcript.
//!
//! The same hook event can start several `cvc ingest claude-code` processes
//! for one session at once (the Claude Code plugin's hook and a checkout's own
//! entries both fire, in parallel), and an operator can run `--transcript` by
//! hand while hooks fire. Each plans from the stored cursor and commits the new
//! captures with the advanced cursor. The deterministic ids keep that correct,
//! but only the lock makes it orderly: the second run starts from the cursor the
//! first one advanced, finds nothing new, and neither can move the cursor back.
//!
//! The lock is a file in the repository's CVC directory, which lives in the
//! common Git directory and so is shared by every worktree. It is named from a
//! hash of the harness and the session id, never the id itself. It is held with
//! the same advisory file lock (`fs2`) the privacy state uses, so it is released
//! by the operating system when the process ends however it ends, and it is
//! taken without blocking in a bounded polling loop, because a hook has a
//! deadline and a holder that is making progress is no reason to hold a hook
//! past it.
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long `ingest` waits for another run on the same session. Below the 30 s
/// timeout of the hooks (`settings::HOOK_TIMEOUT_SECS`), so a waiting hook
/// gives up on its own terms rather than being killed.
pub const SESSION_LOCK_WAIT: Duration = Duration::from_secs(20);

const POLL_FIRST: Duration = Duration::from_millis(5);
const POLL_LONGEST: Duration = Duration::from_millis(100);

/// Holds the lock for as long as it lives.
#[derive(Debug)]
pub(crate) struct SessionLock {
    /// The locked file; `None` where the filesystem cannot lock at all, and the
    /// run goes on without the lock (see [`SessionLock::acquire`]).
    held: Option<Held>,
}

#[derive(Debug)]
struct Held {
    file: File,
    /// Only read on Unix, where the file is removed on release.
    #[cfg_attr(not(unix), allow(dead_code))]
    path: PathBuf,
}

impl SessionLock {
    /// Takes the lock for `session` of `harness`, waiting up to `wait` for
    /// whoever holds it. `Ok(None)` means it was still held when the wait ended.
    /// The lock is attempted at least once, so a zero wait is a try.
    ///
    /// On a filesystem that cannot lock at all (see [`is_lock_unsupported`])
    /// the result is a lock that holds nothing, and the run goes on without
    /// one. Ingestion is idempotent, with deterministic ids and a cursor that
    /// only ever advances to what was committed, so the lock only spares
    /// concurrent runs from doing the same work twice; refusing to record
    /// because it cannot be had would trade a harmless overlap for lost
    /// capture. Any other failure to lock is an error.
    ///
    /// `cvc_dir` must already be the repository's CVC directory: this does not
    /// create it, and refuses one that is a link or belongs to someone else, as
    /// the store does.
    pub(crate) fn acquire(
        cvc_dir: &Path,
        harness: &str,
        session: &str,
        wait: Duration,
    ) -> io::Result<Option<Self>> {
        Self::acquire_with(cvc_dir, harness, session, wait, |file| {
            fs2::FileExt::try_lock_exclusive(file)
        })
    }

    /// [`SessionLock::acquire`] with the attempt to lock a file given, so that
    /// what the operating system can answer can be injected.
    fn acquire_with(
        cvc_dir: &Path,
        harness: &str,
        session: &str,
        wait: Duration,
        try_lock: impl Fn(&File) -> io::Result<()>,
    ) -> io::Result<Option<Self>> {
        check_directory(cvc_dir)?;
        let path = cvc_dir.join(file_name(harness, session));
        let deadline = Instant::now() + wait;
        let mut delay = POLL_FIRST;
        loop {
            let file = open(&path)?;
            loop {
                match try_lock(&file) {
                    Ok(()) => break,
                    Err(error) if is_contended(&error) => {
                        let now = Instant::now();
                        if now >= deadline {
                            return Ok(None);
                        }
                        std::thread::sleep(delay.min(deadline - now));
                        delay = (delay * 2).min(POLL_LONGEST);
                    }
                    Err(error) if is_lock_unsupported(&error) => {
                        // Nothing to hold, and nothing to leave behind.
                        drop(file);
                        #[cfg(unix)]
                        let _ = fs::remove_file(&path);
                        return Ok(Some(Self { held: None }));
                    }
                    Err(error) => return Err(error),
                }
            }
            if names_the_same_file(&path, &file)? {
                return Ok(Some(Self {
                    held: Some(Held { file, path }),
                }));
            }
            // The previous holder removed the file (see `Drop`) between our
            // opening it and our getting the lock, so the lock is on a file
            // nobody else will look for. Start again on the path's current one.
            drop(file);
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        let Some(held) = &self.held else {
            return;
        };
        // Removed while still held, so that a run that opened the file while
        // we held it notices (it checks the path against what it locked) and
        // starts over on a new one, and the directory does not collect a file
        // for every session ever recorded. Other platforms keep the file: an
        // open file cannot be told from a deleted one there as reliably.
        #[cfg(unix)]
        let _ = fs::remove_file(&held.path);
        let _ = fs2::FileExt::unlock(&held.file);
    }
}

/// `ingest-<hex SHA-256 of the harness and session>.lock`.
fn file_name(harness: &str, session: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"cvc.ingest-lock/v1\0");
    hash.update(harness.as_bytes());
    hash.update(b"\0");
    hash.update(session.as_bytes());
    format!("ingest-{}.lock", hex::encode(hash.finalize()))
}

fn is_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

/// Whether `error` from taking a file lock says that the filesystem has no
/// locking to offer, as opposed to the lock being held or the attempt failing:
/// a network or FUSE filesystem without a lock manager (`ENOLCK`), one that
/// does not implement the call (`EOPNOTSUPP`/`ENOTSUP`, `ENOSYS`).
fn is_lock_unsupported(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        return code == libc::ENOLCK
            || code == libc::EOPNOTSUPP
            || code == libc::ENOTSUP
            || code == libc::ENOSYS;
    }
    false
}

fn check_directory(directory: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(insecure("CVC directory is not an owned regular directory"));
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_dir() {
        return Err(insecure("CVC directory is not a directory"));
    }
    Ok(())
}

/// Opens (creating) the lock file without following a link at its path, and
/// refuses one that is not a regular file of ours.
fn open(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(insecure("ingest lock is not an owned regular file"));
        }
    }
    Ok(file)
}

/// Whether `path` still names the file `locked` is open on.
#[cfg(unix)]
fn names_the_same_file(path: &Path, locked: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    match fs::symlink_metadata(path) {
        Ok(current) => {
            let locked = locked.metadata()?;
            Ok(current.dev() == locked.dev() && current.ino() == locked.ino())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn names_the_same_file(_path: &Path, _locked: &File) -> io::Result<bool> {
    Ok(true)
}

fn insecure(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const SHORT: Duration = Duration::from_millis(50);

    fn names(directory: &Path) -> Vec<String> {
        let mut found: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        found.sort();
        found
    }

    #[test]
    fn the_name_hides_the_session_and_separates_harnesses_and_sessions() {
        let name = file_name("claude-code", "session-1");
        assert!(!name.contains("session"), "{name}");
        assert!(
            name.starts_with("ingest-") && name.ends_with(".lock"),
            "{name}"
        );
        // 64 hex digits between the affixes.
        let digest = &name["ingest-".len()..name.len() - ".lock".len()];
        assert_eq!(digest.len(), 64, "{name}");
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()), "{name}");

        assert_eq!(name, file_name("claude-code", "session-1"));
        assert_ne!(name, file_name("claude-code", "session-2"));
        assert_ne!(name, file_name("other", "session-1"));
        // The separator keeps neighbouring splits apart.
        assert_ne!(file_name("ab", "c"), file_name("a", "bc"));
    }

    #[test]
    fn a_second_taker_waits_out_its_time_and_gets_the_lock_once_it_is_released() {
        let directory = TempDir::new().unwrap();
        let held = SessionLock::acquire(directory.path(), "h", "s", SHORT)
            .unwrap()
            .expect("an unheld lock is taken");

        let started = Instant::now();
        let busy = SessionLock::acquire(directory.path(), "h", "s", SHORT).unwrap();
        assert!(busy.is_none(), "the lock is held");
        assert!(started.elapsed() >= SHORT, "it waited for its time");

        // Another session is not affected.
        assert!(SessionLock::acquire(directory.path(), "h", "other", SHORT)
            .unwrap()
            .is_some());

        drop(held);
        assert!(SessionLock::acquire(directory.path(), "h", "s", SHORT)
            .unwrap()
            .is_some());
    }

    #[test]
    fn a_zero_wait_is_a_single_try() {
        let directory = TempDir::new().unwrap();
        assert!(
            SessionLock::acquire(directory.path(), "h", "s", Duration::ZERO)
                .unwrap()
                .is_some()
        );
        let _held = SessionLock::acquire(directory.path(), "h", "s", Duration::ZERO)
            .unwrap()
            .unwrap();
        assert!(
            SessionLock::acquire(directory.path(), "h", "s", Duration::ZERO)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_waiter_gets_the_lock_when_the_holder_lets_go_within_its_time() {
        let directory = TempDir::new().unwrap();
        let held = SessionLock::acquire(directory.path(), "h", "s", SHORT)
            .unwrap()
            .unwrap();
        let path = directory.path().to_path_buf();
        let waiter = std::thread::spawn(move || {
            SessionLock::acquire(&path, "h", "s", Duration::from_secs(30))
                .unwrap()
                .is_some()
        });
        std::thread::sleep(Duration::from_millis(100));
        drop(held);
        assert!(waiter.join().unwrap(), "the waiter was given the lock");
    }

    #[test]
    fn the_lock_file_does_not_outlive_the_lock_on_unix() {
        let directory = TempDir::new().unwrap();
        let lock = SessionLock::acquire(directory.path(), "h", "s", SHORT)
            .unwrap()
            .unwrap();
        assert_eq!(names(directory.path()).len(), 1);
        drop(lock);
        if cfg!(unix) {
            assert!(names(directory.path()).is_empty());
        }
    }

    /// Many takers on one session at once: one at a time holds it, and every
    /// one of them gets it in turn, though each removes the file on the way
    /// out, which the next taker may have opened already.
    #[test]
    fn takers_that_race_with_the_removal_of_the_file_still_exclude_each_other() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let directory = TempDir::new().unwrap();
        let inside = Arc::new(AtomicUsize::new(0));
        let overlaps = Arc::new(AtomicUsize::new(0));
        let turns = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = directory.path().to_path_buf();
                let (inside, overlaps, turns) = (inside.clone(), overlaps.clone(), turns.clone());
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        let lock = SessionLock::acquire(&path, "h", "s", Duration::from_secs(60))
                            .unwrap()
                            .expect("within a minute");
                        if inside.fetch_add(1, Ordering::SeqCst) != 0 {
                            overlaps.fetch_add(1, Ordering::SeqCst);
                        }
                        turns.fetch_add(1, Ordering::SeqCst);
                        std::thread::yield_now();
                        inside.fetch_sub(1, Ordering::SeqCst);
                        drop(lock);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(overlaps.load(Ordering::SeqCst), 0, "two held it at once");
        assert_eq!(turns.load(Ordering::SeqCst), 8 * 25);
    }

    #[test]
    fn only_a_filesystem_without_locking_is_unsupported() {
        assert!(is_lock_unsupported(&io::Error::from(
            io::ErrorKind::Unsupported
        )));
        // A held lock, a failure to try, and a refusal are none of them that.
        for kind in [
            io::ErrorKind::WouldBlock,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::NotFound,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::Other,
        ] {
            assert!(!is_lock_unsupported(&io::Error::from(kind)), "{kind:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_errnos_a_filesystem_without_locking_answers_with_are_unsupported() {
        for (name, code) in [
            ("ENOLCK", libc::ENOLCK),
            ("EOPNOTSUPP", libc::EOPNOTSUPP),
            ("ENOTSUP", libc::ENOTSUP),
            ("ENOSYS", libc::ENOSYS),
        ] {
            let error = io::Error::from_raw_os_error(code);
            assert!(is_lock_unsupported(&error), "{name}: {error}");
            assert!(!is_contended(&error), "{name}: {error}");
        }
        for (name, code) in [
            ("EWOULDBLOCK", libc::EWOULDBLOCK),
            ("EIO", libc::EIO),
            ("EACCES", libc::EACCES),
            ("EBADF", libc::EBADF),
            ("EINVAL", libc::EINVAL),
            ("EINTR", libc::EINTR),
            ("ENOMEM", libc::ENOMEM),
        ] {
            let error = io::Error::from_raw_os_error(code);
            assert!(!is_lock_unsupported(&error), "{name}: {error}");
        }
    }

    fn unsupported() -> io::Error {
        io::Error::from(io::ErrorKind::Unsupported)
    }

    #[test]
    fn a_filesystem_that_cannot_lock_gives_a_run_that_goes_on_without_one() {
        let directory = TempDir::new().unwrap();
        let attempts = std::cell::Cell::new(0);
        let none = |_: &File| {
            attempts.set(attempts.get() + 1);
            Err(unsupported())
        };
        let first = SessionLock::acquire_with(directory.path(), "h", "s", SHORT, none)
            .unwrap()
            .expect("the run goes on");
        assert!(first.held.is_none(), "nothing is held");
        assert_eq!(attempts.get(), 1, "it is not retried until the wait ends");
        // And nothing excludes anything: a second run is given the same.
        let second =
            SessionLock::acquire_with(directory.path(), "h", "s", SHORT, |_| Err(unsupported()))
                .unwrap()
                .expect("the run goes on");
        assert!(second.held.is_none());
        // No file is left behind, with the locks held or after they are let go.
        if cfg!(unix) {
            assert!(names(directory.path()).is_empty());
        }
        drop((first, second));
        if cfg!(unix) {
            assert!(names(directory.path()).is_empty());
        }
    }

    #[test]
    fn any_other_failure_to_lock_is_an_error_and_a_held_lock_is_still_a_wait() {
        let directory = TempDir::new().unwrap();
        let failure = |_: &File| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        let error =
            SessionLock::acquire_with(directory.path(), "h", "s", SHORT, failure).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        let held = |_: &File| Err(io::Error::from(io::ErrorKind::WouldBlock));
        let started = Instant::now();
        let busy = SessionLock::acquire_with(directory.path(), "h", "s", SHORT, held).unwrap();
        assert!(busy.is_none());
        assert!(started.elapsed() >= SHORT);
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_is_a_link_or_a_lock_that_is_a_link_is_refused() {
        use std::os::unix::fs::symlink;

        let real = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        let link = elsewhere.path().join("cvc");
        symlink(real.path(), &link).unwrap();
        let error = SessionLock::acquire(&link, "h", "s", SHORT).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        assert!(names(real.path()).is_empty());

        let missing = elsewhere.path().join("absent");
        let error = SessionLock::acquire(&missing, "h", "s", SHORT).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");

        // The lock's own path is a link: not followed.
        let directory = TempDir::new().unwrap();
        let target = elsewhere.path().join("target");
        fs::write(&target, "x").unwrap();
        symlink(&target, directory.path().join(file_name("h", "s"))).unwrap();
        assert!(SessionLock::acquire(directory.path(), "h", "s", SHORT).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "x");
    }
}
