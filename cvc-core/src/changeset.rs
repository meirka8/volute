//! Content-only, rename-agnostic identity of a Git tree transition.
//!
//! The encoding is deliberately not a patch.  Each changed raw path carries
//! both complete tree endpoints (kind, mode and object id), making text,
//! binary, symlink, gitlink and type changes equivalent inputs to the hasher.
use git2::{Delta, DiffOptions, Oid, Repository, Tree};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const ALGORITHM: &str = "cvc.changeset/v1";
pub const MAX_DELTAS: usize = 10_000;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_CANONICAL_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangesetId {
    pub algorithm: &'static str,
    pub digest: String,
    pub deltas: usize,
    pub canonical_bytes: usize,
}

#[derive(Error, Debug)]
pub enum ChangesetError {
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("changeset bound exceeded: {0}")]
    Bound(&'static str),
    #[error("unsupported diff delta")]
    Delta,
    #[error("unsupported Git object format")]
    ObjectFormat,
    #[error("changeset deadline exceeded")]
    Deadline,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct Record {
    path: Vec<u8>,
    status: u8,
    old: Endpoint,
    new: Endpoint,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct Endpoint {
    exists: bool,
    kind: u8,
    mode: u32,
    oid: [u8; 20],
}

fn endpoint(file: git2::DiffFile<'_>) -> Result<Endpoint, ChangesetError> {
    let mode = file.mode() as u32;
    let oid = file.id();
    let exists = mode != 0 && !oid.is_zero();
    let kind = if !exists {
        0
    } else {
        match file.mode() {
            git2::FileMode::Blob | git2::FileMode::BlobExecutable => 1,
            git2::FileMode::Link => 2,
            git2::FileMode::Commit => 3,
            git2::FileMode::Tree => 4,
            _ => 5,
        }
    };
    let mut bytes = [0; 20];
    if exists {
        // v1 evidence is explicitly SHA-1-only.  Do not let a SHA-256 OID
        // reach `copy_from_slice`: that would turn an unsupported repository
        // into a hook panic rather than a fail-closed no-op.
        if oid.as_bytes().len() != bytes.len() {
            return Err(ChangesetError::ObjectFormat);
        }
        bytes.copy_from_slice(oid.as_bytes());
    }
    Ok(Endpoint {
        exists,
        kind,
        mode,
        oid: bytes,
    })
}

fn put(hasher: &mut Sha256, consumed: &mut usize, bytes: &[u8]) -> Result<(), ChangesetError> {
    *consumed = consumed
        .checked_add(bytes.len())
        .ok_or(ChangesetError::Bound("material"))?;
    if *consumed > MAX_CANONICAL_BYTES {
        return Err(ChangesetError::Bound("material"));
    }
    hasher.update(bytes);
    Ok(())
}

fn put_field(
    hasher: &mut Sha256,
    consumed: &mut usize,
    tag: u8,
    bytes: &[u8],
) -> Result<(), ChangesetError> {
    put(hasher, consumed, &[tag])?;
    put(hasher, consumed, &(bytes.len() as u64).to_be_bytes())?;
    put(hasher, consumed, bytes)
}

/// Hash `base -> result`; `None` denotes Git's empty tree (a root transition).
pub fn identify(
    repo: &Repository,
    base: Option<&Tree<'_>>,
    result: &Tree<'_>,
) -> Result<ChangesetId, ChangesetError> {
    identify_with_abort(repo, base, result, || false)
}

pub fn identify_with_abort<F: FnMut() -> bool>(
    repo: &Repository,
    base: Option<&Tree<'_>>,
    result: &Tree<'_>,
    mut abort: F,
) -> Result<ChangesetId, ChangesetError> {
    if abort() {
        return Err(ChangesetError::Deadline);
    }
    let mut options = DiffOptions::new();
    options
        .include_typechange(true)
        .include_typechange_trees(true)
        .recurse_untracked_dirs(false);
    // Deliberately do not invoke Diff::find_similar: rename/copy configuration
    // therefore cannot alter the delete+add representation.
    let diff = repo.diff_tree_to_tree(base, Some(result), Some(&mut options))?;
    if diff.deltas().len() > MAX_DELTAS {
        return Err(ChangesetError::Bound("deltas"));
    }
    let mut records = Vec::with_capacity(diff.deltas().len());
    for delta in diff.deltas() {
        if abort() {
            return Err(ChangesetError::Deadline);
        }
        let status = match delta.status() {
            Delta::Added => 1,
            Delta::Deleted => 2,
            Delta::Modified => 3,
            Delta::Typechange => 4,
            Delta::Unmodified => 0,
            // With similarity disabled these cannot be produced. Fail closed if
            // a future libgit2 changes that contract.
            _ => return Err(ChangesetError::Delta),
        };
        let old = delta.old_file();
        let new = delta.new_file();
        let path = (if status == 2 {
            old.path_bytes()
        } else {
            new.path_bytes()
        })
        .ok_or(ChangesetError::Delta)?
        .to_vec();
        if path.len() > MAX_PATH_BYTES {
            return Err(ChangesetError::Bound("path"));
        }
        records.push(Record {
            path,
            status,
            old: endpoint(old)?,
            new: endpoint(new)?,
        });
    }
    records.sort();
    let mut h = Sha256::new();
    let mut bytes = 0;
    put_field(&mut h, &mut bytes, 1, ALGORITHM.as_bytes())?;
    put_field(&mut h, &mut bytes, 2, &(records.len() as u64).to_be_bytes())?;
    for record in &records {
        put_field(&mut h, &mut bytes, 3, &record.path)?;
        put_field(&mut h, &mut bytes, 4, &[record.status])?;
        for (tag, endpoint) in [(5, &record.old), (6, &record.new)] {
            let mut value = Vec::with_capacity(27);
            value.push(endpoint.exists as u8);
            value.push(endpoint.kind);
            value.extend_from_slice(&endpoint.mode.to_be_bytes());
            value.extend_from_slice(&endpoint.oid);
            put_field(&mut h, &mut bytes, tag, &value)?;
        }
    }
    Ok(ChangesetId {
        algorithm: ALGORITHM,
        digest: hex::encode(h.finalize()),
        deltas: records.len(),
        canonical_bytes: bytes,
    })
}

pub fn commit_transition(
    repo: &Repository,
    base: Option<Oid>,
    tip: Oid,
) -> Result<ChangesetId, ChangesetError> {
    let result_commit = repo.find_commit(tip)?;
    let result = result_commit.tree()?;
    let base_tree = base
        .map(|oid| repo.find_commit(oid).and_then(|c| c.tree()))
        .transpose()?;
    identify(repo, base_tree.as_ref(), &result)
}
