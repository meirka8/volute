use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tower_lsp::lsp_types::Url;

struct TestRepo {
    _temp: TempDir,
    home: PathBuf,
    root: PathBuf,
}

fn isolated_command(program: &str, home: &Path) -> Command {
    let mut command = Command::new(program);
    // Do not inherit host Git/CVC configuration, templates, or Git path overrides.
    command.env_clear();
    command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
    command.env("HOME", home);
    command.env("XDG_CONFIG_HOME", home.join("config"));
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("GIT_CONFIG_GLOBAL", home.join("empty.gitconfig"));
    command.env("GIT_TEMPLATE_DIR", home.join("templates"));
    command
}

fn git(repo: &TestRepo, dir: &Path, args: &[&str]) {
    let status = isolated_command("git", &repo.home)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn test_repo(name: &str) -> TestRepo {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join(name);
    std::fs::create_dir_all(home.join("templates")).unwrap();
    std::fs::create_dir(&root).unwrap();
    let repo = TestRepo {
        _temp: temp,
        home,
        root,
    };
    git(&repo, &repo.root, &["init"]);
    git(
        &repo,
        &repo.root,
        &["config", "user.email", "lsp@example.invalid"],
    );
    git(&repo, &repo.root, &["config", "user.name", "LSP test"]);
    std::fs::write(repo.root.join("tracked"), "one\n").unwrap();
    git(&repo, &repo.root, &["add", "tracked"]);
    git(&repo, &repo.root, &["commit", "-m", "initial"]);
    repo
}

fn lsp_command(repo: &TestRepo, root: &Path) -> Command {
    let mut command = isolated_command(env!("CARGO_BIN_EXE_cvc-lsp"), &repo.home);
    command
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Read one LSP-framed message: headers up to the blank line, then exactly
/// `Content-Length` body bytes. Returns `None` on EOF or a malformed frame.
fn read_message<R: BufRead>(reader: &mut R) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None; // EOF
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            content_length = value.trim().parse::<usize>().ok();
        }
        // Other headers (e.g. Content-Type) are allowed by the spec; ignore them.
    }

    let mut body = vec![0u8; content_length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn send(stdin: &mut ChildStdin, msg: &Value) {
    let body = msg.to_string();
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    stdin.flush().unwrap();
}

/// Tests must reap the LSP even when an assertion panics; `kill` alone leaves
/// a zombie until the test process exits.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Block until a message matching `pred` arrives, draining (and discarding)
/// any other notifications in between. Panics on timeout or a closed channel
/// rather than hanging, so a broken protocol fails the test instead of
/// stalling CI.
fn wait_for(
    rx: &mpsc::Receiver<Value>,
    timeout: Duration,
    mut pred: impl FnMut(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for expected LSP message");
        }
        match rx.recv_timeout(remaining) {
            Ok(msg) if pred(&msg) => return msg,
            Ok(_) => continue, // not the message we're waiting for; keep draining
            Err(_) => panic!("LSP message channel closed before expected message arrived"),
        }
    }
}

#[test]
fn test_lsp_turn_lifecycle() {
    let repo = test_repo("repo with ünicode space");
    let mut child = ChildGuard(
        lsp_command(&repo, &repo.root)
            .spawn()
            .expect("Failed to spawn cvc-lsp"),
    );

    let mut stdin = child.0.stdin.take().expect("Failed to open stdin");
    let stdout = child.0.stdout.take().expect("Failed to open stdout");

    // Continuously drain and parse framed messages on a background thread so
    // the child's stdout pipe never backs up. cvc-lsp emits several
    // window/logMessage notifications around this exchange (hook install,
    // "initialized", turn/start, turn/end) in addition to the initialize
    // response -- if nothing reads them, the pipe buffer can fill and block
    // the server's writes, stalling the very DB write this test verifies.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Some(msg) = read_message(&mut reader) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });

    // 1. Initialize
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {},
                "rootUri": Url::from_directory_path(&repo.root).unwrap(),
                "processId": null
            }
        }),
    );

    let init_response = wait_for(&rx, Duration::from_secs(5), |m| {
        m.get("id") == Some(&json!(1))
    });
    assert!(
        init_response["result"]["capabilities"].is_object(),
        "unexpected initialize response: {:?}",
        init_response
    );

    // Privacy status is a read-only request: it returns policy metadata only
    // and does not acknowledge capture or accept an acknowledgement parameter.
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "cvc/privacy/status",
            "params": {}
        }),
    );
    let privacy_response = wait_for(&rx, Duration::from_secs(5), |m| {
        m.get("id") == Some(&json!(2))
    });
    assert_eq!(privacy_response["result"]["privateByDefault"], true);
    assert_eq!(
        privacy_response["result"]["passiveCaptureAllowed"],
        privacy_response["result"]["captureAcknowledged"]
    );
    assert!(privacy_response["result"].get("prompt").is_none());

    // 2. Initialized
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );

    // 3. Turn Start
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "$/cvc/turn/start",
            "params": {
                "id": "turn-1",
                "prompt": "Hello CVC",
                "author": "human"
            }
        }),
    );

    // 4. Turn End
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "$/cvc/turn/end",
            "params": {
                "id": "turn-1",
                "response": "Hello Human",
                "chain_of_thought": "Thinking...",
                "model": "gpt-4"
            }
        }),
    );

    // Wait for the server's own completion signal for the async DB write
    // instead of a fixed sleep, which was both flaky (no guarantee 500ms is
    // enough) and slower than necessary on a fast machine.
    let completion = wait_for(&rx, Duration::from_secs(5), |m| {
        m.get("method") == Some(&json!("window/logMessage"))
            && m["params"]["message"].as_str().is_some_and(|s| {
                s.contains("Interaction saved to DB") || s.contains("Failed to save interaction")
            })
    });
    assert!(
        completion["params"]["message"]
            .as_str()
            .unwrap()
            .contains("Interaction saved to DB"),
        "turn/end did not save the interaction: {:?}",
        completion["params"]["message"]
    );
    let layout = cvc_core::repository::RepositoryLayout::discover(&repo.root).unwrap();
    assert!(layout.db_path().exists());
    assert_eq!(
        cvc_core::db::CvcStore::open(layout.db_path())
            .unwrap()
            .get_floating_interactions()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn linked_detached_worktree_uses_common_store_and_local_policy() {
    let repo = test_repo("main");
    let linked = repo.root.parent().unwrap().join("linked worktree ü");
    git(
        &repo,
        &repo.root,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
    let nested = linked.join("nested");
    std::fs::create_dir(&nested).unwrap();
    // This must be read from the linked worktree, not common Git storage or main.
    std::fs::write(
        linked.join(".thoughtignore"),
        "literal:LINKED_PRIVATE_VALUE\n",
    )
    .unwrap();

    let mut child = ChildGuard(lsp_command(&repo, &nested).spawn().unwrap());
    let mut stdin = child.0.stdin.take().unwrap();
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Some(msg) = read_message(&mut reader) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "capabilities":{}, "rootUri": Url::from_directory_path(&nested).unwrap(), "processId":null
        }}),
    );
    let initialized = wait_for(&rx, Duration::from_secs(5), |m| {
        m.get("id") == Some(&json!(1))
    });
    assert!(initialized.get("result").is_some(), "{initialized:?}");
    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","method":"$/cvc/turn/start","params":{
            "id":"linked-turn", "prompt":"LINKED_PRIVATE_VALUE", "author":"human"
        }}),
    );
    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","method":"$/cvc/turn/end","params":{
            "id":"linked-turn", "response":"saved", "model":"test"
        }}),
    );
    let completion = wait_for(&rx, Duration::from_secs(5), |m| {
        m.get("method") == Some(&json!("window/logMessage"))
            && m["params"]["message"]
                .as_str()
                .is_some_and(|s| s.contains("Interaction saved to DB"))
    });
    assert!(completion["params"]["message"]
        .as_str()
        .unwrap()
        .contains("saved"));

    let main_layout = cvc_core::repository::RepositoryLayout::discover(&repo.root).unwrap();
    let linked_layout = cvc_core::repository::RepositoryLayout::discover(&nested).unwrap();
    assert_eq!(main_layout.db_path(), linked_layout.db_path());
    assert!(
        !linked.join(".git").join("cvc").exists(),
        "gitfile must not receive CVC storage"
    );
    let store = cvc_core::db::CvcStore::open(main_layout.db_path()).unwrap();
    let interactions = store.get_floating_interactions().unwrap();
    assert_eq!(interactions.len(), 1);
    assert!(!interactions[0].user_prompt.contains("LINKED_PRIVATE_VALUE"));
    // The capture is attributed to the linked worktree, so the primary
    // worktree's automatic linker must not see it as eligible.
    let linked_origin = linked_layout.worktree_origin().unwrap();
    assert_eq!(
        store
            .get_floating_interactions_for_worktree(&linked_origin)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .get_floating_interactions_for_worktree(&main_layout.worktree_origin().unwrap())
        .unwrap()
        .is_empty());
}

#[test]
fn cross_repository_reinitialize_is_rejected_without_creating_a_store() {
    let first = test_repo("first");
    let second = test_repo("second");
    let mut child = ChildGuard(lsp_command(&first, &first.root).spawn().unwrap());
    let mut stdin = child.0.stdin.take().unwrap();
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Some(msg) = read_message(&mut reader) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
    for (id, root) in [(1, &first.root), (2, &second.root)] {
        send(
            &mut stdin,
            &json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{
                "capabilities":{}, "rootUri": Url::from_directory_path(root).unwrap(), "processId":null
            }}),
        );
        let response = wait_for(&rx, Duration::from_secs(5), |m| {
            m.get("id") == Some(&json!(id))
        });
        if id == 1 {
            assert!(response.get("result").is_some());
        } else {
            assert!(response.get("error").is_some());
        }
    }
    let second_layout = cvc_core::repository::RepositoryLayout::discover(&second.root).unwrap();
    assert!(!second_layout.cvc_dir().exists());
}
