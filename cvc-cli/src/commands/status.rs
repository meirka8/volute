use anyhow::{Context, Result};
use cvc_core::db::CvcStore;
use std::env;

pub async fn run() -> Result<()> {
    let current_dir = env::current_dir().context("Failed to get current directory")?;
    let cvc_dir = current_dir.join(".git").join("cvc");

    if !cvc_dir.exists() {
        println!("CVC is not initialized in this repository. Run 'cvc init' to setup.");
        return Ok(());
    }

    let db_path = cvc_dir.join("index.db");
    let store = CvcStore::open(&db_path).context("Failed to open CVC database")?;

    let all_ids = store.get_all_interaction_ids()?;
    let floating = store.get_floating_interactions()?;

    println!("CVC Status for {}", current_dir.display());
    println!("----------------------------------------");
    println!("Total Interactions:      {}", all_ids.len());
    println!("Floating Interactions (Unlinked): {}", floating.len());

    // Future improvement: Check divergence with refs/cvc/main

    Ok(())
}
