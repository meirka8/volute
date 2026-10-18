#![cfg(unix)]

//! Smoke test of the interactive `cvc conversations` through the real binary
//! on a pseudo-terminal: it takes over the screen, shows its header, quits on
//! `q` and gives the terminal back; `--plain` and a window that is too small
//! print the plain listing instead.
//!
//! The consent flows are covered by the acceptance tests; this file only checks
//! that the view starts, draws and leaves.

mod common;

use common::{Fixture, PtySession};
use std::process::Command;

const ENTER_ALTERNATE_SCREEN: &str = "\x1b[?1049h";
const LEAVE_ALTERNATE_SCREEN: &str = "\x1b[?1049l";

/// `cvc` as a person's terminal runs it: a terminal type that can redraw, a
/// UTF-8 locale, and no `NO_COLOR` from the developer's environment.
fn tui_command(fixture: &Fixture, args: &[&str]) -> Command {
    let mut command = fixture.cvc_command(args);
    command
        .env("TERM", "xterm-256color")
        .env("LC_ALL", "C.UTF-8")
        .env_remove("NO_COLOR");
    command
}

fn fixture_with_a_conversation() -> Fixture {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    fixture
}

#[test]
fn the_view_enters_the_alternate_screen_draws_its_header_and_quits_on_q() {
    let fixture = fixture_with_a_conversation();
    let mut session =
        PtySession::spawn_sized(tui_command(&fixture, &["conversations"]), b"", 120, 40);

    session.wait_for_text(ENTER_ALTERNATE_SCREEN);
    let screen = session.wait_for_screen_text("CVC conversations").join("\n");
    assert!(screen.contains("remote: origin"), "{screen}");
    assert!(
        screen.contains("publishing: not authorized yet"),
        "{screen}"
    );
    assert!(screen.contains("Run: printf"), "{screen}");
    assert!(screen.contains("q quit"), "{screen}");

    // While the view is up the terminal is in raw mode.
    assert!(!session.terminal_is_cooked());

    session.write(b"q");
    let status = session.wait_for_exit();
    let output = session.output();
    assert!(status.success(), "{output}");
    assert!(session.terminal_is_cooked(), "raw mode was left on");
    let entered = output.find(ENTER_ALTERNATE_SCREEN).unwrap();
    let left = output
        .rfind(LEAVE_ALTERNATE_SCREEN)
        .expect("the screen is given back");
    assert!(entered < left, "{output:?}");
}

#[test]
fn ctrl_c_quits_and_gives_the_terminal_back() {
    let fixture = fixture_with_a_conversation();
    let mut session =
        PtySession::spawn_sized(tui_command(&fixture, &["conversations"]), b"", 120, 40);
    session.wait_for_screen_text("CVC conversations");

    // In raw mode Ctrl-C is a key, not a signal.
    session.write(&[0x03]);
    let status = session.wait_for_exit();
    let output = session.output();
    assert!(status.success(), "{output}");
    assert!(output.contains(LEAVE_ALTERNATE_SCREEN), "{output:?}");
    assert!(session.terminal_is_cooked(), "raw mode was left on");
}

#[test]
fn a_terminate_signal_gives_the_terminal_back_and_still_ends_the_process() {
    use std::os::unix::process::ExitStatusExt;

    let fixture = fixture_with_a_conversation();
    let mut session =
        PtySession::spawn_sized(tui_command(&fixture, &["conversations"]), b"", 120, 40);
    session.wait_for_screen_text("CVC conversations");
    assert!(!session.terminal_is_cooked());

    session.signal(libc::SIGTERM);
    let status = session.wait_for_exit();
    let output = session.output();
    // The process dies of the signal, as it would have without the view ...
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
    // ... but not before the screen and the terminal modes were put back.
    assert!(output.contains(LEAVE_ALTERNATE_SCREEN), "{output:?}");
    assert!(session.terminal_is_cooked(), "raw mode was left on");
}

#[test]
fn plain_prints_the_listing_without_the_alternate_screen() {
    let fixture = fixture_with_a_conversation();
    let session = PtySession::spawn_sized(
        tui_command(&fixture, &["conversations", "--plain"]),
        b"",
        120,
        40,
    );
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert!(!output.contains(ENTER_ALTERNATE_SCREEN), "{output:?}");
    assert!(
        output.contains("Conversations (most recent first); share state for remote 'origin'"),
        "{output}"
    );
    assert!(output.contains("Run: printf"), "{output}");
}

#[test]
fn a_window_that_is_too_small_gets_the_plain_listing_and_says_why() {
    let fixture = fixture_with_a_conversation();
    let session = PtySession::spawn_sized(tui_command(&fixture, &["conversations"]), b"", 52, 9);
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert!(!output.contains(ENTER_ALTERNATE_SCREEN), "{output:?}");
    assert!(
        output.contains(
            "Interactive view unavailable (terminal too small, 52x9). Showing the plain list."
        ),
        "{output}"
    );
    assert!(
        output.contains("Conversations (most recent first)"),
        "{output}"
    );
}

#[test]
fn an_empty_repository_says_so_with_one_hint_and_never_takes_over_the_screen() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let empty = |fixture: &Fixture| {
        let session =
            PtySession::spawn_sized(tui_command(fixture, &["conversations"]), b"", 120, 40);
        let (status, output) = session.wait_exit();
        assert!(status.success(), "{output}");
        assert!(!output.contains(ENTER_ALTERNATE_SCREEN), "{output:?}");
        assert!(
            output.contains("No conversations captured yet."),
            "{output}"
        );
        output
    };

    // Capture has not been acknowledged yet.
    let output = empty(&fixture);
    assert!(
        output.contains("Passive capture is off. Turn it on with: cvc privacy acknowledge-capture"),
        "{output}"
    );

    // Capture is on, but this checkout does not record Claude Code sessions.
    cvc_core::privacy::acknowledge_capture(&fixture.open_repo()).unwrap();
    let output = empty(&fixture);
    assert!(
        output.contains("To record Claude Code sessions here: cvc harness install claude-code"),
        "{output}"
    );
    assert!(!output.contains("Passive capture is off"), "{output}");

    // Both are done.
    fixture.cvc_ok(&["harness", "install", "claude-code"]);
    let output = empty(&fixture);
    assert!(
        output.contains("Conversations appear here after a session has been captured."),
        "{output}"
    );
    assert!(
        !output.contains("To record Claude Code sessions"),
        "{output}"
    );
}
