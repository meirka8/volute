#![cfg(unix)]

//! Typed acknowledgements through the real binary on a pseudo-terminal.
//!
//! The challenge must be answered by input typed *after* the prompt was
//! displayed. Input that was already queued on the terminal, whether pasted,
//! typed ahead, or left over from a preceding challenge in the same command,
//! must never satisfy it.

mod common;

use common::{Fixture, PtySession};

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
        output.contains("Published 1 shared interaction(s) to 'origin'"),
        "{output}"
    );
    let remote = git2::Repository::open_bare(&fixture.remote).unwrap();
    assert!(remote.find_reference("refs/cvc/main").is_ok());
}
