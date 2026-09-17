#![cfg(unix)]

//! `cvc share --push` through the real binary on a pseudo-terminal: the
//! bundled publication honours the destination's standing auto-push grant
//! instead of always demanding a second challenge.

mod common;

use common::{challenges, Fixture, PtySession};

fn shared_run(fixture: &Fixture) -> String {
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    fixture.consent_to_sharing();
    fixture.only_run_conversation_id()
}

#[test]
fn share_push_with_auto_push_acknowledged_publishes_after_the_share_challenge_alone() {
    let fixture = Fixture::new();
    let id = shared_run(&fixture);
    fixture.enable_auto_push();

    let mut session = PtySession::spawn(
        fixture.cvc_command(&["share", &id, "--remote", "origin", "--push"]),
        b"",
    );
    let share = session.wait_for_challenge(1);
    assert!(share.starts_with("I SHARE "), "{share}");
    session.write(format!("{share}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert_eq!(challenges(&output), vec![share], "{output}");
    assert!(!output.contains("I PUBLISH"), "{output}");
    assert!(
        output.contains("Published 1 shared interaction(s) to 'origin'"),
        "{output}"
    );
    let remote = git2::Repository::open_bare(&fixture.remote).unwrap();
    assert!(remote.find_reference("refs/cvc/main").is_ok());
    let listing = fixture.cvc_ok(&["conversations"]);
    assert!(listing.contains("[shared, 1 published]"), "{listing}");
}

#[test]
fn share_push_without_auto_push_still_requires_the_publish_challenge() {
    let fixture = Fixture::new();
    let id = shared_run(&fixture);

    let mut session = PtySession::spawn(
        fixture.cvc_command(&["share", &id, "--remote", "origin", "--push"]),
        b"",
    );
    let share = session.wait_for_challenge(1);
    session.write(format!("{share}\n").as_bytes());
    let publish = session.wait_for_challenge(2);
    assert!(publish.starts_with("I PUBLISH "), "{publish}");

    // Declining the second challenge keeps the share and publishes nothing:
    // the share succeeded, the publication was refused.
    session.write(b"no\n");
    let (status, output) = session.wait_exit();
    assert!(!status.success(), "{output}");
    assert!(
        output.contains("acknowledgement challenge did not match"),
        "{output}"
    );
    let remote = git2::Repository::open_bare(&fixture.remote).unwrap();
    assert!(remote.find_reference("refs/cvc/main").is_err());
    let listing = fixture.cvc_ok(&["conversations"]);
    assert!(listing.contains("[shared, 0/1 published]"), "{listing}");
}
