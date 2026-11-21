//! The harness registry with a second adapter (#187).
//!
//! `Fake` is a minimal adapter of the kind a v0.8.0 lane writes: it reads a
//! JSONL record of its own invented shape and claims `HarnessId::Codex`,
//! which has no adapter in this build, so the shared pieces can be exercised
//! with a harness that is not Claude Code: the engine, the cursor codec, the
//! drift rule, the family rescue and the consent coverage. Nothing here is a
//! real agent's format, and nothing is a captured prompt.
//!
//! Codex cannot be acknowledged in this build (it has no notice yet), so a
//! test that needs it covered writes the channel into `privacy.json` itself,
//! as an acknowledgement given by a build that can capture Codex would.
use chrono::{DateTime, TimeZone, Utc};
use cvc_core::claude_code;
use cvc_core::db::{CvcStore, DbError, StoredHarnessCursor};
use cvc_core::harness::adapter::{
    CallerVerdict, HarnessStatus, HookCaller, HookEnv, HookRequest, InstallContext, InstallOutcome,
    InstallScope, Installer, Located, OutputContract, PayloadError, Planner, Read, ReadContext,
    RecordSource, Sniff, StartLocation, UninstallOutcome,
};
use cvc_core::harness::candidate::{
    CaptureCandidate, ChildRef, ConversationRole, Finality, Plan, PlanInput, Reasoning,
    ReasoningKind, SourcePosition, StimulusPart, TitleHint, TitleRank,
};
use cvc_core::harness::cursor::{
    Carry, CursorCodec, CursorError, JsonlCursor, Progress, SeqCursor, SpoolCursor, SqliteWatermark,
};
use cvc_core::harness::drift::{
    Drift, DriftError, DriftRule, FormatPin, MalformedLine, PlaceholderStyle,
};
use cvc_core::harness::engine::{self, EngineError, EngineOptions, EngineReport};
use cvc_core::harness::{
    derived_interaction_id, with_adapter, AdapterVisitor, HarnessAdapter, HarnessId, PassiveChannel,
};
use cvc_core::models::Author;
use cvc_core::privacy::{self, CaptureCoverage, PreparedPolicy, PrivacyError};
use cvc_core::reasoning::{self, ReasoningView};
use cvc_core::repository::RepositoryLayout;
use git2::{Repository, Signature, Time};
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use std::ffi::OsString;
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

/// A fake adapter. `CLAUDE` makes it claim Claude Code instead, which the
/// engine must refuse: Claude Code's registered adapter is another.
#[derive(Debug, Default, Clone, Copy)]
struct Fake<const CLAUDE: bool>;

type Codex = Fake<false>;

#[derive(Debug, thiserror::Error)]
enum FakeError {
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

struct FakeWindow {
    lines: Vec<(u64, Value)>,
    end: u64,
    from: JsonlCursor,
}

impl<const CLAUDE: bool> HookCaller for Fake<CLAUDE> {
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

impl<const CLAUDE: bool> RecordSource for Fake<CLAUDE> {
    const ID: HarnessId = if CLAUDE {
        HarnessId::ClaudeCode
    } else {
        HarnessId::Codex
    };
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

impl<const CLAUDE: bool> Planner for Fake<CLAUDE> {
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
                    kind: cvc_core::db::RelationKind::Subagent,
                    spawn_key: Some(key.clone()),
                    spawn_call_id: None,
                    evidence: cvc_core::db::RelationEvidence::ParentRecord,
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

impl<const CLAUDE: bool> Installer for Fake<CLAUDE> {
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

struct Fixture {
    _temp: TempDir,
    layout: RepositoryLayout,
    store: CvcStore,
    policy: PreparedPolicy,
    root: PathBuf,
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

impl Fixture {
    fn new(ack: Ack) -> Self {
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
        if ack != Ack::None {
            privacy::acknowledge_capture(layout.repository()).unwrap();
        }
        if ack == Ack::WithCodex {
            let path = layout.cvc_dir().join("privacy.json");
            let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            state["capture_acknowledged_channels"]
                .as_array_mut()
                .unwrap()
                .push(json!("codex"));
            fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        }
        Self {
            _temp: temp,
            layout,
            store,
            policy,
            root,
        }
    }

    /// Writes the record `name` from `lines`, one JSON object per line.
    fn record(&self, name: &str, lines: &[Value]) -> PathBuf {
        let path = self.root.join(format!("{name}.jsonl"));
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        fs::write(&path, text).unwrap();
        path
    }

    fn ingest<const C: bool>(
        &self,
        path: &Path,
        agent_id: &str,
    ) -> Result<EngineReport<JsonlCursor>, EngineError<FakeError>> {
        let adapter = Fake::<C>;
        let located = adapter.locate_file(path, Some(agent_id)).unwrap();
        engine::ingest(
            &adapter,
            &self.layout,
            &self.store,
            &self.policy,
            located,
            EngineOptions {
                finality: Finality::Final,
                rescan: false,
                lock_wait: Duration::from_secs(5),
            },
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

    let report = fixture.ingest::<false>(&path, "t1").unwrap();
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
        .prepare("SELECT conversation_id, capture_source, model_response FROM interactions ORDER BY timestamp")
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

    let again = fixture.ingest::<false>(&path, "t1").unwrap();
    assert_eq!((again.root.inserted, again.root.already_present), (0, 0));
    assert_eq!(again.root.cursor, report.root.cursor);
    // Read from the start, everything is already there.
    let adapter = Codex::default();
    let located = adapter.locate_file(&path, Some("t1")).unwrap();
    let rescanned = engine::ingest(
        &adapter,
        &fixture.layout,
        &fixture.store,
        &fixture.policy,
        located,
        EngineOptions {
            finality: Finality::Final,
            rescan: true,
            lock_wait: Duration::from_secs(5),
        },
    )
    .unwrap();
    assert_eq!(
        (rescanned.root.inserted, rescanned.root.already_present),
        (0, 2)
    );
    assert_eq!(rescanned.root.cursor, report.root.cursor);
    assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 2);
}

/// The typed cursor the engine stores decodes with the adapter's codec into
/// the cursor it committed; every codec round-trips and refuses what it did
/// not write.
#[test]
fn the_cursor_codec_round_trips_through_the_store() {
    let fixture = Fixture::new(Ack::WithCodex);
    let path = fixture.record("t1", &[response("resp:1", 10)]);
    let report = fixture.ingest::<false>(&path, "t1").unwrap();
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

    let jsonl = JsonlCursor {
        last_ordinal: Some(88),
        carry: Carry {
            backlog: true,
            ..Carry::default()
        },
        ..JsonlCursor::start("/r.jsonl").at(9)
    };
    assert_eq!(JsonlCursor::decode(1, 0, &jsonl.encode()).unwrap(), jsonl);
    assert_eq!(jsonl.describe(), "ordinal 88");
    let watermark = SqliteWatermark {
        db_id: "d".into(),
        time_updated_ms: 1,
        last_id: "ses_1".into(),
        overlap_ms: 0,
    };
    assert_eq!(
        SqliteWatermark::decode(1, 0, &watermark.encode()).unwrap(),
        watermark
    );
    let seq = SeqCursor {
        db_id: "d".into(),
        seq: 4,
    };
    assert_eq!(SeqCursor::decode(1, 0, &seq.encode()).unwrap(), seq);
    let spool = SpoolCursor {
        assembled_through: 2,
    };
    assert_eq!(SpoolCursor::decode(1, 0, &spool.encode()).unwrap(), spool);
    assert_eq!(spool.progress(&spool), Progress::Same);
    // What another codec wrote is not read.
    assert!(matches!(
        SpoolCursor::decode(1, 0, &seq.encode()),
        Err(CursorError::State(_))
    ));
    assert_eq!(
        SeqCursor::decode(2, 0, &seq.encode()),
        Err(CursorError::Version(2))
    );
}

/// An unknown major version fails loudly and writes nothing; unfamiliar
/// blocks are placeholders that name their type (sanitised), never their
/// content. Claude Code's instance of the rule is its 0.7.x behaviour.
#[test]
fn the_drift_rule_fails_loudly_on_a_major_version_and_names_what_it_keeps() {
    let fixture = Fixture::new(Ack::WithCodex);
    let mut later = response("resp:2", 20);
    later["v"] = json!("2.0");
    let path = fixture.record("t1", &[response("resp:1", 10), later]);
    let error = fixture.ingest::<false>(&path, "t1").unwrap_err();
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

    // Claude Code's instance: major version 2, placeholders as 0.7.x spelled
    // them, and a line that is not an entry fails the read.
    let rule = claude_code::transcript::DRIFT;
    assert_eq!(rule.pin.harness, HarnessId::ClaudeCode);
    assert_eq!(rule.pin.check_major("2.1.300"), Ok(()));
    assert!(matches!(
        rule.pin.check_major("3.0.0"),
        Err(DriftError::UnsupportedMajor { .. })
    ));
    assert_eq!(rule.placeholders, PlaceholderStyle::Verbatim);
    assert_eq!(rule.placeholders.placeholder("a b"), "[a b]");
    assert_eq!(rule.malformed, MalformedLine::Fail);
    assert_eq!(
        <claude_code::Adapter as Planner>::DRIFT,
        claude_code::transcript::DRIFT
    );
}

/// A commit a family member made links the family's floating responses of
/// the harness's own source, root and recorded subagent alike, and never a
/// row another source recorded in a member's conversation.
#[test]
fn the_family_rescue_claims_only_its_own_harness_rows() {
    let fixture = Fixture::new(Ack::WithCodex);
    // The subagent's own record first.
    let child = fixture.record("c1", &[response("resp:c", 10)]);
    fixture.ingest::<false>(&child, "c1").unwrap();
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
    let report = fixture.ingest::<false>(&root, "r1").unwrap();

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

/// Without coverage of the harness's channel nothing is read, recorded or
/// moved: not with no acknowledgement, and not with one that predates the
/// channel.
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
        match fixture.ingest::<false>(&path, "t1") {
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

/// An adapter that claims a harness whose registered adapter is another
/// records nothing, however covered the channel is.
#[test]
fn an_adapter_that_is_not_the_registered_one_records_nothing() {
    let fixture = Fixture::new(Ack::WithCodex);
    let path = fixture.record("t1", &[response("resp:1", 10)]);
    assert!(matches!(
        fixture.ingest::<true>(&path, "t1"),
        Err(EngineError::Unregistered(HarnessId::ClaudeCode))
    ));
    assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 0);
}

struct Name;
impl AdapterVisitor for Name {
    type Output = (HarnessId, &'static str);
    fn visit<A: HarnessAdapter>(self, _adapter: &A) -> Self::Output {
        (A::ID, A::DEFAULT_TITLE)
    }
}

#[test]
fn the_registry_has_claude_code_and_knows_the_others_by_name() {
    assert!(HarnessId::ClaudeCode.available());
    assert_eq!(
        with_adapter(HarnessId::ClaudeCode, Name),
        Some((HarnessId::ClaudeCode, "Claude Code session"))
    );
    for harness in [
        HarnessId::Codex,
        HarnessId::OpenCode,
        HarnessId::Cursor,
        HarnessId::Antigravity,
    ] {
        assert!(!harness.available(), "{harness}");
        assert_eq!(with_adapter(harness, Name), None);
    }
}

const BASIC: &str = include_str!("fixtures/claude-code/v2.1.260/basic.jsonl");
const BASIC_SESSION: &str = "7c1e9f5a-2b2f-4f4e-9c3b-0f2e7a1d5b60";

/// Claude Code through the gated engine path records what its
/// 0.7.x-compatible entry point records, and only when covered.
#[test]
fn claude_code_on_the_gated_path_records_what_the_compatible_path_records() {
    let run = |ack: Ack| {
        let fixture = Fixture::new(ack);
        let path = fixture.root.join(format!("{BASIC_SESSION}.jsonl"));
        fs::write(
            &path,
            BASIC.replace("__WORKTREE__", fixture.root.to_str().unwrap()),
        )
        .unwrap();
        (fixture, path)
    };

    let (compatible, path) = run(Ack::None);
    let expected = claude_code::ingest(
        &compatible.layout,
        &compatible.store,
        &compatible.policy,
        &path,
        None,
        claude_code::IngestMode::Final,
    )
    .unwrap();

    let (gated, path) = run(Ack::Legacy);
    let adapter = claude_code::Adapter;
    let located = adapter.locate_file(&path, None).unwrap();
    let report = engine::ingest(
        &adapter,
        &gated.layout,
        &gated.store,
        &gated.policy,
        located,
        EngineOptions {
            finality: Finality::Final,
            rescan: false,
            lock_wait: Duration::from_secs(5),
        },
    )
    .unwrap();
    assert_eq!(claude_code::IngestReport::from(report), expected);
    let ids = |fixture: &Fixture| -> Vec<(String, String)> {
        fixture
            .conn()
            .prepare("SELECT id, capture_source FROM interactions ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(ids(&gated), ids(&compatible));
    assert_eq!(
        gated
            .store
            .harness_ingest_cursor("claude-code", BASIC_SESSION)
            .unwrap()
            .unwrap()
            .byte_offset,
        expected.cursor
    );

    // Uncovered: the gated path reads nothing and records nothing.
    let (uncovered, path) = run(Ack::None);
    let located = adapter.locate_file(&path, None).unwrap();
    let error = engine::ingest(
        &adapter,
        &uncovered.layout,
        &uncovered.store,
        &uncovered.policy,
        located,
        EngineOptions {
            finality: Finality::Final,
            rescan: false,
            lock_wait: Duration::from_secs(5),
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            claude_code::IngestError::from(error),
            claude_code::IngestError::Privacy(PrivacyError::NotCovered(
                CaptureCoverage::NotAcknowledged
            ))
        ),
        "uncovered"
    );
    assert_eq!(uncovered.count("SELECT COUNT(*) FROM interactions"), 0);
    assert!(uncovered
        .store
        .harness_ingest_cursor("claude-code", BASIC_SESSION)
        .unwrap()
        .is_none());
}

/// The environment-free parts of the fake's hook contract are reachable
/// through the trait, as the hook shell (#187, PR 3) will reach them.
#[test]
fn hook_callers_are_pure_over_an_injected_environment() {
    let env: std::collections::HashMap<String, OsString> = std::collections::HashMap::new();
    let fake = Codex::default();
    assert_eq!(
        fake.classify(&Map::new(), &env),
        CallerVerdict::Ours { foreign_env: None }
    );
    assert_eq!(fake.contract(None), OutputContract::EmptyObject);
    let claude = claude_code::Adapter;
    assert_eq!(claude.contract(Some("Stop")), OutputContract::Silent);
    assert!(claude
        .stop_hint()
        .contains("cvc harness uninstall claude-code"));
}

/// Claude Code's typed cursor follows its 0.7.x row and never moves back
/// within an epoch: a transcript cut short, or a 0.7.x row an older binary
/// moved back, opens the next epoch, and the 0.7.x row is written as 0.7.x
/// writes it.
#[test]
fn claude_codes_typed_cursor_opens_an_epoch_instead_of_moving_back() {
    let fixture = Fixture::new(Ack::None);
    let path = fixture.root.join(format!("{BASIC_SESSION}.jsonl"));
    let full = BASIC.replace("__WORKTREE__", fixture.root.to_str().unwrap());
    fs::write(&path, &full).unwrap();
    let ingest = || {
        claude_code::ingest(
            &fixture.layout,
            &fixture.store,
            &fixture.policy,
            &path,
            None,
            claude_code::IngestMode::Final,
        )
        .unwrap()
    };
    let cursors = || {
        let legacy = fixture
            .store
            .harness_ingest_cursor("claude-code", BASIC_SESSION)
            .unwrap()
            .unwrap()
            .byte_offset;
        let stored = fixture
            .store
            .harness_cursor(HarnessId::ClaudeCode, BASIC_SESSION)
            .unwrap()
            .unwrap();
        (legacy, JsonlCursor::decode_stored(&stored).unwrap())
    };

    let first = ingest();
    let (legacy, typed) = cursors();
    assert_eq!(
        (legacy, typed.byte_offset, typed.epoch),
        (first.cursor, first.cursor, 0)
    );
    assert_eq!(typed.epoch_reason, None);

    // Cut short: the cursor no longer fits, and the transcript is read again.
    let cut = full
        .lines()
        .take(2)
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    fs::write(&path, cut).unwrap();
    let second = ingest();
    assert!(second.cursor < first.cursor);
    let (legacy, typed) = cursors();
    assert_eq!(
        (legacy, typed.byte_offset, typed.epoch),
        (second.cursor, second.cursor, 1)
    );
    assert_eq!(
        typed.epoch_reason,
        Some(cvc_core::harness::cursor::EpochReason::Rewritten)
    );

    // The full transcript again, read forward in the same epoch.
    fs::write(&path, &full).unwrap();
    let third = ingest();
    let (legacy, typed) = cursors();
    assert_eq!(
        (legacy, typed.byte_offset, typed.epoch),
        (third.cursor, third.cursor, 1)
    );

    // An older binary moved its row back: the typed copy follows in the next
    // epoch, never behind within this one.
    fixture
        .conn()
        .execute(
            "UPDATE harness_ingest_cursors SET byte_offset=0 WHERE session_id=?1",
            params![BASIC_SESSION],
        )
        .unwrap();
    let fourth = ingest();
    let (legacy, typed) = cursors();
    assert_eq!(
        (legacy, typed.byte_offset, typed.epoch),
        (fourth.cursor, fourth.cursor, 2)
    );
    assert_eq!(fourth.inserted, 0, "nothing is recorded twice");
}
