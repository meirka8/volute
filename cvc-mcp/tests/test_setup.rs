use cvc_mcp::tools;
use serde_json::json;
use std::process::Command;
use tempfile::tempdir;

#[tokio::test]
async fn test_setup_cvc_integration() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path();

    // 1. Initialize git repo
    Command::new("git")
        .arg("init")
        .current_dir(repo_path)
        .output()
        .expect("Failed to init git repo");

    // 2. Call setup_cvc with explicit cwd
    let args = json!({
        "cwd": repo_path.to_str().unwrap()
    });

    // Create a dummy store for call_tool compatibility
    let dummy_db = repo_path.join("dummy_mcp.db");
    // We create the dummy DB file so CvcStore::open doesn't fail
    // CvcStore::open uses SQLite open which creates file if missing.

    let store = std::sync::Arc::new(std::sync::Mutex::new(
        cvc_core::db::CvcStore::open(&dummy_db).unwrap(),
    ));
    let state = std::sync::Arc::new(cvc_mcp::server::AppState::new(store));

    let res = tools::call_tool(
        json!({
            "name": "setup_cvc",
            "arguments": args
        }),
        state,
    )
    .await;

    assert!(res.is_ok(), "setup_cvc failed: {:?}", res.err());

    // 3. Verify side effects

    // Check .git/cvc/index.db exists
    let db_path = repo_path.join(".git").join("cvc").join("index.db");
    assert!(db_path.exists(), "CVC DB not created at {:?}", db_path);

    // Check hooks installed
    let hook_path = repo_path.join(".git").join("hooks").join("post-commit");
    assert!(hook_path.exists(), "post-commit hook not created");

    let content = std::fs::read_to_string(hook_path).unwrap();
    assert!(
        content.contains("cvc hook post-commit"),
        "Hook content missing command"
    );
}
