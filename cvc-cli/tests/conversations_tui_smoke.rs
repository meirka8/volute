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
use std::time::{Duration, Instant};

const ENTER_ALTERNATE_SCREEN: &str = "\x1b[?1049h";
const LEAVE_ALTERNATE_SCREEN: &str = "\x1b[?1049l";
/// Device status report: "where is the cursor?". A terminal answers it on
/// stdin; a test terminal, and some real ones, never do.
const ASK_CURSOR_POSITION: &str = "\x1b[6n";

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
    assert!(!output.contains(ASK_CURSOR_POSITION), "{output:?}");
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

#[test]
fn a_flow_brings_the_view_back_on_a_terminal_that_never_answers_cursor_queries() {
    let fixture = fixture_with_a_conversation();
    let mut session =
        PtySession::spawn_sized(tui_command(&fixture, &["conversations"]), b"", 120, 40);
    session.wait_for_screen_text("CVC conversations");

    // The remote is not authorized, so `s` offers to authorize it, and Enter
    // hands the terminal over to the phrase.
    session.write(b"s");
    session.wait_for_screen_text("is not authorized");
    session.write(b"\r");
    let phrase = session.wait_for_challenge(1);
    assert!(phrase.starts_with("I AUTHORIZE SHARING "), "{phrase}");
    session.write(b"not the phrase\n");

    // The view is back with the result. Resuming used to ask the terminal where
    // the cursor was and, unanswered, end the view with an error instead.
    session.wait_for_screen_text("Not authorized: the typed phrase did not match.");
    session.write(b"q");
    let status = session.wait_for_exit();
    let output = session.output();
    assert!(status.success(), "{output}");
    assert!(!output.contains(ASK_CURSOR_POSITION), "{output:?}");
    assert!(
        !output.contains("cursor position could not be read"),
        "{output:?}"
    );
}

/// The view on a repository whose `origin` is authorized for publishing, with
/// one captured conversation.
fn authorized_session() -> (Fixture, PtySession) {
    let fixture = fixture_with_a_conversation();
    fixture.consent_to_sharing();
    let mut session =
        PtySession::spawn_sized(tui_command(&fixture, &["conversations"]), b"", 120, 40);
    session.wait_for_screen_text("CVC conversations");
    (fixture, session)
}

const SETTINGS_CHANGED: &str =
    "Publishing settings for 'origin' changed; check them and press the key again.";

#[test]
fn a_share_asked_for_after_auto_push_was_turned_on_elsewhere_runs_nothing() {
    let (fixture, mut session) = authorized_session();
    session.write(b"s");
    session.wait_for_screen_text("not yet shared");

    // Another terminal turns auto-push on while the bar is open. The bar, and
    // the banner that would follow, say nothing of it.
    fixture.enable_auto_push();
    session.write(b"\r");
    let screen = session.wait_for_screen_text(SETTINGS_CHANGED);
    assert!(
        !session.output().contains("Type exactly"),
        "no phrase may be asked on settings the screen did not show"
    );
    // The screen is refreshed: the header says it loudly now.
    assert!(
        screen.iter().any(|row| row.contains("auto-push: ON")),
        "{screen:#?}"
    );

    // Pressing the key again shows the bar for the settings that apply.
    session.write(b"s");
    let screen = session.wait_for_screen_text("auto-push publishes them");
    assert!(screen.iter().any(|row| row.contains("Esc: cancel")));
    // Esc closes the bar. It is sent alone and waited for: a terminal reads an
    // Esc with a key right behind it as Alt and that key.
    session.write(b"\x1b");
    session.wait_for_screen_text("q quit");
    session.write(b"q");
    let status = session.wait_for_exit();
    assert!(status.success(), "{}", session.output());
}

#[test]
fn a_publish_asked_for_after_the_settings_changed_elsewhere_runs_nothing() {
    let (fixture, mut session) = authorized_session();
    session.write(b"p");
    session.wait_for_screen_text("Check 'origin' anyway?");

    fixture.enable_auto_push();
    session.write(b"\r");
    session.wait_for_screen_text(SETTINGS_CHANGED);
    let output = session.output();
    assert!(!output.contains("Type exactly"), "{output}");
    assert!(!output.contains("Checking 'origin' first"), "{output}");
    session.write(b"q");
    let status = session.wait_for_exit();
    assert!(status.success(), "{}", session.output());
}

#[test]
fn a_conversation_with_an_id_that_cannot_be_shown_safely_is_not_shared_or_unshared() {
    let fixture = fixture_with_a_conversation();
    fixture.consent_to_sharing();
    // An MCP client chooses the id of what it records. This one holds an escape
    // sequence, which the phrase to type would carry.
    let id = "'evil' || char(27) || '[2Jid'";
    rusqlite::Connection::open(fixture.index_db_path())
        .unwrap()
        .execute_batch(&format!(
            "PRAGMA foreign_keys = OFF;
             UPDATE conversations SET id = {id};
             UPDATE interactions SET conversation_id = {id};"
        ))
        .unwrap();
    let mut session =
        PtySession::spawn_sized(tui_command(&fixture, &["conversations"]), b"", 120, 40);
    session.wait_for_screen_text("Run: printf");

    for key in [b"s", b"u"] {
        session.write(key);
        let screen = session.wait_for_screen_text(
            "This conversation's id contains characters that cannot be shown safely, so it cannot be shared or unshared from here.",
        );
        assert!(screen.iter().any(|row| row.contains("any key: ok")));
        session.write(b"x");
    }
    assert!(!session.output().contains("Type exactly"));
    session.write(b"q");
    let status = session.wait_for_exit();
    assert!(status.success(), "{}", session.output());
}

/// `q` quits the view, so a stream of them that is acted on ends it.
const QUIT_KEYS: &[u8] = b"q";

/// Longer than the view once drained for (half a second).
const A_LONG_STREAM: Duration = Duration::from_millis(1200);

#[test]
fn keys_that_keep_arriving_when_the_view_starts_do_not_act_in_it() {
    let fixture = fixture_with_a_conversation();
    let started = Instant::now();
    let mut session =
        PtySession::spawn_sized(tui_command(&fixture, &["conversations"]), b"", 120, 40);
    // A paste into a terminal that does not bracket it, or a slow link: keys that
    // are still arriving after the view has started.
    session.flood(QUIT_KEYS, Duration::from_millis(10), A_LONG_STREAM);

    // The view is shown once the input has been quiet, which is after the stream.
    session.wait_for_screen_text("CVC conversations");
    assert!(
        started.elapsed() >= A_LONG_STREAM,
        "the view was ready after {:?}, while keys were still arriving",
        started.elapsed()
    );
    // None of the keys acted.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !session.has_exited(),
        "a key of the stream acted:\n{}",
        session.output()
    );
    assert!(!session.terminal_is_cooked());

    session.write(b"q");
    let status = session.wait_for_exit();
    assert!(status.success(), "{}", session.output());
}

/// A flow that ends in "Not authorized: the typed phrase did not match.", with
/// `then` typed at the terminal right after the wrong phrase.
fn failed_authorization_then(session: &mut PtySession, then: impl FnOnce(&PtySession)) {
    session.wait_for_screen_text("CVC conversations");
    session.write(b"s");
    session.wait_for_screen_text("is not authorized");
    session.write(b"\r");
    let phrase = session.wait_for_challenge(1);
    assert!(phrase.starts_with("I AUTHORIZE SHARING "), "{phrase}");
    session.write(b"not the phrase\n");
    then(session);
}

#[test]
fn keys_that_keep_arriving_after_a_flow_do_not_act_in_the_view() {
    let fixture = fixture_with_a_conversation();
    let mut session =
        PtySession::spawn_sized(tui_command(&fixture, &["conversations"]), b"", 120, 40);
    let mut flooded_at = None;
    failed_authorization_then(&mut session, |session| {
        session.flood(QUIT_KEYS, Duration::from_millis(10), A_LONG_STREAM);
        flooded_at = Some(Instant::now());
    });

    // The view comes back with the result once the input has been quiet.
    session.wait_for_screen_text("Not authorized: the typed phrase did not match.");
    assert!(
        flooded_at.unwrap().elapsed() >= A_LONG_STREAM,
        "the view was back after {:?}, while keys were still arriving",
        flooded_at.unwrap().elapsed()
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !session.has_exited(),
        "a key of the stream acted:\n{}",
        session.output()
    );

    session.write(b"q");
    let status = session.wait_for_exit();
    assert!(status.success(), "{}", session.output());
}

#[test]
fn input_that_never_stops_after_a_flow_closes_the_view_and_still_tells_the_result() {
    let fixture = fixture_with_a_conversation();
    let mut session =
        PtySession::spawn_sized(tui_command(&fixture, &["conversations"]), b"", 120, 40);
    failed_authorization_then(&mut session, |session| {
        // Longer than the view waits for the input to stop.
        session.flood(QUIT_KEYS, Duration::from_millis(10), Duration::from_secs(8));
    });

    let status = session.wait_for_exit();
    let output = session.output();
    assert!(!status.success(), "{output}");
    assert!(session.terminal_is_cooked(), "raw mode was left on");
    // The screen was given back, and on it what the flow came to and why the view
    // ended.
    let left = output
        .rfind(LEAVE_ALTERNATE_SCREEN)
        .expect("the screen is given back");
    let main_screen = &output[left + LEAVE_ALTERNATE_SCREEN.len()..];
    assert!(
        main_screen.contains("Not authorized: the typed phrase did not match."),
        "{main_screen:?}"
    );
    assert!(
        main_screen.contains(
            "input kept arriving at the terminal, so the view was closed without acting on any of it"
        ),
        "{main_screen:?}"
    );
}

#[test]
fn input_that_never_stops_when_the_view_starts_means_it_does_not_start() {
    let fixture = fixture_with_a_conversation();
    let mut session =
        PtySession::spawn_sized(tui_command(&fixture, &["conversations"]), b"", 120, 40);
    session.flood(QUIT_KEYS, Duration::from_millis(10), Duration::from_secs(8));

    let status = session.wait_for_exit();
    let output = session.output();
    assert!(!status.success(), "{output}");
    assert!(session.terminal_is_cooked(), "raw mode was left on");
    assert!(
        output.contains(
            "input kept arriving at the terminal, so the view was closed without acting on any of it"
        ),
        "{output:?}"
    );
    // No frame was ever drawn.
    assert!(!output.contains("CVC conversations"), "{output:?}");
}
