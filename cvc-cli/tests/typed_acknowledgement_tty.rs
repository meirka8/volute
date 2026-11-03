#![cfg(unix)]

//! Typed acknowledgements through the real binary on a pseudo-terminal.
//!
//! The challenge must be answered by input typed *after* the prompt was
//! displayed. Input that was already queued on the terminal, whether pasted,
//! typed ahead, or left over from a preceding challenge in the same command,
//! must never satisfy it.

mod common;

use common::{Fixture, PtySession};
use std::time::Duration;

#[test]
fn input_queued_before_the_prompt_cannot_answer_a_challenge() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);

    // The correct answer is on the terminal before the process even starts.
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["privacy", "acknowledge-capture"]),
        b"I UNDERSTAND LOCAL CAPTURE\n",
    );
    assert_eq!(session.wait_for_challenge(1), "I UNDERSTAND LOCAL CAPTURE");
    session.write(b"something else entirely\n");
    let (status, output) = session.wait_exit();
    assert!(!status.success(), "{output}");
    assert!(
        output.contains("acknowledgement challenge did not match"),
        "{output}"
    );
    let status = fixture.cvc_ok(&["privacy", "status"]);
    assert!(status.contains("capture_acknowledged: false"), "{status}");

    // Control: the same answer typed after the prompt is accepted.
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["privacy", "acknowledge-capture"]),
        b"",
    );
    assert_eq!(session.wait_for_challenge(1), "I UNDERSTAND LOCAL CAPTURE");
    session.write(b"I UNDERSTAND LOCAL CAPTURE\n");
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    let status = fixture.cvc_ok(&["privacy", "status"]);
    assert!(status.contains("capture_acknowledged: true"), "{status}");
}

#[test]
fn a_chained_challenge_waits_for_fresh_input_instead_of_the_previous_lines_type_ahead() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    fixture.consent_to_sharing();
    let id = fixture.only_run_conversation_id();

    // `share --push` chains `I SHARE` and `I PUBLISH`. Answer the first and
    // deliver a second line in the same write, as a paste would.
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["share", &id, "--remote", "origin", "--push"]),
        b"",
    );
    let share = session.wait_for_challenge(1);
    assert!(share.starts_with("I SHARE "), "{share}");
    session.write(format!("{share}\nSTALE LINE FROM THE PASTE\n").as_bytes());

    // The second prompt must be displayed and then wait, rather than being
    // answered by the stale line in the same breath as it is printed.
    let publish = session.wait_for_challenge(2);
    assert!(publish.starts_with("I PUBLISH "), "{publish}");
    let output = session.output();
    assert!(
        !output.contains("acknowledgement challenge did not match"),
        "{output}"
    );
    session.write(format!("{publish}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert!(
        output.contains("Published 1 new shared thought(s) to 'origin'."),
        "{output}"
    );
    let remote = git2::Repository::open_bare(&fixture.remote).unwrap();
    assert!(remote.find_reference("refs/cvc/main").is_ok());
}

#[test]
fn an_answer_that_is_not_text_does_not_confirm_a_challenge_and_reads_as_not_matching() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);

    // The right phrase, followed by bytes that are not UTF-8: not an answer.
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["privacy", "acknowledge-capture"]),
        b"",
    );
    assert_eq!(session.wait_for_challenge(1), "I UNDERSTAND LOCAL CAPTURE");
    session.write(b"I UNDERSTAND LOCAL CAPTURE\xff\xfe\n");
    let (status, output) = session.wait_exit();
    assert!(!status.success(), "{output}");
    // It reads as the phrase not matching, not as an I/O failure.
    assert!(
        output.contains("acknowledgement challenge did not match"),
        "{output}"
    );
    assert!(!output.contains("invalid"), "{output}");
    let status = fixture.cvc_ok(&["privacy", "status"]);
    assert!(status.contains("capture_acknowledged: false"), "{status}");

    // Control: the same phrase as text is accepted.
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["privacy", "acknowledge-capture"]),
        b"",
    );
    assert_eq!(session.wait_for_challenge(1), "I UNDERSTAND LOCAL CAPTURE");
    session.write(b"I UNDERSTAND LOCAL CAPTURE\n");
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    let status = fixture.cvc_ok(&["privacy", "status"]);
    assert!(status.contains("capture_acknowledged: true"), "{status}");
}

/// What `cvc privacy acknowledge-sharing --remote origin` shows right before it
/// asks for its phrase. The phrase itself, `I AUTHORIZE SHARING <fingerprint>`,
/// can be worked out from the remote before the command is even started.
const ABOUT_TO_ASK: &str = "Acknowledging immutable publication to remote 'origin'";

fn precomputed_phrase(fixture: &Fixture) -> String {
    let destination =
        cvc_core::privacy::remote_destination(&fixture.open_repo(), "origin").unwrap();
    format!("I AUTHORIZE SHARING {}", destination.fingerprint)
}

fn sharing_is_consented(fixture: &Fixture) -> bool {
    fixture
        .cvc_ok(&["privacy", "status", "--remote", "origin"])
        .contains("sharing_consented: true")
}

#[test]
fn a_phrase_sent_before_the_prompt_but_delivered_after_a_flush_cannot_answer_it() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let phrase = precomputed_phrase(&fixture);

    let mut session = PtySession::spawn(
        fixture.cvc_command(&["privacy", "acknowledge-sharing", "--remote", "origin"]),
        b"",
    );
    session.wait_for_text(ABOUT_TO_ASK);
    // Input that was sent before the prompt, and reaches the terminal after the
    // flush that precedes it (a terminal emulator whose write was blocked, a slow
    // link): filler, then a line-kill (^U) that erases the filler from the line
    // being typed, then the phrase and its Enter. If the prompt were shown, and
    // the line read, as soon as the flush is done, this would answer it.
    let mut chunks: Vec<(Duration, Vec<u8>)> = (0..15)
        .map(|_| (Duration::from_millis(20), b"filler".to_vec()))
        .collect();
    chunks.push((
        Duration::from_millis(100),
        format!("\x15{phrase}\n").into_bytes(),
    ));
    let sender = session.send_later(chunks);

    // The prompt is shown once the input has stopped, and then it waits.
    assert_eq!(session.wait_for_challenge(1), phrase);
    sender.join().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !session.has_exited(),
        "the phrase that was already on its way answered the challenge:\n{}",
        session.output()
    );
    assert!(!sharing_is_consented(&fixture));

    // Typed after the prompt, the same phrase is accepted.
    session.write(format!("{phrase}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert!(sharing_is_consented(&fixture));
}

#[test]
fn input_that_never_goes_quiet_gets_no_prompt_and_confirms_nothing() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let phrase = precomputed_phrase(&fixture);

    let mut session = PtySession::spawn(
        fixture.cvc_command(&["privacy", "acknowledge-sharing", "--remote", "origin"]),
        b"",
    );
    session.wait_for_text(ABOUT_TO_ASK);
    // Longer than the prompt waits: the phrase is in the stream, over and over.
    session.flood(
        format!("\x15{phrase}\n").as_bytes(),
        Duration::from_millis(20),
        Duration::from_secs(6),
    );

    let (status, output) = session.wait_exit();
    assert!(!status.success(), "{output}");
    // The usual refusal, with the reason in front of it, and no prompt shown.
    assert!(
        output.contains(
            "input kept arriving at the terminal, so no phrase was asked: acknowledgement challenge did not match"
        ),
        "{output}"
    );
    assert!(!output.contains("Type exactly"), "{output}");
    assert!(!sharing_is_consented(&fixture));
}
