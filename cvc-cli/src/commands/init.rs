use anyhow::{bail, Context, Result};
use cvc_core::db::CvcStore;
use cvc_core::hooks;
use std::env;
use std::fs;

pub async fn run() -> Result<()> {
    let current_dir = env::current_dir().context("Failed to get current directory")?;
    let git_dir = current_dir.join(".git");

    if !git_dir.exists() {
        bail!("Current directory is not a git repository. Run 'git init' first.");
    }

    let cvc_dir = git_dir.join("cvc");
    if !cvc_dir.exists() {
        fs::create_dir_all(&cvc_dir).context("Failed to create .git/cvc directory")?;
    }

    let db_path = cvc_dir.join("index.db");

    println!("Initializing CVC database at {:?}", db_path);
    let store = CvcStore::open(&db_path).context("Failed to open CVC database")?;
    store
        .init()
        .context("Failed to initialize database schema")?;

    println!("Installing CVC hooks...");
    hooks::install(&current_dir).context("Failed to install hooks")?;

    println!("CVC Initialized successfully!");
    Ok(())
}
