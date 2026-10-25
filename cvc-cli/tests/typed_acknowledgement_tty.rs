#![cfg(unix)]

//! Typed acknowledgements through the real binary on a pseudo-terminal.
//!
//! The challenge must be answered by input typed *after* the prompt was
//! displayed. Input that was already queued on the terminal, whether pasted,
//! typed ahead, or left over from a preceding challenge in the same command,
//! must never satisfy it.
//!
//! A phrase that can be worked out in advance ends with a nonce drawn for its
//! prompt, so not even input prepared in advance and sent once the prompt is
//! shown can answer it. These tests read the nonce from the prompt, as a person
//! does, and type it back.

mod common;

use common::{split_nonce, with_another_nonce, without_nonce, Fixture, PtySession};
use std::time::Duration;

/// `cvc init` and one `cvc run` conversation (one thought); returns its id.
fn captured_conversation(fixture: &Fixture) -> String {
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    fixture.only_run_conversation_id()
}

/// The destination fingerprint of `origin`.
fn fingerprint(fixture: &Fixture) -> String {
    cvc_core::privacy::remote_destination(&fixture.open_repo(), "origin")
        .unwrap()
        .fingerprint
}

/// `cvc privacy status --remote origin`.
fn privacy_status(fixture: &Fixture) -> String {
    fixture.cvc_ok(&["privacy", "status", "--remote", "origin"])
}

/// The bracketed share state the plain listing gives `id`, such as `[private]`.
fn listed_state(fixture: &Fixture, id: &str) -> String {
    let listing = fixture.cvc_ok(&["conversations"]);
    let line = listing
        .lines()
        .find(|line| line.split_whitespace().any(|word| word == id))
        .unwrap_or_else(|| panic!("{id} is not listed:\n{listing}"));
    line[line
        .rfind('[')
        .unwrap_or_else(|| panic!("no state in {line:?}"))..]
        .to_owned()
}

/// One number the index database gives for `sql`.
fn count(fixture: &Fixture, sql: &str) -> i64 {
    rusqlite::Connection::open(fixture.index_db_path())
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

/// The phrase `cvc share <id> --remote origin` asks, learned by starting the
/// command and walking away from its prompt. A share's phrase has no nonce: it
/// is a function of the thoughts shared, so the next prompt for the same
/// snapshot asks the very same one.
fn share_phrase_seen_earlier(fixture: &Fixture, id: &str) -> String {
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["share", id, "--remote", "origin"]),
        b"",
    );
    session.wait_for_challenge(1)
}

/// Runs `cvc <args>` and answers its one challenge with what `answer` makes of
/// it. Returns the challenge, whether the command succeeded, and its output.
fn answer_the_challenge(
    fixture: &Fixture,
    args: &[&str],
    answer: impl FnOnce(&str) -> String,
) -> (String, bool, String) {
    let mut session = PtySession::spawn(fixture.cvc_command(args), b"");
    let challenge = session.wait_for_challenge(1);
    session.write(format!("{}\n", answer(&challenge)).as_bytes());
    let (status, output) = session.wait_exit();
    (challenge, status.success(), output)
}

/// Runs `cvc <args>` and types its one challenge back, as a person would.
fn type_the_challenge_back(fixture: &Fixture, args: &[&str]) {
    let (challenge, confirmed, output) = answer_the_challenge(fixture, args, str::to_owned);
    assert!(confirmed, "{args:?} with {challenge:?}:\n{output}");
}

#[test]
fn input_queued_before_the_prompt_cannot_answer_a_challenge() {
    let fixture = Fixture::new();
    let id = captured_conversation(&fixture);
    let share = ["share", id.as_str(), "--remote", "origin"];
    let phrase = share_phrase_seen_earlier(&fixture, &id);

    // The correct answer is on the terminal before the process even starts.
    let mut session = PtySession::spawn(
        fixture.cvc_command(&share),
        format!("{phrase}\n").as_bytes(),
    );
    assert_eq!(session.wait_for_challenge(1), phrase);
    session.write(b"something else entirely\n");
    let (status, output) = session.wait_exit();
    assert!(!status.success(), "{output}");
    assert!(
        output.contains("acknowledgement challenge did not match"),
        "{output}"
    );
    assert_eq!(listed_state(&fixture, &id), "[private]");

    // Control: the same answer typed after the prompt is accepted.
    let mut session = PtySession::spawn(fixture.cvc_command(&share), b"");
    assert_eq!(session.wait_for_challenge(1), phrase);
    session.write(format!("{phrase}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert_eq!(listed_state(&fixture, &id), "[shared, 0/1 published]");
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
        output.contains("Published 1 new shared interaction(s) to 'origin'."),
        "{output}"
    );
    let remote = git2::Repository::open_bare(&fixture.remote).unwrap();
    assert!(remote.find_reference("refs/cvc/main").is_ok());
}

#[test]
fn an_answer_that_is_not_text_does_not_confirm_a_challenge_and_reads_as_not_matching() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let acknowledge = ["privacy", "acknowledge-capture"];

    // The right phrase, nonce and all, followed by bytes that are not UTF-8:
    // not an answer.
    let mut session = PtySession::spawn(fixture.cvc_command(&acknowledge), b"");
    let challenge = session.wait_for_challenge(1);
    assert_eq!(without_nonce(&challenge), "I UNDERSTAND LOCAL CAPTURE");
    session.write(&[challenge.as_bytes(), b"\xff\xfe\n"].concat());
    let (status, output) = session.wait_exit();
    assert!(!status.success(), "{output}");
    // It reads as the phrase not matching, not as an I/O failure.
    assert!(
        output.contains("acknowledgement challenge did not match"),
        "{output}"
    );
    assert!(!output.contains("invalid"), "{output}");
    assert!(privacy_status(&fixture).contains("capture_acknowledged: false"));

    // Control: the phrase its prompt shows, as text, is accepted.
    type_the_challenge_back(&fixture, &acknowledge);
    assert!(privacy_status(&fixture).contains("capture_acknowledged: true"));
}

/// What `cvc share <id> --remote origin` shows right before it asks for its
/// phrase.
const ABOUT_TO_ASK_SHARE: &str = "Sharing conversation for remote 'origin':";

#[test]
fn a_phrase_sent_before_the_prompt_but_delivered_after_a_flush_cannot_answer_it() {
    let fixture = Fixture::new();
    let id = captured_conversation(&fixture);
    // A share's phrase has no nonce, so the one seen at an earlier prompt for
    // the same snapshot is the right answer: only the wait for quiet input
    // keeps it from answering a prompt it was sent before.
    let phrase = share_phrase_seen_earlier(&fixture, &id);

    let mut session = PtySession::spawn(
        fixture.cvc_command(&["share", &id, "--remote", "origin"]),
        b"",
    );
    session.wait_for_text(ABOUT_TO_ASK_SHARE);
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
    assert_eq!(listed_state(&fixture, &id), "[private]");

    // Typed after the prompt, the same phrase is accepted.
    session.write(format!("{phrase}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert_eq!(listed_state(&fixture, &id), "[shared, 0/1 published]");
}

/// What `cvc privacy acknowledge-sharing --remote origin` shows right before it
/// asks for its phrase.
const ABOUT_TO_ASK_AUTHORIZATION: &str = "Acknowledging immutable publication to remote 'origin'";

/// The phrase `cvc privacy acknowledge-sharing --remote origin` asks, without
/// the nonce of its prompt: `I AUTHORIZE SHARING <fingerprint>`, which can be
/// worked out from the remote before the command is even started.
fn precomputed_phrase(fixture: &Fixture) -> String {
    format!("I AUTHORIZE SHARING {}", fingerprint(fixture))
}

fn sharing_is_consented(fixture: &Fixture) -> bool {
    privacy_status(fixture).contains("sharing_consented: true")
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
    session.wait_for_text(ABOUT_TO_ASK_AUTHORIZATION);
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

// ---------------------------------------------------------------------------
// The nonce of a phrase that can be worked out in advance
// ---------------------------------------------------------------------------

/// `cvc <args>` asks `phrase` followed by a nonce, and only the nonce its own
/// prompt shows answers it. Each wrong answer is typed after the prompt is
/// shown, as input sent at the right moment by something that cannot see the
/// screen would be: the phrase alone (everything that could be worked out in
/// advance), the phrase with a nonce that is not this prompt's, and the whole
/// challenge of an earlier prompt. Each is refused and leaves `done` false. The
/// challenge the prompt shows is accepted and makes it true.
fn only_the_nonce_of_its_own_prompt_answers(
    fixture: &Fixture,
    args: &[&str],
    phrase: &str,
    done: impl Fn() -> bool,
) {
    assert!(!done(), "{args:?} is done before it was asked");
    let spawn = || PtySession::spawn(fixture.cvc_command(args), b"");
    let asked = |session: &mut PtySession| {
        let challenge = session.wait_for_challenge(1);
        assert_eq!(split_nonce(&challenge).0, phrase, "{args:?}");
        challenge
    };
    let refused = |mut session: PtySession, answer: &str, what: &str| {
        session.write(format!("{answer}\n").as_bytes());
        let (status, output) = session.wait_exit();
        assert!(!status.success(), "{what} answered {args:?}:\n{output}");
        assert!(
            output.contains("acknowledgement challenge did not match"),
            "{what}, {args:?}:\n{output}"
        );
        assert!(!done(), "{what} confirmed {args:?}:\n{output}");
    };

    let mut session = spawn();
    let earlier = asked(&mut session);
    refused(session, phrase, "the phrase without its nonce");

    let mut session = spawn();
    let challenge = asked(&mut session);
    refused(
        session,
        &with_another_nonce(&challenge),
        "the phrase with another nonce",
    );

    // Should this prompt draw the very nonce the earlier one did (one chance in
    // 2^25), it is asked again, so that the earlier answer is wrong for certain.
    // Four draws in a row that match it are not chance.
    let mut session = spawn();
    let mut challenge = asked(&mut session);
    for _ in 0..3 {
        if challenge != earlier {
            break;
        }
        session = spawn();
        challenge = asked(&mut session);
    }
    assert_ne!(
        challenge, earlier,
        "every prompt of {args:?} drew one nonce"
    );
    refused(session, &earlier, "the challenge of an earlier prompt");

    let mut session = spawn();
    let challenge = asked(&mut session);
    session.write(format!("{challenge}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{args:?} with {challenge:?}:\n{output}");
    assert!(done(), "{args:?} recorded nothing:\n{output}");
}

#[test]
fn the_capture_acknowledgement_needs_the_nonce_of_its_own_prompt() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    only_the_nonce_of_its_own_prompt_answers(
        &fixture,
        &["privacy", "acknowledge-capture"],
        "I UNDERSTAND LOCAL CAPTURE",
        || privacy_status(&fixture).contains("capture_acknowledged: true"),
    );
}

#[test]
fn the_sharing_authorization_needs_the_nonce_of_its_own_prompt() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    only_the_nonce_of_its_own_prompt_answers(
        &fixture,
        &["privacy", "acknowledge-sharing", "--remote", "origin"],
        &precomputed_phrase(&fixture),
        || sharing_is_consented(&fixture),
    );
}

#[test]
fn turning_auto_push_on_and_off_needs_the_nonce_of_its_own_prompt() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.consent_to_sharing();
    let fp = fingerprint(&fixture);
    only_the_nonce_of_its_own_prompt_answers(
        &fixture,
        &["privacy", "set-auto-push", "on", "--remote", "origin"],
        &format!("I AUTHORIZE AUTO PUSH ON {fp}"),
        || privacy_status(&fixture).contains("auto_push: true"),
    );
    only_the_nonce_of_its_own_prompt_answers(
        &fixture,
        &["privacy", "set-auto-push", "off", "--remote", "origin"],
        &format!("I AUTHORIZE AUTO PUSH OFF {fp}"),
        || privacy_status(&fixture).contains("auto_push: false"),
    );
}

#[test]
fn an_unshare_needs_the_nonce_of_its_own_prompt() {
    let fixture = Fixture::new();
    let id = captured_conversation(&fixture);
    type_the_challenge_back(&fixture, &["share", &id, "--remote", "origin"]);
    assert_eq!(listed_state(&fixture, &id), "[shared, 0/1 published]");
    only_the_nonce_of_its_own_prompt_answers(
        &fixture,
        &["unshare", &id, "--remote", "origin"],
        &format!("I UNSHARE {id} {}", fingerprint(&fixture)),
        || listed_state(&fixture, &id) == "[private]",
    );
}

#[test]
fn authorizing_a_range_for_a_remote_needs_the_nonce_of_its_own_prompt() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    std::fs::write(fixture.repo.join("second"), "second\n").unwrap();
    fixture.git(&["add", "second"]);
    fixture.git(&["commit", "-q", "-m", "second"]);
    let repository = fixture.open_repo();
    let base = repository
        .revparse_single("HEAD~1")
        .unwrap()
        .id()
        .to_string();
    let tip = repository.revparse_single("HEAD").unwrap().id().to_string();
    only_the_nonce_of_its_own_prompt_answers(
        &fixture,
        &["relink", "observe-range", &base, &tip, "--remote", "origin"],
        &format!("I AUTHORIZE RANGE {base} {tip} {}", fingerprint(&fixture)),
        || count(&fixture, "SELECT COUNT(*) FROM range_authorizations") > 0,
    );
}

#[test]
fn a_remote_redaction_needs_the_nonce_of_its_own_prompt() {
    let fixture = Fixture::new();
    let id = captured_conversation(&fixture);
    fixture.consent_to_sharing();
    // Redacting for a remote needs that remote's baseline: share, then publish.
    type_the_challenge_back(&fixture, &["share", &id, "--remote", "origin"]);
    type_the_challenge_back(&fixture, &["push", "--manual", "--remote", "origin"]);
    let interaction: String = rusqlite::Connection::open(fixture.index_db_path())
        .unwrap()
        .query_row("SELECT id FROM interactions", [], |row| row.get(0))
        .unwrap();
    let plan = fixture.temp.path().join("redaction-plan.json");
    only_the_nonce_of_its_own_prompt_answers(
        &fixture,
        &[
            "redact",
            &interaction,
            "--remote",
            "origin",
            "--rewrite-plan",
            plan.to_str().unwrap(),
        ],
        &format!("I REDACT {interaction} {}", fingerprint(&fixture)),
        || {
            count(
                &fixture,
                "SELECT COUNT(*) FROM tombstones WHERE scope_kind = 'remote'",
            ) > 0
        },
    );
}

// ---------------------------------------------------------------------------
// The phrase of a share says whether future thoughts are shared
// ---------------------------------------------------------------------------

#[test]
fn a_future_share_is_confirmed_only_by_the_phrase_that_says_future() {
    let fixture = Fixture::new();
    let id = captured_conversation(&fixture);
    let fp = fingerprint(&fixture);
    let future = ["share", id.as_str(), "--remote", "origin", "--future"];

    // The phrase of a snapshot share does not confirm a share of later thoughts.
    let (asked, confirmed, output) = answer_the_challenge(&fixture, &future, |challenge| {
        challenge.replacen("I SHARE FUTURE ", "I SHARE ", 1)
    });
    assert!(
        asked.starts_with(&format!("I SHARE FUTURE {id} {fp} 1 ")),
        "{asked}"
    );
    assert!(!confirmed, "{output}");
    assert!(
        output.contains("acknowledgement challenge did not match"),
        "{output}"
    );
    assert_eq!(listed_state(&fixture, &id), "[private]");
    // That is the phrase a snapshot share of the same thoughts asks, word for
    // word: `FUTURE` is all that tells the two apart, and it has no nonce.
    assert_eq!(
        share_phrase_seen_earlier(&fixture, &id),
        asked.replacen("I SHARE FUTURE ", "I SHARE ", 1)
    );

    // The phrase with FUTURE in it does.
    let (again, confirmed, output) = answer_the_challenge(&fixture, &future, str::to_owned);
    assert_eq!(again, asked);
    assert!(confirmed, "{output}");
    assert!(
        output.contains("1 turn(s) marked shared; future turns are shared."),
        "{output}"
    );
    assert_eq!(listed_state(&fixture, &id), "[shared, 0/1 published]");
}

#[test]
fn a_snapshot_share_is_not_confirmed_by_the_phrase_that_says_future() {
    let fixture = Fixture::new();
    let id = captured_conversation(&fixture);
    let fp = fingerprint(&fixture);
    let snapshot = ["share", id.as_str(), "--remote", "origin"];

    // The phrase of a future share does not confirm a snapshot share.
    let (asked, confirmed, output) = answer_the_challenge(&fixture, &snapshot, |challenge| {
        challenge.replacen("I SHARE ", "I SHARE FUTURE ", 1)
    });
    assert!(
        asked.starts_with(&format!("I SHARE {id} {fp} 1 ")),
        "{asked}"
    );
    assert!(!asked.contains("FUTURE"), "{asked}");
    assert!(!confirmed, "{output}");
    assert!(
        output.contains("acknowledgement challenge did not match"),
        "{output}"
    );
    assert_eq!(listed_state(&fixture, &id), "[private]");

    // Control: its own phrase shares the snapshot, and later thoughts stay
    // private.
    let (again, confirmed, output) = answer_the_challenge(&fixture, &snapshot, str::to_owned);
    assert_eq!(again, asked);
    assert!(confirmed, "{output}");
    assert!(
        output.contains("1 turn(s) marked shared; future turns are private."),
        "{output}"
    );
}
