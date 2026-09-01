//! End-to-end stdio tests deliberately use disposable repositories only.
use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::{Author, Conversation, Interaction, InteractionId};
use cvc_core::privacy::{McpCapture, PreparedPolicy};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let home = dir.join("git-home");
    std::fs::create_dir_all(&home).unwrap();
    let global_config = home.join("global-config");
    std::fs::write(&global_config, "").unwrap();
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global_config)
        .env("GIT_TEMPLATE_DIR", "")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {:?}: {:?}", args, output);
}

fn interaction_count(db_path: &Path) -> usize {
    CvcStore::open(db_path)
        .unwrap()
        .get_recent_interactions(100)
        .unwrap()
        .len()
}

fn project_fixture_to_ref(
    repo: &git2::Repository,
    store: &CvcStore,
    ref_name: &str,
    destination: &str,
) {
    let projection = cvc_core::sync::push_projection_to_ref(repo, store, "", destination).unwrap();
    let cvc_core::sync::ProjectionResult::Candidate { oid, candidate, .. } = projection else {
        panic!("fixture expected projection candidate");
    };
    repo.reference(ref_name, oid, true, "fixture: remote accepted candidate")
        .unwrap();
    drop(candidate);
}

fn capture_fixture_interaction(store: &CvcStore, interaction: Interaction) {
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: interaction.conversation_id.clone(),
                title: "fixture".into(),
                created_at: interaction.timestamp,
            },
            interaction,
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
}

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: BufReader<std::process::ChildStdout>,
}

impl Mcp {
    fn start(cwd: &Path, home: &Path) -> Self {
        let global_config = home.join("global-config");
        std::fs::write(&global_config, "").unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_cvc-mcp"))
            .current_dir(cwd)
            // No host home, git configuration, hooks, or CVC configuration leaks
            // into this protocol fixture.
            .env_clear()
            .env("HOME", home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", global_config)
            .env("GIT_TEMPLATE_DIR", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // MCP stderr is diagnostic-only; do not leave a pipe that can fill
            // and deadlock a hostile/noisy child.
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
        }
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn linked_worktree_binds_common_store_policy_and_rejects_other_repositories() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let main = temp.path().join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init"]);
    git(&main, &["config", "user.name", "fixture"]);
    git(&main, &["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(main.join("README"), "fixture\n").unwrap();
    git(&main, &["add", "README"]);
    git(&main, &["commit", "-m", "initial"]);

    let linked = temp.path().join("linked");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "linked-branch",
            linked.to_str().unwrap(),
        ],
    );
    let sibling = temp.path().join("sibling");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "sibling-branch",
            sibling.to_str().unwrap(),
        ],
    );
    let nested = linked.join("nested");
    std::fs::create_dir(&nested).unwrap();
    // This must be read from the linked root, rather than the server process's
    // nested cwd or the primary worktree.
    std::fs::write(linked.join(".thoughtignore"), "literal:LINKED_SECRET").unwrap();

    let other = temp.path().join("other");
    std::fs::create_dir(&other).unwrap();
    git(&other, &["init"]);

    let mut mcp = Mcp::start(&nested, &home);
    assert!(mcp.request(1, "initialize", json!({}))["result"].is_object());
    let setup = mcp.request(2, "tools/call", json!({"name":"setup_cvc", "arguments":{}}));
    assert!(setup["result"]["content"].is_array());
    let thought = mcp.request(
        3,
        "tools/call",
        json!({
            "name":"commit_thought",
            "arguments":{"task":"linked task", "reasoning":"LINKED_SECRET must be scrubbed"}
        }),
    );
    assert!(thought["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Thought recorded"));
    let history = mcp.request(
        4,
        "tools/call",
        json!({"name":"read_history", "arguments":{}}),
    );
    assert!(history["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("linked task"));
    let context = mcp.request(
        5,
        "tools/call",
        json!({"name":"get_context", "arguments":{"file_path":"README"}}),
    );
    assert!(context["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("File: README"));
    let hook_path = main.join(".git/hooks/post-commit");
    let hooks_before = std::fs::read(&hook_path).unwrap();
    let layout = cvc_core::repository::RepositoryLayout::discover(&linked).unwrap();
    let db_path = layout.db_path();
    let interactions_before = interaction_count(&db_path);
    // A legacy cwd cannot redirect setup, sync, context, or writes to another checkout.
    for (id, name) in [
        (6, "setup_cvc"),
        (7, "sync_history"),
        (8, "commit_thought"),
        (9, "get_context"),
    ] {
        let arguments = if name == "commit_thought" {
            json!({"cwd":other, "task":"wrong", "reasoning":"wrong"})
        } else if name == "get_context" {
            json!({"cwd":other, "file_path":"README"})
        } else {
            json!({"cwd":other})
        };
        let response = mcp.request(
            id,
            "tools/call",
            json!({"name":name, "arguments":arguments}),
        );
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(response["error"]["message"], "Repository mismatch");
        assert_eq!(interaction_count(&db_path), interactions_before);
        assert_eq!(std::fs::read(&hook_path).unwrap(), hooks_before);
    }

    // Even a sibling worktree sharing common storage cannot substitute its
    // active policy/context root.
    for (id, name) in [
        (10, "setup_cvc"),
        (11, "sync_history"),
        (12, "commit_thought"),
        (13, "get_context"),
    ] {
        let arguments = if name == "commit_thought" {
            json!({"cwd":sibling, "task":"wrong", "reasoning":"wrong"})
        } else if name == "get_context" {
            json!({"cwd":sibling, "file_path":"README"})
        } else {
            json!({"cwd":sibling})
        };
        let response = mcp.request(
            id,
            "tools/call",
            json!({"name":name, "arguments":arguments}),
        );
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(response["error"]["message"], "Repository mismatch");
        assert_eq!(interaction_count(&db_path), interactions_before);
        assert_eq!(std::fs::read(&hook_path).unwrap(), hooks_before);
    }

    for (id, cwd) in [
        (14, json!(null)),
        (15, json!(7)),
        (16, json!({})),
        (17, json!("")),
    ] {
        let response = mcp.request(
            id,
            "tools/call",
            json!({"name":"get_context", "arguments":{"cwd":cwd, "file_path":"README"}}),
        );
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(response["error"]["message"], "Invalid 'cwd' parameter");
    }

    // A linked checkout has a gitfile; CVC belongs in the common Git dir only.
    assert!(std::fs::metadata(linked.join(".git")).unwrap().is_file());
    assert!(!linked.join(".git").join("cvc").exists());
    assert!(layout.db_path().exists());
    assert!(!other.join(".git/cvc/index.db").exists());
    assert!(!other.join(".git/hooks/post-commit").exists());
    assert_eq!(std::fs::read(hook_path).unwrap(), hooks_before);
    let store = CvcStore::open(layout.db_path()).unwrap();
    let interactions = store.get_recent_interactions(10).unwrap();
    assert_eq!(interactions.len(), 1);
    assert!(!interactions[0]
        .model_cot
        .as_deref()
        .unwrap()
        .contains("LINKED_SECRET"));
    // The thought is attributed to the bound linked worktree, so only that
    // worktree's automatic linker may consider it.
    let linked_origin = cvc_core::repository::worktree_origin_fingerprint(&linked).unwrap();
    let scoped = store
        .get_floating_interactions_for_worktree(&linked_origin)
        .unwrap();
    assert_eq!(scoped.len(), 1, "own-worktree scoping sees the capture");
    assert!(
        store
            .get_floating_interactions_for_worktree(&"f".repeat(64))
            .unwrap()
            .is_empty(),
        "a different worktree origin must not see the capture"
    );
}

#[test]
fn linked_worktree_sync_history_imports_remote_projection_into_common_store() {
    // Everything, including Git's global configuration and the remote, is local to
    // this fixture.  The server itself starts below the linked worktree root.
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let remote = temp.path().join("remote.git");
    std::fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "--bare", "--initial-branch=main"]);

    let publisher = temp.path().join("publisher");
    std::fs::create_dir(&publisher).unwrap();
    git(&publisher, &["init", "-b", "main"]);
    git(&publisher, &["config", "user.name", "fixture"]);
    git(
        &publisher,
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::write(publisher.join("README"), "fixture\n").unwrap();
    git(&publisher, &["add", "README"]);
    git(&publisher, &["commit", "-m", "initial"]);
    git(
        &publisher,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&publisher, &["push", "origin", "main"]);

    let publisher_layout = cvc_core::repository::RepositoryLayout::discover(&publisher).unwrap();
    let publisher_store = CvcStore::open(publisher_layout.db_path()).unwrap();
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "remote-conversation".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "linked remote thought".into(),
        model_name: None,
        model_cot: Some("remote reasoning".into()),
        model_response: None,
        source_request_id: None,
    };
    capture_fixture_interaction(&publisher_store, interaction.clone());
    let destination =
        cvc_core::privacy::remote_destination(publisher_layout.repository(), "origin")
            .unwrap()
            .fingerprint;
    publisher_store
        .share_conversation_for_remote(
            &interaction.conversation_id,
            &destination,
            cvc_core::models::FutureSharePolicy::Private,
        )
        .unwrap();
    project_fixture_to_ref(
        publisher_layout.repository(),
        &publisher_store,
        "refs/cvc/main",
        &destination,
    );
    git(
        &publisher,
        &["push", "origin", "refs/cvc/main:refs/cvc/main"],
    );

    let main = temp.path().join("main");
    git(
        temp.path(),
        &["clone", remote.to_str().unwrap(), main.to_str().unwrap()],
    );
    let linked = temp.path().join("linked");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "linked-branch",
            linked.to_str().unwrap(),
        ],
    );
    let nested = linked.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let other = temp.path().join("other");
    std::fs::create_dir(&other).unwrap();
    git(&other, &["init"]);

    let linked_layout = cvc_core::repository::RepositoryLayout::discover(&nested).unwrap();
    let main_layout = cvc_core::repository::RepositoryLayout::discover(&main).unwrap();
    assert_eq!(linked_layout.db_path(), main_layout.db_path());
    assert!(std::fs::metadata(linked.join(".git")).unwrap().is_file());

    let mut mcp = Mcp::start(&nested, &home);
    assert!(mcp.request(1, "initialize", json!({}))["result"].is_object());
    let synced = mcp.request(
        2,
        "tools/call",
        json!({"name":"sync_history", "arguments":{"cwd":linked}}),
    );
    assert!(synced["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Pulled 1 new interaction"));
    let history = mcp.request(
        3,
        "tools/call",
        json!({"name":"read_history", "arguments":{}}),
    );
    assert!(history["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("linked remote thought"));

    // The linked server and primary checkout observe one common SQLite store.
    assert!(CvcStore::open(main_layout.db_path())
        .unwrap()
        .get_interaction(&interaction.id)
        .unwrap()
        .is_some());
    assert!(!linked.join(".git").join("cvc").exists());
    assert!(!other.join(".git/cvc/index.db").exists());
}
