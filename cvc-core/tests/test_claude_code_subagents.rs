//! Public issue 71: a session's subagent (Agent tool) transcripts are ingested
//! as their own conversations alongside the main session, once, with their own
//! titles from the sibling meta file.
//!
//! Public issue 183: a session and its subagents are one family for the commit
//! rescue. A commit that any of their transcripts created links the family's
//! floating responses, whichever transcript each came from, and `--rescan`
//! repairs a store captured before that.
use cvc_core::claude_code::transcript::derived_interaction_id;
use cvc_core::claude_code::{
    ingest, ingest_with, IngestError, IngestMode, IngestOptions, IngestReport, HARNESS,
};
use cvc_core::db::{CvcStore, HarnessCursorUpdate};
use cvc_core::models::{Author, Conversation, Interaction, TombstoneReasonCode};
use cvc_core::privacy::{ClaudeCodeCapture, PreparedPolicy};
use cvc_core::repository::RepositoryLayout;
use git2::Repository;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const SESSION: &str = "0f773b11-7a20-427d-bd9d-208729c5fd87";
const AGENT: &str = "agent-a0bd78e0693c35eed";

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

    /// Lays out `<dir>/<SESSION>.jsonl` and `<dir>/<SESSION>/subagents/<AGENT>.jsonl`
    /// plus its meta, returning the main transcript path.
    fn lay_out(&self, main: &str, subagent: Option<(&str, &str)>) -> PathBuf {
        let dir = self.root.join("proj");
        fs::create_dir_all(&dir).unwrap();
        let main_path = dir.join(format!("{SESSION}.jsonl"));
        fs::write(&main_path, main).unwrap();
        if let Some((body, meta)) = subagent {
            let sub_dir = dir.join(SESSION).join("subagents");
            fs::create_dir_all(&sub_dir).unwrap();
            fs::write(sub_dir.join(format!("{AGENT}.jsonl")), body).unwrap();
            fs::write(sub_dir.join(format!("{AGENT}.meta.json")), meta).unwrap();
        }
        main_path
    }

    fn ingest(&self, path: &std::path::Path) -> cvc_core::claude_code::IngestReport {
        ingest(
            &self.layout,
            &self.store(),
            &PreparedPolicy::built_ins_only(),
            path,
            Some(SESSION),
            IngestMode::Final,
        )
        .unwrap()
    }

    fn conversation_count(&self, id: &str) -> usize {
        self.store().conversation_interaction_count(id).unwrap()
    }

    /// A real commit made at `secs`, so a transcript can name a hash that
    /// resolves and place its responses on the commit's clock. Returns its SHA.
    fn commit_at(&self, file: &str, secs: i64) -> String {
        fs::write(self.root.join(file), "fn work() {}\n").unwrap();
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
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, file, &tree, &parent_refs)
            .unwrap()
            .to_string()
    }

    /// Lays out `<dir>/<session>.jsonl` and, for each `(stem, script)`,
    /// `<dir>/<session>/subagents/<stem>.jsonl` with its meta, returning the
    /// main transcript path. Files already there are replaced.
    fn lay_out_family(
        &self,
        session: &str,
        main: &Script,
        subagents: &[(&str, &Script)],
    ) -> PathBuf {
        let dir = self.root.join("proj");
        fs::create_dir_all(&dir).unwrap();
        let main_path = dir.join(format!("{session}.jsonl"));
        fs::write(&main_path, main.text()).unwrap();
        for (stem, script) in subagents {
            let sub_dir = dir.join(session).join("subagents");
            fs::create_dir_all(&sub_dir).unwrap();
            fs::write(sub_dir.join(format!("{stem}.jsonl")), script.text()).unwrap();
            fs::write(sub_dir.join(format!("{stem}.meta.json")), META).unwrap();
        }
        main_path
    }

    fn ingest_family(&self, store: &CvcStore, path: &Path, session: &str) -> IngestReport {
        self.try_ingest_family(store, path, session, false)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn rescan_family(&self, store: &CvcStore, path: &Path, session: &str) -> IngestReport {
        self.try_ingest_family(store, path, session, true)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn try_ingest_family(
        &self,
        store: &CvcStore,
        path: &Path,
        session: &str,
        rescan: bool,
    ) -> Result<IngestReport, IngestError> {
        ingest_with(
            &self.layout,
            store,
            &PreparedPolicy::built_ins_only(),
            path,
            Some(session),
            IngestMode::Final,
            IngestOptions {
                rescan,
                ..IngestOptions::default()
            },
        )
    }

    /// Records a floating response of `conversation` the way a capture from
    /// the worktree whose fingerprint is `worktree` would have.
    fn insert_floating(&self, store: &CvcStore, conversation: &str, request: &str, worktree: &str) {
        let id = derived_interaction_id(conversation, request);
        let timestamp = chrono::DateTime::from_timestamp(now() - 1300, 0).unwrap();
        let capture = ClaudeCodeCapture::new(
            Conversation {
                id: conversation.to_owned(),
                title: "Foreign".to_owned(),
                created_at: timestamp,
            },
            Interaction {
                id,
                conversation_id: conversation.to_owned(),
                parent_id: None,
                timestamp,
                author: Author::Agent,
                user_prompt: "task".to_owned(),
                model_name: Some("m".to_owned()),
                model_cot: None,
                model_response: Some("response".to_owned()),
                source_request_id: Some(request.to_owned()),
            },
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            worktree.to_owned(),
        );
        // Under a cursor of its own, so the family's cursors stay as they are.
        store
            .capture_claude_code_batch(
                vec![capture],
                HarnessCursorUpdate {
                    harness: HARNESS,
                    session_id: "foreign-capture",
                    transcript_path: "foreign.jsonl",
                    byte_offset: 0,
                },
            )
            .unwrap();
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// RFC3339 UTC millis for a Unix second.
fn at(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A transcript written entry by entry, each chained to the one before it as
/// Claude Code does. A subagent's entries are side chains carrying the parent
/// session id and the agent id.
#[derive(Clone)]
struct Script {
    session: String,
    agent: Option<String>,
    cwd: String,
    lines: Vec<String>,
    last: Option<String>,
}

impl Script {
    fn main(session: &str, cwd: &Path) -> Self {
        Self {
            session: session.to_owned(),
            agent: None,
            cwd: cwd.to_str().unwrap().to_owned(),
            lines: Vec::new(),
            last: None,
        }
    }

    /// `stem` is the file stem, `agent-<id>`.
    fn subagent(session: &str, stem: &str, cwd: &Path) -> Self {
        Self {
            agent: Some(stem.trim_start_matches("agent-").to_owned()),
            ..Self::main(session, cwd)
        }
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
            "message": message,
            "uuid": uuid,
            "timestamp": at(secs),
            "sessionId": self.session,
            "version": "2.1.260",
            "cwd": self.cwd,
        });
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
        self.push(
            "assistant",
            secs,
            Some(request),
            json!({
                "model": "m", "id": request, "type": "message", "role": "assistant",
                "content": blocks, "stop_reason": stop_reason, "stop_sequence": null,
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }),
        );
    }

    /// What a person (or, in a subagent, the delegating agent) said.
    fn prompt(mut self, secs: i64, text: &str) -> Self {
        self.push("user", secs, None, json!({"role": "user", "content": text}));
        self
    }

    /// A response that only talks.
    fn say(mut self, request: &str, secs: i64, text: &str) -> Self {
        self.assistant(
            request,
            secs,
            json!([{"type": "text", "text": text}]),
            "end_turn",
        );
        self
    }

    /// A response that runs `git commit`, at `secs`, and the output it got a
    /// second later, which names the commit `sha`.
    fn commit(mut self, request: &str, secs: i64, sha: &str) -> Self {
        let tool = format!("tool-{request}");
        self.assistant(
            request,
            secs,
            json!([{"type": "tool_use", "id": tool, "name": "Bash",
                    "input": {"command": "git commit -am work"}}]),
            "tool_use",
        );
        self.push(
            "user",
            secs + 1,
            None,
            json!({"role": "user", "content": [{
                "tool_use_id": tool, "type": "tool_result",
                "content": format!("[main {}] work\n 1 file changed", &sha[..7]),
                "is_error": false
            }]}),
        );
        self
    }

    /// A response whose content block has no type: the parser refuses it.
    fn broken(mut self, request: &str, secs: i64) -> Self {
        self.assistant(
            request,
            secs,
            json!([{"kind": "text", "text": "x"}]),
            "end_turn",
        );
        self
    }

    fn text(&self) -> String {
        self.lines.join("\n") + "\n"
    }
}

/// The conversation a subagent file is stored under.
fn sub(session: &str, stem: &str) -> String {
    format!("{session}:{stem}")
}

/// The commits (and link types) a recorded response is linked to. Fails when
/// the response was never recorded, so a mistyped key cannot pass for "floating".
fn links_of(store: &CvcStore, conversation: &str, request: &str) -> Vec<(String, String)> {
    let id = derived_interaction_id(conversation, request);
    assert!(
        store.interaction_exists(&id).unwrap(),
        "{conversation} {request} is not recorded"
    );
    store
        .get_artifact_links(&id)
        .unwrap()
        .into_iter()
        .map(|link| (link.git_commit_hash.as_str().to_owned(), link.link_type))
        .collect()
}

fn assert_linked(store: &CvcStore, conversation: &str, request: &str, sha: &str) {
    assert_eq!(
        links_of(store, conversation, request),
        vec![(sha.to_owned(), "generated".to_owned())],
        "{conversation} {request}"
    );
}

fn assert_floating(store: &CvcStore, conversation: &str, request: &str) {
    assert_eq!(
        links_of(store, conversation, request),
        vec![],
        "{conversation} {request}"
    );
}

fn cursor_of(store: &CvcStore, conversation: &str) -> u64 {
    store
        .harness_ingest_cursor(HARNESS, conversation)
        .unwrap()
        .unwrap_or_else(|| panic!("no cursor for {conversation}"))
        .byte_offset
}

fn main_transcript() -> String {
    [
        format!(r#"{{"parentUuid":null,"isSidechain":false,"type":"user","message":{{"role":"user","content":"Delegate to a subagent"}},"uuid":"u1","timestamp":"2026-08-10T22:04:00.000Z","sessionId":"{SESSION}","version":"2.1.219","cwd":"x"}}"#),
        format!(r#"{{"parentUuid":"u1","isSidechain":false,"requestId":"req_A","type":"assistant","message":{{"model":"m","id":"a","type":"message","role":"assistant","content":[{{"type":"text","text":"Delegating."}}],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"s1","timestamp":"2026-08-10T22:04:01.000Z","sessionId":"{SESSION}","version":"2.1.219","cwd":"x"}}"#),
    ]
    .join("\n")
        + "\n"
}

/// A subagent transcript: every entry is a sidechain carrying the parent
/// session id and the agent id, exactly as Claude Code writes it.
fn subagent_transcript() -> String {
    [
        format!(r#"{{"parentUuid":null,"isSidechain":true,"agentId":"a0bd78e0693c35eed","type":"user","message":{{"role":"user","content":"You are the subagent; do the task."}},"uuid":"su1","timestamp":"2026-08-10T22:04:26.089Z","sessionId":"{SESSION}","version":"2.1.219","cwd":"x"}}"#),
        format!(r#"{{"parentUuid":"su1","isSidechain":true,"agentId":"a0bd78e0693c35eed","requestId":"req_S","type":"assistant","message":{{"model":"m","id":"sa","type":"message","role":"assistant","content":[{{"type":"thinking","thinking":"Subagent reasoning here.","signature":"z"}}],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"su2","timestamp":"2026-08-10T22:04:27.000Z","sessionId":"{SESSION}","version":"2.1.219","cwd":"x"}}"#),
        format!(r#"{{"parentUuid":"su2","isSidechain":true,"agentId":"a0bd78e0693c35eed","requestId":"req_S2","type":"assistant","message":{{"model":"m","id":"sb","type":"message","role":"assistant","content":[{{"type":"text","text":"Subagent done."}}],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"su3","timestamp":"2026-08-10T22:04:28.000Z","sessionId":"{SESSION}","version":"2.1.219","cwd":"x"}}"#),
    ]
    .join("\n")
        + "\n"
}

const META: &str = r#"{"agentType":"ux-psychologist","description":"Design the keymap","model":"sonnet","spawnDepth":1,"toolUseId":"toolu_01SN"}"#;

#[test]
fn subagent_transcript_becomes_its_own_conversation() {
    let fixture = Fixture::new();
    let path = fixture.lay_out(&main_transcript(), Some((&subagent_transcript(), META)));

    let report = fixture.ingest(&path);
    // Main session: the two main-transcript responses.
    assert_eq!(report.session_id, SESSION);
    assert_eq!(report.inserted, 1, "{report:?}"); // one closed assistant response (req_A)
    assert_eq!(report.subagent_sessions, 1, "{report:?}");
    assert_eq!(report.subagent_inserted, 2, "{report:?}"); // req_S, req_S2

    let sub_conv = format!("{SESSION}:{AGENT}");
    assert_eq!(fixture.conversation_count(&sub_conv), 2);
    // The subagent's reasoning is captured and kept out of the parent conversation.
    assert_eq!(fixture.conversation_count(SESSION), 1);

    // Titled from the meta file.
    let store = fixture.store();
    let title = store.get_conversation(&sub_conv).unwrap().unwrap().title;
    assert!(title.contains("ux-psychologist"), "{title}");

    // Idempotent: a second ingest of the whole layout adds nothing.
    let again = fixture.ingest(&path);
    assert_eq!(again.inserted, 0, "{again:?}");
    assert_eq!(again.subagent_inserted, 0, "{again:?}");
    assert_eq!(fixture.conversation_count(&sub_conv), 2);
}

#[test]
fn a_session_without_subagents_reports_none() {
    let fixture = Fixture::new();
    let path = fixture.lay_out(&main_transcript(), None);
    let report = fixture.ingest(&path);
    assert_eq!(report.subagent_sessions, 0);
    assert_eq!(report.subagent_inserted, 0);
}

#[test]
fn a_malformed_subagent_fails_loudly_after_the_main_session_persists() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let t = now() - 1000;
    let sha = fixture.commit_at("work.rs", t);
    // Main is fine and committed; the subagent has a content block without a type.
    let main = Script::main(SESSION, &fixture.root)
        .prompt(t - 1000, "Delegate to a subagent")
        .say("req_A", t - 950, "Delegating.")
        .commit("req_B", t - 2, &sha)
        .say("req_C", t + 10, "Committed.");
    let broken = subagent_transcript().replace("\"type\":\"thinking\"", "\"kind\":\"thinking\"");
    let path = fixture.lay_out(&main.text(), Some((&broken, META)));

    let error = ingest(
        &fixture.layout,
        &store,
        &PreparedPolicy::built_ins_only(),
        &path,
        Some(SESSION),
        IngestMode::Final,
    )
    .unwrap_err();
    assert!(
        matches!(error, cvc_core::claude_code::IngestError::Transcript(_)),
        "{error}"
    );
    // The main session was committed before the subagent was attempted, so it
    // is durable and its cursor advanced; only the subagent is unrecorded.
    assert_eq!(fixture.conversation_count(SESSION), 3);
    assert_eq!(fixture.conversation_count(&format!("{SESSION}:{AGENT}")), 0);
    // Its cursor is past the committing response, which is never read again,
    // so the rescue has to have happened before the error was returned.
    assert_linked(&store, SESSION, "req_A", &sha);
    assert_linked(&store, SESSION, "req_B", &sha);
    assert_floating(&store, SESSION, "req_C");
}

const OTHER_AGENT_STEM: &str = "agent-b7f31c05d9e8a2461";

/// A session that delegated to one subagent, before it committed anything:
/// every response of both is floating. Times are relative to `t`.
fn uncommitted_family(root: &Path, t: i64) -> (Script, Script) {
    let main = Script::main(SESSION, root)
        .prompt(t - 1000, "Build the feature")
        .say("req_A", t - 950, "Delegating the work.");
    let agent = Script::subagent(SESSION, AGENT, root)
        .prompt(t - 900, "Implement the feature")
        .say("req_S1", t - 800, "Reading the code.")
        .say("req_S2", t - 600, "Writing the code.")
        .say("req_S3", t - 400, "Done.")
        .say("req_S4", t + 300, "A late note.");
    (main, agent)
}

/// The same session after its own response ran `git commit`, at `t`.
fn committed(main: &Script, sha: &str, t: i64) -> Script {
    main.clone()
        .commit("req_B", t - 2, sha)
        .say("req_C", t + 10, "Committed.")
}

/// The parent's `git commit` links the reasoning a subagent did earlier, when
/// the subagent was already in the store, floating, from a previous run.
#[test]
fn the_parents_commit_links_a_subagents_earlier_floating_responses() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let t = now() - 1000;
    let sha = fixture.commit_at("work.rs", t);
    let sub_id = sub(SESSION, AGENT);
    let (main, agent) = uncommitted_family(&fixture.root, t);

    let path = fixture.lay_out_family(SESSION, &main, &[(AGENT, &agent)]);
    let first = fixture.ingest_family(&store, &path, SESSION);
    assert_eq!(first.subagent_inserted, 4, "{first:?}");
    assert_eq!((first.linked_from_commits, first.subagent_linked), (0, 0));
    assert_floating(&store, &sub_id, "req_S1");

    fixture.lay_out_family(SESSION, &committed(&main, &sha, t), &[(AGENT, &agent)]);
    let second = fixture.ingest_family(&store, &path, SESSION);
    assert_eq!(second.inserted, 2, "{second:?}"); // the commit and the closing response
    assert_eq!(second.linked_from_commits, 2, "{second:?}"); // req_A, req_B
    assert_eq!(second.subagent_linked, 3, "{second:?}"); // req_S1..req_S3

    // The session's own responses as before, the subagent's up to the commit.
    assert_linked(&store, SESSION, "req_A", &sha);
    assert_linked(&store, SESSION, "req_B", &sha);
    for request in ["req_S1", "req_S2", "req_S3"] {
        assert_linked(&store, &sub_id, request, &sha);
    }
    // After the commit stays floating, in both conversations.
    assert_floating(&store, SESSION, "req_C");
    assert_floating(&store, &sub_id, "req_S4");

    // A third run changes nothing.
    let third = fixture.ingest_family(&store, &path, SESSION);
    assert_eq!((third.inserted, third.linked_from_commits), (0, 0));
    assert_eq!(third.subagent_linked, 0, "{third:?}");
}

/// Issue 183, the order: the session used to be rescued before its subagents
/// were ingested, so a subagent that first arrives in the run that reads the
/// commit was never in reach of it.
#[test]
fn a_subagent_ingested_in_the_same_run_as_the_parents_commit_is_linked() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let t = now() - 1000;
    let sha = fixture.commit_at("work.rs", t);
    let sub_id = sub(SESSION, AGENT);
    let (main, agent) = uncommitted_family(&fixture.root, t);

    let path = fixture.lay_out_family(SESSION, &committed(&main, &sha, t), &[(AGENT, &agent)]);
    let report = fixture.ingest_family(&store, &path, SESSION);
    assert_eq!(report.inserted, 3, "{report:?}");
    assert_eq!(report.subagent_sessions, 1, "{report:?}");
    assert_eq!(report.subagent_inserted, 4, "{report:?}");
    assert_eq!(report.linked_from_commits, 2, "{report:?}");
    assert_eq!(report.subagent_linked, 3, "{report:?}");

    assert_linked(&store, SESSION, "req_A", &sha);
    assert_linked(&store, SESSION, "req_B", &sha);
    for request in ["req_S1", "req_S2", "req_S3"] {
        assert_linked(&store, &sub_id, request, &sha);
    }
    assert_floating(&store, SESSION, "req_C");
    assert_floating(&store, &sub_id, "req_S4");
}

/// A commit a subagent itself ran links the session's floating turns too.
#[test]
fn a_subagents_own_commit_links_the_whole_family() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let t = now() - 1000;
    let sha = fixture.commit_at("work.rs", t);
    let sub_id = sub(SESSION, AGENT);

    let main = Script::main(SESSION, &fixture.root)
        .prompt(t - 1000, "Build the feature")
        .say("req_P1", t - 900, "Delegating the work.")
        .say("req_P2", t - 100, "Waiting for the agent.")
        .say("req_P3", t + 50, "The agent committed.");
    let agent = Script::subagent(SESSION, AGENT, &fixture.root)
        .prompt(t - 500, "Implement it and commit")
        .say("req_S1", t - 400, "Writing the code.")
        .commit("req_S2", t - 2, &sha)
        .say("req_S3", t + 20, "Committed.");
    let path = fixture.lay_out_family(SESSION, &main, &[(AGENT, &agent)]);

    let report = fixture.ingest_family(&store, &path, SESSION);
    assert_eq!(report.linked_from_commits, 2, "{report:?}"); // req_P1, req_P2
    assert_eq!(report.subagent_linked, 2, "{report:?}"); // req_S1, req_S2
    assert_linked(&store, SESSION, "req_P1", &sha);
    assert_linked(&store, SESSION, "req_P2", &sha);
    assert_linked(&store, &sub_id, "req_S1", &sha);
    assert_linked(&store, &sub_id, "req_S2", &sha);
    assert_floating(&store, SESSION, "req_P3");
    assert_floating(&store, &sub_id, "req_S3");
}

/// What the time-window linker (or anyone) already linked is not the rescue's
/// to touch, and nothing ends up with a second link.
#[test]
fn responses_that_are_already_linked_are_left_untouched() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let sub_id = sub(SESSION, AGENT);
    let (t1, t2) = (now() - 900, now() - 600);

    // The first commit comes with no transcript evidence of its own: only the
    // post-commit linker sees it, and links what is in the store.
    let sha1 = fixture.commit_at("first.rs", t1);
    let main = Script::main(SESSION, &fixture.root)
        .prompt(t1 - 200, "Build the feature")
        .say("req_A", t1 - 100, "Delegating the work.");
    let agent = Script::subagent(SESSION, AGENT, &fixture.root)
        .prompt(t1 - 150, "Implement the feature")
        .say("req_S1", t1 - 80, "Reading the code.")
        .say("req_S2", t1 - 40, "Writing the code.");
    let path = fixture.lay_out_family(SESSION, &main, &[(AGENT, &agent)]);
    fixture.ingest_family(&store, &path, SESSION);
    let linked = cvc_core::linker::link_current_commit_to_floating_nodes(
        fixture.layout.repository(),
        &store,
    )
    .unwrap();
    assert_eq!(linked, 3, "the linker linked what it could see");
    for (conversation, request) in [
        (SESSION, "req_A"),
        (sub_id.as_str(), "req_S1"),
        (sub_id.as_str(), "req_S2"),
    ] {
        assert_eq!(
            links_of(&store, conversation, request),
            vec![(sha1.clone(), "temporal".to_owned())]
        );
    }

    // The session goes on, commits again, and its transcript says so.
    let sha2 = fixture.commit_at("second.rs", t2);
    let main = main
        .say("req_A2", t1 + 60, "More work.")
        .commit("req_B", t2 - 2, &sha2)
        .say("req_C", t2 + 10, "Committed.");
    let agent =
        agent
            .say("req_S3", t1 + 100, "More code.")
            .say("req_S4", t2 + 50, "After the commit.");
    fixture.lay_out_family(SESSION, &main, &[(AGENT, &agent)]);
    let report = fixture.ingest_family(&store, &path, SESSION);
    assert_eq!(report.linked_from_commits, 2, "{report:?}"); // req_A2, req_B
    assert_eq!(report.subagent_linked, 1, "{report:?}"); // req_S3

    // What the linker linked keeps its one link, to its commit.
    for (conversation, request) in [
        (SESSION, "req_A"),
        (sub_id.as_str(), "req_S1"),
        (sub_id.as_str(), "req_S2"),
    ] {
        assert_eq!(
            links_of(&store, conversation, request),
            vec![(sha1.clone(), "temporal".to_owned())],
            "{conversation} {request}"
        );
    }
    assert_linked(&store, SESSION, "req_A2", &sha2);
    assert_linked(&store, SESSION, "req_B", &sha2);
    assert_linked(&store, &sub_id, "req_S3", &sha2);
    assert_floating(&store, SESSION, "req_C");
    assert_floating(&store, &sub_id, "req_S4");

    // Run again, and rescan: nothing is linked a second time.
    for report in [
        fixture.ingest_family(&store, &path, SESSION),
        fixture.rescan_family(&store, &path, SESSION),
    ] {
        assert_eq!(report.inserted, 0, "{report:?}");
        assert_eq!(report.linked_from_commits, 0, "{report:?}");
        assert_eq!(report.subagent_linked, 0, "{report:?}");
    }
    for (conversation, request) in [
        (SESSION, "req_A"),
        (SESSION, "req_A2"),
        (SESSION, "req_B"),
        (sub_id.as_str(), "req_S1"),
        (sub_id.as_str(), "req_S2"),
        (sub_id.as_str(), "req_S3"),
    ] {
        assert_eq!(
            links_of(&store, conversation, request).len(),
            1,
            "{conversation} {request}"
        );
    }
}

/// The family is this session's own conversations in this worktree. Another
/// session's, whatever its id looks like, another worktree's, and a
/// conversation that merely carries this session's id as a prefix are not.
#[test]
fn another_sessions_subagents_and_another_worktrees_thoughts_are_never_claimed() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let t = now() - 1000;
    let sha = fixture.commit_at("work.rs", t);
    let sub_id = sub(SESSION, AGENT);

    // Another session, whose id begins with this one's, with a subagent of its
    // own, all floating and all before the commit.
    let other = format!("{SESSION}-other");
    let other_main = Script::main(&other, &fixture.root)
        .prompt(t - 800, "Another job")
        .say("req_X1", t - 700, "Working.");
    let other_agent = Script::subagent(&other, OTHER_AGENT_STEM, &fixture.root)
        .prompt(t - 650, "Help")
        .say("req_Y1", t - 600, "Helping.");
    let other_path =
        fixture.lay_out_family(&other, &other_main, &[(OTHER_AGENT_STEM, &other_agent)]);
    fixture.ingest_family(&store, &other_path, &other);

    let (main, agent) = uncommitted_family(&fixture.root, t);
    let path = fixture.lay_out_family(SESSION, &main, &[(AGENT, &agent)]);
    fixture.ingest_family(&store, &path, SESSION);

    // Responses in this session's conversations but captured in another
    // worktree, and a conversation of this worktree whose id has this session's
    // as a prefix but whose transcript is not among this session's.
    let here = fixture.layout.worktree_origin().unwrap();
    let elsewhere = "ab".repeat(32); // a fingerprint is 64 lowercase hex digits
    assert_ne!(elsewhere, here);
    fixture.insert_floating(&store, SESSION, "req_elsewhere", &elsewhere);
    fixture.insert_floating(&store, &sub_id, "req_elsewhere_sub", &elsewhere);
    let lookalike = sub(SESSION, "agent-without-a-transcript");
    fixture.insert_floating(&store, &lookalike, "req_L", &here);

    fixture.lay_out_family(SESSION, &committed(&main, &sha, t), &[(AGENT, &agent)]);
    let report = fixture.ingest_family(&store, &path, SESSION);
    assert_eq!(report.linked_from_commits, 2, "{report:?}");
    assert_eq!(report.subagent_linked, 3, "{report:?}");

    assert_linked(&store, SESSION, "req_B", &sha);
    assert_linked(&store, &sub_id, "req_S1", &sha);
    for (conversation, request) in [
        (other.as_str(), "req_X1"),
        (sub(&other, OTHER_AGENT_STEM).as_str(), "req_Y1"),
        (SESSION, "req_elsewhere"),
        (sub_id.as_str(), "req_elsewhere_sub"),
        (lookalike.as_str(), "req_L"),
    ] {
        assert_floating(&store, conversation, request);
    }
}

/// Each commit claims only what is still unclaimed up to its own time, so the
/// subagent's responses split between two commits as the session's do.
#[test]
fn two_commits_split_the_subagents_responses_between_them() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let (t1, t2) = (now() - 1000, now() - 500);
    let sha1 = fixture.commit_at("first.rs", t1);
    let sha2 = fixture.commit_at("second.rs", t2);
    let sub_id = sub(SESSION, AGENT);

    let main = Script::main(SESSION, &fixture.root)
        .prompt(t1 - 1000, "Build it in two steps")
        .say("req_A1", t1 - 900, "Delegating the first step.")
        .commit("req_B1", t1 - 2, &sha1)
        .say("req_C1", t1 + 50, "First step committed.")
        .say("req_A2", t1 + 200, "Delegating the second step.")
        .commit("req_B2", t2 - 2, &sha2)
        .say("req_C2", t2 + 30, "Second step committed.");
    let agent = Script::subagent(SESSION, AGENT, &fixture.root)
        .prompt(t1 - 600, "Do both steps")
        .say("req_SA", t1 - 500, "First step.")
        .say("req_SB", t1 + 100, "Between the commits.")
        .say("req_SC", t1 + 300, "Also between the commits.")
        .say("req_SD", t2 + 100, "After both.");
    let path = fixture.lay_out_family(SESSION, &main, &[(AGENT, &agent)]);

    let report = fixture.ingest_family(&store, &path, SESSION);
    assert_eq!(report.linked_from_commits, 5, "{report:?}"); // A1 B1 | C1 A2 B2
    assert_eq!(report.subagent_linked, 3, "{report:?}"); // SA | SB SC
    assert_linked(&store, SESSION, "req_A1", &sha1);
    assert_linked(&store, SESSION, "req_B1", &sha1);
    assert_linked(&store, &sub_id, "req_SA", &sha1);
    for request in ["req_C1", "req_A2", "req_B2"] {
        assert_linked(&store, SESSION, request, &sha2);
    }
    assert_linked(&store, &sub_id, "req_SB", &sha2);
    assert_linked(&store, &sub_id, "req_SC", &sha2);
    assert_floating(&store, SESSION, "req_C2");
    assert_floating(&store, &sub_id, "req_SD");
}

/// Issue 183, the repair: a session captured before the fix has its parent
/// linked and its subagents floating, and the parent's cursor is past the
/// commit, so only a re-read from the start can find it again.
#[test]
fn rescan_links_the_subagents_of_a_session_that_is_already_captured() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let t = now() - 1000;
    let sha = fixture.commit_at("work.rs", t);
    let sub_id = sub(SESSION, AGENT);
    let (main, agent) = uncommitted_family(&fixture.root, t);
    let main = committed(&main, &sha, t);

    // The old behaviour: the session committed and was captured on its own
    // (its subagent's file is not there yet, as after a hook that was killed),
    // then the subagent arrives in a later run that no longer reads the commit.
    let path = fixture.lay_out_family(SESSION, &main, &[]);
    let first = fixture.ingest_family(&store, &path, SESSION);
    assert_eq!(first.linked_from_commits, 2, "{first:?}");
    fixture.lay_out_family(SESSION, &main, &[(AGENT, &agent)]);
    let later = fixture.ingest_family(&store, &path, SESSION);
    assert_eq!(later.subagent_inserted, 4, "{later:?}");
    assert_eq!(
        (later.linked_from_commits, later.subagent_linked),
        (0, 0),
        "{later:?}"
    );
    assert_floating(&store, &sub_id, "req_S1");
    let parent_cursor = cursor_of(&store, SESSION);
    let agent_cursor = cursor_of(&store, &sub_id);

    // One thought of the subagent was deleted by its owner: it stays deleted.
    let deleted = derived_interaction_id(&sub_id, "req_S2");
    store
        .tombstone_local(&deleted, TombstoneReasonCode::UserRequested, None)
        .unwrap();
    assert_eq!(store.conversation_interaction_count(&sub_id).unwrap(), 3);

    let rescan = fixture.rescan_family(&store, &path, SESSION);
    assert_eq!(rescan.inserted, 0, "{rescan:?}");
    assert_eq!(rescan.subagent_inserted, 0, "{rescan:?}");
    assert_eq!(rescan.linked_from_commits, 0, "{rescan:?}"); // already linked
    assert_eq!(rescan.subagent_linked, 2, "{rescan:?}"); // req_S1, req_S3
    assert_linked(&store, &sub_id, "req_S1", &sha);
    assert_linked(&store, &sub_id, "req_S3", &sha);
    assert_floating(&store, &sub_id, "req_S4");
    assert_linked(&store, SESSION, "req_B", &sha);
    assert_floating(&store, SESSION, "req_C");
    // Suppressed, not recorded again, and not linked.
    assert!(!store.interaction_exists(&deleted).unwrap());
    assert!(store.is_tombstoned(&deleted).unwrap());
    assert_eq!(store.conversation_interaction_count(&sub_id).unwrap(), 3);
    // The cursors end where a normal run leaves them, never earlier.
    assert_eq!(cursor_of(&store, SESSION), parent_cursor);
    assert_eq!(cursor_of(&store, &sub_id), agent_cursor);
    assert_eq!(rescan.transcript_len, fs::metadata(&path).unwrap().len());

    // A second rescan, and a plain run after it, change nothing.
    for report in [
        fixture.rescan_family(&store, &path, SESSION),
        fixture.ingest_family(&store, &path, SESSION),
    ] {
        assert_eq!(report.inserted + report.subagent_inserted, 0, "{report:?}");
        assert_eq!(
            report.linked_from_commits + report.subagent_linked,
            0,
            "{report:?}"
        );
    }
    for request in ["req_S1", "req_S3"] {
        assert_eq!(links_of(&store, &sub_id, request).len(), 1);
    }
    assert_eq!(cursor_of(&store, SESSION), parent_cursor);
    assert_eq!(cursor_of(&store, &sub_id), agent_cursor);
}

/// A rescan is a re-read, not a reset: a cursor a later run left further on
/// stays where it is.
#[test]
fn rescan_never_moves_a_cursor_backwards() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let t = now() - 1000;
    let (main, agent) = uncommitted_family(&fixture.root, t);
    let path = fixture.lay_out_family(SESSION, &main, &[(AGENT, &agent)]);
    let sub_id = sub(SESSION, AGENT);

    // Incremental runs leave the last response open, so their cursor stops
    // earlier than a final run's.
    let incremental = |rescan: bool| {
        ingest_with(
            &fixture.layout,
            &store,
            &PreparedPolicy::built_ins_only(),
            &path,
            Some(SESSION),
            IngestMode::Incremental,
            IngestOptions {
                rescan,
                ..IngestOptions::default()
            },
        )
        .unwrap()
    };
    incremental(false);
    let incremental_cursor = (cursor_of(&store, SESSION), cursor_of(&store, &sub_id));
    fixture.ingest_family(&store, &path, SESSION);
    let final_cursor = (cursor_of(&store, SESSION), cursor_of(&store, &sub_id));
    assert!(final_cursor > incremental_cursor);

    let report = incremental(true);
    assert_eq!(report.inserted + report.subagent_inserted, 0, "{report:?}");
    assert_eq!(
        (cursor_of(&store, SESSION), cursor_of(&store, &sub_id)),
        final_cursor
    );
}

/// A subagent that fails does not stop the family rescue: the session is
/// already recorded and its cursor is past its committing response, and the
/// subagents before and after the failing one are rescued with it.
#[test]
fn a_failing_subagent_does_not_stop_the_rescue_of_the_rest_of_the_family() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let t = now() - 1000;
    let sha = fixture.commit_at("work.rs", t);
    let (first_stem, second_stem) = ("agent-a0000000000000000", "agent-b0000000000000000");
    let (first_id, second_id) = (sub(SESSION, first_stem), sub(SESSION, second_stem));

    let main = Script::main(SESSION, &fixture.root)
        .prompt(t - 1000, "Build the feature")
        .say("req_A", t - 950, "Delegating the work.");
    let first = Script::subagent(SESSION, first_stem, &fixture.root)
        .prompt(t - 900, "Part one")
        .say("req_S1", t - 800, "Part one.");
    let second = Script::subagent(SESSION, second_stem, &fixture.root)
        .prompt(t - 700, "Part two")
        .say("req_S2", t - 600, "Part two.");
    let path = fixture.lay_out_family(
        SESSION,
        &main,
        &[(first_stem, &first), (second_stem, &second)],
    );
    fixture.ingest_family(&store, &path, SESSION);
    assert_floating(&store, &first_id, "req_S1");
    assert_floating(&store, &second_id, "req_S2");

    // The session commits, and the first subagent's file now has an entry the
    // parser refuses: the run fails there, before it reaches the second.
    let main = committed(&main, &sha, t);
    let first = first.broken("req_S1b", t - 500);
    let path = fixture.lay_out_family(
        SESSION,
        &main,
        &[(first_stem, &first), (second_stem, &second)],
    );
    let error = fixture
        .try_ingest_family(&store, &path, SESSION, false)
        .unwrap_err();
    assert!(matches!(error, IngestError::Transcript(_)), "{error}");

    assert_linked(&store, SESSION, "req_A", &sha);
    assert_linked(&store, SESSION, "req_B", &sha);
    assert_floating(&store, SESSION, "req_C");
    // Recorded in an earlier run: part of the family, reached or not.
    assert_linked(&store, &first_id, "req_S1", &sha);
    assert_linked(&store, &second_id, "req_S2", &sha);
}
