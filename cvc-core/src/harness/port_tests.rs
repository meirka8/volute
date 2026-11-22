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
    CallerVerdict, FamilyRead, HarnessStatus, HookCaller, HookEnv, HookRequest, InstallContext,
    InstallOutcome, InstallScope, Installer, Located, OutputContract, PayloadError, Planner, Read,
    ReadContext, ReadLimit, RecordSource, Sniff, StartLocation, UninstallOutcome,
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
use crate::harness::family::Family;
use crate::harness::lock::FamilyLock;
use crate::harness::{
    derived_interaction_id, test_registry, HarnessId, PassiveChannel, SESSION_LOCK_WAIT,
};
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
use std::time::{Duration, Instant};
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
    /// The recorded family, and as members to read in this run the ones
    /// whose `<agent id>.member.jsonl` is beside the root's record. A
    /// `withdraw-coverage` file there names a `privacy.json` whose Codex
    /// coverage is withdrawn now: a test's hook between the root's batch and
    /// the rest of the run.
    fn family(
        &self,
        root: &Located<PathBuf>,
        store: &CvcStore,
    ) -> Result<FamilyRead<PathBuf>, FakeError> {
        let family = Family::from_relations(store, Self::ID, &root.family_root)?;
        let directory = root.source.parent().unwrap();
        if let Ok(privacy_json) = fs::read_to_string(directory.join("withdraw-coverage")) {
            withdraw_codex(Path::new(privacy_json.trim()));
        }
        let members = family
            .members()
            .filter_map(|id| {
                let agent = id.strip_prefix("codex:")?;
                let source = directory.join(format!("{agent}.member.jsonl"));
                source.exists().then(|| Located {
                    conversation_id: id.to_owned(),
                    family_root: root.family_root.clone(),
                    role: ConversationRole::Subagent,
                    source,
                    title_seed: None,
                })
            })
            .collect();
        Ok(FamilyRead { family, members })
    }
    /// Honours a byte limit by whole lines. A `delay-ms` file beside the
    /// record makes every read take that long: a test's slow disk.
    fn read(
        &self,
        source: &mut PathBuf,
        from: &JsonlCursor,
        context: &ReadContext<'_>,
    ) -> Result<Read<FakeWindow, JsonlCursor>, FakeError> {
        if let Ok(delay) = fs::read_to_string(source.parent().unwrap().join("delay-ms")) {
            std::thread::sleep(Duration::from_millis(delay.trim().parse().unwrap()));
        }
        let mut bytes = Vec::new();
        fs::File::open(&*source)?.read_to_end(&mut bytes)?;
        let start = if context.rescan { 0 } else { from.byte_offset };
        let end = context.limit.bytes.map_or(bytes.len(), |limit| {
            bytes.len().min(start as usize + limit as usize)
        });
        let mut lines = Vec::new();
        let mut own_history_start = None;
        let mut offset = start as usize;
        while let Some(newline) = bytes[offset..end].iter().position(|b| *b == b'\n') {
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
            // `{"type":"history_base","inherited":N}`: the record begins with
            // N responses of another conversation's history.
            if value.get("type").and_then(Value::as_str) == Some("history_base") {
                own_history_start = value
                    .get("inherited")
                    .and_then(Value::as_u64)
                    .map(SourcePosition::Ordinal);
            }
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
            limited: bytes[offset..].contains(&b'\n'),
            own_history_start,
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
        let mut ordinal = 0u64;
        for (offset, line) in &window.lines {
            let text = |key: &str| line.get(key).and_then(Value::as_str).map(str::to_owned);
            match line.get("type").and_then(Value::as_str) {
                Some("response") => {
                    ordinal += 1;
                    // Inherited from another conversation: not this one's.
                    if let Some(SourcePosition::Ordinal(inherited)) = input.own_history_start {
                        if ordinal <= *inherited {
                            continue;
                        }
                    }
                }
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

/// Removes Codex from the channels the acknowledgement in `privacy_json`
/// covers.
fn withdraw_codex(privacy_json: &Path) {
    let mut state: Value = serde_json::from_slice(&fs::read(privacy_json).unwrap()).unwrap();
    state["capture_acknowledged_channels"]
        .as_array_mut()
        .unwrap()
        .retain(|channel| channel != "codex");
    fs::write(privacy_json, serde_json::to_vec(&state).unwrap()).unwrap();
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
            lock_wait: Duration::from_secs(5),
            ..EngineOptions::operator(Finality::Final, rescan)
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
        self.ingest_options(path, agent_id, Self::options(rescan))
    }

    fn ingest_options(
        &self,
        path: &Path,
        agent_id: &str,
        options: EngineOptions,
    ) -> Result<EngineReport<JsonlCursor>, EngineError<FakeError>> {
        let located = Fake.locate_file(path, Some(agent_id)).unwrap();
        engine::ingest(
            &Fake,
            &self.layout,
            &self.store,
            &self.policy,
            located,
            options,
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

    /// Codex coverage is withdrawn once the root's batch has committed.
    fn withdraw_after_the_root(&self) {
        fs::write(
            self.root.join("withdraw-coverage"),
            self.layout
                .cvc_dir()
                .join("privacy.json")
                .to_string_lossy()
                .as_bytes(),
        )
        .unwrap();
    }

    /// A row another source recorded in `conversation`, which keeps the fake
    /// from recording into it.
    fn foreign_row(&self, conversation: &str, id: &str, seconds: i64) {
        let worktree = self.layout.worktree_origin().unwrap();
        let conn = self.conn();
        conn.execute(
            "INSERT OR IGNORE INTO conversations(id,title,created_at) VALUES (?1,'t',?2)",
            params![conversation, T0],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO interactions(id,conversation_id,timestamp,author,user_prompt,capture_source,scrubber_version,capture_worktree) VALUES (?1,?2,?3,'agent','an mcp thought','mcp',1,?4)",
            params![id, conversation, T0 + seconds, worktree],
        )
        .unwrap();
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
    let mcp = "6b1f0c0e-1111-4aaa-8bbb-000000000042";
    fixture.foreign_row("codex:c1", mcp, 15);
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

/// A member whose batch fails contributes no commit evidence: its commit
/// links nothing, not even the root's floating reasoning before it.
#[test]
fn a_failing_members_commit_links_nothing() {
    let fixture = Fixture::new(Ack::WithCodex);
    let sha = fixture.commit(100);
    let mut root = response("resp:r", 30);
    root["child"] = json!("c1");
    let root = fixture.record("r1", &[root]);
    let mut committing = response("resp:c", 40);
    committing["commit"] = json!(sha);
    fixture.record("c1.member", &[committing]);
    // The member's conversation holds another source's row, so its batch is
    // refused.
    fixture.foreign_row("codex:c1", "6b1f0c0e-1111-4aaa-8bbb-000000000043", 5);

    let error = fixture.ingest(&root, "r1").unwrap_err();
    assert!(
        matches!(error, EngineError::Db(DbError::HarnessBatch(_))),
        "{error:?}"
    );
    assert!(fixture
        .store
        .interaction_exists(&derived_interaction_id(
            HarnessId::Codex,
            "codex:r1",
            "resp:r"
        ))
        .unwrap());
    assert_eq!(fixture.count("SELECT COUNT(*) FROM artifact_links"), 0);
}

/// Each conversation obtains its grant before its record is read: an
/// uncovered channel reads nothing, so a record that cannot be read is never
/// even tried.
#[test]
fn an_uncovered_channel_reads_nothing() {
    let missing = |fixture: &Fixture| fixture.root.join("absent.jsonl");
    let uncovered = Fixture::new(Ack::Legacy);
    assert!(matches!(
        uncovered.ingest(&missing(&uncovered), "t1"),
        Err(EngineError::Privacy(PrivacyError::NotCovered(_)))
    ));
    // Covered, the same record is read, and its absence is what fails.
    let covered = Fixture::new(Ack::WithCodex);
    assert!(matches!(
        covered.ingest(&missing(&covered), "t1"),
        Err(EngineError::Adapter(FakeError::Io(_)))
    ));
}

/// Coverage withdrawn after the root's batch: the next member reads nothing
/// (its record is a directory, which a read would fail on), the run fails
/// with the coverage it found, and the rescue, which writes links, does not
/// run: the root's commit links nothing.
#[test]
fn a_member_after_coverage_is_withdrawn_reads_nothing_and_nothing_is_linked() {
    let fixture = Fixture::new(Ack::WithCodex);
    let sha = fixture.commit(100);
    let mut root = response("resp:r", 30);
    root["child"] = json!("c1");
    root["commit"] = json!(sha);
    let root = fixture.record("r1", &[root]);
    fs::create_dir(fixture.root.join("c1.member.jsonl")).unwrap();
    fixture.withdraw_after_the_root();

    match fixture.ingest(&root, "r1") {
        Err(EngineError::Privacy(PrivacyError::NotCovered(coverage))) => assert_eq!(
            coverage,
            CaptureCoverage::NotCovering {
                channel: PassiveChannel::Harness(HarnessId::Codex)
            }
        ),
        other => panic!("{:?}", other.map(|_| ())),
    }
    assert!(!fixture.linked("codex:r1", "resp:r"));
    assert_eq!(fixture.count("SELECT COUNT(*) FROM artifact_links"), 0);
    assert!(fixture
        .store
        .harness_cursor(HarnessId::Codex, "codex:c1")
        .unwrap()
        .is_none());
}

/// Coverage that no longer holds when the rescue is due skips the rescue:
/// the root's batch stands, and its commit links nothing. Covered, the same
/// run links it.
#[test]
fn the_rescue_runs_only_while_the_channel_is_covered() {
    let run = |withdraw: bool| {
        let fixture = Fixture::new(Ack::WithCodex);
        let sha = fixture.commit(100);
        let mut root = response("resp:r", 30);
        root["commit"] = json!(sha);
        let root = fixture.record("r1", &[root]);
        if withdraw {
            fixture.withdraw_after_the_root();
        }
        let report = fixture.ingest(&root, "r1").unwrap();
        (fixture, report)
    };
    let (withdrawn, report) = run(true);
    assert!(report.rescue_skipped);
    assert_eq!(report.root.inserted, 1);
    assert_eq!((report.linked.root, report.linked.members), (0, 0));
    assert!(!withdrawn.linked("codex:r1", "resp:r"));

    let (covered, report) = run(false);
    assert!(!report.rescue_skipped);
    assert_eq!(report.linked.root, 1);
    assert!(covered.linked("codex:r1", "resp:r"));
}

/// Relations the store would refuse are checked before the batch: a child
/// that is the conversation itself, or whose id the store cannot hold, is
/// left out and counted, a spawn call id it cannot hold is dropped, and the
/// batch with its other relations commits.
#[test]
fn relations_the_store_would_refuse_are_left_out_not_the_batch() {
    let fixture = Fixture::new(Ack::WithCodex);
    let mut myself = response("resp:1", 10);
    myself["child"] = json!("r1");
    let mut unsafe_child = response("resp:2", 20);
    unsafe_child["child"] = json!("a b");
    let mut odd_call = response("resp:3", 30);
    odd_call["child"] = json!("c1");
    odd_call["spawn_call"] = json!("call with spaces");
    let root = fixture.record("r1", &[myself, unsafe_child, odd_call]);

    let report = fixture.ingest(&root, "r1").unwrap();
    assert_eq!(report.root.inserted, 3);
    assert_eq!(
        (
            report.root.relations_recorded,
            report.root.relations_skipped
        ),
        (1, 2)
    );
    let relation = fixture
        .store
        .conversation_relation("codex:c1")
        .unwrap()
        .unwrap();
    assert_eq!(relation.parent_id, "codex:r1");
    assert_eq!(relation.spawn_call_id, None);
    assert!(fixture
        .store
        .conversation_relation("codex:r1")
        .unwrap()
        .is_none());
}

/// The reader finds where the conversation's own history begins, and the
/// planner gets it: responses inherited from another conversation (a fork,
/// a revert file) are not recorded as this one's.
#[test]
fn inherited_history_is_skipped_from_where_the_reader_says_it_ends() {
    let fixture = Fixture::new(Ack::WithCodex);
    let fork = fixture.record(
        "f1",
        &[
            json!({"v": "1.0", "type": "history_base", "inherited": 2}),
            response("resp:inherited-1", 10),
            response("resp:inherited-2", 20),
            response("resp:own", 30),
        ],
    );
    let report = fixture.ingest(&fork, "f1").unwrap();
    assert_eq!(report.root.inserted, 1);
    let keys: Vec<String> = fixture
        .conn()
        .prepare("SELECT source_request_id FROM interactions")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(keys, ["resp:own"]);
}

// ---- batches and the time budget (#149) ---------------------------------------------

/// A hook's run of `finality` in batches of `bytes`, over by `deadline`.
fn batched(deadline: Option<Instant>, bytes: u64, finality: Finality) -> EngineOptions {
    EngineOptions {
        finality,
        rescan: false,
        lock_wait: SESSION_LOCK_WAIT,
        deadline,
        batch: ReadLimit {
            bytes: Some(bytes),
            units: None,
        },
    }
}

/// Forty responses of the same length, a second apart, and that length.
fn forty(fixture: &Fixture) -> (PathBuf, u64) {
    let lines: Vec<Value> = (0..40)
        .map(|n| response(&format!("resp:{n:02}"), n))
        .collect();
    let line = format!("{}\n", lines[0]).len() as u64;
    (fixture.record("t1", &lines), line)
}

/// Codex's `SessionEnd` has two seconds (its cap is three): a run on a slow
/// record stops between batches before its deadline, with what it read
/// committed, the cursor after it and the backlog flag set, since a final
/// event may be the session's last. The next run goes on from the cursor:
/// nothing twice, nothing lost, and the flag goes.
#[test]
fn a_two_second_budget_stops_between_batches_and_the_next_run_goes_on() {
    let fixture = Fixture::new(Ack::WithCodex);
    let (path, line) = forty(&fixture);
    // Every read takes 150 ms, and a batch reads two responses: twenty
    // batches would take three seconds.
    fs::write(fixture.root.join("delay-ms"), "150").unwrap();
    let started = Instant::now();
    let report = fixture
        .ingest_options(
            &path,
            "t1",
            batched(
                Some(started + Duration::from_secs(2)),
                2 * line + 5,
                Finality::Final,
            ),
        )
        .unwrap();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert!(report.cut_short && report.root.cut_short, "{report:?}");
    let first = report.root.inserted;
    assert!((4..40).contains(&first), "{first}");
    assert_eq!(first % 2, 0, "whole batches only");
    assert_eq!(report.root.cursor.byte_offset, first as u64 * line);
    assert!(report.backlog);
    assert_eq!(
        engine::backlog::<Fake>(&fixture.store).unwrap(),
        ["codex:t1"]
    );

    fs::remove_file(fixture.root.join("delay-ms")).unwrap();
    let rest = fixture.ingest(&path, "t1").unwrap();
    assert_eq!(rest.root.inserted, 40 - first);
    assert_eq!(rest.root.already_present, 0);
    assert!(!rest.cut_short && !rest.backlog, "{rest:?}");
    assert!(engine::backlog::<Fake>(&fixture.store).unwrap().is_empty());
    assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 40);
    assert_eq!(rest.root.cursor.byte_offset, 40 * line);
}

/// A run whose deadline is already past reads nothing. A final one leaves
/// the backlog flag, an incremental one does not, and neither takes the flag
/// away; a run that reads everything does.
#[test]
fn only_a_final_run_out_of_time_sets_the_backlog_flag() {
    let fixture = Fixture::new(Ack::WithCodex);
    let (path, _) = forty(&fixture);
    let past = |finality| batched(Some(Instant::now()), 1024, finality);
    let incremental = fixture
        .ingest_options(&path, "t1", past(Finality::Incremental))
        .unwrap();
    assert!(incremental.cut_short && !incremental.backlog);
    assert_eq!(incremental.root.inserted, 0);
    // Nothing was written: no cursor, no flag.
    assert!(fixture
        .store
        .harness_cursor(HarnessId::Codex, "codex:t1")
        .unwrap()
        .is_none());
    let final_run = fixture
        .ingest_options(&path, "t1", past(Finality::Final))
        .unwrap();
    assert!(final_run.cut_short && final_run.backlog);
    assert_eq!(final_run.root.inserted, 0);
    assert_eq!(
        engine::backlog::<Fake>(&fixture.store).unwrap(),
        ["codex:t1"]
    );
    let again = fixture
        .ingest_options(&path, "t1", past(Finality::Incremental))
        .unwrap();
    assert!(again.backlog, "an incremental run keeps the flag");
    let done = fixture
        .ingest_options(&path, "t1", batched(None, 1024, Finality::Incremental))
        .unwrap();
    assert!(!done.cut_short && !done.backlog);
    assert_eq!(done.root.inserted, 40);
    assert!(engine::backlog::<Fake>(&fixture.store).unwrap().is_empty());
}

/// R9: a response much larger than the window. Each run with a short budget
/// grows the window as far as its time allows and leaves the grown window
/// with the cursor; the next run starts from it, so short runs get past the
/// response. Runs that started again from the base window every time would
/// never get past it.
#[test]
fn a_grown_window_carries_over_to_the_next_run() {
    let fixture = Fixture::new(Ack::WithCodex);
    let mut big = response("resp:big", 2);
    big["text"] = json!("x".repeat(40_000));
    let path = fixture.record("t1", &[response("resp:a", 1), big, response("resp:z", 3)]);
    // Every read takes 60 ms, whatever it reads.
    fs::write(fixture.root.join("delay-ms"), "60").unwrap();
    let base = 512;
    let mut windows = Vec::new();
    let mut runs = 0;
    loop {
        runs += 1;
        assert!(
            runs <= 10,
            "short runs did not get past the response: {windows:?}"
        );
        let report = fixture
            .ingest_options(
                &path,
                "t1",
                batched(
                    Some(Instant::now() + Duration::from_millis(500)),
                    base,
                    Finality::Incremental,
                ),
            )
            .unwrap();
        let carried = report.root.cursor.carry.stall_window;
        if !report.cut_short {
            assert_eq!(carried, None);
            break;
        }
        let grown = carried.expect("a run cut short while the window grew keeps it");
        assert!(grown > base, "{grown}");
        // Never back to the base window, and further with every run.
        if let Some(last) = windows.last() {
            assert!(grown > *last, "{windows:?} then {grown}");
        }
        windows.push(grown);
    }
    assert!(!windows.is_empty(), "one run read it all");
    assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 3);
}

/// Codex's `SessionEnd` waits a quarter of a second for a lock another run
/// holds, then ends as busy, which a hook takes for success: the holder is
/// making progress.
#[test]
fn a_two_second_budget_waits_a_quarter_second_for_the_lock() {
    let fixture = Fixture::new(Ack::WithCodex);
    let (path, _) = forty(&fixture);
    let _held = FamilyLock::acquire(
        &fixture.layout.cvc_dir(),
        HarnessId::Codex.name(),
        "codex:t1",
        Duration::ZERO,
    )
    .unwrap()
    .unwrap();
    let started = Instant::now();
    let result = fixture.ingest_options(
        &path,
        "t1",
        batched(
            Some(started + Duration::from_secs(2)),
            1024,
            Finality::Final,
        ),
    );
    let took = started.elapsed();
    match result {
        Err(EngineError::Busy { waited, .. }) => assert!(
            waited <= Duration::from_millis(250) && waited >= Duration::from_millis(240),
            "{waited:?}"
        ),
        other => panic!("{:?}", other.map(|_| ())),
    }
    assert!(
        took >= Duration::from_millis(240) && took < Duration::from_secs(1),
        "{took:?}"
    );
    assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 0);
}

/// A run that fails between batches, as one killed there would, keeps every
/// batch before the failure, with the commits their responses reported. The
/// next run goes on from the cursor and never reads the committing response
/// again, yet its rescue links the responses before the commit from the
/// stored evidence, as one run of the whole record does.
#[test]
fn a_run_that_fails_between_batches_loses_nothing_and_the_next_run_links_its_commit() {
    let fixture = Fixture::new(Ack::WithCodex);
    let sha = fixture.commit(100);
    let mut lines: Vec<Value> = (0..10)
        .map(|n| response(&format!("resp:{n:02}"), 10 * n))
        .collect();
    lines[1]["commit"] = json!(sha);
    let path = fixture.record("t1", &lines);
    let text = fs::read_to_string(&path).unwrap();
    // The seventh line cannot be read until it is repaired.
    let broken: String = text
        .lines()
        .enumerate()
        .map(|(index, line)| match index {
            6 => format!("{}\n", "x".repeat(line.len())),
            _ => format!("{line}\n"),
        })
        .collect();
    fs::write(&path, &broken).unwrap();
    let error = fixture
        .ingest_options(&path, "t1", batched(None, 400, Finality::Final))
        .unwrap_err();
    assert!(
        matches!(error, EngineError::Adapter(FakeError::Malformed(_))),
        "{error:?}"
    );
    let first = fixture.count("SELECT COUNT(*) FROM interactions");
    assert!((2..=6).contains(&first), "{first}");
    assert_eq!(
        fixture
            .store
            .harness_commit_evidence(HarnessId::Codex, "codex:t1")
            .unwrap()
            .len(),
        1
    );
    // Like a killed run, it linked nothing.
    assert_eq!(fixture.count("SELECT COUNT(*) FROM artifact_links"), 0);

    fs::write(&path, &text).unwrap();
    let report = fixture
        .ingest_options(&path, "t1", batched(None, 400, Finality::Final))
        .unwrap();
    assert_eq!(report.root.inserted as i64, 10 - first);
    assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 10);
    assert_eq!(report.linked.root, 10);
    for n in 0..10 {
        assert!(fixture.linked("codex:t1", &format!("resp:{n:02}")), "{n}");
    }
}

/// A commit replayed from the store links only responses within the link
/// window before it, and none when the window is 0; the commits a run reads
/// itself are not bounded, as before.
#[test]
fn a_replayed_commit_links_only_within_the_link_window() {
    let fixture = Fixture::new(Ack::WithCodex);
    let sha = fixture.commit(100_000);
    let set_window = |seconds: &str| {
        fixture
            .layout
            .repository()
            .config()
            .unwrap()
            .set_str("cvc.linkWindow", seconds)
            .unwrap()
    };
    set_window("3600");
    let mut committing = response("resp:c", 99_990);
    committing["commit"] = json!(sha);
    let old = response("resp:old", 100_000 - 4000);
    let path = fixture.record("t1", &[old.clone(), committing.clone()]);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!(report.linked.root, 2, "its own run's commit is not bounded");

    // Responses recorded after the committing one was read, which the next
    // run never reads again: only the one in the window before the commit
    // is linked, from the stored evidence.
    let mut lines = vec![
        old,
        committing,
        response("resp:older", 100_000 - 5000),
        response("resp:near", 100_000 - 600),
        response("resp:after", 100_010),
    ];
    let path = fixture.record("t1", &lines);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!(report.root.inserted, 3);
    assert_eq!(report.linked.root, 1);
    assert!(fixture.linked("codex:t1", "resp:near"));
    assert!(!fixture.linked("codex:t1", "resp:older"));
    assert!(!fixture.linked("codex:t1", "resp:after"));

    // At 0, the link window links nothing from the store at all.
    set_window("0");
    lines.push(response("resp:late", 100_000 - 60));
    let path = fixture.record("t1", &lines);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!((report.root.inserted, report.linked.root), (1, 0));
    assert!(!fixture.linked("codex:t1", "resp:late"));
}
