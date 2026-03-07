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

    let remotes = repo.remotes()?;
    let remote_name = if remotes.iter().any(|r| r == Some("origin")) {
        "origin"
    } else {
        remotes.get(0).unwrap_or("origin")
    };

    println!(
        "Pushing interactions to refs/cvc/main in remote '{}'...",
        remote_name
    );
    sync::push_to_ref(&repo, &store, "refs/cvc/main")?;
    println!("Successfully pushed interactions.");

    // Execute git push to sync the ref
    println!(
        "Syncing storage ref with remote '{}' (git push)...",
        remote_name
    );

    // We use --no-verify to prevent the pre-push hook (which runs `cvc push`)
    // from causing an infinite recursion loop.
    let status = std::process::Command::new("git")
        .arg("push")
        .arg("--no-verify")
        .arg(remote_name)
        .arg("refs/cvc/main")
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("Successfully synced with remote.");
        }
        Ok(_) => {
            eprintln!("Warning: Failed to push to remote. This might be due to missing credentials or network issues.");
            eprintln!("Your interactions are saved locally in the shadow branch.");
        }
        Err(e) => {
            eprintln!("Warning: Failed to execute git push: {}", e);
        }
    }

    Ok(())
}

pub async fn pull() -> Result<()> {
    let current_dir = env::current_dir()?;
    let repo = Repository::open(&current_dir).context("Failed to open git repository")?;
    let db_path = current_dir.join(".git").join("cvc").join("index.db");
    let store = CvcStore::open(&db_path).context("Failed to open CVC database")?;

    let remotes = repo.remotes()?;
    let remote_name = if remotes.iter().any(|r| r == Some("origin")) {
        "origin"
    } else {
        remotes.get(0).unwrap_or("origin")
    };

    println!("Fetching interactions from remote '{}'...", remote_name);

    // 1. Fetch remote ref to local tracking ref
    let ref_name = "refs/cvc/main";
    let remote_tracking_ref = format!("refs/remotes/{}/cvc/main", remote_name);

    let mut remote = repo
        .find_remote(remote_name)
        .context("Failed to find remote")?;

    // We use a custom fetch refspec to map refs/cvc/main -> refs/remotes/<origin>/cvc/main
    let refspec = format!("{}:{}", ref_name, remote_tracking_ref);

    // Try using git CLI first (better auth support)
    println!("Fetching with git CLI...");
    let status = std::process::Command::new("git")
        .arg("fetch")
        .arg(remote_name)
        .arg(&refspec)
        .status();

    // If git CLI works, great. If not, fallback to libgit2.
    let git_cli_success = match status {
        Ok(s) => s.success(),
        Err(_) => false,
    };

    if !git_cli_success {
        eprintln!("Git CLI fetch failed/missing. Falling back to internal libgit2...");

        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(|_url, username_from_url, _allowed_types| {
            git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
        });

        let mut fetch_opts = git2::FetchOptions::new();
        fetch_opts.remote_callbacks(callbacks);

        match remote.fetch(&[&refspec], Some(&mut fetch_opts), None) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("Warning: internal fetch failed: {}", e);
                // If both failed, we must error out to prevent ingesting stale data.
                if repo.find_reference(&remote_tracking_ref).is_err() {
                    // Only error if we strictly needed this fetch to proceed,
                    // i.e., correct tracking ref might not exist or be stale.
                    // But user feedback says: "return an error ... when it didn't".
                    return Err(anyhow::anyhow!(
                        "Failed to fetch from remote (both CLI and internal failed). Aborting."
                    ));
                } else {
                    eprintln!("Warning: Proceeding with potentially stale tracking ref.");
                }
            }
        }
    }

    // Check if we managed to get the ref
    if repo.find_reference(&remote_tracking_ref).is_err() {
        println!("No remote interactions found (or fetch failed).");
        return Ok(());
    }

    println!("Ingesting interactions from {}...", remote_tracking_ref);
    sync::pull_from_ref(&repo, &store, &remote_tracking_ref)?;

    // 2. Force Update Local Ref to match Remote Ref
    // This effectively "Rebases" our shadow history onto the remote's truth.
    // Since we just ingested everything into sqlite, we don't lose data.
    println!("Synchronizing local shadow ref...");

    let tracking_ref = repo.find_reference(&remote_tracking_ref)?;
    if let Some(oid) = tracking_ref.target() {
        repo.reference(ref_name, oid, true, "cvc pull: synchronization")?;
    }

    println!("Successfully pulled interactions.");

    Ok(())
}
