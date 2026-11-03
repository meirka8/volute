#![cfg(unix)]

//! After `cvc unshare`, a conversation is private again: the listing says so
//! and the interactive `cvc share` picker offers it once more. Unsharing keeps
//! a conversation-level policy row (future thoughts private) behind, which used to
//! make the conversation read as shared with nothing in it.
//!
//! An unshare the store would refuse, because the remote holds some of the
//! conversation or a publish of it did not finish, is refused before any phrase
//! is asked.

mod common;

use common::{challenges, Fixture, PtySession};
use rusqlite::{params, Connection};

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
        output.contains("1 unpublished thought(s) made private."),
        "{output}"
    );

    let listing = fixture.cvc_ok(&["conversations"]);
    let entry = listing_entry(&listing, &id);
    assert!(entry.contains("[private]"), "{listing}");
    assert!(!entry.contains("[shared"), "{listing}");
    assert!(picker_offers(&fixture, &id));

    // And it can be shared again, this time with future thoughts.
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

#[test]
fn an_unshare_the_store_would_refuse_is_refused_before_any_phrase_is_asked() {
    let fixture = Fixture::new();
    let published = captured_conversation(&fixture);
    fixture.consent_to_sharing();
    answer_the_one_challenge(
        &fixture,
        &["share", &published, "--remote", "origin"],
        "I SHARE ",
    );
    answer_the_one_challenge(
        &fixture,
        &["push", "--manual", "--remote", "origin"],
        "I PUBLISH ",
    );

    // A second conversation, shared, with a publish that did not conclude
    // cleanly: what a push that failed ambiguously leaves behind.
    fixture.cvc_ok(&["run", "--", "printf", "second"]);
    let unfinished = fixture
        .run_conversation_ids()
        .into_iter()
        .find(|id| id != &published)
        .expect("the second run is a distinct conversation");
    answer_the_one_challenge(
        &fixture,
        &["share", &unfinished, "--remote", "origin"],
        "I SHARE ",
    );
    let destination =
        cvc_core::privacy::remote_destination(&fixture.open_repo(), "origin").unwrap();
    Connection::open(fixture.index_db_path())
        .unwrap()
        .execute(
            "INSERT INTO publications(interaction_id, remote_fingerprint, state, updated_at)
             SELECT id, ?2, 'unknown', 0 FROM interactions WHERE conversation_id = ?1",
            params![unfinished, destination.fingerprint],
        )
        .unwrap();
    let before = fixture.cvc_ok(&["conversations"]);
    assert!(
        listing_entry(&before, &published).contains("[shared, 1 published]"),
        "{before}"
    );

    for (id, reason) in [
        (
            &published,
            "Cannot unshare: 1 thought is already published to 'origin', and published thoughts cannot be recalled.",
        ),
        (
            &unfinished,
            "Cannot unshare yet: an earlier publish to 'origin' did not finish cleanly. Run: cvc privacy reconcile --remote origin",
        ),
    ] {
        // On a terminal, where a phrase could be asked: the command ends by
        // itself, with the reason, and never shows the prompt.
        let session = PtySession::spawn(
            fixture.cvc_command(&["unshare", id, "--remote", "origin"]),
            b"",
        );
        let (status, output) = session.wait_exit();
        assert!(!status.success(), "{output}");
        assert!(output.contains(reason), "{output}");
        assert!(!output.contains("Type exactly"), "{output}");
        assert!(!output.contains("I UNSHARE"), "{output}");
        assert!(challenges(&output).is_empty(), "{output}");
    }

    // Neither refusal changed anything.
    assert_eq!(fixture.cvc_ok(&["conversations"]), before);
}
