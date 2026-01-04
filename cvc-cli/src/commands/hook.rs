use anyhow::{Context, Result};
use cvc_core::db::CvcStore;
use cvc_core::linker;
use git2::Repository;
use std::env;

pub async fn post_commit() -> Result<()> {
    let current_dir = env::current_dir().context("Failed to get current directory")?;

    // We swallow errors in hooks to avoid breaking git flow, but verify logic is sound
    if let Err(e) = run_post_commit_logic(&current_dir) {
        eprintln!("CVC Hook Warning: Failed to link artifacts: {}", e);
    }

    Ok(()) // Explicitly return Ok to ensure we don't fail the hook script
}

fn run_post_commit_logic(current_dir: &std::path::Path) -> Result<()> {
    let cvc_dir = current_dir.join(".git").join("cvc");
    if !cvc_dir.exists() {
        // Not initialized, do nothing
        return Ok(());
    }

    let repo = Repository::open(current_dir).context("Failed to open git repository")?;
    let db_path = cvc_dir.join("index.db");
    let store = CvcStore::open(&db_path)?; // No context here to keep error clean if fails

    let count = linker::link_current_commit_to_floating_nodes(&repo, &store)?;

    if count > 0 {
        println!("CVC: Linked {} thought(s) to this commit.", count);
    }

    Ok(())
}
