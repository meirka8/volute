use crate::db::CvcStore;
use crate::models::CommitSha;
use git2::Repository;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LinkerError {
    #[error("Git error: {0}")]
    Git(#[from] crate::git::GitError),
    #[error("DB error: {0}")]
    Db(#[from] crate::db::DbError),
}

pub type Result<T> = std::result::Result<T, LinkerError>;

/// Links all "floating" (unlinked) interactions to the current HEAD commit.
/// Returns the number of interactions linked.
pub fn link_current_commit_to_floating_nodes(repo: &Repository, db: &CvcStore) -> Result<usize> {
    // 1. Get HEAD commit SHA
    let head_sha = crate::git::get_head_commit_hash(repo)?;
    let commit_sha = CommitSha::new(head_sha);

    // 2. Get all floating interactions
    let floating_nodes = db.get_floating_interactions()?;
    if floating_nodes.is_empty() {
        return Ok(0);
    }

    // 3. Link each one
    let mut count = 0;
    for node in floating_nodes {
        // We use "generated" as the default link type for now.
        // In the future, we might want to infer this or allow it to be passed in.
        db.link_interaction(&node.id, &commit_sha, "generated")?;
        count += 1;
    }

    Ok(count)
}
