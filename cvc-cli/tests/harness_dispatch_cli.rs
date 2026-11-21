#![cfg(unix)]

//! `cvc ingest <harness>` and `cvc harness ... <harness>` through the
//! harness registry, end to end: a harness CVC knows but this build cannot
//! capture is refused plainly with exit status 1 (never 2, which agents take
//! as blocking); the capture acknowledgement's notice lists what it covers;
//! `cvc privacy status` and `cvc harness status` show the coverage channel by
//! channel.

mod common;

use common::{split_nonce, Fixture, PtySession};
use std::io::Write;
use std::process::Stdio;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Each harness without an adapter in this build, and what its hooks must
/// print on stdout.
const UNAVAILABLE: [(&str, &str, &str); 4] = [
    ("codex", "Codex", ""),
    ("opencode", "OpenCode", ""),
    ("cursor", "Cursor", "{}"),
    ("antigravity", "Antigravity", "{}"),
];

fn initialized() -> Fixture {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture
}

fn acknowledged() -> Fixture {
    let fixture = initialized();
    cvc_core::privacy::acknowledge_capture(&fixture.open_repo()).unwrap();
    fixture
}

/// Runs `cvc <args>` with `stdin` on its standard input.
fn run_with_stdin(fixture: &Fixture, args: &[&str], stdin: &[u8]) -> std::process::Output {
    let mut child = fixture
        .cvc_command(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The process may exit before it reads all of it.
    let _ = child.stdin.take().unwrap().write_all(stdin);
    child.wait_with_output().unwrap()
}

/// `cvc ingest codex --hook` (and every other harness without an adapter in
/// this build) fails plainly, with status 1 and the output its agent
/// requires (`{}` for Cursor and Antigravity, nothing for the others),
/// whatever the repository's state and the payload, which it reads whole; the
/// operator's runs and the `harness` commands fail the same way, with
/// nothing on stdout.
#[test]
fn a_harness_without_an_adapter_in_this_build_is_refused_with_exit_status_1() {
    for fixture in [Fixture::new(), initialized(), acknowledged()] {
        for (name, display, contract) in UNAVAILABLE {
            let expected = format!("cvc {VERSION} cannot capture {display} yet\n");
            let small =
                br#"{"session_id":"s","transcript_path":"/t.jsonl","hook_event_name":"Stop"}"#
                    .to_vec();
            let large = vec![b' '; 4 * 1024 * 1024];
            for payload in [small, large] {
                let output = run_with_stdin(&fixture, &["ingest", name, "--hook"], &payload);
                assert_eq!(output.status.code(), Some(1), "{name} --hook");
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout),
                    contract,
                    "{name} --hook"
                );
                assert_eq!(String::from_utf8_lossy(&output.stderr), expected);
            }

            for args in [
                vec!["ingest", name, "--transcript", "s.jsonl"],
                vec!["ingest", name, "--scan"],
                vec!["harness", "install", name],
                vec!["harness", "uninstall", name],
                vec!["harness", "status", name],
            ] {
                let output = fixture.cvc(&args);
                assert_eq!(output.status.code(), Some(1), "{args:?}");
                assert!(output.stdout.is_empty(), "{args:?}");
                assert_eq!(
                    String::from_utf8_lossy(&output.stderr),
                    expected,
                    "{args:?}"
                );
            }
        }
    }
}

/// A name CVC does not know is a usage error, with status 1 as well.
#[test]
fn an_unknown_harness_is_a_usage_error() {
    let fixture = initialized();
    let output = fixture.cvc(&["ingest", "vim", "--hook"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid value 'vim'"), "{stderr}");
    assert!(stderr.contains("claude-code"), "{stderr}");
}

/// Claude Code's own `--session` and `--scan` are not supported, and say how
/// to record the session instead, wherever they are run.
#[test]
fn claude_code_locates_by_transcript_only() {
    for fixture in [Fixture::new(), initialized(), acknowledged()] {
        for (args, expected) in [
        (
            &["ingest", "claude-code", "--session", "s"][..],
            "--session without --transcript is not supported for claude-code; pass --transcript\n",
        ),
        (
            &["ingest", "claude-code", "--scan"][..],
            "--scan is not supported for claude-code; pass --transcript\n",
        ),
    ] {
        let output = fixture.cvc(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            expected,
            "{args:?}"
        );
    }
    }
}

const NOT_ACKNOWLEDGED_STATUS: &str = "harness: Claude Code
checkout_hooks_installed: no
note: hooks from the cvc Claude Code plugin or from other Claude Code settings files also record here while capture is covered; cvc does not check for them (see /plugin list)
capture_covered: false
note: capture is not acknowledged in this repository; run `cvc privacy acknowledge-capture`
capture_covers: none
capture_not_covered: VS Code, Claude Code
capture_unavailable_in_this_cvc: Codex, OpenCode, Cursor, Antigravity
";

const ACKNOWLEDGED_STATUS: &str = "harness: Claude Code
checkout_hooks_installed: no
note: hooks from the cvc Claude Code plugin or from other Claude Code settings files also record here while capture is covered; cvc does not check for them (see /plugin list)
capture_covered: true
capture_covers: VS Code, Claude Code
capture_unavailable_in_this_cvc: Codex, OpenCode, Cursor, Antigravity
";

const NO_LIST_NOTE: &str = "note: this acknowledgement was recorded without a list of what it covers (by cvc 0.7.2 or earlier, or rewritten by such a cvc since), so it covers VS Code and Claude Code only. An agent added by a later cvc needs `cvc privacy acknowledge-capture` run with that cvc.\n";

/// Writes `payload` to a child's stdin in full or fails the test: a child
/// that stops reading early breaks the pipe.
fn writes_all(fixture: &Fixture, args: &[&str], payload: &[u8]) -> std::process::Output {
    let mut child = fixture
        .cvc_command(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(payload)
        .expect("the whole payload was read");
    drop(stdin);
    child.wait_with_output().unwrap()
}

/// The payload of a refused hook is read off the pipe, so the agent writing
/// it never meets a closed pipe.
#[test]
fn a_refused_hook_reads_its_whole_payload() {
    let fixture = initialized();
    let output = writes_all(
        &fixture,
        &["ingest", "cursor", "--hook"],
        &vec![b' '; 8 * 1024 * 1024],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"{}");
}

/// Drops the channel list from the acknowledgement on record, as cvc 0.7.x
/// does whenever it rewrites the privacy file.
fn drop_the_channel_list(fixture: &Fixture) {
    let path = fixture.repo.join(".git").join("cvc").join("privacy.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let removed = state
        .as_object_mut()
        .unwrap()
        .remove("capture_acknowledged_channels");
    assert!(removed.is_some(), "{state}");
    std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
}

#[test]
fn harness_status_shows_the_installation_and_the_coverage_by_channel() {
    let fixture = initialized();
    assert_eq!(
        fixture.cvc_ok(&["harness", "status", "claude-code"]),
        NOT_ACKNOWLEDGED_STATUS
    );

    cvc_core::privacy::acknowledge_capture(&fixture.open_repo()).unwrap();
    assert_eq!(
        fixture.cvc_ok(&["harness", "status", "claude-code"]),
        ACKNOWLEDGED_STATUS
    );

    // An acknowledgement whose list an older cvc dropped covers the legacy
    // channels, and says that another agent needs a fresh one.
    drop_the_channel_list(&fixture);
    assert_eq!(
        fixture.cvc_ok(&["harness", "status", "claude-code"]),
        format!("{ACKNOWLEDGED_STATUS}{NO_LIST_NOTE}")
    );

    let binary = env!("CARGO_BIN_EXE_cvc");
    fixture.cvc_ok(&["harness", "install", "claude-code", "--binary", binary]);
    let status = fixture.cvc_ok(&["harness", "status", "claude-code"]);
    assert!(
        status.starts_with(&format!(
            "harness: Claude Code\ncheckout_hooks_installed: yes\nnote: hooks from the cvc Claude Code plugin or from other Claude Code settings files also record here while capture is covered; cvc does not check for them (see /plugin list)\nbinary_for_new_worktrees: {binary}\ncapture_covered: true\n"
        )),
        "{status}"
    );
    fixture.cvc_ok(&["harness", "uninstall", "claude-code"]);
    assert!(fixture
        .cvc_ok(&["harness", "status", "claude-code"])
        .starts_with(
            "harness: Claude Code\ncheckout_hooks_installed: no\nnote: hooks from the cvc Claude Code plugin or from other Claude Code settings files also record here while capture is covered; cvc does not check for them (see /plugin list)\ncapture_covered: true\n"
        ));
}

/// `cvc privacy status` adds the coverage, channel by channel, once an
/// acknowledgement stands here, after everything it printed before.
#[test]
fn privacy_status_lists_the_channels_covered_once_acknowledged() {
    let fixture = initialized();
    let before = fixture.cvc_ok(&["privacy", "status", "--remote", "origin"]);
    assert!(
        before.starts_with("capture_acknowledged: false\n"),
        "{before}"
    );
    assert!(!before.contains("capture_covers"), "{before}");

    cvc_core::privacy::acknowledge_capture(&fixture.open_repo()).unwrap();
    let status = fixture.cvc_ok(&["privacy", "status", "--remote", "origin"]);
    let (head, coverage) = status
        .split_once("capture_covers: ")
        .unwrap_or_else(|| panic!("{status}"));
    assert!(head.starts_with("capture_acknowledged: true\nsharing_consented: false\n"));
    assert!(head.contains("\ndestination_fingerprint: "), "{head}");
    assert_eq!(
        coverage,
        "VS Code, Claude Code\ncapture_unavailable_in_this_cvc: Codex, OpenCode, Cursor, Antigravity\n"
    );

    drop_the_channel_list(&fixture);
    let status = fixture.cvc_ok(&["privacy", "status", "--remote", "origin"]);
    assert!(status.ends_with(NO_LIST_NOTE), "{status}");
}

/// Before its phrase, `cvc privacy acknowledge-capture` shows what each
/// channel it covers reads, and what it covers; the acknowledgement it
/// records lists exactly those channels.
#[test]
fn the_capture_acknowledgement_shows_what_it_covers_and_records_exactly_that() {
    let fixture = initialized();
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["privacy", "acknowledge-capture"]),
        b"",
    );
    let challenge = session.wait_for_challenge(1);
    assert_eq!(split_nonce(&challenge).0, "I UNDERSTAND LOCAL CAPTURE");
    let shown = session.plain_output();
    for expected in [
        "Once you type the phrase below, CVC records without asking each time, privately, in this repository's local CVC store (inside .git, shared by all its worktrees; git push does not carry it):",
        cvc_core::harness::PassiveChannel::Vscode.notice().unwrap(),
        cvc_core::harness::HarnessId::ClaudeCode.channel().notice().unwrap(),
        "This acknowledgement covers VS Code and Claude Code, in this repository and all its worktrees, at this location and for this user. It covers no agent that a later version of cvc adds.",
        "Text is scrubbed for likely secrets before it is stored, but scrubbing is defense in depth, not a guarantee: what is stored can still contain secrets and personal data. Nothing is shared until you share it with `cvc share`.",
        "Acknowledging installs nothing, but it turns on what is already installed: the VS Code extension records once it sees the acknowledgement, and Claude Code hooks already in place (the cvc plugin, or `cvc harness install claude-code`) record from their next event.",
    ] {
        assert!(shown.contains(expected), "{expected:?} in {shown}");
    }
    let notice_at = shown.find("Once you type the phrase below").unwrap();
    let prompt_at = shown.find("Type exactly '").unwrap();
    assert!(notice_at < prompt_at, "{shown}");

    session.write(format!("{challenge}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    let state: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.repo.join(".git").join("cvc").join("privacy.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        state["capture_acknowledged_channels"],
        serde_json::json!(["vscode", "claude-code"])
    );
    assert_eq!(
        fixture.cvc_ok(&["harness", "status", "claude-code"]),
        ACKNOWLEDGED_STATUS
    );
}
