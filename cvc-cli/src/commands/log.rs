use anyhow::Result;
use cvc_core::db::CvcStore;
use std::env;

pub async fn run() -> Result<()> {
    let current_dir = env::current_dir()?;
    let cvc_dir = current_dir.join(".git").join("cvc");

    if !cvc_dir.exists() {
        println!("CVC is not initialized.");
        return Ok(());
    }

    let db_path = cvc_dir.join("index.db");
    let store = CvcStore::open(&db_path)?;

    // Simplistic log: Get all IDs and show them.
    let ids = store.get_all_interaction_ids()?;

    println!("CVC Log ({} interactions)", ids.len());
    println!("----------------------------------------");

    for id in ids {
        if let Some(interaction) = store.get_interaction(&id)? {
            println!("Node: {}", interaction.id.to_string());
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
