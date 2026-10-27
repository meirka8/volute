#![cfg(unix)]

//! `cvc push --manual` through the real binary on a pseudo-terminal: the
//! consent line and the `I PUBLISH` challenge describe what this push newly
//! sends to the destination, not everything ever shared with it (issue #106).

mod common;

use common::{challenges, Fixture, PtySession};
use rusqlite::Connection;

/// A `publications.updated_at` far older than anything a run could write, so
/// "unchanged" cannot be confused with "rewritten within the same second".
const SENTINEL: i64 = 1_000;

fn share(fixture: &Fixture, id: &str) {
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["share", id, "--remote", "origin"]),
        b"",
    );
    let challenge = session.wait_for_challenge(1);
    assert!(challenge.starts_with("I SHARE "), "{challenge}");
    session.write(format!("{challenge}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
}

struct ManualPush {
    challenge: String,
    /// Everything displayed up to the moment the challenge was answered.
    before_answer: String,
    output: String,
}

fn push_manually(fixture: &Fixture) -> ManualPush {
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["push", "--manual", "--remote", "origin"]),
        b"",
    );
    let challenge = session.wait_for_challenge(1);
    let before_answer = session.output();
    session.write(format!("{challenge}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    ManualPush {
        challenge,
        before_answer,
        output,
    }
}

fn last_token(challenge: &str) -> &str {
    challenge.rsplit(' ').next().unwrap()
}

/// `(state, updated_at)` of the single publication row of `conversation`.
fn publication_of(fixture: &Fixture, conversation: &str) -> (String, i64) {
    let db = Connection::open(fixture.index_db_path()).unwrap();
    db.query_row(
        "SELECT p.state, p.updated_at FROM publications p \
         JOIN interactions i ON i.id = p.interaction_id WHERE i.conversation_id = ?1",
        [conversation],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

fn set_publication_timestamp(fixture: &Fixture, conversation: &str, updated_at: i64) {
    let db = Connection::open(fixture.index_db_path()).unwrap();
    let changed = db
        .execute(
            "UPDATE publications SET updated_at = ?2 WHERE interaction_id IN \
             (SELECT id FROM interactions WHERE conversation_id = ?1)",
            rusqlite::params![conversation, updated_at],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

fn remote_node_count(fixture: &Fixture) -> usize {
    let remote = git2::Repository::open_bare(&fixture.remote).unwrap();
    let tree = remote
        .find_reference("refs/cvc/main")
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .tree()
        .unwrap();
    let nodes = tree
        .get_name("nodes")
        .unwrap()
        .to_object(&remote)
        .unwrap()
        .peel_to_tree()
        .unwrap();
    nodes
        .iter()
        .map(|shard| {
            shard
                .to_object(&remote)
                .unwrap()
                .peel_to_tree()
                .unwrap()
                .len()
        })
        .sum()
}

#[test]
fn a_second_push_asks_for_and_reports_only_what_is_new() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "first"]);
    fixture.consent_to_sharing();
    let first = fixture.only_run_conversation_id();

    // First conversation: share and publish. Everything is new, so the count
    // is the whole projection and the line does not repeat it as a total.
    share(&fixture, &first);
    let initial = push_manually(&fixture);
    assert_eq!(last_token(&initial.challenge), "1", "{}", initial.challenge);
    assert!(
        initial
            .before_answer
            .contains("Publishing 1 new shared thought(s) to 'origin'.\r\nType exactly"),
        "{}",
        initial.before_answer
    );
    assert!(
        initial
            .output
            .contains("Published 1 new shared thought(s) to 'origin'."),
        "{}",
        initial.output
    );
    assert_eq!(remote_node_count(&fixture), 1);

    // Second conversation shared on top of the published first one.
    fixture.cvc_ok(&["run", "--", "printf", "second"]);
    let second = fixture
        .run_conversation_ids()
        .into_iter()
        .find(|id| id != &first)
        .expect("the second run is a distinct conversation");
    share(&fixture, &second);
    set_publication_timestamp(&fixture, &first, SENTINEL);

    let push = push_manually(&fixture);
    // The count in the challenge is what this push adds, not the 2 shared.
    assert_eq!(last_token(&push.challenge), "1", "{}", push.challenge);
    assert!(
        push.challenge.starts_with("I PUBLISH "),
        "{}",
        push.challenge
    );
    let summary = "Publishing 1 new shared thought(s) to 'origin' (2 shared in total).";
    let summary_at = push
        .before_answer
        .find(summary)
        .unwrap_or_else(|| panic!("no summary line:\n{}", push.before_answer));
    let prompt_at = push.before_answer.find("Type exactly").unwrap();
    assert!(
        push.before_answer[summary_at + summary.len()..prompt_at]
            .trim()
            .is_empty(),
        "the summary must sit directly above the prompt:\n{}",
        push.before_answer
    );
    assert!(
        push.output
            .contains("Published 1 new shared thought(s) to 'origin' (2 shared in total)."),
        "{}",
        push.output
    );
    assert_eq!(remote_node_count(&fixture), 2);

    // The already-published conversation was not touched; the new one is stamped.
    assert_eq!(
        publication_of(&fixture, &first),
        ("published".to_owned(), SENTINEL)
    );
    let (state, stamped) = publication_of(&fixture, &second);
    assert_eq!(state, "published");
    assert!(stamped > SENTINEL);

    // Nothing differs from the destination now: say so, ask for nothing, and
    // change no state.
    let listing_before = fixture.cvc_ok(&["conversations"]);
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["push", "--manual", "--remote", "origin"]),
        b"",
    );
    let (status, output) = {
        session.wait_for_text("No new shared thoughts to publish to 'origin'");
        session.wait_exit()
    };
    assert!(status.success(), "{output}");
    assert!(
        output.contains("No new shared thoughts to publish to 'origin' (2 shared in total)."),
        "{output}"
    );
    assert!(challenges(&output).is_empty(), "{output}");
    assert!(!output.contains("Published"), "{output}");
    assert_eq!(fixture.cvc_ok(&["conversations"]), listing_before);
    assert_eq!(
        publication_of(&fixture, &first),
        ("published".to_owned(), SENTINEL)
    );
    assert_eq!(
        publication_of(&fixture, &second),
        ("published".to_owned(), stamped)
    );
    assert_eq!(remote_node_count(&fixture), 2);
}
