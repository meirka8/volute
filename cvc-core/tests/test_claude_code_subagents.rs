//! Public issue 71: a session's subagent (Agent tool) transcripts are ingested
//! as their own conversations alongside the main session, once, with their own
//! titles from the sibling meta file.
use cvc_core::claude_code::{ingest, IngestMode};
use cvc_core::db::CvcStore;
use cvc_core::privacy::PreparedPolicy;
use cvc_core::repository::RepositoryLayout;
use git2::Repository;
use std::fs;
use std::path::PathBuf;
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
    // Main is fine; the subagent names an unknown content block type.
    let broken = subagent_transcript().replace("\"type\":\"thinking\"", "\"type\":\"mystery\"");
    let path = fixture.lay_out(&main_transcript(), Some((&broken, META)));

    let error = ingest(
        &fixture.layout,
        &fixture.store(),
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
    assert_eq!(fixture.conversation_count(SESSION), 1);
    assert_eq!(fixture.conversation_count(&format!("{SESSION}:{AGENT}")), 0);
}
