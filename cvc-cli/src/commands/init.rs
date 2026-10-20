use anyhow::{Context, Result};
use cvc_core::db::CvcStore;
use cvc_core::hooks::{self, HookAction};
use cvc_core::repository::{RepositoryLayout, RepositoryLayoutError};
use std::env;
use std::fs;

pub async fn run() -> Result<()> {
    let current_dir = env::current_dir().context("Failed to get current directory")?;
    let layout = match RepositoryLayout::discover(&current_dir) {
        Ok(layout) => layout,
        Err(RepositoryLayoutError::NotRepository) => {
            anyhow::bail!("Current directory is not a git repository. Run 'git init' first.")
        }
        Err(error) => {
            return Err(anyhow::anyhow!(
                "Failed to resolve Git repository layout: {error}"
            ))
        }
    };
    // Fail before mutating common storage when discovery found a bare repository.
    layout.worktree_root()?;
    let cvc_dir = layout.cvc_dir();
    let db_path = layout.db_path();
    println!("Initializing CVC database at {:?}", db_path);
    println!("Installing CVC hooks...");
    // Hooks are advisory and inert without the database. Install and validate
    // them before creating shared state, so an unusable hooksPath cannot leave
    // a fresh repository looking initialized. Existing state is never removed
    // if this step fails or if the later database creation/open fails.
    //
    // `init` is the one command that installs every managed hook, including
    // `post-checkout`, which `harness install` later gives something to do.
    let outcomes = hooks::install_all(&layout).context("Failed to install hooks")?;

    if !cvc_dir.exists() {
        fs::create_dir_all(&cvc_dir).context("Failed to create .git/cvc directory")?;
    }
    let _store = CvcStore::open_initialized(&db_path).context("Failed to open CVC database")?;
    for outcome in outcomes {
        match outcome.action {
            HookAction::Created => println!(
                "Created new {} hook at {:?}",
                outcome.hook_name, outcome.hook_path
            ),
            HookAction::Appended => println!(
                "Appended CVC hook to existing {} at {:?}",
                outcome.hook_name, outcome.hook_path
            ),
            HookAction::Updated => println!(
                "Updated CVC hook in existing {} at {:?}",
                outcome.hook_name, outcome.hook_path
            ),
            HookAction::AlreadyPresent => println!(
                "CVC hook already present in {} at {:?}",
                outcome.hook_name, outcome.hook_path
            ),
            HookAction::SkippedNotShell => {
                eprintln!("{}", super::hook::not_shell_warning(&outcome, None))
            }
        }
    }

    println!("CVC Initialized successfully!");
    Ok(())
}
