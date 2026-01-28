use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::*;
use std::str::FromStr;

#[test]
fn test_db_workflow() -> anyhow::Result<()> {
    // 1. Setup in-memory DB
    let store = CvcStore::open(":memory:")?;
    store.init()?;

    // 2. Create Conversation
    let conv_id = "test-conv-1";
    let conv = Conversation {
        id: conv_id.to_string(),
        title: "Test Conversation".to_string(),
        created_at: Utc::now(),
    };
    store.create_conversation(&conv)?;

    let fetched_conv = store.get_conversation(conv_id)?;
    assert!(fetched_conv.is_some());
    assert_eq!(fetched_conv.unwrap().title, "Test Conversation");

    // 3. Create Interaction
    let inter_id = InteractionId::from_str("550e8400-e29b-41d4-a716-446655440000")?;
    let interaction = Interaction {
        id: inter_id.clone(),
        conversation_id: conv_id.to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Hello World".to_string(),
        model_name: Some("gpt-4".to_string()),
        model_cot: None,
        model_response: Some("Hi there".to_string()),
        source_request_id: None,
    };
    store.create_interaction(&interaction)?;

    let fetched_inter = store.get_interaction(&inter_id)?;
    assert!(fetched_inter.is_some());
    assert_eq!(fetched_inter.unwrap().user_prompt, "Hello World");

    // 4. Test Floating Nodes
    let floating = store.get_floating_interactions()?;
    assert_eq!(floating.len(), 1);
    assert_eq!(floating[0].id, inter_id);

    // 5. Link Artifact
    let commit_sha = CommitSha::new("abc1234567890abcdef1234567890abcdef12");
    store.link_interaction(&inter_id, &commit_sha, "generated")?;

    let links = store.get_artifact_links(&inter_id)?;
    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0].git_commit_hash.as_str(),
        "abc1234567890abcdef1234567890abcdef12"
    );

    // 6. Test Floating Nodes Empty
    let floating_after = store.get_floating_interactions()?;
    assert_eq!(floating_after.len(), 0);

    Ok(())
}
