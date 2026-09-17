//! Public issue 70: a response that runs `git commit` — and the reasoning
//! before it — links to the commit it created, by exact transcript evidence
//! rather than the time-window linker, and re-ingesting changes nothing.
use cvc_core::claude_code::{ingest, IngestMode};
use cvc_core::db::CvcStore;
use cvc_core::models::InteractionId;
use cvc_core::privacy::PreparedPolicy;
use cvc_core::repository::RepositoryLayout;
use git2::Repository;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

const SESSION: &str = "5f2a7c9d-1e60-4c8d-9e3b-0b7d2c4e6a1f";

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

    /// Makes a real commit and returns its full SHA and commit time, so the
    /// transcript can name a hash that actually resolves and stamp its
    /// responses on the same clock the commit carries.
    fn commit(&self, file: &str, content: &str, message: &str) -> (String, i64) {
        fs::write(self.root.join(file), content).unwrap();
        let repo = self.layout.repository();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(file)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
        let parents: Vec<git2::Commit> = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap();
        let time = repo.find_commit(oid).unwrap().time().seconds();
        (oid.to_string(), time)
    }

    fn store(&self) -> CvcStore {
        CvcStore::open(self.layout.db_path()).unwrap()
    }

    fn write_transcript(&self, body: &str) -> PathBuf {
        let dir = self.root.join("t");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{SESSION}.jsonl"));
        fs::write(&path, body).unwrap();
        path
    }

    fn ingest(
        &self,
        path: &std::path::Path,
        store: &CvcStore,
    ) -> cvc_core::claude_code::IngestReport {
        ingest(
            &self.layout,
            store,
            &PreparedPolicy::built_ins_only(),
            path,
            Some(SESSION),
            IngestMode::Final,
        )
        .unwrap()
    }
}

/// RFC3339 UTC millis for a Unix second, so transcript timestamps sit on the
/// same clock as the commit under test.
fn at(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A response that thinks, then a response that commits, referencing the real
/// SHA in the commit tool result. Responses are stamped just before the commit
/// time, and the closing response just after it.
fn transcript(root: &str, sha: &str, commit_time: i64) -> String {
    let short = &sha[..7];
    let w = root;
    let (t_prompt, t_a, t_b, t_result, t_c) = (
        at(commit_time - 4),
        at(commit_time - 3),
        at(commit_time - 2),
        at(commit_time - 1),
        at(commit_time + 1),
    );
    [
        // parent-less human prompt
        format!(r#"{{"parentUuid":null,"isSidechain":false,"type":"user","message":{{"role":"user","content":"Add a feature and commit it"}},"uuid":"u1","timestamp":"{t_prompt}","sessionId":"{SESSION}","version":"2.1.260","cwd":"{w}"}}"#),
        // response A: reasoning only (the kind the linker would strand — no context, precedes the commit)
        format!(r#"{{"parentUuid":"u1","isSidechain":false,"requestId":"req_A","type":"assistant","message":{{"model":"m","id":"a","type":"message","role":"assistant","content":[{{"type":"thinking","thinking":"I will design the feature first.","signature":"s"}}],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"s1","timestamp":"{t_a}","sessionId":"{SESSION}","version":"2.1.260","cwd":"{w}"}}"#),
        // response B: runs git commit
        format!(r#"{{"parentUuid":"s1","isSidechain":false,"requestId":"req_B","type":"assistant","message":{{"model":"m","id":"b","type":"message","role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"git commit -am feature"}}}}],"stop_reason":"tool_use","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"s2","timestamp":"{t_b}","sessionId":"{SESSION}","version":"2.1.260","cwd":"{w}"}}"#),
        // tool result carrying the real short SHA
        format!(r#"{{"parentUuid":"s2","isSidechain":false,"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"t1","type":"tool_result","content":"[main {short}] feature\n 1 file changed","is_error":false}}]}},"uuid":"u2","timestamp":"{t_result}","sessionId":"{SESSION}","version":"2.1.260","cwd":"{w}"}}"#),
        // response C: closes the session
        format!(r#"{{"parentUuid":"u2","isSidechain":false,"requestId":"req_C","type":"assistant","message":{{"model":"m","id":"c","type":"message","role":"assistant","content":[{{"type":"text","text":"Committed."}}],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"s3","timestamp":"{t_c}","sessionId":"{SESSION}","version":"2.1.260","cwd":"{w}"}}"#),
    ]
    .join("\n")
        + "\n"
}

fn links_to(store: &CvcStore, response_key: &str, sha: &str) -> bool {
    let id = cvc_core::claude_code::transcript::derived_interaction_id(SESSION, response_key);
    store
        .get_artifact_links(&id)
        .unwrap()
        .iter()
        .any(|link| link.git_commit_hash.as_str() == sha && link.link_type == "generated")
}

#[test]
fn committing_response_and_prior_reasoning_link_to_the_commit() {
    let fixture = Fixture::new();
    let (sha, commit_time) = fixture.commit("feature.rs", "fn feature() {}\n", "feature");
    let path = fixture.write_transcript(&transcript(
        fixture.root.to_str().unwrap(),
        &sha,
        commit_time,
    ));
    let store = fixture.store();

    let report = fixture.ingest(&path, &store);
    assert_eq!(report.inserted, 3, "{report:?}");
    // The committing response (B) and the reasoning before it (A) both link to
    // the commit; the closing response (C) came after the commit and stays
    // floating.
    assert!(report.linked_from_commits >= 2, "{report:?}");
    assert!(
        links_to(&store, "req_B", &sha),
        "committing response linked"
    );
    assert!(links_to(&store, "req_A", &sha), "prior reasoning linked");
    let closing = cvc_core::claude_code::transcript::derived_interaction_id(SESSION, "req_C");
    assert!(
        store.get_artifact_links(&closing).unwrap().is_empty(),
        "post-commit reasoning stays floating"
    );

    // Re-ingesting the same transcript links nothing new and keeps the links.
    let again = fixture.ingest(&path, &store);
    assert_eq!(again.inserted, 0, "{again:?}");
    assert_eq!(again.linked_from_commits, 0, "{again:?}");
    assert!(links_to(&store, "req_B", &sha));
    assert!(links_to(&store, "req_A", &sha));
}

#[test]
fn an_unresolvable_or_stale_hash_creates_no_link() {
    let fixture = Fixture::new();
    // A real commit exists, but the transcript names a hash that is not it.
    let (_real, commit_time) = fixture.commit("x.rs", "fn x() {}\n", "x");
    let bogus = "0123456789abcdef0123456789abcdef01234567";
    let path = fixture.write_transcript(&transcript(
        fixture.root.to_str().unwrap(),
        bogus,
        commit_time,
    ));
    let store = fixture.store();
    let report = fixture.ingest(&path, &store);
    assert_eq!(report.linked_from_commits, 0, "{report:?}");
    let committing = cvc_core::claude_code::transcript::derived_interaction_id(SESSION, "req_B");
    assert!(store.get_artifact_links(&committing).unwrap().is_empty());
}

#[test]
fn incremental_ingest_links_the_commit_when_its_result_arrives() {
    let fixture = Fixture::new();
    let (sha, commit_time) = fixture.commit("f.rs", "fn f() {}\n", "f");
    let full = transcript(fixture.root.to_str().unwrap(), &sha, commit_time);
    let store = fixture.store();

    // First window: up to and including the git commit tool_use, before its
    // result — as a PostToolUse hook would see mid-turn.
    let cut = full.find("\"uuid\":\"u2\"").unwrap();
    let head = &full[..full[..cut].rfind('\n').unwrap() + 1];
    let path = fixture.write_transcript(head);
    let first = ingest(
        &fixture.layout,
        &store,
        &PreparedPolicy::built_ins_only(),
        &path,
        Some(SESSION),
        IngestMode::Incremental,
    )
    .unwrap();
    // The commit result is not in view yet, so nothing links from a commit.
    assert_eq!(first.linked_from_commits, 0, "{first:?}");

    // Next window: the whole transcript, result now present.
    fixture.write_transcript(&full);
    let second = ingest(
        &fixture.layout,
        &store,
        &PreparedPolicy::built_ins_only(),
        &path,
        Some(SESSION),
        IngestMode::Incremental,
    )
    .unwrap();
    assert!(second.linked_from_commits >= 1, "{second:?}");
    assert!(links_to(&store, "req_B", &sha));

    let _ = InteractionId::new();
}
