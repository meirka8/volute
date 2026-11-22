#![cfg(unix)]

//! Batched hook ingest (#149, #187 PR 4) end to end, through the `cvc`
//! binary, with the hook's real time budget (20 s from the process's start)
//! and its real batch size (4 MiB).
//!
//! * A hook run killed with `SIGKILL` part way through a large session, after
//!   it committed a batch, loses nothing and records nothing twice: the next
//!   runs go on from the cursors and end in exactly the state one manual
//!   `--transcript` run reaches (rows, typed and 0.7.x cursors, evidence,
//!   links and published node bytes).
//! * A `SessionEnd` hook that runs out of time leaves the backlog flag and
//!   prints nothing; `cvc harness status claude-code` lists the session with
//!   the catch-up command, and running it clears the flag (R7).
//!
//! The budget cannot be shrunk from outside the binary (no variable or flag
//! changes it), so the transcripts are generated larger than one 4 MiB batch,
//! and a hook is kept short of time by holding the session's lock as another
//! `cvc` would. Nothing here is a captured prompt: every transcript is
//! generated from invented words, with nothing shaped like a credential.
use chrono::{SecondsFormat, TimeZone, Utc};
use cvc_core::claude_code::settings::HOOK_TIMEOUT_SECS;
use cvc_core::claude_code::Adapter;
use cvc_core::db::CvcStore;
use cvc_core::harness::adapter::HookCaller;
use cvc_core::harness::engine::HOOK_BATCH;
use cvc_core::models::FutureSharePolicy;
use cvc_core::sync;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const SESSION: &str = "5d2e8a41-7c3b-4e19-a06f-2b9c4d7e1f83";
const AGENT_A: &str = "agent-a7e3c1d9a7e3c1d9a";
const AGENT_B: &str = "agent-b8f4d2e0b8f4d2e0b";
const META: &str = r#"{"agentType":"explorer","description":"Map the parser"}"#;
const VERSION: &str = "2.1.300";
/// The destination every conversation is shared with for the projection.
const DESTINATION: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
/// Claude Code kills a hook after this long.
const HOOK_TIMEOUT: Duration = Duration::from_secs(HOOK_TIMEOUT_SECS);

/// The most one batch of a hook run reads.
fn hook_batch() -> usize {
    HOOK_BATCH.bytes.unwrap() as usize
}
/// The most hook runs a sequence may take before the test calls it stuck.
const MAX_RUNS: usize = 30;

/// See `claude_code_cli.rs`: a test run from a Claude Code or Cursor session
/// inherits these, and each changes what hook mode does.
const AGENT_ENV: [&str; 4] = [
    "CLAUDECODE",
    "CLAUDE_PLUGIN_ROOT",
    "CLAUDE_PROJECT_DIR",
    "CURSOR_VERSION",
];

/// Git's background maintenance keeps writing under `.git` after a command
/// returns (#217); see `claude_code_cli.rs`.
const NO_BACKGROUND_MAINTENANCE: [(&str, &str); 5] = [
    ("GIT_CONFIG_COUNT", "2"),
    ("GIT_CONFIG_KEY_0", "maintenance.auto"),
    ("GIT_CONFIG_VALUE_0", "false"),
    ("GIT_CONFIG_KEY_1", "gc.auto"),
    ("GIT_CONFIG_VALUE_1", "0"),
];

// ---- the repository ---------------------------------------------------------------------

struct Fixture {
    temp: TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir(&home).unwrap();
        fs::create_dir(home.join("templates")).unwrap();
        fs::create_dir(&repo).unwrap();
        let fixture = Self { temp, home, repo };
        fixture.git(&["init", "-q"], &[]);
        fixture.git(&["config", "user.email", "test@example.invalid"], &[]);
        fixture.git(&["config", "user.name", "CVC test"], &[]);
        fs::write(fixture.repo.join("tracked"), "initial\n").unwrap();
        fixture.git(&["add", "tracked"], &[]);
        fixture.git(&["commit", "-q", "-m", "initial"], &[]);
        fixture
    }

    /// The canonical worktree root, as Claude Code records `cwd`.
    fn root(&self) -> PathBuf {
        fs::canonicalize(&self.repo).unwrap()
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        for name in AGENT_ENV {
            command.env_remove(name);
        }
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("empty.gitconfig"))
            .envs(NO_BACKGROUND_MAINTENANCE)
            .env("GIT_TEMPLATE_DIR", self.home.join("templates"))
            .stdin(Stdio::null());
        command
    }

    fn git(&self, args: &[&str], env: &[(&str, String)]) -> String {
        let mut command = self.command("git");
        command.args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// A real commit of `file` dated `secs`, made before `cvc init`, so that
    /// no post-commit hook links it: only the transcripts do. Returns its id.
    fn commit_at(&self, file: &str, secs: i64) -> String {
        fs::write(self.repo.join(file), format!("// {file}\n")).unwrap();
        let date = format!("@{secs} +0000");
        let dates = [
            ("GIT_AUTHOR_DATE", date.clone()),
            ("GIT_COMMITTER_DATE", date),
        ];
        self.git(&["add", file], &[]);
        self.git(&["commit", "-q", "-m", file], &dates);
        self.git(&["rev-parse", "HEAD"], &[])
    }

    /// `cvc init`, and the capture acknowledgement, which the CLI takes as a
    /// typed TTY challenge: granted through the core API as it would be.
    fn init_capturing(&self) {
        self.cvc_ok(&["init"]);
        let repo = git2::Repository::open(&self.repo).unwrap();
        cvc_core::privacy::acknowledge_capture(&repo).unwrap();
    }

    fn cvc(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_cvc"))
            .args(args)
            .output()
            .unwrap()
    }

    fn cvc_ok(&self, args: &[&str]) -> String {
        let output = self.cvc(args);
        assert!(
            output.status.success(),
            "cvc {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// One manual `cvc ingest claude-code --transcript` run.
    fn manual(&self, transcript: &Path) -> String {
        self.cvc_ok(&[
            "ingest",
            "claude-code",
            "--transcript",
            transcript.to_str().unwrap(),
        ])
    }

    /// Starts `cvc ingest claude-code --hook` as Claude Code does, with the
    /// payload of `event` written to its stdin and stdin closed.
    fn spawn_hook(&self, transcript: &Path, event: &str) -> Child {
        let payload = json!({
            "session_id": SESSION,
            "transcript_path": transcript,
            "cwd": self.repo,
            "hook_event_name": event,
        })
        .to_string();
        let mut child = self
            .command(env!("CARGO_BIN_EXE_cvc"))
            .args(["ingest", "claude-code", "--hook"])
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        child
    }

    fn db_path(&self) -> PathBuf {
        self.repo.join(".git").join("cvc").join("index.db")
    }

    /// Removes the store, so that the next run starts from nothing; the
    /// repository, its commits, the privacy state and the transcripts stay.
    fn reset_store(&self) {
        let db = self.db_path();
        for suffix in ["", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", db.display()));
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("{error}"),
            }
        }
    }

    /// The 0.7.x cursor of `conversation`, read without writing anything, as
    /// a concurrent reader would; `None` until a batch has committed one.
    fn legacy_cursor(&self, conversation: &str) -> Option<u64> {
        let conn =
            Connection::open_with_flags(self.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        conn.query_row(
            "SELECT byte_offset FROM harness_ingest_cursors WHERE harness='claude-code' AND session_id=?1",
            [conversation],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .map(|offset| offset as u64)
    }

    /// The carry of `conversation`'s typed cursor (`{}` when it has none).
    fn carry(&self, conversation: &str) -> Value {
        let conn =
            Connection::open_with_flags(self.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let state: String = conn
            .query_row(
                "SELECT state FROM harness_cursors WHERE harness='claude-code' AND cursor_key=?1",
                [conversation],
                |row| row.get(0),
            )
            .unwrap();
        let state: Value = serde_json::from_str(&state).unwrap();
        state.get("carry").cloned().unwrap_or_else(|| json!({}))
    }

    fn backlog_flag(&self) -> bool {
        self.carry(SESSION)["backlog"] == json!(true)
    }

    fn interaction_count(&self) -> i64 {
        Connection::open_with_flags(self.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
            .unwrap()
    }

    fn integrity_check(&self) -> String {
        Connection::open(self.db_path())
            .unwrap()
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap()
    }

    fn ingest_lock_files(&self) -> Vec<String> {
        fs::read_dir(self.repo.join(".git").join("cvc"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with("ingest-"))
            .collect()
    }

    /// The session's ingest lock, held as another `cvc` process holds it
    /// (the name is written out from `cvc-core/src/harness/lock.rs`).
    fn hold_session_lock(&self) -> fs::File {
        let mut hash = Sha256::new();
        hash.update(b"cvc.ingest-lock/v1\0claude-code\0");
        hash.update(SESSION.as_bytes());
        let path = self
            .repo
            .join(".git")
            .join("cvc")
            .join(format!("ingest-{}.lock", hex::encode(hash.finalize())));
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0);
        file
    }

    /// Every row the comparison looks at, and the published tree. Shares
    /// every conversation first for the projection, so it is taken last.
    fn snapshot(&self) -> Snapshot {
        let conn = Connection::open(self.db_path()).unwrap();
        let mut tables = BTreeMap::new();
        for (name, sql) in TABLES {
            tables.insert(*name, rows(&conn, sql));
        }
        let conversations: Vec<String> = conn
            .prepare("SELECT id FROM conversations ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        drop(conn);
        let repo = git2::Repository::open(&self.repo).unwrap();
        let store = CvcStore::open(self.db_path()).unwrap();
        for conversation in &conversations {
            store
                .share_conversation_for_remote(
                    conversation,
                    DESTINATION,
                    FutureSharePolicy::Private,
                )
                .unwrap();
        }
        let baseline =
            sync::ImportedBaseline::observe(&repo, "refs/remotes/unpublished/cvc/main").unwrap();
        let published =
            match sync::push_projection_to_ref(&repo, &store, &baseline, DESTINATION).unwrap() {
                sync::ProjectionResult::Candidate { oid, candidate, .. } => {
                    let tree = repo.find_commit(oid).unwrap().tree().unwrap();
                    let mut files = Vec::new();
                    walk(&repo, &tree, "", &mut files);
                    drop(candidate);
                    files.sort();
                    files
                }
                sync::ProjectionResult::NoChanges { .. } => panic!("nothing to publish"),
            };
        Snapshot { tables, published }
    }
}

/// The tables the Claude Code goldens dump, without surrogate row ids
/// (`context_items` and `tool_executions` count insertions, which follow the
/// order runs reached each conversation) and wall-clock columns, and the
/// harness tables.
const TABLES: &[(&str, &str)] = &[
    (
        "conversations",
        "SELECT id, title, created_at FROM conversations ORDER BY id",
    ),
    (
        "interactions",
        "SELECT id, conversation_id, parent_id, timestamp, author, user_prompt, model_name, \
         model_cot, model_response, source_request_id, visibility, capture_source, \
         scrubber_version, capture_worktree FROM interactions ORDER BY id",
    ),
    (
        "context_items",
        "SELECT interaction_id, file_path, git_blob_sha, dirty_patch, start_line, end_line \
         FROM context_items ORDER BY interaction_id, id",
    ),
    (
        "tool_executions",
        "SELECT interaction_id, tool_protocol, tool_name, arguments, status \
         FROM tool_executions ORDER BY interaction_id, id",
    ),
    (
        "artifact_links",
        "SELECT interaction_id, git_commit_hash, link_type, linked_by FROM artifact_links \
         ORDER BY interaction_id, git_commit_hash",
    ),
    (
        "harness_ingest_cursors",
        "SELECT harness, session_id, transcript_path, byte_offset FROM harness_ingest_cursors \
         ORDER BY harness, session_id",
    ),
    (
        "harness_cursors",
        "SELECT harness, cursor_key, kind, version, epoch, state FROM harness_cursors \
         ORDER BY harness, cursor_key",
    ),
    (
        "harness_commit_evidence",
        "SELECT harness, family_root, interaction_id, commit_oid, response_at \
         FROM harness_commit_evidence ORDER BY harness, family_root, interaction_id, commit_oid",
    ),
    (
        "conversation_relations",
        "SELECT child_id, parent_id, kind, harness, spawn_interaction_id, spawn_call_id, \
         evidence FROM conversation_relations ORDER BY child_id",
    ),
    (
        "derivation_observations",
        "SELECT event_id, source_fingerprint, source_key, origin, trusted_local \
         FROM derivation_observations ORDER BY event_id, source_key",
    ),
    (
        "derivation_events",
        "SELECT event_id, interaction_id, target_commit, relation, evidence_version, \
         evidence_kind, origin, source_event_ids, old_oid, new_oid, range_id, linked_by, payload \
         FROM derivation_events ORDER BY event_id",
    ),
    (
        "tombstones",
        "SELECT interaction_id, scope_kind, remote_fingerprint, reason_code, source, state \
         FROM tombstones ORDER BY interaction_id, scope_kind, remote_fingerprint",
    ),
];

#[derive(Debug, PartialEq)]
struct Snapshot {
    tables: BTreeMap<&'static str, Vec<Value>>,
    /// Every published file, by path and blob id (the hash of its bytes).
    published: Vec<(String, String)>,
}

/// `actual` is `expected`, said table by table.
fn assert_same(expected: &Snapshot, actual: &Snapshot, what: &str) {
    for (name, rows) in &expected.tables {
        let other = &actual.tables[name];
        if rows != other {
            let first = rows
                .iter()
                .zip(other)
                .position(|(a, b)| a != b)
                .unwrap_or(rows.len().min(other.len()));
            panic!(
                "{what}: table {name} differs ({} rows against {}), first at row {first}:\n expected {}\n actual   {}",
                rows.len(),
                other.len(),
                rows.get(first).map_or("nothing".into(), Value::to_string),
                other.get(first).map_or("nothing".into(), Value::to_string),
            );
        }
    }
    assert_eq!(
        expected.published, actual.published,
        "{what}: the published tree differs"
    );
    assert_eq!(expected, actual, "{what}");
}

fn rows(conn: &Connection, sql: &str) -> Vec<Value> {
    let mut statement = conn.prepare(sql).unwrap();
    let names: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    statement
        .query_map([], |row| {
            let mut object = serde_json::Map::new();
            for (index, name) in names.iter().enumerate() {
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(n) => json!(n),
                    ValueRef::Text(text) => json!(String::from_utf8_lossy(text)),
                    ValueRef::Blob(blob) => json!(blob),
                };
                object.insert(name.clone(), value);
            }
            Ok(Value::Object(object))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn walk(
    repo: &git2::Repository,
    tree: &git2::Tree<'_>,
    prefix: &str,
    files: &mut Vec<(String, String)>,
) {
    for entry in tree.iter() {
        let path = format!("{prefix}{}", entry.name().unwrap());
        match entry.kind() {
            Some(git2::ObjectType::Tree) => {
                let subtree = repo.find_tree(entry.id()).unwrap();
                walk(repo, &subtree, &format!("{path}/"), files);
            }
            _ => files.push((path, entry.id().to_string())),
        }
    }
}

/// What a hook must be on every path: exit 0, nothing on either stream
/// (Claude Code takes stdout as hook output and shows stderr).
fn assert_silent_success(output: &Output, what: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{what}: {:?} {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "{what}: stdout {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "{what}: stderr {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---- transcripts ------------------------------------------------------------------------

fn at(secs: i64) -> String {
    Utc.timestamp_opt(secs, 0)
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Text of about `bytes` bytes, from invented words, the same for a seed.
fn filler(seed: u64, bytes: usize) -> String {
    const WORDS: [&str; 16] = [
        "parser", "window", "cursor", "batch", "record", "family", "subagent", "budget",
        "deadline", "rescue", "evidence", "title", "stall", "branch", "review", "module",
    ];
    let mut text = String::with_capacity(bytes + 16);
    let mut state = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    while text.len() < bytes {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        text.push_str(WORDS[(state >> 60) as usize]);
        text.push(if state.is_multiple_of(13) { '\n' } else { ' ' });
    }
    text
}

/// A Claude Code transcript, line by line, each entry chained to the last.
struct Script {
    agent: Option<String>,
    cwd: String,
    lines: Vec<String>,
    len: usize,
    last: Option<String>,
}

impl Script {
    fn main(cwd: &Path) -> Self {
        Self {
            agent: None,
            cwd: cwd.to_str().unwrap().to_owned(),
            lines: Vec::new(),
            len: 0,
            last: None,
        }
    }

    /// `stem` is the file stem, `agent-<id>`.
    fn subagent(stem: &str, cwd: &Path) -> Self {
        Self {
            agent: Some(stem.trim_start_matches("agent-").to_owned()),
            ..Self::main(cwd)
        }
    }

    fn line(&mut self, line: String) {
        self.len += line.len() + 1;
        self.lines.push(line);
    }

    fn push(&mut self, kind: &str, secs: i64, request: Option<&str>, message: Value) {
        let uuid = format!(
            "{}-{}",
            self.agent.as_deref().unwrap_or("main"),
            self.lines.len()
        );
        let mut entry = json!({
            "parentUuid": self.last,
            "isSidechain": self.agent.is_some(),
            "type": kind,
            "uuid": uuid,
            "timestamp": at(secs),
            "sessionId": SESSION,
            "version": VERSION,
            "cwd": self.cwd,
            "message": message,
        });
        if let Some(agent) = &self.agent {
            entry["agentId"] = json!(agent);
        }
        if let Some(request) = request {
            entry["requestId"] = json!(request);
        }
        self.line(entry.to_string());
        self.last = Some(uuid);
    }

    fn assistant(&mut self, request: &str, secs: i64, blocks: Value, stop_reason: &str) {
        let message = json!({
            "model": "claude-fixture", "id": format!("msg-{request}"), "type": "message",
            "role": "assistant", "content": blocks, "stop_reason": stop_reason,
            "stop_sequence": null, "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        self.push("assistant", secs, Some(request), message);
    }

    fn user(&mut self, secs: i64, content: Value) {
        self.push(
            "user",
            secs,
            None,
            json!({"role": "user", "content": content}),
        );
    }

    fn result(&mut self, secs: i64, tool: &str, text: &str) {
        self.user(
            secs,
            json!([{"tool_use_id": tool, "type": "tool_result", "content": text, "is_error": false}]),
        );
    }

    fn prompt(&mut self, secs: i64, text: &str) {
        self.user(secs, json!(text));
    }

    /// A response in two entries, its reasoning and then its text.
    fn respond(&mut self, request: &str, secs: i64, thinking: &str, text: &str) {
        self.assistant(
            request,
            secs,
            json!([{"type": "thinking", "thinking": thinking, "signature": "s"}]),
            "end_turn",
        );
        self.assistant(
            request,
            secs,
            json!([{"type": "text", "text": text}]),
            "end_turn",
        );
    }

    /// A response that calls `tool` with `input`, and its result a second
    /// later.
    fn call(&mut self, request: &str, secs: i64, tool: &str, input: Value, output: &str) {
        let id = format!("tool-{request}");
        self.assistant(
            request,
            secs,
            json!([
                {"type": "thinking", "thinking": format!("Calling {tool}."), "signature": "s"},
                {"type": "tool_use", "id": id, "name": tool, "input": input}
            ]),
            "tool_use",
        );
        self.result(secs + 1, &id, output);
    }

    /// A response that commits, and git's output naming the commit `sha`.
    fn commit(&mut self, request: &str, secs: i64, sha: &str, subject: &str) {
        self.call(
            request,
            secs,
            "Bash",
            json!({"command": format!("git commit -am '{subject}'")}),
            &format!("[main {}] {subject}\n 1 file changed", &sha[..7]),
        );
    }

    fn title(&mut self, kind: &str, key: &str, title: &str) {
        self.line(json!({"type": kind, key: title, "sessionId": SESSION}).to_string());
    }

    fn text(&self) -> String {
        self.lines.join("\n") + "\n"
    }
}

/// A long session of at least `bytes` bytes, laid out as Claude Code lays it
/// out: turns of prompts, reasoning, reads and edits, three real commits, two
/// subagents (one of which commits), a generated title, one the person set
/// and a generated one after it. Returns the session's transcript.
fn long_session(fixture: &Fixture, bytes: usize) -> PathBuf {
    // About ten kilobytes, and ten seconds, a turn.
    let turns = (bytes / 9_000) as i64 + 1;
    let start = Utc::now().timestamp() - 10 * turns - 3600;
    let turn = |i: i64| start + 10 * i;
    let root = fixture.root();
    let file = |i: i64| root.join(format!("src/m{:02}.rs", i % 40));
    let commits: BTreeMap<i64, String> = [turns / 5, turns / 2, 4 * turns / 5]
        .into_iter()
        .map(|i| (i, fixture.commit_at(&format!("c{i}.rs"), turn(i) + 3)))
        .collect();
    let spawn_a = turns / 3;
    let spawn_b = 2 * turns / 3;
    let agent_commit = fixture.commit_at("agent.rs", turn(spawn_a) + 8);

    let mut main = Script::main(&root);
    let mut agent_a = Script::subagent(AGENT_A, &root);
    let mut agent_b = Script::subagent(AGENT_B, &root);
    let mut i = 0i64;
    while main.len < bytes || i <= 4 * turns / 5 {
        let t = turn(i);
        let seed = i as u64;
        if i == 2 {
            main.title("ai-title", "aiTitle", "Sweep the parser");
        }
        if i == turns / 4 {
            main.title("custom-title", "customTitle", "Parser sweep, by hand");
        }
        if i == 9 * turns / 10 {
            main.title("ai-title", "aiTitle", "Parser and cursor sweep");
        }
        main.prompt(t, &format!("Turn {i}: {}", filler(seed, 120)));
        let request = |part: &str| format!("req_{i:05}_{part}");
        if let Some(sha) = commits.get(&i) {
            main.commit(&request("a"), t + 1, sha, &format!("work {i}"));
            main.respond(&request("b"), t + 4, &filler(seed + 7, 600), "Committed.");
        } else if i == spawn_a || i == spawn_b {
            let agent = if i == spawn_a {
                &mut agent_a
            } else {
                &mut agent_b
            };
            let id = format!("tool-{}", request("a"));
            main.assistant(
                &request("a"),
                t + 1,
                json!([{"type": "tool_use", "id": id, "name": "Task",
                        "input": {"description": "Map the parser", "prompt": "Map it", "subagent_type": "explorer"}}]),
                "tool_use",
            );
            agent.prompt(t + 2, "Map the parser and report back.");
            for k in 0..12i64 {
                let at = t + 3 + k / 3;
                let sub = format!("req_sub_{i:05}_{k:02}");
                match k % 3 {
                    0 => agent.call(
                        &sub,
                        at,
                        "Read",
                        json!({"file_path": file(i + k).to_str().unwrap()}),
                        &filler(seed * 100 + k as u64, 6000),
                    ),
                    1 if i == spawn_a && k == 7 => {
                        agent.commit(&sub, t + 6, &agent_commit, "agent work")
                    }
                    _ => agent.respond(&sub, at, &filler(seed * 100 + k as u64, 2500), "Noted."),
                }
            }
            main.result(t + 8, &id, "The subagent mapped the parser.");
            main.respond(
                &request("b"),
                t + 9,
                &filler(seed + 7, 1500),
                "Delegated and done.",
            );
        } else {
            match i % 3 {
                0 => main.respond(
                    &request("a"),
                    t + 1,
                    &filler(seed, 4000),
                    &filler(seed + 1, 300),
                ),
                1 => {
                    main.call(
                        &request("a"),
                        t + 1,
                        "Read",
                        json!({"file_path": file(i).to_str().unwrap(), "offset": 1, "limit": 200}),
                        &filler(seed + 2, 10_000),
                    );
                    main.respond(&request("b"), t + 3, &filler(seed + 3, 2500), "Read it.");
                }
                _ => {
                    main.call(
                        &request("a"),
                        t + 1,
                        "Edit",
                        json!({"file_path": file(i).to_str().unwrap(), "old_string": "a", "new_string": "b"}),
                        "The file was updated.",
                    );
                    main.respond(&request("b"), t + 3, &filler(seed + 4, 2500), "Edited.");
                }
            }
        }
        i += 1;
    }

    let dir = fixture.temp.path().join("transcripts");
    let sub_dir = dir.join(SESSION).join("subagents");
    fs::create_dir_all(&sub_dir).unwrap();
    let path = dir.join(format!("{SESSION}.jsonl"));
    fs::write(&path, main.text()).unwrap();
    for (stem, script) in [(AGENT_A, &agent_a), (AGENT_B, &agent_b)] {
        fs::write(sub_dir.join(format!("{stem}.jsonl")), script.text()).unwrap();
        fs::write(sub_dir.join(format!("{stem}.meta.json")), META).unwrap();
    }
    path
}

/// Waits until the running `cvc` has committed a batch that moved the
/// session's cursor past `past`, and returns that cursor; `None` when the run
/// ended first.
fn wait_for_a_batch_past(fixture: &Fixture, child: &mut Child, past: u64) -> Option<u64> {
    let deadline = Instant::now() + HOOK_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(cursor) = fixture
            .legacy_cursor(SESSION)
            .filter(|cursor| *cursor > past)
        {
            return Some(cursor);
        }
        if child.try_wait().unwrap().is_some() {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no batch committed within {HOOK_TIMEOUT:?}");
}

// ---- tests ------------------------------------------------------------------------------

/// A hook run killed by `SIGKILL` once it has committed a batch, twice over,
/// loses nothing and records nothing twice: the next hook runs go on from the
/// cursors, and the session ends exactly as one manual `--transcript` run
/// leaves it. A killed run's lock is the operating system's, so nothing waits
/// for it, and no lock file is left once a run ends normally.
#[test]
fn a_hook_killed_mid_run_loses_nothing_and_later_runs_reach_the_manual_state() {
    let fixture = Fixture::new();
    // Over three batches of the hook's real size, so that a run commits a
    // batch well before it can finish.
    let transcript = long_session(&fixture, 3 * hook_batch() + hook_batch() / 4);
    let size = fs::metadata(&transcript).unwrap().len();
    assert!(size > 3 * hook_batch() as u64, "{size}");
    fixture.init_capturing();

    // One manual run.
    let stdout = fixture.manual(&transcript);
    assert!(stdout.contains("new thought(s)"), "{stdout}");
    let expected = fixture.snapshot();
    assert_eq!(
        expected.tables["conversations"].len(),
        3,
        "the session and its two subagents"
    );
    let links = &expected.tables["artifact_links"];
    assert!(links.len() > 4, "{links:?}");
    let titles: Vec<&Value> = expected.tables["conversations"]
        .iter()
        .map(|row| &row["title"])
        .collect();
    assert!(
        titles.contains(&&json!("Parser sweep, by hand")),
        "{titles:?}"
    );
    fixture.reset_store();

    // Killed twice, each time once a batch of the run has committed.
    let mut cursor = 0;
    for kill in 1..=2 {
        let mut child = fixture.spawn_hook(&transcript, "PostToolUse");
        let Some(reached) = wait_for_a_batch_past(&fixture, &mut child, cursor) else {
            panic!(
                "kill {kill}: the run ended before a batch could be seen: {:?}",
                child.wait_with_output().unwrap()
            )
        };
        child.kill().unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGKILL),
            "kill {kill}: the run ended before it could be killed: {output:?}"
        );
        assert!(output.stdout.is_empty(), "kill {kill}");
        assert!(reached < size, "kill {kill}: {reached} of {size}");
        assert_eq!(fixture.integrity_check(), "ok", "kill {kill}");
        // What the killed run committed stays, and nothing moved back.
        let now = fixture.legacy_cursor(SESSION).unwrap();
        assert!(now >= reached, "kill {kill}: {now} < {reached}");
        assert!(fixture.interaction_count() > 0);
        cursor = now;
    }

    // Then the session's own events until one reads it to its end: a `Stop`
    // that runs out of time leaves the backlog flag, and one that reads
    // everything takes it away.
    let mut runs = 0;
    loop {
        runs += 1;
        assert!(runs <= MAX_RUNS, "{MAX_RUNS} hook runs did not finish");
        let started = Instant::now();
        let output = fixture
            .spawn_hook(&transcript, "Stop")
            .wait_with_output()
            .unwrap();
        let took = started.elapsed();
        assert_silent_success(&output, &format!("Stop run {runs}"));
        assert!(took < HOOK_TIMEOUT, "Stop run {runs} took {took:?}");
        if !fixture.backlog_flag() {
            break;
        }
    }
    assert_eq!(
        fixture.ingest_lock_files(),
        Vec::<String>::new(),
        "no lock file is left behind"
    );
    assert_same(
        &expected,
        &fixture.snapshot(),
        "two killed hook runs, then Stop runs",
    );
    let status = fixture.cvc_ok(&["harness", "status", "claude-code"]);
    assert!(!status.contains("backlog"), "{status}");
}

/// R7: a `SessionEnd` hook that runs out of time, here because another run
/// held the session's lock for most of its budget, stops between batches
/// before Claude Code's timeout, prints nothing, and leaves the backlog flag
/// on the session's cursor. `cvc harness status claude-code` lists the
/// session with the catch-up command; running it reads the rest and clears
/// the flag, and the status no longer lists it.
#[test]
fn a_session_end_hook_out_of_time_is_silent_and_harness_status_lists_the_backlog() {
    let fixture = Fixture::new();
    let transcript = long_session(&fixture, 3 * hook_batch());
    let size = fs::metadata(&transcript).unwrap().len();
    fixture.init_capturing();
    let status = fixture.cvc_ok(&["harness", "status", "claude-code"]);
    assert!(!status.contains("backlog"), "{status}");

    // The holder lets go a second before the hook would stop waiting (with
    // a 20-second budget its lock wait ends four seconds before its
    // deadline), which leaves the hook about three seconds to read beyond
    // the two it keeps in reserve: far less than the session needs.
    let budget = Adapter.budget(Some("SessionEnd"));
    assert_eq!(budget, Duration::from_secs(20), "the lock wait below");
    let holder = fixture.hold_session_lock();
    let started = Instant::now();
    let child = fixture.spawn_hook(&transcript, "SessionEnd");
    std::thread::sleep(budget - Duration::from_secs(5));
    drop(holder);
    let output = child.wait_with_output().unwrap();
    let took = started.elapsed();
    assert_silent_success(&output, "the SessionEnd hook");
    assert!(took < HOOK_TIMEOUT, "{took:?}");
    let cursor = fixture.legacy_cursor(SESSION);
    assert!(
        cursor.is_some_and(|cursor| cursor > 0 && cursor < size),
        "it read part of the session: {cursor:?} of {size}"
    );
    assert!(fixture.backlog_flag(), "{}", fixture.carry(SESSION));

    let status = fixture.cvc_ok(&["harness", "status", "claude-code"]);
    assert!(
        status.contains(
            "backlog: 1 session(s) that a hook run left unread to the end, and that no run has read to the end since; catch up with `cvc ingest claude-code --transcript ~/.claude/projects/<workspace>/<id>.jsonl`"
        ),
        "{status}"
    );
    assert!(
        status.contains(&format!("backlog_session: {SESSION}\n")),
        "{status}"
    );

    fixture.manual(&transcript);
    assert!(!fixture.backlog_flag(), "{}", fixture.carry(SESSION));
    let status = fixture.cvc_ok(&["harness", "status", "claude-code"]);
    assert!(!status.contains("backlog"), "{status}");
}
