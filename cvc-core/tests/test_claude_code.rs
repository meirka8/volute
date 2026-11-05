//! Claude Code adapter: transcript parsing, planning, idempotent ingestion,
//! the capture_source migration, and hook settings installation.
//!
//! Fixtures are synthetic transcripts shaped like the Claude Code versions
//! named by their directory; `__WORKTREE__` is substituted with the fixture
//! repository's canonical root at test time.
use chrono::Utc;
use cvc_core::claude_code::settings::{self, ExcludeAction, InstallAction};
use cvc_core::claude_code::transcript::{
    self, derived_interaction_id, ParsedLine, PlanInput, PlannedInteraction, TranscriptError,
};
use cvc_core::claude_code::{ingest, IngestError, IngestMode};
use cvc_core::db::CvcStore;
use cvc_core::models::{
    Author, Conversation, FutureSharePolicy, Interaction, InteractionId, ToolStatus,
};
use cvc_core::privacy::{McpCapture, PreparedPolicy};
use cvc_core::repository::RepositoryLayout;
use cvc_core::stimulus::{self, PartKind, Reading};
use git2::Repository;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

const BASIC: &str = include_str!("fixtures/claude-code/v2.1.260/basic.jsonl");
const MINIMAL_219: &str = include_str!("fixtures/claude-code/v2.1.219/minimal.jsonl");
const TITLED_289: &str = include_str!("fixtures/claude-code/v2.1.289/titled.jsonl");
const TITLED_289_SESSION: &str = "9d4b2e7f-6a1c-4e3b-8f5d-2c7a9b1e0d43";
const CACHED_300: &str = include_str!("fixtures/claude-code/v2.1.300/cached-requests.jsonl");
const CACHED_300_SESSION: &str = "4c8e1f2a-9b7d-4e6f-a3c5-1d2e3f4a5b6c";
/// Synthetic (public issue 134): provider-side tool calls and their result
/// blocks, shaped as the Claude API reference documents them. No real session
/// was recorded; every third-party text in it carries the token `THIRD-PARTY`.
const SERVER_300: &str = include_str!("fixtures/claude-code/v2.1.300/server-tools.jsonl");
const SERVER_300_SESSION: &str = "2d7f4b9e-3a5c-4c1e-8b6d-9e0f1a2b3c4d";
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
            subagent: false,
        }
    }
}

fn parse(content: &str, session: &str) -> Result<transcript::Window, TranscriptError> {
    transcript::parse_window(content.as_bytes(), 0, session, false)
}

/// The stimulus of `planned` as it is stored under the built-in scrubber.
fn prompt(planned: &PlannedInteraction) -> String {
    planned.user_prompt(&PreparedPolicy::built_ins_only())
}

/// A rendered stimulus read back, as `(kind, tool name, is_error, content)`
/// per part. The stimulus must verify.
fn parts(planned: &PlannedInteraction) -> Vec<(PartKind, Option<String>, bool, String)> {
    let text = prompt(planned);
    match stimulus::read(&planned.id, &text) {
        Reading::Verified(verified) => verified
            .parts
            .iter()
            .map(|part| {
                (
                    part.kind,
                    part.name.map(str::to_owned),
                    part.is_error,
                    part.content_of(&text).to_owned(),
                )
            })
            .collect(),
        other => panic!("stimulus did not verify: {other:?}\n{text}"),
    }
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
    let a_parts = parts(a);
    assert_eq!(a_parts.len(), 2, "{a_parts:?}");
    assert_eq!(
        a_parts[0],
        (
            PartKind::Prompt,
            None,
            false,
            "Add a greeting to lib.rs".to_owned()
        )
    );
    assert_eq!(a_parts[1].0, PartKind::Meta);
    assert!(a_parts[1].3.starts_with("<system-reminder>"));
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
    assert_eq!(
        parts(b),
        vec![(
            PartKind::ToolResult,
            Some("Read".to_owned()),
            false,
            "     1\tfn main() {}".to_owned()
        )]
    );
    assert_eq!(b.model_response, None);
    assert_eq!(b.parent_id.as_ref(), Some(&a.id));
    assert_eq!(b.tool_executions[0].tool_name, "Edit");
    assert_eq!(b.tool_executions[0].status, ToolStatus::Failure);
    assert_eq!(b.context_items[0].file_path, "src/lib.rs");
    assert_eq!(b.context_items[0].start_line, None);

    let c = &plan.interactions[2];
    assert_eq!(
        parts(c),
        vec![(
            PartKind::ToolResult,
            Some("Edit".to_owned()),
            true,
            "String to replace not found in file.".to_owned()
        )]
    );
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
    let e_parts = parts(e);
    let names: Vec<_> = e_parts.iter().map(|part| part.1.as_deref()).collect();
    assert_eq!(names, [Some("Bash"), Some("Read")], "{e_parts:?}");
    assert!(e_parts[0].3.starts_with("[main 1234abc] greet\n"));
    assert_eq!(e_parts[1].3, "127.0.0.1 localhost");
    assert_eq!(e.model_response.as_deref(), Some("Done."));
    assert_eq!(e.parent_id.as_ref(), Some(&d.id));

    // Compaction: the boundary's logical parent keeps the chain continuous
    // and the summary is the recorded stimulus.
    let f = &plan.interactions[5];
    let f_parts = parts(f);
    assert_eq!(f_parts.len(), 1, "{f_parts:?}");
    assert_eq!(f_parts[0].0, PartKind::CompactionSummary);
    assert!(f_parts[0].3.starts_with("This session is being continued"));
    assert_eq!(f.author, Author::System);
    assert_eq!(f.parent_id.as_ref(), Some(&e.id));
    assert_eq!(f.context_items[0].file_path, "secret/key.txt");

    let g = &plan.interactions[6];
    assert_eq!(g.parent_id.as_ref(), Some(&f.id));
    assert_eq!(plan.cursor_offset, g.first_offset);
}

#[test]
fn a_subagent_transcript_records_its_user_text_as_tasks_and_nothing_else_changes() {
    let fixture = Fixture::new();
    let content = fixture.render(BASIC);
    let lines = transcript::parse_window(content.as_bytes(), 0, BASIC_SESSION, true)
        .unwrap()
        .lines;
    let person = transcript::plan(&lines, 0, &fixture.plan_input(true));
    let agent = transcript::plan(
        &lines,
        0,
        &PlanInput {
            subagent: true,
            ..fixture.plan_input(true)
        },
    );
    assert_eq!(agent.interactions.len(), person.interactions.len());

    // Only the kind of the typed text differs: a delegated task, not a person.
    let task = parts(&agent.interactions[0]);
    assert_eq!(
        task[0],
        (
            PartKind::Task,
            None,
            false,
            "Add a greeting to lib.rs".to_owned()
        )
    );
    assert_eq!(task[1].0, PartKind::Meta);
    assert_eq!(agent.interactions[0].author, Author::Human);
    assert_eq!(parts(&person.interactions[0])[0].0, PartKind::Prompt);
    for index in 1..person.interactions.len() {
        assert_eq!(
            parts(&agent.interactions[index]),
            parts(&person.interactions[index]),
            "response {index}"
        );
    }
    assert_eq!(
        parts(&agent.interactions[5])[0].0,
        PartKind::CompactionSummary
    );
}

#[test]
fn the_planner_hands_over_the_model_text_arguments_and_title_raw() {
    // Scrubbing and bounding belong to the ingest step, which has the policy:
    // the planner cuts nothing, so nothing is cut before it is scrubbed.
    let fixture = Fixture::new();
    let long = "r".repeat(300 * 1024);
    let content = "w".repeat(40_000);
    let title = "t".repeat(500);
    let lines = [
        format!(
            r#"{{"parentUuid":null,"isSidechain":false,"type":"user","message":{{"role":"user","content":"Go"}},"uuid":"u1","timestamp":"2026-09-10T10:00:00.000Z","sessionId":"{BASIC_SESSION}","version":"2.1.260","cwd":"x"}}"#
        ),
        format!(
            r#"{{"parentUuid":"u1","isSidechain":false,"requestId":"req_A","type":"assistant","message":{{"model":"m","id":"a","type":"message","role":"assistant","content":[{{"type":"thinking","thinking":"{long}","signature":"s"}},{{"type":"text","text":"{long}"}},{{"type":"tool_use","id":"toolu_1","name":"Write","input":{{"file_path":"/opt/x","content":"{content}"}}}}],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"s1","timestamp":"2026-09-10T10:00:01.000Z","sessionId":"{BASIC_SESSION}","version":"2.1.260","cwd":"x"}}"#
        ),
        format!(r#"{{"type":"ai-title","aiTitle":"{title}","sessionId":"{BASIC_SESSION}"}}"#),
    ]
    .join("\n")
        + "\n";
    let window = parse(&lines, BASIC_SESSION).unwrap();
    let plan = transcript::plan(&window.lines, 0, &fixture.plan_input(true));
    let planned = &plan.interactions[0];
    assert_eq!(planned.model_response.as_deref(), Some(long.as_str()));
    assert_eq!(planned.model_cot.as_deref(), Some(long.as_str()));
    let arguments: serde_json::Value =
        serde_json::from_str(&planned.tool_executions[0].arguments).unwrap();
    assert_eq!(arguments["content"], content.as_str());
    assert_eq!(plan.title.as_deref(), Some(title.as_str()));
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

    // A block whose type alone is unfamiliar is kept as a placeholder naming
    // the type, never its content, and reported; the transcript still parses.
    let baseline = transcript::plan(
        &parse(&content, BASIC_SESSION).unwrap().lines,
        0,
        &fixture.plan_input(true),
    );
    let unknown_block = content.replace("\"type\":\"thinking\"", "\"type\":\"mystery\"");
    let window = parse(&unknown_block, BASIC_SESSION).unwrap();
    let plan = transcript::plan(&window.lines, 0, &fixture.plan_input(true));
    assert_eq!(plan.interactions.len(), baseline.interactions.len());
    let first = &plan.interactions[0];
    assert_eq!(first.unrecorded_blocks, vec!["mystery".to_owned()]);
    let response = first.model_response.as_deref().unwrap_or_default();
    assert!(response.contains("[mystery]"), "{response}");
    assert!(
        !response.contains("read lib.rs"),
        "block content must not be recorded: {response}"
    );
    assert!(first.model_cot.is_none());
    assert!(plan.interactions[1..]
        .iter()
        .all(|planned| planned.unrecorded_blocks.is_empty()));

    // A block without a type is an unknown shape and still fails.
    let untyped_block = content.replacen("\"type\":\"thinking\"", "\"kind\":\"thinking\"", 1);
    match parse(&untyped_block, BASIC_SESSION) {
        Err(TranscriptError::Malformed { reason, .. }) => {
            assert!(reason.contains("no type"), "{reason}")
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
            subagent: false,
        },
    );
    assert_eq!(plan.interactions.len(), 1);
    assert_eq!(plan.interactions[0].author, Author::Human);
    assert_eq!(
        parts(&plan.interactions[0]),
        vec![(PartKind::Prompt, None, false, "Say hello".to_owned())]
    );
    assert_eq!(plan.title.as_deref(), Some("Hello exchange"));
}

/// Public issue 101: Claude Code 2.1.289 records its generated session title
/// as an `ai-title` entry (and adds `permission-mode` bookkeeping). The
/// generated title names the conversation, and a title the user set outranks
/// it wherever the two appear.
#[test]
fn generated_titles_and_bookkeeping_entries_of_2_1_289_are_understood() {
    let fixture = Fixture::new();
    let content = fixture.render(TITLED_289);
    let window = parse(&content, TITLED_289_SESSION).unwrap();
    let plan = transcript::plan(&window.lines, 0, &fixture.plan_input(true));
    assert_eq!(plan.title.as_deref(), Some("Roadmap bug sweep"));
    assert_eq!(plan.pending_responses, 0);
    assert_eq!(plan.interactions.len(), 2);
    // The quiet commit's hash is found in the hook's line and at the start of
    // the `git log --oneline` lines (public issue 102).
    assert_eq!(
        plan.interactions[0].commit_candidates,
        vec![
            "0123456789ab".to_string(),
            "0123456".to_string(),
            "fedcba9".to_string()
        ]
    );

    let custom = format!(
        "{{\"type\":\"custom-title\",\"customTitle\":\"My own name\",\"sessionId\":\"{TITLED_289_SESSION}\"}}\n"
    );
    // Set after the generated title...
    let renamed_later = format!("{content}{custom}");
    let plan = transcript::plan(
        &parse(&renamed_later, TITLED_289_SESSION).unwrap().lines,
        0,
        &fixture.plan_input(true),
    );
    assert_eq!(plan.title.as_deref(), Some("My own name"));
    // ...or before it, with generated titles still arriving afterwards.
    let renamed_first = format!("{custom}{content}");
    let plan = transcript::plan(
        &parse(&renamed_first, TITLED_289_SESSION).unwrap().lines,
        0,
        &fixture.plan_input(true),
    );
    assert_eq!(plan.title.as_deref(), Some("My own name"));
}

/// Public issue 110: Claude Code 2.1.300 caches request messages as
/// chain-less `api-request-blob` entries that carry a `message`. An entry
/// without a uuid is outside the conversation chain and is skipped whatever
/// it carries; a chained entry of an unknown type with a message still fails.
#[test]
fn chainless_cache_entries_of_2_1_300_are_skipped_and_never_captured() {
    let fixture = Fixture::new();
    let content = fixture.render(CACHED_300);
    let window = parse(&content, CACHED_300_SESSION).unwrap();
    let plan = transcript::plan(&window.lines, 0, &fixture.plan_input(true));
    assert_eq!(plan.title.as_deref(), Some("Version flag"));
    assert_eq!(plan.pending_responses, 0);
    assert_eq!(plan.interactions.len(), 2);
    let captured: String = plan
        .interactions
        .iter()
        .map(|planned| {
            format!(
                "{} {} {}",
                prompt(planned),
                planned.model_response.clone().unwrap_or_default(),
                planned.model_cot.clone().unwrap_or_default()
            )
        })
        .collect();
    assert!(!captured.contains("CACHED COPY"), "{captured}");
    assert!(captured.contains("Add a --version flag"), "{captured}");

    // The same blob with a uuid would be part of the chain: refused.
    let chained = content.replacen(
        "\"type\":\"api-request-blob\"",
        "\"type\":\"api-request-blob\",\"uuid\":\"x9\"",
        1,
    );
    match parse(&chained, CACHED_300_SESSION) {
        Err(TranscriptError::Malformed { reason, .. }) => {
            assert!(reason.contains("carries a message"), "{reason}")
        }
        other => panic!("expected malformed, got {other:?}"),
    }
}

/// The calls of a capture as `(name, protocol, status)`, in recorded order.
fn call_summary(calls: &[cvc_core::models::ToolExecution]) -> Vec<(&str, &str, ToolStatus)> {
    calls
        .iter()
        .map(|call| {
            (
                call.tool_name.as_str(),
                call.tool_protocol.as_str(),
                call.status.clone(),
            )
        })
        .collect()
}

/// What `SERVER_300`'s first response records, call by call.
fn expected_server_calls() -> Vec<(&'static str, &'static str, ToolStatus)> {
    vec![
        // A list of hits.
        ("server:web_search", "server", ToolStatus::Success),
        // `max_uses_exceeded`, an error object in `content`.
        ("server:web_search", "server", ToolStatus::Failure),
        ("server:web_fetch", "server", ToolStatus::Success),
        // Exit code 0, then 2.
        ("server:bash_code_execution", "server", ToolStatus::Success),
        ("server:bash_code_execution", "server", ToolStatus::Failure),
        (
            "server:text_editor_code_execution",
            "server",
            ToolStatus::Success,
        ),
        // No result block in the response: never finished.
        ("server:web_search", "server", ToolStatus::Failure),
        // An ordinary client tool beside them, unchanged.
        ("Read", "native", ToolStatus::Success),
    ]
}

/// Public issue 134: a provider-side call (`server_tool_use`) never gets a
/// user-side `tool_result`, so its status was always `failure`. Its outcome is
/// the result block the API delivers inside the same assistant message.
#[test]
fn server_side_calls_take_their_status_from_the_result_block_of_their_response() {
    let fixture = Fixture::new();
    let window = parse(&fixture.render(SERVER_300), SERVER_300_SESSION).unwrap();
    let plan = transcript::plan(
        &window.lines,
        0,
        &PlanInput {
            session_id: SERVER_300_SESSION,
            worktree_root: &fixture.root,
            path_aliases: &[],
            final_mode: true,
            subagent: false,
        },
    );
    assert_eq!(plan.title.as_deref(), Some("Server tool outcomes"));
    assert_eq!(plan.pending_responses, 0);
    assert_eq!(plan.interactions.len(), 2);

    let a = &plan.interactions[0];
    assert_eq!(call_summary(&a.tool_executions), expected_server_calls());
    assert_eq!(
        a.tool_executions[0].arguments,
        r#"{"query":"fixture query"}"#
    );
    // Each result block adds its placeholder, in order, and nothing else.
    assert_eq!(
        a.model_response.as_deref(),
        Some(
            "Searching and fetching.\n\n[web_search_tool_result]\n\n[web_search_tool_result]\n\n\
             [web_fetch_tool_result]\n\n[bash_code_execution_tool_result]\n\n\
             [bash_code_execution_tool_result]\n\n[text_editor_code_execution_tool_result]"
        )
    );
    assert!(a.unrecorded_blocks.is_empty(), "{:?}", a.unrecorded_blocks);
    // The client Read still becomes file context; the provider's calls do not.
    assert_eq!(a.context_items.len(), 1);
    assert_eq!(a.context_items[0].file_path, "src/lib.rs");

    let b = &plan.interactions[1];
    assert_eq!(
        parts(b),
        vec![(
            PartKind::ToolResult,
            Some("Read".to_owned()),
            false,
            "     1\tfn main() {}".to_owned()
        )]
    );
    assert_eq!(
        call_summary(&b.tool_executions),
        vec![("server:web_search", "server", ToolStatus::Success)]
    );
    assert_eq!(
        b.model_response.as_deref(),
        Some("Summary of the lookups.\n\n[web_search_tool_result]")
    );
    assert!(b.unrecorded_blocks.is_empty());

    // What the provider fetched for the model is third-party text and is not
    // recorded anywhere: only the placeholders above reach a capture.
    let captured = format!("{plan:?}");
    assert!(!captured.contains("THIRD-PARTY"), "{captured}");
    assert!(!captured.contains("example.invalid/a"), "{captured}");
}

#[test]
fn ingest_records_server_side_outcomes_and_waits_for_the_response_to_end() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let policy = fixture.policy();
    let content = fixture.render(SERVER_300);
    let path = fixture.write_transcript(&format!("{SERVER_300_SESSION}.jsonl"), &content);

    // A hook run before the client tool's result is written finds the response
    // with its provider-side calls still open, and records nothing yet.
    let cut = content.find("\"uuid\":\"u2\"").unwrap();
    let line_start = content[..cut].rfind('\n').unwrap() + 1;
    fs::write(&path, &content[..line_start]).unwrap();
    let early = ingest(
        &fixture.layout,
        &store,
        &policy,
        &path,
        None,
        IngestMode::Incremental,
    )
    .unwrap();
    assert_eq!(
        (early.inserted, early.pending_responses),
        (0, 1),
        "{early:?}"
    );

    fs::write(&path, &content).unwrap();
    let report = ingest(
        &fixture.layout,
        &store,
        &policy,
        &path,
        None,
        IngestMode::Final,
    )
    .unwrap();
    assert_eq!(report.inserted, 2, "{report:?}");
    assert_eq!(report.unrecorded_blocks, 0, "{report:?}");
    assert!(report.unrecorded_block_types.is_empty());

    let a_id = derived_interaction_id(SERVER_300_SESSION, "req_A");
    let calls = store.get_tool_executions(&a_id).unwrap();
    assert_eq!(call_summary(&calls), expected_server_calls());
    let b_id = derived_interaction_id(SERVER_300_SESSION, "req_B");
    assert_eq!(
        call_summary(&store.get_tool_executions(&b_id).unwrap()),
        vec![("server:web_search", "server", ToolStatus::Success)]
    );

    // Third-party text never reaches the store, in prompts, responses or
    // arguments; the placeholders do.
    let conn = rusqlite::Connection::open(fixture.layout.db_path()).unwrap();
    let leaked: i64 = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM interactions WHERE user_prompt LIKE '%THIRD-PARTY%' \
             OR model_response LIKE '%THIRD-PARTY%' OR model_cot LIKE '%THIRD-PARTY%') \
             + (SELECT COUNT(*) FROM tool_executions WHERE arguments LIKE '%THIRD-PARTY%')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leaked, 0);
    let stored = store.get_interaction(&a_id).unwrap().unwrap();
    assert!(stored
        .model_response
        .unwrap()
        .contains("[web_fetch_tool_result]"));

    // Rows are never replaced: a second pass changes nothing.
    let again = ingest(
        &fixture.layout,
        &store,
        &policy,
        &path,
        None,
        IngestMode::Final,
    )
    .unwrap();
    assert_eq!(again.inserted, 0, "{again:?}");
    assert_eq!(
        call_summary(&store.get_tool_executions(&a_id).unwrap()),
        expected_server_calls()
    );
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
fn future_sharing_extends_to_later_ingests_of_the_session_but_not_to_mcp_thoughts_in_it() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let policy = fixture.policy();
    let destination = "7".repeat(64);
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
    assert_eq!(first.inserted, 3, "{first:?}");
    let title = || {
        store
            .get_conversation(BASIC_SESSION)
            .unwrap()
            .unwrap()
            .title
    };
    assert_eq!(title(), "Add a greeting to lib.rs");

    // The session is shared with its future thoughts, as `cvc share <session>
    // --future` records it.
    let snapshot = store.share_snapshot(BASIC_SESSION).unwrap();
    store
        .share_exact_snapshot(
            BASIC_SESSION,
            &destination,
            &snapshot,
            FutureSharePolicy::Shared,
        )
        .unwrap();

    // A model in that session calls `commit_thought` naming its own session id,
    // which it can read from its transcript's path.
    let through_mcp = InteractionId::new();
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: BASIC_SESSION.into(),
                title: format!("Session {BASIC_SESSION}"),
                created_at: Utc::now(),
            },
            Interaction {
                id: through_mcp.clone(),
                conversation_id: BASIC_SESSION.into(),
                parent_id: None,
                timestamp: Utc::now(),
                author: Author::Agent,
                user_prompt: "recorded through MCP".into(),
                model_name: Some("agent".into()),
                model_cot: Some("reasoning".into()),
                model_response: None,
                source_request_id: None,
            },
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    // It leaves the session's title as it was.
    assert_eq!(title(), "Add a greeting to lib.rs");

    // The rest of the transcript, ingested later, joins the share as it is
    // recorded; the MCP thought does not.
    fs::write(&transcript_path, &content).unwrap();
    let rest = ingest(
        &fixture.layout,
        &store,
        &policy,
        &transcript_path,
        None,
        IngestMode::Final,
    )
    .unwrap();
    assert_eq!(rest.inserted, 4, "{rest:?}");
    assert_eq!(
        store.conversation_interaction_count(BASIC_SESSION).unwrap(),
        8
    );
    let ingested: HashSet<InteractionId> = [
        "req_A", "req_B", "req_C", "req_D", "req_E", "req_F", "req_G",
    ]
    .into_iter()
    .map(|key| derived_interaction_id(BASIC_SESSION, key))
    .collect();
    let shared: HashSet<InteractionId> = store
        .projection_interaction_ids(&destination)
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(shared, ingested);
    assert!(!shared.contains(&through_mcp));
    // Ingestion still renames the session once its own title appears.
    assert_eq!(title(), "Greeting work");
}

#[test]
fn fallback_marker_adds_nothing_to_its_response() {
    let fixture = Fixture::new();
    let content = fixture.render(BASIC);
    let baseline = transcript::plan(
        &parse(&content, BASIC_SESSION).unwrap().lines,
        0,
        &fixture.plan_input(true),
    );
    // Claude Code records a model switch as an assistant entry of the same
    // request whose only block is a `fallback` marker.
    let marker = fixture.render(&format!(
        "{{\"parentUuid\":\"a1\",\"isSidechain\":false,\"message\":{{\"model\":\"claude-fixture-1\",\"id\":\"msg_A0\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"fallback\",\"from\":{{\"model\":\"claude-fixture-0\"}},\"to\":{{\"model\":\"claude-fixture-1\"}}}}],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}},\"requestId\":\"req_A\",\"type\":\"assistant\",\"uuid\":\"s0\",\"timestamp\":\"2026-09-10T10:00:00.900Z\",\"userType\":\"external\",\"entrypoint\":\"cli\",\"cwd\":\"__WORKTREE__\",\"sessionId\":\"{BASIC_SESSION}\",\"version\":\"2.1.260\",\"gitBranch\":\"main\"}}\n"
    ));
    let first_response = content.find("\"uuid\":\"s1\"").unwrap();
    let line_start = content[..first_response]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let with_marker = format!(
        "{}{}{}",
        &content[..line_start],
        marker,
        &content[line_start..]
    );

    let window = parse(&with_marker, BASIC_SESSION).unwrap();
    let plan = transcript::plan(&window.lines, 0, &fixture.plan_input(true));
    assert_eq!(plan.interactions.len(), baseline.interactions.len());
    let (marked, expected) = (&plan.interactions[0], &baseline.interactions[0]);
    assert_eq!(marked.id, expected.id);
    assert_eq!(marked.model_response, expected.model_response);
    assert_eq!(marked.model_cot, expected.model_cot);
    assert_eq!(marked.tool_executions.len(), expected.tool_executions.len());
    assert_eq!(marked.model_name.as_deref(), Some("claude-fixture-1"));
    assert!(marked.unrecorded_blocks.is_empty());
}

#[test]
fn ingest_keeps_placeholders_and_reports_unrecorded_blocks() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let policy = fixture.policy();
    let content = fixture
        .render(BASIC)
        .replace("\"type\":\"thinking\"", "\"type\":\"mystery\"");
    let path = fixture.write_transcript(&format!("{BASIC_SESSION}.jsonl"), &content);
    let report = ingest(
        &fixture.layout,
        &store,
        &policy,
        &path,
        None,
        IngestMode::Final,
    )
    .unwrap();
    assert!(report.inserted > 0);
    assert_eq!(report.unrecorded_blocks, 1);
    assert_eq!(report.unrecorded_block_types, vec!["mystery".to_owned()]);

    let conn = rusqlite::Connection::open(fixture.layout.db_path()).unwrap();
    let response: String = conn
        .query_row(
            "SELECT model_response FROM interactions WHERE conversation_id = ?1 \
             ORDER BY timestamp ASC LIMIT 1",
            [BASIC_SESSION],
            |row| row.get(0),
        )
        .unwrap();
    assert!(response.contains("[mystery]"), "{response}");
    assert!(!response.contains("read lib.rs"), "{response}");

    // Thoughts already recorded are not reported again.
    let again = ingest(
        &fixture.layout,
        &store,
        &policy,
        &path,
        None,
        IngestMode::Final,
    )
    .unwrap();
    assert_eq!(again.inserted, 0);
    assert_eq!(again.unrecorded_blocks, 0);
    assert!(again.unrecorded_block_types.is_empty());
}

#[test]
fn ingest_rejects_bad_transcripts_without_writing() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let policy = fixture.policy();
    let broken = fixture
        .render(BASIC)
        .replace("\"type\":\"thinking\"", "\"kind\":\"thinking\"");
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

/// A stand-in for the `cvc` binary: a hook needs an existing, executable file.
fn write_fake_binary(path: &Path) {
    fs::write(path, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn hook_settings_install_is_idempotent_and_preserves_foreign_entries() {
    let fixture = Fixture::new();
    let binary = fixture.root.join("fake-cvc");
    write_fake_binary(&binary);
    let settings_path = settings::settings_path(&fixture.layout).unwrap();
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    fs::write(
        &settings_path,
        r#"{"permissions":{"allow":["Bash(ls:*)"]},"hooks":{"PostToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo other"}]}]}}"#,
    )
    .unwrap();

    let first = settings::install(&fixture.layout, &binary).unwrap();
    assert_eq!(first.action, InstallAction::Created);
    // A first install on a fresh repository adds the entry, on any machine.
    assert_eq!(first.exclude, ExcludeAction::AddedToInfoExclude);
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
    write_fake_binary(&moved);
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

// ---- the settings file and the ignore rules of the checkout ----
//
// `install` and `create` keep `.claude/settings.local.json` out of commits with
// an entry in the repository's `info/exclude`, which they add whenever that
// exact line is missing, whatever else ignores the path. Git reads the
// `.gitignore` files of the checkout in preference to that file, so a rule such
// as `!/.claude/settings.local.json` leaves the settings file visible to Git
// although the entry is there. What counts as ignored is what `git` itself says
// once the entry is in (`git check-ignore`: exit status 0 ignored, 1 not
// ignored, anything else no verdict): libgit2's `is_path_ignored` answers
// "ignored" for a plain `!` rule, so it is not the reference here.
//
// These tests share one process, so they leave the environment alone, and none
// of them depends on the machine: `info/exclude` outranks the user's global
// excludes and a `.gitignore` rule outranks both, so a global excludes file
// that lists this very path can neither hide the entry nor undo a rule. The
// cases that need a global excludes file point `core.excludesFile` of the
// fixture repository at one.

const EXCLUDE_ENTRY: &str = "/.claude/settings.local.json";
const NOT_IGNORED_MESSAGE: &str = "`.claude/settings.local.json` would not be ignored by Git in this worktree: a `.gitignore` rule overrides the entry CVC adds to `.git/info/exclude`; CVC writes a machine-specific path there and will not create a file that a commit could carry (fix the rule, then run `cvc harness install claude-code` here)";
const IGNORE_UNKNOWN_MESSAGE: &str = "could not ask Git whether `.claude/settings.local.json` is ignored in this worktree; CVC writes a machine-specific path there and creates nothing it cannot confirm Git ignores (run `cvc harness install claude-code` here)";
/// A root `.gitignore` rule that leaves the settings file visible to Git.
const OVERRIDING_RULE: &str = "!/.claude/settings.local.json\n";

/// A `git` command for the checkout at `root`. The variables a surrounding Git
/// command exports to point at a repository, to change how pathspecs are read,
/// or to stop Git from fetching a missing object, are removed, so that the
/// answer is about `root` whatever started the test run.
fn git_in(root: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(root);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
        "GIT_NO_LAZY_FETCH",
    ] {
        command.env_remove(name);
    }
    command
}

/// What Git says about the settings file in the checkout at `root`, by the exit
/// status of `git check-ignore -q`: `Some(true)` ignored (0), `Some(false)` not
/// ignored (1), `None` no verdict (any other status: Git refuses to judge a path
/// inside a submodule, for one).
fn git_ignore_verdict(root: &Path) -> Option<bool> {
    let status = git_in(root)
        .args(["check-ignore", "-q", "--", settings::SETTINGS_RELATIVE_PATH])
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Whether Git ignores the settings file in the checkout at `root`, which must
/// be one where Git gives a verdict.
fn git_ignores_settings(root: &Path) -> bool {
    git_ignore_verdict(root).expect("git check-ignore gave no verdict")
}

/// `git status --porcelain -uall` of the checkout at `root`, one entry per line.
fn git_status_lines(root: &Path) -> Vec<String> {
    let output = git_in(root)
        .args(["status", "--porcelain", "-uall"])
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// How many times the repository's `info/exclude` lists the settings file.
fn exclude_entries(layout: &RepositoryLayout) -> usize {
    fs::read_to_string(layout.common_git_dir().join("info/exclude"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.trim() == EXCLUDE_ENTRY)
        .count()
}

/// A stand-in `cvc` inside the fixture's checkout.
fn fake_cvc(fixture: &Fixture) -> PathBuf {
    let binary = fixture.root.join("fake-cvc");
    write_fake_binary(&binary);
    binary
}

/// A fixture whose root `.gitignore` holds `rules`.
fn fixture_with_gitignore(rules: &str) -> Fixture {
    let fixture = Fixture::new();
    fs::write(fixture.root.join(".gitignore"), rules).unwrap();
    fixture
}

/// Puts the settings file in the index, as a branch that commits it would.
fn track_settings_file(fixture: &Fixture) {
    let mut index = fixture.layout.repository().index().unwrap();
    index
        .add_path(Path::new(settings::SETTINGS_RELATIVE_PATH))
        .unwrap();
    index.write().unwrap();
}

/// Puts a submodule at `.claude` in the index, as a branch that commits one
/// would. The commit it names does not have to exist here. Git gives no verdict
/// on a path inside a submodule.
fn track_claude_as_a_submodule(fixture: &Fixture) {
    let mut index = fixture.layout.repository().index().unwrap();
    index
        .add(&git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: 0o160000,
            uid: 0,
            gid: 0,
            file_size: 0,
            id: git2::Oid::from_str("1111111111111111111111111111111111111111").unwrap(),
            flags: 0,
            flags_extended: 0,
            path: b".claude".to_vec(),
        })
        .unwrap();
    index.write().unwrap();
}

/// A fixture whose repository reads `rules` as its global excludes
/// (`core.excludesFile` in the repository's own configuration, which `git`
/// honours like the user's). The file lives outside the checkout, in the
/// directory that is returned with the fixture and must outlive it.
fn fixture_with_global_excludes(rules: &str) -> (Fixture, TempDir) {
    let fixture = Fixture::new();
    let outside = TempDir::new().unwrap();
    let excludes = outside.path().join("global-ignore");
    fs::write(&excludes, rules).unwrap();
    fixture
        .layout
        .repository()
        .config()
        .unwrap()
        .set_str("core.excludesFile", excludes.to_str().unwrap())
        .unwrap();
    (fixture, outside)
}

/// What is inside `<root>/.claude`, by name and sorted; `None` when there is
/// no such path.
fn claude_dir_names(root: &Path) -> Option<Vec<String>> {
    let dir = root.join(".claude");
    if fs::symlink_metadata(&dir).is_err() {
        return None;
    }
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    Some(names)
}

/// `create` declines with the message of [`settings::SettingsNotIgnored`],
/// leaves the entry in `info/exclude` exactly once, and Git still does not
/// ignore the path.
fn assert_create_declines(fixture: &Fixture, binary: &Path, case: &str) {
    let error = settings::create(&fixture.layout, binary).unwrap_err();
    assert!(
        error.is::<settings::SettingsNotIgnored>(),
        "{case}: {error:#}"
    );
    assert_eq!(error.to_string(), NOT_IGNORED_MESSAGE, "{case}");
    assert_eq!(exclude_entries(&fixture.layout), 1, "{case}");
    assert!(!git_ignores_settings(&fixture.root), "{case}");
}

#[test]
fn create_declines_when_a_root_gitignore_rule_overrides_the_exclude_entry() {
    let fixture = fixture_with_gitignore(OVERRIDING_RULE);
    let binary = fake_cvc(&fixture);

    assert_create_declines(&fixture, &binary, "first attempt");
    // Nothing was created: not the settings file, not even its directory.
    assert!(!fixture.root.join(".claude").exists());

    // Asked again, it declines the same way and adds nothing.
    assert_create_declines(&fixture, &binary, "second attempt");
    assert!(!fixture.root.join(".claude").exists());
}

#[test]
fn install_reports_not_ignored_when_a_gitignore_rule_overrides_the_exclude_entry() {
    let fixture = fixture_with_gitignore(OVERRIDING_RULE);
    let binary = fake_cvc(&fixture);
    let settings_path = settings::settings_path(&fixture.layout).unwrap();

    let first = settings::install(&fixture.layout, &binary).unwrap();
    assert_eq!(first.action, InstallAction::Created);
    assert_eq!(first.exclude, ExcludeAction::NotIgnored);
    let rendered: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    for event in settings::HOOK_EVENTS {
        assert_eq!(
            rendered["hooks"][event][0]["hooks"][0]["command"], first.command,
            "{event}: {rendered}"
        );
    }
    // Git agrees with the outcome: the entry is there, the file is not ignored,
    // and it shows up as a new file.
    assert_eq!(exclude_entries(&fixture.layout), 1);
    assert!(!git_ignores_settings(&fixture.root));
    assert!(git_status_lines(&fixture.root)
        .contains(&format!("?? {}", settings::SETTINGS_RELATIVE_PATH)));

    // A second install changes nothing, says the same, and adds no second entry.
    let again = settings::install(&fixture.layout, &binary).unwrap();
    assert_eq!(again.action, InstallAction::AlreadyPresent);
    assert_eq!(again.exclude, ExcludeAction::NotIgnored);
    assert_eq!(exclude_entries(&fixture.layout), 1);
    assert!(!git_ignores_settings(&fixture.root));
}

#[test]
fn create_declines_when_a_gitignore_inside_claude_overrides_the_exclude_entry() {
    let fixture = Fixture::new();
    let binary = fake_cvc(&fixture);
    let claude = fixture.root.join(".claude");
    fs::create_dir(&claude).unwrap();
    fs::write(claude.join(".gitignore"), "!settings.local.json\n").unwrap();

    assert_create_declines(&fixture, &binary, "nested .gitignore");
    // The directory was there already and stays as it was: its one file,
    // unchanged, and no settings file or temporary beside it.
    let names: Vec<String> = fs::read_dir(&claude)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [".gitignore"]);
    assert_eq!(
        fs::read_to_string(claude.join(".gitignore")).unwrap(),
        "!settings.local.json\n"
    );
}

#[test]
fn create_declines_when_a_wildcard_gitignore_rule_overrides_the_exclude_entry() {
    let fixture = fixture_with_gitignore("!/.claude/*.json\n");
    let binary = fake_cvc(&fixture);

    assert_create_declines(&fixture, &binary, "wildcard rule");
    assert!(!fixture.root.join(".claude").exists());
}

#[test]
fn git_decides_for_every_spelling_of_the_overriding_rule() {
    // (file holding the rule, its text)
    let spellings = [
        (".gitignore", "!/.claude/settings.local.json\n"),
        (".gitignore", "!.claude/settings.local.json\n"),
        (".gitignore", "!settings.local.json\n"),
        (".gitignore", "!/.claude/*.json\n"),
        // Re-including the file after ignoring everything else in its directory.
        (".gitignore", ".claude/*\n!.claude/settings.local.json\n"),
        (".claude/.gitignore", "!settings.local.json\n"),
        (".claude/.gitignore", "!*.json\n"),
    ];
    for (file, rules) in spellings {
        let case = format!("{file} holding {rules:?}");
        let with_rules = || {
            let fixture = Fixture::new();
            let path = fixture.root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, rules).unwrap();
            fixture
        };

        let fixture = with_rules();
        let binary = fake_cvc(&fixture);
        let installed = settings::install(&fixture.layout, &binary).expect(&case);
        assert_eq!(installed.action, InstallAction::Created, "{case}");
        assert_eq!(installed.exclude, ExcludeAction::NotIgnored, "{case}");
        assert!(!git_ignores_settings(&fixture.root), "{case}");

        let fixture = with_rules();
        let binary = fake_cvc(&fixture);
        assert_create_declines(&fixture, &binary, &case);
    }
}

#[test]
fn create_declines_and_creates_nothing_when_git_gives_no_verdict() {
    // A submodule at `.claude` makes Git refuse to judge a path inside it. The
    // `.claude` of the checkout is absent, empty, or holds a file, and in each
    // case stays as it was.
    let cases: [(&str, Option<&[&str]>); 3] = [
        ("no .claude directory", None),
        ("an empty .claude directory", Some(&[])),
        ("a .claude directory holding a file", Some(&["notes.md"])),
    ];
    for (case, existing) in cases {
        let fixture = Fixture::new();
        let binary = fake_cvc(&fixture);
        track_claude_as_a_submodule(&fixture);
        if let Some(names) = existing {
            let dir = fixture.root.join(".claude");
            fs::create_dir(&dir).unwrap();
            for name in names {
                fs::write(dir.join(name), "kept\n").unwrap();
            }
        }
        let before = claude_dir_names(&fixture.root);
        // The premise: Git itself gives no verdict here.
        assert_eq!(git_ignore_verdict(&fixture.root), None, "{case}");

        for attempt in ["first attempt", "second attempt"] {
            let error = settings::create(&fixture.layout, &binary).unwrap_err();
            assert!(
                error.is::<settings::SettingsIgnoreUnknown>(),
                "{case}, {attempt}: {error:#}"
            );
            assert!(
                !error.is::<settings::SettingsNotIgnored>(),
                "{case}, {attempt}"
            );
            assert_eq!(
                error.to_string(),
                IGNORE_UNKNOWN_MESSAGE,
                "{case}, {attempt}"
            );
            // Nothing was created, and the entry is in `info/exclude` once.
            assert_eq!(claude_dir_names(&fixture.root), before, "{case}, {attempt}");
            assert_eq!(exclude_entries(&fixture.layout), 1, "{case}, {attempt}");
        }
    }
}

#[test]
fn install_with_a_submodule_at_claude_neither_panics_nor_reports_an_unknown_verdict() {
    let fixture = Fixture::new();
    let binary = fake_cvc(&fixture);
    track_claude_as_a_submodule(&fixture);
    assert_eq!(git_ignore_verdict(&fixture.root), None);

    // Git gives no verdict, so libgit2's answer stands in for it. What that
    // comes to is left open here; it is an outcome or a plain error, never the
    // unattended path's "could not ask".
    match settings::install(&fixture.layout, &binary) {
        Ok(outcome) => assert!(
            matches!(
                outcome.exclude,
                ExcludeAction::NotIgnored | ExcludeAction::AddedToInfoExclude
            ),
            "{:?}",
            outcome.exclude
        ),
        Err(error) => assert!(!error.is::<settings::SettingsIgnoreUnknown>(), "{error:#}"),
    }
    assert_eq!(exclude_entries(&fixture.layout), 1);
}

#[test]
fn install_falls_back_on_libgit2_only_when_git_gives_no_verdict() {
    // Rules that ignore the file and then re-include it: libgit2 and Git agree
    // that it is not ignored when a verdict is to be had...
    let rules = "/.claude/settings.local.json\n!/.claude/settings.local.json\n";
    let fixture = fixture_with_gitignore(rules);
    let binary = fake_cvc(&fixture);
    assert_eq!(git_ignore_verdict(&fixture.root), Some(false));
    let outcome = settings::install(&fixture.layout, &binary).unwrap();
    assert_eq!(outcome.action, InstallAction::Created);
    assert_eq!(outcome.exclude, ExcludeAction::NotIgnored);

    // ...and with a submodule at `.claude`, where Git gives none, libgit2's
    // answer is the same, so is the outcome.
    let fixture = fixture_with_gitignore(rules);
    let binary = fake_cvc(&fixture);
    track_claude_as_a_submodule(&fixture);
    assert_eq!(git_ignore_verdict(&fixture.root), None);
    let outcome = settings::install(&fixture.layout, &binary).unwrap();
    assert_eq!(outcome.action, InstallAction::Created);
    assert_eq!(outcome.exclude, ExcludeAction::NotIgnored);
    assert_eq!(exclude_entries(&fixture.layout), 1);

    // The unattended path does not take libgit2's word for it.
    let fixture = fixture_with_gitignore(rules);
    let binary = fake_cvc(&fixture);
    track_claude_as_a_submodule(&fixture);
    let error = settings::create(&fixture.layout, &binary).unwrap_err();
    assert!(error.is::<settings::SettingsIgnoreUnknown>(), "{error:#}");
    assert_eq!(claude_dir_names(&fixture.root), None);
}

#[test]
fn create_makes_the_file_and_git_ignores_it_when_no_rule_overrides_the_exclude_entry() {
    let fixture = Fixture::new();
    let binary = fake_cvc(&fixture);

    // A first create on a fresh repository adds the entry, on any machine: the
    // user's global excludes do not come into it.
    let outcome = settings::create(&fixture.layout, &binary).unwrap();
    assert_eq!(outcome.action, InstallAction::Created);
    assert_eq!(outcome.exclude, ExcludeAction::AddedToInfoExclude);
    assert!(outcome.settings_path.is_file());
    assert!(git_ignores_settings(&fixture.root));
    assert!(
        git_status_lines(&fixture.root)
            .iter()
            .all(|line| !line.contains(".claude")),
        "{:?}",
        git_status_lines(&fixture.root)
    );
    assert_eq!(exclude_entries(&fixture.layout), 1);
}

#[test]
fn create_adds_the_info_exclude_entry_even_when_a_gitignore_rule_already_ignores_the_file() {
    let rules = [
        ".claude/\n",
        "/.claude/settings.local.json\n",
        // Re-including a neighbour says nothing about this file.
        ".claude/*\n!.claude/settings.json\n",
        // The last matching rule decides.
        "!/.claude/settings.local.json\n/.claude/settings.local.json\n",
    ];
    for rules in rules {
        let fixture = fixture_with_gitignore(rules);
        let binary = fake_cvc(&fixture);

        // The rules alone already ignore the file, and the entry goes in all
        // the same: it must not depend on rules that may change later.
        let outcome = settings::create(&fixture.layout, &binary).expect(rules);
        assert_eq!(outcome.action, InstallAction::Created, "{rules:?}");
        assert_eq!(
            outcome.exclude,
            ExcludeAction::AddedToInfoExclude,
            "{rules:?}"
        );
        assert!(outcome.settings_path.is_file(), "{rules:?}");
        assert_eq!(exclude_entries(&fixture.layout), 1, "{rules:?}");
        assert!(git_ignores_settings(&fixture.root), "{rules:?}");

        // With the line there, nothing is appended again.
        let again = settings::install(&fixture.layout, &binary).expect(rules);
        assert_eq!(again.action, InstallAction::AlreadyPresent, "{rules:?}");
        assert_eq!(again.exclude, ExcludeAction::AlreadyIgnored, "{rules:?}");
        assert_eq!(exclude_entries(&fixture.layout), 1, "{rules:?}");
    }
}

#[test]
fn create_adds_the_info_exclude_entry_whatever_the_global_excludes_say() {
    // (case, what the global excludes file holds, whether Git ignores the file
    // before CVC adds anything)
    let cases = [
        ("lists the file", ".claude/settings.local.json\n", true),
        ("lists the directory", ".claude/\n", true),
        (
            "re-includes the file",
            "!.claude/settings.local.json\n",
            false,
        ),
    ];
    for (case, rules, ignored_before) in cases {
        let (fixture, _outside) = fixture_with_global_excludes(rules);
        let binary = fake_cvc(&fixture);
        assert_eq!(
            git_ignore_verdict(&fixture.root),
            Some(ignored_before),
            "{case}"
        );

        let outcome = settings::create(&fixture.layout, &binary).expect(case);
        assert_eq!(outcome.action, InstallAction::Created, "{case}");
        assert_eq!(outcome.exclude, ExcludeAction::AddedToInfoExclude, "{case}");
        assert!(outcome.settings_path.is_file(), "{case}");
        assert_eq!(exclude_entries(&fixture.layout), 1, "{case}");
        // `info/exclude` outranks the global excludes, so the file is ignored
        // now whatever they say.
        assert!(git_ignores_settings(&fixture.root), "{case}");
    }
}

#[test]
fn info_exclude_keeps_its_content_and_gets_the_entry_once_on_a_line_of_its_own() {
    // (case, what `info/exclude` holds before, whether `create` adds the entry)
    let cases = [
        ("no file", None, true),
        (
            "an exact entry",
            Some("# mine\n/.claude/settings.local.json\n"),
            false,
        ),
        (
            "an exact entry as the last line, no newline after it",
            Some("# mine\n/.claude/settings.local.json"),
            false,
        ),
        ("other lines", Some("# mine\n*.log\n"), true),
        (
            "other lines, no newline after the last",
            Some("# mine\n*.log"),
            true,
        ),
        (
            "the entry commented out",
            Some("# /.claude/settings.local.json\n"),
            true,
        ),
        (
            "the path without its leading slash",
            Some(".claude/settings.local.json\n"),
            true,
        ),
    ];
    for (case, before, adds) in cases {
        let fixture = Fixture::new();
        let binary = fake_cvc(&fixture);
        let exclude = fixture.layout.common_git_dir().join("info/exclude");
        match before {
            Some(text) => {
                fs::create_dir_all(exclude.parent().unwrap()).unwrap();
                fs::write(&exclude, text).unwrap();
            }
            None => {
                let _ = fs::remove_file(&exclude);
            }
        }

        let outcome = settings::create(&fixture.layout, &binary).expect(case);
        assert_eq!(outcome.action, InstallAction::Created, "{case}");
        let expected = if adds {
            ExcludeAction::AddedToInfoExclude
        } else {
            ExcludeAction::AlreadyIgnored
        };
        assert_eq!(outcome.exclude, expected, "{case}");

        let after = fs::read_to_string(&exclude).unwrap();
        let kept = before.unwrap_or_default();
        assert!(after.starts_with(kept), "{case}: {after:?}");
        if adds {
            // What was there stays, a missing final newline is supplied, and
            // the entry is the last line.
            let appended = &after[kept.len()..];
            let appended = if kept.is_empty() || kept.ends_with('\n') {
                appended
            } else {
                appended.strip_prefix('\n').expect(case)
            };
            assert!(
                appended.ends_with(&format!("{EXCLUDE_ENTRY}\n")),
                "{case}: {after:?}"
            );
            assert_eq!(appended.lines().last(), Some(EXCLUDE_ENTRY), "{case}");
        } else {
            assert_eq!(after, kept, "{case}");
        }
        assert_eq!(exclude_entries(&fixture.layout), 1, "{case}");
        assert!(git_ignores_settings(&fixture.root), "{case}");

        // Run again, nothing more is appended.
        settings::install(&fixture.layout, &binary).expect(case);
        assert_eq!(fs::read_to_string(&exclude).unwrap(), after, "{case}");
    }
}

#[test]
fn an_info_exclude_line_that_only_resembles_the_entry_never_leaves_a_file_git_lists() {
    // Lines whose effect on Git differs from how they read. What `create` makes
    // of each is not fixed here; what it leaves behind is: it declines and
    // creates nothing, or it creates the file and Git ignores it.
    let cases = [
        ("a leading space", "  /.claude/settings.local.json\n"),
        ("a leading tab", "\t/.claude/settings.local.json\n"),
        ("trailing spaces", "/.claude/settings.local.json   \n"),
        ("a carriage return", "/.claude/settings.local.json\r\n"),
        (
            "the entry, then a rule that re-includes the file",
            "/.claude/settings.local.json\n!/.claude/settings.local.json\n",
        ),
    ];
    for (case, text) in cases {
        let (fixture, _outside) = fixture_with_global_excludes("");
        let binary = fake_cvc(&fixture);
        let exclude = fixture.layout.common_git_dir().join("info/exclude");
        fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        fs::write(&exclude, text).unwrap();

        match settings::create(&fixture.layout, &binary) {
            Ok(outcome) => {
                assert!(outcome.settings_path.is_file(), "{case}");
                assert!(git_ignores_settings(&fixture.root), "{case}");
                assert!(
                    git_status_lines(&fixture.root)
                        .iter()
                        .all(|line| !line.contains(".claude")),
                    "{case}"
                );
            }
            Err(error) => {
                assert!(
                    error.is::<settings::SettingsNotIgnored>(),
                    "{case}: {error:#}"
                );
                assert_eq!(claude_dir_names(&fixture.root), None, "{case}");
                assert!(!git_ignores_settings(&fixture.root), "{case}");
            }
        }
    }
}

// The refusals that come before the ignore check keep their order. With an
// overriding rule in place, `info/exclude` would get its entry on any machine
// the moment the ignore check was reached, so an entry that is absent shows
// that the refusal came first.

#[test]
fn a_tracked_settings_file_is_refused_before_info_exclude_is_touched() {
    let fixture = fixture_with_gitignore(OVERRIDING_RULE);
    let binary = fake_cvc(&fixture);
    let path = settings::settings_path(&fixture.layout).unwrap();
    fs::create_dir(path.parent().unwrap()).unwrap();
    let committed = "{\"permissions\":{\"allow\":[\"Bash(ls:*)\"]}}\n";
    fs::write(&path, committed).unwrap();
    track_settings_file(&fixture);

    let error = settings::create(&fixture.layout, &binary).unwrap_err();
    assert!(error.is::<settings::TrackedSettings>(), "create: {error:#}");
    assert!(!error.is::<settings::SettingsNotIgnored>(), "create");
    let error = settings::install(&fixture.layout, &binary).unwrap_err();
    assert!(
        error.is::<settings::TrackedSettings>(),
        "install: {error:#}"
    );
    assert!(!error.is::<settings::SettingsNotIgnored>(), "install");
    assert_eq!(exclude_entries(&fixture.layout), 0);
    assert_eq!(fs::read_to_string(&path).unwrap(), committed);
}

#[test]
fn an_existing_settings_file_is_refused_before_info_exclude_is_touched() {
    let fixture = fixture_with_gitignore(OVERRIDING_RULE);
    let binary = fake_cvc(&fixture);
    let path = settings::settings_path(&fixture.layout).unwrap();
    fs::create_dir(path.parent().unwrap()).unwrap();
    fs::write(&path, "{\"permissions\":{}}\n").unwrap();

    let error = settings::create(&fixture.layout, &binary).unwrap_err();
    assert!(error.is::<settings::SettingsExist>(), "{error:#}");
    assert!(!error.is::<settings::SettingsNotIgnored>());
    assert_eq!(exclude_entries(&fixture.layout), 0);
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"permissions\":{}}\n");

    // A link with nothing behind it counts as being there as well.
    #[cfg(unix)]
    {
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(fixture.root.join("nowhere"), &path).unwrap();
        let error = settings::create(&fixture.layout, &binary).unwrap_err();
        assert!(error.is::<settings::SettingsExist>(), "{error:#}");
        assert!(!error.is::<settings::SettingsNotIgnored>());
        assert_eq!(exclude_entries(&fixture.layout), 0);
        assert!(fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}

#[test]
fn a_claude_path_that_is_a_file_is_refused_before_info_exclude_is_touched() {
    let fixture = fixture_with_gitignore(OVERRIDING_RULE);
    let binary = fake_cvc(&fixture);
    fs::write(fixture.root.join(".claude"), "a file\n").unwrap();

    let refusals = [
        ("create", settings::create(&fixture.layout, &binary)),
        ("install", settings::install(&fixture.layout, &binary)),
    ];
    for (name, result) in refusals {
        let error = result.unwrap_err();
        assert!(!error.is::<settings::SettingsNotIgnored>(), "{name}");
        assert!(
            format!("{error:#}").contains("`.claude` in this worktree is not a directory"),
            "{name}: {error:#}"
        );
    }
    assert_eq!(exclude_entries(&fixture.layout), 0);
    assert_eq!(
        fs::read_to_string(fixture.root.join(".claude")).unwrap(),
        "a file\n"
    );
}

#[cfg(unix)]
#[test]
fn a_claude_symlink_is_refused_before_info_exclude_is_touched() {
    // `git` will not look through the link and gives no verdict, so were the
    // ignore check reached `create` would report that and `install` would take
    // libgit2's answer, which reads this pair of rules as "not ignored". The
    // refusal of the link comes first, and says so, in both.
    let fixture = fixture_with_gitignore(".claude/*\n!.claude/settings.local.json\n");
    let binary = fake_cvc(&fixture);
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.root.join(".claude")).unwrap();

    let refusals = [
        ("create", settings::create(&fixture.layout, &binary)),
        ("install", settings::install(&fixture.layout, &binary)),
    ];
    for (name, result) in refusals {
        let error = result.unwrap_err();
        assert!(!error.is::<settings::SettingsNotIgnored>(), "{name}");
        assert!(
            format!("{error:#}")
                .contains("`.claude` in this worktree is a symlink; not following it"),
            "{name}: {error:#}"
        );
    }
    assert_eq!(exclude_entries(&fixture.layout), 0);
    // Nothing was written through the link.
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn a_symlinked_info_exclude_stops_create_and_install_before_anything_is_written() {
    let fixture = fixture_with_gitignore(OVERRIDING_RULE);
    let binary = fake_cvc(&fixture);
    let info = fixture.layout.common_git_dir().join("info");
    fs::create_dir_all(&info).unwrap();
    let _ = fs::remove_file(info.join("exclude"));
    let elsewhere = TempDir::new().unwrap();
    let target = elsewhere.path().join("exclude");
    fs::write(&target, "# kept as it is\n").unwrap();
    std::os::unix::fs::symlink(&target, info.join("exclude")).unwrap();

    let refusals = [
        ("create", settings::create(&fixture.layout, &binary)),
        ("install", settings::install(&fixture.layout, &binary)),
    ];
    for (name, result) in refusals {
        let error = result.unwrap_err();
        assert!(!error.is::<settings::SettingsNotIgnored>(), "{name}");
        assert!(
            format!("{error:#}").contains("refusing to modify symlinked"),
            "{name}: {error:#}"
        );
    }
    // No directory, no settings file, and the link's target is as it was.
    assert!(!fixture.root.join(".claude").exists());
    assert_eq!(fs::read_to_string(&target).unwrap(), "# kept as it is\n");
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
    let window = transcript::parse_window(&bytes, 0, &session, false).unwrap();
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
            subagent: false,
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
        .map(|planned| prompt(planned).len())
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
