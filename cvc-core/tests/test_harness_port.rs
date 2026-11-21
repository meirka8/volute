//! The harness registry and Claude Code's port onto it (#187), from outside
//! the crate.
//!
//! The engine records only for the adapter registered for its harness, so an
//! adapter written outside `cvc-core` records nothing, whichever harness it
//! names: `Outsider` shows that. The engine tests with a recording second
//! adapter live inside the crate (`harness::port_tests`), where a stand-in
//! can be registered for a harness that has no adapter in the build.
//!
//! Everything else here is Claude Code on the registry: its gated path, its
//! cursor rules and its family, through the public API only. Nothing here is
//! a captured prompt.
use chrono::{TimeZone, Utc};
use cvc_core::claude_code;
use cvc_core::db::{CvcStore, DbError};
use cvc_core::harness::adapter::{
    CallerVerdict, HarnessStatus, HookCaller, HookEnv, HookRequest, InstallContext, InstallOutcome,
    InstallScope, Installer, Located, OutputContract, PayloadError, Planner, Read, ReadContext,
    RecordSource, Sniff, StartLocation, UninstallOutcome,
};
use cvc_core::harness::candidate::{
    CaptureCandidate, ConversationRole, Finality, Plan, PlanInput, SourcePosition, StimulusPart,
};
use cvc_core::harness::cursor::{
    Carry, CursorCodec, CursorError, JsonlCursor, Progress, SeqCursor, SpoolCursor, SqliteWatermark,
};
use cvc_core::harness::drift::{
    Drift, DriftError, DriftRule, FormatPin, MalformedLine, PlaceholderStyle,
};
use cvc_core::harness::engine::{self, EngineError, EngineOptions};
use cvc_core::harness::{
    derived_interaction_id, with_adapter, AdapterVisitor, HarnessAdapter, HarnessId,
};
use cvc_core::models::Author;
use cvc_core::privacy::{self, CaptureCoverage, PreparedPolicy, PrivacyError};
use cvc_core::repository::RepositoryLayout;
use git2::Repository;
use rusqlite::{params, Connection};
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;

// ---- an adapter from outside the crate ------------------------------------------------

/// An adapter written outside `cvc-core`: it would record one response. It
/// claims Codex (no adapter in this build) or, with `CLAUDE`, Claude Code
/// (whose registered adapter is another).
#[derive(Debug, Default, Clone, Copy)]
struct Outsider<const CLAUDE: bool>;

#[derive(Debug, thiserror::Error)]
enum OutsiderError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("not offered")]
    Unsupported,
}

impl<const CLAUDE: bool> HookCaller for Outsider<CLAUDE> {
    fn classify(&self, _payload: &Map<String, Value>, _env: &dyn HookEnv) -> CallerVerdict {
        CallerVerdict::Ours { foreign_env: None }
    }
    fn request(&self, _payload: Map<String, Value>) -> Result<HookRequest, PayloadError> {
        Err(PayloadError::NotTheShape("Outsider"))
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
        "(or stop the outsider's hooks)"
    }
}

impl<const CLAUDE: bool> RecordSource for Outsider<CLAUDE> {
    const ID: HarnessId = if CLAUDE {
        HarnessId::ClaudeCode
    } else {
        HarnessId::Codex
    };
    type Source = PathBuf;
    type Cursor = JsonlCursor;
    type Window = JsonlCursor;
    type Error = OutsiderError;

    fn locate_hook(
        &self,
        _request: &HookRequest,
        _layout: &RepositoryLayout,
    ) -> Result<Located<PathBuf>, OutsiderError> {
        Err(OutsiderError::Unsupported)
    }
    fn locate_file(
        &self,
        path: &Path,
        id: Option<&str>,
    ) -> Result<Located<PathBuf>, OutsiderError> {
        let conversation_id = Self::ID
            .conversation_id(id.ok_or(OutsiderError::Unsupported)?)
            .map_err(|_| OutsiderError::Unsupported)?;
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
    ) -> Result<Located<PathBuf>, OutsiderError> {
        Err(OutsiderError::Unsupported)
    }
    fn scan(&self, _layout: &RepositoryLayout) -> Result<Vec<Located<PathBuf>>, OutsiderError> {
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
        _source: &mut PathBuf,
        from: &JsonlCursor,
        _context: &ReadContext<'_>,
    ) -> Result<Read<JsonlCursor, JsonlCursor>, OutsiderError> {
        Ok(Read {
            window: from.clone(),
            trusted: from.clone(),
            replaced: None,
            record_len: None,
        })
    }
}

impl<const CLAUDE: bool> Planner for Outsider<CLAUDE> {
    const DRIFT: DriftRule = DriftRule {
        pin: FormatPin {
            harness: HarnessId::Codex,
            supported_major: &[1],
            verified: &[],
        },
        placeholders: PlaceholderStyle::Sanitised,
        malformed: MalformedLine::Fail,
    };
    const DEFAULT_TITLE: &'static str = "Outsider session";

    fn plan(&self, window: &JsonlCursor, input: &PlanInput<'_>) -> Plan<JsonlCursor> {
        Plan {
            candidates: vec![CaptureCandidate {
                key: "resp:1".into(),
                id: derived_interaction_id(input.harness, input.conversation_id, "resp:1"),
                parent: None,
                timestamp: Utc.timestamp_opt(1_767_225_600, 0).unwrap(),
                author: Author::Human,
                stimulus: vec![StimulusPart::prompt("fix the flaky test")],
                model_name: None,
                reasoning: None,
                response: Some("done".into()),
                tools: Vec::new(),
                context: Vec::new(),
                commit_candidates: Vec::new(),
                unrecorded: Vec::new(),
                start: SourcePosition::Byte(0),
            }],
            pending: 0,
            resume: window.at(1),
            title: None,
            first_prompt: None,
            children: Vec::new(),
            parent: None,
            drift: Drift::default(),
        }
    }
}

impl<const CLAUDE: bool> Installer for Outsider<CLAUDE> {
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
        Repository::init(temp.path()).unwrap();
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
                .push(Value::from("codex"));
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

    fn conn(&self) -> Connection {
        Connection::open(self.layout.db_path()).unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.conn().query_row(sql, [], |row| row.get(0)).unwrap()
    }

    /// The v2.1.260 fixture as the session's transcript in this worktree.
    fn basic_transcript(&self) -> PathBuf {
        let path = self.root.join(format!("{BASIC_SESSION}.jsonl"));
        fs::write(
            &path,
            BASIC.replace("__WORKTREE__", self.root.to_str().unwrap()),
        )
        .unwrap();
        path
    }

    fn ingest_claude_code(&self, path: &Path) -> claude_code::IngestReport {
        claude_code::ingest(
            &self.layout,
            &self.store,
            &self.policy,
            path,
            None,
            claude_code::IngestMode::Final,
        )
        .unwrap()
    }
}

fn options() -> EngineOptions {
    EngineOptions {
        finality: Finality::Final,
        rescan: false,
        lock_wait: Duration::from_secs(5),
    }
}

const BASIC: &str = include_str!("fixtures/claude-code/v2.1.260/basic.jsonl");
const BASIC_SESSION: &str = "7c1e9f5a-2b2f-4f4e-9c3b-0f2e7a1d5b60";

// ---- tests --------------------------------------------------------------------------

/// An adapter from outside the crate records nothing, whichever harness it
/// names and however covered the channel is: neither a harness without an
/// adapter in the build, nor one whose registered adapter is another.
#[test]
fn an_adapter_that_is_not_the_registered_one_records_nothing() {
    let fixture = Fixture::new(Ack::WithCodex);
    let path = fixture.root.join("t1.jsonl");
    fs::write(&path, "").unwrap();
    let codex = Outsider::<false>;
    let located = codex.locate_file(&path, Some("t1")).unwrap();
    assert!(matches!(
        engine::ingest(
            &codex,
            &fixture.layout,
            &fixture.store,
            &fixture.policy,
            located,
            options()
        ),
        Err(EngineError::Unregistered(HarnessId::Codex))
    ));
    let claude = Outsider::<true>;
    let located = claude.locate_file(&path, Some("t1")).unwrap();
    assert!(matches!(
        engine::ingest(
            &claude,
            &fixture.layout,
            &fixture.store,
            &fixture.policy,
            located,
            options()
        ),
        Err(EngineError::Unregistered(HarnessId::ClaudeCode))
    ));
    assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 0);
    assert_eq!(fixture.count("SELECT COUNT(*) FROM harness_cursors"), 0);
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

/// Every codec round-trips and refuses what it did not write.
#[test]
fn every_cursor_codec_round_trips() {
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

/// Claude Code's instance of the drift rule is its 0.7.x behaviour.
#[test]
fn claude_codes_drift_rule_is_its_0_7_behaviour() {
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

/// Claude Code through the gated engine path records what its
/// 0.7.x-compatible entry point records, and only when covered.
#[test]
fn claude_code_on_the_gated_path_records_what_the_compatible_path_records() {
    let compatible = Fixture::new(Ack::None);
    let expected = compatible.ingest_claude_code(&compatible.basic_transcript());

    let gated = Fixture::new(Ack::Legacy);
    let adapter = claude_code::Adapter;
    let located = adapter
        .locate_file(&gated.basic_transcript(), None)
        .unwrap();
    let report = engine::ingest(
        &adapter,
        &gated.layout,
        &gated.store,
        &gated.policy,
        located,
        options(),
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
    let uncovered = Fixture::new(Ack::None);
    let located = adapter
        .locate_file(&uncovered.basic_transcript(), None)
        .unwrap();
    let error = engine::ingest(
        &adapter,
        &uncovered.layout,
        &uncovered.store,
        &uncovered.policy,
        located,
        options(),
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

/// The environment-free parts of a hook contract are reachable through the
/// trait, as the hook shell will reach them.
#[test]
fn hook_callers_are_pure_over_an_injected_environment() {
    let env: std::collections::HashMap<String, OsString> = std::collections::HashMap::new();
    let outsider = Outsider::<false>;
    assert_eq!(
        outsider.classify(&Map::new(), &env),
        CallerVerdict::Ours { foreign_env: None }
    );
    assert_eq!(outsider.contract(None), OutputContract::EmptyObject);
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
    let path = fixture.basic_transcript();
    let full = fs::read_to_string(&path).unwrap();
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

    let first = fixture.ingest_claude_code(&path);
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
    let second = fixture.ingest_claude_code(&path);
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
    let third = fixture.ingest_claude_code(&path);
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
    let fourth = fixture.ingest_claude_code(&path);
    let (legacy, typed) = cursors();
    assert_eq!(
        (legacy, typed.byte_offset, typed.epoch),
        (fourth.cursor, fourth.cursor, 2)
    );
    assert_eq!(fourth.inserted, 0, "nothing is recorded twice");
}

/// A typed row of another codec kind or version for a Claude Code session (a
/// newer cvc wrote it, then the user went back to this one) is left as it
/// is, and the session's 0.7.x row advances exactly as without it: Claude
/// Code reads where to resume from that row, so its cursor keeps moving.
#[test]
fn claude_codes_cursor_advances_beside_a_typed_row_it_cannot_read() {
    let clean = Fixture::new(Ack::None);
    let expected = clean.ingest_claude_code(&clean.basic_transcript());

    for (kind, version) in [("spool", 1), ("jsonl", 2)] {
        let fixture = Fixture::new(Ack::None);
        let foreign = r#"{"assembled_through":9}"#;
        fixture
            .conn()
            .execute(
                "INSERT INTO harness_cursors(harness,cursor_key,kind,version,epoch,state,updated_at) VALUES ('claude-code',?1,?2,?3,4,?4,1)",
                params![BASIC_SESSION, kind, version, foreign],
            )
            .unwrap();
        let typed_row = || -> (String, i64, i64, String, i64) {
            fixture
                .conn()
                .query_row(
                    "SELECT kind, version, epoch, state, updated_at FROM harness_cursors WHERE cursor_key=?1",
                    params![BASIC_SESSION],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                )
                .unwrap()
        };
        let before = typed_row();

        let path = fixture.basic_transcript();
        let report = fixture.ingest_claude_code(&path);
        assert_eq!(report, expected, "{kind} {version}");
        let legacy = fixture
            .store
            .harness_ingest_cursor("claude-code", BASIC_SESSION)
            .unwrap()
            .unwrap();
        assert_eq!(legacy.byte_offset, expected.cursor, "{kind} {version}");
        assert_eq!(typed_row(), before, "{kind} {version}: never spliced into");

        // The next run resumes from the 0.7.x row: nothing again.
        let again = fixture.ingest_claude_code(&path);
        assert_eq!(
            (again.inserted, again.cursor),
            (0, expected.cursor),
            "{kind} {version}"
        );
        assert_eq!(typed_row(), before, "{kind} {version}");
    }
}

const FAMILY_SESSION: &str = "0f773b11-7a20-427d-bd9d-208729c5fd87";
const META: &str = r#"{"agentType":"ux-psychologist","description":"Design the keymap","model":"sonnet","spawnDepth":1,"toolUseId":"toolu_01SN"}"#;
const TASK: &str = "You are the subagent; do the task.";

/// A session that delegates to the subagent `agent-x`, laid out as Claude
/// Code lays it out; `meta` writes the subagent's `.meta.json`, or puts
/// something else at its path. Returns the session's transcript.
fn delegating_session(fixture: &Fixture, meta: impl FnOnce(&Path)) -> PathBuf {
    let line = |text: String| text + "\n";
    let main = [
        format!(r#"{{"parentUuid":null,"isSidechain":false,"type":"user","message":{{"role":"user","content":"Delegate to a subagent"}},"uuid":"u1","timestamp":"2026-08-10T22:04:00.000Z","sessionId":"{FAMILY_SESSION}","version":"2.1.219","cwd":"x"}}"#),
        format!(r#"{{"parentUuid":"u1","isSidechain":false,"requestId":"req_A","type":"assistant","message":{{"model":"m","id":"a","type":"message","role":"assistant","content":[{{"type":"text","text":"Delegating."}}],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"s1","timestamp":"2026-08-10T22:04:01.000Z","sessionId":"{FAMILY_SESSION}","version":"2.1.219","cwd":"x"}}"#),
    ]
    .map(line)
    .concat();
    let subagent = [
        format!(r#"{{"parentUuid":null,"isSidechain":true,"agentId":"x","type":"user","message":{{"role":"user","content":"{TASK}"}},"uuid":"su1","timestamp":"2026-08-10T22:04:26.089Z","sessionId":"{FAMILY_SESSION}","version":"2.1.219","cwd":"x"}}"#),
        format!(r#"{{"parentUuid":"su1","isSidechain":true,"agentId":"x","requestId":"req_S","type":"assistant","message":{{"model":"m","id":"sb","type":"message","role":"assistant","content":[{{"type":"text","text":"Subagent done."}}],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"su2","timestamp":"2026-08-10T22:04:28.000Z","sessionId":"{FAMILY_SESSION}","version":"2.1.219","cwd":"x"}}"#),
    ]
    .map(line)
    .concat();
    let directory = fixture.root.join("proj");
    let subagents = directory.join(FAMILY_SESSION).join("subagents");
    fs::create_dir_all(&subagents).unwrap();
    let main_path = directory.join(format!("{FAMILY_SESSION}.jsonl"));
    fs::write(&main_path, main).unwrap();
    fs::write(subagents.join("agent-x.jsonl"), subagent).unwrap();
    meta(&subagents.join("agent-x.meta.json"));
    main_path
}

/// The subagent's title, and its report, after one ingest of the session.
fn subagent_title(meta: impl FnOnce(&Path)) -> (String, claude_code::IngestReport) {
    let fixture = Fixture::new(Ack::None);
    let path = delegating_session(&fixture, meta);
    let started = std::time::Instant::now();
    let report = claude_code::ingest(
        &fixture.layout,
        &fixture.store,
        &fixture.policy,
        &path,
        Some(FAMILY_SESSION),
        claude_code::IngestMode::Final,
    )
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the ingest returns promptly"
    );
    let title = fixture
        .store
        .get_conversation(&format!("{FAMILY_SESSION}:agent-x"))
        .unwrap()
        .unwrap()
        .title;
    (title, report)
}

/// A subagent's `.meta.json` is read the way its transcript is, to at most
/// 64 KiB: a regular file under the bound titles the subagent as before, and
/// anything else at its path gives no title seed (the subagent's own task
/// titles it) while the subagent is ingested all the same.
#[cfg(unix)]
#[test]
fn a_subagents_meta_file_is_read_the_way_its_transcript_is() {
    let (title, report) = subagent_title(|path| fs::write(path, META).unwrap());
    assert_eq!(title, "ux-psychologist: Design the keymap");
    assert_eq!(report.subagent_inserted, 1);

    let fifo = |path: &Path| {
        let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: `name` is a valid, NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    };
    let link = |path: &Path| {
        let target = path.with_file_name("elsewhere.json");
        fs::write(&target, META).unwrap();
        std::os::unix::fs::symlink(&target, path).unwrap();
    };
    let oversized = |path: &Path| {
        let padding = "p".repeat(64 * 1024);
        fs::write(
            path,
            format!(r#"{{"agentType":"ux-psychologist","description":"Design the keymap","padding":"{padding}"}}"#),
        )
        .unwrap();
    };
    for (name, meta) in [
        ("a FIFO", &fifo as &dyn Fn(&Path)),
        ("a link", &link),
        ("a file over 64 KiB", &oversized),
    ] {
        let (title, report) = subagent_title(meta);
        assert_eq!(title, TASK, "{name}");
        assert_eq!(report.subagent_sessions, 1, "{name}");
        assert_eq!(report.subagent_inserted, 1, "{name}");
    }
}
