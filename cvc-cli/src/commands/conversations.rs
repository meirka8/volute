use crate::commands::sync::SUMMARY_SCAN_LIMIT;
use crate::terminal_text::sanitize_for_terminal;
use crate::tui::tree::{Kind, Tree};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{TimeZone, Utc};
use cvc_core::claude_code;
use cvc_core::db::{ConversationSummary, CvcStore};
use cvc_core::harness::engine::{self, Backlog};
use cvc_core::harness::{with_adapter, AdapterVisitor, HarnessAdapter, HarnessId};
use cvc_core::privacy;
use cvc_core::repository::RepositoryLayout;
use git2::Repository;
use std::collections::{BTreeMap, BTreeSet};
use std::env;

/// How many conversations the plain listing shows when `--limit` is absent.
const PLAIN_DEFAULT_LIMIT: usize = 20;

/// The bracketed share state of one listing line, for the destination the
/// summary was read for.
///
/// - Something is shared from here: `[shared, N published]` when every thought
///   is published, else `[shared, P/T published]`. P counts the shared thoughts
///   the destination is known to hold.
/// - Nothing is shared from here, but the destination holds, or may hold,
///   thoughts of it (`held`): `[published, not shared here]` when it is known to
///   hold some, `[unconfirmed, not shared here]` when every one of them is a
///   publish that did not conclude. This is how a re-ingested session or an
///   imported conversation reads. It is never `[private]`, which would send a
///   person deciding whether to redact it looking in the wrong place.
/// - Nothing is shared or held, but later thoughts are shared as they are
///   captured: `[shared, future only]`.
/// - Otherwise `[private]`.
fn share_state(summary: &ConversationSummary) -> String {
    if summary.shared {
        if summary.published > 0 && summary.published == summary.thoughts {
            format!("[shared, {} published]", summary.published)
        } else {
            format!(
                "[shared, {}/{} published]",
                summary.published, summary.thoughts
            )
        }
    } else if summary.held > 0 {
        if summary.held_published > 0 {
            "[published, not shared here]".into()
        } else {
            "[unconfirmed, not shared here]".into()
        }
    } else if summary.share_future == Some(true) {
        "[shared, future only]".into()
    } else {
        "[private]".into()
    }
}

/// What follows the agent of an imported conversation wherever it is named.
const IMPORTED_AGENT_CAVEAT: &str = "(as its imported id says)";

/// Which agent recorded a conversation, as every reader names it (the plain
/// listing, the share preamble, and the view's row, strip and details), or
/// `None` when that is not known: an agent that is unknown, or a conversation
/// that more than one source wrote to (a Claude Code session that also holds
/// MCP thoughts, say). For an imported conversation the agent is what its id
/// names, the publisher's claim, and the label always says so.
pub(crate) fn agent_label(summary: &ConversationSummary) -> Option<String> {
    let agent = summary.agent?.display();
    Some(if summary.imported {
        format!("{agent} {IMPORTED_AGENT_CAVEAT}")
    } else {
        agent.to_owned()
    })
}

/// [`agent_label`] as a listing line says it: `recorded by <label>`.
fn agent_words(summary: &ConversationSummary) -> Option<String> {
    agent_label(summary).map(|label| format!("recorded by {}", sanitize_for_terminal(&label)))
}

/// Renders one summary line. Kept width-stable so the picker and the listing
/// read identically.
///
/// The id and the title are text from the store, and a title is whatever the
/// harness or the first prompt line said (a Claude Code `ai-title`, a subagent
/// description), secret-scrubbed and nothing more, so a prompt-injected agent
/// can put escape sequences in it. Both go through
/// [`sanitize_for_terminal`](crate::terminal_text::sanitize_for_terminal): a
/// title cannot redraw the lines above it, and a title with a line break stays
/// one title. Text without such characters is printed as it is.
pub(crate) fn render_line(summary: &ConversationSummary, with_destination: bool) -> String {
    render_entry(summary, with_destination, &Marks::default())
}

/// What the listing adds to a line beyond the summary itself.
#[derive(Default)]
struct Marks<'a> {
    /// Listed under its parent: both lines are indented.
    nested: bool,
    /// A subagent that is not listed under its parent, with or without agents
    /// of its own: the parent's id.
    orphan_of: Option<&'a str>,
    /// A hook run left the session unread to the end.
    backlog: bool,
}

/// The first line is `<when>  <counts>  <id>`, then what is known of the
/// conversation (its agent, the parent of an orphaned subagent, a backlog),
/// and the share state last, so that a script reading the last bracket of the
/// line still reads the share state.
fn render_entry(summary: &ConversationSummary, with_destination: bool, marks: &Marks) -> String {
    let when = |seconds: i64| {
        Utc.timestamp_opt(seconds, 0)
            .single()
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "-".into())
    };
    let state = if !with_destination {
        String::new()
    } else {
        format!("  {}", share_state(summary))
    };
    let agent = agent_words(summary)
        .map(|words| format!("  {words}"))
        .unwrap_or_default();
    let orphan = marks
        .orphan_of
        .map(|parent| format!("  subagent of {}", sanitize_for_terminal(parent)))
        .unwrap_or_default();
    let backlog = if marks.backlog { BACKLOG_MARK } else { "" };
    let indent = if marks.nested { NESTED_INDENT } else { "" };
    format!(
        "{indent}{}  {:>3} thought(s), {:>3} linked  {}{agent}{orphan}{backlog}{state}\n{indent}      {}",
        when(summary.last_activity),
        summary.thoughts,
        summary.linked,
        sanitize_for_terminal(&summary.id),
        sanitize_for_terminal(&summary.title),
    )
}

/// How far the plain listing indents a subagent listed under its parent.
const NESTED_INDENT: &str = "    ";

/// What ends the first line of a conversation whose session has a backlog.
const BACKLOG_MARK: &str = "  [backlog]";

/// What [`read_backlog`] found.
#[derive(Clone, Debug, Default)]
pub(crate) struct BacklogRead {
    /// The family roots with a backlog, by conversation id, with their harness.
    pub sessions: BTreeMap<String, HarnessId>,
    /// The harnesses whose cursors could not be read, with why (text from the
    /// store's error, to be sanitized before it is shown). Their sessions are
    /// not marked; the others' still are.
    pub unreadable: Vec<(HarnessId, String)>,
}

/// The family roots that a hook run left unread to the end and that no run has
/// read to the end since (`harness::engine::backlog`), by conversation id, for
/// every harness this build can capture. Each harness is read on its own, so
/// one whose cursors cannot be read is reported and skipped and the others are
/// still marked. Read-only: it reads the stored cursors and writes nothing.
pub(crate) fn read_backlog(store: &CvcStore) -> BacklogRead {
    struct Read<'a>(&'a CvcStore);
    impl AdapterVisitor for Read<'_> {
        type Output = Result<Vec<Backlog>>;
        fn visit<A: HarnessAdapter>(self, _: &A) -> Self::Output {
            Ok(engine::backlog::<A>(self.0)?)
        }
    }
    let mut read = BacklogRead::default();
    for harness in HarnessId::ALL {
        match with_adapter(harness, Read(store)) {
            None => {}
            Some(Ok(sessions)) => {
                for session in sessions {
                    read.sessions.insert(session.conversation_id, harness);
                }
            }
            Some(Err(error)) => read.unreadable.push((harness, format!("{error:#}"))),
        }
    }
    read
}

/// The one line that says a harness's backlog could not be read.
pub(crate) fn backlog_unreadable_words(harness: HarnessId, reason: &str) -> String {
    format!(
        "cannot read which {} sessions a hook left unread, so none is marked: {}",
        harness.display(),
        sanitize_for_terminal(reason.lines().next().unwrap_or("").trim())
    )
}

/// The command that names the catch-up command for a harness's backlog.
pub(crate) fn backlog_hint(harness: HarnessId) -> String {
    format!("cvc harness status {}", harness.name())
}

/// The plain listing's lines, one entry (two lines) per conversation, most
/// recent group first, each subagent indented under its parent (see
/// [`Tree`]), then what a `[backlog]` mark means when one is shown.
fn listing_lines(
    summaries: &[ConversationSummary],
    with_destination: bool,
    backlog: &BTreeMap<String, HarnessId>,
) -> Vec<String> {
    let tree = Tree::build_with_empty(summaries);
    let mut lines = Vec::new();
    let mut behind = BTreeSet::new();
    for index in tree.visible(|_| true) {
        let entry = &tree.entries[index];
        let summary = &entry.summary;
        let harness = backlog.get(&summary.id).copied();
        behind.extend(harness);
        let marks = Marks {
            nested: matches!(entry.kind, Kind::Child { .. }),
            orphan_of: entry.unlisted_parent(),
            backlog: harness.is_some(),
        };
        lines.push(render_entry(summary, with_destination, &marks));
    }
    if !behind.is_empty() {
        let hints: Vec<String> = behind
            .iter()
            .map(|harness| format!("`{}`", backlog_hint(*harness)))
            .collect();
        lines.push(format!(
            "\n[backlog]: a hook run left the session unread to the end, and no run has read it to the end since. For the catch-up command, run {}.",
            hints.join(" or ")
        ));
    }
    lines
}

/// Moves, for every remote of the repository, the records an earlier version
/// kept under its destination's legacy fingerprint to its fingerprint
/// (`sync::adopt_legacy_records`), before anything is listed, so that the share
/// state, the publication counts and the authorization the listing and the
/// interactive view show read as they did. This is the one write of
/// `cvc conversations`, and it happens at most once per destination: a remote
/// without user information in its push URL has nothing to move, and nothing is
/// written for it.
///
/// Best effort, like the listing: a remote that cannot be resolved, or whose
/// records cannot be moved now, is listed as it stands, and the next command
/// that acts on it moves them or says why it cannot.
fn adopt_legacy_records_of_every_remote(repo: &Repository, store: &CvcStore) {
    let Ok(names) = repo.remotes() else {
        return;
    };
    for name in names.iter().flatten() {
        if let Ok(destination) = privacy::remote_destination(repo, name) {
            let _ = cvc_core::sync::adopt_legacy_records(repo, store, &destination);
        }
    }
}

/// Resolve the destination fingerprint for display, tolerating repositories
/// with no usable remote: the listing is read-only and must still work there.
pub(crate) fn display_destination(
    repo: &Repository,
    requested: Option<&str>,
) -> Option<(String, String)> {
    let name = requested.unwrap_or("origin").to_owned();
    privacy::remote_destination(repo, &name)
        .ok()
        .map(|destination| (name, destination.fingerprint))
}

/// Whether `cvc conversations` may start its interactive view instead of
/// printing the plain listing: not when `--plain` was given, only when stdin
/// and stdout are both terminals, and only when `TERM` names a terminal that
/// can redraw (set, and not `dumb`).
///
/// The terminal check is the very one (`tty::stdio_is_terminal`) that lets a
/// consent challenge be asked at all, so a script, a pipe or an IDE tool never
/// gets the interactive view and the view can never be a way around a
/// challenge.
pub(crate) fn interactive_ui_allowed(plain: bool) -> bool {
    !plain
        && crate::tty::stdio_is_terminal()
        && terminal_type_allows_ui(std::env::var_os("TERM").as_deref())
}

/// A full-screen view needs a terminal that can redraw. `TERM` is the Unix
/// convention for saying so: unset, empty or `dumb` means it cannot. Windows
/// consoles do not set `TERM`, so only an explicit `dumb` rules them out.
fn terminal_type_allows_ui(term: Option<&std::ffi::OsStr>) -> bool {
    match term {
        Some(term) => !term.is_empty() && !term.eq_ignore_ascii_case("dumb"),
        None => !cfg!(unix),
    }
}

/// Whether the interactive view may run the actions that end in a typed
/// challenge: share, unshare, publish and authorize sharing. Only where
/// `tty.rs` can keep a challenge from being answered by type-ahead, which is
/// Unix for v0.7.1: elsewhere its fallback reads the shared buffered stdin and
/// cannot discard what was queued before the prompt, so the view there only
/// browses and points at the matching `cvc` command instead of running it.
pub(crate) const fn consent_actions_allowed() -> bool {
    crate::tty::protects_challenges_from_type_ahead()
}

/// `cvc conversations`. `limit` is `--limit` as given: the plain listing falls
/// back to [`PLAIN_DEFAULT_LIMIT`], the interactive view to
/// `sync::SUMMARY_SCAN_LIMIT`. `plain` is `--plain`.
pub async fn run(requested: Option<&str>, limit: Option<usize>, plain: bool) -> Result<()> {
    let current_dir = env::current_dir()?;
    let layout =
        RepositoryLayout::discover(&current_dir).context("Failed to discover Git repository")?;
    layout.worktree_root()?;
    if !layout.cvc_dir().exists() {
        println!("CVC is not initialized in this repository. Run 'cvc init' to set up.");
        return Ok(());
    }
    let store = CvcStore::open_initialized(layout.db_path())?;
    let interactive = interactive_ui_allowed(plain);
    // The view's hint for an empty list asks whether this checkout records
    // Claude Code sessions, which needs the layout that becomes a bare
    // repository below. Only the view asks.
    let harness_installed = interactive && claude_code::settings::installed(&layout);
    let repo = layout.into_repository();
    adopt_legacy_records_of_every_remote(&repo, &store);

    // The interactive view, from a terminal and unless `--plain` was given. It
    // takes over the whole command, except when the terminal is too small for
    // it: it then says so in one line and the plain listing below follows.
    if interactive {
        let started = crate::tui::run(crate::tui::Start {
            repo: &repo,
            store: &store,
            requested,
            limit: limit.unwrap_or(SUMMARY_SCAN_LIMIT),
            harness_installed,
        })?;
        match started {
            crate::tui::Launch::Finished => return Ok(()),
            crate::tui::Launch::PlainListing => {}
        }
    }

    let destination = display_destination(&repo, requested);
    let summaries = store.list_conversation_summaries(
        destination
            .as_ref()
            .map(|(_, fingerprint)| fingerprint.as_str()),
        limit.unwrap_or(PLAIN_DEFAULT_LIMIT),
    )?;
    if summaries.is_empty() {
        println!("No conversations captured yet.");
        return Ok(());
    }
    match &destination {
        Some((name, fingerprint)) => println!(
            "Conversations (most recent first); share state for remote '{}' ({}…):",
            sanitize_for_terminal(name),
            &fingerprint[..12]
        ),
        None => println!("Conversations (most recent first); no remote resolved for share state:"),
    }
    // Best effort, like the share state: a harness whose cursors cannot be
    // read is named, and its sessions are listed unmarked.
    let backlog = read_backlog(&store);
    for (harness, reason) in &backlog.unreadable {
        eprintln!("warning: {}", backlog_unreadable_words(*harness, reason));
    }
    for line in listing_lines(&summaries, destination.is_some(), &backlog.sessions) {
        println!("{line}");
    }
    println!(
        "\nShare one with: cvc share <conversation-id> --remote <name>  (or run `cvc share` interactively)"
    );
    Ok(())
}

/// One remote of the repository as the interactive view shows it, in the
/// header and the remote picker. It is a read-only description and grants
/// nothing: consent is whatever `privacy.json` already records for the
/// destination fingerprint, and only the typed challenges change that.
///
/// It holds the fingerprint, not the destination itself, so no raw URL (which
/// may embed credentials) is kept around or printed by a stray `{:?}`; the only
/// trace of the URL is the sanitized `location`. To act on a remote, call
/// [`RemoteOverview::resolve`], which re-resolves it the way
/// `cvc share --remote <name>` would.
#[derive(Clone, Debug)]
pub(crate) struct RemoteOverview {
    /// The remote's name as configured.
    pub name: String,
    /// The destination fingerprint (the SHA-256 of the effective push URL
    /// without its user information, and of the ref, that consent is bound
    /// to), or the reason the remote cannot be a destination, such as
    /// `remote has no push URL`. The reason is plain text meant for display.
    pub fingerprint: std::result::Result<String, String>,
    /// Where the remote points, for display only: `host[:port]/path`,
    /// `host:path` or a local path, with user information (the user and any
    /// password or token), query and fragment removed. `None` when the remote
    /// is unusable or no safe form exists (see `privacy::display_location`).
    /// The raw URL is never a fallback. Do not resolve, compare or fetch
    /// through it; the fingerprint is the identity.
    pub location: Option<String>,
    /// Whether `cvc privacy acknowledge-sharing` was completed for exactly
    /// this fingerprint. `false` when `fingerprint` is an error or
    /// `consent_error` is set, so it never reads as consent that could not be
    /// established.
    pub sharing_consented: bool,
    /// Whether `cvc privacy set-auto-push on` was completed for exactly this
    /// fingerprint. Same fail-closed rule as `sharing_consented`.
    pub auto_push: bool,
    /// Why `sharing_consented` and `auto_push` could not be read (an unreadable
    /// or insecure `privacy.json`), when that is the case. Then both are
    /// `false`, but that is "unknown", not "declined".
    pub consent_error: Option<String>,
}

impl RemoteOverview {
    /// The destination to hand to the share, unshare, publish and
    /// acknowledge flows in `sync`, resolved now rather than when the
    /// overview was built, exactly as the subcommands resolve `--remote`.
    ///
    /// Fails closed, with nothing resolved, when the remote is unusable or its
    /// fingerprint is no longer the one this overview showed (its URL or push
    /// URL was edited while the view was open): the person decided on the
    /// fingerprint on screen, so an action must not silently move to another
    /// destination.
    pub(crate) fn resolve(&self, repo: &Repository) -> Result<privacy::RemoteDestination> {
        let shown = match &self.fingerprint {
            Ok(fingerprint) => fingerprint,
            Err(reason) => bail!("remote '{}' is unusable: {reason}", self.name),
        };
        let destination = privacy::remote_destination(repo, &self.name)
            .map_err(|error| anyhow!("remote '{}': {error}", self.name))?;
        if &destination.fingerprint != shown {
            bail!(
                "remote '{}' now resolves to a different destination than the one listed; refresh the remote list before acting",
                self.name
            );
        }
        Ok(destination)
    }
}

/// The repository's remotes in the order libgit2 lists them (the order
/// `git remote` prints), each with its destination fingerprint and consent
/// state. Read-only: nothing is written, fetched or resolved implicitly.
///
/// A remote that cannot be a destination, or whose consent state cannot be
/// read, is reported on its own entry and never fails the call; only being
/// unable to list the remotes at all is an error.
pub(crate) fn remote_overview(repo: &Repository) -> Result<Vec<RemoteOverview>> {
    let names = repo
        .remotes()
        .context("Failed to list the repository's remotes")?;
    Ok(names
        .iter_bytes()
        .map(|raw| match std::str::from_utf8(raw) {
            Ok(name) => overview_of(repo, name),
            // Not addressable by any command, but still worth showing.
            Err(_) => RemoteOverview {
                name: String::from_utf8_lossy(raw).into_owned(),
                fingerprint: Err("remote name is not valid UTF-8".into()),
                location: None,
                sharing_consented: false,
                auto_push: false,
                consent_error: None,
            },
        })
        .collect())
}

fn overview_of(repo: &Repository, name: &str) -> RemoteOverview {
    let destination = privacy::remote_destination(repo, name);
    let location = destination
        .as_ref()
        .ok()
        .and_then(|destination| privacy::display_location(&destination.effective_url));
    let fingerprint = destination
        .map(|destination| destination.fingerprint)
        .map_err(|error| error.to_string());
    let (sharing_consented, auto_push, consent_error) = match &fingerprint {
        Ok(fingerprint) => match privacy::privacy_status_for_fingerprint(repo, fingerprint) {
            Ok(status) => (status.sharing_consented, status.auto_push, None),
            Err(error) => (false, false, Some(error.to_string())),
        },
        Err(_) => (false, false, None),
    };
    RemoteOverview {
        name: name.to_owned(),
        fingerprint,
        location,
        sharing_consented,
        auto_push,
        consent_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repository_with(remotes: &[(&str, &str)]) -> (TempDir, Repository) {
        let directory = TempDir::new().unwrap();
        let repo = Repository::init(directory.path()).unwrap();
        for (name, url) in remotes {
            repo.remote(name, url).unwrap();
        }
        (directory, repo)
    }

    fn entry<'a>(overview: &'a [RemoteOverview], name: &str) -> &'a RemoteOverview {
        overview
            .iter()
            .find(|remote| remote.name == name)
            .unwrap_or_else(|| panic!("no remote {name} in {overview:?}"))
    }

    /// Remotes git lists (they have a `url` key) but that cannot be a
    /// destination: an empty URL, and one that is only whitespace.
    fn add_unusable_remotes(repo: &Repository) {
        let mut config = repo.config().unwrap();
        config.set_str("remote.emptyurl.url", "").unwrap();
        config.set_str("remote.blankurl.url", "   ").unwrap();
    }

    fn fingerprint_of(repo: &Repository, name: &str) -> String {
        privacy::remote_destination(repo, name).unwrap().fingerprint
    }

    /// A conversation of four thoughts with no share, publication or hold.
    fn summary() -> ConversationSummary {
        ConversationSummary {
            id: "c-1".into(),
            title: "a title".into(),
            thoughts: 4,
            linked: 1,
            first_activity: 1_700_000_000,
            last_activity: 1_700_000_000,
            shared: false,
            shared_count: 0,
            published: 0,
            unconfirmed: 0,
            share_future: None,
            imported: false,
            held: 0,
            held_published: 0,
            agent: None,
            parent: None,
        }
    }

    /// The same conversation with `shared` thoughts shared and `published` of
    /// them published.
    fn shared(shared: i64, published: i64) -> ConversationSummary {
        ConversationSummary {
            shared: shared > 0,
            shared_count: shared,
            published,
            held: published,
            held_published: published,
            ..summary()
        }
    }

    /// The same conversation, none of it shared, `published` thoughts of it held
    /// for certain by the remote and `unconfirmed` more perhaps.
    fn held(published: i64, unconfirmed: i64) -> ConversationSummary {
        ConversationSummary {
            held: published + unconfirmed,
            held_published: published,
            ..summary()
        }
    }

    #[test]
    fn what_is_shared_from_here_keeps_its_labels() {
        assert_eq!(share_state(&shared(4, 4)), "[shared, 4 published]");
        assert_eq!(share_state(&shared(4, 1)), "[shared, 1/4 published]");
        assert_eq!(share_state(&shared(2, 0)), "[shared, 0/4 published]");
        assert_eq!(share_state(&shared(2, 2)), "[shared, 2/4 published]");
        // What else the remote holds changes nothing while something is shared.
        let mut also_held = shared(2, 1);
        also_held.held = 4;
        also_held.held_published = 3;
        also_held.share_future = Some(true);
        assert_eq!(share_state(&also_held), "[shared, 1/4 published]");
    }

    #[test]
    fn what_the_remote_holds_is_never_listed_as_private() {
        assert_eq!(share_state(&summary()), "[private]");
        // Held for certain, whether or not other held thoughts are unconfirmed.
        assert_eq!(share_state(&held(4, 0)), "[published, not shared here]");
        assert_eq!(share_state(&held(1, 3)), "[published, not shared here]");
        // Held only by publishes that did not conclude: not called published.
        assert_eq!(share_state(&held(0, 2)), "[unconfirmed, not shared here]");
        // Later thoughts shared, nothing else: said so. Held outranks it.
        let mut future = summary();
        future.share_future = Some(true);
        assert_eq!(share_state(&future), "[shared, future only]");
        future.share_future = Some(false);
        assert_eq!(share_state(&future), "[private]");
        let mut both = held(1, 0);
        both.share_future = Some(true);
        assert_eq!(share_state(&both), "[published, not shared here]");

        // Every mix of held thoughts with nothing shared.
        for published in 0..=4 {
            for unconfirmed in 0..=(4 - published) {
                let label = share_state(&held(published, unconfirmed));
                let case = format!("{published} published, {unconfirmed} unconfirmed: {label}");
                assert_eq!(label == "[private]", published + unconfirmed == 0, "{case}");
                assert_eq!(label.contains("published"), published > 0, "{case}");
                assert_eq!(
                    label.contains("unconfirmed"),
                    published == 0 && unconfirmed > 0,
                    "{case}"
                );
            }
        }
    }

    #[test]
    fn a_line_carries_the_state_only_with_a_destination() {
        let line = render_line(&held(1, 0), true);
        assert!(
            line.contains("c-1  [published, not shared here]\n"),
            "{line}"
        );
        let line = render_line(&held(1, 0), false);
        assert!(!line.contains('['), "{line}");
        assert!(line.contains("c-1\n      a title"), "{line}");
    }

    /// Whether `text` holds a character that acts on a terminal. A line feed is
    /// how a line ends, so it is the one thing allowed.
    fn acts_on_a_terminal(text: &str) -> bool {
        text.chars()
            .any(|c| c != '\n' && crate::terminal_text::is_unsafe_for_terminal(c))
    }

    #[test]
    fn a_title_or_an_id_cannot_act_on_the_terminal_or_redraw_a_line() {
        let mut hostile = summary();
        hostile.id = "c\u{1b}[2J-1".into();
        hostile.title =
            "Fix it\u{1b}]0;pwned\u{7}\u{202e}\rSharing conversation for remote 'x':\nI SHARE c-1"
                .into();
        let line = render_line(&hostile, true);
        assert!(!acts_on_a_terminal(&line), "{line:?}");
        // One entry stays two lines, whatever the title holds.
        assert_eq!(line.matches('\n').count(), 1, "{line:?}");
        assert!(line.contains("  c?[2J-1  [private]\n"), "{line:?}");
        assert!(
            line.ends_with(
                "      Fix it?]0;pwned???Sharing conversation for remote 'x':?I SHARE c-1"
            ),
            "{line:?}"
        );
        // Without a destination too.
        assert!(!acts_on_a_terminal(&render_line(&hostile, false)));
    }

    #[test]
    fn text_without_control_or_format_characters_is_printed_as_it_is() {
        let mut ordinary = summary();
        ordinary.id = "run-0d2a:agent_1.b".into();
        ordinary.title = "שלום – café – 日本語 ✓ emoji 🙂 \"quoted\" <tag> & more".into();
        let line = render_line(&ordinary, true);
        assert_eq!(
            line,
            format!(
                "2023-11-14 22:13    4 thought(s),   1 linked  run-0d2a:agent_1.b  [private]\n      {}",
                ordinary.title
            )
        );
    }

    #[test]
    fn a_line_names_the_agent_only_when_one_recorded_it_all() {
        // Unknown or mixed: nothing.
        assert!(!render_line(&summary(), true).contains("recorded by"));
        let mut codex = summary();
        codex.agent = Some(HarnessId::Codex);
        let line = render_line(&codex, true);
        assert!(
            line.contains("c-1  recorded by Codex  [private]\n"),
            "{line}"
        );
        // Without a destination too, and still without a bracket.
        let line = render_line(&codex, false);
        assert!(line.contains("c-1  recorded by Codex\n"), "{line}");
        assert!(!line.contains('['), "{line}");
        // Imported: its state as before, and the agent as the id's claim.
        let mut imported = held(1, 0);
        imported.imported = true;
        imported.agent = Some(HarnessId::OpenCode);
        let line = render_line(&imported, true);
        assert!(
            line.contains(
                "c-1  recorded by OpenCode (as its imported id says)  [published, not shared here]\n"
            ),
            "{line}"
        );
    }

    fn conversation(id: &str, last: i64) -> ConversationSummary {
        ConversationSummary {
            id: id.into(),
            title: format!("title of {id}"),
            first_activity: last,
            last_activity: last,
            ..summary()
        }
    }

    #[test]
    fn the_listing_nests_subagents_and_marks_and_explains_a_backlog() {
        let mut codex_child = conversation("codex:t-child", 1_700_000_300);
        codex_child.parent = Some("codex:t-root".into());
        codex_child.agent = Some(HarnessId::Codex);
        let mut codex_root = conversation("codex:t-root", 1_700_000_100);
        codex_root.agent = Some(HarnessId::Codex);
        let mut orphan = conversation("codex:t-lost", 1_700_000_050);
        orphan.parent = Some("codex:t-gone".into());
        let mut lost_parent = conversation("codex:t-lost-parent", 1_700_000_040);
        lost_parent.parent = Some("codex:t-gone-2".into());
        let mut lost_kid = conversation("codex:t-lost-kid", 1_700_000_030);
        lost_kid.parent = Some("codex:t-lost-parent".into());
        let summaries = vec![
            codex_child,
            conversation("s:agent-a", 1_700_000_200),
            codex_root,
            conversation("s", 1_700_000_000),
            orphan,
            lost_parent,
            lost_kid,
            // No thoughts: the plain listing still lists it.
            ConversationSummary {
                thoughts: 0,
                ..conversation("empty", 1_600_000_000)
            },
        ];
        let backlog = BTreeMap::from([("s".to_owned(), HarnessId::ClaudeCode)]);
        let lines = listing_lines(&summaries, true, &backlog);
        let firsts: Vec<&str> = lines
            .iter()
            .filter_map(|entry| entry.lines().next())
            .collect();
        let at = |id: &str| {
            firsts
                .iter()
                .position(|line| line.contains(&format!(" {id}  ")))
                .unwrap_or_else(|| panic!("{id} not listed: {lines:#?}"))
        };
        // Groups by their newest member, each child right after its parent.
        assert!(at("codex:t-root") < at("codex:t-child"));
        assert_eq!(at("codex:t-child"), at("codex:t-root") + 1);
        assert!(at("codex:t-child") < at("s"));
        assert_eq!(at("s:agent-a"), at("s") + 1);
        assert!(firsts[at("codex:t-child")].starts_with("    2023-"));
        assert!(firsts[at("s:agent-a")].starts_with("    2023-"));
        assert!(lines[at("s:agent-a")].ends_with("\n          title of s:agent-a"));
        assert!(firsts[at("s")].starts_with("2023-"));
        // An orphan says whose subagent it is.
        assert!(
            firsts[at("codex:t-lost")]
                .ends_with("codex:t-lost  subagent of codex:t-gone  [private]"),
            "{lines:#?}"
        );
        // So does one with agents of its own, which are listed under it.
        assert!(
            firsts[at("codex:t-lost-parent")]
                .ends_with("codex:t-lost-parent  subagent of codex:t-gone-2  [private]"),
            "{lines:#?}"
        );
        assert_eq!(at("codex:t-lost-kid"), at("codex:t-lost-parent") + 1);
        assert!(firsts[at("codex:t-lost-kid")].starts_with("    2023-"));
        assert!(!firsts[at("codex:t-lost-kid")].contains("subagent of"));
        assert!(at("empty") > at("codex:t-lost"));
        // The backlog mark, once, and the hint for its harness.
        assert!(
            firsts[at("s")].ends_with("  [backlog]  [private]"),
            "{lines:#?}"
        );
        assert_eq!(firsts.iter().filter(|l| l.contains("[backlog]")).count(), 1);
        let hint = lines.last().unwrap();
        assert!(
            hint.contains("run `cvc harness status claude-code`."),
            "{hint}"
        );
        // Without a backlog, no hint.
        let lines = listing_lines(&summaries, true, &BTreeMap::new());
        assert!(!lines.iter().any(|line| line.contains("backlog")));
    }

    #[test]
    fn a_parent_id_from_the_store_cannot_act_on_the_terminal() {
        let mut orphan = summary();
        orphan.parent = Some("p\u{1b}[2J".into());
        let lines = listing_lines(&[orphan], true, &BTreeMap::new());
        assert!(!acts_on_a_terminal(&lines[0]), "{:?}", lines[0]);
        assert!(
            lines[0].contains("subagent of p?[2J  [private]\n"),
            "{:?}",
            lines[0]
        );
    }

    /// The backlog is read from the stored cursors, for every harness, and
    /// reading it writes nothing.
    #[test]
    fn the_backlog_is_read_from_the_cursors_without_writing() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("index.db");
        let store = CvcStore::open(&path).unwrap();
        let empty = read_backlog(&store);
        assert!(empty.sessions.is_empty() && empty.unreadable.is_empty());
        let raw = rusqlite::Connection::open(&path).unwrap();
        let cursor = |key: &str, backlog: bool| {
            raw.execute(
                "INSERT INTO harness_cursors(harness,cursor_key,kind,version,epoch,state,updated_at) VALUES('claude-code',?1,'jsonl',1,0,?2,1)",
                rusqlite::params![
                    key,
                    serde_json::json!({
                        "byte_offset": 10,
                        "path": "/transcripts/x.jsonl",
                        "carry": { "backlog": backlog },
                    })
                    .to_string()
                ],
            )
            .unwrap();
        };
        cursor("behind", true);
        cursor("caught-up", false);
        let changes = || -> i64 {
            raw.query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap()
        };
        let before = (changes(), store_dump(&raw));
        let read = read_backlog(&store);
        assert_eq!(
            read.sessions,
            BTreeMap::from([("behind".to_owned(), HarnessId::ClaudeCode)])
        );
        assert!(read.unreadable.is_empty());
        assert_eq!((changes(), store_dump(&raw)), before);
    }

    /// A harness whose cursors cannot be read is reported, not fatal, and its
    /// reason reaches the terminal sanitized.
    #[test]
    fn an_unreadable_backlog_is_reported_per_harness() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("index.db");
        let store = CvcStore::open(&path).unwrap();
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TABLE harness_cursors")
            .unwrap();
        let read = read_backlog(&store);
        assert!(read.sessions.is_empty());
        let harnesses: Vec<HarnessId> = read.unreadable.iter().map(|(h, _)| *h).collect();
        assert_eq!(harnesses, vec![HarnessId::ClaudeCode]);
        let words = backlog_unreadable_words(HarnessId::ClaudeCode, "bad\u{1b}[2J\nsecond line");
        assert_eq!(
            words,
            "cannot read which Claude Code sessions a hook left unread, so none is marked: bad?[2J"
        );
    }

    fn store_dump(raw: &rusqlite::Connection) -> Vec<String> {
        let mut statement = raw
            .prepare("SELECT harness||'|'||cursor_key||'|'||state FROM harness_cursors ORDER BY 1")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<String>, _>>()
            .unwrap();
        rows
    }

    #[test]
    fn plain_never_allows_the_interactive_view() {
        assert!(!interactive_ui_allowed(true));
    }

    #[test]
    fn the_terminal_type_must_be_able_to_redraw() {
        use std::ffi::OsStr;
        let allows = |term: &str| terminal_type_allows_ui(Some(OsStr::new(term)));
        assert!(allows("xterm-256color"));
        assert!(allows("linux"));
        assert!(allows("screen"));
        assert!(!allows("dumb"));
        assert!(!allows("DUMB"));
        assert!(!allows(""));
        // Unset: no terminal type on Unix, where one is always set for a
        // terminal; Windows consoles never set it.
        assert_eq!(terminal_type_allows_ui(None), cfg!(not(unix)));
    }

    #[test]
    fn consent_actions_need_type_ahead_protection() {
        assert_eq!(consent_actions_allowed(), cfg!(unix));
        assert_eq!(
            consent_actions_allowed(),
            crate::tty::protects_challenges_from_type_ahead()
        );
    }

    #[test]
    fn remotes_are_listed_in_git_order_with_their_fingerprints() {
        let (_directory, repo) = repository_with(&[
            ("zeta", "https://example.invalid/z.git"),
            ("origin", "https://example.invalid/o.git"),
            ("alpha", "https://example.invalid/a.git"),
        ]);
        let overview = remote_overview(&repo).unwrap();
        // `git remote` prints these sorted, whatever order they were added in.
        let names: Vec<_> = overview.iter().map(|remote| remote.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "origin", "zeta"]);
        let git_order = repo.remotes().unwrap();
        let listed: Vec<_> = git_order.iter().flatten().collect();
        assert_eq!(names, listed);
        for remote in &overview {
            assert_eq!(
                remote.fingerprint.as_deref().unwrap(),
                fingerprint_of(&repo, &remote.name)
            );
            assert!(!remote.sharing_consented && !remote.auto_push);
            assert_eq!(remote.consent_error, None);
        }
    }

    #[test]
    fn the_location_is_sanitized_and_no_raw_url_is_kept() {
        let (_directory, repo) = repository_with(&[
            (
                "alpha",
                "https://user:tok-3xample@example.invalid/o/a.git?x=1#f",
            ),
            ("beta", "git@example.invalid:o/b.git"),
            (
                "gamma",
                "ssh://deploy:hunter-22@example.invalid:2222/o/c.git",
            ),
            ("delta", "/srv/repos/d.git"),
        ]);
        repo.remote_set_pushurl("gamma", Some("ssh://tok-9yz@push.example.invalid/o/c.git"))
            .unwrap();
        add_unusable_remotes(&repo);
        let overview = remote_overview(&repo).unwrap();
        let location = |name: &str| entry(&overview, name).location.as_deref();
        assert_eq!(location("alpha"), Some("example.invalid/o/a.git"));
        assert_eq!(location("beta"), Some("example.invalid:o/b.git"));
        // The effective push URL is what is described, not the fetch URL.
        assert_eq!(location("gamma"), Some("push.example.invalid/o/c.git"));
        assert_eq!(location("delta"), Some("/srv/repos/d.git"));
        assert_eq!(location("emptyurl"), None);
        assert_eq!(location("blankurl"), None);
        // Nothing of any credential survives, not even in `Debug`.
        let debug = format!("{overview:?}");
        for secret in ["tok-3xample", "hunter-22", "tok-9yz", "user:", "deploy"] {
            assert!(!debug.contains(secret), "{secret} leaked: {debug}");
        }
    }

    #[test]
    fn a_repository_without_remotes_has_an_empty_overview() {
        let (_directory, repo) = repository_with(&[]);
        assert!(remote_overview(&repo).unwrap().is_empty());
    }

    #[test]
    fn an_unusable_remote_is_reported_alone_and_does_not_fail_the_overview() {
        let (_directory, repo) = repository_with(&[("origin", "https://example.invalid/o.git")]);
        add_unusable_remotes(&repo);
        let overview = remote_overview(&repo).unwrap();
        assert_eq!(overview.len(), 3);

        for name in ["emptyurl", "blankurl"] {
            let broken = entry(&overview, name);
            let reason = broken.fingerprint.as_ref().unwrap_err();
            assert!(reason.contains("empty push URL"), "{name}: {reason}");
            assert!(
                !reason.contains(".thoughtignore"),
                "the policy prefix does not belong on a remote problem: {reason}"
            );
            assert!(!broken.sharing_consented && !broken.auto_push);
            assert_eq!(broken.consent_error, None);
        }

        let healthy = entry(&overview, "origin");
        assert_eq!(
            healthy.fingerprint.as_deref().unwrap(),
            fingerprint_of(&repo, "origin")
        );
    }

    #[test]
    fn consent_is_reported_per_destination_fingerprint() {
        let (_directory, repo) = repository_with(&[
            ("alpha", "https://example.invalid/a.git"),
            ("origin", "https://example.invalid/o.git"),
        ]);
        let alpha = privacy::remote_destination(&repo, "alpha").unwrap();
        privacy::acknowledge_sharing_destination(&repo, &alpha).unwrap();

        let overview = remote_overview(&repo).unwrap();
        assert!(entry(&overview, "alpha").sharing_consented);
        assert!(!entry(&overview, "alpha").auto_push);
        assert!(!entry(&overview, "origin").sharing_consented);

        privacy::set_auto_push_destination(&repo, &alpha, true).unwrap();
        let overview = remote_overview(&repo).unwrap();
        assert!(entry(&overview, "alpha").sharing_consented);
        assert!(entry(&overview, "alpha").auto_push);
        assert!(!entry(&overview, "origin").auto_push);
    }

    #[test]
    fn consent_for_one_url_grants_nothing_for_another() {
        let (_directory, repo) = repository_with(&[("origin", "https://example.invalid/o.git")]);
        let before = privacy::remote_destination(&repo, "origin").unwrap();
        privacy::acknowledge_sharing_destination(&repo, &before).unwrap();
        assert!(remote_overview(&repo).unwrap()[0].sharing_consented);

        // The effective push URL is what consent is bound to.
        repo.remote_set_pushurl("origin", Some("https://example.invalid/elsewhere.git"))
            .unwrap();
        let overview = remote_overview(&repo).unwrap();
        let origin = entry(&overview, "origin");
        assert_ne!(origin.fingerprint.as_deref().unwrap(), before.fingerprint);
        assert!(!origin.sharing_consented);
        assert!(!origin.auto_push);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_consent_state_is_unknown_not_declined_and_not_fatal() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, repo) = repository_with(&[
            ("alpha", "https://example.invalid/a.git"),
            ("origin", "https://example.invalid/o.git"),
        ]);
        let alpha = privacy::remote_destination(&repo, "alpha").unwrap();
        privacy::acknowledge_sharing_destination(&repo, &alpha).unwrap();
        let state = repo.path().join("cvc").join("privacy.json");
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o644)).unwrap();

        let overview = remote_overview(&repo).unwrap();
        assert_eq!(overview.len(), 2);
        for remote in &overview {
            assert!(remote.fingerprint.is_ok());
            assert!(!remote.sharing_consented && !remote.auto_push);
            let reason = remote.consent_error.as_deref().unwrap();
            assert!(reason.contains("insecure"), "{reason}");
            assert!(!reason.contains(".thoughtignore"), "{reason}");
        }
    }

    #[test]
    fn resolve_returns_the_destination_the_overview_showed() {
        let (_directory, repo) = repository_with(&[("origin", "https://example.invalid/o.git")]);
        let overview = remote_overview(&repo).unwrap();
        let destination = overview[0].resolve(&repo).unwrap();
        assert_eq!(destination.name, "origin");
        assert_eq!(
            &destination.fingerprint,
            overview[0].fingerprint.as_ref().unwrap()
        );
    }

    #[test]
    fn resolve_refuses_an_unusable_remote_and_a_remote_that_moved() {
        let (_directory, repo) = repository_with(&[("origin", "https://example.invalid/o.git")]);
        add_unusable_remotes(&repo);
        let overview = remote_overview(&repo).unwrap();
        let error = entry(&overview, "emptyurl").resolve(&repo).unwrap_err();
        assert!(error.to_string().contains("unusable"), "{error}");

        repo.remote_set_pushurl("origin", Some("https://example.invalid/moved.git"))
            .unwrap();
        let error = entry(&overview, "origin").resolve(&repo).unwrap_err();
        assert!(
            error.to_string().contains("different destination"),
            "{error}"
        );

        // A remote deleted since the listing is refused too.
        repo.remote_delete("origin").unwrap();
        let error = entry(&overview, "origin").resolve(&repo).unwrap_err();
        assert!(error.to_string().contains("not configured"), "{error}");
        assert!(!error.to_string().contains(".thoughtignore"), "{error}");
    }
}
