//! The repository-wide record that the Claude Code harness is wanted.
//!
//! `cvc harness install claude-code` writes hook entries into one checkout's
//! `.claude/settings.local.json`, and a fresh linked worktree starts without
//! that file. The record is what lets Git's `post-checkout` hook install the
//! same entries into a new worktree: it says that the harness is wanted for
//! this repository and which `cvc` binary to run.
//!
//! It is a plain-text file, `<common git dir>/cvc/harness/claude-code.path`,
//! holding the absolute path of that binary and one trailing newline. Plain
//! text on purpose: the managed `post-checkout` hook is a POSIX shell script
//! that must read it without a JSON parser, and then run the binary it names,
//! so it never depends on `PATH` or `HOME`. The file lives in the common
//! directory, so every worktree shares it like the database and the consent
//! state.
//!
//! # What the checks protect
//!
//! The ownership and permission checks in [`read_record`] (and the directory
//! check in [`write_record`]) govern what CVC *writes into settings*: the Rust
//! half of the hook refuses to install anything from a record that someone else
//! could have edited. They do not govern which program the Git hook starts.
//! The hook is a shell block with no portable way to check owner or mode, and
//! it starts the binary the record names before the Rust half ever runs. The
//! shell block does refuse a symlinked record or directory. Beyond that the
//! trust boundary is write access to the common Git directory: anyone who can
//! write the record can equally write `hooks/post-checkout` itself, so a
//! writable record gives them nothing they could not already do.
//!
//! Like the other installers in this crate, this module never prints: callers
//! decide how to report what happened. Reading is cheap (one small file, no
//! database) because the hook reads it on every new worktree.
use crate::repository::RepositoryLayout;
use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Directory below the CVC directory that holds per-harness records.
const RECORD_DIR: &str = "harness";
/// Record file name inside [`RECORD_DIR`].
const RECORD_FILE: &str = "claude-code.path";
/// A path plus its newline. Longer than any real path, far shorter than junk.
const MAX_RECORD_BYTES: u64 = 8 * 1024;

/// Where the record lives for this repository (it may not exist).
pub fn record_path(layout: &RepositoryLayout) -> PathBuf {
    record_dir(layout).join(RECORD_FILE)
}

fn record_dir(layout: &RepositoryLayout) -> PathBuf {
    layout.cvc_dir().join(RECORD_DIR)
}

/// Writes the record atomically.
///
/// `binary` must be an absolute, UTF-8 path with no NUL or line break in it:
/// the same constraint the hook entries carry, plus what a one-line text file
/// can hold. The record directory is created when missing, but the CVC
/// directory must already exist, so this never makes an uninitialized
/// repository look initialized.
pub fn write_record(layout: &RepositoryLayout, binary: &Path) -> Result<()> {
    if !binary.is_absolute() {
        bail!("harness binary path must be absolute: {}", binary.display());
    }
    let text = binary
        .to_str()
        .ok_or_else(|| anyhow!("harness binary path is not valid UTF-8"))?;
    if text.contains(['\0', '\n', '\r']) {
        bail!("harness binary path must not contain NUL or line breaks");
    }
    let cvc_dir = layout.cvc_dir();
    if !cvc_dir.is_dir() {
        bail!(
            "CVC is not initialized in this repository ({} is missing)",
            cvc_dir.display()
        );
    }
    let dir = record_dir(layout);
    if inspect_dir(&dir)? {
        // A directory that was already there must be as private as one made
        // here. Better to fail the install's record step now, loudly, than to
        // write a record that every later `git worktree add` would decline.
        require_private(&dir, &fs::symlink_metadata(&dir)?)?;
    } else {
        match create_private_dir(&dir) {
            Ok(()) => {}
            // A concurrent install created it first; use it if it is sound.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !inspect_dir(&dir)? {
                    bail!("{} vanished while it was being created", dir.display());
                }
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to create {}", dir.display()))
            }
        }
    }
    let path = dir.join(RECORD_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => refuse_unless_regular(&path, &metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()))
        }
    }

    // Write-then-rename in the same directory, so a reader (the post-checkout
    // hook of a concurrently created worktree) sees the old record or the new
    // one, never a torn file. `rename` replaces a symlink at the destination
    // rather than following it.
    let temporary = dir.join(format!(".{RECORD_FILE}.{}.tmp", uuid::Uuid::new_v4()));
    let written = (|| -> std::io::Result<()> {
        let mut file = create_private_file(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("failed to write {}", path.display()));
    }
    sync_directory(&dir);
    Ok(())
}

/// Reads the recorded binary path: `Ok(None)` when there is no record, an
/// error when it is present but unusable. Unusable means symlinked, not a
/// regular file, oversized, not UTF-8, not exactly one absolute path, or, on
/// Unix, owned by someone else or writable by group or others (the same
/// ownership discipline as the consent state): a record somebody else can
/// edit would let them choose what the hook installs. Never panics.
pub fn read_record(layout: &RepositoryLayout) -> Result<Option<PathBuf>> {
    let dir = record_dir(layout);
    if !inspect_dir(&dir)? {
        return Ok(None);
    }
    let path = dir.join(RECORD_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()))
        }
    };
    refuse_unless_regular(&path, &metadata)?;
    require_private(&dir, &fs::symlink_metadata(&dir)?)?;
    let mut file =
        open_record(&path).with_context(|| format!("failed to read {}", path.display()))?;
    // Judge the file actually opened, not the one the lstat saw.
    require_private(&path, &file.metadata()?)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    parse_record(&bytes)
        .map(Some)
        .with_context(|| format!("harness record {} is unusable", path.display()))
}

/// Removes the record. Returns whether one was removed. A record that cannot
/// be parsed, or that fails the ownership checks, is still removed, so a
/// damaged record never blocks an uninstall; a symlinked record or directory
/// is refused rather than followed.
pub fn clear_record(layout: &RepositoryLayout) -> Result<bool> {
    let dir = record_dir(layout);
    if !inspect_dir(&dir)? {
        return Ok(false);
    }
    let path = dir.join(RECORD_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()))
        }
    };
    refuse_unless_regular(&path, &metadata)?;
    let removed = match fs::remove_file(&path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error).with_context(|| format!("failed to remove {}", path.display()))
        }
    };
    // Tidy the directory away when nothing else is in it; a non-empty
    // directory (another harness's record, a stray temporary) just stays.
    let _ = fs::remove_dir(&dir);
    Ok(removed)
}

fn parse_record(bytes: &[u8]) -> Result<PathBuf> {
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        bail!("too large to be a harness record");
    }
    let text = std::str::from_utf8(bytes).map_err(|_| anyhow!("not valid UTF-8"))?;
    // One trailing newline is the format; shells strip more, but accepting
    // fewer keeps Rust and the hook script in agreement about what it means.
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() {
        bail!("names no binary");
    }
    if text.contains(['\0', '\n', '\r']) {
        bail!("must hold exactly one line");
    }
    let binary = PathBuf::from(text);
    if !binary.is_absolute() {
        bail!("{text} is not an absolute path");
    }
    Ok(binary)
}

/// `Ok(true)` for an existing real directory, `Ok(false)` when missing, and an
/// error for a symlink or anything that is not a directory.
fn inspect_dir(dir: &Path) -> Result<bool> {
    match fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "refusing to use symlinked harness directory {}",
                dir.display()
            )
        }
        Ok(metadata) if !metadata.is_dir() => {
            bail!("{} is not a directory", dir.display())
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", dir.display())),
    }
}

fn refuse_unless_regular(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() {
        bail!(
            "refusing to use symlinked harness record {}",
            path.display()
        );
    }
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok(())
}

/// On Unix: owned by the effective user and not writable by group or others.
#[cfg(unix)]
fn require_private(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != unsafe { libc::geteuid() } {
        bail!(
            "{} is not owned by the current user; not using it",
            path.display()
        );
    }
    if metadata.mode() & 0o022 != 0 {
        bail!(
            "{} is writable by group or others; not using it",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_private(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir(dir)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Opens without following a symlink swapped in after the metadata check.
#[cfg(unix)]
fn open_record(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_record(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

/// Best effort: a failed directory sync only weakens durability across a
/// power loss, and the record is rewritten by the next `harness install`.
fn sync_directory(dir: &Path) {
    #[cfg(unix)]
    {
        let _ = fs::File::open(dir).and_then(|handle| handle.sync_all());
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}
