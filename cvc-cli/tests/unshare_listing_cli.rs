#![cfg(unix)]

//! After `cvc unshare`, a conversation is private again: the listing says so
//! and the interactive `cvc share` picker offers it once more. Unsharing keeps
//! a conversation-level policy row (future turns private) behind, which used to
//! make the conversation read as shared with nothing in it.

mod common;

use common::{Fixture, PtySession};

fn answer_the_one_challenge(fixture: &Fixture, args: &[&str], expected_prefix: &str) -> String {
    let mut session = PtySession::spawn(fixture.cvc_command(args), b"");
    let challenge = session.wait_for_challenge(1);
    assert!(challenge.starts_with(expected_prefix), "{challenge}");
    session.write(format!("{challenge}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    output
}

/// The listing line for `id`, and the title line under it.
fn listing_entry(listing: &str, id: &str) -> String {
    let lines: Vec<_> = listing.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.contains(id))
        .unwrap_or_else(|| panic!("{id} is not listed:\n{listing}"));
    lines[at].to_owned()
}

fn picker_offers(fixture: &Fixture, id: &str) -> bool {
    let mut session = PtySession::spawn(fixture.cvc_command(&["share", "--remote", "origin"]), b"");
    // The picker reads a selection line; a blank one aborts without a challenge.
    let shown = session.wait_for_text("Share which conversation?");
    session.write(b"\n");
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert!(!output.contains("Type exactly"), "{output}");
    shown.contains(id)
}

fn captured_conversation(fixture: &Fixture) -> String {
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    fixture.only_run_conversation_id()
}

#[test]
fn an_unshared_conversation_lists_as_private_and_the_picker_offers_it_again() {
    let fixture = Fixture::new();
    let id = captured_conversation(&fixture);
    assert!(picker_offers(&fixture, &id));

    answer_the_one_challenge(&fixture, &["share", &id, "--remote", "origin"], "I SHARE ");
    let listing = fixture.cvc_ok(&["conversations"]);
    assert!(
        listing_entry(&listing, &id).contains("[shared, 0/1 published]"),
        "{listing}"
    );
    // A shared conversation is not offered again.
    let session = PtySession::spawn(fixture.cvc_command(&["share", "--remote", "origin"]), b"");
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert!(
        output.contains("No unshared conversations for remote 'origin'."),
        "{output}"
    );

    let output = answer_the_one_challenge(
        &fixture,
        &["unshare", &id, "--remote", "origin"],
        "I UNSHARE ",
    );
    assert!(
        output.contains("1 unpublished turn(s) made private."),
        "{output}"
    );

    let listing = fixture.cvc_ok(&["conversations"]);
    let entry = listing_entry(&listing, &id);
    assert!(entry.contains("[private]"), "{listing}");
    assert!(!entry.contains("[shared"), "{listing}");
    assert!(picker_offers(&fixture, &id));

    // And it can be shared again, this time with future turns.
    answer_the_one_challenge(
        &fixture,
        &["share", &id, "--remote", "origin", "--future"],
        "I SHARE ",
    );
    let listing = fixture.cvc_ok(&["conversations"]);
    assert!(
        listing_entry(&listing, &id).contains("[shared, 0/1 published]"),
        "{listing}"
    );
}
