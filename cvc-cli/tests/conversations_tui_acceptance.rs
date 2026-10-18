#![cfg(unix)]

//! Acceptance tests of the interactive `cvc conversations` view (issue #112),
//! through the real binary on a pseudo-terminal. Each test sits under the
//! criterion it covers, numbered as in the test plan:
//!
//! 1. Mode selection: a terminal gets the view, everything else the plain
//!    listing, unchanged.
//! 2. Sharing a session and its subagent and publishing them takes three typed
//!    phrases and no typed id, flag or remote name.
//! 3. Subagents nest under their session, collapsed by default.
//! 4. Another remote can be picked; one that is not authorized offers
//!    authorization and never chains into a share or a publish.
//! 7. Imported conversations are read-only; an unshare after a publish is
//!    refused before any phrase.
//! 8. The terminal is given back on every way out.
//!
//! The consent paths (criterion 5) and type-ahead and paste (criterion 6) are
//! in `conversations_tui_consent.rs`; the view's start, its drawing and its
//! signals are in `conversations_tui_smoke.rs`.
//!
//! What a flow records is always read around the view: the plain listing,
//! `cvc privacy status`, the index database and the remote's `refs/cvc/main`.

mod common;
mod tui_support;

use common::{challenges, Fixture, PtySession};
use std::collections::BTreeMap;
use std::process::Command;
use std::time::{Duration, Instant};
use tui_support::*;

// ---------------------------------------------------------------------------
// 1. Mode selection
// ---------------------------------------------------------------------------

/// What `cvc conversations` printed before the view existed, for the session
/// `ingest_the_session` captures.
fn the_plain_listing(fixture: &Fixture) -> String {
    format!(
        "Conversations (most recent first); share state for remote 'origin' ({}…):\n\
         2026-08-10 22:10    3 thought(s),   0 linked  {SESSION}  [private]\n      {SESSION_TITLE}\n\
         2026-08-10 22:08    1 thought(s),   0 linked  {}  [private]\n      {AGENT_B_TITLE}\n\
         2026-08-10 22:06    2 thought(s),   0 linked  {}  [private]\n      {AGENT_A_TITLE}\n\
         \n\
         Share one with: cvc share <conversation-id> --remote <name>  (or run `cvc share` interactively)\n",
        &fingerprint(fixture, "origin")[..12],
        agent_b(),
        agent_a(),
    )
}

/// Runs `command` to its end on a terminal and returns what it printed, with
/// the terminal's line endings turned back into line feeds.
fn printed_on_a_terminal(command: Command) -> String {
    let session = PtySession::spawn_sized(command, b"", COLUMNS, ROWS);
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    output.replace("\r\n", "\n")
}

#[test]
fn a_terminal_gets_the_view_and_every_other_invocation_gets_the_plain_listing_unchanged() {
    let fixture = Fixture::new();
    ingest_the_session(&fixture);
    let listing = the_plain_listing(&fixture);

    // Not a terminal: the output piped, nothing on the input.
    assert_eq!(fixture.cvc_ok(&["conversations"]), listing);
    assert_eq!(fixture.cvc_ok(&["conversations", "--plain"]), listing);

    // `--plain` on a terminal, which is how a tool that runs cvc on a pseudo-
    // terminal asks for the listing.
    let printed = printed_on_a_terminal(tui_command(&fixture, &["conversations", "--plain"]));
    assert_eq!(printed, listing);

    // A terminal that cannot redraw gets the listing without being asked to.
    for term in [Some("dumb"), Some(""), None] {
        let mut command = tui_command(&fixture, &["conversations"]);
        match term {
            Some(term) => command.env("TERM", term),
            None => command.env_remove("TERM"),
        };
        assert_eq!(printed_on_a_terminal(command), listing, "TERM={term:?}");
    }

    // Both ends must be a terminal. The shell below leaves one of them a pipe.
    let through_a_shell = |script: &str| {
        let mut command = fixture.command("sh");
        command
            .args(["-c", script])
            .env("CVC", env!("CARGO_BIN_EXE_cvc"))
            .env("TERM", "xterm-256color");
        printed_on_a_terminal(command)
    };
    assert_eq!(
        through_a_shell("\"$CVC\" conversations | cat"),
        listing,
        "input on a terminal, output on a pipe"
    );
    assert_eq!(
        through_a_shell("echo | \"$CVC\" conversations"),
        listing,
        "input on a pipe, output on a terminal"
    );

    // And on a terminal, with nothing else asked, the view opens.
    let mut view = View::open(&fixture);
    assert!(view.session.output().contains(ENTER_ALTERNATE_SCREEN));
    assert_view_is_up(&view.session);
    let screen = view.flat();
    assert!(screen.contains(SESSION_TITLE), "{screen}");
    assert!(!screen.contains("Conversations (most recent first)"));
    assert!(!screen.contains("Share one with:"));
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
}

// ---------------------------------------------------------------------------
// 2. Three phrases, and no typed id, flag or remote name
// ---------------------------------------------------------------------------

#[test]
fn a_session_and_its_subagent_are_shared_and_published_with_three_phrases_and_only_keys() {
    let fixture = Fixture::new();
    ingest_the_session(&fixture);
    fixture.consent_to_sharing();
    let origin = fingerprint(&fixture, "origin");
    let mut view = View::open(&fixture);

    // The session is the row the view starts on: share it.
    view.key('s');
    view.wait_for("not yet shared");
    view.press(Key::Enter);

    // The view has handed the terminal over: the phrase is asked on the main
    // screen, in the terminal's ordinary mode, under a banner that says what
    // the phrase does and where it goes.
    let first = view.prompt(1);
    assert_on_the_main_screen(&view.session);
    let main_screen = flat(&view.session.screen());
    for said in [
        "-- cvc: share",
        "Remote 'origin' ->",
        "Marks this conversation's thoughts as shared for 'origin'. CVC sends nothing to 'origin' yet.",
        "Thoughts added later stay private.",
        "An empty line and Enter cancels.",
    ] {
        assert!(main_screen.contains(said), "{said:?} in:\n{main_screen}");
    }
    view.type_back(&first);
    view.wait_for(&format!(
        "{SESSION_THOUGHTS} thoughts marked shared for remote 'origin'; future thoughts are private."
    ));
    assert_view_is_up(&view.session);
    // Sharing the session does not share its agents, and the notice says that
    // what the agents were asked and answered is in the session's thoughts.
    assert!(
        view.flat().contains("2 agent conversations are not shared"),
        "{}",
        view.flat()
    );
    for agent in [agent_a(), agent_b()] {
        assert_eq!(
            share_record(&fixture, &agent, &origin),
            ShareRecord::default(),
            "{agent}"
        );
    }

    // Open the session and move to its first agent: share that one too.
    view.key('l');
    view.wait_for("└─ agent");
    view.press(Key::Down);
    view.wait_for(&format!("Subagent of \"{SESSION_TITLE}\"."));
    view.key('s');
    view.wait_for(&format!("Share \"{AGENT_A_TITLE}\""));
    view.press(Key::Enter);
    let second = view.type_phrase(2);
    view.wait_for(&format!(
        "{AGENT_A_THOUGHTS} thoughts marked shared for remote 'origin'; future thoughts are private."
    ));

    // Publish what is shared.
    let waiting = SESSION_THOUGHTS + AGENT_A_THOUGHTS;
    view.key('p');
    view.wait_for(&format!("{waiting} waiting by this clone's count"));
    view.press(Key::Enter);
    let third = view.prompt(3);
    assert_on_the_main_screen(&view.session);
    let main_screen = flat(&view.session.screen());
    for said in [
        "-- cvc: publish",
        "Published thoughts cannot be recalled, and scrubbing can miss secrets.",
        &format!("Publishing {waiting} new shared interaction(s) to 'origin'."),
    ] {
        assert!(main_screen.contains(said), "{said:?} in:\n{main_screen}");
    }
    view.type_back(&third);
    view.wait_for(&format!(
        "Published {waiting} new shared thoughts to 'origin'."
    ));
    let status = view.quit();
    assert!(status.success(), "{}", describe(&view.session));
    assert_terminal_restored(&view.session);

    // Three phrases were asked, one for each of the three decisions ...
    let asked = challenges(&view.session.output());
    assert_eq!(asked.len(), 3, "{asked:#?}");
    assert!(
        asked[0].starts_with(&format!("I SHARE {SESSION} {origin} {SESSION_THOUGHTS} ")),
        "{}",
        asked[0]
    );
    assert!(
        asked[1].starts_with(&format!(
            "I SHARE {} {origin} {AGENT_A_THOUGHTS} ",
            agent_a()
        )),
        "{}",
        asked[1]
    );
    assert!(
        asked[2].starts_with(&format!("I PUBLISH {origin} "))
            && asked[2].ends_with(&format!(" {waiting}")),
        "{}",
        asked[2]
    );
    assert_eq!(asked, [first, second, third]);
    // ... and nothing but keys and those phrases was typed: no id, no flag, no
    // remote name.
    assert_eq!(
        view.script(),
        [
            "s", "Enter", A_PHRASE, "l", "Down", "s", "Enter", A_PHRASE, "p", "Enter", A_PHRASE,
            "q"
        ]
    );
    assert_eq!(view.phrases_typed(), asked);

    // The remote holds the nodes of exactly the two conversations shared, and
    // the listing, read by a script, says both are published.
    let expected: BTreeMap<String, usize> = [
        (SESSION.to_owned(), SESSION_THOUGHTS),
        (agent_a(), AGENT_A_THOUGHTS),
    ]
    .into();
    assert_eq!(published_conversations(&fixture.remote), Some(expected));
    assert_eq!(
        listed_state(&fixture, SESSION),
        format!("[shared, {SESSION_THOUGHTS} published]")
    );
    assert_eq!(
        listed_state(&fixture, &agent_a()),
        format!("[shared, {AGENT_A_THOUGHTS} published]")
    );
    // The other agent was never asked about, and nothing of it left the machine.
    assert_eq!(listed_state(&fixture, &agent_b()), "[private]");
    assert_eq!(
        share_record(&fixture, &agent_b(), &origin),
        ShareRecord::default()
    );
}

// ---------------------------------------------------------------------------
// 3. Nesting
// ---------------------------------------------------------------------------

/// The rows of the list: between the table head and the rule above the strip
/// that repeats the selected conversation's title.
fn list_rows(session: &PtySession) -> Vec<String> {
    let screen = session.screen();
    let start = screen
        .iter()
        .position(|row| row.contains("when (UTC)"))
        .expect("the table head")
        + 1;
    let end = screen[start..]
        .iter()
        .position(|row| row.starts_with("───"))
        .map_or(screen.len(), |at| start + at);
    screen[start..end]
        .iter()
        .filter(|row| !row.is_empty())
        .cloned()
        .collect()
}

/// The rows of the list that hold `needle`.
fn rows_with(session: &PtySession, needle: &str) -> Vec<String> {
    list_rows(session)
        .into_iter()
        .filter(|row| row.contains(needle))
        .collect()
}

#[test]
fn subagents_nest_under_their_session_collapsed_by_default() {
    let fixture = Fixture::new();
    ingest_the_session(&fixture);
    let mut view = View::open(&fixture);

    // Collapsed: one row for the session, with a badge that counts its agents.
    // The agents are not rows of their own.
    let session_row = rows_with(&view.session, SESSION_TITLE);
    assert_eq!(session_row.len(), 1, "{session_row:#?}");
    assert!(
        session_row[0].trim_start().starts_with('▸'),
        "{session_row:#?}"
    );
    assert!(session_row[0].contains("+2 agents"), "{session_row:#?}");
    assert!(rows_with(&view.session, "└─ agent").is_empty());
    assert!(!view.flat().contains(AGENT_A_TITLE));

    // Right expands it: the agents sit under the session, in the order they
    // were spawned, and the badge gives way to them.
    view.press(Key::Right);
    view.wait_for("└─ agent");
    let agents = rows_with(&view.session, "└─ agent");
    assert_eq!(agents.len(), 2, "{agents:#?}");
    assert!(agents[0].contains(AGENT_A_TITLE), "{agents:#?}");
    assert!(agents[1].contains(AGENT_B_TITLE), "{agents:#?}");
    let session_row = rows_with(&view.session, SESSION_TITLE);
    assert!(
        session_row[0].trim_start().starts_with('▾'),
        "{session_row:#?}"
    );
    assert!(!session_row[0].contains("+2 agents"), "{session_row:#?}");
    // The rows are in this order on the screen: the session, then its agents.
    let rows = list_rows(&view.session);
    let at = |needle: &str| rows.iter().position(|row| row.contains(needle)).unwrap();
    assert!(at(SESSION_TITLE) < at(AGENT_A_TITLE) && at(AGENT_A_TITLE) < at(AGENT_B_TITLE));

    // Left hides them again.
    view.press(Key::Left);
    view.wait_until_gone("└─ agent");
    let session_row = rows_with(&view.session, SESSION_TITLE);
    assert!(session_row[0].contains("+2 agents"), "{session_row:#?}");
    assert!(!view.flat().contains(AGENT_A_TITLE));

    // `l` and `h` do the same as the arrows.
    view.key('l');
    view.wait_for("└─ agent");
    assert_eq!(rows_with(&view.session, "└─ agent").len(), 2);
    view.key('h');
    view.wait_until_gone("└─ agent");
    assert!(rows_with(&view.session, SESSION_TITLE)[0].contains("+2 agents"));

    // An agent is a row to move to, and says whose it is. Left on an agent
    // goes back to its session; Left on the session hides the agents.
    view.key('l');
    view.wait_for("└─ agent");
    view.press(Key::Down);
    view.wait_for(&format!("Subagent of \"{SESSION_TITLE}\"."));
    assert!(view.flat().contains(AGENT_A_TITLE));
    view.key('h');
    view.wait_until_gone(&format!("Subagent of \"{SESSION_TITLE}\"."));
    assert_eq!(rows_with(&view.session, "└─ agent").len(), 2);
    view.key('h');
    view.wait_until_gone("└─ agent");

    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    // Looking at the agents shared and published nothing.
    assert!(!is_authorized(&fixture, "origin"));
    assert_eq!(plain_listing(&fixture).matches("[private]").count(), 3);
}

// ---------------------------------------------------------------------------
// Details: what a share would cover, shown before the key is pressed
// ---------------------------------------------------------------------------

#[test]
fn details_say_what_a_share_would_cover_before_the_key_is_pressed() {
    let fixture = Fixture::new();
    ingest_the_session(&fixture);
    fixture.consent_to_sharing();
    let origin = fingerprint(&fixture, "origin");
    let mut view = View::open(&fixture);

    // Enter on the session.
    view.press(Key::Enter);
    view.wait_for(&format!("ID {SESSION}"));
    let details = overlay_text(&view.session);
    for said in [
        SESSION_TITLE,
        &format!("ID {SESSION}"),
        "Activity 2026-08-10 22:00 – 22:10 UTC",
        "Thoughts 3",
        "Linked 0 of 3 thoughts linked to a commit",
        "Context files none",
        "Subagents 2 conversations, each shared separately; their prompts and results are part of this one",
        &format!("With 'origin' (fingerprint {}…)", &origin[..12]),
        "State ○ private",
        "Future thoughts private",
        // The count the phrase of a share will carry.
        "Share covers 3 thoughts: all of them",
        "Auto-push off",
    ] {
        assert!(details.contains(said), "{said:?} in:\n{details}");
    }
    view.press(Key::Esc);
    view.wait_until_gone(&format!("ID {SESSION}"));

    // The bar of the share says the same number, and so does its phrase.
    view.key('s');
    view.wait_for("(3 thoughts, 3 not yet shared)");
    view.press(Key::Enter);
    let asked = view.prompt(1);
    assert!(
        asked.starts_with(&format!("I SHARE {SESSION} {origin} 3 ")),
        "{asked}"
    );
    view.answer_with(1, |_| String::new());
    view.wait_for("Not shared: the typed phrase did not match. Nothing was changed.");

    // Enter on an agent says whose it is, and that it is shared on its own.
    view.key('l');
    view.wait_for("└─ agent");
    view.press(Key::Down);
    view.wait_for(&format!("Subagent of \"{SESSION_TITLE}\"."));
    view.press(Key::Enter);
    view.wait_for(&format!("ID {}", agent_a()));
    let details = overlay_text(&view.session);
    for said in [
        AGENT_A_TITLE,
        &format!("Kind Subagent of \"{SESSION_TITLE}\""),
        "Thoughts 2",
        "Share covers 2 thoughts: all of them",
    ] {
        assert!(details.contains(said), "{said:?} in:\n{details}");
    }
    assert!(!details.contains("Subagents"), "{details}");
    view.press(Key::Esc);
    view.wait_until_gone(&format!("ID {}", agent_a()));
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    assert_eq!(
        share_record(&fixture, SESSION, &origin),
        ShareRecord::default()
    );
}

// ---------------------------------------------------------------------------
// 4. Remote switching and authorization
// ---------------------------------------------------------------------------

/// The one row of the picker that names `remote` and says `publishing`.
fn picker_row(session: &PtySession, remote: &str, publishing: &str) -> String {
    session
        .screen()
        .into_iter()
        .find(|row| row.contains(remote) && row.contains(publishing) && row.contains('│'))
        .unwrap_or_else(|| panic!("no picker row for {remote}:\n{}", describe(session)))
}

/// The state column of the row of the list that holds `title`.
fn state_of(session: &PtySession, title: &str) -> String {
    let rows = rows_with(session, title);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    let row = &rows[0];
    // The state is the last column; the glyph that leads it is not a letter.
    let at = row
        .find(['○', '◐', '●', '?'])
        .or_else(|| row.rfind("imported"))
        .unwrap_or_else(|| panic!("no state in {row:?}"));
    row[at..].trim().to_owned()
}

/// Picks `remote` in the picker, with the cursor moved by `moves`.
fn pick_remote(view: &mut View, moves: &[Key], remote: &str) {
    view.key('r');
    view.wait_for("Choose a remote");
    for key in moves {
        view.press(*key);
    }
    view.press(Key::Enter);
    view.wait_for(&format!("remote: {remote}"));
    view.wait_until_gone("Choose a remote");
}

#[test]
fn changing_the_remote_changes_every_state_column_and_the_consent_line() {
    let fixture = Fixture::new();
    ingest_the_session(&fixture);
    fixture.consent_to_sharing();
    // Shared for `origin`: the session and its first agent, not yet published.
    share_with_the_command(&fixture, SESSION);
    share_with_the_command(&fixture, &agent_a());
    // `second` is another destination. `mirror` is the same one under another
    // name: consent is for the push URL, not for what a remote is called.
    add_a_second_remote(&fixture);
    let origin_path = fixture.remote.to_str().unwrap().to_owned();
    fixture.git(&["remote", "add", "mirror", &origin_path]);
    assert_eq!(
        fingerprint(&fixture, "mirror"),
        fingerprint(&fixture, "origin")
    );
    assert_ne!(
        fingerprint(&fixture, "second"),
        fingerprint(&fixture, "origin")
    );

    let mut view = View::open(&fixture);
    let screen = view.flat();
    assert!(screen.contains("remote: origin"), "{screen}");
    assert!(
        screen.contains("publishing: authorized by you auto-push: off"),
        "{screen}"
    );
    assert!(
        screen.contains("5 shared thoughts waiting to publish"),
        "{screen}"
    );

    // The state column of each row, the agents' too, and the badge that stands
    // for them while they are hidden.
    assert!(rows_with(&view.session, SESSION_TITLE)[0].contains("+2 agents · 1 waiting"));
    view.key('l');
    view.wait_for("└─ agent");
    assert_eq!(
        state_of(&view.session, SESSION_TITLE),
        "◐ shared · 3 waiting"
    );
    assert_eq!(
        state_of(&view.session, AGENT_A_TITLE),
        "◐ shared · 2 waiting"
    );
    assert_eq!(state_of(&view.session, AGENT_B_TITLE), "○ private");

    // The picker lists every remote with whether publishing to it is
    // authorized, and `mirror` is as authorized as `origin` is.
    view.key('r');
    view.wait_for("Choose a remote");
    let origin_row = picker_row(&view.session, "origin", "authorized");
    assert!(!origin_row.contains("not authorized"), "{origin_row}");
    let mirror_row = picker_row(&view.session, "mirror", "authorized");
    assert!(!mirror_row.contains("not authorized"), "{mirror_row}");
    picker_row(&view.session, "second", "not authorized yet");
    view.press(Key::Esc);
    view.wait_until_gone("Choose a remote");

    // Choosing `second` changes the consent line and every state column: none
    // of it is shared for that destination, and publishing to it is not
    // authorized.
    pick_remote(&mut view, &[Key::Down], "second");
    let screen = view.flat();
    assert!(
        screen.contains("publishing: not authorized yet (s or p will walk you through it)"),
        "{screen}"
    );
    assert!(!screen.contains("publishing: authorized"), "{screen}");
    assert!(screen.contains("nothing waiting to publish"), "{screen}");
    for title in [SESSION_TITLE, AGENT_A_TITLE, AGENT_B_TITLE] {
        assert_eq!(state_of(&view.session, title), "○ private", "{title}");
    }
    view.key('h');
    view.wait_until_gone("└─ agent");
    assert!(rows_with(&view.session, SESSION_TITLE)[0].contains("+2 agents · 2 private"));
    view.key('l');
    view.wait_for("└─ agent");
    assert!(!is_authorized(&fixture, "second"));

    // `mirror` is the destination `origin` is: the same consent line, the same
    // state, and no phrase for it.
    pick_remote(&mut view, &[Key::Up, Key::Up], "mirror");
    let screen = view.flat();
    assert!(
        screen.contains("publishing: authorized by you auto-push: off"),
        "{screen}"
    );
    assert_eq!(
        state_of(&view.session, SESSION_TITLE),
        "◐ shared · 3 waiting"
    );
    assert_eq!(
        state_of(&view.session, AGENT_A_TITLE),
        "◐ shared · 2 waiting"
    );
    assert_eq!(state_of(&view.session, AGENT_B_TITLE), "○ private");

    // And back to `origin`.
    pick_remote(&mut view, &[Key::Down], "origin");
    assert_eq!(
        state_of(&view.session, SESSION_TITLE),
        "◐ shared · 3 waiting"
    );
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    // Looking around asked for nothing and published nothing.
    assert_eq!(view.prompts_so_far(), 0);
    assert_eq!(published_conversations(&fixture.remote), None);
}

#[test]
fn the_remote_flag_chooses_where_the_view_starts() {
    let (fixture, _id) = authorized_with_a_conversation();
    add_a_second_remote(&fixture);

    // Another remote that is configured.
    let mut view = View::open_with(&fixture, &["conversations", "--remote", "second"]);
    let screen = view.flat();
    assert!(screen.contains("remote: second"), "{screen}");
    assert!(
        screen.contains("publishing: not authorized yet"),
        "{screen}"
    );
    assert!(view.quit().success());

    // One that is not: the header says so, no action is offered for it, and the
    // picker leads to the ones that exist.
    let mut view = View::open_with(&fixture, &["conversations", "--remote", "nowhere"]);
    let screen = view.flat();
    assert!(
        screen.contains("remote: nowhere (not configured)"),
        "{screen}"
    );
    let problem = "Sharing and publishing need a configured remote. Press r to choose one.";
    assert!(screen.contains(problem), "{screen}");
    for key in ['s', 'p'] {
        view.key(key);
        view.wait_for("any key: ok");
        assert!(view.flat().contains(&format!("{problem} any key: ok")));
        view.key('x');
        view.wait_until_gone("any key: ok");
    }
    pick_remote(&mut view, &[], "origin");
    assert!(view
        .flat()
        .contains("publishing: authorized by you auto-push: off"));
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    assert_eq!(view.prompts_so_far(), 0);
}

#[test]
fn a_remote_without_consent_offers_authorization_and_cannot_share_or_publish() {
    let fixture = Fixture::new();
    ingest_the_session(&fixture);
    fixture.consent_to_sharing();
    let second_remote = add_a_second_remote(&fixture);
    let origin = fingerprint(&fixture, "origin");
    let second = fingerprint(&fixture, "second");
    let mut view = View::open(&fixture);
    pick_remote(&mut view, &[Key::Down], "second");
    assert!(view
        .flat()
        .contains("publishing: not authorized yet (s or p will walk you through it)"));
    let offer = "is not authorized in this clone yet.";

    // `s` offers to authorize. It does not offer to share.
    view.key('s');
    view.wait_for(offer);
    let bar = view.flat();
    assert!(bar.contains("Publishing to 'second'"), "{bar}");
    assert!(
        bar.contains("Enter: authorize (you will type a phrase) Esc: cancel"),
        "{bar}"
    );
    assert!(!bar.contains("not yet shared"), "{bar}");
    // Esc closes the offer and records nothing.
    view.press(Key::Esc);
    view.wait_until_gone(offer);
    assert_eq!(view.prompts_so_far(), 0);
    assert!(!is_authorized(&fixture, "second"));

    // `p` offers to authorize. It does not offer to publish.
    view.key('p');
    view.wait_for(offer);
    let bar = view.flat();
    assert!(bar.contains("Publishing to 'second'"), "{bar}");
    assert!(!bar.contains("waiting by this clone's count"), "{bar}");
    view.press(Key::Esc);
    view.wait_until_gone(offer);
    assert_eq!(view.prompts_so_far(), 0);

    // Enter asks the phrase that authorizes `second`, for its own fingerprint
    // and not for that of `origin`. A wrong phrase authorizes nothing.
    view.key('p');
    view.wait_for(offer);
    view.press(Key::Enter);
    let asked = view.answer_with(1, |_| "not the phrase".to_owned());
    assert_eq!(asked, format!("I AUTHORIZE SHARING {second}"));
    view.wait_for("Not authorized: the typed phrase did not match.");
    assert!(!is_authorized(&fixture, "second"));
    assert!(is_authorized(&fixture, "origin"));

    // The right phrase authorizes `second`, and that is all it does: asked for
    // by `s`, it does not go on to share.
    view.key('s');
    view.wait_for(offer);
    view.press(Key::Enter);
    let asked = view.type_phrase(2);
    assert_eq!(asked, format!("I AUTHORIZE SHARING {second}"));
    view.wait_for("Publishing to 'second' is now authorized. Auto-push stays off.");
    assert!(is_authorized(&fixture, "second"));
    assert!(!auto_push_is_on(&fixture, "second"));
    view.wait_for("publishing: authorized by you auto-push: off");
    view.wait_quietly(Duration::from_millis(800));
    assert_eq!(view.prompts_so_far(), 2);
    let screen = view.flat();
    assert!(!screen.contains("not yet shared"), "{screen}");
    assert!(
        !screen.contains("waiting by this clone's count"),
        "{screen}"
    );
    view.is_at_the_list();
    assert_eq!(
        share_record(&fixture, SESSION, &second),
        ShareRecord::default()
    );
    assert_eq!(state_of(&view.session, SESSION_TITLE), "○ private");
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);

    // Only the two authorization phrases were asked: no share and no publish
    // phrase, for either remote, and nothing left the machine.
    let asked = challenges(&view.session.output());
    assert_eq!(asked.len(), 2, "{asked:#?}");
    assert!(asked
        .iter()
        .all(|phrase| phrase.starts_with("I AUTHORIZE SHARING ")));
    assert_eq!(
        share_record(&fixture, SESSION, &origin),
        ShareRecord::default()
    );
    assert_eq!(published_conversations(&fixture.remote), None);
    assert_eq!(published_conversations(&second_remote), None);
}

#[test]
fn a_remote_that_moved_while_the_view_was_open_is_not_acted_on() {
    let (fixture, id) = authorized_with_a_conversation();
    let origin = fingerprint(&fixture, "origin");
    let mut view = View::open(&fixture);
    // The bar is about the destination the header names ...
    view.key('s');
    view.wait_for("not yet shared");

    // ... and the remote is pointed somewhere else before Enter.
    let elsewhere = fixture.temp.path().join("elsewhere.git");
    git2::Repository::init_bare(&elsewhere).unwrap();
    fixture.git(&["remote", "set-url", "origin", elsewhere.to_str().unwrap()]);
    assert_ne!(fingerprint(&fixture, "origin"), origin);
    view.press(Key::Enter);

    // Nothing runs on the new destination on the strength of a decision about
    // the old one. The view says so and shows the destination as it is now,
    // which the consent given for the old one does not cover.
    view.wait_for(
        "Remote 'origin' changed while this view was open, so nothing was done. The remote list was refreshed.",
    );
    assert_eq!(view.prompts_so_far(), 0);
    assert!(!view.session.output().contains("-- cvc:"));
    view.wait_for("publishing: not authorized yet");
    assert!(!is_authorized(&fixture, "origin"));
    view.key('s');
    view.wait_for("is not authorized in this clone yet.");
    assert!(!view.flat().contains("not yet shared"));
    view.press(Key::Esc);
    view.wait_until_gone("is not authorized in this clone yet.");
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    assert_eq!(
        share_record(&fixture, &id, &fingerprint(&fixture, "origin")),
        ShareRecord::default()
    );
    assert_eq!(share_record(&fixture, &id, &origin), ShareRecord::default());
}

#[test]
fn without_a_remote_nothing_can_be_shared_or_published() {
    let (fixture, id) = authorized_with_a_conversation();
    fixture.git(&["remote", "remove", "origin"]);
    let mut view = View::open(&fixture);
    let screen = view.flat();
    assert!(screen.contains("remote: none (no Git remotes)"), "{screen}");
    assert!(
        screen.contains("Add a remote to share: git remote add origin <url>"),
        "{screen}"
    );
    for key in ['s', 'u', 'p'] {
        view.key(key);
        view.wait_for("any key: ok");
        assert!(view
            .flat()
            .contains("Add a remote to share: git remote add origin <url> any key: ok"));
        view.key('x');
        view.wait_until_gone("any key: ok");
    }
    assert_eq!(view.prompts_so_far(), 0);
    assert!(!view.session.output().contains("-- cvc:"));
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    let listing = plain_listing(&fixture);
    assert!(
        listing.contains("no remote resolved for share state"),
        "{listing}"
    );
    assert!(listing.contains(&id), "{listing}");
}

// ---------------------------------------------------------------------------
// 7. Blocked actions
// ---------------------------------------------------------------------------

/// A second clone of the repository that `fixture` is a clone of: its own
/// directory and its own index, with the same remote as `origin`.
fn another_clone_of(fixture: &Fixture) -> Fixture {
    let clone = Fixture::new();
    clone.git(&[
        "remote",
        "set-url",
        "origin",
        fixture.remote.to_str().unwrap(),
    ]);
    clone.cvc_ok(&["init"]);
    clone
}

#[test]
fn an_imported_conversation_is_read_only_in_the_view() {
    // One clone shares a conversation and publishes it ...
    let (author, id) = authorized_with_a_conversation();
    share_with_the_command(&author, &id);
    publish_with_the_command(&author);
    assert!(published_conversations(&author.remote).is_some());

    // ... another clone, authorized for the same remote, pulls it.
    let reader = another_clone_of(&author);
    reader.consent_to_sharing();
    let pulled = reader.cvc_ok(&["pull"]);
    assert!(pulled.contains("Pulled 1 interaction(s)"), "{pulled}");
    let imported = format!("remote:{}:{id}", fingerprint(&reader, "origin"));
    assert_eq!(
        listed_state(&reader, &imported),
        "[published, not shared here]"
    );

    let mut view = View::open(&reader);
    view.wait_for("imported");
    let screen = view.flat();
    assert!(
        screen.contains("Imported from a remote, so it is read-only here."),
        "{screen}"
    );
    for (key, name) in [('s', "share"), ('u', "unshare")] {
        view.key(key);
        view.wait_for("any key: ok");
        let screen = view.flat();
        assert!(
            screen.contains("Imported from a remote, so it is read-only here. any key: ok"),
            "{name}: {screen}"
        );
        // No question about what a phrase would do, and no way on to one.
        assert!(!screen.contains("Esc: cancel"), "{name}: {screen}");
        view.press(Key::Enter);
        view.wait_until_gone("any key: ok");
        assert_eq!(view.prompts_so_far(), 0, "{name}");
    }
    view.is_at_the_list();
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    assert!(!view.session.output().contains("-- cvc:"));
    assert_eq!(
        share_record(&reader, &imported, &fingerprint(&reader, "origin")).shared_thoughts,
        0
    );
}

#[test]
fn an_unshare_after_a_publish_is_refused_before_any_phrase() {
    let (fixture, id) = authorized_with_a_conversation();
    share_with_the_command(&fixture, &id);
    publish_with_the_command(&fixture);
    let origin = fingerprint(&fixture, "origin");
    let before = share_record(&fixture, &id, &origin);
    assert_eq!(before.shared_thoughts, 1);
    assert_eq!(before.publications, ["published"]);

    let mut view = View::open(&fixture);
    view.wait_for("● published");
    for _ in 0..2 {
        view.key('u');
        view.wait_for("any key: ok");
        let screen = view.flat();
        assert!(
            screen.contains(
                "Cannot unshare: 1 thought is already published to 'origin', and published thoughts cannot be recalled."
            ),
            "{screen}"
        );
        assert!(!screen.contains("Esc: cancel"), "{screen}");
        // Any key closes it. This one is not one that does anything else.
        view.key('x');
        view.wait_until_gone("any key: ok");
    }
    assert_eq!(view.prompts_so_far(), 0);
    view.is_at_the_list();
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);

    assert!(!view.session.output().contains("-- cvc:"));
    assert_eq!(share_record(&fixture, &id, &origin), before);
    assert_eq!(listed_state(&fixture, &id), "[shared, 1 published]");
}

#[test]
fn an_unshare_of_something_never_shared_is_refused_before_any_phrase() {
    let (fixture, id) = authorized_with_a_conversation();
    let mut view = View::open(&fixture);
    view.key('u');
    view.wait_for("any key: ok");
    assert!(view
        .flat()
        .contains("Not shared for 'origin', so there is nothing to unshare. any key: ok"));
    view.key('x');
    view.wait_until_gone("any key: ok");
    assert_eq!(view.prompts_so_far(), 0);
    view.is_at_the_list();
    assert!(view.quit().success());
    assert!(!view.session.output().contains("-- cvc:"));
    assert_eq!(listed_state(&fixture, &id), "[private]");
}

// ---------------------------------------------------------------------------
// 8. The terminal is given back on every way out
// ---------------------------------------------------------------------------

#[test]
fn the_terminal_is_given_back_after_q() {
    let (fixture, id) = authorized_with_a_conversation();
    let mut view = View::open(&fixture);
    assert_view_is_up(&view.session);
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);
    let output = view.session.output();
    assert!(output.rfind(LEAVE_ALTERNATE_SCREEN) > output.rfind(ENTER_ALTERNATE_SCREEN));
    assert_eq!(listed_state(&fixture, &id), "[private]");
}

#[test]
fn the_terminal_is_given_back_after_ctrl_c_at_the_phrase_prompt() {
    use std::os::unix::process::ExitStatusExt;

    let (fixture, id) = authorized_with_a_conversation();
    let mut view = View::open(&fixture);
    view.key('s');
    view.wait_for("not yet shared");
    view.press(Key::Enter);
    view.prompt(1);
    // The view is suspended at the prompt: this is the ordinary terminal, where
    // Ctrl-C is a signal, and the process ends as it would without the view.
    assert_on_the_main_screen(&view.session);
    view.press(Key::CtrlC);
    let status = view.session.wait_for_exit();
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
    assert_terminal_restored(&view.session);

    // The phrase was never answered: nothing was shared.
    let origin = fingerprint(&fixture, "origin");
    assert_eq!(share_record(&fixture, &id, &origin), ShareRecord::default());
    assert_eq!(listed_state(&fixture, &id), "[private]");
}

#[test]
fn the_terminal_is_given_back_after_ctrl_c_in_a_bar() {
    let (fixture, id) = authorized_with_a_conversation();
    let mut view = View::open(&fixture);
    view.key('s');
    view.wait_for("not yet shared");
    // In the view Ctrl-C is a key, and it quits without going on.
    view.press(Key::CtrlC);
    let status = view.session.wait_for_exit();
    assert!(status.success(), "{}", describe(&view.session));
    assert_terminal_restored(&view.session);
    assert_eq!(view.prompts_so_far(), 0);
    assert_eq!(listed_state(&fixture, &id), "[private]");
}

#[test]
fn the_terminal_is_given_back_around_a_flow_that_fails() {
    let (fixture, id) = authorized_with_a_conversation();
    share_with_the_command(&fixture, &id);
    let origin = fingerprint(&fixture, "origin");
    let before = share_record(&fixture, &id, &origin);
    let mut view = View::open(&fixture);
    view.wait_for("◐ shared · 1 waiting");

    // The remote goes away while the view is open. The publish cannot even
    // look at it, so it fails before it can ask for a phrase.
    std::fs::remove_dir_all(&fixture.remote).unwrap();
    view.key('p');
    view.wait_for("1 waiting by this clone's count");
    view.press(Key::Enter);

    // The failure is printed on the ordinary screen, where it can be read,
    // and the view waits to be told to go on.
    view.wait_for_main_screen("Press Enter to return to cvc conversations.");
    assert_on_the_main_screen(&view.session);
    let printed = view.session.plain_output();
    assert!(printed.contains("cvc: publish failed: "), "{printed}");
    assert!(
        printed.contains("Publish to 'origin' did not complete; the remote may hold some of it."),
        "{printed}"
    );
    assert_eq!(view.prompts_so_far(), 0);
    view.wait_quietly(Duration::from_millis(400));
    assert!(!view.session.has_exited());

    // Enter ends the wait. The wait throws away what was typed before it began
    // to listen, so an Enter that came too soon is sent again; one that comes
    // when the view is back is a line feed, which is Ctrl-J there and does
    // nothing.
    let notice = "did not complete; the remote may hold some of it";
    let mut back = false;
    for _ in 0..5 {
        view.session.write(b"\n");
        let until = Instant::now() + Duration::from_secs(3);
        while !back && Instant::now() < until {
            let (on_alternate, rows) = screen_state(&view.session);
            back = on_alternate && flat(&rows).contains(notice);
            std::thread::sleep(Duration::from_millis(15));
        }
        if back {
            break;
        }
    }

    // Back in the view, with the failure in its notice.
    view.wait_for(notice);
    assert_view_is_up(&view.session);
    view.is_at_the_list();
    assert!(view.quit().success());
    assert_terminal_restored(&view.session);

    // A publish that never got as far as a phrase records nothing.
    assert_eq!(share_record(&fixture, &id, &origin), before);
    assert_eq!(listed_state(&fixture, &id), "[shared, 0/1 published]");
}
