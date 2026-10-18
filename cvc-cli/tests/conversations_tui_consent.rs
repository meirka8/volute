#![cfg(unix)]

//! Acceptance tests of the interactive `cvc conversations` view (issue #112):
//! its consent paths, through the real binary on a pseudo-terminal. The numbers
//! are those of the criteria in `conversations_tui_acceptance.rs`.
//!
//! 5. Every consent path goes through its typed phrase. For a share, a share
//!    with future thoughts, an unshare, a publish and an authorization: an
//!    empty line, a wrong phrase and Esc in the bar record nothing, Esc asks no
//!    phrase at all, and the right phrase records exactly what the bar offered.
//! 6. Type-ahead and paste cannot take the place of a key or of a phrase.
//!
//! What a flow records is read around the view: the plain listing, `cvc
//! privacy status`, the index database and the remote's `refs/cvc/main`.

mod common;
mod tui_support;

use common::{challenges, PtySession};
use std::time::Duration;
use tui_support::*;

/// Opens the pre-flight bar of `key` and waits for its words.
fn open_bar(view: &mut View, key: char, words: &str) {
    view.key(key);
    view.wait_for(words);
}

/// Esc in a bar: it closes, and no phrase is asked. Esc is sent alone and the
/// next key waits for the bar to go, or a terminal reads the two as one chord.
fn cancel_the_bar(view: &mut View, words: &str) {
    let prompts = view.prompts_so_far();
    view.press(Key::Esc);
    view.wait_until_gone(words);
    assert_eq!(view.prompts_so_far(), prompts, "Esc asked for a phrase");
    assert!(
        !view.session.output().contains("-- cvc:"),
        "Esc started a flow"
    );
}

/// A phrase that is not the one asked: the last character dropped.
fn truncated(phrase: &str) -> String {
    phrase[..phrase.len() - 1].to_owned()
}

// ---------------------------------------------------------------------------
// 5. Share
// ---------------------------------------------------------------------------

#[test]
fn a_share_is_recorded_only_by_its_phrase() {
    let (fixture, id) = authorized_with_a_conversation();
    let origin = fingerprint(&fixture, "origin");
    let nothing_recorded = || {
        assert_eq!(share_record(&fixture, &id, &origin), ShareRecord::default());
        assert_eq!(listed_state(&fixture, &id), "[private]");
    };
    let mut view = View::open(&fixture);

    // Esc in the bar.
    open_bar(&mut view, 's', "not yet shared");
    cancel_the_bar(&mut view, "not yet shared");
    nothing_recorded();

    // An empty line.
    open_bar(&mut view, 's', "not yet shared");
    view.press(Key::Enter);
    let asked = view.answer_with(1, |_| String::new());
    assert!(
        asked.starts_with(&format!("I SHARE {id} {origin} 1 ")),
        "{asked}"
    );
    view.wait_for("Not shared: the typed phrase did not match. Nothing was changed.");
    nothing_recorded();

    // A wrong phrase.
    open_bar(&mut view, 's', "not yet shared");
    view.press(Key::Enter);
    view.answer_with(2, |_| "not the phrase".to_owned());
    view.wait_for("Not shared: the typed phrase did not match. Nothing was changed.");
    nothing_recorded();

    // The phrase in lower case: it is matched exactly.
    open_bar(&mut view, 's', "not yet shared");
    view.press(Key::Enter);
    view.answer_with(3, |phrase| phrase.to_lowercase());
    view.wait_for("Not shared: the typed phrase did not match. Nothing was changed.");
    nothing_recorded();

    // The right phrase records the snapshot, and later thoughts stay private.
    open_bar(&mut view, 's', "not yet shared");
    view.press(Key::Enter);
    view.type_phrase(4);
    view.wait_for("1 thought marked shared for remote 'origin'; future thoughts are private.");
    assert_eq!(
        share_record(&fixture, &id, &origin),
        ShareRecord {
            shared_thoughts: 1,
            future: Some(false),
            publications: vec![],
        }
    );
    assert_eq!(listed_state(&fixture, &id), "[shared, 0/1 published]");
    view.is_at_the_list();
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    // Four phrases were asked for four attempts; nothing was published.
    assert_eq!(challenges(&view.session.output()).len(), 4);
    assert_eq!(published_conversations(&fixture.remote), None);
}

#[test]
fn a_share_with_future_thoughts_is_recorded_only_by_f_and_its_phrase() {
    let (fixture, id) = authorized_with_a_conversation();
    let origin = fingerprint(&fixture, "origin");
    let nothing_recorded = || {
        // No decision about the conversation, now or later, was recorded.
        assert_eq!(share_record(&fixture, &id, &origin), ShareRecord::default());
        assert_eq!(listed_state(&fixture, &id), "[private]");
    };
    let mut view = View::open(&fixture);

    // The bar offers it, and Esc in it records nothing.
    open_bar(&mut view, 's', "f: also future thoughts");
    cancel_the_bar(&mut view, "f: also future thoughts");
    nothing_recorded();

    // `f` leads to the phrase, and an empty line records nothing.
    open_bar(&mut view, 's', "f: also future thoughts");
    view.key('f');
    let asked = view.answer_with(1, |_| String::new());
    assert!(asked.starts_with(&format!("I SHARE {id} {origin} 1 ")));
    view.wait_for("Not shared: the typed phrase did not match. Nothing was changed.");
    nothing_recorded();

    // So does a phrase that is almost right.
    open_bar(&mut view, 's', "f: also future thoughts");
    view.key('f');
    view.answer_with(2, truncated);
    view.wait_for("Not shared: the typed phrase did not match. Nothing was changed.");
    nothing_recorded();

    // The right phrase after `f` records the snapshot and the future together.
    open_bar(&mut view, 's', "f: also future thoughts");
    view.key('f');
    view.type_phrase(3);
    view.wait_for("1 thought marked shared for remote 'origin'; future thoughts are shared too.");
    assert_eq!(
        share_record(&fixture, &id, &origin),
        ShareRecord {
            shared_thoughts: 1,
            future: Some(true),
            publications: vec![],
        }
    );
    assert!(view.flat().contains("◐ shared · 1 waiting +future"));
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    assert_eq!(challenges(&view.session.output()).len(), 3);
    assert_eq!(published_conversations(&fixture.remote), None);
}

#[test]
fn future_thoughts_are_added_to_a_snapshot_share_only_by_f_and_its_phrase() {
    let (fixture, id) = authorized_with_a_conversation();
    let origin = fingerprint(&fixture, "origin");
    let snapshot_only = ShareRecord {
        shared_thoughts: 1,
        future: Some(false),
        publications: vec![],
    };
    let mut view = View::open(&fixture);

    // Enter in the bar shares what is there and nothing later.
    open_bar(&mut view, 's', "not yet shared");
    view.press(Key::Enter);
    view.type_phrase(1);
    view.wait_for("1 thought marked shared for remote 'origin'; future thoughts are private.");
    assert_eq!(share_record(&fixture, &id, &origin), snapshot_only);

    // Everything is shared now, so the bar offers `f` and Esc only. Enter is
    // not a key of that bar: it asks nothing and starts nothing.
    open_bar(&mut view, 's', "is already shared for 'origin'");
    view.press(Key::Enter);
    view.wait_quietly(Duration::from_millis(500));
    assert!(view.flat().contains("is already shared for 'origin'"));
    assert_eq!(view.prompts_so_far(), 1);
    assert!(!view.flat().contains("Not shared"));

    // `f` and a wrong phrase leave the future as it was.
    view.key('f');
    view.answer_with(2, |_| "not the phrase".to_owned());
    view.wait_for("Not shared: the typed phrase did not match. Nothing was changed.");
    assert_eq!(share_record(&fixture, &id, &origin), snapshot_only);

    // `f` and the right phrase turn it on.
    open_bar(&mut view, 's', "is already shared for 'origin'");
    view.key('f');
    view.type_phrase(3);
    view.wait_for("future thoughts are shared too.");
    assert_eq!(
        share_record(&fixture, &id, &origin),
        ShareRecord {
            shared_thoughts: 1,
            future: Some(true),
            publications: vec![],
        }
    );
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
}

// ---------------------------------------------------------------------------
// 5. Unshare
// ---------------------------------------------------------------------------

#[test]
fn an_unshare_is_recorded_only_by_its_phrase() {
    let (fixture, id) = authorized_with_a_conversation();
    share_with_the_command(&fixture, &id);
    let origin = fingerprint(&fixture, "origin");
    // The phrase of another decision, which is not this one.
    let share_phrase = the_share_phrase(&fixture, &id);
    let shared = ShareRecord {
        shared_thoughts: 1,
        future: Some(false),
        publications: vec![],
    };
    let still_shared = || {
        assert_eq!(share_record(&fixture, &id, &origin), shared);
        assert_eq!(listed_state(&fixture, &id), "[shared, 0/1 published]");
    };
    let mut view = View::open(&fixture);
    view.wait_for("◐ shared · 1 waiting");
    let question = "Its 1 shared thought becomes private again.";

    // Esc in the bar.
    open_bar(&mut view, 'u', question);
    assert!(view
        .flat()
        .contains("Unshare \"Run: printf\" for 'origin'?"));
    cancel_the_bar(&mut view, question);
    still_shared();

    // An empty line.
    open_bar(&mut view, 'u', question);
    view.press(Key::Enter);
    let asked = view.answer_with(1, |_| String::new());
    assert_eq!(asked, format!("I UNSHARE {id} {origin}"));
    view.wait_for("Not unshared: the typed phrase did not match. Nothing was changed.");
    still_shared();

    // The phrase of a share, which is a phrase, but not this one.
    open_bar(&mut view, 'u', question);
    view.press(Key::Enter);
    view.answer_with(2, |_| share_phrase.clone());
    view.wait_for("Not unshared: the typed phrase did not match. Nothing was changed.");
    still_shared();

    // The right phrase makes it private again.
    open_bar(&mut view, 'u', question);
    view.press(Key::Enter);
    view.type_phrase(3);
    view.wait_for("1 thought made private again for remote 'origin'.");
    assert_eq!(
        share_record(&fixture, &id, &origin),
        ShareRecord {
            shared_thoughts: 0,
            future: Some(false),
            publications: vec![],
        }
    );
    assert_eq!(listed_state(&fixture, &id), "[private]");
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
}

// ---------------------------------------------------------------------------
// 5. Publish
// ---------------------------------------------------------------------------

#[test]
fn a_publish_sends_nothing_before_its_phrase() {
    let (fixture, id) = authorized_with_a_conversation();
    share_with_the_command(&fixture, &id);
    let origin = fingerprint(&fixture, "origin");
    let unpublished = || {
        assert_eq!(
            share_record(&fixture, &id, &origin),
            ShareRecord {
                shared_thoughts: 1,
                future: Some(false),
                publications: vec![],
            }
        );
        assert_eq!(listed_state(&fixture, &id), "[shared, 0/1 published]");
        assert_eq!(published_conversations(&fixture.remote), None);
    };
    let mut view = View::open(&fixture);
    view.wait_for("◐ shared · 1 waiting");
    let question = "1 waiting by this clone's count";

    // Esc in the bar.
    open_bar(&mut view, 'p', question);
    cancel_the_bar(&mut view, question);
    unpublished();

    // An empty line. The phrase is for this push: the destination, the exact
    // tree and how many thoughts it adds.
    open_bar(&mut view, 'p', question);
    view.press(Key::Enter);
    let asked = view.answer_with(1, |_| String::new());
    assert!(
        asked.starts_with(&format!("I PUBLISH {origin} ")),
        "{asked}"
    );
    assert!(asked.ends_with(" 1"), "{asked}");
    view.wait_for(
        "Not published: the typed phrase did not match. Nothing was published to 'origin'.",
    );
    unpublished();

    // The right phrase with a space after it.
    open_bar(&mut view, 'p', question);
    view.press(Key::Enter);
    view.answer_with(2, |phrase| format!("{phrase} "));
    view.wait_for(
        "Not published: the typed phrase did not match. Nothing was published to 'origin'.",
    );
    unpublished();

    // The right phrase publishes it.
    open_bar(&mut view, 'p', question);
    view.press(Key::Enter);
    view.type_phrase(3);
    view.wait_for("Published 1 new shared thought to 'origin'.");
    assert_eq!(
        share_record(&fixture, &id, &origin),
        ShareRecord {
            shared_thoughts: 1,
            future: Some(false),
            publications: vec!["published".to_owned()],
        }
    );
    assert_eq!(listed_state(&fixture, &id), "[shared, 1 published]");
    assert_eq!(
        published_conversations(&fixture.remote).unwrap(),
        [(id.clone(), 1)].into()
    );
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
}

// ---------------------------------------------------------------------------
// 5. Authorize
// ---------------------------------------------------------------------------

#[test]
fn an_authorization_is_recorded_only_by_its_phrase() {
    let fixture = common::Fixture::new();
    let id = captured_conversation(&fixture);
    let origin = fingerprint(&fixture, "origin");
    let phrase = format!("I AUTHORIZE SHARING {origin}");
    let not_authorized = || {
        assert!(!is_authorized(&fixture, "origin"));
        assert!(!auto_push_is_on(&fixture, "origin"));
    };
    let mut view = View::open(&fixture);
    view.wait_for("publishing: not authorized yet (s or p will walk you through it)");
    let offer = "is not authorized in this clone yet.";

    // Esc in the offer, from either key.
    for key in ['s', 'p'] {
        open_bar(&mut view, key, offer);
        cancel_the_bar(&mut view, offer);
        not_authorized();
    }

    // An empty line.
    open_bar(&mut view, 's', offer);
    view.press(Key::Enter);
    let asked = view.answer_with(1, |_| String::new());
    assert_eq!(asked, phrase);
    view.wait_for("Not authorized: the typed phrase did not match.");
    not_authorized();

    // A phrase that is almost right.
    open_bar(&mut view, 'p', offer);
    view.press(Key::Enter);
    view.answer_with(2, truncated);
    view.wait_for("Not authorized: the typed phrase did not match.");
    not_authorized();

    // The right phrase authorizes publishing to this exact destination, and
    // does not turn auto-push on.
    open_bar(&mut view, 's', offer);
    view.press(Key::Enter);
    view.type_phrase(3);
    view.wait_for("Publishing to 'origin' is now authorized. Auto-push stays off.");
    assert!(is_authorized(&fixture, "origin"));
    assert!(!auto_push_is_on(&fixture, "origin"));
    view.wait_for("publishing: authorized by you auto-push: off");

    // It authorizes and stops there: nothing is shared, nothing is asked next.
    view.wait_quietly(Duration::from_millis(700));
    assert_eq!(view.prompts_so_far(), 3);
    assert!(!view.flat().contains("not yet shared"));
    assert_eq!(share_record(&fixture, &id, &origin), ShareRecord::default());
    assert_eq!(listed_state(&fixture, &id), "[private]");
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    assert_eq!(published_conversations(&fixture.remote), None);
}

// ---------------------------------------------------------------------------
// 5. A phrase consents to what it was shown
// ---------------------------------------------------------------------------

#[test]
fn a_thought_captured_while_the_phrase_is_open_is_not_shared_by_it() {
    let fixture = common::Fixture::new();
    ingest_the_session(&fixture);
    fixture.consent_to_sharing();
    let origin = fingerprint(&fixture, "origin");
    let mut view = View::open(&fixture);

    // The phrase asks for the three thoughts the bar showed.
    open_bar(&mut view, 's', "(3 thoughts, 3 not yet shared)");
    view.press(Key::Enter);
    let asked = view.prompt(1);
    assert!(
        asked.starts_with(&format!("I SHARE {SESSION} {origin} 3 ")),
        "{asked}"
    );

    // The session goes on while the phrase is open, and then the phrase is
    // typed. It is right for the three thoughts it names, not for four.
    capture_one_more_thought(&fixture);
    view.type_back(&asked);
    view.wait_for(
        "Not shared: this conversation gained new thoughts while you were confirming. Press s to try again.",
    );
    assert_eq!(
        share_record(&fixture, SESSION, &origin),
        ShareRecord::default()
    );
    assert_eq!(listed_state(&fixture, SESSION), "[private]");

    // Asked again, the bar and the phrase cover the thought that came, and the
    // phrase is a different one.
    open_bar(&mut view, 's', "(4 thoughts, 4 not yet shared)");
    view.press(Key::Enter);
    let again = view.prompt(2);
    assert!(
        again.starts_with(&format!("I SHARE {SESSION} {origin} 4 ")),
        "{again}"
    );
    assert_ne!(again, asked);
    view.type_back(&again);
    view.wait_for("4 thoughts marked shared for remote 'origin'; future thoughts are private.");
    assert_eq!(share_record(&fixture, SESSION, &origin).shared_thoughts, 4);
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
}

#[test]
fn an_unshare_does_not_need_authorization_but_a_publish_still_does() {
    let fixture = common::Fixture::new();
    let id = captured_conversation(&fixture);
    // `cvc share` shares for a destination whether or not publishing to it is
    // authorized; only a publish needs that.
    share_with_the_command(&fixture, &id);
    let origin = fingerprint(&fixture, "origin");
    assert!(!is_authorized(&fixture, "origin"));
    let mut view = View::open(&fixture);
    view.wait_for("publishing: not authorized yet");
    view.wait_for("◐ shared · 1 waiting");

    // `p` offers authorization, not a publish, and Esc leaves it at that.
    open_bar(&mut view, 'p', "is not authorized in this clone yet.");
    assert!(!view.flat().contains("waiting by this clone's count"));
    cancel_the_bar(&mut view, "is not authorized in this clone yet.");

    // `u` asks for its own phrase, and the right one makes the share private.
    open_bar(
        &mut view,
        'u',
        "Its 1 shared thought becomes private again.",
    );
    view.press(Key::Enter);
    let asked = view.type_phrase(1);
    assert_eq!(asked, format!("I UNSHARE {id} {origin}"));
    view.wait_for("1 thought made private again for remote 'origin'.");
    assert_eq!(listed_state(&fixture, &id), "[private]");
    assert!(!is_authorized(&fixture, "origin"));
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
}

// ---------------------------------------------------------------------------
// 6. Type-ahead and paste
// ---------------------------------------------------------------------------

/// The prompt is the last thing on the main screen: nothing was echoed after
/// it, and no result was printed. (A long phrase wraps onto a second row, so
/// this looks at the screen as one line.)
fn waits_at_the_prompt(view: &View) -> bool {
    flat(&view.session.screen()).ends_with("' to continue:")
}

#[test]
fn a_share_phrase_typed_in_the_same_write_as_the_keys_does_not_answer_the_prompt() {
    let (fixture, id) = authorized_with_a_conversation();
    let origin = fingerprint(&fixture, "origin");
    // The very phrase the view is about to ask: it is a function of the thoughts
    // shared, so it can be known before the prompt is shown.
    let phrase = the_share_phrase(&fixture, &id);
    let mut view = View::open(&fixture);

    // `s`, Enter and the phrase, in one write.
    view.session.write(format!("s\r{phrase}\n").as_bytes());
    let asked = view.prompt(1);
    assert_eq!(asked, phrase);

    // The prompt is displayed and waits. Nothing answered it.
    view.wait_quietly(Duration::from_millis(800));
    assert!(waits_at_the_prompt(&view), "{}", describe(&view.session));
    assert_eq!(share_record(&fixture, &id, &origin), ShareRecord::default());
    assert_on_the_main_screen(&view.session);

    // It is waiting for an answer typed now, which a wrong one is.
    view.answer_with(1, |_| "no".to_owned());
    view.wait_for("Not shared: the typed phrase did not match. Nothing was changed.");
    // And what else came with the phrase, letters that are keys of the view,
    // did not act in the list when it came back.
    view.is_at_the_list();
    assert_eq!(view.prompts_so_far(), 1);

    // The same when the phrase follows the keys a moment later, while the view
    // is handing the terminal over and the prompt waits for the input to go
    // quiet: what arrives then is thrown away, not read as the answer.
    open_bar(&mut view, 's', "not yet shared");
    drop(view.session.send_later(vec![
        (Duration::ZERO, b"\r".to_vec()),
        (
            Duration::from_millis(40),
            format!("{phrase}\n").into_bytes(),
        ),
    ]));
    assert_eq!(view.prompt(2), phrase);
    view.wait_quietly(Duration::from_millis(800));
    assert!(waits_at_the_prompt(&view), "{}", describe(&view.session));
    assert_eq!(share_record(&fixture, &id, &origin), ShareRecord::default());
    view.answer_with(2, |_| "no".to_owned());
    view.wait_for("Not shared: the typed phrase did not match. Nothing was changed.");
    view.is_at_the_list();

    // Control: the same phrase, typed after the prompt, is the right answer.
    open_bar(&mut view, 's', "not yet shared");
    view.press(Key::Enter);
    assert_eq!(view.prompt(3), phrase);
    view.type_back(&phrase);
    view.wait_for("1 thought marked shared for remote 'origin'; future thoughts are private.");
    assert_eq!(share_record(&fixture, &id, &origin).shared_thoughts, 1);
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
}

#[test]
fn an_authorization_phrase_typed_in_the_same_write_as_the_keys_does_not_answer_the_prompt() {
    let fixture = common::Fixture::new();
    captured_conversation(&fixture);
    let phrase = format!("I AUTHORIZE SHARING {}", fingerprint(&fixture, "origin"));
    let mut view = View::open(&fixture);

    view.session.write(format!("s\r{phrase}\n").as_bytes());
    assert_eq!(view.prompt(1), phrase);
    view.wait_quietly(Duration::from_millis(800));
    assert!(waits_at_the_prompt(&view), "{}", describe(&view.session));
    assert!(!is_authorized(&fixture, "origin"));

    view.answer_with(1, |_| "no".to_owned());
    view.wait_for("Not authorized: the typed phrase did not match.");
    view.is_at_the_list();
    assert!(!is_authorized(&fixture, "origin"));

    // Control: typed after the prompt it authorizes.
    open_bar(&mut view, 's', "is not authorized in this clone yet.");
    view.press(Key::Enter);
    assert_eq!(view.prompt(2), phrase);
    view.type_back(&phrase);
    view.wait_for("Publishing to 'origin' is now authorized.");
    assert!(is_authorized(&fixture, "origin"));
    assert!(view.quit().success());
}

#[test]
fn keys_and_a_phrase_queued_before_the_program_starts_do_nothing() {
    let (fixture, id) = authorized_with_a_conversation();
    let origin = fingerprint(&fixture, "origin");
    let phrase = the_share_phrase(&fixture, &id);

    // The keys that share and publish, and the phrase that answers the first,
    // are on the terminal before `cvc` starts.
    let queued = format!("s\r{phrase}\np\r\ns\r{phrase}\n");
    let session = PtySession::spawn_sized(
        tui_command(&fixture, &["conversations"]),
        queued.as_bytes(),
        COLUMNS,
        ROWS,
    );
    let mut view = View::attach(session);

    // The first frame is the list, not a bar, and nothing was started.
    let screen = view.flat();
    assert!(screen.contains("q quit"), "{screen}");
    assert!(!screen.contains("not yet shared"), "{screen}");
    assert!(
        !screen.contains("waiting by this clone's count"),
        "{screen}"
    );
    view.wait_quietly(Duration::from_millis(700));
    assert_eq!(view.prompts_so_far(), 0, "{}", describe(&view.session));
    assert!(
        !view.session.output().contains("-- cvc:"),
        "{}",
        describe(&view.session)
    );
    view.is_at_the_list();
    assert_eq!(share_record(&fixture, &id, &origin), ShareRecord::default());

    // Control: the view takes keys typed now.
    open_bar(&mut view, 's', "not yet shared");
    cancel_the_bar(&mut view, "not yet shared");
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    assert_eq!(listed_state(&fixture, &id), "[private]");
}

#[test]
fn a_bracketed_paste_is_never_taken_for_keys() {
    let (fixture, id) = authorized_with_a_conversation();
    let origin = fingerprint(&fixture, "origin");
    let phrase = the_share_phrase(&fixture, &id);
    let mut view = View::open(&fixture);
    // The view asks the terminal to bracket what is pasted; without it a paste
    // would arrive as keys.
    assert!(view.session.output().contains("\x1b[?2004h"));

    // Pasted: the keys that share, and the phrase that answers them.
    view.session
        .write(format!("\x1b[200~s\r{phrase}\n\x1b[201~").as_bytes());
    // `?` opens the help from the list and from nowhere else, so this proves the
    // paste was taken in and left the view at the list.
    view.is_at_the_list();
    assert_eq!(view.prompts_so_far(), 0);
    assert!(!view.session.output().contains("-- cvc:"));

    // Pasted into a bar: an Enter, which would start the flow.
    open_bar(&mut view, 's', "not yet shared");
    view.session.write(b"\x1b[200~\r\x1b[201~");
    view.wait_quietly(Duration::from_millis(600));
    assert!(view.flat().contains("not yet shared"));
    assert_eq!(view.prompts_so_far(), 0);
    assert!(!view.session.output().contains("-- cvc:"));

    // The bar is the bar it was: Esc, typed, closes it.
    cancel_the_bar(&mut view, "not yet shared");
    assert_eq!(share_record(&fixture, &id, &origin), ShareRecord::default());

    // Control: typed one key at a time, the same keys do open the bar and ask.
    open_bar(&mut view, 's', "not yet shared");
    view.press(Key::Enter);
    assert_eq!(view.prompt(1), phrase);
    view.answer_with(1, |_| "no".to_owned());
    view.wait_for("Not shared: the typed phrase did not match. Nothing was changed.");
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    assert_eq!(listed_state(&fixture, &id), "[private]");
}

#[test]
fn keys_typed_while_a_flows_result_is_printed_do_not_act_after_the_view_resumes() {
    let (fixture, id) = authorized_with_a_conversation();
    let origin = fingerprint(&fixture, "origin");
    let mut view = View::open(&fixture);

    open_bar(&mut view, 's', "not yet shared");
    view.press(Key::Enter);
    let phrase = view.prompt(1);
    // The phrase is answered, and keys that would start a publish come in the
    // same write and keep coming while the share is recorded, its result is
    // printed and the view returns.
    view.session.write(format!("{phrase}\np\r").as_bytes());
    view.session.flood(
        b"p\r",
        Duration::from_millis(10),
        Duration::from_millis(600),
    );

    // The view returns with the result, once the input has been quiet ...
    view.wait_for("1 thought marked shared for remote 'origin'; future thoughts are private.");
    // ... and none of those keys acted in it.
    view.wait_quietly(Duration::from_millis(700));
    assert_eq!(view.prompts_so_far(), 1, "a publish was started");
    assert!(!view.flat().contains("waiting by this clone's count"));
    assert!(!view.session.output().contains("-- cvc: publish"));
    view.is_at_the_list();
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);

    // The share is recorded; nothing was sent anywhere.
    assert_eq!(share_record(&fixture, &id, &origin).shared_thoughts, 1);
    assert_eq!(published_conversations(&fixture.remote), None);
}

// ---------------------------------------------------------------------------
// Standing choices are never hidden
// ---------------------------------------------------------------------------

#[test]
fn auto_push_is_disclosed_before_during_and_after_a_share() {
    let (fixture, id) = authorized_with_a_conversation();
    fixture.enable_auto_push();
    let origin = fingerprint(&fixture, "origin");
    let mut view = View::open(&fixture);

    // The header says so loudly, in words that name what it does.
    let screen = view.flat();
    assert!(
        screen.contains(
            "auto-push: ON - every git push to this remote also publishes shared thoughts"
        ),
        "{screen}"
    );

    // The bar warns that a share will be published by a git push ...
    open_bar(&mut view, 's', "not yet shared");
    assert!(view.flat().contains("auto-push publishes them"));
    view.press(Key::Enter);

    // ... the banner before the phrase says it again, with an editor's push ...
    view.prompt(1);
    assert_on_the_main_screen(&view.session);
    let main_screen = flat(&view.session.screen());
    assert!(
        main_screen.contains(
            "Auto-push is on: every git push to 'origin', including one an editor runs, publishes them."
        ),
        "{main_screen}"
    );
    view.type_phrase(1);

    // ... and so does the result, which does not claim it is unpublished.
    view.wait_for("1 thought marked shared for remote 'origin'; future thoughts are private.");
    let screen = view.flat();
    assert!(
        screen.contains("Auto-push is on: every git push to 'origin' publishes them."),
        "{screen}"
    );
    assert!(!screen.contains("Not published yet"), "{screen}");
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    // A share is a share: the view published nothing itself.
    assert_eq!(share_record(&fixture, &id, &origin).shared_thoughts, 1);
    assert_eq!(published_conversations(&fixture.remote), None);
}

/// What the process wrote to the main screen from the last time it left the
/// view: a flow's banner, summary and prompt.
fn written_on_the_main_screen(view: &View) -> String {
    let output = view.session.output();
    let at = output
        .rfind(LEAVE_ALTERNATE_SCREEN)
        .expect("the view was left");
    output[at + LEAVE_ALTERNATE_SCREEN.len()..].to_owned()
}

#[test]
fn a_title_chosen_by_the_agent_cannot_act_on_the_terminal_or_pass_for_the_prompt() {
    let fixture = common::Fixture::new();
    // Titles are text the agent can steer: this one sets the window title,
    // clears the screen, reverses the text and returns to the start of the line
    // to write a prompt of its own over the one the person is about to answer.
    let hostile = "Fix it\u{1b}]0;pwned\u{7}\u{1b}[2J\u{202e}\rSharing conversation for remote 'origin':\nI SHARE x";
    ingest_a_session(&fixture, hostile, hostile);
    fixture.consent_to_sharing();
    let acts_on_a_terminal = |text: &str| {
        text.chars()
            .any(|c| c != '\n' && (c.is_control() || c == '\u{202e}'))
    };

    // The listing a script gets.
    assert!(!acts_on_a_terminal(&plain_listing(&fixture)));

    // The view: only what the view itself writes, never an operating system
    // command, a bidirectional override or a bell.
    let mut view = View::open(&fixture);
    view.key('l');
    view.wait_for("└─ agent");
    let output = view.session.output();
    assert!(!output.contains("\u{1b}]"), "an OSC reached the terminal");
    assert!(!output.contains('\u{202e}') && !output.contains('\u{7}'));
    let screen = view.flat();
    assert!(screen.contains("Fix it?]0;pwned"), "{screen}");
    view.key('h');
    view.wait_until_gone("└─ agent");

    // The flow's own screen: the title is one line between the summary and the
    // prompt, and nothing in it moves the cursor, so the summary above the
    // prompt cannot be rewritten.
    view.key('s');
    view.wait_for("not yet shared");
    view.press(Key::Enter);
    view.prompt(1);
    let main_screen = written_on_the_main_screen(&view);
    // What the view itself writes as it hands the terminal over ...
    let mut flow_output = main_screen.clone();
    for mode in [
        "\u{1b}[?25h",
        "\u{1b}[?25l",
        "\u{1b}[?2004l",
        "\u{1b}[?1049l",
    ] {
        flow_output = flow_output.replace(mode, "");
    }
    // ... apart from which the flow's lines hold no character that acts on a
    // terminal, and no carriage return that is not half of a line ending.
    assert!(
        !acts_on_a_terminal(&flow_output.replace("\r\n", "\n")),
        "{main_screen:?}"
    );
    assert!(main_screen.contains("Fix it?]0;pwned"), "{main_screen:?}");
    view.answer_with(1, |_| String::new());
    view.wait_for("Not shared: the typed phrase did not match. Nothing was changed.");
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
}

#[test]
fn a_remote_url_with_credentials_is_never_shown_or_echoed() {
    let fixture = common::Fixture::new();
    let id = captured_conversation(&fixture);
    // A token in the user information, in the query and in the fragment.
    fixture.git(&[
        "remote",
        "set-url",
        "origin",
        "https://alice:tok-SECRET-1@example.invalid/org/repo.git?token=tok-SECRET-2#tok-SECRET-3",
    ]);
    let origin = fingerprint(&fixture, "origin");
    let mut view = View::open(&fixture);
    let screen = view.flat();
    assert!(
        screen.contains("remote: origin -> example.invalid/org/repo.git"),
        "{screen}"
    );

    // Authorizing: the offer, the banner and the phrase name the remote and its
    // fingerprint, and the location without the secrets.
    open_bar(&mut view, 's', "is not authorized in this clone yet.");
    assert!(view
        .flat()
        .contains("Publishing to 'origin' (example.invalid/org/repo.git)"));
    view.press(Key::Enter);
    let phrase = view.type_phrase(1);
    assert_eq!(phrase, format!("I AUTHORIZE SHARING {origin}"));
    view.wait_for("Publishing to 'origin' is now authorized.");

    // Sharing, the picker and the details say the same.
    open_bar(&mut view, 's', "not yet shared");
    assert!(view
        .flat()
        .contains("for 'origin' (example.invalid/org/repo.git)?"));
    view.press(Key::Enter);
    view.type_phrase(2);
    view.wait_for("1 thought marked shared for remote 'origin'");
    view.key('r');
    view.wait_for("Choose a remote");
    assert!(overlay_text(&view.session).contains("origin example.invalid/org/repo.git"));
    view.press(Key::Esc);
    view.wait_until_gone("Choose a remote");
    view.press(Key::Enter);
    view.wait_for(&format!("ID {id}"));
    assert!(overlay_text(&view.session).contains("With 'origin' (fingerprint"));
    view.press(Key::Esc);
    view.wait_until_gone(&format!("ID {id}"));
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);

    // Nothing the process wrote, on either screen, nor what the terminal echoed
    // of the phrases, holds any of it.
    let output = view.session.output();
    for secret in ["alice", "tok-SECRET", "token=", "#tok"] {
        assert!(!output.contains(secret), "{secret:?} in the output");
    }
}

#[test]
fn the_bar_is_built_from_what_the_store_says_when_the_key_is_pressed() {
    let (fixture, id) = authorized_with_a_conversation();
    let origin = fingerprint(&fixture, "origin");
    let mut view = View::open(&fixture);
    view.wait_for("○ private");

    // Another terminal shares the conversation while this view is open. The row
    // still reads as it did when the list was loaded ...
    share_with_the_command(&fixture, &id);
    assert!(view.flat().contains("○ private"));

    // ... but `s` asks about what is there now, so the bar does not offer to
    // share a snapshot that has already been shared.
    view.key('s');
    view.wait_for("is already shared for 'origin'");
    let bar = view.flat();
    assert!(!bar.contains("not yet shared"), "{bar}");
    assert!(!bar.contains("Enter: this snapshot only"), "{bar}");
    view.press(Key::Enter);
    view.wait_quietly(Duration::from_millis(500));
    assert_eq!(view.prompts_so_far(), 0);
    assert!(view.flat().contains("is already shared for 'origin'"));
    cancel_the_bar(&mut view, "is already shared for 'origin'");
    assert!(view.quit().success());
    assert_eq!(share_record(&fixture, &id, &origin).shared_thoughts, 1);
}

#[test]
fn a_publish_reports_what_is_new_against_the_total_and_asks_nothing_when_nothing_is_new() {
    let fixture = common::Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "first"]);
    fixture.cvc_ok(&["run", "--", "printf", "second"]);
    fixture.consent_to_sharing();
    let origin = fingerprint(&fixture, "origin");
    let ids = fixture.run_conversation_ids();
    assert_eq!(ids.len(), 2, "{ids:?}");
    // One conversation is published; another is shared on top of it.
    share_with_the_command(&fixture, &ids[0]);
    publish_with_the_command(&fixture);
    share_with_the_command(&fixture, &ids[1]);
    let mut view = View::open(&fixture);
    view.wait_for("1 shared thought waiting to publish");

    // The phrase and the notice count what this push adds, and give the total
    // the destination holds afterwards.
    open_bar(&mut view, 'p', "1 waiting by this clone's count");
    view.press(Key::Enter);
    let asked = view.prompt(1);
    assert!(
        asked.starts_with(&format!("I PUBLISH {origin} ")),
        "{asked}"
    );
    assert!(asked.ends_with(" 1"), "{asked}");
    let summary = flat(&view.session.screen());
    assert!(
        summary.contains("Publishing 1 new shared interaction(s) to 'origin' (2 shared in total)."),
        "{summary}"
    );
    view.type_back(&asked);
    view.wait_for("Published 1 new shared thought to 'origin' (2 shared in total).");
    assert_eq!(
        published_conversations(&fixture.remote).unwrap(),
        [(ids[0].clone(), 1), (ids[1].clone(), 1)].into()
    );

    // With nothing new the publish says so, with the total, and asks for no
    // phrase: there is nothing to consent to.
    open_bar(&mut view, 'p', "Nothing is waiting by this clone's count.");
    view.press(Key::Enter);
    view.wait_for("No new shared thoughts to publish to 'origin' (2 shared in total).");
    assert_eq!(view.prompts_so_far(), 1);
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
}
