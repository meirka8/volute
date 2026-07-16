use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::{Author, CommitSha, Conversation, Interaction, InteractionId};
use cvc_mcp::server::{start_session, AppState};
use cvc_mcp::tools::{call_tool, list_tools};
use git2::Repository;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tempfile::{tempdir, TempDir};

/// Builds a fresh, isolated store + `AppState`. The returned `TempDir` must be kept
/// alive for as long as `AppState` is used, since dropping it deletes the DB file.
fn make_state() -> (TempDir, Arc<AppState>) {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("cvc.db");
    let store = CvcStore::open(&db_path).unwrap();
    store.init().unwrap();
    (dir, Arc::new(AppState::new(Arc::new(Mutex::new(store)))))
}

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
    let (_dir, state) = make_state();
    start_session(&state, "test-client").unwrap();

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

    let res = call_tool(args, state.clone()).await.unwrap();
    assert!(res["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Thought recorded"));

    // Verify in DB
    let interactions = {
        let store_locked = state.store.lock().unwrap();
        store_locked.get_floating_interactions().unwrap()
    };
    assert!(!interactions.is_empty());
    assert_eq!(
        interactions[0].model_cot.as_ref().unwrap(),
        "Thinking process"
    );
    assert_eq!(interactions[0].user_prompt, "User query");
}

#[tokio::test]
async fn test_read_history() {
    let (_dir, state) = make_state();
    start_session(&state, "test-client").unwrap();

    // Insert a thought. See the comment in test_commit_thought: the schema
    // field is "task", not "prompt".
    let args = json!({
        "name": "commit_thought",
        "arguments": {
            "reasoning": "Old thought",
            "task": "Old prompt"
        }
    });
    call_tool(args, state.clone()).await.unwrap();

    let history_args = json!({
        "name": "read_history",
        "arguments": { "limit": 5 }
    });
    let res = call_tool(history_args, state).await.unwrap();
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Old prompt"));
    assert!(text.contains("Old thought"));
}

#[tokio::test]
async fn test_read_history_includes_linked_interactions() {
    // HEL-62 acceptance criterion: read_history must not go blank the moment an
    // interaction is linked to a commit -- it previously only read floating nodes.
    let (_dir, state) = make_state();
    start_session(&state, "test-client").unwrap();

    let args = json!({
        "name": "commit_thought",
        "arguments": { "reasoning": "Committed reasoning", "task": "Committed task" }
    });
    call_tool(args, state.clone()).await.unwrap();

    {
        let store = state.store.lock().unwrap();
        let interactions = store.get_floating_interactions().unwrap();
        let interaction_id = &interactions[0].id;
        store
            .link_interaction(interaction_id, &CommitSha::new("deadbeef"), "generated")
            .unwrap();
    }

    let history_args = json!({
        "name": "read_history",
        "arguments": { "limit": 5 }
    });
    let res = call_tool(history_args, state).await.unwrap();
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("Committed task"),
        "read_history should surface linked (committed) interactions, not only floating ones"
    );
}

#[tokio::test]
async fn test_parent_chaining_within_session() {
    // HEL-62 acceptance criterion: two consecutive commit_thought calls in one
    // server process produce chained parent_ids in one conversation.
    let (_dir, state) = make_state();
    let conversation_id = start_session(&state, "test-client").unwrap();

    let first_args = json!({
        "name": "commit_thought",
        "arguments": { "reasoning": "First reasoning", "task": "First task" }
    });
    call_tool(first_args, state.clone()).await.unwrap();

    let second_args = json!({
        "name": "commit_thought",
        "arguments": { "reasoning": "Second reasoning", "task": "Second task" }
    });
    call_tool(second_args, state.clone()).await.unwrap();

    let store = state.store.lock().unwrap();
    let interactions = store
        .get_recent_interactions_for_conversation(&conversation_id, 10)
        .unwrap();
    assert_eq!(interactions.len(), 2);
    assert!(interactions
        .iter()
        .all(|i| i.conversation_id == conversation_id));

    let root = interactions
        .iter()
        .find(|i| i.parent_id.is_none())
        .expect("exactly one root interaction");
    let child = interactions
        .iter()
        .find(|i| i.parent_id.is_some())
        .expect("exactly one chained interaction");

    assert_eq!(root.user_prompt, "First task");
    assert_eq!(child.user_prompt, "Second task");
    assert_eq!(child.parent_id.as_ref(), Some(&root.id));
}

#[tokio::test]
async fn test_distinct_sessions_get_distinct_conversations() {
    // HEL-62 acceptance criterion: two concurrent server processes produce two
    // distinct conversations. Simulated here as two independent AppStates (each its
    // own DB connection, its own session state) sharing the same repo DB file, the
    // way two real cvc-mcp processes on the same repo would.
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("cvc.db");

    let store_a = CvcStore::open(&db_path).unwrap();
    store_a.init().unwrap();
    let state_a = Arc::new(AppState::new(Arc::new(Mutex::new(store_a))));

    let store_b = CvcStore::open(&db_path).unwrap();
    let state_b = Arc::new(AppState::new(Arc::new(Mutex::new(store_b))));

    let conv_a = start_session(&state_a, "client-a").unwrap();
    let conv_b = start_session(&state_b, "client-b").unwrap();

    assert_ne!(conv_a, conv_b);

    let store_a = state_a.store.lock().unwrap();
    let conversation_a = store_a.get_conversation(&conv_a).unwrap().unwrap();
    assert!(conversation_a.title.starts_with("client-a session"));

    // Both conversations live in the same shared repo DB.
    let conversation_b_from_a = store_a.get_conversation(&conv_b).unwrap();
    assert!(conversation_b_from_a.is_some());
}

#[tokio::test]
async fn test_sync_history_pulls_from_remote() {
    // HEL-57 continuation-of-work scenario: a thought recorded and pushed from one
    // machine/harness should be pullable via the sync_history MCP tool alone, by a
    // second machine that never called commit_thought locally.
    let temp_dir = tempdir().unwrap();
    let remote_path = temp_dir.path().join("remote");
    Repository::init_bare(&remote_path).unwrap();

    // Machine A: records a thought and pushes it to the shared remote.
    let local_a_path = temp_dir.path().join("local_a");
    let repo_a = Repository::clone(remote_path.to_str().unwrap(), &local_a_path).unwrap();
    {
        let mut config_a = repo_a.config().unwrap();
        config_a.set_str("user.name", "User A").unwrap();
        config_a.set_str("user.email", "a@example.com").unwrap();
    }
    let store_a = CvcStore::open(local_a_path.join(".git/cvc/index.db")).unwrap();
    store_a.init().unwrap();
    store_a
        .create_conversation(&Conversation {
            id: "conv-a".to_string(),
            title: "Conv A".to_string(),
            created_at: Utc::now(),
        })
        .unwrap();
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
    store_a.create_interaction(&inter_a).unwrap();
    cvc_core::sync::push_to_ref(&repo_a, &store_a, "refs/cvc/main").unwrap();

    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|_, _, _| git2::Cred::default());
    let mut push_opts = git2::PushOptions::new();
    push_opts.remote_callbacks(callbacks);
    repo_a
        .find_remote("origin")
        .unwrap()
        .push(&["refs/cvc/main:refs/cvc/main"], Some(&mut push_opts))
        .unwrap();

    // Machine B: a fresh clone; the MCP server's state points at its own local DB.
    let local_b_path = temp_dir.path().join("local_b");
    Repository::clone(remote_path.to_str().unwrap(), &local_b_path).unwrap();
    let store_b = CvcStore::open(local_b_path.join(".git/cvc/index.db")).unwrap();
    store_b.init().unwrap();
    let state = Arc::new(AppState::new(Arc::new(Mutex::new(store_b))));

    let args = json!({
        "name": "sync_history",
        "arguments": { "cwd": local_b_path.to_str().unwrap() }
    });
    let res = call_tool(args, state.clone()).await.unwrap();
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("Pulled 1 new interaction"),
        "unexpected sync_history response: {text}"
    );

    let store = state.store.lock().unwrap();
    let fetched = store.get_interaction(&inter_a.id).unwrap();
    assert!(fetched.is_some(), "machine B should now have A's thought");
    assert_eq!(fetched.unwrap().user_prompt, "Thought from machine A");
}
