use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::*;
use cvc_core::sync;
use git2::{Repository, Signature};
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
