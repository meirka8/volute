#![cfg(unix)]

//! Regression for public issue 60: a long-lived `cvc-mcp` must keep its
//! SQLite WAL sidecars (and therefore its writes' visibility) when another
//! process opens and closes the same database. The test process plays the
//! role of a CLI run: it is a different process from the spawned server, so
//! POSIX lock semantics are exercised for real.
use cvc_core::db::CvcStore;
use cvc_core::models::InteractionId;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
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
            .env_clear()
            .env("HOME", home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", global_config)
            .env("GIT_TEMPLATE_DIR", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
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

    fn commit_thought(&mut self, id: u64, task: &str) -> Value {
        self.request(
            id,
            "tools/call",
            json!({"name":"commit_thought", "arguments":{"task":task, "reasoning":"regression fixture"}}),
        )
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Fixture {
    _temp: TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init"]);
        git(&repo, &["config", "user.name", "fixture"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        std::fs::write(repo.join("README"), "fixture\n").unwrap();
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-m", "initial"]);
        Self {
            _temp: temp,
            repo,
            home,
        }
    }

    fn db_path(&self) -> PathBuf {
        self.repo.join(".git").join("cvc").join("index.db")
    }

    fn sidecars_present(&self) -> (bool, bool) {
        let db = self.db_path();
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        let shm = PathBuf::from(format!("{}-shm", db.display()));
        (wal.exists(), shm.exists())
    }
}

fn recorded_id(response: &Value) -> InteractionId {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("unexpected response: {response}"));
    let id = text
        .strip_prefix("Thought recorded. ID: ")
        .unwrap_or_else(|| panic!("unexpected text: {text}"));
    id.trim().parse().unwrap()
}

#[test]
fn foreign_open_and_close_does_not_strand_the_server() {
    let fixture = Fixture::new();
    let mut mcp = Mcp::start(&fixture.repo, &fixture.home);
    assert!(mcp.request(1, "initialize", json!({}))["result"].is_object());
    assert!(
        mcp.request(2, "tools/call", json!({"name":"setup_cvc", "arguments":{}}))["result"]
            .is_object()
    );
    let first = recorded_id(&mcp.commit_thought(3, "first"));
    assert_eq!(
        fixture.sidecars_present(),
        (true, true),
        "a live WAL connection keeps its sidecars"
    );

    // Another process (this one) opens and closes the shared database, as
    // every CLI command and Git hook does.
    drop(CvcStore::open(fixture.db_path()).unwrap());
    assert_eq!(
        fixture.sidecars_present(),
        (true, true),
        "a foreign close must not unlink the sidecars under the live server"
    );

    let second = recorded_id(&mcp.commit_thought(4, "second"));
    // Visible from outside the server process, which is what hooks, `cvc
    // status`, and publication rely on.
    let outside = CvcStore::open(fixture.db_path()).unwrap();
    assert!(outside.get_interaction(&first).unwrap().is_some());
    assert!(outside.get_interaction(&second).unwrap().is_some());
}

#[test]
fn a_stranded_connection_reports_failure_instead_of_silent_success() {
    let fixture = Fixture::new();
    let mut mcp = Mcp::start(&fixture.repo, &fixture.home);
    assert!(mcp.request(1, "initialize", json!({}))["result"].is_object());
    assert!(
        mcp.request(2, "tools/call", json!({"name":"setup_cvc", "arguments":{}}))["result"]
            .is_object()
    );
    recorded_id(&mcp.commit_thought(3, "first"));

    // Simulate the WAL vanishing under the server (what issue 60's bug did,
    // or an operator deleting sidecars by hand). Writes now land in an
    // unlinked file; the server must say so rather than report success.
    let db = fixture.db_path();
    std::fs::remove_file(format!("{}-wal", db.display())).unwrap();
    std::fs::remove_file(format!("{}-shm", db.display())).unwrap();
    let response = mcp.commit_thought(4, "stranded");
    assert!(
        response.get("error").is_some(),
        "expected a loud failure, got {response}"
    );
}
