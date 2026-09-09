use cvc_mcp::server::AppState;
use cvc_mcp::tools;
use serde_json::json;
use std::process::Command;
use tempfile::tempdir;

#[tokio::test]
async fn test_setup_cvc_integration() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path();
    let home = repo_path.join("home");
    std::fs::create_dir(&home).unwrap();

    // 1. Initialize git repo
    let output = Command::new("git")
        .arg("init")
        .current_dir(repo_path)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("global-config"))
        .env("GIT_TEMPLATE_DIR", "")
        .output()
        .expect("Failed to init git repo");
    assert!(output.status.success(), "git init failed: {output:?}");

    // 2. Call setup_cvc with explicit cwd
    let args = json!({
        "cwd": repo_path.to_str().unwrap()
    });

    let layout = cvc_core::repository::RepositoryLayout::discover(repo_path).unwrap();
    let state = std::sync::Arc::new(AppState::open(&layout).unwrap());

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
