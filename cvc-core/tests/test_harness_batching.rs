//! Batched hook ingest with a time budget (#149, #187 PR 4), with Claude Code
//! through the public API.
//!
//! A hook run reads a session's record in batches, each committed with its
//! cursor, and stops between batches before its deadline; the next run goes
//! on from the cursors. What this pins:
//!
//! * many hook runs, each with a small batch and a small time budget, on a
//!   generated multi-MB transcript with subagents and real commits, reach
//!   exactly the state one manual `--transcript` run reaches: every row of
//!   the tables the Claude Code goldens dump, the typed and 0.7.x cursors,
//!   the commit evidence, the links and the published node bytes. Only the
//!   surrogate row ids of `context_items` and `tool_executions` (insertion
//!   counters, which follow the order runs reached each conversation) and
//!   wall-clock columns are left out;
//! * so do one run in many batches, final hook runs alone, and a run that
//!   fails between batches (as one killed there would) followed by hook
//!   runs: nothing is lost and nothing is recorded twice;
//! * a title the person set is kept against a generated one that a later
//!   window holds alone (R5);
//! * a response that stays open past many windows grows the window, and the
//!   grown window carries over to the next run (R9);
//! * a final run that runs out of time leaves the backlog flag (R7).
//!
//! Nothing here is a captured prompt: every transcript is generated from
//! invented words, with nothing shaped like a credential.
#![cfg(unix)]
use chrono::{SecondsFormat, TimeZone, Utc};
use cvc_core::claude_code::Adapter;
use cvc_core::db::CvcStore;
use cvc_core::harness::adapter::{ReadLimit, RecordSource};
use cvc_core::harness::candidate::{Finality, TitleRank};
use cvc_core::harness::cursor::{CursorCodec, JsonlCursor};
use cvc_core::harness::engine::{self, EngineError, EngineOptions, EngineReport};
use cvc_core::harness::{HarnessId, SESSION_LOCK_WAIT};
use cvc_core::models::FutureSharePolicy;
use cvc_core::privacy::{self, PreparedPolicy};
use cvc_core::repository::RepositoryLayout;
use cvc_core::sync;
use git2::{ObjectType, Repository};
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const SESSION: &str = "3a9c5e71-2b4d-4f86-9e0a-1c7d3b5f9e24";
const AGENT_A: &str = "agent-a51c0de7a51c0de7a";
const AGENT_B: &str = "agent-b62d1ef8b62d1ef8b";
const META: &str = r#"{"agentType":"explorer","description":"Map the parser"}"#;
const VERSION: &str = "2.1.300";
/// The destination every conversation is shared with for the projection.
const DESTINATION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// The most hook runs a sequence may take before the test calls it stuck.
const MAX_RUNS: usize = 5000;

// ---- the repository ---------------------------------------------------------------------

struct Fixture {
    _temp: TempDir,
    layout: RepositoryLayout,
    root: PathBuf,
    policy: PreparedPolicy,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Fixture").unwrap();
        config
            .set_str("user.email", "fixture@example.invalid")
            .unwrap();
        drop(repo);
        let layout = RepositoryLayout::discover(temp.path()).unwrap();
        let root = layout.worktree_root().unwrap().to_path_buf();
        privacy::acknowledge_capture(layout.repository()).unwrap();
        let policy = PreparedPolicy::load(&root).unwrap();
        Self {
            _temp: temp,
            layout,
            root,
            policy,
        }
    }

    fn store(&self) -> CvcStore {
        CvcStore::open(self.layout.db_path()).unwrap()
    }

    /// Removes the store, so that the next run starts from nothing; the
    /// repository, its commits and the transcripts stay.
    fn reset(&self, store: CvcStore) {
        drop(store);
        let db = self.layout.db_path();
        for suffix in ["", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", db.display()));
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("{error}"),
            }
        }
    }

    /// A real commit of `file` at `secs`, so that a transcript can name a
    /// hash that resolves. Returns its id.
    fn commit_at(&self, file: &str, secs: i64) -> String {
        fs::write(self.root.join(file), format!("// {file}\n")).unwrap();
        let repo = self.layout.repository();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(file)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::new(
            "Fixture",
            "fixture@example.invalid",
            &git2::Time::new(secs, 0),
        )
        .unwrap();
        let parents: Vec<git2::Commit> = repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parents: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, file, &tree, &parents)
            .unwrap()
            .to_string()
    }

    /// Writes the session's transcript and its subagents' beside it, as
    /// Claude Code lays them out, and returns the session's path.
    fn lay_out(&self, main: &Script, subagents: &[(&str, &Script)]) -> PathBuf {
        let dir = self.root.join("proj");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{SESSION}.jsonl"));
        fs::write(&path, main.text()).unwrap();
        for (stem, script) in subagents {
            let sub_dir = dir.join(SESSION).join("subagents");
            fs::create_dir_all(&sub_dir).unwrap();
            fs::write(sub_dir.join(format!("{stem}.jsonl")), script.text()).unwrap();
            fs::write(sub_dir.join(format!("{stem}.meta.json")), META).unwrap();
        }
        path
    }

    fn try_run(
        &self,
        store: &CvcStore,
        path: &Path,
        options: EngineOptions,
    ) -> Result<EngineReport<JsonlCursor>, EngineError<cvc_core::claude_code::IngestError>> {
        let located = Adapter.locate_file(path, Some(SESSION)).unwrap();
        engine::ingest(
            &Adapter,
            &self.layout,
            store,
            &self.policy,
            located,
            options,
        )
    }

    fn run(
        &self,
        store: &CvcStore,
        path: &Path,
        options: EngineOptions,
    ) -> EngineReport<JsonlCursor> {
        self.try_run(store, path, options)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// One manual `--transcript` run.
    fn manual(&self, store: &CvcStore, path: &Path) -> EngineReport<JsonlCursor> {
        self.run(store, path, EngineOptions::operator(Finality::Final, false))
    }

    /// Hook runs of `finality`, each over by `budget` from its start, in
    /// batches of `batch` bytes, until one reads everything. Returns how
    /// many it took.
    fn hook_runs(
        &self,
        store: &CvcStore,
        path: &Path,
        finality: Finality,
        budget: Duration,
        batch: u64,
    ) -> usize {
        for runs in 1..=MAX_RUNS {
            let report = self.run(
                store,
                path,
                hook_options(finality, Some(Instant::now() + budget), batch),
            );
            if !report.cut_short {
                return runs;
            }
        }
        panic!("{MAX_RUNS} hook runs did not read the session to its end");
    }

    /// Every row the comparison looks at, and the published tree. Shares
    /// every conversation first for the projection, so it is taken last.
    fn snapshot(&self, store: &CvcStore) -> Snapshot {
        let conn = Connection::open(self.layout.db_path()).unwrap();
        let mut tables = BTreeMap::new();
        for (name, sql) in TABLES {
            tables.insert(*name, rows(&conn, sql));
        }
        let mut statement = conn
            .prepare("SELECT id FROM conversations ORDER BY id")
            .unwrap();
        let conversations: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for conversation in &conversations {
            store
                .share_conversation_for_remote(
                    conversation,
                    DESTINATION,
                    FutureSharePolicy::Private,
                )
                .unwrap();
        }
        let repo = self.layout.repository();
        let baseline =
            sync::ImportedBaseline::observe(repo, "refs/remotes/unpublished/cvc/main").unwrap();
        let published =
            match sync::push_projection_to_ref(repo, store, &baseline, DESTINATION).unwrap() {
                sync::ProjectionResult::Candidate { oid, candidate, .. } => {
                    let tree = repo.find_commit(oid).unwrap().tree().unwrap();
                    let mut files = Vec::new();
                    walk(repo, &tree, "", &mut files);
                    drop(candidate);
                    files.sort();
                    files
                }
                sync::ProjectionResult::NoChanges { .. } => panic!("nothing to publish"),
            };
        Snapshot { tables, published }
    }
}

/// A hook's run of `finality`, by `deadline`, in batches of `batch` bytes.
fn hook_options(finality: Finality, deadline: Option<Instant>, batch: u64) -> EngineOptions {
    EngineOptions {
        finality,
        rescan: false,
        lock_wait: SESSION_LOCK_WAIT,
        deadline,
        batch: ReadLimit {
            bytes: Some(batch),
            units: None,
        },
    }
}

/// The tables the Claude Code goldens dump, without surrogate row ids and
/// wall-clock columns, and the harness tables.
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
        "SELECT child_id, parent_id, kind, harness FROM conversation_relations ORDER BY child_id",
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

fn walk(repo: &Repository, tree: &git2::Tree<'_>, prefix: &str, files: &mut Vec<(String, String)>) {
    for entry in tree.iter() {
        let path = format!("{prefix}{}", entry.name().unwrap());
        match entry.kind() {
            Some(ObjectType::Tree) => {
                let subtree = repo.find_tree(entry.id()).unwrap();
                walk(repo, &subtree, &format!("{path}/"), files);
            }
            _ => files.push((path, entry.id().to_string())),
        }
    }
}

/// The typed cursor of `conversation`.
fn typed(store: &CvcStore, conversation: &str) -> JsonlCursor {
    JsonlCursor::decode_stored(
        &store
            .harness_cursor(HarnessId::ClaudeCode, conversation)
            .unwrap()
            .unwrap(),
    )
    .unwrap()
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
    last: Option<String>,
}

impl Script {
    fn main(cwd: &Path) -> Self {
        Self {
            agent: None,
            cwd: cwd.to_str().unwrap().to_owned(),
            lines: Vec::new(),
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

    fn push(&mut self, kind: &str, secs: i64, request: Option<&str>, message: Option<Value>) {
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
        });
        if let Some(message) = message {
            entry["message"] = message;
        }
        if let Some(agent) = &self.agent {
            entry["agentId"] = json!(agent);
        }
        if let Some(request) = request {
            entry["requestId"] = json!(request);
        }
        self.lines.push(entry.to_string());
        self.last = Some(uuid);
    }

    fn assistant(&mut self, request: &str, secs: i64, blocks: Value, stop_reason: &str) {
        let message = json!({
            "model": "claude-fixture", "id": format!("msg-{request}"), "type": "message",
            "role": "assistant", "content": blocks, "stop_reason": stop_reason,
            "stop_sequence": null, "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        self.push("assistant", secs, Some(request), Some(message));
    }

    fn user(&mut self, secs: i64, content: Value) {
        self.push(
            "user",
            secs,
            None,
            Some(json!({"role": "user", "content": content})),
        );
    }

    fn result(&mut self, secs: i64, tool: &str, text: &str) {
        self.user(
            secs,
            json!([{"tool_use_id": tool, "type": "tool_result", "content": text, "is_error": false}]),
        );
    }

    /// What a person (or, in a subagent, the delegating agent) said.
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

    fn ai_title(&mut self, title: &str) {
        self.lines
            .push(json!({"type": "ai-title", "aiTitle": title, "sessionId": SESSION}).to_string());
    }

    fn custom_title(&mut self, title: &str) {
        self.lines.push(
            json!({"type": "custom-title", "customTitle": title, "sessionId": SESSION}).to_string(),
        );
    }

    /// A chained bookkeeping entry with no message, which neither closes the
    /// response before it nor is one.
    fn system(&mut self, secs: i64, text: &str) {
        self.push("system", secs, None, None);
        let last = self.lines.pop().unwrap();
        let mut entry: Value = serde_json::from_str(&last).unwrap();
        entry["subtype"] = json!("informational");
        entry["content"] = json!(text);
        self.lines.push(entry.to_string());
    }

    fn text(&self) -> String {
        self.lines.join("\n") + "\n"
    }
}

/// A long session: 300 turns of prompts, reasoning, reads, edits and three
/// commits, two subagents (one of which commits too), a generated title, a
/// title the person set later, and a generated one after that. About 3 MB.
fn long_session(fixture: &Fixture) -> PathBuf {
    let now = Utc::now().timestamp();
    let start = now - 4 * 3600;
    let turn = |i: i64| start + 30 * i;
    let file = |i: i64| fixture.root.join(format!("src/m{:02}.rs", i % 40));
    fs::create_dir_all(fixture.root.join("src")).unwrap();
    let commits: BTreeMap<i64, String> = [60, 150, 240]
        .into_iter()
        .map(|i| (i, fixture.commit_at(&format!("c{i}.rs"), turn(i) + 3)))
        .collect();
    let agent_commit = fixture.commit_at("agent.rs", turn(100) + 12);

    let mut main = Script::main(&fixture.root);
    let mut agent_a = Script::subagent(AGENT_A, &fixture.root);
    let mut agent_b = Script::subagent(AGENT_B, &fixture.root);
    for i in 0..300i64 {
        let t = turn(i);
        let seed = i as u64;
        if i == 2 {
            main.ai_title("Sweep the parser");
        }
        if i == 120 {
            main.custom_title("Parser sweep, by hand");
        }
        if i == 280 {
            main.ai_title("Parser and cursor sweep");
        }
        main.prompt(t, &format!("Turn {i}: {}", filler(seed, 120)));
        let request = |part: &str| format!("req_{i:03}_{part}");
        if let Some(sha) = commits.get(&i) {
            main.commit(&request("a"), t + 1, sha, &format!("work {i}"));
            main.respond(&request("b"), t + 4, &filler(seed + 7, 600), "Committed.");
            continue;
        }
        if i == 100 || i == 200 {
            let agent = if i == 100 { &mut agent_a } else { &mut agent_b };
            let id = format!("tool-{}", request("a"));
            main.assistant(
                &request("a"),
                t + 1,
                json!([{"type": "tool_use", "id": id, "name": "Task",
                        "input": {"description": "Map the parser", "prompt": "Map it", "subagent_type": "explorer"}}]),
                "tool_use",
            );
            agent.prompt(t + 2, "Map the parser and report back.");
            for k in 0..20i64 {
                let at = t + 3 + k / 2;
                let sub = format!("req_sub_{i:03}_{k:02}");
                match k % 3 {
                    0 => agent.call(
                        &sub,
                        at,
                        "Read",
                        json!({"file_path": file(i + k).to_str().unwrap()}),
                        &filler(seed * 100 + k as u64, 6000),
                    ),
                    1 if i == 100 && k == 16 => {
                        agent.commit(&sub, t + 10, &agent_commit, "agent work")
                    }
                    _ => agent.respond(&sub, at, &filler(seed * 100 + k as u64, 2500), "Noted."),
                }
            }
            main.result(t + 20, &id, "The subagent mapped the parser.");
            main.respond(
                &request("b"),
                t + 21,
                &filler(seed + 7, 1500),
                "Delegated and done.",
            );
            continue;
        }
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
    fixture.lay_out(&main, &[(AGENT_A, &agent_a), (AGENT_B, &agent_b)])
}

// ---- tests ------------------------------------------------------------------------------

/// The whole of #149's acceptance on one generated session, against one
/// manual run of it.
#[test]
fn many_hook_runs_reach_the_state_of_one_manual_run() {
    let fixture = Fixture::new();
    let path = long_session(&fixture);
    let size = fs::metadata(&path).unwrap().len();
    assert!(size > 2_500_000, "{size}");

    // One manual run.
    let store = fixture.store();
    let started = Instant::now();
    let manual = fixture.manual(&store, &path);
    let took = started.elapsed();
    assert!(!manual.cut_short);
    assert_eq!(manual.members.len(), 2);
    assert!(
        manual.linked.root > 0 && manual.linked.members > 0,
        "{:?}",
        manual.linked
    );
    let expected = fixture.snapshot(&store);
    fixture.reset(store);
    // Each hook run has a quarter of what the manual run took (and no less
    // than a batch of the smallest size needs), so that the session takes
    // several runs whatever the machine.
    let budget = (took / 4).max(Duration::from_millis(250));
    let batch = 64 * 1024;

    // Hook runs mid-session, then the final event's.
    let store = fixture.store();
    let incremental = fixture.hook_runs(&store, &path, Finality::Incremental, budget, batch);
    let last = fixture.hook_runs(&store, &path, Finality::Final, budget, batch);
    assert!(incremental + last > 2, "{incremental} + {last} runs");
    assert!(engine::backlog::<Adapter>(&store).unwrap().is_empty());
    assert_same(
        &expected,
        &fixture.snapshot(&store),
        "incremental, then final, hook runs",
    );
    fixture.reset(store);

    // Final events alone: each that runs out of time leaves the backlog
    // flag, and the last takes it away.
    let store = fixture.store();
    let first = fixture.run(
        &store,
        &path,
        hook_options(Finality::Final, Some(Instant::now() + budget), batch),
    );
    assert!(first.cut_short && first.backlog);
    assert_eq!(engine::backlog::<Adapter>(&store).unwrap(), [SESSION]);
    fixture.hook_runs(&store, &path, Finality::Final, budget, batch);
    assert!(engine::backlog::<Adapter>(&store).unwrap().is_empty());
    assert!(!typed(&store, SESSION).carry.backlog);
    assert_same(&expected, &fixture.snapshot(&store), "final hook runs");
    fixture.reset(store);

    // One run in many batches, with no deadline.
    let store = fixture.store();
    let one = fixture.run(&store, &path, hook_options(Finality::Final, None, batch));
    assert!(!one.cut_short);
    assert_same(
        &expected,
        &fixture.snapshot(&store),
        "one run in many batches",
    );
    fixture.reset(store);

    // A run that fails between batches, as one killed there would: what it
    // committed is kept, and hook runs go on from there.
    let text = fs::read_to_string(&path).unwrap();
    let middle = text.len() / 2;
    let line_start = text[..middle].rfind('\n').unwrap() + 1;
    let line_end = line_start + text[line_start..].find('\n').unwrap();
    let mut broken = text.clone();
    broken.replace_range(line_start..line_end, &"x".repeat(line_end - line_start));
    fs::write(&path, &broken).unwrap();
    let store = fixture.store();
    let error = fixture
        .try_run(
            &store,
            &path,
            hook_options(Finality::Incremental, None, batch),
        )
        .unwrap_err();
    assert!(matches!(error, EngineError::Adapter(_)), "{error:?}");
    let cursor = typed(&store, SESSION).byte_offset;
    assert!(cursor > 0 && cursor < line_start as u64, "{cursor}");
    assert!(store.conversation_interaction_count(SESSION).unwrap() > 0);
    fs::write(&path, &text).unwrap();
    fixture.hook_runs(&store, &path, Finality::Incremental, budget, batch);
    fixture.hook_runs(&store, &path, Finality::Final, budget, batch);
    assert_same(
        &expected,
        &fixture.snapshot(&store),
        "a failed run, then hook runs",
    );
}

/// A title the person set in an early window is kept when a later window
/// holds only a generated one, as one read of the whole transcript keeps it
/// (R5); without it, the later window's generated title is taken.
#[test]
fn a_title_the_person_set_outlasts_a_later_generated_one_across_batches() {
    let fixture = Fixture::new();
    let start = Utc::now().timestamp() - 3600;
    let session = |custom: bool| {
        let mut main = Script::main(&fixture.root);
        main.ai_title("Generated first");
        for i in 0..40i64 {
            if i == 3 && custom {
                main.custom_title("Named by hand");
            }
            if i == 30 {
                main.ai_title("Generated later");
            }
            main.prompt(start + 10 * i, &format!("Step {i}"));
            main.respond(
                &format!("req_{i:02}"),
                start + 10 * i + 1,
                &filler(i as u64, 2000),
                "Done.",
            );
        }
        fixture.lay_out(&main, &[])
    };
    let title = |store: &CvcStore| store.get_conversation(SESSION).unwrap().unwrap().title;
    let batch = 8 * 1024;

    let path = session(true);
    let store = fixture.store();
    fixture.manual(&store, &path);
    assert_eq!(title(&store), "Named by hand");
    fixture.reset(store);
    let store = fixture.store();
    let report = fixture.run(&store, &path, hook_options(Finality::Final, None, batch));
    assert!(!report.cut_short);
    assert_eq!(title(&store), "Named by hand");
    assert_eq!(
        typed(&store, SESSION).carry.title_rank,
        Some(TitleRank::UserSet)
    );
    fixture.reset(store);

    // The window that holds the later generated title records responses, so
    // it does give its title when no title was set by hand.
    let path = session(false);
    let store = fixture.store();
    fixture.run(&store, &path, hook_options(Finality::Final, None, batch));
    assert_eq!(title(&store), "Generated later");
    assert_eq!(typed(&store, SESSION).carry.title_rank, None);
}

/// A response that stays open past many windows (its tool result comes three
/// MiB later) is read by growing the window, planned open until the window
/// holds its result, so that neither one run in small batches nor hook runs
/// with a short budget close it early or record its result as missing: both
/// end where one manual run does (R9). A hook run that runs out of time while
/// the window grows keeps the grown window with the cursor, before the open
/// response. (That the next run starts from it is pinned with a slowed-down
/// record in the engine's own tests, where it does not depend on how fast
/// this machine parses.)
#[test]
fn a_response_open_past_many_windows_is_read_by_growing_the_window() {
    let fixture = Fixture::new();
    let start = Utc::now().timestamp() - 3600;
    let mut main = Script::main(&fixture.root);
    main.prompt(start, "Build it");
    main.respond("req_00", start + 1, "Starting.", "Starting the build.");
    main.prompt(start + 2, "Go on");
    main.assistant(
        "req_01",
        start + 3,
        json!([{"type": "tool_use", "id": "tool-build", "name": "Bash", "input": {"command": "make"}}]),
        "tool_use",
    );
    // Three MiB of progress entries before the build's result.
    for k in 0..3072i64 {
        main.system(start + 4, &filler(k as u64, 1000));
    }
    main.result(start + 5, "tool-build", "built");
    main.respond("req_02", start + 6, "It built.", "The build passed.");
    main.prompt(start + 7, "Thanks");
    main.respond("req_03", start + 8, "Done.", "Done.");
    let path = fixture.lay_out(&main, &[]);
    let batch = 16 * 1024;

    let store = fixture.store();
    let started = Instant::now();
    fixture.manual(&store, &path);
    let took = started.elapsed();
    let expected = fixture.snapshot(&store);
    assert!(expected.tables["tool_executions"]
        .iter()
        .all(|row| row["status"] == "success"));
    fixture.reset(store);
    // A hook run must be able to read the whole grown window once, with
    // twice its estimate to spare, or it never gets past the response (an
    // operator's run then does): three times what one manual run takes.
    let budget = took * 3 + Duration::from_millis(100);

    // One run in small batches.
    let store = fixture.store();
    let one = fixture.run(&store, &path, hook_options(Finality::Final, None, batch));
    assert!(!one.cut_short);
    assert_eq!(typed(&store, SESSION).carry.stall_window, None);
    assert_same(
        &expected,
        &fixture.snapshot(&store),
        "one run in small batches",
    );
    fixture.reset(store);

    // Hook runs with that budget.
    let store = fixture.store();
    for runs in 1..=MAX_RUNS {
        let report = fixture.run(
            &store,
            &path,
            hook_options(Finality::Incremental, Some(Instant::now() + budget), batch),
        );
        if !report.cut_short {
            break;
        }
        assert!(
            runs < MAX_RUNS,
            "hook runs did not get past the open response"
        );
        let cursor = typed(&store, SESSION);
        if store.conversation_interaction_count(SESSION).unwrap() == 1 {
            // Stopped at the open response: before it, with the window it
            // grew to.
            assert!(cursor.byte_offset > 0);
            if let Some(grown) = cursor.carry.stall_window {
                assert!(grown > batch, "{grown}");
            }
        }
    }
    assert_eq!(typed(&store, SESSION).carry.stall_window, None);
    fixture.hook_runs(&store, &path, Finality::Final, budget, batch);
    assert_same(&expected, &fixture.snapshot(&store), "short hook runs");
}

/// `Stop` and `SessionEnd` may be a session's last events: one that runs out
/// of time before reading every transcript of the family leaves the backlog
/// flag, which only a run that reads them all takes away. A hook prints
/// nothing of it; `cvc harness status` lists it.
#[test]
fn a_final_event_out_of_time_leaves_the_backlog_flag_until_a_run_reads_everything() {
    let fixture = Fixture::new();
    let start = Utc::now().timestamp() - 3600;
    let mut main = Script::main(&fixture.root);
    let mut agent = Script::subagent(AGENT_A, &fixture.root);
    for i in 0..20i64 {
        main.prompt(start + 10 * i, &format!("Step {i}"));
        main.respond(
            &format!("req_{i:02}"),
            start + 10 * i + 1,
            &filler(i as u64, 3000),
            "Done.",
        );
        agent.prompt(start + 10 * i + 2, &format!("Sub-step {i}"));
        agent.respond(
            &format!("req_s{i:02}"),
            start + 10 * i + 3,
            &filler(i as u64 + 50, 3000),
            "Done.",
        );
    }
    let path = fixture.lay_out(&main, &[(AGENT_A, &agent)]);
    let store = fixture.store();
    let past = |finality| hook_options(finality, Some(Instant::now()), 16 * 1024);

    // Out of time before anything was read: the final event flags it, the
    // incremental one does not.
    let incremental = fixture.run(&store, &path, past(Finality::Incremental));
    assert!(incremental.cut_short && !incremental.backlog);
    assert!(engine::backlog::<Adapter>(&store).unwrap().is_empty());
    let session_end = fixture.run(&store, &path, past(Finality::Final));
    assert!(session_end.cut_short && session_end.backlog);
    assert!(session_end.members.iter().all(|member| member.cut_short));
    assert_eq!(engine::backlog::<Adapter>(&store).unwrap(), [SESSION]);
    // A later event that runs out of time too keeps it.
    let again = fixture.run(&store, &path, past(Finality::Incremental));
    assert!(again.cut_short && again.backlog);
    assert_eq!(engine::backlog::<Adapter>(&store).unwrap(), [SESSION]);
    // A run that reads every transcript to its end, in batches, takes it
    // away.
    let complete = fixture.run(
        &store,
        &path,
        hook_options(Finality::Incremental, None, 16 * 1024),
    );
    assert!(!complete.cut_short && !complete.backlog, "{complete:?}");
    assert!(engine::backlog::<Adapter>(&store).unwrap().is_empty());
    // The flag is the root's, carried by its typed cursor only.
    assert!(!typed(&store, SESSION).carry.backlog);
    assert!(!typed(&store, &format!("{SESSION}:{AGENT_A}")).carry.backlog);
}
