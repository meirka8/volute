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
use crate::harness::family::{Family, FamilyLinks};
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
use std::cell::RefCell;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;

// ---- the fake adapter ---------------------------------------------------------------

thread_local! {
    /// When each read of the fake on this thread began and ended: the
    /// engine runs on its caller's thread, so a test reads the batches its
    /// own run started, and what a delay made each cost, from here.
    static READS: RefCell<Vec<(Instant, Instant)>> = const { RefCell::new(Vec::new()) };
}

/// The reads this thread's fake made since the last call, in order.
fn take_reads() -> Vec<(Instant, Instant)> {
    READS.with(|reads| std::mem::take(&mut *reads.borrow_mut()))
}

thread_local! {
    /// The byte limit each read of the fake on this thread was given.
    static LIMITS: RefCell<Vec<Option<u64>>> = const { RefCell::new(Vec::new()) };
}

/// The limits this thread's fake was read with since the last call.
fn take_limits() -> Vec<Option<u64>> {
    LIMITS.with(|limits| std::mem::take(&mut *limits.borrow_mut()))
}

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
    /// record makes every read take at least that long: a test's slow disk.
    /// Each read that returns a window is logged ([`take_reads`]).
    fn read(
        &self,
        source: &mut PathBuf,
        from: &JsonlCursor,
        context: &ReadContext<'_>,
    ) -> Result<Read<FakeWindow, JsonlCursor>, FakeError> {
        let began = Instant::now();
        LIMITS.with(|limits| limits.borrow_mut().push(context.limit.bytes));
        let read = self.read_window(source, from, context)?;
        READS.with(|reads| reads.borrow_mut().push((began, Instant::now())));
        Ok(read)
    }
}

impl Fake {
    /// [`RecordSource::read`], unlogged.
    fn read_window(
        &self,
        source: &Path,
        from: &JsonlCursor,
        context: &ReadContext<'_>,
    ) -> Result<Read<FakeWindow, JsonlCursor>, FakeError> {
        if let Ok(delay) = fs::read_to_string(source.parent().unwrap().join("delay-ms")) {
            std::thread::sleep(Duration::from_millis(delay.trim().parse().unwrap()));
        }
        let mut bytes = Vec::new();
        fs::File::open(source)?.read_to_end(&mut bytes)?;
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
            window_bytes: Some(offset as u64 - start),
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
        // A last response marked `reread` keeps the cursor at its start, as
        // Claude Code's planner keeps it at the last closed response: the
        // next read reads it again.
        let resume = match window.lines.last() {
            Some((offset, line)) if line.get("reread") == Some(&json!(true)) => *offset,
            _ => window.end,
        };
        Plan {
            candidates,
            pending: 0,
            resume: window.from.at(resume),
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

    /// The commits the fake's response `key` of `conversation` is linked to.
    fn links_of(&self, conversation: &str, key: &str) -> Vec<String> {
        let id = derived_interaction_id(HarnessId::Codex, conversation, key);
        self.conn()
            .prepare(
                "SELECT git_commit_hash FROM artifact_links WHERE interaction_id=?1 ORDER BY 1",
            )
            .unwrap()
            .query_map(params![id.as_str()], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Every link, as `(response key, commit)`.
    fn all_links(&self) -> Vec<(String, String)> {
        self.conn()
            .prepare("SELECT i.source_request_id, l.git_commit_hash FROM artifact_links l JOIN interactions i ON i.id = l.interaction_id ORDER BY 1, 2")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Points the current branch at a new commit made at `T0 + seconds` in
    /// place of its tip, as an amend does: the old tip is no longer reached.
    fn amend(&self, seconds: i64) -> String {
        let repo = self.layout.repository();
        let head = repo.head().unwrap();
        let old = head.peel_to_commit().unwrap();
        let signature = Signature::new(
            "Fixture",
            "fixture@example.invalid",
            &Time::new(T0 + seconds, 0),
        )
        .unwrap();
        let parents: Vec<git2::Commit<'_>> = old.parents().collect();
        let parents: Vec<&git2::Commit<'_>> = parents.iter().collect();
        let tree = old.tree().unwrap();
        let new = repo
            .commit(None, &signature, &signature, "amended", &tree, &parents)
            .unwrap();
        repo.reference(head.name().unwrap(), new, true, "amend")
            .unwrap();
        new.to_string()
    }

    /// A commit made at `T0 + seconds` that no reference reaches, as one a
    /// rewrite left behind.
    fn unreferenced_commit(&self, seconds: i64) -> String {
        let repo = self.layout.repository();
        let signature = Signature::new(
            "Fixture",
            "fixture@example.invalid",
            &Time::new(T0 + seconds, 0),
        )
        .unwrap();
        let tree = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree).unwrap();
        repo.commit(None, &signature, &signature, "left behind", &tree, &[])
            .unwrap()
            .to_string()
    }

    /// A linked worktree of the repository, sharing its store.
    fn linked_worktree(&self) -> RepositoryLayout {
        let path = self._temp.path().join("other-worktree");
        self.layout
            .repository()
            .worktree("other", &path, None)
            .unwrap();
        RepositoryLayout::discover(&path).unwrap()
    }

    /// [`Self::ingest`], run in the worktree of `layout`.
    fn ingest_in(
        &self,
        layout: &RepositoryLayout,
        path: &Path,
        agent_id: &str,
    ) -> EngineReport<JsonlCursor> {
        let located = Fake.locate_file(path, Some(agent_id)).unwrap();
        engine::ingest(
            &Fake,
            layout,
            &self.store,
            &self.policy,
            located,
            Self::options(false),
        )
        .unwrap()
    }

    /// A floating row of the fake's own source in `conversation`, as one
    /// recorded in this worktree would be.
    fn own_row(&self, conversation: &str, id: &str, seconds: i64) {
        let worktree = self.layout.worktree_origin().unwrap();
        self.conn()
            .execute(
                "INSERT INTO interactions(id,conversation_id,timestamp,author,user_prompt,capture_source,scrubber_version,capture_worktree) VALUES (?1,?2,?3,'agent','a response','codex',1,?4)",
                params![id, conversation, T0 + seconds, worktree],
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

/// The conversations the backlog lists.
fn backlog_ids(store: &CvcStore) -> Vec<String> {
    engine::backlog::<Fake>(store)
        .unwrap()
        .into_iter()
        .map(|backlog| backlog.conversation_id)
        .collect()
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
///
/// What holds whatever the machine's load is checked on the batches the run
/// started, from the fake's log of its reads, not on how many there were: a
/// read takes at least the delay, so every batch after the first starts only
/// with room beyond the reserve for twice that, and the run stops between
/// batches. That the run is over by its deadline also needs its last batch
/// to cost no more than its estimate allows: the delay is long against the
/// rest of a batch's work, so that a loaded machine would have to slow that
/// work by over half a second in the last batch to miss it.
#[test]
fn a_two_second_budget_stops_between_batches_and_the_next_run_goes_on() {
    let fixture = Fixture::new(Ack::WithCodex);
    let (path, line) = forty(&fixture);
    // Every read takes at least 330 ms, and a batch reads two responses:
    // twenty batches would take over six seconds. Four batches fit (three
    // on a machine that adds 25 ms to each); a run that started a fifth,
    // with room for only one more batch's cost, would start it 230 ms too
    // late for twice the fourth's, far more than the slack below.
    let delay = Duration::from_millis(330);
    fs::write(fixture.root.join("delay-ms"), delay.as_millis().to_string()).unwrap();
    let budget = Duration::from_secs(2);
    // The engine keeps an eighth of a run's time this short in reserve.
    let reserve = budget / 8;
    take_reads();
    let started = Instant::now();
    let deadline = started + budget;
    let report = fixture
        .ingest_options(
            &path,
            "t1",
            batched(Some(deadline), 2 * line + 5, Finality::Final),
        )
        .unwrap();
    let took = started.elapsed();
    let reads = take_reads();
    let log: Vec<(Duration, Duration)> = reads
        .iter()
        .map(|(began, ended)| (*began - started, *ended - started))
        .collect();
    // Whole batches, the cursor after the last, more of the record left.
    assert!(report.cut_short && report.root.cut_short, "{report:?}");
    let first = report.root.inserted;
    assert_eq!(first, 2 * reads.len(), "whole batches only: {log:?}");
    assert!(first < 40, "{log:?}");
    assert_eq!(report.root.cursor.byte_offset, first as u64 * line);
    // The first batch, timed, leaves room for at least one more: it fits as
    // long as the first takes under 580 ms.
    assert!(reads.len() >= 2, "{log:?}");
    // Every batch after the first started only with room beyond the reserve
    // for twice the one before it, which took at least the delay. (The read
    // begins a little after the engine decides: the coverage check comes
    // between, which the slack is generous for.)
    let slack = Duration::from_millis(100);
    for (number, (began, _)) in reads.iter().enumerate().skip(1) {
        assert!(
            *began + 2 * delay <= deadline - reserve + slack,
            "batch {} started without room for it: {log:?}",
            number + 1
        );
    }
    // And the run was over by its deadline.
    assert!(took < budget, "{took:?}: {log:?}");
    assert!(report.backlog);
    assert_eq!(backlog_ids(&fixture.store), ["codex:t1"]);

    fs::remove_file(fixture.root.join("delay-ms")).unwrap();
    let rest = fixture.ingest(&path, "t1").unwrap();
    assert_eq!(rest.root.inserted, 40 - first);
    assert_eq!(rest.root.already_present, 0);
    assert!(!rest.cut_short && !rest.backlog, "{rest:?}");
    assert!(backlog_ids(&fixture.store).is_empty());
    assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 40);
    assert_eq!(rest.root.cursor.byte_offset, 40 * line);
}

/// A run whose deadline is already past reads nothing and leaves the
/// backlog flag, whatever its event: the run that holds the lock when a
/// session's last event comes may be one the deadline stops too. Only a run
/// that reads everything takes the flag away.
#[test]
fn any_run_out_of_time_sets_the_backlog_flag_until_one_reads_everything() {
    let fixture = Fixture::new(Ack::WithCodex);
    let (path, _) = forty(&fixture);
    let past = |finality| batched(Some(Instant::now()), 1024, finality);
    let incremental = fixture
        .ingest_options(&path, "t1", past(Finality::Incremental))
        .unwrap();
    assert!(incremental.cut_short && incremental.backlog);
    assert_eq!(incremental.root.inserted, 0);
    // Only the flag was written, at the start of the record.
    let cursor = fixture
        .store
        .harness_cursor(HarnessId::Codex, "codex:t1")
        .unwrap()
        .unwrap();
    assert_eq!(
        (cursor.state["byte_offset"].as_u64(), &cursor.state["carry"]),
        (Some(0), &json!({"backlog": true}))
    );
    let final_run = fixture
        .ingest_options(&path, "t1", past(Finality::Final))
        .unwrap();
    assert!(final_run.cut_short && final_run.backlog);
    assert_eq!(final_run.root.inserted, 0);
    assert_eq!(backlog_ids(&fixture.store), ["codex:t1"]);
    let again = fixture
        .ingest_options(&path, "t1", past(Finality::Incremental))
        .unwrap();
    assert!(again.backlog);
    let done = fixture
        .ingest_options(&path, "t1", batched(None, 1024, Finality::Incremental))
        .unwrap();
    assert!(!done.cut_short && !done.backlog);
    assert_eq!(done.root.inserted, 40);
    assert!(backlog_ids(&fixture.store).is_empty());
}

/// R9: a response much larger than the window. A run with a short budget
/// grows the window as far as its time allows and leaves the grown window
/// with the cursor, and the next run's first read is of that window, not of
/// the base one. The rate a window is afforded at is the slowest the run
/// measured on a batch that closed responses, and never less than a second
/// per MiB: a batch that only grows the window measures nothing.
#[test]
fn a_grown_window_carries_over_to_the_next_run() {
    let fixture = Fixture::new(Ack::WithCodex);
    // A 20 KiB response, then one of 300 KiB and a small one.
    let mut first = response("resp:a", 1);
    first["text"] = json!("x".repeat(20 * 1024));
    let mut big = response("resp:big", 2);
    big["text"] = json!("x".repeat(300 * 1024));
    let path = fixture.record("t1", &[first, big, response("resp:z", 3)]);
    // Every read takes 60 ms, whatever it reads: the first, which closes a
    // response, measures about 3 s per MiB.
    fs::write(fixture.root.join("delay-ms"), "60").unwrap();
    let base = 24 * 1024;
    let run = || {
        take_limits();
        let report = fixture
            .ingest_options(
                &path,
                "t1",
                batched(
                    Some(Instant::now() + Duration::from_millis(1500)),
                    base,
                    Finality::Incremental,
                ),
            )
            .unwrap();
        (report, take_limits())
    };
    let (report, limits) = run();
    assert!(report.cut_short, "{report:?} {limits:?}");
    assert_eq!(report.root.inserted, 1);
    let grown = report.root.cursor.carry.stall_window.expect("it grew");
    assert!(grown > 2 * base && grown < 300 * 1024, "{grown} {limits:?}");
    // It grew from the base window by doubling, and stopped where twice the
    // next one's estimate no longer fit.
    assert_eq!(limits[0], Some(base), "{limits:?}");
    assert_eq!(limits.last().unwrap().unwrap() * 2, grown, "{limits:?}");

    let (report, limits) = run();
    assert_eq!(
        limits[0],
        Some(grown),
        "the next run starts from it: {limits:?}"
    );
    assert!(!report.cut_short, "{report:?} {limits:?}");
    assert_eq!(report.root.cursor.carry.stall_window, None);
    assert_eq!(fixture.count("SELECT COUNT(*) FROM interactions"), 3);
}

/// Codex's `SessionEnd` waits a quarter of a second for a lock another run
/// holds, then ends as busy, which a hook takes for success: the holder is
/// making progress.
///
/// The wait is an eighth of what is left of the budget when the run begins
/// to wait (the engine's own unit tests pin the numbers with an injected
/// clock). Here it is checked against bounds that hold however late a loaded
/// machine starts the wait: never above a quarter of a second, never below
/// an eighth of the least the run can have had left when it began to wait,
/// and waited out in full before the run gives up, well inside the budget.
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
    let budget = Duration::from_secs(2);
    take_reads();
    let started = Instant::now();
    let result = fixture.ingest_options(
        &path,
        "t1",
        batched(Some(started + budget), 1024, Finality::Final),
    );
    let took = started.elapsed();
    let waited = match result {
        Err(EngineError::Busy { waited, .. }) => waited,
        other => panic!("{:?}", other.map(|_| ())),
    };
    assert!(waited <= budget / 8, "{waited:?}");
    // The wait began at the latest `took - waited` into the budget, so at
    // least the rest of it was left then (a millisecond for rounding).
    let least_left = budget.saturating_sub(took.saturating_sub(waited));
    assert!(
        waited + Duration::from_millis(1) >= least_left / 8,
        "waited {waited:?} of {least_left:?} left (took {took:?})"
    );
    assert!(
        took >= waited,
        "gave up before its wait: {took:?} {waited:?}"
    );
    assert!(took < budget, "{took:?}");
    // No batch was read.
    assert!(take_reads().is_empty());
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

/// Every commit the rescue knows takes part in one order, read in this run
/// or replayed from the store: each claims the reasoning after the commit
/// before it, up to its own time, with no other lower bound. A response
/// recorded late (after the committing one was read) before the first commit
/// is the first commit's, never a later one's, however long before the
/// commit it was made; and `cvc.linkWindow`, the time-window linker's, does
/// not bound the rescue, so many runs link as one manual run does.
#[test]
fn every_commit_known_claims_only_the_reasoning_since_the_one_before() {
    let fixture = Fixture::new(Ack::WithCodex);
    let set_window = |fixture: &Fixture, seconds: &str| {
        fixture
            .layout
            .repository()
            .config()
            .unwrap()
            .set_str("cvc.linkWindow", seconds)
            .unwrap()
    };
    set_window(&fixture, "3600");
    let c1 = fixture.commit(100_000);
    let c2 = fixture.commit(100_100);
    let mut committing = response("resp:c1", 99_990);
    committing["commit"] = json!(c1);
    let mut lines = vec![response("resp:old", 100_000 - 4000), committing];
    let path = fixture.record("t1", &lines);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!(report.linked.root, 2);

    // The next run never reads resp:c1 again: C1 is replayed, C2 is read.
    let mut second = response("resp:c2", 100_090);
    second["commit"] = json!(c2);
    lines.extend([
        response("resp:older", 100_000 - 50_000),
        response("resp:near", 100_000 - 600),
        response("resp:mid", 100_050),
        second,
    ]);
    let path = fixture.record("t1", &lines);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!((report.root.inserted, report.linked.root), (4, 4));
    for (key, commit) in [
        ("resp:older", &c1),
        ("resp:near", &c1),
        ("resp:mid", &c2),
        ("resp:c2", &c2),
    ] {
        assert_eq!(
            fixture.links_of("codex:t1", key),
            std::slice::from_ref(commit),
            "{key}"
        );
    }

    // At 0, the link window still bounds nothing of the rescue.
    set_window(&fixture, "0");
    lines.push(response("resp:late", 100_000 - 60));
    let path = fixture.record("t1", &lines);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!((report.root.inserted, report.linked.root), (1, 1));
    assert_eq!(
        fixture.links_of("codex:t1", "resp:late"),
        std::slice::from_ref(&c1)
    );

    // One manual run of the whole record links exactly the same.
    let manual = Fixture::new(Ack::WithCodex);
    set_window(&manual, "0");
    assert_eq!((manual.commit(100_000), manual.commit(100_100)), (c1, c2));
    let path = manual.record("t1", &lines);
    manual.ingest(&path, "t1").unwrap();
    assert_eq!(manual.all_links(), fixture.all_links());
}

/// A stored commit that a rebase or an amend left behind is not replayed:
/// it gets no new links, and the run says so.
#[test]
fn a_rewritten_commit_is_not_replayed() {
    let fixture = Fixture::new(Ack::WithCodex);
    let c1 = fixture.commit(100);
    let mut committing = response("resp:c1", 90);
    committing["commit"] = json!(c1);
    let mut lines = vec![committing];
    let path = fixture.record("t1", &lines);
    assert_eq!(fixture.ingest(&path, "t1").unwrap().linked.root, 1);

    let amended = fixture.amend(101);
    assert_ne!(amended, c1);
    lines.push(response("resp:late", 80));
    let path = fixture.record("t1", &lines);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!(report.root.inserted, 1);
    assert_eq!((report.linked.root, report.not_replayed), (0, 1));
    assert!(fixture.links_of("codex:t1", "resp:late").is_empty());

    // A later commit, recorded (and so replayed), claims only what follows
    // the commit left behind, which still bounds it.
    let c2 = fixture.commit(200);
    let mut second = response("resp:c2", 190);
    second["commit"] = json!(c2);
    second["reread"] = json!(true);
    lines.push(second);
    let path = fixture.record("t1", &lines);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!((report.root.inserted, report.linked.root), (1, 1));
    assert_eq!(
        fixture.links_of("codex:t1", "resp:c2"),
        std::slice::from_ref(&c2)
    );
    assert!(fixture.links_of("codex:t1", "resp:late").is_empty());

    // The next run only reads resp:c2 again: nothing new, nothing pending,
    // so its rescue replays nothing, yet the stored commits still bound the
    // one it read.
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!((report.root.inserted, report.root.already_present), (0, 1));
    assert_eq!(report.linked, FamilyLinks::default());
    assert!(fixture.links_of("codex:t1", "resp:late").is_empty());
    assert_eq!(fixture.links_of("codex:t1", "resp:c2"), [c2]);
}

/// A commit reported in another worktree is not replayed here, and yet it
/// bounds this worktree's later commits, with replay and without: a session
/// that moves between worktrees never has its reasoning given to the wrong
/// later commit.
#[test]
fn a_commit_reported_in_another_worktree_still_bounds_this_ones() {
    let fixture = Fixture::new(Ack::WithCodex);
    let c1 = fixture.commit(100);
    let other = fixture.linked_worktree();
    let mut committing = response("resp:c1", 90);
    committing["commit"] = json!(c1);
    let mut lines = vec![committing];
    let path = fixture.record("t1", &lines);
    assert_eq!(fixture.ingest_in(&other, &path, "t1").linked.root, 1);

    // Back here: a response before C1, and a commit after it.
    let c2 = fixture.commit(200);
    let mut second = response("resp:c2", 190);
    second["commit"] = json!(c2);
    second["reread"] = json!(true);
    lines.extend([response("resp:early", 80), second]);
    let path = fixture.record("t1", &lines);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!(report.root.inserted, 2);
    assert_eq!((report.linked.root, report.not_replayed), (1, 1));
    assert_eq!(fixture.links_of("codex:t1", "resp:c2"), [c2]);
    assert!(fixture.links_of("codex:t1", "resp:early").is_empty());
    // Without replay, as with it.
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!(report.root.inserted, 0);
    assert!(fixture.links_of("codex:t1", "resp:early").is_empty());
}

/// A branch deleted after it was pushed, or a tagged commit, is still
/// reached: a remote-tracking branch and a tag are tips of the walk, so the
/// commit is replayed.
#[test]
fn a_commit_a_remote_tracking_branch_or_a_tag_reaches_is_replayed() {
    let fixture = Fixture::new(Ack::WithCodex);
    let repo = fixture.layout.repository();
    let c1 = fixture.commit(100);
    let mut committing = response("resp:c1", 90);
    committing["commit"] = json!(c1);
    let mut lines = vec![committing];
    let path = fixture.record("t1", &lines);
    assert_eq!(fixture.ingest(&path, "t1").unwrap().linked.root, 1);
    repo.reference(
        "refs/remotes/origin/feature",
        git2::Oid::from_str(&c1).unwrap(),
        true,
        "pushed",
    )
    .unwrap();
    fixture.amend(101);
    lines.push(response("resp:late", 80));
    let path = fixture.record("t1", &lines);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!((report.linked.root, report.not_replayed), (1, 0));
    assert_eq!(fixture.links_of("codex:t1", "resp:late"), [c1]);

    let c2 = fixture.commit(200);
    let mut second = response("resp:c2", 190);
    second["commit"] = json!(c2);
    lines.push(second);
    let path = fixture.record("t1", &lines);
    assert_eq!(fixture.ingest(&path, "t1").unwrap().linked.root, 1);
    repo.reference(
        "refs/tags/kept",
        git2::Oid::from_str(&c2).unwrap(),
        true,
        "tagged",
    )
    .unwrap();
    fixture.amend(201);
    lines.push(response("resp:later", 150));
    let path = fixture.record("t1", &lines);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!((report.linked.root, report.not_replayed), (1, 0));
    assert_eq!(fixture.links_of("codex:t1", "resp:later"), [c2]);
}

/// Only the commits from the latest one before the family's oldest floating
/// response on are considered: the ones before it can claim nothing, so an
/// old commit left behind is not even walked for (and not counted).
#[test]
fn commits_before_the_one_before_the_oldest_floating_response_are_not_considered() {
    let fixture = Fixture::new(Ack::WithCodex);
    let c0 = fixture.unreferenced_commit(10);
    let mut first = response("resp:c0", 5);
    first["commit"] = json!(c0);
    let mut lines = vec![first];
    let path = fixture.record("t1", &lines);
    // A commit this run reads is not walked for: it links.
    assert_eq!(fixture.ingest(&path, "t1").unwrap().linked.root, 1);

    let c1 = fixture.commit(100);
    let mut second = response("resp:c1", 90);
    second["commit"] = json!(c1);
    lines.push(second);
    let path = fixture.record("t1", &lines);
    // resp:c1 floats until this rescue: C0, before it, bounds it and is
    // walked for, and not reached.
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!((report.linked.root, report.not_replayed), (1, 1));

    lines.push(response("resp:new", 150));
    let path = fixture.record("t1", &lines);
    // Now only resp:new floats, after C1: C0 is not considered at all.
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!((report.root.inserted, report.linked.root), (1, 0));
    assert_eq!(report.not_replayed, 0);
}

/// A member that fails leaves the members after it unread, and one of them
/// may hold an earlier commit than the ones read: the rescue is left
/// pending, so nothing is linked to a later commit that the earlier one
/// claims in a run of the whole family, and the next run that reads the
/// whole family links as that run does.
#[test]
fn a_failed_member_leaves_the_rescue_to_a_run_that_reads_the_whole_family() {
    let fixture = Fixture::new(Ack::WithCodex);
    let early = fixture.commit(100);
    let late = fixture.commit(400);
    let child = |key: &str, seconds: i64, child: &str| {
        let mut line = response(key, seconds);
        line["child"] = json!(child);
        line
    };
    let mut closing = response("resp:rc", 390);
    closing["commit"] = json!(late);
    let path = fixture.record(
        "r1",
        &[
            child("resp:r1", 50, "c1"),
            child("resp:r2", 55, "c2"),
            child("resp:r3", 56, "c3"),
            closing,
        ],
    );
    fixture.record("c1.member", &[response("resp:m1", 60)]);
    fixture.record("c2.member", &[response("resp:m2", 70)]);
    let mut committing = response("resp:m3", 90);
    committing["commit"] = json!(early);
    fixture.record("c3.member", &[committing]);
    // The second member's batch is refused: its conversation holds another
    // source's row.
    let foreign = "6b1f0c0e-1111-4aaa-8bbb-000000000093";
    fixture.foreign_row("codex:c2", foreign, 5);

    let error = fixture.ingest(&path, "r1").unwrap_err();
    assert!(
        matches!(error, EngineError::Db(DbError::HarnessBatch(_))),
        "{error:?}"
    );
    assert_eq!(fixture.count("SELECT COUNT(*) FROM artifact_links"), 0);
    let stored = fixture
        .store
        .harness_cursor(HarnessId::Codex, "codex:r1")
        .unwrap()
        .unwrap();
    assert!(
        JsonlCursor::decode_stored(&stored)
            .unwrap()
            .carry
            .rescue_pending
    );

    fixture
        .conn()
        .execute("DELETE FROM interactions WHERE id=?1", params![foreign])
        .unwrap();
    let report = fixture.ingest(&path, "r1").unwrap();
    assert!(!report.rescue_pending);
    for (conversation, key) in [
        ("codex:r1", "resp:r1"),
        ("codex:r1", "resp:r2"),
        ("codex:r1", "resp:r3"),
        ("codex:c1", "resp:m1"),
        ("codex:c2", "resp:m2"),
        ("codex:c3", "resp:m3"),
    ] {
        assert_eq!(
            fixture.links_of(conversation, key),
            std::slice::from_ref(&early),
            "{key}"
        );
    }
    assert_eq!(fixture.links_of("codex:r1", "resp:rc"), [late]);
}

/// A replayed commit claims only rows of the worktree its reporting
/// interaction was captured in: a run in another worktree of the repository
/// does not replay it, and a run in its own does not claim the other
/// worktree's rows.
#[test]
fn a_replayed_commit_links_no_row_of_another_worktree() {
    let fixture = Fixture::new(Ack::WithCodex);
    let c1 = fixture.commit(100);
    let mut committing = response("resp:c1", 90);
    committing["commit"] = json!(c1);
    let mut lines = vec![committing];
    let path = fixture.record("t1", &lines);
    assert_eq!(fixture.ingest(&path, "t1").unwrap().linked.root, 1);

    // A linked worktree of the same repository, sharing the store.
    let other = fixture._temp.path().join("other-worktree");
    fixture
        .layout
        .repository()
        .worktree("other", &other, None)
        .unwrap();
    let layout = RepositoryLayout::discover(&other).unwrap();
    assert_eq!(layout.db_path(), fixture.layout.db_path());
    assert_ne!(
        layout.worktree_origin().unwrap(),
        fixture.layout.worktree_origin().unwrap()
    );
    lines.push(response("resp:there", 80));
    let path = fixture.record("t1", &lines);
    let located = Fake.locate_file(&path, Some("t1")).unwrap();
    let report = engine::ingest(
        &Fake,
        &layout,
        &fixture.store,
        &fixture.policy,
        located,
        Fixture::options(false),
    )
    .unwrap();
    assert_eq!(report.root.inserted, 1);
    assert_eq!((report.linked.root, report.not_replayed), (0, 1));

    // Back in the first worktree, a run that records something new replays
    // the commit, and claims only its own worktree's rows.
    lines.push(response("resp:here", 85));
    let path = fixture.record("t1", &lines);
    let report = fixture.ingest(&path, "t1").unwrap();
    assert_eq!((report.root.inserted, report.linked.root), (1, 1));
    assert_eq!(fixture.links_of("codex:t1", "resp:here"), [c1]);
    assert!(fixture.links_of("codex:t1", "resp:there").is_empty());
}

/// A run cut short anywhere in the family, the root's own record included,
/// leaves the rescue it was due to a later run, and says it is pending on
/// the root's cursor. A later run that reads the whole family rescues from
/// the stored evidence even when it records nothing new; one that has
/// nothing new and nothing pending does not rescue at all.
#[test]
fn a_skipped_rescue_is_pending_until_a_run_reads_the_whole_family() {
    let fixture = Fixture::new(Ack::WithCodex);
    let sha = fixture.commit(100);
    let mut root = response("resp:r", 30);
    root["child"] = json!("c1");
    root["commit"] = json!(sha);
    let path = fixture.record("r1", &[root.clone(), response("resp:r2", 40)]);
    fixture.record("c1.member", &[]);
    // Every read takes 400 ms, and a batch reads one line: the root's first
    // batch leaves no time for the next within a second.
    fs::write(fixture.root.join("delay-ms"), "400").unwrap();
    let line = format!("{root}\n").len() as u64;
    let options = || {
        batched(
            Some(Instant::now() + Duration::from_secs(1)),
            line + 5,
            Finality::Final,
        )
    };
    let carry = || {
        let stored = fixture
            .store
            .harness_cursor(HarnessId::Codex, "codex:r1")
            .unwrap()
            .unwrap();
        JsonlCursor::decode_stored(&stored).unwrap().carry
    };
    // Cut short in the root's own record: no rescue, though the root has no
    // member yet whose record is left.
    let report = fixture.ingest_options(&path, "r1", options()).unwrap();
    assert!(report.root.cut_short && report.rescue_skipped, "{report:?}");
    assert_eq!(report.root.inserted, 1);
    assert!(report.rescue_pending && carry().rescue_pending && carry().backlog);
    assert_eq!(fixture.count("SELECT COUNT(*) FROM artifact_links"), 0);

    // The root read to its end, its member left unread: no rescue still.
    let report = fixture.ingest_options(&path, "r1", options()).unwrap();
    assert!(!report.root.cut_short && report.cut_short, "{report:?}");
    assert_eq!(report.members.len(), 1);
    assert!(report.rescue_skipped && report.rescue_pending);
    assert_eq!(fixture.count("SELECT COUNT(*) FROM artifact_links"), 0);

    // A run that reads it all, and records nothing new, rescues.
    fs::remove_file(fixture.root.join("delay-ms")).unwrap();
    let report = fixture.ingest(&path, "r1").unwrap();
    assert_eq!(report.root.inserted + report.members[0].inserted, 0);
    assert!(!report.rescue_skipped && !report.rescue_pending && !report.backlog);
    assert_eq!(report.linked.root, 2);
    assert!(!carry().rescue_pending && !carry().backlog);

    // Nothing new and nothing pending: no rescue, so a row that is floating
    // now stays so...
    fixture.own_row("codex:r1", "6b1f0c0e-1111-4aaa-8bbb-000000000071", 50);
    let report = fixture.ingest(&path, "r1").unwrap();
    assert_eq!(report.linked, FamilyLinks::default());
    assert_eq!(fixture.count("SELECT COUNT(*) FROM artifact_links"), 2);
    // ...until a run records something new, whose rescue claims both.
    let path = fixture.record(
        "r1",
        &[root, response("resp:r2", 40), response("resp:r3", 60)],
    );
    let report = fixture.ingest(&path, "r1").unwrap();
    assert_eq!((report.root.inserted, report.linked.root), (1, 2));
    assert_eq!(fixture.count("SELECT COUNT(*) FROM artifact_links"), 4);
}
