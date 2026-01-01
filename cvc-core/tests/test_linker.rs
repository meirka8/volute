use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::linker;
use cvc_core::models::*;
use git2::{Repository, Signature};
use tempfile::TempDir;

#[test]
fn test_linker_workflow() -> anyhow::Result<()> {
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

    // 2. Create Floating Interaction
    let inter_id = InteractionId::new();
    let inter = Interaction {
        id: inter_id.clone(),
        conversation_id: "conv-1".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Thinking about code".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
    };
    store.create_conversation(&Conversation {
        id: "conv-1".to_string(),
        title: "Linker Test".to_string(),
        created_at: Utc::now(),
    })?;
    store.create_interaction(&inter)?;

    // Verify it is floating
    let floating = store.get_floating_interactions()?;
    assert_eq!(floating.len(), 1);

    // 3. Run Linker
    let linked_count = linker::link_current_commit_to_floating_nodes(&repo, &store)?;
    assert_eq!(linked_count, 1);

    // 4. Verify Link
    let links = store.get_artifact_links(&inter_id)?;
    assert_eq!(links.len(), 1);

    // Check against current HEAD
    let head = repo.head()?.peel_to_commit()?.id().to_string();
    assert_eq!(links[0].git_commit_hash.as_str(), head);
    assert_eq!(links[0].link_type, "generated");

    // 5. Verify Floating is empty
    let floating_after = store.get_floating_interactions()?;
    assert_eq!(floating_after.len(), 0);

    // 6. Run Linker again (Idempotency check / Clean state)
    let linked_count_2 = linker::link_current_commit_to_floating_nodes(&repo, &store)?;
    assert_eq!(linked_count_2, 0);

    Ok(())
}
