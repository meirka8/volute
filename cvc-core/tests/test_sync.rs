use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::*;
use cvc_core::sync::{self, SyncNode};
use git2::{FileMode, Repository, Signature};
use tempfile::TempDir;

#[test]
fn test_sync_push_pull() -> anyhow::Result<()> {
    // 1. Setup Repo and DB
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;

    // Create initial commit so we have a HEAD (optional but good practice)
    let signature = Signature::now("Test User", "test@example.com")?;
    let tree_oid = repo.index()?.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    repo.commit(Some("HEAD"), &signature, &signature, "Init", &tree, &[])?;

    let db_path = temp_dir.path().join("cvc.db");
    let store = CvcStore::open(&db_path)?;
    store.init()?;

    // 2. Insert Data
    let conv = Conversation {
        id: "conv-1".to_string(),
        title: "Sync Test".to_string(),
        created_at: Utc::now(),
    };
    store.create_conversation(&conv)?;

    let inter = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "To be synced".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&inter)?;

    // 3. Push to Ref
    let ref_name = "refs/cvc/test";
    sync::push_to_ref(&repo, &store, ref_name)?;

    // Verify ref exists
    let _ref_obj = repo.find_reference(ref_name)?;

    // 4. Simulate Fresh DB (Pull)
    let db_path_2 = temp_dir.path().join("cvc_2.db");
    let store_2 = CvcStore::open(&db_path_2)?;
    store_2.init()?;

    sync::pull_from_ref(&repo, &store_2, ref_name)?;

    // 5. Verify Data in store_2
    let fetched_inter = store_2.get_interaction(&inter.id)?;
    assert!(fetched_inter.is_some());
    assert_eq!(fetched_inter.unwrap().user_prompt, "To be synced");

    Ok(())
}

#[test]
fn test_sync_creates_commits() -> anyhow::Result<()> {
    // 1. Setup Repo and DB
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;

    // Create initial commit
    let signature = Signature::now("Test User", "test@example.com")?;
    let tree_oid = repo.index()?.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    repo.commit(Some("HEAD"), &signature, &signature, "Init", &tree, &[])?;

    let db_path = temp_dir.path().join("cvc.db");
    let store = CvcStore::open(&db_path)?;
    store.init()?;

    // 2. Insert Data
    let conv = Conversation {
        id: "conv-1".to_string(),
        title: "Sync Test".to_string(),
        created_at: Utc::now(),
    };
    store.create_conversation(&conv)?;

    let inter = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "To be synced".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&inter)?;

    // 3. Push to Ref
    let ref_name = "refs/cvc/test";
    sync::push_to_ref(&repo, &store, ref_name)?;

    // 4. Verify ref points to a Commit
    let reference = repo.find_reference(ref_name)?;
    let commit = reference.peel_to_commit();
    assert!(commit.is_ok(), "Ref should point to a commit");

    // 5. Push again (should add another commit or fast-forward if no changes)
    // Add another interaction to force a new commit
    let inter2 = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: Some(inter.id.clone()),
        timestamp: Utc::now(),
        author: Author::Agent,
        user_prompt: "Response".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&inter2)?;
    sync::push_to_ref(&repo, &store, ref_name)?;

    let reference = repo.find_reference(ref_name)?;
    let head_commit = reference.peel_to_commit()?;
    let parents: Vec<_> = head_commit.parents().collect();
    assert_eq!(parents.len(), 1, "Should have one parent");

    Ok(())
}

#[test]
fn test_sync_idempotency() -> anyhow::Result<()> {
    // 1. Setup Repo and DB
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;

    // Create initial commit
    let signature = Signature::now("Test User", "test@example.com")?;
    let tree_oid = repo.index()?.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    repo.commit(Some("HEAD"), &signature, &signature, "Init", &tree, &[])?;

    let db_path = temp_dir.path().join("cvc.db");
    let store = CvcStore::open(&db_path)?;
    store.init()?;

    // 2. Insert Data
    let conv = Conversation {
        id: "conv-1".to_string(),
        title: "Sync Test".to_string(),
        created_at: Utc::now(),
    };
    store.create_conversation(&conv)?;
    let inter = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "To be synced".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&inter)?;

    let ref_name = "refs/cvc/test";

    // 3. First Push
    sync::push_to_ref(&repo, &store, ref_name)?;
    let initial_commit_oid = repo.find_reference(ref_name)?.peel_to_commit()?.id();

    // 4. Second Push (No new data)
    sync::push_to_ref(&repo, &store, ref_name)?;
    let second_commit_oid = repo.find_reference(ref_name)?.peel_to_commit()?.id();

    // 5. Verify OID hasn't changed
    assert_eq!(
        initial_commit_oid, second_commit_oid,
        "Should not create new commit if no changes"
    );

    // 6. Add new data and Push
    let inter2 = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: Some(inter.id.clone()),
        timestamp: Utc::now(),
        author: Author::Agent,
        user_prompt: "Response".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&inter2)?;
    sync::push_to_ref(&repo, &store, ref_name)?;
    let third_commit_oid = repo.find_reference(ref_name)?.peel_to_commit()?.id();

    assert_ne!(
        second_commit_oid, third_commit_oid,
        "Should create new commit when data changes"
    );

    Ok(())
}

#[test]
fn test_sync_divergence_recovery() -> anyhow::Result<()> {
    // 1. Setup Remote Repo (Bare)
    let temp_dir = TempDir::new()?;
    let remote_path = temp_dir.path().join("remote");
    let _remote_repo = Repository::init_bare(&remote_path)?;

    // 2. Setup Local Repo A (User A)
    let local_a_path = temp_dir.path().join("local_a");
    let repo_a = Repository::clone(remote_path.to_str().expect("Path not UTF-8"), &local_a_path)?;

    // Config signature
    let mut config_a = repo_a.config()?;
    config_a.set_str("user.name", "User A")?;
    config_a.set_str("user.email", "a@example.com")?;

    let db_path_a = local_a_path.join(".git/cvc/index.db");
    let store_a = CvcStore::open(&db_path_a)?;
    store_a.init()?;

    // Create conversation for A
    let conv_a = Conversation {
        id: "conv-1".to_string(),
        title: "Conv A".to_string(),
        created_at: Utc::now(),
    };
    store_a.create_conversation(&conv_a)?;

    // 3. Setup Local Repo B (User B)
    let local_b_path = temp_dir.path().join("local_b");
    let repo_b = Repository::clone(remote_path.to_str().expect("Path not UTF-8"), &local_b_path)?;

    // Config signature
    let mut config_b = repo_b.config()?;
    config_b.set_str("user.name", "User B")?;
    config_b.set_str("user.email", "b@example.com")?;

    let db_path_b = local_b_path.join(".git/cvc/index.db");
    let store_b = CvcStore::open(&db_path_b)?;
    store_b.init()?;

    // Create conversation for B
    let conv_b = Conversation {
        id: "conv-2".to_string(),
        title: "Conv B".to_string(),
        created_at: Utc::now(),
    };
    store_b.create_conversation(&conv_b)?;

    // 4. User A creates thought and pushes
    let inter_a = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Thought A".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store_a.create_interaction(&inter_a)?;

    let ref_name = "refs/cvc/main";
    sync::push_to_ref(&repo_a, &store_a, ref_name)?;

    // Push ref to remote
    let mut remote_callbacks = git2::RemoteCallbacks::new();
    remote_callbacks.credentials(|_, _, _| git2::Cred::default());
    let mut push_opts = git2::PushOptions::new();
    push_opts.remote_callbacks(remote_callbacks);

    let mut origin_a = repo_a.find_remote("origin")?;
    origin_a.push(
        &[format!("{}:{}", ref_name, ref_name)],
        Some(&mut push_opts),
    )?;

    // 5. User B creates thought (Divergence!)
    let inter_b = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-2".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Thought B".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store_b.create_interaction(&inter_b)?;

    // B pushes locally
    sync::push_to_ref(&repo_b, &store_b, ref_name)?;

    // 6. User B tries to push and fails (Simulation)
    // We expect this to fail because remote has A's work, which is not in B's history.
    let mut origin_b = repo_b.find_remote("origin")?;

    let mut callbacks_b = git2::RemoteCallbacks::new();
    callbacks_b.credentials(|_, _, _| git2::Cred::default());
    let mut push_opts_b = git2::PushOptions::new();
    push_opts_b.remote_callbacks(callbacks_b);

    let result = origin_b.push(
        &[format!("{}:{}", ref_name, ref_name)],
        Some(&mut push_opts_b),
    );
    assert!(result.is_err(), "Push should fail due to non-fast-forward");

    // 7. Recovery Logic (Simulating what cvc pull will do)
    let mut origin_b = repo_b.find_remote("origin")?;
    let remote_tracking_ref = "refs/remotes/origin/cvc/main";

    // Fetch specifically the cvc ref
    origin_b.fetch(
        &[format!("{}:{}", ref_name, remote_tracking_ref)],
        None,
        None,
    )?;

    // Pull/Ingest from remote ref
    sync::pull_from_ref(&repo_b, &store_b, remote_tracking_ref)?;

    // Verify we have A's thought
    assert!(store_b.get_interaction(&inter_a.id)?.is_some());

    // Reset local ref to remote ref (The Fix)
    let remote_ref = repo_b.find_reference(remote_tracking_ref)?;
    let remote_oid = remote_ref.target().unwrap();
    repo_b.reference(ref_name, remote_oid, true, "Reset to remote")?;

    // Push local again (Should now be a merge/union on top of A)
    sync::push_to_ref(&repo_b, &store_b, ref_name)?;

    // Push to remote (Should succeed)
    origin_b.push(
        &[format!("{}:{}", ref_name, ref_name)],
        Some(&mut push_opts),
    )?;

    // 8. Verify Remote has both via A
    origin_a.fetch(
        &[format!("{}:{}", ref_name, remote_tracking_ref)],
        None,
        None,
    )?;
    sync::pull_from_ref(&repo_a, &store_a, remote_tracking_ref)?;

    assert!(store_a.get_interaction(&inter_b.id)?.is_some());

    Ok(())
}

#[test]
fn test_fetch_and_pull_from_fresh_clone() -> anyhow::Result<()> {
    // Simulates HEL-57's continuation-of-work scenario: work started on one machine
    // (repo_a) is pushed to a shared remote, then a second machine (repo_b, a fresh
    // clone with an empty CVC cache) catches up via `fetch_and_pull` alone -- the
    // same primitive the cvc-mcp `sync_history` tool calls.
    let temp_dir = TempDir::new()?;
    let remote_path = temp_dir.path().join("remote");
    let _remote_repo = Repository::init_bare(&remote_path)?;

    // Machine A: records a thought and pushes it to the shared remote.
    let local_a_path = temp_dir.path().join("local_a");
    let repo_a = Repository::clone(remote_path.to_str().expect("Path not UTF-8"), &local_a_path)?;
    let mut config_a = repo_a.config()?;
    config_a.set_str("user.name", "User A")?;
    config_a.set_str("user.email", "a@example.com")?;

    let store_a = CvcStore::open(local_a_path.join(".git/cvc/index.db"))?;
    store_a.init()?;
    store_a.create_conversation(&Conversation {
        id: "conv-a".to_string(),
        title: "Conv A".to_string(),
        created_at: Utc::now(),
    })?;
    let inter_a = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-a".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Thought from machine A".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store_a.create_interaction(&inter_a)?;
    sync::push_to_ref(&repo_a, &store_a, "refs/cvc/main")?;

    let mut push_callbacks = git2::RemoteCallbacks::new();
    push_callbacks.credentials(|_, _, _| git2::Cred::default());
    let mut push_opts = git2::PushOptions::new();
    push_opts.remote_callbacks(push_callbacks);
    repo_a
        .find_remote("origin")?
        .push(&["refs/cvc/main:refs/cvc/main"], Some(&mut push_opts))?;

    // Machine B: a fresh clone that never ran `commit_thought` locally.
    let local_b_path = temp_dir.path().join("local_b");
    let repo_b = Repository::clone(remote_path.to_str().expect("Path not UTF-8"), &local_b_path)?;
    let store_b = CvcStore::open(local_b_path.join(".git/cvc/index.db"))?;
    store_b.init()?;

    assert!(
        store_b.get_interaction(&inter_a.id)?.is_none(),
        "sanity check: machine B shouldn't have A's thought yet"
    );

    // `git fetch` needs a resolvable remote URL; the bare repo is a local path here,
    // which the system `git` CLI (fetch_and_pull's first attempt) handles fine.
    let new_count = sync::fetch_and_pull(&repo_b, &store_b, "origin")?;

    assert_eq!(new_count, 1, "should report exactly one newly ingested interaction");
    let fetched = store_b.get_interaction(&inter_a.id)?;
    assert!(fetched.is_some(), "machine B should now have A's thought");
    assert_eq!(fetched.unwrap().user_prompt, "Thought from machine A");

    // The local shadow ref should now be fast-forwarded to what was fetched.
    let local_ref = repo_b.find_reference("refs/cvc/main")?;
    let remote_ref = repo_b.find_reference("refs/remotes/origin/cvc/main")?;
    assert_eq!(local_ref.target(), remote_ref.target());

    // Calling again with nothing new on the remote should be a no-op.
    let second_count = sync::fetch_and_pull(&repo_b, &store_b, "origin")?;
    assert_eq!(second_count, 0, "should report zero on a repeat sync");

    Ok(())
}

#[test]
fn test_sync_cycle_detection() -> anyhow::Result<()> {
    // 1. Setup Repo and DB
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;

    // Create initial commit
    let signature = Signature::now("Test User", "test@example.com")?;
    let tree_oid = repo.index()?.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    repo.commit(Some("HEAD"), &signature, &signature, "Init", &tree, &[])?;

    let db_path = temp_dir.path().join("cvc.db");
    let store = CvcStore::open(&db_path)?;
    store.init()?;

    // 2. Insert Data manually to force a cycle (A -> B -> A)
    let id_a = InteractionId::new();
    let id_b = InteractionId::new();

    // We also need conversations to avoid FK error on conversation_id if we didn't create them.
    // The sync logic creates conversation placeholders if missing, so that's fine.

    let node_a = SyncNode {
        interaction: Interaction {
            id: id_a.clone(),
            conversation_id: "conv-cycle".to_string(),
            parent_id: Some(id_b.clone()), // A depends on B
            timestamp: Utc::now(),
            author: Author::Human,
            user_prompt: "A".to_string(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        },
        context_items: vec![],
        tool_executions: vec![],
        artifact_links: vec![],
    };

    let node_b = SyncNode {
        interaction: Interaction {
            id: id_b.clone(),
            conversation_id: "conv-cycle".to_string(),
            parent_id: Some(id_a.clone()), // B depends on A
            timestamp: Utc::now(),
            author: Author::Human,
            user_prompt: "B".to_string(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        },
        context_items: vec![],
        tool_executions: vec![],
        artifact_links: vec![],
    };

    // Write these to the git ref manually
    let mut tree_builder = repo.treebuilder(None)?;

    let json_a = serde_json::to_string_pretty(&node_a)?;
    let oid_a = repo.blob(json_a.as_bytes())?;
    tree_builder.insert(
        format!("{}.json", id_a).as_str(),
        oid_a,
        FileMode::Blob.into(),
    )?;

    let json_b = serde_json::to_string_pretty(&node_b)?;
    let oid_b = repo.blob(json_b.as_bytes())?;
    tree_builder.insert(
        format!("{}.json", id_b).as_str(),
        oid_b,
        FileMode::Blob.into(),
    )?;

    let tree_oid = tree_builder.write()?;
    let new_tree = repo.find_tree(tree_oid)?;

    repo.commit(
        Some("refs/cvc/cycle"),
        &signature,
        &signature,
        "Cyclic Sync",
        &new_tree,
        &[],
    )?;

    // 3. Try to Pull
    // This should FAIL with Cycle detected error.
    let result = sync::pull_from_ref(&repo, &store, "refs/cvc/cycle");
    assert!(result.is_err(), "Pull should fail on cycle");

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Cycle detected"),
        "Error should be about cycle, got: {}",
        err
    );

    // 4. Verify NOTHING was ingested (Atomic failure would be nice, but here we fail before insert loop starts sort of?)
    // Actually, `pull_from_ref` sorts ALL nodes before inserting ANY.
    // So if sort fails, NOTHING is inserted.

    let fetched_a = store.get_interaction(&id_a)?;
    assert!(
        fetched_a.is_none(),
        "Node A should NOT be ingested if cycle detected"
    );

    Ok(())
}
