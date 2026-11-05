#![cfg(unix)]

//! A destination's fingerprint is computed over its push URL without the user
//! information it may carry, so the consent, shares and publications of a
//! remote whose push URL embeds a user and a token are recorded under the same
//! fingerprint as the URL without them. What an earlier version recorded under
//! the fingerprint of the whole URL reads the same from the first command that
//! looks at the destination, and nothing is left under the old one.
//!
//! The remote problems a command reports are worded as remote problems.

mod common;

use common::Fixture;
use cvc_core::privacy::{
    acknowledge_sharing_destination, remote_destination, set_auto_push_destination,
    RemoteDestination,
};
use rusqlite::{params, Connection};

/// The push URL, with a user and a token in it. Nothing in this test fetches
/// or pushes, so the host is never contacted.
const PUSH_URL: &str = "https://deploy:tok-3xample@example.invalid/acme/project.git";

/// Every column of the local database that holds a destination fingerprint.
const DESTINATION_COLUMNS: [(&str, &str); 10] = [
    ("conversation_shares", "remote_fingerprint"),
    ("interaction_shares", "remote_fingerprint"),
    ("publications", "remote_fingerprint"),
    ("tombstones", "remote_fingerprint"),
    ("derivation_authorizations", "remote_fingerprint"),
    ("range_authorizations", "remote_fingerprint"),
    ("derivation_observations", "source_fingerprint"),
    ("derivation_observations", "source_key"),
    ("range_observations", "source_key"),
    ("range_observations", "source_remote"),
];

/// A captured conversation, and what an earlier version recorded for `origin`
/// once its push URL had user information in it: sharing consent with auto-push
/// on, the conversation shared, and its thought published, all under the
/// fingerprint of the whole URL. Returns the conversation id, the destination
/// as this version resolves it, and the earlier fingerprint.
fn recorded_by_an_earlier_version(fixture: &Fixture) -> (String, RemoteDestination, String) {
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    let conversation = fixture.only_run_conversation_id();
    fixture.git(&["remote", "set-url", "--push", "origin", PUSH_URL]);

    let repo = fixture.open_repo();
    let destination = remote_destination(&repo, "origin").unwrap();
    let legacy = destination
        .legacy_fingerprint
        .clone()
        .expect("the push URL has user information");
    assert_ne!(legacy, destination.fingerprint);
    let earlier = RemoteDestination {
        fingerprint: legacy.clone(),
        legacy_fingerprint: None,
        ..destination.clone()
    };
    acknowledge_sharing_destination(&repo, &earlier).unwrap();
    set_auto_push_destination(&repo, &earlier, true).unwrap();
    let connection = Connection::open(fixture.index_db_path()).unwrap();
    for statement in [
        "INSERT INTO conversation_shares(conversation_id, remote_fingerprint, share_future) VALUES(?1, ?2, 0)",
        "INSERT INTO interaction_shares(interaction_id, remote_fingerprint) SELECT id, ?2 FROM interactions WHERE conversation_id = ?1",
        "INSERT INTO publications(interaction_id, remote_fingerprint, state, updated_at) SELECT id, ?2, 'published', 0 FROM interactions WHERE conversation_id = ?1",
    ] {
        connection
            .execute(statement, params![conversation, legacy])
            .unwrap();
    }
    (conversation, destination, legacy)
}

/// How many rows of the local database hold `fingerprint`, over every column
/// that names a destination.
fn rows_under(fixture: &Fixture, fingerprint: &str) -> i64 {
    let connection = Connection::open(fixture.index_db_path()).unwrap();
    DESTINATION_COLUMNS
        .iter()
        .map(|(table, column)| {
            connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                    [fingerprint],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
        })
        .sum()
}

fn privacy_json(fixture: &Fixture) -> String {
    let path = fixture
        .open_repo()
        .commondir()
        .join("cvc")
        .join("privacy.json");
    std::fs::read_to_string(path).unwrap()
}

/// That nothing is left under the earlier fingerprint, and nothing of it or of
/// the credential reached `output`.
fn nothing_left_under(fixture: &Fixture, legacy: &str, output: &str) {
    assert_eq!(rows_under(fixture, legacy), 0);
    let state = privacy_json(fixture);
    assert!(!state.contains(legacy), "{state}");
    assert!(!output.contains(legacy), "{output}");
    assert!(!output.contains("tok-3xample"), "{output}");
}

fn line_of(listing: &str, id: &str) -> String {
    listing
        .lines()
        .find(|line| line.contains(id))
        .unwrap_or_else(|| panic!("{id} is not listed:\n{listing}"))
        .to_owned()
}

#[test]
fn privacy_status_reads_what_an_earlier_version_recorded_and_moves_it() {
    let fixture = Fixture::new();
    let (conversation, destination, legacy) = recorded_by_an_earlier_version(&fixture);
    assert_eq!(rows_under(&fixture, &legacy), 3);
    assert_eq!(rows_under(&fixture, &destination.fingerprint), 0);

    let status = fixture.cvc_ok(&["privacy", "status", "--remote", "origin"]);
    // Consent and auto-push read as they did, under the fingerprint of the URL
    // without user information, and nothing else is printed.
    assert_eq!(
        status,
        format!(
            "capture_acknowledged: false\nsharing_consented: true\nauto_push: true\nremote: origin\ndestination_fingerprint: {}\n",
            destination.fingerprint
        )
    );
    nothing_left_under(&fixture, &legacy, &status);
    assert_eq!(rows_under(&fixture, &destination.fingerprint), 3);

    // The share and the publication too.
    let listing = fixture.cvc_ok(&["conversations", "--plain", "--remote", "origin"]);
    assert!(
        line_of(&listing, &conversation).ends_with("[shared, 1 published]"),
        "{listing}"
    );
    assert!(
        listing.contains(&format!("({}", &destination.fingerprint[..12])),
        "{listing}"
    );
    nothing_left_under(&fixture, &legacy, &listing);

    // The same destination whichever credentials the push URL carries, and so
    // the same consent.
    fixture.git(&[
        "remote",
        "set-url",
        "--push",
        "origin",
        "https://other:s3cret@example.invalid/acme/project.git",
    ]);
    let again = fixture.cvc_ok(&["privacy", "status", "--remote", "origin"]);
    assert_eq!(again, status);
}

#[test]
fn the_conversation_listing_reads_what_an_earlier_version_recorded() {
    let fixture = Fixture::new();
    let (conversation, destination, legacy) = recorded_by_an_earlier_version(&fixture);

    // The listing is the first command to look at the destination.
    let listing = fixture.cvc_ok(&["conversations", "--plain", "--remote", "origin"]);
    assert!(
        line_of(&listing, &conversation).ends_with("[shared, 1 published]"),
        "{listing}"
    );
    nothing_left_under(&fixture, &legacy, &listing);
    let status = fixture.cvc_ok(&["privacy", "status", "--remote", "origin"]);
    assert!(
        status.contains("sharing_consented: true\nauto_push: true\n"),
        "{status}"
    );
    assert!(status.ends_with(&format!(
        "destination_fingerprint: {}\n",
        destination.fingerprint
    )));
}

#[test]
fn a_remote_without_user_information_keeps_its_records_where_they_are() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let destination = fixture.consent_to_sharing();
    assert_eq!(destination.legacy_fingerprint, None);
    let state = privacy_json(&fixture);
    let status = fixture.cvc_ok(&["privacy", "status", "--remote", "origin"]);
    assert!(status.contains("sharing_consented: true\n"), "{status}");
    assert!(status.ends_with(&format!(
        "destination_fingerprint: {}\n",
        destination.fingerprint
    )));
    assert_eq!(privacy_json(&fixture), state);
}

#[test]
fn a_remote_that_is_not_configured_is_reported_as_one() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    for args in [
        &["privacy", "status", "--remote", "typo"][..],
        &["share", "some-conversation", "--remote", "typo"],
        &["unshare", "some-conversation", "--remote", "typo"],
        &["privacy", "acknowledge-sharing", "--remote", "typo"],
    ] {
        let output = fixture.cvc(args);
        assert!(!output.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr, "remote is not configured\n", "{args:?}");
    }
}
