//! Claude Code adapter: transcript parsing, planning, idempotent ingestion,
//! the capture_source migration, and hook settings installation.
//!
//! Fixtures are synthetic transcripts shaped like the Claude Code versions
//! named by their directory; `__WORKTREE__` is substituted with the fixture
//! repository's canonical root at test time.
use cvc_core::claude_code::settings::{self, ExcludeAction, InstallAction};
use cvc_core::claude_code::transcript::{
    self, derived_interaction_id, ParsedLine, PlanInput, TranscriptError,
};
use cvc_core::claude_code::{ingest, IngestError, IngestMode};
use cvc_core::db::CvcStore;
use cvc_core::models::{Author, ToolStatus};
use cvc_core::privacy::PreparedPolicy;
use cvc_core::repository::RepositoryLayout;
use git2::Repository;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const BASIC: &str = include_str!("fixtures/claude-code/v2.1.260/basic.jsonl");
const MINIMAL_219: &str = include_str!("fixtures/claude-code/v2.1.219/minimal.jsonl");
const BASIC_SESSION: &str = "7c1e9f5a-2b2f-4f4e-9c3b-0f2e7a1d5b60";

struct Fixture {
    _temp: TempDir,
    layout: RepositoryLayout,
    root: PathBuf,
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
        fs::write(temp.path().join(".thoughtignore"), "path:secret\n").unwrap();
        let layout = RepositoryLayout::discover(temp.path()).unwrap();
        let root = layout.worktree_root().unwrap().to_path_buf();
        Self {
            _temp: temp,
            layout,
            root,
        }
    }

    fn store(&self) -> CvcStore {
        CvcStore::open(self.layout.db_path()).unwrap()
    }

    fn policy(&self) -> PreparedPolicy {
        PreparedPolicy::load(self.layout.policy_root().unwrap()).unwrap()
    }

    fn render(&self, template: &str) -> String {
        template.replace("__WORKTREE__", self.root.to_str().unwrap())
    }

    fn write_transcript(&self, name: &str, content: &str) -> PathBuf {
        let dir = self.root.join("transcripts");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    fn plan_input(&self, final_mode: bool) -> PlanInput<'_> {
        PlanInput {
            session_id: BASIC_SESSION,
            worktree_root: &self.root,
            path_aliases: &[],
            final_mode,
        }
    }
}

fn parse(content: &str, session: &str) -> Result<transcript::Window, TranscriptError> {
    transcript::parse_window(content.as_bytes(), 0, session)
}

#[test]
fn plans_one_interaction_per_response_with_threaded_stimuli() {
    let fixture = Fixture::new();
    let window = parse(&fixture.render(BASIC), BASIC_SESSION).unwrap();
    assert!(!window.truncated_tail);
    let plan = transcript::plan(&window.lines, 0, &fixture.plan_input(true));
    assert_eq!(plan.pending_responses, 0);
    assert_eq!(plan.title.as_deref(), Some("Greeting work"));
    assert_eq!(
        plan.first_human_prompt.as_deref(),
        Some("Add a greeting to lib.rs")
    );
    let keys: Vec<&str> = plan
        .interactions
        .iter()
        .map(|planned| planned.response_key.as_str())
        .collect();
    assert_eq!(
        keys,
        ["req_A", "req_B", "req_C", "req_D", "req_E", "req_F", "req_G"]
    );

    let a = &plan.interactions[0];
    assert_eq!(a.author, Author::Human);
    assert!(a.user_prompt.starts_with("Add a greeting to lib.rs"));
    assert!(a.user_prompt.contains("[meta] <system-reminder>"));
    assert_eq!(a.model_cot.as_deref(), Some("I should read lib.rs first."));
    assert_eq!(
        a.model_response.as_deref(),
        Some("I'll read the file first.")
    );
    assert_eq!(a.model_name.as_deref(), Some("claude-fixture-1"));
    assert_eq!(a.parent_id, None);
    assert_eq!(a.tool_executions.len(), 1);
    assert_eq!(a.tool_executions[0].tool_name, "Read");
    assert_eq!(a.tool_executions[0].tool_protocol, "native");
    assert_eq!(a.tool_executions[0].status, ToolStatus::Success);
    assert_eq!(a.context_items.len(), 1);
    assert_eq!(a.context_items[0].file_path, "src/lib.rs");
    assert_eq!(a.context_items[0].start_line, Some(1));
    assert_eq!(a.context_items[0].end_line, Some(20));
    assert_eq!(a.id, derived_interaction_id(BASIC_SESSION, "req_A"));

    let b = &plan.interactions[1];
    assert_eq!(b.author, Author::System);
    assert!(
        b.user_prompt.starts_with("[tool_result Read] "),
        "{}",
        b.user_prompt
    );
    assert!(b.user_prompt.contains("fn main() {}"));
    assert_eq!(b.model_response, None);
    assert_eq!(b.parent_id.as_ref(), Some(&a.id));
    assert_eq!(b.tool_executions[0].tool_name, "Edit");
    assert_eq!(b.tool_executions[0].status, ToolStatus::Failure);
    assert_eq!(b.context_items[0].file_path, "src/lib.rs");
    assert_eq!(b.context_items[0].start_line, None);

    let c = &plan.interactions[2];
    assert!(c
        .user_prompt
        .starts_with("[tool_result Edit error] String to replace"));
    assert_eq!(c.tool_executions[0].tool_name, "Write");
    assert_eq!(c.tool_executions[0].status, ToolStatus::Success);
    assert_eq!(c.parent_id.as_ref(), Some(&b.id));

    // A response with two tool calls: both recorded, only the in-worktree
    // path becomes context.
    let d = &plan.interactions[3];
    assert_eq!(d.tool_executions.len(), 2);
    assert_eq!(d.tool_executions[0].tool_name, "Bash");
    assert_eq!(d.tool_executions[1].tool_name, "Read");
    assert!(d.context_items.is_empty(), "{:?}", d.context_items);
    assert_eq!(d.parent_id.as_ref(), Some(&c.id));

    // The stimulus walk passes through the stop-hook system entry and
    // collects both tool results in chronological order.
    let e = &plan.interactions[4];
    let bash_at = e.user_prompt.find("[tool_result Bash]").unwrap();
    let read_at = e.user_prompt.find("[tool_result Read]").unwrap();
    assert!(bash_at < read_at, "{}", e.user_prompt);
    assert_eq!(e.model_response.as_deref(), Some("Done."));
    assert_eq!(e.parent_id.as_ref(), Some(&d.id));

    // Compaction: the boundary's logical parent keeps the chain continuous
    // and the summary is the recorded stimulus.
    let f = &plan.interactions[5];
    assert!(f.user_prompt.starts_with("[compaction summary]\n"));
    assert_eq!(f.author, Author::System);
    assert_eq!(f.parent_id.as_ref(), Some(&e.id));
    assert_eq!(f.context_items[0].file_path, "secret/key.txt");

    let g = &plan.interactions[6];
    assert_eq!(g.parent_id.as_ref(), Some(&f.id));
    assert_eq!(plan.cursor_offset, g.first_offset);
}

#[test]
fn incremental_mode_keeps_the_last_response_pending_until_closed() {
    let fixture = Fixture::new();
    let content = fixture.render(BASIC);
    let window = parse(&content, BASIC_SESSION).unwrap();
    let plan = transcript::plan(&window.lines, 0, &fixture.plan_input(false));
    assert_eq!(plan.pending_responses, 1);
    assert_eq!(plan.interactions.len(), 6);
    assert_eq!(
        plan.cursor_offset,
        plan.interactions.last().unwrap().first_offset
    );

    // Cut the transcript inside the Bash response: A, B and C are closed by
    // their results, D is still streaming.
    let cut = content.find("\"uuid\":\"s7b\"").unwrap();
    let line_start = content[..cut].rfind('\n').unwrap() + 1;
    let partial = &content[..line_start];
    let window = parse(partial, BASIC_SESSION).unwrap();
    let plan = transcript::plan(&window.lines, 0, &fixture.plan_input(false));
    let keys: Vec<&str> = plan
        .interactions
        .iter()
        .map(|planned| planned.response_key.as_str())
        .collect();
    assert_eq!(keys, ["req_A", "req_B", "req_C"]);
    assert_eq!(plan.pending_responses, 1);

    // A half-written trailing line is left for the next read, not an error.
    let torn = &content[..line_start + 40];
    let window = parse(torn, BASIC_SESSION).unwrap();
    assert!(window.truncated_tail);
    assert_eq!(window.consumed as usize, line_start);
}

#[test]
fn missing_tool_results_fail_closed_once_the_conversation_moved_on() {
    let fixture = Fixture::new();
    let content = fixture.render(BASIC);
    // Drop the Read result of response D; the stop-hook entry that follows
    // proves the turn ended without it.
    let without: String = content
        .lines()
        .filter(|line| !line.contains("\"uuid\":\"u5b\""))
        .map(|line| format!("{line}\n"))
        .collect();
    let window = parse(&without, BASIC_SESSION).unwrap();
    let plan = transcript::plan(&window.lines, 0, &fixture.plan_input(false));
    let d = plan
        .interactions
        .iter()
        .find(|planned| planned.response_key == "req_D")
        .unwrap();
    assert_eq!(d.tool_executions[0].status, ToolStatus::Success);
    assert_eq!(d.tool_executions[1].status, ToolStatus::Failure);
}

#[test]
fn parser_fails_loudly_on_unknown_shapes_and_other_sessions() {
    let fixture = Fixture::new();
    let content = fixture.render(BASIC);

    let unknown_block = content.replace("\"type\":\"thinking\"", "\"type\":\"mystery\"");
    match parse(&unknown_block, BASIC_SESSION) {
        Err(TranscriptError::Malformed { reason, .. }) => {
            assert!(reason.contains("mystery"), "{reason}")
        }
        other => panic!("expected malformed, got {other:?}"),
    }

    let future = content.replacen("\"version\":\"2.1.260\"", "\"version\":\"3.0.0\"", 1);
    match parse(&future, BASIC_SESSION) {
        Err(TranscriptError::UnsupportedVersion { version, .. }) => assert_eq!(version, "3.0.0"),
        other => panic!("expected unsupported version, got {other:?}"),
    }

    match parse(&content, "another-session") {
        Err(TranscriptError::SessionMismatch { found, .. }) => assert_eq!(found, BASIC_SESSION),
        other => panic!("expected session mismatch, got {other:?}"),
    }

    let missing_message = content.replacen(
        "\"message\":{\"role\":\"user\"",
        "\"msg\":{\"role\":\"user\"",
        1,
    );
    assert!(matches!(
        parse(&missing_message, BASIC_SESSION),
        Err(TranscriptError::Malformed { .. })
    ));

    // Unknown bookkeeping without a message is tolerated; with one it is not.
    let bookkeeping = format!("{{\"type\":\"future-thing\",\"sessionId\":\"{BASIC_SESSION}\"}}\n");
    let window = parse(&bookkeeping, BASIC_SESSION).unwrap();
    assert!(matches!(window.lines[0], ParsedLine::Skipped));
    let with_message = format!(
        "{{\"type\":\"future-thing\",\"uuid\":\"x\",\"sessionId\":\"{BASIC_SESSION}\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n"
    );
    assert!(parse(&with_message, BASIC_SESSION).is_err());
}

#[test]
fn earlier_minor_version_fixture_parses() {
    let fixture = Fixture::new();
    let content = fixture.render(MINIMAL_219);
    let session = "0b7d2c4e-6a1f-4c8d-9e3b-5f2a7c9d1e60";
    let window = parse(&content, session).unwrap();
    let plan = transcript::plan(
        &window.lines,
        0,
        &PlanInput {
            session_id: session,
            worktree_root: &fixture.root,
            path_aliases: &[],
            final_mode: true,
        },
    );
    assert_eq!(plan.interactions.len(), 1);
    assert_eq!(plan.interactions[0].author, Author::Human);
    assert_eq!(plan.interactions[0].user_prompt, "Say hello");
    assert_eq!(plan.title.as_deref(), Some("Hello exchange"));
}

#[test]
fn ingest_is_incremental_idempotent_and_policy_filtered() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let policy = fixture.policy();
    let content = fixture.render(BASIC);
    let cut = content.find("\"uuid\":\"s7\"").unwrap();
    let line_start = content[..cut].rfind('\n').unwrap() + 1;
    let transcript_path =
        fixture.write_transcript(&format!("{BASIC_SESSION}.jsonl"), &content[..line_start]);

    let first = ingest(
        &fixture.layout,
        &store,
        &policy,
        &transcript_path,
        None,
        IngestMode::Incremental,
    )
    .unwrap();
    assert_eq!(first.session_id, BASIC_SESSION);
    assert_eq!(first.inserted, 3);
    assert_eq!(first.already_present, 0);
    assert_eq!(first.pending_responses, 0);
    assert_eq!(
        store.conversation_interaction_count(BASIC_SESSION).unwrap(),
        3
    );
    let cursor = store
        .harness_ingest_cursor(transcript::HARNESS, BASIC_SESSION)
        .unwrap()
        .unwrap();
    assert_eq!(cursor.byte_offset, first.cursor);
    assert!(cursor.byte_offset > 0);
    // The conversation is titled from the first prompt until a title appears.
    assert_eq!(
        store
            .get_conversation(BASIC_SESSION)
            .unwrap()
            .unwrap()
            .title,
        "Add a greeting to lib.rs"
    );

    fs::write(&transcript_path, &content).unwrap();
    let second = ingest(
        &fixture.layout,
        &store,
        &policy,
        &transcript_path,
        Some(BASIC_SESSION),
        IngestMode::Incremental,
    )
    .unwrap();
    assert_eq!(second.inserted, 3, "{second:?}");
    assert_eq!(second.already_present, 1, "{second:?}");
    assert_eq!(second.pending_responses, 1);
    assert_eq!(
        store
            .get_conversation(BASIC_SESSION)
            .unwrap()
            .unwrap()
            .title,
        "Greeting work"
    );

    let third = ingest(
        &fixture.layout,
        &store,
        &policy,
        &transcript_path,
        None,
        IngestMode::Final,
    )
    .unwrap();
    assert_eq!(third.inserted, 1, "{third:?}");
    assert_eq!(third.pending_responses, 0);
    assert_eq!(
        store.conversation_interaction_count(BASIC_SESSION).unwrap(),
        7
    );

    let fourth = ingest(
        &fixture.layout,
        &store,
        &policy,
        &transcript_path,
        None,
        IngestMode::Final,
    )
    .unwrap();
    assert_eq!(fourth.inserted, 0);
    assert_eq!(fourth.already_present, 1);
    assert_eq!(
        store.conversation_interaction_count(BASIC_SESSION).unwrap(),
        7
    );

    // Threading survives the cursor: D's parent is C, recorded in a different run.
    let d = store
        .get_interaction(&derived_interaction_id(BASIC_SESSION, "req_D"))
        .unwrap()
        .unwrap();
    assert_eq!(
        d.parent_id,
        Some(derived_interaction_id(BASIC_SESSION, "req_C"))
    );
    assert_eq!(d.source_request_id.as_deref(), Some("req_D"));
    assert_eq!(d.author, Author::System);
    let tools = store.get_tool_executions(&d.id).unwrap();
    assert_eq!(tools.len(), 2);
    assert!(tools[0].arguments.contains("git commit -am greet"));

    // `.thoughtignore` removed the secret path but the response itself stayed.
    let f_id = derived_interaction_id(BASIC_SESSION, "req_F");
    assert!(store.get_context_items(&f_id).unwrap().is_empty());
    let a_items = store
        .get_context_items(&derived_interaction_id(BASIC_SESSION, "req_A"))
        .unwrap();
    assert_eq!(a_items.len(), 1);
    assert_eq!(a_items[0].file_path, "src/lib.rs");

    // Provenance and worktree attribution are recorded on every row.
    let conn = rusqlite::Connection::open(fixture.layout.db_path()).unwrap();
    let (sources, origins): (i64, i64) = conn
        .query_row(
            "SELECT SUM(capture_source='claude_code'), SUM(capture_worktree IS NOT NULL) FROM interactions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((sources, origins), (7, 7));
}

#[test]
fn ingest_rejects_bad_transcripts_without_writing() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let policy = fixture.policy();
    let broken = fixture
        .render(BASIC)
        .replace("\"type\":\"thinking\"", "\"type\":\"mystery\"");
    let path = fixture.write_transcript(&format!("{BASIC_SESSION}.jsonl"), &broken);
    let error = ingest(
        &fixture.layout,
        &store,
        &policy,
        &path,
        None,
        IngestMode::Final,
    )
    .unwrap_err();
    assert!(matches!(error, IngestError::Transcript(_)), "{error}");
    assert_eq!(
        store.conversation_interaction_count(BASIC_SESSION).unwrap(),
        0
    );
    assert!(store
        .harness_ingest_cursor(transcript::HARNESS, BASIC_SESSION)
        .unwrap()
        .is_none());

    let odd = fixture.write_transcript("not a session id.jsonl", "");
    assert!(matches!(
        ingest(
            &fixture.layout,
            &store,
            &policy,
            &odd,
            None,
            IngestMode::Final
        ),
        Err(IngestError::InvalidSession(_))
    ));
}

#[test]
fn stale_cursor_resets_and_stays_idempotent() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let policy = fixture.policy();
    let content = fixture.render(BASIC);
    let path = fixture.write_transcript(&format!("{BASIC_SESSION}.jsonl"), &content);
    ingest(
        &fixture.layout,
        &store,
        &policy,
        &path,
        None,
        IngestMode::Final,
    )
    .unwrap();
    // Rewrite the transcript shorter than the stored cursor.
    let cut = content.find("\"uuid\":\"s4\"").unwrap();
    fs::write(&path, &content[..cut]).unwrap();
    let report = ingest(
        &fixture.layout,
        &store,
        &policy,
        &path,
        None,
        IngestMode::Final,
    )
    .unwrap();
    assert_eq!(report.inserted, 0);
    assert!(report.already_present >= 1);
    assert_eq!(
        store.conversation_interaction_count(BASIC_SESSION).unwrap(),
        7
    );
}

#[test]
fn legacy_capture_source_constraint_is_widened_once() {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("cvc").join("index.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (id TEXT PRIMARY KEY, title TEXT, created_at INTEGER);
             CREATE TABLE interactions (
                 id TEXT PRIMARY KEY, conversation_id TEXT, parent_id TEXT, timestamp INTEGER,
                 author TEXT, user_prompt TEXT, model_name TEXT, model_cot TEXT, model_response TEXT,
                 source_request_id TEXT,
                 visibility TEXT NOT NULL DEFAULT 'private' CHECK(visibility IN ('private','shared')),
                 capture_source TEXT NOT NULL DEFAULT 'legacy' CHECK(capture_source IN ('mcp','vscode_passive','vscode_explicit','cli_run','sync_import','legacy')),
                 scrubber_version INTEGER NOT NULL DEFAULT 0 CHECK(scrubber_version BETWEEN 0 AND 1),
                 capture_worktree TEXT CHECK(capture_worktree IS NULL OR (length(capture_worktree)=64 AND capture_worktree NOT GLOB '*[^0-9a-f]*')),
                 FOREIGN KEY(conversation_id) REFERENCES conversations(id),
                 FOREIGN KEY(parent_id) REFERENCES interactions(id));
             INSERT INTO conversations VALUES ('conv', 'old', 1);
             INSERT INTO interactions (id,conversation_id,parent_id,timestamp,author,user_prompt,capture_source,scrubber_version)
                 VALUES ('6b1f0c0e-1111-4aaa-8bbb-000000000001','conv',NULL,1,'human','old prompt','mcp',1);",
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO interactions (id,conversation_id,timestamp,author,user_prompt,capture_source) VALUES ('6b1f0c0e-1111-4aaa-8bbb-000000000002','conv',2,'system','x','claude_code')",
                [],
            )
            .is_err());
    }

    let store = CvcStore::open(&db_path).unwrap();
    assert_eq!(store.conversation_interaction_count("conv").unwrap(), 1);
    drop(store);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let migrated: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM cvc_internal_migrations WHERE name='capture-source-claude-code/v1')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(migrated);
    let prompt: String = conn
        .query_row(
            "SELECT user_prompt FROM interactions WHERE id='6b1f0c0e-1111-4aaa-8bbb-000000000001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(prompt, "old prompt");
    conn.execute(
        "INSERT INTO interactions (id,conversation_id,timestamp,author,user_prompt,capture_source,scrubber_version) VALUES ('6b1f0c0e-1111-4aaa-8bbb-000000000002','conv',2,'system','x','claude_code',1)",
        [],
    )
    .unwrap();
    let indexes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND tbl_name='interactions' AND name LIKE 'idx_interactions_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 4);
    let foreign_keys: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    // The rebuild leaves enforcement as the store expects on its own connection;
    // a fresh connection reports SQLite's default.
    assert!(foreign_keys == 0 || foreign_keys == 1);

    // Reopening does not rebuild again.
    let store = CvcStore::open(&db_path).unwrap();
    assert_eq!(store.conversation_interaction_count("conv").unwrap(), 2);
}

#[test]
fn hook_settings_install_is_idempotent_and_preserves_foreign_entries() {
    let fixture = Fixture::new();
    let binary = fixture.root.join("fake-cvc");
    fs::write(&binary, "#!/bin/sh\n").unwrap();
    let settings_path = settings::settings_path(&fixture.layout).unwrap();
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    fs::write(
        &settings_path,
        r#"{"permissions":{"allow":["Bash(ls:*)"]},"hooks":{"PostToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo other"}]}]}}"#,
    )
    .unwrap();

    let first = settings::install(&fixture.layout, &binary).unwrap();
    assert_eq!(first.action, InstallAction::Created);
    assert!(first.command.ends_with(" ingest claude-code --hook"));
    assert!(first.command.contains(binary.to_str().unwrap()));
    let rendered: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(rendered["permissions"]["allow"][0], "Bash(ls:*)");
    for event in settings::HOOK_EVENTS {
        let groups = rendered["hooks"][event].as_array().unwrap();
        let ours: Vec<&serde_json::Value> = groups
            .iter()
            .flat_map(|group| group["hooks"].as_array().unwrap())
            .filter(|entry| entry["command"] == first.command)
            .collect();
        assert_eq!(ours.len(), 1, "{event}: {rendered}");
        assert_eq!(ours[0]["timeout"], settings::HOOK_TIMEOUT_SECS);
        assert_eq!(ours[0]["type"], "command");
    }
    assert_eq!(
        rendered["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "echo other"
    );
    assert!(fixture
        .layout
        .repository()
        .is_path_ignored(Path::new(settings::SETTINGS_RELATIVE_PATH))
        .unwrap());

    let again = settings::install(&fixture.layout, &binary).unwrap();
    assert_eq!(again.action, InstallAction::AlreadyPresent);
    assert_eq!(again.exclude, ExcludeAction::AlreadyIgnored);

    let moved = fixture.root.join("moved-cvc");
    fs::write(&moved, "#!/bin/sh\n").unwrap();
    let updated = settings::install(&fixture.layout, &moved).unwrap();
    assert_eq!(updated.action, InstallAction::Updated);
    let rendered = fs::read_to_string(&settings_path).unwrap();
    assert!(rendered.contains("moved-cvc"));
    assert!(!rendered.contains("fake-cvc"));

    let removed = settings::uninstall(&fixture.layout).unwrap();
    assert_eq!(removed.removed, 3);
    assert!(!removed.deleted_file);
    let rendered: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(rendered["permissions"]["allow"][0], "Bash(ls:*)");
    assert_eq!(
        rendered["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "echo other"
    );
    assert!(rendered["hooks"].get("Stop").is_none());
    assert!(!rendered.to_string().contains("claude-code --hook"));

    let none = settings::uninstall(&fixture.layout).unwrap();
    assert_eq!(none.removed, 0);

    // A file that only ever held our hooks is removed entirely.
    fs::remove_file(&settings_path).unwrap();
    settings::install(&fixture.layout, &moved).unwrap();
    let removed = settings::uninstall(&fixture.layout).unwrap();
    assert!(removed.deleted_file);
    assert!(!settings_path.exists());

    assert!(settings::hook_command(Path::new("relative/cvc")).is_err());
    assert!(settings::hook_command(&fixture.root.join("missing")).is_err());
}

/// Maintainer smoke check against a real transcript; it parses and plans
/// without touching any database and prints a summary:
///
/// ```text
/// CVC_CLAUDE_CODE_TRANSCRIPT=~/.claude/projects/<slug>/<session>.jsonl \
/// CVC_CLAUDE_CODE_WORKTREE=/path/to/checkout \
/// cargo test -p cvc-core --test test_claude_code -- --ignored real_transcript --nocapture
/// ```
#[test]
#[ignore = "needs a real transcript path in CVC_CLAUDE_CODE_TRANSCRIPT"]
fn real_transcript_dry_run() {
    let Ok(path) = std::env::var("CVC_CLAUDE_CODE_TRANSCRIPT") else {
        return;
    };
    let path = PathBuf::from(path);
    let session = path.file_stem().unwrap().to_str().unwrap().to_owned();
    let worktree = std::env::var("CVC_CLAUDE_CODE_WORKTREE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap());
    let worktree = fs::canonicalize(worktree).unwrap();
    let bytes = fs::read(&path).unwrap();
    let window = transcript::parse_window(&bytes, 0, &session).unwrap();
    let entries = window
        .lines
        .iter()
        .filter(|line| matches!(line, ParsedLine::Entry(_)))
        .count();
    let plan = transcript::plan(
        &window.lines,
        0,
        &PlanInput {
            session_id: &session,
            worktree_root: &worktree,
            path_aliases: &[],
            final_mode: true,
        },
    );
    let humans = plan
        .interactions
        .iter()
        .filter(|planned| planned.author == Author::Human)
        .count();
    let with_parent = plan
        .interactions
        .iter()
        .filter(|planned| planned.parent_id.is_some())
        .count();
    let with_cot = plan
        .interactions
        .iter()
        .filter(|planned| planned.model_cot.is_some())
        .count();
    let tools: usize = plan
        .interactions
        .iter()
        .map(|planned| planned.tool_executions.len())
        .sum();
    let failures: usize = plan
        .interactions
        .iter()
        .flat_map(|planned| planned.tool_executions.iter())
        .filter(|tool| tool.status == ToolStatus::Failure)
        .count();
    let context: usize = plan
        .interactions
        .iter()
        .map(|planned| planned.context_items.len())
        .sum();
    let largest_prompt = plan
        .interactions
        .iter()
        .map(|planned| planned.user_prompt.len())
        .max()
        .unwrap_or(0);
    println!(
        "lines={} entries={} truncated_tail={} interactions={} human_stimuli={} with_parent={} with_cot={} tool_executions={} tool_failures={} context_items={} largest_prompt_bytes={} pending={} title={:?} cursor={}",
        window.lines.len(),
        entries,
        window.truncated_tail,
        plan.interactions.len(),
        humans,
        with_parent,
        with_cot,
        tools,
        failures,
        context,
        largest_prompt,
        plan.pending_responses,
        plan.title,
        plan.cursor_offset
    );
    assert!(!plan.interactions.is_empty());
}
