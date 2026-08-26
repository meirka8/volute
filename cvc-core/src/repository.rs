//! Authoritative, fail-closed Git repository layout discovery.
//!
//! libgit2 exposes the active git directory but not `git_common_dir` in the
//! version used by CVC.  Consequently `commondir` is parsed here, once, using
//! Git's one-line format.  Its target is required to be an existing canonical
//! directory; a bad `commondir` is never silently treated as a normal repo.
use git2::{ErrorCode, Repository};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_COMMONDIR_BYTES: u64 = 4096;

#[derive(Debug, Error)]
pub enum RepositoryLayoutError {
    #[error("current directory is not inside a Git repository")]
    NotRepository,
    #[error("unable to discover Git repository: {0}")]
    Git(#[from] git2::Error),
    #[error("repository metadata is invalid: {0}")]
    Metadata(String),
    #[error("this operation requires a non-bare worktree")]
    BareRepository,
}

/// Immutable paths for one discovered repository. All returned paths are
/// canonical existing paths, so they are suitable for identity and containment
/// comparisons without depending on the caller's current directory.
pub struct RepositoryLayout {
    repository: Repository,
    git_dir: PathBuf,
    common_git_dir: PathBuf,
    worktree_root: Option<PathBuf>,
}

impl RepositoryLayout {
    /// Discovers from a repository root or any existing path below it.
    pub fn discover(path: impl AsRef<Path>) -> Result<Self, RepositoryLayoutError> {
        let repository = match Repository::discover(path.as_ref()) {
            Ok(repository) => repository,
            Err(error) if error.code() == ErrorCode::NotFound => {
                // A broken gitfile/commondir can make libgit2 report NotFound.
                // Never mistake recognizable repository metadata for an ordinary
                // outside-repository invocation, since callers may otherwise
                // proceed without validating its storage boundary.
                if has_git_marker(path.as_ref()) {
                    return Err(RepositoryLayoutError::Metadata(format!(
                        "Git metadata is present but repository discovery failed: {error}"
                    )));
                }
                return Err(RepositoryLayoutError::NotRepository);
            }
            Err(error) => return Err(RepositoryLayoutError::Git(error)),
        };
        Self::from_repository(repository)
    }

    /// Binds an already opened repository while applying the same validation.
    pub fn from_repository(repository: Repository) -> Result<Self, RepositoryLayoutError> {
        let git_dir = canonical_directory(repository.path(), "active git directory")?;
        let common_git_dir = common_dir(&git_dir)?;
        let worktree_root = repository
            .workdir()
            .map(|root| canonical_directory(root, "worktree root"))
            .transpose()?;
        Ok(Self {
            repository,
            git_dir,
            common_git_dir,
            worktree_root,
        })
    }

    pub fn repository(&self) -> &Repository {
        &self.repository
    }
    /// Consumes the layout while retaining the repository validated during discovery.
    pub fn into_repository(self) -> Repository {
        self.repository
    }
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }
    pub fn common_git_dir(&self) -> &Path {
        &self.common_git_dir
    }
    pub fn cvc_dir(&self) -> PathBuf {
        self.common_git_dir.join("cvc")
    }
    pub fn db_path(&self) -> PathBuf {
        self.cvc_dir().join("index.db")
    }
    pub fn worktree_root(&self) -> Result<&Path, RepositoryLayoutError> {
        self.worktree_root
            .as_deref()
            .ok_or(RepositoryLayoutError::BareRepository)
    }
    /// `.thoughtignore` is deliberately per active worktree, not shared storage.
    pub fn policy_root(&self) -> Result<&Path, RepositoryLayoutError> {
        self.worktree_root()
    }
}

fn has_git_marker(path: &Path) -> bool {
    path.ancestors()
        .any(|directory| fs::symlink_metadata(directory.join(".git")).is_ok())
}

/// Fallible compatibility helper for callers that have a `git2::Repository`.
pub fn common_git_dir(repo: &Repository) -> Result<PathBuf, RepositoryLayoutError> {
    let git_dir = canonical_directory(repo.path(), "active git directory")?;
    common_dir(&git_dir)
}

fn canonical_directory(path: &Path, name: &str) -> Result<PathBuf, RepositoryLayoutError> {
    let metadata =
        fs::metadata(path).map_err(|e| RepositoryLayoutError::Metadata(format!("{name}: {e}")))?;
    if !metadata.is_dir() {
        return Err(RepositoryLayoutError::Metadata(format!(
            "{name} is not a directory"
        )));
    }
    fs::canonicalize(path).map_err(|e| RepositoryLayoutError::Metadata(format!("{name}: {e}")))
}

fn common_dir(git_dir: &Path) -> Result<PathBuf, RepositoryLayoutError> {
    let marker = git_dir.join("commondir");
    let metadata = match fs::symlink_metadata(&marker) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if is_linked_worktree_admin_dir(git_dir)? {
                return Err(RepositoryLayoutError::Metadata(
                    "linked-worktree commondir is missing".into(),
                ));
            }
            return Ok(git_dir.to_owned());
        }
        Err(e) => return Err(RepositoryLayoutError::Metadata(format!("commondir: {e}"))),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_COMMONDIR_BYTES
    {
        return Err(RepositoryLayoutError::Metadata(
            "commondir is symlinked, not a file, or oversized".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize + 1);
    open_marker(&marker)
        .and_then(|mut f| {
            f.by_ref()
                .take(MAX_COMMONDIR_BYTES + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|e| RepositoryLayoutError::Metadata(format!("commondir: {e}")))?;
    // Check again after opening: this catches replacement races detectable with
    // portable metadata APIs (complete race-free opening needs platform support).
    if bytes.len() as u64 > MAX_COMMONDIR_BYTES
        || fs::symlink_metadata(&marker)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true)
    {
        return Err(RepositoryLayoutError::Metadata(
            "commondir changed or is oversized".into(),
        ));
    }
    let value = std::str::from_utf8(&bytes)
        .map_err(|_| RepositoryLayoutError::Metadata("commondir is not UTF-8".into()))?;
    if value.contains('\0') {
        return Err(RepositoryLayoutError::Metadata(
            "commondir contains NUL".into(),
        ));
    }
    let line = value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .unwrap_or(value);
    if line.is_empty() || line.contains('\n') || line.contains('\r') {
        return Err(RepositoryLayoutError::Metadata(
            "commondir must contain exactly one non-empty line".into(),
        ));
    }
    let target = Path::new(line);
    let target = if target.is_absolute() {
        target.to_owned()
    } else {
        git_dir.join(target)
    };
    canonical_directory(&target, "commondir target")
}

/// A linked worktree's administrative directory contains `gitdir`, pointing
/// back to the worktree's `.git` *gitfile*. Validate both sides before using it
/// as evidence; ordinary repositories do not have this reciprocal pair.
fn is_linked_worktree_admin_dir(git_dir: &Path) -> Result<bool, RepositoryLayoutError> {
    let marker = git_dir.join("gitdir");
    let metadata = match fs::symlink_metadata(&marker) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(RepositoryLayoutError::Metadata(format!("gitdir: {e}"))),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_COMMONDIR_BYTES
    {
        return Ok(false);
    }
    let worktree_gitfile = match one_line_file(&marker) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    let worktree_gitfile = if worktree_gitfile.is_absolute() {
        worktree_gitfile
    } else {
        git_dir.join(worktree_gitfile)
    };
    let meta = match fs::symlink_metadata(&worktree_gitfile) {
        Ok(meta)
            if meta.is_file()
                && !meta.file_type().is_symlink()
                && meta.len() <= MAX_COMMONDIR_BYTES =>
        {
            meta
        }
        _ => return Ok(false),
    };
    let _ = meta;
    let contents = match one_line_file(&worktree_gitfile) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    let Some(back_reference) = contents.to_str().and_then(|v| v.strip_prefix("gitdir: ")) else {
        return Ok(false);
    };
    let back_reference = Path::new(back_reference);
    let back_reference = if back_reference.is_absolute() {
        back_reference.to_owned()
    } else {
        worktree_gitfile
            .parent()
            .unwrap_or(git_dir)
            .join(back_reference)
    };
    Ok(fs::canonicalize(back_reference).ok().as_deref() == Some(git_dir))
}

fn one_line_file(path: &Path) -> Result<PathBuf, RepositoryLayoutError> {
    let mut bytes = Vec::new();
    open_marker(path)
        .and_then(|mut file| {
            file.by_ref()
                .take(MAX_COMMONDIR_BYTES + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|e| RepositoryLayoutError::Metadata(format!("metadata marker: {e}")))?;
    if bytes.len() as u64 > MAX_COMMONDIR_BYTES || bytes.contains(&0) {
        return Err(RepositoryLayoutError::Metadata(
            "metadata marker is invalid".into(),
        ));
    }
    let value = std::str::from_utf8(&bytes)
        .map_err(|_| RepositoryLayoutError::Metadata("metadata marker is not UTF-8".into()))?;
    let line = value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .unwrap_or(value);
    if line.is_empty() || line.contains('\n') || line.contains('\r') {
        return Err(RepositoryLayoutError::Metadata(
            "metadata marker is not one line".into(),
        ));
    }
    Ok(PathBuf::from(line))
}

#[cfg(unix)]
fn open_marker(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_marker(path: &Path) -> std::io::Result<File> {
    File::open(path)
}
