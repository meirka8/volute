#![cfg(unix)]

//! Text that did not come from a person typing at the terminal cannot act on
//! it. A title is whatever the harness or the first prompt line said (a Claude
//! Code `ai-title`, a subagent description), secret-scrubbed and nothing more,
//! so a prompt-injected agent can put escape sequences in one. Printed raw, they
//! redraw the "Sharing conversation for remote 'origin':" line above the
//! `I SHARE` prompt. The text of an error, which can be a remote server's, is
//! the same kind of text.

mod common;

use common::{Fixture, PtySession};
use rusqlite::{params, Connection};
use std::process::Output;

/// Cursor up, erase the line, set the window title, ring the bell, reverse the
/// text that follows, return to the start of the line, and then forge the line
/// above the prompt.
const HOSTILE_TITLE: &str = "Fix the build\u{1b}[1A\u{1b}[2K\u{1b}]0;pwned\u{7}\u{202e}\rSharing conversation for remote 'origin':";

/// What the title above reads as once its control and format characters are
/// replaced.
const NEUTRALIZED_TITLE: &str =
    "Fix the build?[1A?[2K?]0;pwned???Sharing conversation for remote 'origin':";

/// Whether `text` holds nothing that acts on a terminal. A line feed is how a
/// line ends, and a pseudo-terminal turns it into a carriage return and a line
/// feed on the way out, so that pair counts as one line feed. The set is spelled
/// out here and not taken from the code under test.
fn is_inert(text: &str) -> bool {
    text.replace("\r\n", "\n").chars().all(|c| {
        c == '\n'
            || !(c.is_control()
                || matches!(
                    c,
                    '\u{ad}'
                        | '\u{61c}'
                        | '\u{180e}'
                        | '\u{200b}'..='\u{200f}'
                        | '\u{2028}'..='\u{202e}'
                        | '\u{2060}'..='\u{2069}'
                        | '\u{feff}'
                        | '\u{fff9}'..='\u{fffb}'
                        | '\u{e0001}'..='\u{e007f}'
                ))
    })
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// One captured conversation whose title is the hostile one.
fn conversation_with_hostile_title(fixture: &Fixture) -> String {
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    let id = fixture.only_run_conversation_id();
    Connection::open(fixture.index_db_path())
        .unwrap()
        .execute(
            "UPDATE conversations SET title = ?1 WHERE id = ?2",
            params![HOSTILE_TITLE, id],
        )
        .unwrap();
    id
}

#[test]
fn the_plain_listing_prints_a_hostile_title_as_inert_text_on_one_line() {
    let fixture = Fixture::new();
    let id = conversation_with_hostile_title(&fixture);

    let output = fixture.cvc(&["conversations", "--plain"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let listing = stdout(&output);
    assert!(is_inert(&listing), "{listing:?}");
    let entry: Vec<_> = listing
        .lines()
        .skip_while(|line| !line.contains(&id))
        .take(2)
        .collect();
    assert_eq!(entry.len(), 2, "{listing:?}");
    assert_eq!(
        entry[1],
        format!("      {NEUTRALIZED_TITLE}"),
        "{listing:?}"
    );
    // The forged line did not become a line of its own.
    assert_eq!(
        listing.matches("Sharing conversation for remote").count(),
        1,
        "{listing:?}"
    );
}

#[test]
fn the_share_picker_prints_a_hostile_title_as_inert_text() {
    let fixture = Fixture::new();
    conversation_with_hostile_title(&fixture);

    let mut session = PtySession::spawn(fixture.cvc_command(&["share", "--remote", "origin"]), b"");
    let shown = session.wait_for_text("Share which conversation?");
    session.write(b"\n");
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert!(is_inert(&output), "{output:?}");
    assert!(shown.contains(NEUTRALIZED_TITLE), "{shown:?}");
}

#[test]
fn the_summary_before_the_share_challenge_cannot_be_rewritten_by_a_title() {
    let fixture = Fixture::new();
    let id = conversation_with_hostile_title(&fixture);

    let mut session = PtySession::spawn(
        fixture.cvc_command(&["share", &id, "--remote", "origin"]),
        b"",
    );
    let challenge = session.wait_for_challenge(1);
    assert!(challenge.starts_with("I SHARE "), "{challenge}");
    session.write(b"no\n");
    let (status, output) = session.wait_exit();
    assert!(!status.success(), "{output}");
    assert!(is_inert(&output), "{output:?}");
    let summary = output.replace("\r\n", "\n");
    let lines: Vec<_> = summary.lines().collect();
    let header = lines
        .iter()
        .position(|line| *line == "Sharing conversation for remote 'origin':")
        .unwrap_or_else(|| panic!("no summary header: {output:?}"));
    assert!(lines[header + 1].contains(&id), "{output:?}");
    assert_eq!(
        lines[header + 2],
        format!("      {NEUTRALIZED_TITLE}"),
        "{output:?}"
    );
    // The forgery is not a line of its own either.
    assert_eq!(
        summary.matches("Sharing conversation for remote").count(),
        2,
        "the real one and the neutralized title: {output:?}"
    );
    // Nothing was shared.
    let listing = fixture.cvc_ok(&["conversations", "--plain"]);
    assert!(listing.contains("[private]"), "{listing}");
}

#[test]
fn a_phrase_with_a_control_character_in_it_is_not_asked() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);

    let session = PtySession::spawn(
        fixture.cvc_command(&["share", "c\u{1b}[2J-1", "--remote", "origin"]),
        b"",
    );
    let (status, output) = session.wait_exit();
    assert!(!status.success(), "{output:?}");
    assert!(is_inert(&output), "{output:?}");
    assert!(output.contains("cannot be shown"), "{output:?}");
    assert!(output.contains("nothing was recorded"), "{output:?}");
    assert!(!output.contains("Type exactly"), "{output:?}");
}

#[test]
fn an_error_that_quotes_hostile_text_prints_it_as_inert_text() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);

    // The parser of the interaction id quotes the first character it refused.
    let output = fixture.cvc(&["delete-local", "\u{1b}[2J\u{1b}]0;pwned\u{7}\u{202e}x"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(is_inert(&message), "{message:?}");
    assert!(message.contains("invalid interaction id"), "{message:?}");
    assert!(message.contains("found `?` at 1"), "{message:?}");
}

#[test]
fn an_advisory_hook_warning_prints_hostile_text_as_inert_text_and_still_succeeds() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);

    // clap quotes the argument it did not recognize. It strips escape sequences
    // and controls from its messages itself; a bidirectional override it does not.
    let output = fixture.cvc(&["hook", "bogus\u{202e}x"]);
    assert!(
        output.status.success(),
        "a hook never fails the git operation"
    );
    let message = stderr(&output);
    assert!(message.starts_with("CVC Hook Warning: advisory hook skipped: "));
    assert!(is_inert(&message), "{message:?}");
    assert!(message.contains("'bogus?x'"), "{message:?}");
}

#[test]
fn a_command_line_mistake_keeps_the_lines_of_its_usage() {
    let fixture = Fixture::new();

    let output = fixture.cvc(&["nonsense"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(is_inert(&message), "{message:?}");
    assert!(message.lines().count() >= 3, "{message:?}");
    assert!(message.contains("\nUsage: cvc "), "{message:?}");
    assert!(!message.contains('?'), "{message:?}");
}
