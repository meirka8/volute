#![cfg(unix)]

//! `cvc ingest <harness>` and `cvc harness ... <harness>` through the
//! harness registry, end to end: a harness CVC knows but this build cannot
//! capture is refused plainly with exit status 1 (never 2, which agents take
//! as blocking); the capture acknowledgement's notice lists what it covers;
//! `cvc privacy status` and `cvc harness status` show the coverage channel by
//! channel.

mod common;

use common::Fixture;
use std::io::Write;
use std::process::Stdio;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const UNAVAILABLE: [(&str, &str); 4] = [
    ("codex", "Codex"),
    ("opencode", "OpenCode"),
    ("cursor", "Cursor"),
    ("antigravity", "Antigravity"),
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
/// this build) fails plainly, with status 1 and nothing on stdout, whatever
/// the repository's state and the payload; so do the operator's runs and the
/// `harness` commands.
#[test]
fn a_harness_without_an_adapter_in_this_build_is_refused_with_exit_status_1() {
    for fixture in [Fixture::new(), initialized(), acknowledged()] {
        for (name, display) in UNAVAILABLE {
            let expected = format!("cvc {VERSION} cannot capture {display} yet\n");
            let payload =
                br#"{"session_id":"s","transcript_path":"/t.jsonl","hook_event_name":"Stop"}"#;
            let output = run_with_stdin(&fixture, &["ingest", name, "--hook"], payload);
            assert_eq!(output.status.code(), Some(1), "{name} --hook");
            assert!(output.stdout.is_empty(), "{name} --hook");
            assert_eq!(String::from_utf8_lossy(&output.stderr), expected);

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
/// to record the session instead.
#[test]
fn claude_code_locates_by_transcript_only() {
    let fixture = acknowledged();
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

const NOT_ACKNOWLEDGED_STATUS: &str = "harness: Claude Code
hooks_installed: no
capture_covered: false
note: capture is not acknowledged in this repository; run `cvc privacy acknowledge-capture`
capture_covers: none
capture_not_covered: VS Code, Claude Code
capture_unavailable_in_this_cvc: Codex, OpenCode, Cursor, Antigravity
";

const ACKNOWLEDGED_STATUS: &str = "harness: Claude Code
hooks_installed: no
capture_covered: true
capture_covers: VS Code, Claude Code
capture_unavailable_in_this_cvc: Codex, OpenCode, Cursor, Antigravity
";

const NO_LIST_NOTE: &str = "note: this acknowledgement lists no agents, so it covers VS Code and Claude Code only; capturing any other agent needs a fresh `cvc privacy acknowledge-capture` (an older cvc may have rewritten the privacy file and dropped the list)\n";

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
            "harness: Claude Code\nhooks_installed: this checkout\nbinary_for_new_worktrees: {binary}\ncapture_covered: true\n"
        )),
        "{status}"
    );
    fixture.cvc_ok(&["harness", "uninstall", "claude-code"]);
    assert!(fixture
        .cvc_ok(&["harness", "status", "claude-code"])
        .starts_with("harness: Claude Code\nhooks_installed: no\ncapture_covered: true\n"));
}
