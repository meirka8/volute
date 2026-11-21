//! The shared harness pieces with a second adapter (#187).
//!
//! `Fake` is a minimal adapter of the kind a v0.8.0 lane writes: it reads a
//! JSONL record of its own invented shape and stands in for Codex, which has
//! no adapter in this build. The engine records only for a registered
//! adapter, so each test registers `Fake` for Codex on its own thread
//! ([`crate::harness::test_registry`], which exists in this crate's unit
//! tests only). Nothing here is a real agent's format, and nothing is a
//! captured prompt.
//!
//! Codex cannot be acknowledged in this build (it has no notice yet), so a
//! test that needs it covered writes the channel into `privacy.json` itself,
//! as an acknowledgement given by a build that can capture Codex would.
use crate::db::{CvcStore, DbError, RelationEvidence, RelationKind, StoredHarnessCursor};
use crate::harness::adapter::{
    CallerVerdict, HarnessStatus, HookCaller, HookEnv, HookRequest, InstallContext, InstallOutcome,
    InstallScope, Installer, Located, OutputContract, PayloadError, Planner, Read, ReadContext,
    RecordSource, Sniff, StartLocation, UninstallOutcome,
};
use crate::harness::candidate::{
    CaptureCandidate, ChildRef, ConversationRole, Finality, Plan, PlanInput, Reasoning,
    ReasoningKind, SourcePosition, StimulusPart, TitleHint, TitleRank,
};
use crate::harness::cursor::{CursorCodec, JsonlCursor};
use crate::harness::drift::{
    Drift, DriftError, DriftRule, FormatPin, MalformedLine, PlaceholderStyle,
};
use crate::harness::engine::{self, EngineError, EngineOptions, EngineReport};
use crate::harness::{derived_interaction_id, test_registry, HarnessId, PassiveChannel};
use crate::models::Author;
use crate::privacy::{self, CaptureCoverage, PreparedPolicy, PrivacyError};
use crate::reasoning::{self, ReasoningView};
use crate::repository::RepositoryLayout;
use chrono::{DateTime, TimeZone, Utc};
use git2::{Repository, Signature, Time};
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;

// ---- the fake adapter ---------------------------------------------------------------

/// The fake record's format, pinned to major version 1.
const FAKE_DRIFT: DriftRule = DriftRule {
    pin: FormatPin {
        harness: HarnessId::Codex,
        supported_major: &[1],
        verified: &["1.0"],
    },
    placeholders: PlaceholderStyle::Sanitised,
    malformed: MalformedLine::Fail,
};

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Fake;

#[derive(Debug, thiserror::Error)]
pub(super) enum FakeError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("line at byte {offset}: {error}")]
    Drift { offset: u64, error: DriftError },
    #[error("line at byte {0} is not a record")]
    Malformed(u64),
    #[error("not offered")]
    Unsupported,
}

pub(super) struct FakeWindow {
    lines: Vec<(u64, Value)>,
    end: u64,
    from: JsonlCursor,
}

impl HookCaller for Fake {
    fn classify(&self, _payload: &Map<String, Value>, _env: &dyn HookEnv) -> CallerVerdict {
        CallerVerdict::Ours { foreign_env: None }
    }
    fn request(&self, _payload: Map<String, Value>) -> Result<HookRequest, PayloadError> {
        Err(PayloadError::NotTheShape("Fake"))
    }
    fn start(&self, _request: &HookRequest, _env: &dyn HookEnv) -> StartLocation {
        StartLocation::Unknown
    }
    fn wants(&self, _request: &HookRequest) -> bool {
        true
    }
    fn finality(&self, _event: Option<&str>) -> Finality {
        Finality::Final
    }
    fn budget(&self, _event: Option<&str>) -> Duration {
        Duration::from_secs(20)
    }
    fn contract(&self, _event: Option<&str>) -> OutputContract {
        OutputContract::EmptyObject
    }
    fn stop_hint(&self) -> &'static str {
        "(or stop the fake hooks)"
    }
}

impl RecordSource for Fake {
    const ID: HarnessId = HarnessId::Codex;
    type Source = PathBuf;
    type Cursor = JsonlCursor;
    type Window = FakeWindow;
    type Error = FakeError;

    fn locate_hook(
        &self,
        _request: &HookRequest,
        _layout: &RepositoryLayout,
    ) -> Result<Located<PathBuf>, FakeError> {
        Err(FakeError::Unsupported)
    }
    fn locate_file(&self, path: &Path, id: Option<&str>) -> Result<Located<PathBuf>, FakeError> {
        let agent_id = id.ok_or(FakeError::Unsupported)?;
        let conversation_id = Self::ID
            .conversation_id(agent_id)
            .map_err(|_| FakeError::Unsupported)?;
        Ok(Located {
            family_root: conversation_id.clone(),
            conversation_id,
            role: ConversationRole::Root,
            source: path.to_path_buf(),
            title_seed: None,
        })
    }
    fn locate_session(
        &self,
        _id: &str,
        _layout: &RepositoryLayout,
    ) -> Result<Located<PathBuf>, FakeError> {
        Err(FakeError::Unsupported)
    }
    fn scan(&self, _layout: &RepositoryLayout) -> Result<Vec<Located<PathBuf>>, FakeError> {
        Ok(Vec::new())
    }
    fn sniff(&self, _source: &PathBuf) -> Sniff {
        Sniff::Ours
    }
    fn initial_cursor(&self, located: &Located<PathBuf>) -> JsonlCursor {
        JsonlCursor::start(located.source.to_string_lossy())
    }
    fn read(
        &self,
        source: &mut PathBuf,
        from: &JsonlCursor,
        context: &ReadContext<'_>,
    ) -> Result<Read<FakeWindow, JsonlCursor>, FakeError> {
        let mut bytes = Vec::new();
        fs::File::open(&*source)?.read_to_end(&mut bytes)?;
        let start = if context.rescan { 0 } else { from.byte_offset };
        let mut lines = Vec::new();
        let mut offset = start as usize;
        while let Some(newline) = bytes[offset..].iter().position(|b| *b == b'\n') {
            let line = &bytes[offset..offset + newline];
            let value: Value =
                serde_json::from_slice(line).map_err(|_| FakeError::Malformed(offset as u64))?;
            let version = value.get("v").and_then(Value::as_str).unwrap_or("");
            FAKE_DRIFT
                .pin
                .check_major(version)
                .map_err(|error| FakeError::Drift {
                    offset: offset as u64,
                    error,
                })?;
            lines.push((offset as u64, value));
            offset += newline + 1;
        }
        Ok(Read {
            window: FakeWindow {
                lines,
                end: offset as u64,
                from: from.clone(),
            },
            trusted: from.clone(),
            replaced: None,
            record_len: Some(bytes.len() as u64),
        })
    }
}

impl Planner for Fake {
    const DRIFT: DriftRule = FAKE_DRIFT;
    const DEFAULT_TITLE: &'static str = "Fake session";

    fn plan(&self, window: &FakeWindow, input: &PlanInput<'_>) -> Plan<JsonlCursor> {
        let mut drift = Drift::default();
        let mut candidates = Vec::new();
        let mut children = Vec::new();
        let mut title = None;
        let mut first_prompt = None;
        let mut previous = None;
        for (offset, line) in &window.lines {
            let text = |key: &str| line.get(key).and_then(Value::as_str).map(str::to_owned);
            match line.get("type").and_then(Value::as_str) {
                Some("response") => {}
                Some("title") => {
                    title = text("text").map(|text| TitleHint {
                        text,
                        rank: TitleRank::UserSet,
                    });
                    continue;
                }
                other => {
                    drift.skip(other.unwrap_or(""));
                    continue;
                }
            }
            let key = text("key").unwrap();
            let id = derived_interaction_id(input.harness, input.conversation_id, &key);
            let mut response = text("text");
            let mut unrecorded = Vec::new();
            for block in line
                .get("blocks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let block = block.as_str().unwrap_or("");
                if block != "text" {
                    let placeholder = drift.placeholder(Self::DRIFT.placeholders, block);
                    unrecorded.push(PlaceholderStyle::Sanitised.name(block));
                    let joined = [response.unwrap_or_default(), placeholder].join("\n\n");
                    response = Some(joined);
                }
            }
            if let Some(child) = text("child") {
                children.push(ChildRef {
                    agent_id: child,
                    kind: RelationKind::Subagent,
                    spawn_key: Some(key.clone()),
                    spawn_call_id: text("spawn_call"),
                    evidence: RelationEvidence::ParentRecord,
                });
            }
            let prompt = text("prompt");
            if first_prompt.is_none() {
                first_prompt = prompt.clone();
            }
            let timestamp: DateTime<Utc> = text("ts").unwrap().parse().unwrap();
            candidates.push(CaptureCandidate {
                key,
                id: id.clone(),
                parent: previous.replace(id),
                timestamp,
                author: if prompt.is_some() {
                    Author::Human
                } else {
                    Author::System
                },
                stimulus: prompt.into_iter().map(StimulusPart::prompt).collect(),
                model_name: None,
                reasoning: text("summary").map(|text| Reasoning {
                    text,
                    kind: ReasoningKind::ProviderSummary,
                }),
                response,
                tools: Vec::new(),
                context: Vec::new(),
                commit_candidates: text("commit").into_iter().collect(),
                unrecorded,
                start: SourcePosition::Byte(*offset),
            });
        }
        Plan {
            candidates,
            pending: 0,
            resume: window.from.at(window.end),
            title,
            first_prompt,
            children,
            parent: None,
            drift,
        }
    }
}

impl Installer for Fake {
    fn scopes(&self) -> &'static [InstallScope] {
        &[]
    }
    fn install(
        &self,
        _context: &InstallContext<'_>,
        _scope: InstallScope,
        _binary: &Path,
    ) -> anyhow::Result<InstallOutcome> {
        anyhow::bail!("not offered")
    }
    fn uninstall(
        &self,
        _context: &InstallContext<'_>,
        _scope: InstallScope,
    ) -> anyhow::Result<UninstallOutcome> {
        anyhow::bail!("not offered")
    }
    fn status(&self, _context: &InstallContext<'_>) -> anyhow::Result<HarnessStatus> {
        Ok(HarnessStatus::default())
    }
}

// ---- fixture ------------------------------------------------------------------------

/// 2026-01-01T00:00:00Z: the clock of every record and commit here.
const T0: i64 = 1_767_225_600;

fn at(seconds: i64) -> String {
    Utc.timestamp_opt(T0 + seconds, 0)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// How much of the capture acknowledgement a fixture has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ack {
    None,
    /// As 0.7.1 and later record it: the legacy channels only.
    Legacy,
    /// As a build that captures Codex would record it.
    WithCodex,
}

struct Fixture {
    _temp: TempDir,
    layout: RepositoryLayout,
    store: CvcStore,
    policy: PreparedPolicy,
    root: PathBuf,
}

impl Fixture {
    /// A repository with a CVC store, `ack`, and `Fake` registered for Codex
    /// on this test's thread.
    fn new(ack: Ack) -> Self {
        test_registry::register::<Fake>();
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
        let store = CvcStore::open(layout.db_path()).unwrap();
        let policy = PreparedPolicy::load(&root).unwrap();
        let fixture = Self {
            _temp: temp,
            layout,
            store,
            policy,
            root,
        };
        fixture.acknowledge(ack);
        fixture
    }

    /// Records `ack` as the repository's capture acknowledgement.
    fn acknowledge(&self, ack: Ack) {
        let path = self.layout.cvc_dir().join("privacy.json");
        match ack {
            Ack::None => {
                let _ = fs::remove_file(&path);
                return;
            }
            Ack::Legacy | Ack::WithCodex => {
                privacy::acknowledge_capture(self.layout.repository()).unwrap()
            }
        }
        if ack == Ack::WithCodex {
            let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            state["capture_acknowledged_channels"]
                .as_array_mut()
                .unwrap()
                .push(json!("codex"));
            fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        }
    }

    /// Writes the record `name` from `lines`, one JSON object per line.
    fn record(&self, name: &str, lines: &[Value]) -> PathBuf {
        let path = self.root.join(format!("{name}.jsonl"));
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        fs::write(&path, text).unwrap();
        path
    }

    fn options(rescan: bool) -> EngineOptions {
        EngineOptions {
            finality: Finality::Final,
            rescan,
            lock_wait: Duration::from_secs(5),
        }
    }

    fn ingest(
        &self,
        path: &Path,
        agent_id: &str,
    ) -> Result<EngineReport<JsonlCursor>, EngineError<FakeError>> {
        self.ingest_with(path, agent_id, false)
    }

    fn ingest_with(
        &self,
        path: &Path,
        agent_id: &str,
        rescan: bool,
    ) -> Result<EngineReport<JsonlCursor>, EngineError<FakeError>> {
        let located = Fake.locate_file(path, Some(agent_id)).unwrap();
        engine::ingest(
            &Fake,
            &self.layout,
            &self.store,
            &self.policy,
            located,
            Self::options(rescan),
        )
    }

    fn conn(&self) -> Connection {
        Connection::open(self.layout.db_path()).unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.conn().query_row(sql, [], |row| row.get(0)).unwrap()
    }

    /// A commit made at `T0 + seconds`, by the fixture's identity.
    fn commit(&self, seconds: i64) -> String {
        let repo = self.layout.repository();
        let signature = Signature::new(
            "Fixture",
            "fixture@example.invalid",
            &Time::new(T0 + seconds, 0),
        )
        .unwrap();
        let tree = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree).unwrap();
        let parents: Vec<git2::Commit<'_>> = repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parents: Vec<&git2::Commit<'_>> = parents.iter().collect();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "work",
            &tree,
            &parents,
        )
        .unwrap()
        .to_string()
    }

    fn linked(&self, conversation: &str, key: &str) -> bool {
        let id = derived_interaction_id(HarnessId::Codex, conversation, key);
        self.count(&format!(
            "SELECT COUNT(*) FROM artifact_links WHERE interaction_id='{id}'"
        )) > 0
    }
}

fn response(key: &str, seconds: i64) -> Value {
    json!({"v": "1.0", "type": "response", "key": key, "ts": at(seconds), "prompt": "fix the flaky test", "text": "done"})
}

// ---- tests --------------------------------------------------------------------------

/// A whole record through the engine: captures of the fake's own source in
/// its own namespace, its title, placeholders and skipped records reported,
/// a provider summary marked, the typed cursor at the end; and a second run
/// records nothing twice and keeps the cursor where it is.
#[test]
fn a_second_adapter_ingests_through_the_engine() {
    let fixture = Fixture::new(Ack::WithCodex);
    let mut summarised = response("resp:2", 20);
    summarised["summary"] = json!("looked at the retry loop");
    summarised["blocks"] = json!(["text", "mystery", "evil] name"]);
    let path = fixture.record(
        "t1",
        &[
            json!({"v": "1.0", "type": "session_meta"}),
            response("resp:1", 10),
            summarised,
            json!({"v": "1.0", "type": "token_count"}),
        ],
    );

    let report = fixture.ingest(&path, "t1").unwrap();
    let root = &report.root;
    assert_eq!(root.conversation_id, "codex:t1");
    assert_eq!((root.inserted, root.already_present), (2, 0));
    assert_eq!(root.unrecorded, ["mystery", "unknown-type"]);
    assert_eq!(root.drift.placeholders["mystery"], 1);
    assert_eq!(root.drift.placeholders["unknown-type"], 1);
    assert_eq!(root.drift.skipped["session_meta"], 1);
    assert_eq!(root.drift.skipped["token_count"], 1);
    let len = fs::metadata(&path).unwrap().len();
    assert_eq!(root.cursor.byte_offset, len);
    assert_eq!(root.record_len, Some(len));
    assert!(!root.cursor_held);
    assert!(report.members.is_empty());

    let conn = fixture.conn();
    let sources: Vec<(String, String, String)> = conn
        .prepare(
            "SELECT conversation_id, capture_source, model_response FROM interactions ORDER BY timestamp",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(sources
        .iter()
        .all(|(conversation, source, _)| conversation == "codex:t1" && source == "codex"));
    assert_eq!(sources[1].2, "done\n\n[mystery]\n\n[unknown-type]");
    let title: String = conn
        .query_row(
            "SELECT title FROM conversations WHERE id='codex:t1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(title, "fix the flaky test");

    // The provider summary is marked, and reads back as one.
    let id = derived_interaction_id(HarnessId::Codex, "codex:t1", "resp:2");
    let cot: String = conn
        .query_row(
            "SELECT model_cot FROM interactions WHERE id=?1",
            params![id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        reasoning::read(&id, &cot),
        ReasoningView::Verified {
            kind: ReasoningKind::ProviderSummary,
            body: "looked at the retry loop"
        }
    );
    // The chain: the second response's parent is the first.
    let parent: Option<String> = conn
        .query_row(
            "SELECT parent_id FROM interactions WHERE id=?1",
            params![id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        parent,
        Some(derived_interaction_id(HarnessId::Codex, "codex:t1", "resp:1").to_string())
    );
    // No 0.7.x row for a harness other than Claude Code.
    assert!(fixture
        .store
        .harness_ingest_cursor("codex", "codex:t1")
        .unwrap()
        .is_none());

    let again = fixture.ingest(&path, "t1").unwrap();
    assert_eq!((again.root.inserted, again.root.already_present), (0, 0));
    assert_eq!(again.root.cursor, report.root.cursor);
    // Read from the start, everything is already there.
    let rescanned = fixture.ingest_with(&path, "t1", true).unwrap();
    assert_eq!(
        (rescanned.root.inserted, rescanned.root.already_present),
        (0, 2)
    );
    assert_eq!(rescanned.root.cursor, report.root.cursor);
    assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 2);
}

/// The typed cursor the engine stores decodes with the adapter's codec into
/// the cursor it committed.
#[test]
fn the_cursor_codec_round_trips_through_the_store() {
    let fixture = Fixture::new(Ack::WithCodex);
    let path = fixture.record("t1", &[response("resp:1", 10)]);
    let report = fixture.ingest(&path, "t1").unwrap();
    let stored: StoredHarnessCursor = fixture
        .store
        .harness_cursor(HarnessId::Codex, "codex:t1")
        .unwrap()
        .unwrap();
    assert_eq!(
        (stored.kind.as_str(), stored.version, stored.epoch),
        ("jsonl", 1, 0)
    );
    assert_eq!(
        JsonlCursor::decode_stored(&stored).unwrap(),
        report.root.cursor
    );
    assert_eq!(
        stored.state,
        json!({"path": path.to_string_lossy(), "byte_offset": report.root.cursor.byte_offset})
    );
}

/// An unknown major version fails loudly and writes nothing.
#[test]
fn an_unknown_major_version_fails_loudly_and_writes_nothing() {
    let fixture = Fixture::new(Ack::WithCodex);
    let mut later = response("resp:2", 20);
    later["v"] = json!("2.0");
    let path = fixture.record("t1", &[response("resp:1", 10), later]);
    let error = fixture.ingest(&path, "t1").unwrap_err();
    match &error {
        EngineError::Adapter(FakeError::Drift {
            error: DriftError::UnsupportedMajor { found, supported },
            ..
        }) => {
            assert_eq!(found, "2.0");
            assert_eq!(supported.to_vec(), vec![1u64]);
        }
        other => panic!("{other:?}"),
    }
    assert!(fixture
        .store
        .get_conversation("codex:t1")
        .unwrap()
        .is_none());
    assert!(fixture
        .store
        .harness_cursor(HarnessId::Codex, "codex:t1")
        .unwrap()
        .is_none());
    assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 0);
}

/// A commit a family member made links the family's floating responses of
/// the harness's own source, root and recorded subagent alike, and never a
/// row another source recorded in a member's conversation.
#[test]
fn the_family_rescue_claims_only_its_own_harness_rows() {
    let fixture = Fixture::new(Ack::WithCodex);
    // The subagent's own record first.
    let child = fixture.record("c1", &[response("resp:c", 10)]);
    fixture.ingest(&child, "c1").unwrap();
    // A row another source recorded in the subagent's conversation (as a
    // 0.7.x MCP client could), floating, in this worktree, before the commit.
    let worktree = fixture.layout.worktree_origin().unwrap();
    let mcp = "6b1f0c0e-1111-4aaa-8bbb-000000000042";
    fixture
        .conn()
        .execute(
            "INSERT INTO interactions(id,conversation_id,timestamp,author,user_prompt,capture_source,scrubber_version,capture_worktree) VALUES (?1,'codex:c1',?2,'agent','an mcp thought','mcp',1,?3)",
            params![mcp, T0 + 15, worktree],
        )
        .unwrap();
    // The root names the subagent and the commit it made.
    let sha = fixture.commit(100);
    let mut committing = response("resp:r", 30);
    committing["child"] = json!("c1");
    committing["commit"] = json!(sha.clone());
    let root = fixture.record("r1", &[committing]);
    let report = fixture.ingest(&root, "r1").unwrap();

    assert_eq!(report.root.relations_recorded, 1);
    let relation = fixture
        .store
        .conversation_relation("codex:c1")
        .unwrap()
        .unwrap();
    assert_eq!(relation.parent_id, "codex:r1");
    assert_eq!(
        relation.spawn_interaction_id,
        Some(derived_interaction_id(
            HarnessId::Codex,
            "codex:r1",
            "resp:r"
        ))
    );
    assert_eq!((report.linked.root, report.linked.members), (1, 1));
    assert!(fixture.linked("codex:r1", "resp:r"));
    assert!(fixture.linked("codex:c1", "resp:c"));
    assert_eq!(
        fixture.count(&format!(
            "SELECT COUNT(*) FROM artifact_links WHERE interaction_id='{mcp}'"
        )),
        0,
        "another source's row is never claimed"
    );
}

/// Without coverage of the harness's channel nothing is recorded or moved:
/// not with no acknowledgement, and not with one that predates the channel.
#[test]
fn an_uncovered_channel_captures_nothing_and_moves_no_cursor() {
    for (ack, expected) in [
        (Ack::None, CaptureCoverage::NotAcknowledged),
        (
            Ack::Legacy,
            CaptureCoverage::NotCovering {
                channel: PassiveChannel::Harness(HarnessId::Codex),
            },
        ),
    ] {
        let fixture = Fixture::new(ack);
        let path = fixture.record("t1", &[response("resp:1", 10)]);
        match fixture.ingest(&path, "t1") {
            Err(EngineError::Privacy(PrivacyError::NotCovered(coverage))) => {
                assert_eq!(coverage, expected)
            }
            other => panic!("{:?}", other.map(|_| ())),
        }
        assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 0);
        assert_eq!(fixture.count("SELECT COUNT(*) FROM conversations"), 0);
        assert_eq!(fixture.count("SELECT COUNT(*) FROM harness_cursors"), 0);
    }
}

/// The stand-in registration is the test's own: a harness with an adapter
/// in the build cannot be registered again.
#[test]
#[should_panic(expected = "has an adapter in this build")]
fn a_registered_harness_cannot_be_registered_again() {
    assert!(crate::harness::registered_adapter(HarnessId::ClaudeCode).is_some());
    test_registry::register::<crate::claude_code::Adapter>();
}
