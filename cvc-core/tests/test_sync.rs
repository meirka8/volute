use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::*;
use cvc_core::sync::{self, SyncNode};
use git2::{FileMode, Repository, Signature};
use rusqlite::Connection;
use tempfile::TempDir;

#[test]
fn post_node_link_records_reach_fresh_and_existing_pulls() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let source = CvcStore::open(temp_dir.path().join("source.db"))?;
    let conversation = Conversation {
        id: "late-link".into(),
        title: "late-link".into(),
        created_at: Utc::now(),
    };
    source.create_conversation(&conversation)?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: conversation.id.clone(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "floating first".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&interaction)?;
    let ref_name = "refs/cvc/late-link";
    sync::push_to_ref(&repo, &source, ref_name)?;

    let existing = CvcStore::open(temp_dir.path().join("existing.db"))?;
    sync::pull_from_ref(&repo, &existing, ref_name)?;
    assert!(existing.get_artifact_links(&interaction.id)?.is_empty());

    let sha = CommitSha::new("a".repeat(40));
    source.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &sha,
        "generated",
        Some("linker@example.com"),
    )?;
    sync::push_to_ref(&repo, &source, ref_name)?;

    let fresh = CvcStore::open(temp_dir.path().join("fresh.db"))?;
    sync::pull_from_ref(&repo, &fresh, ref_name)?;
    sync::pull_from_ref(&repo, &existing, ref_name)?;
    for store in [&fresh, &existing] {
        let links = store.get_artifact_links(&interaction.id)?;
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].link_type, "generated");
        assert_eq!(links[0].linked_by.as_deref(), Some("linker@example.com"));
    }
    Ok(())
}

#[test]
fn push_upgrades_v2_format_without_downgrading_data() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let source = CvcStore::open(temp_dir.path().join("source.db"))?;
    source.create_conversation(&Conversation {
        id: "upgrade".into(),
        title: "upgrade".into(),
        created_at: Utc::now(),
    })?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "upgrade".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "preserve me".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&interaction)?;
    let ref_name = "refs/cvc/upgrade";
    sync::push_to_ref(&repo, &source, ref_name)?;

    let previous = repo.find_reference(ref_name)?.peel_to_commit()?;
    let mut builder = repo.treebuilder(Some(&previous.tree()?))?;
    builder.insert("FORMAT", repo.blob(b"2")?, FileMode::Blob.into())?;
    let tree = repo.find_tree(builder.write()?)?;
    let signature = Signature::now("Test User", "test@example.com")?;
    repo.commit(
        Some(ref_name),
        &signature,
        &signature,
        "v2 marker",
        &tree,
        &[&previous],
    )?;

    let existing = CvcStore::open(temp_dir.path().join("existing.db"))?;
    sync::pull_from_ref(&repo, &existing, ref_name)?;
    sync::push_to_ref(&repo, &source, ref_name)?;
    let upgraded_tree = repo.find_reference(ref_name)?.peel_to_commit()?.tree()?;
    let marker = repo.find_blob(upgraded_tree.get_name("FORMAT").unwrap().id())?;
    assert_eq!(marker.content(), b"3");

    let fresh = CvcStore::open(temp_dir.path().join("fresh.db"))?;
    sync::pull_from_ref(&repo, &fresh, ref_name)?;
    sync::pull_from_ref(&repo, &existing, ref_name)?;
    assert!(fresh.get_interaction(&interaction.id)?.is_some());
    assert!(existing.get_interaction(&interaction.id)?.is_some());
    Ok(())
}

#[test]
fn v3_pull_surfaces_conflicting_link_attribution() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let source = CvcStore::open(temp_dir.path().join("source.db"))?;
    source.create_conversation(&Conversation {
        id: "conflict".into(),
        title: "conflict".into(),
        created_at: Utc::now(),
    })?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "conflict".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "conflict".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&interaction)?;
    let ref_name = "refs/cvc/conflict";
    sync::push_to_ref(&repo, &source, ref_name)?;
    let target = CvcStore::open(temp_dir.path().join("target.db"))?;
    sync::pull_from_ref(&repo, &target, ref_name)?;
    let sha = CommitSha::new("c".repeat(40));
    target.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &sha,
        "generated",
        Some("local@example.com"),
    )?;
    source.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &sha,
        "generated",
        Some("remote@example.com"),
    )?;
    sync::push_to_ref(&repo, &source, ref_name)?;
    assert!(sync::pull_from_ref(&repo, &target, ref_name).is_err());
    Ok(())
}

#[test]
fn push_rejects_existing_immutable_link_record_mismatch() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let db_path = temp_dir.path().join("source.db");
    let source = CvcStore::open(&db_path)?;
    source.create_conversation(&Conversation {
        id: "push-conflict".into(),
        title: "push-conflict".into(),
        created_at: Utc::now(),
    })?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "push-conflict".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "push".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&interaction)?;
    let sha = CommitSha::new("d".repeat(40));
    source.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &sha,
        "generated",
        Some("first@example.com"),
    )?;
    let ref_name = "refs/cvc/push-conflict";
    sync::push_to_ref(&repo, &source, ref_name)?;
    // Exact replay is the immutable-record no-op case.
    sync::push_to_ref(&repo, &source, ref_name)?;

    let raw = Connection::open(&db_path)?;
    raw.execute(
        "UPDATE artifact_links SET linked_by = 'second@example.com'",
        [],
    )?;
    drop(raw);
    assert!(sync::push_to_ref(&repo, &source, ref_name).is_err());
    Ok(())
}

#[test]
fn pull_rejects_malformed_legacy_embedded_link() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let signature = Signature::now("Test User", "test@example.com")?;
    let node_id = InteractionId::new();
    let node = SyncNode {
        interaction: Interaction {
            id: node_id.clone(),
            conversation_id: "bad".into(),
            parent_id: None,
            timestamp: Utc::now(),
            author: Author::Human,
            user_prompt: "bad".into(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        },
        context_items: vec![],
        tool_executions: vec![],
        artifact_links: vec![ArtifactLink {
            interaction_id: InteractionId::new(),
            git_commit_hash: CommitSha::new("not-an-oid"),
            link_type: "verified".into(),
            linked_by: None,
        }],
    };
    let mut builder = repo.treebuilder(None)?;
    builder.insert(
        format!("{}.json", node_id),
        repo.blob(serde_json::to_vec(&node)?.as_slice())?,
        FileMode::Blob.into(),
    )?;
    let tree = repo.find_tree(builder.write()?)?;
    repo.commit(
        Some("refs/cvc/bad"),
        &signature,
        &signature,
        "bad",
        &tree,
        &[],
    )?;
    let store = CvcStore::open(temp_dir.path().join("target.db"))?;
    assert!(sync::pull_from_ref(&repo, &store, "refs/cvc/bad").is_err());
    assert!(store.get_all_interaction_ids()?.is_empty());
    Ok(())
}

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

    assert_eq!(
        new_count, 1,
        "should report exactly one newly ingested interaction"
    );
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
fn test_sync_v2_round_trip() -> anyhow::Result<()> {
    // HEL-65 acceptance criterion: push v2 layout, pull into a fresh clone, DBs equal.
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let signature = Signature::now("Test User", "test@example.com")?;
    let tree_oid = repo.index()?.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    let commit_oid = repo.commit(Some("HEAD"), &signature, &signature, "Init", &tree, &[])?;
    let commit_sha = CommitSha::new(commit_oid.to_string());

    let store = CvcStore::open(temp_dir.path().join("cvc.db"))?;
    store.init()?;
    store.create_conversation(&Conversation {
        id: "conv-1".to_string(),
        title: "Round Trip".to_string(),
        created_at: Utc::now(),
    })?;

    // A linked interaction (will end up indexed under by-commit/) ...
    let linked = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Linked thought".to_string(),
        model_name: None,
        model_cot: Some("reasoning".to_string()),
        model_response: Some("response".to_string()),
        source_request_id: None,
    };
    store.create_interaction(&linked)?;
    store.link_interaction_with_metadata(
        &linked.id,
        &commit_sha,
        "generated",
        Some("author@example.com"),
    )?;
    store.add_context_item(&ContextItem {
        id: None,
        interaction_id: linked.id.clone(),
        file_path: "src/lib.rs".to_string(),
        git_blob_sha: Some("deadbeef".to_string()),
        dirty_patch: None,
        start_line: None,
        end_line: None,
    })?;

    // ... and a floating one (nodes/ only, no by-commit/ entry).
    let floating = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: Some(linked.id.clone()),
        timestamp: Utc::now(),
        author: Author::Agent,
        user_prompt: "Floating thought".to_string(),
        model_name: Some("agent".to_string()),
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&floating)?;

    let ref_name = "refs/cvc/main";
    sync::push_to_ref(&repo, &store, ref_name)?;

    // Verify the v2 layout actually landed: FORMAT marker, sharded nodes/, by-commit/ index.
    let pushed_tree = repo.find_reference(ref_name)?.peel_to_commit()?.tree()?;
    let format_entry = pushed_tree
        .get_name("FORMAT")
        .expect("FORMAT marker should be written");
    let format_blob = repo.find_blob(format_entry.id())?;
    assert_eq!(format_blob.content(), b"3");

    let nodes_tree = pushed_tree
        .get_name("nodes")
        .expect("nodes/ should exist")
        .to_object(&repo)?
        .peel_to_tree()?;
    let linked_prefix = &linked.id.as_str()[0..2];
    let shard_tree = nodes_tree
        .get_name(linked_prefix)
        .unwrap_or_else(|| panic!("nodes/{} shard should exist", linked_prefix))
        .to_object(&repo)?
        .peel_to_tree()?;
    assert!(shard_tree
        .get_name(&format!("{}.json", linked.id))
        .is_some());

    let by_commit_tree = pushed_tree
        .get_name("by-commit")
        .expect("by-commit/ should exist")
        .to_object(&repo)?
        .peel_to_tree()?;
    let commit_index = by_commit_tree
        .get_name(commit_sha.as_str())
        .expect("by-commit/<sha> should exist for the linked commit")
        .to_object(&repo)?
        .peel_to_tree()?;
    assert!(commit_index.get_name(&linked.id.to_string()).is_some());
    // The floating interaction was never linked, so it must not appear in the index.
    assert!(commit_index.get_name(&floating.id.to_string()).is_none());

    // Pull into a fresh clone/DB and compare.
    let store_2 = CvcStore::open(temp_dir.path().join("cvc_2.db"))?;
    store_2.init()?;
    sync::pull_from_ref(&repo, &store_2, ref_name)?;

    for original in [&linked, &floating] {
        let fetched = store_2
            .get_interaction(&original.id)?
            .unwrap_or_else(|| panic!("interaction {} missing after pull", original.id));
        assert_eq!(fetched.user_prompt, original.user_prompt);
        assert_eq!(fetched.conversation_id, original.conversation_id);
        assert_eq!(fetched.parent_id, original.parent_id);
        assert_eq!(fetched.author, original.author);
        assert_eq!(fetched.model_cot, original.model_cot);
        assert_eq!(fetched.model_response, original.model_response);
    }

    let fetched_links = store_2.get_artifact_links(&linked.id)?;
    assert_eq!(fetched_links.len(), 1);
    assert_eq!(fetched_links[0].git_commit_hash, commit_sha);
    assert_eq!(
        fetched_links[0].linked_by.as_deref(),
        Some("author@example.com")
    );

    let fetched_context = store_2.get_context_items(&linked.id)?;
    assert_eq!(fetched_context.len(), 1);
    assert_eq!(fetched_context[0].file_path, "src/lib.rs");

    assert!(store_2.get_artifact_links(&floating.id)?.is_empty());

    Ok(())
}

#[test]
fn test_pull_from_ref_reads_legacy_v1_layout() -> anyhow::Result<()> {
    // Repos synced before HEL-65 have a flat `<id>.json` tree with no nodes/,
    // by-commit/, or FORMAT entries at all. pull_from_ref must still read them.
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let signature = Signature::now("Test User", "test@example.com")?;

    let id = InteractionId::new();
    let node = SyncNode {
        interaction: Interaction {
            id: id.clone(),
            conversation_id: "conv-legacy".to_string(),
            parent_id: None,
            timestamp: Utc::now(),
            author: Author::Human,
            user_prompt: "Legacy thought".to_string(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        },
        context_items: vec![],
        tool_executions: vec![],
        artifact_links: vec![],
    };

    let mut tree_builder = repo.treebuilder(None)?;
    let json = serde_json::to_string_pretty(&node)?;
    let blob_oid = repo.blob(json.as_bytes())?;
    tree_builder.insert(format!("{}.json", id), blob_oid, FileMode::Blob.into())?;
    let tree_oid = tree_builder.write()?;
    let tree = repo.find_tree(tree_oid)?;
    repo.commit(
        Some("refs/cvc/main"),
        &signature,
        &signature,
        "Legacy v1 sync",
        &tree,
        &[],
    )?;

    let store = CvcStore::open(temp_dir.path().join("cvc.db"))?;
    store.init()?;
    sync::pull_from_ref(&repo, &store, "refs/cvc/main")?;

    let fetched = store.get_interaction(&id)?;
    assert!(
        fetched.is_some(),
        "legacy v1 interaction should be ingested"
    );
    assert_eq!(fetched.unwrap().user_prompt, "Legacy thought");

    // A push against this legacy tree should leave the old entry alone and start
    // writing new interactions under the v2 layout -- no disruptive migration.
    // (conv-legacy already exists: pull_from_ref auto-created it while ingesting the
    // legacy interaction above.)
    let new_id = InteractionId::new();
    store.create_interaction(&Interaction {
        id: new_id.clone(),
        conversation_id: "conv-legacy".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Agent,
        user_prompt: "Post-upgrade thought".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    })?;
    sync::push_to_ref(&repo, &store, "refs/cvc/main")?;

    let updated_tree = repo
        .find_reference("refs/cvc/main")?
        .peel_to_commit()?
        .tree()?;
    // The old flat entry is untouched (immutable blobs rule).
    assert!(updated_tree.get_name(&format!("{}.json", id)).is_some());
    // The new interaction lands in the sharded layout instead.
    let prefix = &new_id.as_str()[0..2];
    let nodes_tree = updated_tree
        .get_name("nodes")
        .expect("nodes/ should now exist")
        .to_object(&repo)?
        .peel_to_tree()?;
    let shard = nodes_tree
        .get_name(prefix)
        .expect("shard should exist")
        .to_object(&repo)?
        .peel_to_tree()?;
    assert!(shard.get_name(&format!("{}.json", new_id)).is_some());

    // And a pull against this mixed tree picks up both.
    let store_2 = CvcStore::open(temp_dir.path().join("cvc_2.db"))?;
    store_2.init()?;
    sync::pull_from_ref(&repo, &store_2, "refs/cvc/main")?;
    assert!(store_2.get_interaction(&id)?.is_some());
    assert!(store_2.get_interaction(&new_id)?.is_some());

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
