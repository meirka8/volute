//! The interactive view of `cvc conversations`: a full-screen list of the
//! captured conversations with their share state for one remote, from which a
//! conversation can be shared or unshared, shared thoughts published, and
//! publishing to a remote authorized.
//!
//! The view is a thin front. It adds no way to consent and no write path of
//! its own:
//!
//! - Every share, unshare, publish and authorization runs the very function
//!   the matching subcommand runs (`commands::sync`), which asks the same typed
//!   phrase through the private `typed_acknowledgement`: on the controlling
//!   terminal, with type-ahead discarded, matched exactly. No key in the view
//!   is a grant; a key in a pre-flight bar only asks this loop to start the
//!   flow.
//! - Before a flow the screen is suspended completely (alternate screen left,
//!   raw mode off, cursor shown, output flushed), so the flow prints and asks on
//!   the ordinary main screen, as the subcommand would. Afterwards the screen is
//!   resumed and the next key is not read until the input has been quiet for a
//!   moment (and what arrives until then is thrown away), as it is not at the
//!   start.
//! - The destination is resolved at the moment of the action, with
//!   `RemoteOverview::resolve`, which fails closed if the remote now resolves
//!   to a different destination than the one that was listed. A remote is never
//!   picked implicitly.
//! - Input is read only with crossterm's synchronous `poll` and `read`, on this
//!   thread, and never while a flow runs: nothing else reads the terminal while
//!   a challenge is answered.
//! - Where the platform cannot keep type-ahead from answering a challenge, the
//!   view only browses (`conversations::consent_actions_allowed`).
//!
//! Layout: `model` is the state and the key handling, free of any terminal;
//! `tree` groups agents under their sessions; `words` is everything the view
//! says; `view` draws; `terminal` owns the terminal's modes; `text` sanitizes
//! and fits text.

mod model;
mod terminal;
mod text;
mod tree;
mod view;
mod words;

use std::io::{self, Write};

use anyhow::Result;
use cvc_core::db::CvcStore;
use cvc_core::privacy;
use git2::Repository;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::commands::conversations::{consent_actions_allowed, remote_overview, RemoteOverview};
use crate::commands::failure::{classify_failure, FlowFailure};
use crate::commands::sync;
use model::{Action, Data, Flow, Model, Report, RowInfo, Settings, MIN_HEIGHT, MIN_WIDTH};
use terminal::Screen;
use text::Environment;
use view::Theme;
use words::{Destination, FlowKind};

/// How `run` ended.
pub(crate) enum Launch {
    /// The view ran, or had nothing to show and said so.
    Finished,
    /// The terminal is too small for the view; the caller prints the plain list.
    PlainListing,
}

/// What the view needs from the command.
pub(crate) struct Start<'a> {
    pub repo: &'a Repository,
    pub store: &'a CvcStore,
    /// `--remote`, if given.
    pub requested: Option<&'a str>,
    /// How many conversations to load, most recent first.
    pub limit: usize,
    /// Whether this checkout records Claude Code sessions (for the hint shown
    /// when there is nothing to list).
    pub harness_installed: bool,
}

struct Context<'a> {
    repo: &'a Repository,
    store: &'a CvcStore,
    limit: usize,
    theme: Theme,
}

/// Runs the view. The caller has established that stdin and stdout are
/// terminals and that `TERM` can redraw (`interactive_ui_allowed`).
pub(crate) fn run(start: Start<'_>) -> Result<Launch> {
    match terminal::size() {
        Ok((width, height)) if width >= MIN_WIDTH && height >= MIN_HEIGHT => {}
        Ok((width, height)) => {
            println!(
                "Interactive view unavailable (terminal too small, {width}x{height}). Showing the plain list."
            );
            return Ok(Launch::PlainListing);
        }
        Err(_) => {
            println!(
                "Interactive view unavailable (terminal size unknown). Showing the plain list."
            );
            return Ok(Launch::PlainListing);
        }
    }

    let environment = Environment::detect();
    let ctx = Context {
        repo: start.repo,
        store: start.store,
        limit: start.limit,
        theme: Theme {
            color: environment.color,
        },
    };
    let remotes = remote_overview(start.repo)?;
    let remote = model::initial_remote(&remotes, start.requested);
    let data = load(&ctx, remotes, remote.as_deref())?;
    if data.summaries.iter().all(|summary| summary.thoughts == 0) {
        // Nothing to list: say so on the main screen, with the one next step.
        let capture_acknowledged = privacy::capture_acknowledged(start.repo).unwrap_or(false);
        for line in words::empty_state(capture_acknowledged, start.harness_installed) {
            println!("{line}");
        }
        return Ok(Launch::Finished);
    }

    let mut model = Model::new(
        data,
        start.requested,
        Settings {
            consent_actions: consent_actions_allowed(),
            glyphs: environment.glyphs,
        },
    );
    let mut screen = Screen::enter()?;
    let outcome = event_loop(&ctx, &mut screen, &mut model);
    // The terminal is back before anything else is printed, on any outcome.
    drop(screen);
    outcome?;
    if let Some(note) = exit_note(&ctx, &model) {
        println!("{note}");
    }
    Ok(Launch::Finished)
}

fn event_loop(ctx: &Context, screen: &mut Screen, model: &mut Model) -> Result<()> {
    loop {
        let (width, height) = terminal::size()?;
        model.set_size(width, height);
        if let Some(id) = model.wants_info() {
            read_info(ctx, model, &id);
        }
        screen.draw(|frame| view::render(frame, model, ctx.theme))?;

        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                // Whatever this key does, it does on what the store says now.
                if let Some(id) = model.selected_id().map(str::to_owned) {
                    read_info(ctx, model, &id);
                }
                match model.handle_key(key) {
                    None => {}
                    Some(Action::Quit) => return Ok(()),
                    Some(Action::Switch(name)) => switch_remote(ctx, model, name),
                    Some(Action::Run(flow)) => run_flow(ctx, screen, model, flow)?,
                }
            }
            // A resize is picked up at the top of the loop. A paste, a click
            // and a focus change do nothing: a paste in particular must never
            // become a stream of keys.
            _ => {}
        }
    }
}

/// Loads the conversations and the publication counts for `remote`.
fn load(ctx: &Context, remotes: Vec<RemoteOverview>, remote: Option<&str>) -> Result<Data> {
    let fingerprint = model::fingerprint_of(&remotes, remote).map(str::to_owned);
    let summaries = ctx
        .store
        .list_conversation_summaries(fingerprint.as_deref(), ctx.limit)?;
    let counts = fingerprint
        .as_deref()
        .map(|fingerprint| ctx.store.destination_publication_counts(fingerprint))
        .transpose()?;
    Ok(Data {
        summaries,
        remotes,
        counts,
    })
}

/// Loads everything again for the selected remote, keeping the old data and
/// saying so if that fails.
fn reload(ctx: &Context, model: &mut Model) {
    let remote = model.remote_name().map(str::to_owned);
    match remote_overview(ctx.repo).and_then(|remotes| load(ctx, remotes, remote.as_deref())) {
        Ok(data) => model.apply(data),
        Err(error) => {
            let reason = format!("{error:#}");
            model.set_status(&format!(
                "Could not refresh the list: {}",
                text::first_line(&reason)
            ));
        }
    }
}

/// Loads everything for another remote and, only if that works, shows it. The
/// view otherwise stays as it was, so the remote named in the header is always
/// the one the share state is for.
fn switch_remote(ctx: &Context, model: &mut Model, name: String) {
    match remote_overview(ctx.repo).and_then(|remotes| load(ctx, remotes, Some(&name))) {
        Ok(data) => model.switch_to(name, data),
        Err(error) => {
            let reason = format!("{error:#}");
            model.set_status(&format!(
                "Could not switch remote: {}",
                text::first_line(&reason)
            ));
        }
    }
}

/// Reads what the details, the strip and the bars show about one conversation.
/// A read that fails is recorded as such, so nothing is offered on stale or
/// missing numbers.
fn read_info(ctx: &Context, model: &mut Model, id: &str) {
    let fingerprint = model.destination_fingerprint().map(str::to_owned);
    let read = || -> Result<Option<RowInfo>> {
        let Some(details) = ctx.store.conversation_details(id, fingerprint.as_deref())? else {
            return Ok(None);
        };
        let unshare = fingerprint
            .as_deref()
            .map(|fingerprint| ctx.store.can_unshare(id, fingerprint))
            .transpose()?;
        Ok(Some(RowInfo { details, unshare }))
    };
    model.set_info(id, read().unwrap_or(None));
}

/// Suspends the screen, runs one flow on the main screen, and resumes.
fn run_flow(ctx: &Context, screen: &mut Screen, model: &mut Model, flow: Flow) -> Result<()> {
    let kind = flow.kind();

    // The model already refuses to ask for a flow where a challenge cannot be
    // protected from type-ahead. This is the same rule once more, at the one
    // place that starts a challenge.
    if !consent_actions_allowed() {
        model.show_blocked(words::NOT_ON_THIS_PLATFORM);
        return Ok(());
    }

    // The phrase of a share or an unshare names the conversation by its id. One
    // that cannot be shown safely is not asked about (the model does not open a
    // bar for it either); core refuses such a phrase as well, as a backstop.
    if let Flow::Share { id, .. } | Flow::Unshare { id } = &flow {
        if !words::id_is_safe(id) {
            model.show_blocked(words::UNSAFE_ID);
            return Ok(());
        }
    }

    // The destination is resolved now, not when the list was loaded. If the
    // remote has been edited since, nothing runs.
    let Some(overview) = model.selected_remote() else {
        reload(ctx, model);
        model.report(Report::RemoteChanged);
        return Ok(());
    };
    let destination = match overview.resolve(ctx.repo) {
        Ok(destination) => destination,
        Err(_) => {
            reload(ctx, model);
            model.report(Report::RemoteChanged);
            return Ok(());
        }
    };

    // An imported conversation has no actions, whatever the list said when it
    // was loaded.
    if let Flow::Share { id, .. } | Flow::Unshare { id } = &flow {
        match ctx
            .store
            .conversation_details(id, Some(&destination.fingerprint))
        {
            Ok(Some(details)) if details.summary.imported => {
                model.show_blocked(words::IMPORTED_IS_READ_ONLY);
                return Ok(());
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                reload(ctx, model);
                model.set_status("That conversation is no longer listed.");
                return Ok(());
            }
            Err(error) => {
                let failure = FlowFailure::Other(format!("{error}"));
                model.report(Report::Failed { kind, failure });
                return Ok(());
            }
        }
    }

    // A share and a publish are asked for on the settings the screen showed: the
    // header says whether auto-push is on, the share bar and banner warn that a
    // git push will publish what is shared, and the publish bar assumes the
    // remote is authorized. Those are read when the list is loaded, so look at
    // them again now, and if another terminal changed either, run nothing: the
    // screen is refreshed and the key has to be pressed again on the new state.
    if matches!(flow, Flow::Share { .. } | Flow::Publish) {
        match privacy::privacy_status_for_fingerprint(ctx.repo, &destination.fingerprint) {
            Ok(status) if publishing_settings_match(&overview, &status) => {}
            Ok(_) => {
                reload(ctx, model);
                model.set_status(&words::settings_changed_status(&text::sanitize(
                    &overview.name,
                )));
                return Ok(());
            }
            Err(error) => {
                let failure = FlowFailure::Other(format!("{error}"));
                model.report(Report::Failed { kind, failure });
                return Ok(());
            }
        }
    }

    // Authorizing an authorized remote would reset its auto-push setting. The
    // offer is only made while the remote is not authorized, but another
    // terminal may have authorized it since: look again, and do nothing then.
    if matches!(flow, Flow::Authorize) {
        match privacy::privacy_status_for_fingerprint(ctx.repo, &destination.fingerprint) {
            Ok(status) if status.sharing_consented => {
                reload(ctx, model);
                model.set_status(&words::already_authorized_status(&text::sanitize(
                    &overview.name,
                )));
                return Ok(());
            }
            Ok(_) => {}
            Err(error) => {
                let failure = FlowFailure::Other(format!("{error}"));
                model.report(Report::Failed { kind, failure });
                return Ok(());
            }
        }
    }

    screen.suspend()?;
    let future = match &flow {
        Flow::Share { future, .. } => Some(*future),
        _ => None,
    };
    let dest = Destination::of(&overview, model.glyphs());
    for line in words::banner(kind, &dest, future, model.glyphs()) {
        println!("{}", text::sanitize(&line));
    }

    // The flows below are the subcommands' own functions. Publishing is never
    // bundled with a share: it is its own action with its own challenge.
    let outcome: Result<Report> = match &flow {
        Flow::Share { id, future } => {
            sync::share_conversation(ctx.repo, ctx.store, &destination, id, *future, false).map(
                |outcome| Report::Shared {
                    id: id.clone(),
                    outcome,
                },
            )
        }
        Flow::Unshare { id } => sync::unshare_conversation(ctx.repo, ctx.store, &destination, id)
            .map(|made_private| Report::Unshared { made_private }),
        Flow::Publish => {
            sync::publish_manually(ctx.repo, ctx.store, &destination).map(Report::Published)
        }
        Flow::Authorize => {
            sync::acknowledge_sharing_for(ctx.repo, &destination).map(|()| Report::Authorized)
        }
    };

    let report = match outcome {
        Ok(report) => report,
        Err(error) => {
            let failure = classify_failure(&error);
            if let FlowFailure::Other(reason) | FlowFailure::OutcomeUnknown(reason) = &failure {
                // Not one of the expected refusals: show it whole, on the main
                // screen, and wait, so it can be read before the list returns.
                print_failure(kind, reason, &dest);
                wait_for_enter();
            }
            Report::Failed { kind, failure }
        }
    };
    let _ = io::stdout().flush();

    if let Err(error) = screen.resume() {
        // The view cannot go on (input that never stops arriving is the likely
        // reason), but what the flow came to is still told: the screen is given
        // back and the notice printed on it, where the flow itself printed.
        let _ = screen.suspend();
        reload(ctx, model);
        model.report(report);
        for line in model.notice() {
            println!("{}", line.trim_end());
        }
        return Err(error.into());
    }
    reload(ctx, model);
    model.report(report);
    Ok(())
}

/// Whether the publishing settings of a destination are what the listing said:
/// authorized or not, auto-push on or off.
fn publishing_settings_match(listed: &RemoteOverview, now: &privacy::PrivacyStatus) -> bool {
    listed.sharing_consented == now.sharing_consented && listed.auto_push == now.auto_push
}

/// Prints an unexpected failure on the main screen, every line sanitized.
fn print_failure(kind: FlowKind, reason: &str, dest: &Destination) {
    let mut lines = reason.lines();
    println!(
        "cvc: {} failed: {}",
        kind.banner_name(),
        text::sanitize(lines.next().unwrap_or(""))
    );
    for line in lines {
        println!("  {}", text::sanitize(line));
    }
    if kind == FlowKind::Publish {
        // The push may have started before the error.
        println!("{}", words::publish_unfinished(&dest.name));
    }
}

/// Waits for Enter on the controlling terminal, after discarding whatever was
/// typed while the failure was printed. Enter grants nothing, so the keys typed
/// ahead are flushed once and the wait does not insist on a quiet terminal.
fn wait_for_enter() {
    println!("Press Enter to return to cvc conversations.");
    let _ = io::stdout().flush();
    if let Ok(mut tty) = crate::tty::Terminal::open() {
        let _ = tty.flush_input();
        let _ = tty.prompt_line("");
    }
}

/// What is printed after the screen is restored: shared thoughts that were left
/// unpublished, when something was shared this session and nothing publishes
/// them by itself.
fn exit_note(ctx: &Context, model: &Model) -> Option<String> {
    let name = model.shared_remote()?;
    let overview = model.overview_named(name)?;
    if overview.auto_push {
        return None;
    }
    let fingerprint = overview.fingerprint.as_deref().ok()?;
    let counts = ctx.store.destination_publication_counts(fingerprint).ok()?;
    let waiting = counts.unpublished();
    (waiting > 0).then(|| words::waiting_note(waiting, &text::sanitize(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(sharing_consented: bool, auto_push: bool) -> RemoteOverview {
        RemoteOverview {
            name: "origin".to_owned(),
            fingerprint: Ok("f".repeat(64)),
            location: None,
            sharing_consented,
            auto_push,
            consent_error: None,
        }
    }

    fn now(sharing_consented: bool, auto_push: bool) -> privacy::PrivacyStatus {
        privacy::PrivacyStatus {
            capture_acknowledged: true,
            sharing_consented,
            auto_push,
            remote_fingerprint: "f".repeat(64),
        }
    }

    #[test]
    fn publishing_settings_match_only_when_authorization_and_auto_push_both_do() {
        for consented in [false, true] {
            for auto_push in [false, true] {
                assert!(publishing_settings_match(
                    &listed(consented, auto_push),
                    &now(consented, auto_push)
                ));
            }
        }
        // Auto-push turned on, or off, since the listing.
        assert!(!publishing_settings_match(
            &listed(true, false),
            &now(true, true)
        ));
        assert!(!publishing_settings_match(
            &listed(true, true),
            &now(true, false)
        ));
        // Authorized, or revoked, since the listing.
        assert!(!publishing_settings_match(
            &listed(false, false),
            &now(true, false)
        ));
        assert!(!publishing_settings_match(
            &listed(true, false),
            &now(false, false)
        ));
    }
}
