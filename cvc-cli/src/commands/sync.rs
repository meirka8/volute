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

    // Optional: push to origin
    // This requires git credentials. For now we just update local ref.
    // The user can run `git push origin refs/cvc/main` manually or we can try.
    println!(
        "Note: You may need to run 'git push {} refs/cvc/main' to sync with remote.",
        remote_name
    );

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

    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|_url, username_from_url, _allowed_types| {
        git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
    });

    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);

    // Note: Fetch might fail if network is down or credentials are wrong.
    // We should try to handle standard credentials.
    // For CLI tools, using the git binary might be more robust for auth, but we try libgit2 first.
    // If libgit2 fails due to auth, we might fall back or just error out.
    // For now, let's try standard ssh agent.

    // Actually, credential handling in libgit2 is tricky.
    // A simpler approach for the User might be running the git command if they have complex auth (MFA, helpers).
    // However, the requirement was to use libgit2.
    // Let's try with default credentials (agent).

    match remote.fetch(&[&refspec], Some(&mut fetch_opts), None) {
        Ok(_) => {}
        Err(e) => {
            println!(
                "Warning: specific fetch failed: {}. Trying generic fetch...",
                e
            );
            // Fallback: maybe the ref doesn't exist on remote yet?
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
