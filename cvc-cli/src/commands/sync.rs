use anyhow::{Context, Result};
use cvc_core::db::CvcStore;
use cvc_core::sync;
use git2::Repository;
use std::env;

pub async fn push() -> Result<()> {
    let current_dir = env::current_dir()?;
    let repo = Repository::open(&current_dir).context("Failed to open git repository")?;
    let db_path = current_dir.join(".git").join("cvc").join("index.db");
    let store = CvcStore::open(&db_path).context("Failed to open CVC database")?;

    println!("Pushing interactions to refs/cvc/main...");
    sync::push_to_ref(&repo, &store, "refs/cvc/main")?;
    println!("Successfully pushed interactions.");

    // Optional: push to origin
    // This requires git credentials. For now we just update local ref.
    // The user can run `git push origin refs/cvc/main` manually or we can try.
    println!("Note: You may need to run 'git push origin refs/cvc/main' to sync with remote.");

    Ok(())
}

pub async fn pull() -> Result<()> {
    let current_dir = env::current_dir()?;
    let repo = Repository::open(&current_dir).context("Failed to open git repository")?;
    let db_path = current_dir.join(".git").join("cvc").join("index.db");
    let store = CvcStore::open(&db_path).context("Failed to open CVC database")?;

    println!("Pulling interactions from refs/cvc/main...");
    println!("Note: You may need to run 'git fetch origin refs/cvc/main:refs/cvc/main' first.");

    sync::pull_from_ref(&repo, &store, "refs/cvc/main")?;
    println!("Successfully pulled interactions.");

    Ok(())
}
