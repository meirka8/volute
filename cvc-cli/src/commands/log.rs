use anyhow::{Context, Result};
use cvc_core::db::CvcStore;
use cvc_core::repository::RepositoryLayout;
use std::env;

pub async fn run() -> Result<()> {
    let current_dir = env::current_dir()?;
    let layout =
        RepositoryLayout::discover(&current_dir).context("Failed to discover Git repository")?;
    let _worktree_root = layout.worktree_root()?;
    let cvc_dir = layout.cvc_dir();

    if !cvc_dir.exists() {
        println!("CVC is not initialized.");
        return Ok(());
    }

    let store = CvcStore::open_initialized(layout.db_path())?;

    // Simplistic log: Get all IDs and show them.
    let ids = store.get_all_interaction_ids()?;

    println!("CVC Log ({} interactions)", ids.len());
    println!("----------------------------------------");

    for id in ids {
        if let Some(interaction) = store.get_interaction(&id)? {
            println!("Node: {}", interaction.id);
            println!("Time: {}", interaction.timestamp);
            println!("Auth: {:?}", interaction.author);
            println!(
                "Prom: {:.80}...",
                interaction.user_prompt.lines().next().unwrap_or("")
            );
            println!(
                "Resp: {:.80}...",
                interaction
                    .model_response
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("")
            );
            println!("----------------------------------------");
        }
    }

    Ok(())
}
