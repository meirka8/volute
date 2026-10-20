#![cfg(unix)]

//! A conversation the remote holds is never listed as private, whether or not
//! this clone shares it. After an index rebuild or a re-ingest of a Claude Code
//! transcript the interaction ids are the same again, so the next fetch records
//! them as published at the remote with no share behind them; a conversation
//! imported by `cvc pull` is in the same state. Someone deciding whether to
//! `cvc redact` it must not read `[private]`.

mod common;
mod tui_support;

use common::{Fixture, PtySession};
use rusqlite::{params, Connection};
use tui_support::{
    add_a_second_remote, another_clone_of, authorized_with_a_conversation, fingerprint,
    listed_state, publish_with_the_command, share_record, share_with_the_command,
};

/// Three captured conversations and what the remote is made to hold of them:
/// the first is held for certain, the second only by a publish that did not
/// conclude, the third not at all, and a fourth is an import.
struct Held {
    published: String,
    unconfirmed: String,
    untouched: String,
    imported: String,
}

fn conversations_in_four_states(fixture: &Fixture) -> Held {
    fixture.cvc_ok(&["init"]);
    for _ in 0..4 {
        fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    }
    let ids = fixture.run_conversation_ids();
    assert_eq!(ids.len(), 4, "{ids:?}");
    let destination =
        cvc_core::privacy::remote_destination(&fixture.open_repo(), "origin").unwrap();
    let connection = Connection::open(fixture.index_db_path()).unwrap();
    let mark = |id: &str, state: &str| {
        connection
            .execute(
                "INSERT INTO publications(interaction_id, remote_fingerprint, state, updated_at)
                 SELECT id, ?2, ?3, 0 FROM interactions WHERE conversation_id = ?1",
                params![id, destination.fingerprint, state],
            )
            .unwrap();
    };
    mark(&ids[0], "published");
    mark(&ids[1], "pending");
    mark(&ids[3], "published");
    // What a pull does to the rows it imports.
    connection
        .execute(
            "UPDATE interactions SET capture_source = 'sync_import' WHERE conversation_id = ?1",
            params![ids[3]],
        )
        .unwrap();
    Held {
        published: ids[0].clone(),
        unconfirmed: ids[1].clone(),
        untouched: ids[2].clone(),
        imported: ids[3].clone(),
    }
}

/// The listing line for `id`.
fn line_of(listing: &str, id: &str) -> String {
    listing
        .lines()
        .find(|line| line.contains(id))
        .unwrap_or_else(|| panic!("{id} is not listed:\n{listing}"))
        .to_owned()
}

#[test]
fn the_plain_listing_never_calls_a_conversation_the_remote_holds_private() {
    let fixture = Fixture::new();
    let held = conversations_in_four_states(&fixture);

    let listing = fixture.cvc_ok(&["conversations", "--plain"]);
    assert!(
        line_of(&listing, &held.published).ends_with("[published, not shared here]"),
        "{listing}"
    );
    assert!(
        line_of(&listing, &held.unconfirmed).ends_with("[unconfirmed, not shared here]"),
        "{listing}"
    );
    assert!(
        line_of(&listing, &held.untouched).ends_with("[private]"),
        "{listing}"
    );
    // An import is held by the remote it came from, and not shared from here.
    assert!(
        line_of(&listing, &held.imported).ends_with("[published, not shared here]"),
        "{listing}"
    );
    assert_eq!(listing.matches("[private]").count(), 1, "{listing}");
    // A remote that is not configured has no destination, so no state is shown.
    let elsewhere = fixture.cvc(&["conversations", "--plain", "--remote", "nowhere"]);
    let elsewhere = String::from_utf8_lossy(&elsewhere.stdout);
    assert!(elsewhere.contains("no remote resolved"), "{elsewhere}");
    assert!(!elsewhere.contains("[private]"), "{elsewhere}");
    assert!(!elsewhere.contains("not shared here"), "{elsewhere}");
}

/// What `cvc share` with no id lists for `remote`, read from a pseudo-terminal.
/// The prompt that asks which one is answered with a blank line, which aborts
/// before any phrase is asked.
fn picker_for(fixture: &Fixture, remote: &str) -> String {
    let mut session = PtySession::spawn(fixture.cvc_command(&["share", "--remote", remote]), b"");
    let shown = session.wait_for_text("Share which conversation?");
    session.write(b"\n");
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert!(!output.contains("Type exactly"), "{output}");
    shown
}

#[test]
fn the_share_picker_offers_what_the_remote_holds_and_what_was_pulled() {
    let fixture = Fixture::new();
    let held = conversations_in_four_states(&fixture);

    let shown = picker_for(&fixture, "origin");
    for (id, label) in [
        (&held.published, "[published, not shared here]"),
        (&held.unconfirmed, "[unconfirmed, not shared here]"),
        (&held.untouched, "[private]"),
        // A pulled conversation is offered as well, and reads as held by the
        // remote it came from.
        (&held.imported, "[published, not shared here]"),
    ] {
        assert!(
            line_of(&shown, id).ends_with(label),
            "{id} should read {label}:\n{shown}"
        );
    }
    assert!(shown.contains("[1-4, blank to abort]"), "{shown}");
}

/// Git lets you push to one remote what you pulled from another, and so does
/// `cvc share`: a conversation pulled from a remote is offered like any other.
/// The interactive view keeps it read-only; the command and its picker do not.
#[test]
fn a_pulled_conversation_is_offered_by_the_share_picker_for_the_remote_it_came_from_and_for_another(
) {
    // One clone shares a conversation and publishes it ...
    let (author, id) = authorized_with_a_conversation();
    share_with_the_command(&author, &id);
    publish_with_the_command(&author);

    // ... another clone, which has a second remote of its own, pulls it.
    let reader = another_clone_of(&author);
    let pulled = reader.cvc_ok(&["pull"]);
    assert!(pulled.contains("Pulled 1 interaction(s)"), "{pulled}");
    add_a_second_remote(&reader);
    let imported = format!("remote:{}:{id}", fingerprint(&reader, "origin"));
    assert_eq!(
        listed_state(&reader, &imported),
        "[published, not shared here]"
    );

    // Offered for the remote it came from, which holds it ...
    let shown = picker_for(&reader, "origin");
    assert!(
        line_of(&shown, &imported).ends_with("[published, not shared here]"),
        "{shown}"
    );
    assert!(shown.contains("[1-1, blank to abort]"), "{shown}");
    // ... and for the other remote, which this clone has no record of holding
    // it: sharing what was pulled with a remote that has not got it.
    let shown = picker_for(&reader, "second");
    assert!(line_of(&shown, &imported).ends_with("[private]"), "{shown}");
    assert!(shown.contains("[1-1, blank to abort]"), "{shown}");

    // Choosing it asks the phrase of that destination, for the pulled
    // conversation. The phrase is not answered.
    let second = fingerprint(&reader, "second");
    let mut session = PtySession::spawn(reader.cvc_command(&["share", "--remote", "second"]), b"");
    session.wait_for_text("Share which conversation?");
    session.write(b"1\n");
    let phrase = session.wait_for_challenge(1);
    assert!(
        phrase.starts_with(&format!("I SHARE {imported} {second} 1 ")),
        "{phrase}"
    );
    drop(session);

    // Nothing was shared for either remote, and the listing reads as before.
    for remote in ["origin", "second"] {
        assert_eq!(
            share_record(&reader, &imported, &fingerprint(&reader, remote)).shared_thoughts,
            0,
            "{remote}"
        );
    }
    assert_eq!(
        listed_state(&reader, &imported),
        "[published, not shared here]"
    );
}
