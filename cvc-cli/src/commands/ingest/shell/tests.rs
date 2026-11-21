//! The shell's own decisions, in process, with a stand-in adapter (the engine
//! records only for the adapter registered for a harness, so the step after
//! consent is a stand-in too), and Claude Code's caller classification, which
//! the shell now takes from its adapter.
use super::*;
use cvc_core::claude_code;
use cvc_core::harness::adapter::PayloadError;
use git2::ErrorClass;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::time::Duration;
use tempfile::TempDir;

const BUDGET: Duration = Duration::from_secs(20);
const STOP_HINT: &str = "(or stop the stand-in's hooks)";

/// A stand-in harness. Its payload is `{"session": <string>, "event": ...,
/// "subagent": ..., "model": ...}`; a top-level `foreign` key is another
/// agent's; `FOREIGN_ENV` in the environment is another agent's environment.
struct Fake {
    contract: OutputContract,
    /// Where every session starts: `None` cannot be named.
    start: Option<PathBuf>,
    /// Panics in `classify` (`"classify"`) or `request` (`"request"`).
    panics_in: Option<&'static str>,
}

impl HookCaller for Fake {
    const PAYLOAD_KEYS: &'static [&'static str] = &["model"];

    fn classify(&self, payload: &Map<String, Value>, env: &dyn HookEnv) -> CallerVerdict {
        if self.panics_in == Some("classify") {
            panic!("the stand-in's classify");
        }
        if payload.contains_key("foreign") {
            return CallerVerdict::Foreign(Some(HarnessId::Cursor));
        }
        CallerVerdict::Ours {
            foreign_env: env.var_os("FOREIGN_ENV").map(|_| HarnessId::Cursor),
        }
    }

    fn request(&self, payload: Map<String, Value>) -> Result<HookRequest, PayloadError> {
        if self.panics_in == Some("request") {
            panic!("the stand-in's request");
        }
        let text = |key: &str| -> Result<Option<String>, PayloadError> {
            match payload.get(key) {
                None => Ok(None),
                Some(Value::String(text)) => Ok(Some(text.clone())),
                Some(_) => Err(PayloadError::NotTheShape("the stand-in")),
            }
        };
        Ok(HookRequest {
            event: text("event")?,
            session: text("session")?,
            subagent: text("subagent")?,
            // Never kept by `request`: `hook_request` fills it.
            extra: payload,
            ..HookRequest::default()
        })
    }

    fn check_complete(&self, request: &HookRequest) -> Result<(), PayloadError> {
        match request.session {
            Some(_) => Ok(()),
            None => Err(PayloadError::Missing("session")),
        }
    }

    fn start(&self, _request: &HookRequest, _env: &dyn HookEnv) -> StartLocation {
        match &self.start {
            Some(directory) => StartLocation::Directory(directory.clone()),
            None => StartLocation::NotNameable,
        }
    }

    fn wants(&self, request: &HookRequest) -> bool {
        request.subagent.is_none()
    }

    fn finality(&self, event: Option<&str>) -> Finality {
        match event {
            Some("Stop") => Finality::Final,
            _ => Finality::Incremental,
        }
    }

    fn budget(&self, _event: Option<&str>) -> Duration {
        BUDGET
    }

    fn contract(&self, _event: Option<&str>) -> OutputContract {
        self.contract
    }

    fn stop_hint(&self) -> &'static str {
        STOP_HINT
    }
}

/// How the stand-in's step after consent ends.
#[derive(Clone, Copy, Debug)]
enum Then {
    Record,
    PassOver,
    Busy,
    Fail,
    Panic,
}

/// What the step after consent was handed.
#[derive(Debug)]
struct Seen {
    harness: HarnessId,
    request: HookRequest,
    finality: Finality,
    deadline: Instant,
    foreign_env: Option<HarnessId>,
    grant_channel: cvc_core::harness::PassiveChannel,
}

struct FakeCapture {
    then: Then,
    seen: RefCell<Vec<Seen>>,
}

impl FakeCapture {
    fn new(then: Then) -> Self {
        Self {
            then,
            seen: RefCell::new(Vec::new()),
        }
    }
}

impl Capture<Fake> for FakeCapture {
    fn capture(&self, _caller: &Fake, run: CaptureRun<'_>) -> Result<Captured> {
        self.seen.borrow_mut().push(Seen {
            harness: run.harness,
            request: run.request.clone(),
            finality: run.finality,
            deadline: run.deadline,
            foreign_env: run.foreign_env,
            grant_channel: run.grant.channel(),
        });
        match self.then {
            Then::Record => Ok(Captured::Recorded),
            Then::PassOver => Ok(Captured::PassedOver),
            Then::Busy => Ok(Captured::Busy),
            Then::Fail => bail!("the stand-in's ingest failed"),
            Then::Panic => panic!("the stand-in's ingest"),
        }
    }
}

/// A repository with a CVC directory, its capture acknowledged (the legacy
/// channels only) when `acknowledged`.
fn repository(acknowledged: bool) -> TempDir {
    let temp = TempDir::new().unwrap();
    let repo = git2::Repository::init(temp.path()).unwrap();
    fs::create_dir(temp.path().join(".git").join("cvc")).unwrap();
    if acknowledged {
        cvc_core::privacy::acknowledge_capture(&repo).unwrap();
    }
    temp
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, OsString> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect()
}

/// One run: its outcome, and everything it wrote on stdout.
fn run(
    fake: &Fake,
    harness: HarnessId,
    capture: &FakeCapture,
    payload: &[u8],
    environment: &[(&str, &str)],
    started: Instant,
) -> (Result<()>, Vec<u8>) {
    let mut stdin = payload;
    let mut stdout = Vec::new();
    let result = run_hook_with(
        fake,
        harness,
        capture,
        HookIo {
            stdin: &mut stdin,
            stdout: &mut stdout,
        },
        &env(environment),
        started,
    );
    (result, stdout)
}

/// The exit status `main` gives a run's outcome.
fn exit_status(result: &Result<()>) -> u8 {
    match result {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

const CONTRACTS: [OutputContract; 3] = [
    OutputContract::Silent,
    OutputContract::EmptyObject,
    OutputContract::Literal("{\"continue\":true}"),
];

fn contract_bytes(contract: OutputContract) -> &'static [u8] {
    match contract {
        OutputContract::Silent => b"",
        OutputContract::EmptyObject => b"{}",
        OutputContract::Literal(text) => text.as_bytes(),
    }
}

/// One way a run can end.
struct Scenario {
    label: &'static str,
    acknowledged: bool,
    /// The stand-in's sessions start in the repository (else nowhere).
    in_repository: bool,
    harness: HarnessId,
    payload: Vec<u8>,
    environment: Vec<(&'static str, &'static str)>,
    panics_in: Option<&'static str>,
    then: Then,
    /// Exit status 0, and whether the step after consent ran.
    succeeds: bool,
    captured: bool,
    /// What a failure says.
    message: Option<&'static str>,
}

fn paths() -> Vec<Scenario> {
    let ok = br#"{"session":"s1","event":"Stop"}"#.to_vec();
    let path = |label, payload: &[u8]| Scenario {
        label,
        acknowledged: true,
        in_repository: true,
        harness: HarnessId::ClaudeCode,
        payload: payload.to_vec(),
        environment: Vec::new(),
        panics_in: None,
        then: Then::Record,
        succeeds: true,
        captured: false,
        message: None,
    };
    vec![
        Scenario {
            captured: true,
            ..path("recorded", &ok)
        },
        Scenario {
            then: Then::PassOver,
            captured: true,
            ..path("passed over", &ok)
        },
        Scenario {
            then: Then::Busy,
            captured: true,
            ..path("busy", &ok)
        },
        Scenario {
            then: Then::Fail,
            captured: true,
            succeeds: false,
            message: Some("the stand-in's ingest failed"),
            ..path("the ingest failed", &ok)
        },
        Scenario {
            then: Then::Panic,
            captured: true,
            succeeds: false,
            message: Some("cvc stopped on an internal error (the stand-in's ingest)"),
            ..path("the ingest panicked", &ok)
        },
        Scenario {
            panics_in: Some("classify"),
            succeeds: false,
            message: Some("cvc stopped on an internal error (the stand-in's classify)"),
            ..path("the caller's classification panicked", &ok)
        },
        Scenario {
            panics_in: Some("request"),
            succeeds: false,
            message: Some("cvc stopped on an internal error (the stand-in's request)"),
            ..path("reading the payload panicked, before the contract", &ok)
        },
        path("another agent's payload", br#"{"session":"s1","foreign":1}"#),
        Scenario {
            in_repository: false,
            ..path("a session that started nowhere nameable", &ok)
        },
        Scenario {
            succeeds: false,
            message: Some("hook payload is not the JSON Codex delivers on stdin"),
            harness: HarnessId::Codex,
            ..path("not JSON", b"not json")
        },
        Scenario {
            succeeds: false,
            message: Some("hook payload is not the JSON the stand-in delivers on stdin"),
            ..path("the wrong shape", br#"{"session":7}"#)
        },
        Scenario {
            succeeds: false,
            message: Some("hook payload exceeds 1 MiB"),
            ..path("an oversize payload", &vec![b' '; 1024 * 1024 + 1])
        },
        Scenario {
            environment: vec![("FOREIGN_ENV", "1")],
            ..path("an oversize payload under another agent", &vec![b' '; 1024 * 1024 + 1])
        },
        Scenario {
            succeeds: false,
            message: Some("hook payload has no session"),
            ..path("no session", br#"{"event":"Stop"}"#)
        },
        Scenario {
            environment: vec![("FOREIGN_ENV", "1")],
            ..path("no session under another agent", br#"{"event":"Stop"}"#)
        },
        path(
            "a subagent's payload",
            br#"{"session":"s1","subagent":"a1"}"#,
        ),
        Scenario {
            acknowledged: false,
            succeeds: false,
            message: Some("consent-required: Claude Code transcript capture is passive collection and needs the repository-local capture acknowledgement; run `cvc privacy acknowledge-capture` first (or stop the stand-in's hooks)"),
            ..path("capture not acknowledged", &ok)
        },
        Scenario {
            harness: HarnessId::Codex,
            succeeds: false,
            message: Some("consent-required: the capture acknowledgement here was given before cvc could capture Codex; run `cvc privacy acknowledge-capture` again (or stop the stand-in's hooks)"),
            ..path("an acknowledgement that does not cover the channel", &ok)
        },
    ]
}

/// Every way a run ends writes exactly the contract on stdout, once, and
/// exits 0 or 1: a hook never exits 2, the status every agent takes as
/// blocking, and never leaves its harness without the output it requires.
#[test]
fn every_path_writes_exactly_the_contract_and_never_exits_2() {
    for contract in CONTRACTS {
        for path in paths() {
            let repo = repository(path.acknowledged);
            let fake = Fake {
                contract,
                start: path.in_repository.then(|| repo.path().to_path_buf()),
                panics_in: path.panics_in,
            };
            let capture = FakeCapture::new(path.then);
            let (result, stdout) = run(
                &fake,
                path.harness,
                &capture,
                &path.payload,
                &path.environment,
                Instant::now(),
            );
            let label = format!("{} ({contract:?})", path.label);
            assert_eq!(stdout, contract_bytes(contract), "{label}");
            assert!(exit_status(&result) <= 1, "{label}");
            assert_eq!(result.is_ok(), path.succeeds, "{label}: {result:?}");
            assert_eq!(!capture.seen.borrow().is_empty(), path.captured, "{label}");
            if let Some(message) = path.message {
                let printed = format!("{:#}", result.unwrap_err());
                assert!(printed.starts_with(message), "{label}: {printed}");
            }
        }
    }
}

/// Another agent's run is told by its payload: it does nothing but honour
/// the contract, even in an acknowledged repository.
#[test]
fn a_foreign_caller_is_left_alone() {
    let repo = repository(true);
    let fake = Fake {
        contract: OutputContract::EmptyObject,
        start: Some(repo.path().to_path_buf()),
        panics_in: None,
    };
    let capture = FakeCapture::new(Then::Record);
    for payload in [
        &br#"{"session":"s1","foreign":true}"#[..],
        br#"{"foreign":null}"#,
    ] {
        let (result, stdout) = run(
            &fake,
            HarnessId::ClaudeCode,
            &capture,
            payload,
            &[],
            Instant::now(),
        );
        assert!(result.is_ok());
        assert_eq!(stdout, b"{}");
    }
    assert!(capture.seen.borrow().is_empty());
}

/// An acknowledgement given before cvc could capture a harness does not
/// cover it: nothing runs past the consent check, and the refusal says how
/// to fix it and how to stop the hooks. The same acknowledgement covers
/// Claude Code.
#[test]
fn an_acknowledgement_that_does_not_cover_the_channel_stops_the_run() {
    let repo = repository(true);
    let fake = Fake {
        contract: OutputContract::Silent,
        start: Some(repo.path().to_path_buf()),
        panics_in: None,
    };
    let capture = FakeCapture::new(Then::Record);
    let payload = br#"{"session":"s1"}"#;
    let (result, _) = run(
        &fake,
        HarnessId::Codex,
        &capture,
        payload,
        &[],
        Instant::now(),
    );
    assert_eq!(
        format!("{:#}", result.unwrap_err()),
        "consent-required: the capture acknowledgement here was given before cvc could capture Codex; run `cvc privacy acknowledge-capture` again (or stop the stand-in's hooks)"
    );
    assert!(capture.seen.borrow().is_empty());

    let (result, _) = run(
        &fake,
        HarnessId::ClaudeCode,
        &capture,
        payload,
        &[],
        Instant::now(),
    );
    assert!(result.is_ok());
    let seen = capture.seen.borrow();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].harness, HarnessId::ClaudeCode);
    assert_eq!(seen[0].grant_channel, HarnessId::ClaudeCode.channel());
}

/// What the step after consent is handed: the request with only the
/// allow-listed payload keys, the event's finality, the deadline of the
/// run's start plus the budget, and another agent's environment.
#[test]
fn the_capture_gets_the_allow_listed_request_and_the_deadline() {
    let repo = repository(true);
    let fake = Fake {
        contract: OutputContract::Silent,
        start: Some(repo.path().to_path_buf()),
        panics_in: None,
    };
    let capture = FakeCapture::new(Then::Record);
    let started = Instant::now();
    let payload = br#"{"session":"s1","event":"Stop","model":"m-1","user_email":"someone@example.invalid","prompt":"p"}"#;
    let (result, _) = run(
        &fake,
        HarnessId::ClaudeCode,
        &capture,
        payload,
        &[("FOREIGN_ENV", "1")],
        started,
    );
    assert!(result.is_ok(), "{result:?}");
    let seen = capture.seen.borrow();
    let seen = &seen[0];
    assert_eq!(
        seen.request.extra,
        serde_json::json!({"model": "m-1"})
            .as_object()
            .unwrap()
            .clone()
    );
    assert_eq!(seen.request.session.as_deref(), Some("s1"));
    assert_eq!(seen.finality, Finality::Final);
    assert_eq!(seen.deadline, started + BUDGET);
    assert_eq!(seen.foreign_env, Some(HarnessId::Cursor));
}

/// Outside a CVC-initialized repository the hook does nothing, whatever the
/// payload holds; a `cvc` entry that is not a directory is a visible failure.
#[test]
fn outside_an_initialized_repository_nothing_happens() {
    let plain = TempDir::new().unwrap();
    let uninitialized = TempDir::new().unwrap();
    git2::Repository::init(uninitialized.path()).unwrap();
    let missing = plain.path().join("gone");
    for start in [
        plain.path().to_path_buf(),
        uninitialized.path().to_path_buf(),
        missing,
    ] {
        let fake = Fake {
            contract: OutputContract::EmptyObject,
            start: Some(start.clone()),
            panics_in: None,
        };
        let capture = FakeCapture::new(Then::Record);
        for payload in [&b"not json"[..], br#"{"session":"s1"}"#, b""] {
            let (result, stdout) = run(
                &fake,
                HarnessId::ClaudeCode,
                &capture,
                payload,
                &[],
                Instant::now(),
            );
            assert!(result.is_ok(), "{start:?}: {result:?}");
            assert_eq!(stdout, b"{}");
        }
        assert!(capture.seen.borrow().is_empty());
    }

    let broken = TempDir::new().unwrap();
    git2::Repository::init(broken.path()).unwrap();
    fs::write(broken.path().join(".git").join("cvc"), "").unwrap();
    let fake = Fake {
        contract: OutputContract::Silent,
        start: Some(broken.path().to_path_buf()),
        panics_in: None,
    };
    let (result, _) = run(
        &fake,
        HarnessId::ClaudeCode,
        &FakeCapture::new(Then::Record),
        br#"{"session":"s1"}"#,
        &[],
        Instant::now(),
    );
    let printed = format!("{:#}", result.unwrap_err());
    assert!(
        printed.contains("exists but is not a directory"),
        "{printed}"
    );
}

// ---- reading stdin ---------------------------------------------------------

/// A reader of `length` spaces that counts what is taken from it.
struct Counting<R> {
    inner: R,
    taken: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.taken += count as u64;
        Ok(count)
    }
}

fn spaces(length: u64) -> Counting<impl Read> {
    Counting {
        inner: io::repeat(b' ').take(length),
        taken: 0,
    }
}

#[test]
fn a_payload_up_to_the_bound_is_read_whole() {
    let input = read_payload(&b"{\"cwd\":\"/x\"}"[..]);
    assert!(input.read.is_ok());
    assert!(!input.oversize);
    assert_eq!(input.raw, b"{\"cwd\":\"/x\"}");

    let mut source = spaces(MAX_HOOK_PAYLOAD_BYTES);
    let input = read_payload(&mut source);
    assert!(!input.oversize, "exactly the bound is not over it");
    assert_eq!(input.raw.len() as u64, MAX_HOOK_PAYLOAD_BYTES);
    assert_eq!(source.taken, MAX_HOOK_PAYLOAD_BYTES);
}

#[test]
fn an_oversize_payload_is_kept_to_the_bound_and_read_to_the_end() {
    for length in [MAX_HOOK_PAYLOAD_BYTES + 1, 8 * 1024 * 1024] {
        let mut source = spaces(length);
        let input = read_payload(&mut source);
        assert!(input.read.is_ok());
        assert!(input.oversize, "{length}");
        // Only the bound and one byte more are kept...
        assert_eq!(input.raw.len() as u64, MAX_HOOK_PAYLOAD_BYTES + 1);
        // ...and the writer's remaining bytes are taken off the pipe.
        assert_eq!(source.taken, length, "{length}");
    }
}

#[test]
fn what_is_read_off_the_pipe_has_a_cap() {
    let mut source = spaces(MAX_DRAINED_HOOK_PAYLOAD_BYTES + 1024 * 1024);
    let input = read_payload(&mut source);
    assert!(input.oversize);
    assert_eq!(source.taken, MAX_DRAINED_HOOK_PAYLOAD_BYTES);
}

#[test]
fn a_failing_pipe_is_reported_and_a_failing_drain_is_not() {
    struct Failing;
    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("closed"))
        }
    }
    let input = read_payload(Failing);
    assert!(input.read.is_err());
    assert!(!input.oversize);

    // The payload was complete as far as it mattered when the pipe broke.
    let input = read_payload(io::repeat(b' ').take(2 * 1024 * 1024).chain(Failing));
    assert!(input.read.is_ok());
    assert!(input.oversize);
}

// ---- repositories ------------------------------------------------------------

fn git_error(code: ErrorCode) -> RepositoryLayoutError {
    RepositoryLayoutError::Git(git2::Error::new(code, ErrorClass::Repository, "stand-in"))
}

/// CI cannot chown a repository to another user, so the classification is
/// pinned on the error libgit2 raises for one (`GIT_EOWNER`).
#[test]
fn a_repository_owned_by_someone_else_is_not_cvcs_business() {
    assert!(is_not_cvcs_business(&git_error(ErrorCode::Owner)));
    assert!(is_not_cvcs_business(&RepositoryLayoutError::NotRepository));
}

#[test]
fn every_other_failure_to_discover_a_repository_stays_visible() {
    for code in [
        ErrorCode::GenericError,
        ErrorCode::NotFound,
        ErrorCode::Invalid,
        ErrorCode::Locked,
        ErrorCode::Certificate,
        ErrorCode::Ambiguous,
        ErrorCode::BareRepo,
        ErrorCode::UnbornBranch,
        ErrorCode::Directory,
    ] {
        assert!(!is_not_cvcs_business(&git_error(code)), "{code:?}");
    }
    assert!(!is_not_cvcs_business(&RepositoryLayoutError::Metadata(
        "Git cannot open the repository at /x/.git: gone".into()
    )));
    assert!(!is_not_cvcs_business(
        &RepositoryLayoutError::BareRepository
    ));
}

// ---- Claude Code's caller classification (#189), from its adapter ------------

fn object(json: Value) -> Map<String, Value> {
    match json {
        Value::Object(object) => object,
        other => panic!("not an object: {other}"),
    }
}

/// A `PostToolUse` payload as Claude Code sends it; ids are made up.
fn claude_code_payload() -> Value {
    serde_json::json!({
        "session_id": "00000000-0000-4000-8000-0000000000a1",
        "transcript_path": "/home/someone/.claude/projects/-x/00000000-0000-4000-8000-0000000000a1.jsonl",
        "cwd": "/work/example",
        "permission_mode": "default",
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "true"},
        "tool_response": {"stdout": ""},
        "tool_use_id": "tool-use-1"
    })
}

fn classify(payload: &Map<String, Value>, cursor_version: Option<&str>) -> CallerVerdict {
    let environment = match cursor_version {
        Some(version) => env(&[("CURSOR_VERSION", version)]),
        None => env(&[]),
    };
    claude_code::Adapter.classify(payload, &environment)
}

const CLAUDE_CODE: CallerVerdict = CallerVerdict::Ours { foreign_env: None };
const CLAUDE_CODE_UNDER_CURSOR: CallerVerdict = CallerVerdict::Ours {
    foreign_env: Some(HarnessId::Cursor),
};
const CURSOR: CallerVerdict = CallerVerdict::Foreign(Some(HarnessId::Cursor));
const ANTIGRAVITY: CallerVerdict = CallerVerdict::Foreign(Some(HarnessId::Antigravity));

#[test]
fn a_payload_with_cursors_keys_is_cursors() {
    // `cursor_version`, which every Cursor hook input carries, alone.
    let mut payload = object(claude_code_payload());
    payload.insert("cursor_version".into(), Value::String("1.0.0".into()));
    assert_eq!(classify(&payload, None), CURSOR);
    // `conversation_id` in a payload without `session_id`.
    let mut payload = object(claude_code_payload());
    payload.remove("session_id");
    payload.insert("conversation_id".into(), Value::String("c1".into()));
    assert_eq!(classify(&payload, None), CURSOR);
    // Present is enough, whatever the value.
    let payload = object(serde_json::json!({
        "conversation_id": null,
        "hook_event_name": "stop",
        "transcript_path": "/home/someone/.cursor/projects/x/agent-transcripts/y.jsonl"
    }));
    assert_eq!(classify(&payload, Some("1.0.0")), CURSOR);
}

#[test]
fn a_camel_case_payload_is_antigravitys() {
    for key in [
        "hookEventName",
        "toolCall",
        "lastUserInput",
        "conversationId",
        "transcriptPath",
        "workspacePaths",
    ] {
        let payload = object(serde_json::json!({ key: "x" }));
        assert_eq!(classify(&payload, None), ANTIGRAVITY, "{key}");
    }
    for payload in [
        serde_json::json!({
            "conversationId": "00000000-0000-4000-8000-0000000000b1",
            "transcriptPath": "/x/y.jsonl",
            "workspacePaths": ["/work/example"]
        }),
        // Only the keys Antigravity's changelog documents.
        serde_json::json!({
            "hookEventName": "PostToolUse",
            "cwd": "/work/example",
            "toolCall": {"name": "run_command", "args": {"command": "true"}}
        }),
        serde_json::json!({"hookEventName": "Stop", "lastUserInput": "Say hello"}),
    ] {
        assert_eq!(
            classify(&object(payload.clone()), None),
            ANTIGRAVITY,
            "{payload}"
        );
    }
}

/// A tool's input and output are the model's and the tool's to fill, so a
/// key below the top level says nothing about the caller.
#[test]
fn keys_below_the_top_level_do_not_count() {
    let mut payload = object(claude_code_payload());
    payload.insert(
        "tool_input".into(),
        serde_json::json!({
            "conversationId": "x",
            "transcriptPath": "/x",
            "workspacePaths": ["/x"],
            "cursor_version": "1",
            "conversation_id": "x",
            "nested": {"conversationId": "x"}
        }),
    );
    payload.insert(
        "tool_response".into(),
        serde_json::json!([{"cursor_version": "1"}]),
    );
    assert_eq!(classify(&payload, None), CLAUDE_CODE);
}

#[test]
fn a_claude_code_payload_is_claude_codes_whatever_the_environment_says() {
    let payload = object(claude_code_payload());
    assert_eq!(classify(&payload, None), CLAUDE_CODE);
    // Claude Code run in Cursor's integrated terminal may inherit the
    // variable (not verified): that alone never makes its run another
    // agent's.
    assert_eq!(classify(&payload, Some("2.0.0")), CLAUDE_CODE_UNDER_CURSOR);
    assert_eq!(classify(&payload, Some("")), CLAUDE_CODE);
    // Nor does it with a payload that names nothing at all (the plugin's
    // probe sends only a working directory).
    let probe = object(serde_json::json!({"cwd": "/nonexistent/cvc-plugin-probe"}));
    assert_eq!(classify(&probe, Some("2.0.0")), CLAUDE_CODE_UNDER_CURSOR);
    assert_eq!(classify(&Map::new(), None), CLAUDE_CODE);
}

/// Every top-level key Claude Code's hook input documents, all at once and
/// each alone: none of them makes a run another agent's.
#[test]
fn claude_codes_documented_payload_keys_are_not_another_agents() {
    let keys = [
        "session_id",
        "transcript_path",
        "cwd",
        "permission_mode",
        "hook_event_name",
        "tool_name",
        "tool_input",
        "tool_response",
        "tool_use_id",
        "prompt",
        "stop_hook_active",
        "source",
        "reason",
        "trigger",
        "custom_instructions",
        "message",
        "agent_id",
        "agent_type",
        "agent_transcript_path",
        "model",
    ];
    let all: Map<String, Value> = keys
        .iter()
        .map(|key| ((*key).to_owned(), Value::String("x".into())))
        .collect();
    assert_eq!(classify(&all, None), CLAUDE_CODE);
    for key in keys {
        let one: Map<String, Value> = [(key.to_owned(), Value::Bool(true))].into_iter().collect();
        assert_eq!(classify(&one, None), CLAUDE_CODE, "{key}");
    }
}

/// `conversation_id` is Cursor's only where Claude Code's `session_id` is
/// missing; `cursor_version` is Cursor's whatever else is there.
#[test]
fn a_conversation_id_beside_a_session_id_is_not_cursors() {
    let mut payload = object(claude_code_payload());
    payload.insert("conversation_id".into(), Value::String("c1".into()));
    assert_eq!(classify(&payload, None), CLAUDE_CODE);
    payload.insert("cursor_version".into(), Value::String("1.0.0".into()));
    assert_eq!(classify(&payload, None), CURSOR);
}

/// Claude Code's own contract through the shell: nothing on stdout, on every
/// path the shell decides itself.
#[test]
fn claude_codes_contract_is_an_empty_stdout() {
    let adapter = claude_code::Adapter;
    assert_eq!(adapter.contract(None), OutputContract::Silent);
    for event in ["PostToolUse", "Stop", "SessionEnd"] {
        assert_eq!(adapter.contract(Some(event)), OutputContract::Silent);
    }
}
