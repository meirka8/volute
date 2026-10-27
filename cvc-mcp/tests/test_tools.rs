use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::{Author, CommitSha, Conversation, Interaction, InteractionId};
use cvc_core::privacy::{McpCapture, PreparedPolicy};
use cvc_mcp::server::{start_session, AppState};
use cvc_mcp::tools::{call_tool, list_tools};
use git2::Repository;
use serde_json::json;
use std::sync::Arc;
use tempfile::{tempdir, TempDir};

/// "No remote baseline": pins a tracking ref that does not exist, so the
/// projection starts from an empty tree and verifies that nothing appeared.
fn no_baseline(repo: &Repository) -> cvc_core::sync::ImportedBaseline {
    cvc_core::sync::ImportedBaseline::observe(repo, "refs/remotes/unpublished/cvc/main").unwrap()
}

trait CaptureFixture {
    fn create_conversation(&self, _: &Conversation) -> cvc_core::db::Result<()>;
    fn create_interaction(&self, interaction: &Interaction) -> cvc_core::db::Result<()>;
}
impl CaptureFixture for CvcStore {
    fn create_conversation(&self, _: &Conversation) -> cvc_core::db::Result<()> {
        Ok(())
    }
    fn create_interaction(&self, interaction: &Interaction) -> cvc_core::db::Result<()> {
        self.capture_mcp(McpCapture::new(
            Conversation {
                id: interaction.conversation_id.clone(),
                title: "fixture".into(),
                created_at: interaction.timestamp,
            },
            interaction.clone(),
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .map(|_| ())
    }
}

fn project_fixture_to_ref(repo: &Repository, store: &CvcStore, ref_name: &str, destination: &str) {
    let projection =
        cvc_core::sync::push_projection_to_ref(repo, store, &no_baseline(repo), destination)
            .unwrap();
    let cvc_core::sync::ProjectionResult::Candidate { oid, candidate, .. } = projection else {
        panic!("fixture expected projection candidate");
    };
    repo.reference(ref_name, oid, true, "fixture: remote accepted candidate")
        .unwrap();
    drop(candidate);
}

/// Builds a fresh, isolated store + `AppState`. The returned `TempDir` must be kept
/// alive for as long as `AppState` is used, since dropping it deletes the DB file.
fn make_state() -> (TempDir, Arc<AppState>) {
    let dir = tempdir().unwrap();
    Repository::init(dir.path()).unwrap();
    let layout = cvc_core::repository::RepositoryLayout::discover(dir.path()).unwrap();
    (dir, Arc::new(AppState::open(&layout).unwrap()))
}

#[test]
fn bare_repository_startup_does_not_create_cvc_storage_or_hooks() {
    let dir = tempdir().unwrap();
    Repository::init_bare(dir.path()).unwrap();
    let layout = cvc_core::repository::RepositoryLayout::discover(dir.path()).unwrap();
    let hooks_before: Vec<_> = std::fs::read_dir(dir.path().join("hooks"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(AppState::open(&layout).is_err());
    assert!(
        !layout.cvc_dir().exists(),
        "bare startup must not create cvc/"
    );
    assert!(
        !layout.db_path().exists(),
        "bare startup must not reserve index.db"
    );
    let hooks_after: Vec<_> = std::fs::read_dir(dir.path().join("hooks"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        hooks_after, hooks_before,
        "bare startup must not mutate hooks"
    );
}

fn fixture_store(root: &std::path::Path) -> CvcStore {
    let layout = cvc_core::repository::RepositoryLayout::discover(root).unwrap();
    CvcStore::open(layout.db_path()).unwrap()
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
    let (dir, state) = make_state();
    start_session(&state, "test-client").unwrap();

    // The commit_thought input schema (list_tools() above) takes "task", not
    // "prompt" -- it's stored as Interaction.user_prompt. See a677759, which
    // renamed the schema from {reasoning, prompt} to {task, reasoning,
    // response, context_summary} but didn't update this test to match, so it
    // silently broke until this suite was wired into CI.
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
    let interactions = fixture_store(dir.path())
        .get_floating_interactions()
        .unwrap();
    assert!(!interactions.is_empty());
    assert_eq!(
        interactions[0].model_cot.as_ref().unwrap(),
        "Thinking process"
    );
    assert_eq!(interactions[0].user_prompt, "User query");
}

fn commit_thought_in(conversation_id: &str) -> serde_json::Value {
    json!({
        "name": "commit_thought",
        "arguments": {
            "reasoning": "Thinking process",
            "task": "User query",
            "conversation_id": conversation_id,
        }
    })
}

/// What the client is told when the id it chose is outside the rule.
const CONVERSATION_ID_ERROR: &str =
    "Invalid 'conversation_id': use 1 to 256 ASCII letters, digits, '.', '_', '-' or ':'.";

#[tokio::test]
async fn test_a_conversation_id_that_could_alter_what_a_terminal_shows_is_refused() {
    let (dir, state) = make_state();
    start_session(&state, "test-client").unwrap();

    // The id becomes the conversation's title and is named in the phrase typed
    // to share it, and the client chooses it. Every one of these is a control
    // (Cc), format (Cf) or separator (Zl, Zp) character, in the middle of an
    // id that is otherwise fine.
    for id in [
        "has\u{1b}[2Jescape",
        "line\nbreak",
        "carriage\rreturn",
        "tab\there",
        "nul\0byte",
        "delete\u{7f}",
        "c1\u{9b}control",
        "bidi\u{202e}override",
        "isolate\u{2066}isolate",
        "zero\u{200b}width",
        "soft\u{ad}hyphen",
        "byte\u{feff}order",
        "tag\u{e0041}character",
        "line\u{2028}separator",
        "paragraph\u{2029}separator",
    ] {
        let error = call_tool(commit_thought_in(id), state.clone())
            .await
            .expect_err(&format!("{id:?} was accepted"));
        assert_eq!(error.code, -32602, "{id:?}");
        assert_eq!(error.message, CONVERSATION_ID_ERROR, "{id:?}");
    }

    // Nothing was recorded for any of them.
    assert!(fixture_store(dir.path())
        .get_floating_interactions()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn test_an_ordinary_conversation_id_is_still_recorded() {
    let (dir, state) = make_state();
    start_session(&state, "test-client").unwrap();

    // Every character the rule allows, a UUID (which is what the server itself
    // uses when the client does not choose), and the longest id there is.
    let longest = "a".repeat(256);
    let ids = [
        "client-session_42",
        "harness.session:7",
        "A1b2C3",
        "0123456789.:_-abcXYZ",
        "3f2b8c1e-5d4a-4b7e-9c0d-1a2b3c4d5e6f",
        longest.as_str(),
    ];
    for id in ids {
        let res = call_tool(commit_thought_in(id), state.clone())
            .await
            .unwrap_or_else(|error| panic!("{id:?} was refused: {}", error.message));
        assert!(res["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Thought recorded"));
    }
    let recorded: Vec<String> = fixture_store(dir.path())
        .get_floating_interactions()
        .unwrap()
        .into_iter()
        .map(|interaction| interaction.conversation_id)
        .collect();
    for id in ids {
        assert!(recorded.iter().any(|recorded| recorded == id), "{id:?}");
    }
}

#[tokio::test]
async fn test_spaces_and_other_characters_outside_the_capture_rule_are_refused() {
    let (dir, state) = make_state();
    start_session(&state, "test-client").unwrap();

    // Core's capture policy has never accepted any of these in a conversation
    // id, so the server says so up front and records nothing, instead of
    // pretending printable characters are fine and failing in the capture.
    let too_long = "a".repeat(257);
    for id in [
        "my session 1",
        " leading",
        "trailing ",
        "\u{a0}no-break space",
        "\u{3000}ideographic space",
        "\u{c9}clair \u{fc} \u{2713} \u{5e9}\u{5dc}\u{5d5}\u{5dd}",
        "emoji \u{1f680}",
        "with/slash",
        "back\\slash",
        "quo'te",
        "dq\"uote",
        "at@sign",
        "per%cent",
        "pl+us",
        "eq=ual",
        "co,mma",
        "tilde~",
        "semi;colon",
        "",
        too_long.as_str(),
    ] {
        let error = call_tool(commit_thought_in(id), state.clone())
            .await
            .expect_err(&format!("{id:?} was accepted"));
        assert_eq!(error.code, -32602, "{id:?}");
        assert_eq!(error.message, CONVERSATION_ID_ERROR, "{id:?}");
    }
    assert!(fixture_store(dir.path())
        .get_floating_interactions()
        .unwrap()
        .is_empty());
}

/// The rule the tool applies is the rule the capture applies, because it is
/// the same function; this pins that the function says what the capture does,
/// for ids on both sides of every boundary.
#[tokio::test]
async fn test_the_tool_and_the_capture_agree_on_which_ids_are_acceptable() {
    let (_dir, state) = make_state();
    start_session(&state, "test-client").unwrap();
    let capture_dir = tempdir().unwrap();
    Repository::init(capture_dir.path()).unwrap();
    let capture_store = fixture_store(capture_dir.path());

    let longest = "b".repeat(256);
    let too_long = "b".repeat(257);
    for id in [
        "plain",
        "with-dash_and.dot:and-colon",
        longest.as_str(),
        too_long.as_str(),
        "",
        "with space",
        "tab\t",
        "\u{e9}",
        "slash/",
        "new\nline",
        "\u{200b}",
    ] {
        let by_the_rule = cvc_core::privacy::is_safe_identifier(id);
        let by_the_tool = match call_tool(commit_thought_in(id), state.clone()).await {
            Ok(_) => true,
            Err(error) => {
                assert_eq!(error.message, CONVERSATION_ID_ERROR, "{id:?}");
                false
            }
        };
        let by_the_capture = capture_store
            .create_interaction(&Interaction {
                id: InteractionId::new(),
                conversation_id: id.to_owned(),
                parent_id: None,
                timestamp: Utc::now(),
                author: Author::Agent,
                user_prompt: "x".into(),
                model_name: None,
                model_cot: None,
                model_response: None,
                source_request_id: None,
            })
            .is_ok();
        assert_eq!(by_the_tool, by_the_rule, "{id:?}: tool and rule");
        assert_eq!(by_the_capture, by_the_rule, "{id:?}: capture and rule");
    }
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
    // Regression requirement: read_history must not go blank the moment an
    // interaction is linked to a commit -- it previously only read floating nodes.
    let (dir, state) = make_state();
    start_session(&state, "test-client").unwrap();

    let args = json!({
        "name": "commit_thought",
        "arguments": { "reasoning": "Committed reasoning", "task": "Committed task" }
    });
    call_tool(args, state.clone()).await.unwrap();

    {
        let store = fixture_store(dir.path());
        let interactions = store.get_floating_interactions().unwrap();
        let interaction_id = &interactions[0].id;
        store
            .link_interaction(interaction_id, &CommitSha::new("d".repeat(40)), "generated")
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
    // Session-identity requirement: two consecutive commit_thought calls in one
    // server process produce chained parent_ids in one conversation.
    let (dir, state) = make_state();
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

    let store = fixture_store(dir.path());
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
    // Session-identity requirement: two concurrent server processes produce two
    // distinct conversations. Simulated here as two independent AppStates (each its
    // own DB connection, its own session state) sharing the same repo DB file, the
    // way two real cvc-mcp processes on the same repo would.
    let dir = tempdir().unwrap();
    Repository::init(dir.path()).unwrap();
    let layout = cvc_core::repository::RepositoryLayout::discover(dir.path()).unwrap();
    let state_a = Arc::new(AppState::open(&layout).unwrap());
    let state_b = Arc::new(AppState::open(&layout).unwrap());

    let conv_a = start_session(&state_a, "client-a").unwrap();
    let conv_b = start_session(&state_b, "client-b").unwrap();

    assert_ne!(conv_a, conv_b);

    // Sessions are materialized atomically with their first captured thought;
    // start_session itself performs no row-level persistence.
}

#[tokio::test]
async fn test_sync_history_pulls_from_remote() {
    // Continuation-of-work scenario: a thought recorded and pushed from one
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
    // Capture is private by default; this fixture models the explicit user action
    // required before a thought may enter the outbound projection.
    let destination = cvc_core::privacy::remote_destination(&repo_a, "origin")
        .unwrap()
        .fingerprint;
    store_a
        .share_conversation_for_remote(
            "conv-a",
            &destination,
            cvc_core::models::FutureSharePolicy::Private,
        )
        .unwrap();
    project_fixture_to_ref(&repo_a, &store_a, "refs/cvc/main", &destination);

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
    let layout_b = cvc_core::repository::RepositoryLayout::discover(&local_b_path).unwrap();
    let state = Arc::new(AppState::open(&layout_b).unwrap());

    let args = json!({
        "name": "sync_history",
        "arguments": { "cwd": local_b_path.to_str().unwrap() }
    });
    let res = call_tool(args, state.clone()).await.unwrap();
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("Pulled 1 new thought"),
        "unexpected sync_history response: {text}"
    );

    let store = fixture_store(&local_b_path);
    let fetched = store.get_interaction(&inter_a.id).unwrap();
    assert!(fetched.is_some(), "machine B should now have A's thought");
    assert_eq!(fetched.unwrap().user_prompt, "Thought from machine A");
}

#[cfg(unix)]
#[tokio::test]
async fn test_rejects_replaced_database_at_same_path() {
    let (dir, state) = make_state();
    let layout = cvc_core::repository::RepositoryLayout::discover(dir.path()).unwrap();
    let db_path = layout.db_path();
    let replacement = db_path.with_extension("replaced");
    std::fs::rename(&db_path, &replacement).unwrap();
    let replacement_store = CvcStore::open(&db_path).unwrap();
    replacement_store.init().unwrap();

    let error = call_tool(json!({"name":"read_history", "arguments":{}}), state)
        .await
        .unwrap_err();
    assert_eq!(error.code, -32602);
    assert_eq!(error.message, "Repository mismatch");
}

#[cfg(unix)]
#[test]
fn startup_rejects_symlinked_cvc_storage() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    Repository::init(dir.path()).unwrap();
    let layout = cvc_core::repository::RepositoryLayout::discover(dir.path()).unwrap();
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    symlink(&target, layout.cvc_dir()).unwrap();
    assert!(AppState::open(&layout).is_err());
}

#[cfg(unix)]
#[test]
fn startup_rejects_symlinked_database() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    Repository::init(dir.path()).unwrap();
    let layout = cvc_core::repository::RepositoryLayout::discover(dir.path()).unwrap();
    std::fs::create_dir(layout.cvc_dir()).unwrap();
    let target = dir.path().join("target.db");
    std::fs::write(&target, "not sqlite").unwrap();
    symlink(&target, layout.db_path()).unwrap();
    assert!(AppState::open(&layout).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_symlink_replacement_before_operation() {
    use std::os::unix::fs::symlink;

    let (dir, state) = make_state();
    let layout = cvc_core::repository::RepositoryLayout::discover(dir.path()).unwrap();
    let db_path = layout.db_path();
    let target = dir.path().join("replacement.db");
    std::fs::write(&target, "replacement").unwrap();
    std::fs::remove_file(&db_path).unwrap();
    symlink(&target, &db_path).unwrap();
    let error = call_tool(json!({"name":"read_history", "arguments":{}}), state)
        .await
        .unwrap_err();
    assert_eq!(error.code, -32602);
    assert_eq!(error.message, "Repository mismatch");
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_cvc_directory_symlink_before_operation() {
    use std::os::unix::fs::symlink;

    let (dir, state) = make_state();
    let layout = cvc_core::repository::RepositoryLayout::discover(dir.path()).unwrap();
    let cvc_dir = layout.cvc_dir();
    std::fs::rename(&cvc_dir, dir.path().join("old-cvc")).unwrap();
    let target = dir.path().join("replacement-cvc");
    std::fs::create_dir(&target).unwrap();
    symlink(&target, &cvc_dir).unwrap();
    let error = call_tool(json!({"name":"read_history", "arguments":{}}), state)
        .await
        .unwrap_err();
    assert_eq!(error.code, -32602);
    assert_eq!(error.message, "Repository mismatch");
}
