use crate::models::{ContextItem, InteractionId};
use git2::{Diff, DiffOptions, Repository, Status, StatusOptions};
use sha2::{Digest, Sha256};
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum GitError {
    #[error("Git error: {0}")]
    Git2(#[from] git2::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Path error: {0}")]
    Path(String),
}

pub type Result<T> = std::result::Result<T, GitError>;

pub fn open_repo<P: AsRef<Path>>(path: P) -> Result<Repository> {
    Ok(Repository::open(path)?)
}

pub fn snapshot_context(
    repo: &Repository,
    interaction_id: &InteractionId,
    files: &[String],
) -> Result<Vec<ContextItem>> {
    let mut context_items = Vec::new();
    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::Path("No workdir".into()))?;

    for file_path in files {
        if file_path.trim().is_empty() {
            continue;
        }

        let path = Path::new(file_path);
        // Ensure path is relative to repo root
        // If absolute, make relative
        let rel_path = if path.is_absolute() {
            path.strip_prefix(workdir)
                .or_else(|_| path.strip_prefix("/"))
                .unwrap_or(path)
        } else {
            path
        };
        let rel_path_str = rel_path.to_string_lossy().to_string();

        // Check status
        let mut status_opts = StatusOptions::new();
        status_opts.pathspec(&rel_path_str);
        status_opts.include_untracked(true);

        let statuses = repo.statuses(Some(&mut status_opts))?;
        let status = statuses
            .get(0)
            .map(|s| s.status())
            .unwrap_or(Status::CURRENT);

        let mut item = ContextItem {
            id: None,
            interaction_id: interaction_id.clone(),
            file_path: rel_path_str.clone(),
            git_blob_sha: None,
            dirty_patch: None,
            start_line: None,
            end_line: None,
        };

        if status.is_wt_new() {
            // Untracked file: Diff against /dev/null
            // For simplicity, we might just read the file content if it's small,
            // or try to generate a diff.
            // git2 diffing untracked files:
            // We can use diff_index_to_workdir with include_untracked.
            let mut diff_opts = DiffOptions::new();
            diff_opts.pathspec(&rel_path_str);
            diff_opts.include_untracked(true);

            let diff = repo.diff_index_to_workdir(None, Some(&mut diff_opts))?;
            let patch_str = diff_to_string(&diff)?;
            item.dirty_patch = Some(patch_str);
        } else if status.contains(Status::WT_MODIFIED) || status.contains(Status::INDEX_MODIFIED) {
            // Modified: Get HEAD blob + Diff
            let head = repo.head()?;
            let tree = head.peel_to_tree()?;

            // Try to find blob in HEAD
            if let Ok(entry) = tree.get_path(rel_path) {
                if let Ok(obj) = entry.to_object(repo) {
                    if let Some(blob) = obj.as_blob() {
                        item.git_blob_sha = Some(blob.id().to_string());
                    }
                }
            }

            // Diff
            let mut diff_opts = DiffOptions::new();
            diff_opts.pathspec(&rel_path_str);
            let diff = repo.diff_index_to_workdir(None, Some(&mut diff_opts))?;
            let patch_str = diff_to_string(&diff)?;
            item.dirty_patch = Some(patch_str);
        } else {
            // Clean: Just Blob SHA
            let head = repo.head()?;
            let tree = head.peel_to_tree()?;
            if let Ok(entry) = tree.get_path(rel_path) {
                if let Ok(obj) = entry.to_object(repo) {
                    if let Some(blob) = obj.as_blob() {
                        item.git_blob_sha = Some(blob.id().to_string());
                    }
                }
            }
        }

        context_items.push(item);
    }

    Ok(context_items)
}

fn diff_to_string(diff: &Diff) -> Result<String> {
    let mut patch_content = String::new();
    diff.print(git2::DiffFormat::Patch, |_, _, line| {
        let content = std::str::from_utf8(line.content()).unwrap_or("");
        let origin = line.origin();
        match origin {
            '+' | '-' | ' ' => {
                patch_content.push(origin);
                patch_content.push_str(content);
            }
            _ => {
                patch_content.push_str(content);
            }
        }
        true
    })?;
    Ok(patch_content)
}

pub fn calculate_context_hash(prompt: &str, context_items: &[ContextItem]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prompt);
    for item in context_items {
        hasher.update(&item.file_path);
        if let Some(sha) = &item.git_blob_sha {
            hasher.update(sha);
        }
        if let Some(patch) = &item.dirty_patch {
            hasher.update(patch);
        }
    }
    hex::encode(hasher.finalize())
}
