use anyhow::{Context, Result};
use cvc_core::db::CvcStore;
use cvc_core::repository::RepositoryLayout;
use std::env;

pub async fn run() -> Result<()> {
    let current_dir = env::current_dir().context("Failed to get current directory")?;
    let layout =
        RepositoryLayout::discover(&current_dir).context("Failed to discover Git repository")?;
    let worktree_root = layout.worktree_root()?;
    let cvc_dir = layout.cvc_dir();

    if !cvc_dir.exists() {
        println!("CVC is not initialized in this repository. Run 'cvc init' to setup.");
        return Ok(());
    }

    let store =
        CvcStore::open_initialized(layout.db_path()).context("Failed to open CVC database")?;

    let all_ids = store.get_all_interaction_ids()?;
    let floating = store.get_floating_interactions()?;

    println!("CVC Status for {}", worktree_root.display());
    println!("----------------------------------------");
    println!("Total Interactions:      {}", all_ids.len());
    println!("Floating Interactions (Unlinked): {}", floating.len());

    // Future improvement: Check divergence with refs/cvc/main

    Ok(())
}
