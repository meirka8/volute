use cvc_core::db::CvcStore;
use cvc_mcp::tools::{call_tool, list_tools};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

#[tokio::test]
async fn test_list_tools() {
    let tools = list_tools();
    let tools_array = tools["tools"].as_array().expect("tools should be an array");
    assert!(tools_array.iter().any(|t| t["name"] == "commit_thought"));
    assert!(tools_array.iter().any(|t| t["name"] == "read_history"));
    assert!(tools_array.iter().any(|t| t["name"] == "get_context"));
    assert!(tools_array.iter().any(|t| t["name"] == "setup_cvc"));
}

#[tokio::test]
async fn test_commit_thought() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("cvc.db");
    let store = CvcStore::open(&db_path).unwrap();
    store.init().unwrap();
    let store = Arc::new(Mutex::new(store));

    // The commit_thought input schema (list_tools() above) takes "task", not
    // "prompt" -- it's stored as Interaction.user_prompt. See a677759, which
    // renamed the schema from {reasoning, prompt} to {task, reasoning,
    // response, context_summary} but didn't update this test to match, so it
    // silently broke until HEL-58 wired this suite into CI.
    let args = json!({
        "name": "commit_thought",
        "arguments": {
            "reasoning": "Thinking process",
            "task": "User query"
        }
    });

    let res = call_tool(args, store.clone()).await.unwrap();
    assert!(res["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Thought recorded"));

    // Verify in DB
    let store_locked = store.lock().unwrap();
    let interactions = store_locked.get_floating_interactions().unwrap();
    assert!(!interactions.is_empty());
    assert_eq!(
        interactions[0].model_cot.as_ref().unwrap(),
        "Thinking process"
    );
    assert_eq!(interactions[0].user_prompt, "User query");
}

#[tokio::test]
async fn test_read_history() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("cvc.db");
    let store = CvcStore::open(&db_path).unwrap();
    store.init().unwrap();
    let store = Arc::new(Mutex::new(store));

    // Insert a thought. See the comment in test_commit_thought: the schema
    // field is "task", not "prompt".
    let args = json!({
        "name": "commit_thought",
        "arguments": {
            "reasoning": "Old thought",
            "task": "Old prompt"
        }
    });
    call_tool(args, store.clone()).await.unwrap();

    let history_args = json!({
        "name": "read_history",
        "arguments": { "limit": 5 }
    });
    let res = call_tool(history_args, store).await.unwrap();
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Old prompt"));
    assert!(text.contains("Old thought"));
}
