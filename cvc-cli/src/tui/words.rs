//! Everything the interactive view says, as pure functions of plain data, so
//! the wording can be reviewed and tested without a terminal.
//!
//! The vocabulary is fixed: the view says "thought" (with real plurals), never
//! "turn" or "interaction", and the CLI's own output, which the view shows on
//! the suspended main screen, says "thought(s)" as well. There are no
//! contractions, sentences end with a period, header labels are lowercase and
//! a remote's name is in single quotes inside a sentence and bare after
//! `remote:`. Times are UTC. Every string built from the store or from Git goes
//! through `text::sanitize` before it gets here or is put in a sentence here.

use chrono::{TimeZone, Utc};
use cvc_core::db::{ConversationDetails, ConversationSummary, UnshareCheck};
use cvc_core::models::FutureSharePolicy;

use super::text::{self, Glyphs};
use crate::commands::conversations::RemoteOverview;
use crate::commands::failure::FlowFailure;
use crate::commands::sync::PublishOutcome;

/// How a piece of text is emphasised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emph {
    Plain,
    /// A key name, or a heading.
    Strong,
    Dim,
    /// The one loud thing on the screen: auto-push is on.
    Loud,
}

/// A run of text with one emphasis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seg {
    pub text: String,
    pub emph: Emph,
}

impl Seg {
    pub fn new(text: impl Into<String>, emph: Emph) -> Self {
        Self {
            text: text.into(),
            emph,
        }
    }
    pub fn plain(text: impl Into<String>) -> Self {
        Self::new(text, Emph::Plain)
    }
    pub fn strong(text: impl Into<String>) -> Self {
        Self::new(text, Emph::Strong)
    }
    pub fn dim(text: impl Into<String>) -> Self {
        Self::new(text, Emph::Dim)
    }
    pub fn loud(text: impl Into<String>) -> Self {
        Self::new(text, Emph::Loud)
    }
}

/// The text of a line of segments.
#[cfg(test)]
pub fn plain_text(line: &[Seg]) -> String {
    line.iter().map(|seg| seg.text.as_str()).collect()
}

/// `1 thought`, `27 thoughts`.
pub fn thoughts(n: i64) -> String {
    count(n, "thought", "thoughts")
}

/// `1 shared thought`, `27 shared thoughts`.
fn shared_thoughts(n: i64) -> String {
    count(n, "shared thought", "shared thoughts")
}

/// `n` and the matching form of the noun.
pub fn count(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// How a state label is colored; the glyph and the word carry the meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Private,
    Waiting,
    Published,
}

/// What a state label is derived from, as plain numbers. Everything the label
/// depends on is here, so one more input slots in without touching the rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LabelInput {
    /// Thoughts in the conversation (T).
    pub thoughts: i64,
    /// Thoughts shared for the selected remote (S).
    pub shared: i64,
    /// Of the shared thoughts, those the remote is known to hold (P).
    pub published: i64,
    /// Of the shared thoughts, those whose publication is uncertain: a push that
    /// began without concluding, or failed ambiguously (U).
    pub unconfirmed: i64,
    /// Thoughts the remote holds or may hold (published, pending or unknown),
    /// whether or not they are shared from here (H). A row is never labeled
    /// `private` while this is above zero.
    pub held: i64,
    /// Of the held thoughts, those the remote is known to hold: published. Only
    /// a row with one may be called `published` while nothing is shared from
    /// here.
    pub held_published: i64,
    /// Later thoughts are shared as they are captured.
    pub future: bool,
    /// Every thought of the conversation was imported from a remote.
    pub imported: bool,
}

/// Where a conversation stands with the selected remote, before it is put in
/// words. The label and the badge that counts agents both read it, so the two
/// cannot disagree about a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    /// Nothing is shared and the remote holds nothing of it.
    Private,
    /// Nothing is shared and the remote holds nothing of it yet, but later
    /// thoughts are shared as they are captured.
    FutureOnly,
    /// Nothing is shared from here and the remote is known to hold some of it.
    HeldPublished,
    /// Nothing is shared from here and the remote may hold some of it, but no
    /// publish of it is known to have concluded.
    HeldUnconfirmed,
    /// Something is shared and some of it is not yet published.
    Waiting,
    /// Everything shared is published.
    Published,
}

impl Standing {
    fn tone(self) -> Tone {
        match self {
            Standing::Private => Tone::Private,
            Standing::FutureOnly | Standing::HeldUnconfirmed | Standing::Waiting => Tone::Waiting,
            Standing::HeldPublished | Standing::Published => Tone::Published,
        }
    }
}

impl LabelInput {
    /// The input of a listed conversation. The one place that says where each
    /// number comes from.
    pub fn of(summary: &ConversationSummary) -> Self {
        Self {
            thoughts: summary.thoughts,
            shared: summary.shared_count,
            published: summary.published,
            unconfirmed: summary.unconfirmed,
            held: summary.held,
            held_published: summary.held_published,
            future: summary.share_future == Some(true),
            imported: summary.imported,
        }
    }

    /// Shared thoughts the remote does not hold yet (W).
    fn waiting(&self) -> i64 {
        self.shared - self.published
    }

    /// Thoughts the remote holds or may hold that are not shared from here (N):
    /// the held ones less those of them that are shared, which are the
    /// published and the unconfirmed ones. Beside a share this is what the
    /// `not shared here` tail counts; with nothing shared it is all of H.
    fn held_unshared(&self) -> i64 {
        // Never more than the thoughts that are not shared, whatever the numbers
        // say, so that shared, held and private always add up to the thoughts.
        (self.held - self.published - self.unconfirmed)
            .clamp(0, (self.thoughts - self.shared).max(0))
    }

    /// Thoughts that are neither shared nor held by the remote: what `private`
    /// counts, and nothing else (T - S - N).
    fn private_thoughts(&self) -> i64 {
        (self.thoughts - self.shared - self.held_unshared()).max(0)
    }

    fn standing(&self) -> Standing {
        if self.shared > 0 {
            if self.waiting() > 0 {
                Standing::Waiting
            } else {
                Standing::Published
            }
        } else if self.held > 0 {
            if self.held_published > 0 {
                Standing::HeldPublished
            } else {
                Standing::HeldUnconfirmed
            }
        } else if self.future {
            Standing::FutureOnly
        } else {
            Standing::Private
        }
    }

    /// Which part of the story a row tells a badge counting agents. A row that
    /// is only later thoughts shared tells none: nothing waits, nothing is
    /// private to decide about, and nothing is published.
    fn category(&self) -> Option<Tone> {
        if self.imported && self.shared == 0 {
            return None;
        }
        match self.standing() {
            Standing::FutureOnly => None,
            standing => Some(standing.tone()),
        }
    }
}

/// The state column for one conversation and the remote it is shown for.
///
/// With S shared, P published among those, H held (published, pending or
/// unknown, shared or not) and T thoughts in all, W = S - P:
///
/// - S = 0 and H = 0 reads `private`. A conversation whose later thoughts are
///   shared as they are captured reads `shared · future only` instead.
/// - Anything shared but not yet published reads `shared` with W `waiting`;
///   everything shared and published reads `published`. Whenever any are
///   shared, the thoughts that are not shared are counted in tails, apart:
///   those the remote holds anyway as `N not shared here`, and those it holds
///   none of as `M private`. So `private` counts only what the remote does not
///   hold, and never stands alone for a conversation with a published thought.
/// - S = 0 and H > 0, which is what a re-ingested session reads (the remote
///   holds it, nobody here shared it), reads `published · not shared here` when
///   the remote is known to hold some of it, and `unconfirmed · not shared
///   here` when every held thought is a publish that did not conclude. Never
///   `private`, and never `published` without a published thought.
///
/// `+future` follows when later thoughts are shared as they are captured. A
/// conversation imported from a remote and not shared from here reads
/// `imported`. Without a destination there is no share state to show, which
/// reads as a dash.
pub fn state_label(
    summary: &ConversationSummary,
    has_destination: bool,
    g: &Glyphs,
) -> (String, Tone) {
    label_of(&LabelInput::of(summary), has_destination, g)
}

fn label_of(input: &LabelInput, has_destination: bool, g: &Glyphs) -> (String, Tone) {
    if input.imported && input.shared == 0 {
        return ("imported".to_owned(), Tone::Neutral);
    }
    if !has_destination {
        return (g.dash().to_owned(), Tone::Neutral);
    }
    let waiting = input.waiting();
    let dot = g.dot();
    let standing = input.standing();
    let mut label = match standing {
        Standing::Private => format!("{} private", g.private()),
        Standing::FutureOnly => format!("{} shared {dot} future only", g.waiting()),
        Standing::HeldPublished => {
            format!("{} published {dot} not shared here", g.published())
        }
        Standing::HeldUnconfirmed => {
            format!("{} unconfirmed {dot} not shared here", g.unconfirmed())
        }
        Standing::Waiting => format!("{} shared {dot} {waiting} waiting", g.waiting()),
        Standing::Published => format!("{} published", g.published()),
    };
    // Beside a share, the thoughts that are not shared are told apart: those the
    // remote holds anyway, and those it holds none of, which are the private
    // ones. `private` counts nothing the remote holds.
    if input.shared > 0 {
        let held_unshared = input.held_unshared();
        if held_unshared > 0 {
            label.push_str(&format!(" {dot} {held_unshared} not shared here"));
        }
        let private = input.private_thoughts();
        if private > 0 {
            label.push_str(&format!(" {dot} {private} private"));
        }
    }
    // `future only` already says it.
    if input.future && standing != Standing::FutureOnly {
        label.push_str(" +future");
    }
    (label, standing.tone())
}

/// The dim badge that ends a collapsed parent's row: how many agents are under
/// it and, when a remote is selected, the most pressing thing about them (those
/// waiting to publish, else those still private, else those published). Counts
/// are of agent conversations.
pub fn agents_badge(agents: &[&ConversationSummary], has_destination: bool, g: &Glyphs) -> String {
    let n = agents.len() as i64;
    let mut badge = format!("+{}", count(n, "agent", "agents"));
    if !has_destination {
        return badge;
    }
    let categories: Vec<Tone> = agents
        .iter()
        .filter_map(|agent| LabelInput::of(agent).category())
        .collect();
    let in_category = |tone: Tone| categories.iter().filter(|&&t| t == tone).count();
    let note = [
        (Tone::Waiting, "waiting"),
        (Tone::Private, "private"),
        (Tone::Published, "published"),
    ]
    .into_iter()
    .find(|&(tone, _)| in_category(tone) > 0)
    .map(|(tone, word)| format!("{} {word}", in_category(tone)));
    if let Some(note) = note {
        badge.push_str(&format!(" {} {note}", g.dot()));
    }
    badge
}

/// `YYYY-MM-DD HH:MM` (UTC), or `MM-DD HH:MM` when `short`; `-` for a time
/// that cannot be represented.
pub fn format_when(seconds: i64, short: bool) -> String {
    match Utc.timestamp_opt(seconds, 0).single() {
        Some(time) => time
            .format(if short {
                "%m-%d %H:%M"
            } else {
                "%Y-%m-%d %H:%M"
            })
            .to_string(),
        None => "-".to_owned(),
    }
}

/// The activity range of a conversation: one time when it began and ended in
/// the same minute, `HH:MM` for the end when it is the same day, `MM-DD HH:MM`
/// when it is the same year, else the full date.
pub fn activity(first: i64, last: i64, g: &Glyphs) -> String {
    let (Some(start), Some(end)) = (
        Utc.timestamp_opt(first, 0).single(),
        Utc.timestamp_opt(last, 0).single(),
    ) else {
        return g.dash().to_owned();
    };
    let start_text = start.format("%Y-%m-%d %H:%M").to_string();
    let end_text = if start_text == end.format("%Y-%m-%d %H:%M").to_string() {
        return format!("{start_text} UTC");
    } else if start.date_naive() == end.date_naive() {
        end.format("%H:%M").to_string()
    } else if start.format("%Y").to_string() == end.format("%Y").to_string() {
        end.format("%m-%d %H:%M").to_string()
    } else {
        end.format("%Y-%m-%d %H:%M").to_string()
    };
    format!("{start_text} {} {end_text} UTC", g.dash())
}

// ---------------------------------------------------------------------------
// The remote, and the header
// ---------------------------------------------------------------------------

/// The remote the view is showing share state for.
#[derive(Clone, Copy, Debug)]
pub enum RemoteView<'a> {
    /// A configured remote that can be a destination.
    Usable(&'a RemoteOverview),
    /// A configured remote that cannot be one (no push URL).
    Unusable(&'a RemoteOverview),
    /// `--remote` named a remote that is not configured.
    Missing { name: &'a str, others_exist: bool },
    /// Nothing is selected: there is no remote called `origin`, but others.
    NoneSelected,
    /// The repository has no remotes.
    NoRemotes,
}

const ADD_A_REMOTE: &str = "Add a remote to share: git remote add origin <url>";

impl RemoteView<'_> {
    /// Why nothing can be shared or published for this remote, in the words the
    /// header and a blocked action both use. `None` when the remote is usable.
    pub fn problem(&self) -> Option<String> {
        match self {
            RemoteView::Usable(_) => None,
            RemoteView::Unusable(_) => Some(
                "Sharing and publishing are off for this remote. Press r to choose another."
                    .to_owned(),
            ),
            RemoteView::Missing {
                others_exist: true, ..
            } => Some(
                "Sharing and publishing need a configured remote. Press r to choose one."
                    .to_owned(),
            ),
            RemoteView::Missing {
                others_exist: false,
                ..
            }
            | RemoteView::NoRemotes => Some(ADD_A_REMOTE.to_owned()),
            RemoteView::NoneSelected => {
                Some("Choose a remote to see share state. Press r.".to_owned())
            }
        }
    }
}

/// The widest a remote's location is shown. It can be up to about 2 KB, so it
/// is always cut by display width, never trusted to fit.
pub const LOCATION_WIDTH: usize = 48;
/// The next, shorter form of a location the header tries on a narrow terminal.
const LOCATION_WIDTH_NARROW: usize = 24;

/// A remote's location as shown: sanitized and cut to `width` columns.
pub fn shown_location(location: &str, width: usize, g: &Glyphs) -> String {
    text::truncate(&text::sanitize(location), width, g.ellipsis())
}

/// A destination fingerprint as shown: its first 12 characters and an ellipsis.
pub fn short_fingerprint(fingerprint: &str, g: &Glyphs) -> String {
    format!(
        "{}{}",
        text::sanitize(fingerprint.get(..12).unwrap_or(fingerprint)),
        g.ellipsis()
    )
}

/// What the header says, each part as candidates from the fullest to the
/// shortest so that a narrow terminal drops parentheticals first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderText {
    /// Right of the first line.
    pub remote: Vec<String>,
    /// Left of the second line.
    pub publishing: Vec<Vec<Seg>>,
    /// Right of the second line; empty when there is nothing to say.
    pub waiting: Vec<String>,
}

/// The header for the selected remote. `waiting` is how many shared thoughts
/// this clone has not published to it, over all conversations.
pub fn header(remote: &RemoteView, waiting: Option<i64>, g: &Glyphs) -> HeaderText {
    let waiting = match (remote, waiting) {
        (RemoteView::Usable(_), Some(n)) => waiting_text(n),
        _ => Vec::new(),
    };
    let remote_line = |full: Option<String>, short: String| match full {
        Some(full) => vec![full, short],
        None => vec![short],
    };
    match remote {
        RemoteView::Usable(overview) => {
            let name = text::sanitize(&overview.name);
            let fingerprint = overview
                .fingerprint
                .as_deref()
                .map(|fp| short_fingerprint(fp, g))
                .unwrap_or_default();
            let mut candidates = Vec::new();
            match overview.location.as_deref() {
                Some(location) => {
                    let wide = shown_location(location, LOCATION_WIDTH, g);
                    let narrow = shown_location(location, LOCATION_WIDTH_NARROW, g);
                    candidates.push(format!("remote: {name} -> {wide} ({fingerprint})"));
                    candidates.push(format!("remote: {name} -> {wide}"));
                    if narrow != wide {
                        candidates.push(format!("remote: {name} -> {narrow}"));
                    }
                }
                None => candidates.push(format!("remote: {name} ({fingerprint})")),
            }
            candidates.push(format!("remote: {name}"));
            HeaderText {
                remote: candidates,
                publishing: publishing_line(overview),
                waiting,
            }
        }
        RemoteView::Unusable(overview) => {
            let name = text::sanitize(&overview.name);
            HeaderText {
                remote: remote_line(
                    Some(format!("remote: {name} (no push URL)")),
                    format!("remote: {name}"),
                ),
                publishing: problem_line(remote),
                waiting,
            }
        }
        RemoteView::Missing { name, .. } => {
            let name = text::sanitize(name);
            HeaderText {
                remote: remote_line(
                    Some(format!("remote: {name} (not configured)")),
                    format!("remote: {name}"),
                ),
                publishing: problem_line(remote),
                waiting,
            }
        }
        RemoteView::NoneSelected => HeaderText {
            remote: remote_line(
                Some("remote: none selected (no remote named origin)".to_owned()),
                "remote: none selected".to_owned(),
            ),
            publishing: problem_line(remote),
            waiting,
        },
        RemoteView::NoRemotes => HeaderText {
            remote: remote_line(
                Some("remote: none (no Git remotes)".to_owned()),
                "remote: none".to_owned(),
            ),
            publishing: problem_line(remote),
            waiting,
        },
    }
}

fn problem_line(remote: &RemoteView) -> Vec<Vec<Seg>> {
    vec![vec![Seg::plain(remote.problem().unwrap_or_default())]]
}

/// Left of the second header line for a usable remote: whether publishing is
/// authorized, and whether auto-push is on, which is the one loud thing.
fn publishing_line(overview: &RemoteOverview) -> Vec<Vec<Seg>> {
    if let Some(reason) = &overview.consent_error {
        let reason = text::sanitize(reason);
        return vec![
            vec![Seg::plain(format!(
                "publishing: unknown (CVC privacy state cannot be read: {reason}); sharing and publishing from this view are off."
            ))],
            vec![Seg::plain(
                "publishing: unknown; sharing and publishing from this view are off.",
            )],
        ];
    }
    if !overview.sharing_consented {
        return vec![
            vec![Seg::plain(
                "publishing: not authorized yet (s or p will walk you through it)",
            )],
            vec![Seg::plain("publishing: not authorized yet")],
        ];
    }
    if overview.auto_push {
        // Auto-push is a standing choice, not the next push only, and the longer
        // forms say so; the middle one is for a terminal too narrow for the full
        // sentence.
        return vec![
            vec![
                Seg::plain("publishing: authorized in this clone   "),
                Seg::loud(
                    "auto-push: ON - every git push to this remote also publishes shared thoughts",
                ),
            ],
            vec![
                Seg::plain("publishing: authorized   "),
                Seg::loud("auto-push: ON - every git push also publishes shared thoughts"),
            ],
            vec![
                Seg::plain("publishing: authorized   "),
                Seg::loud("auto-push: ON"),
            ],
        ];
    }
    vec![
        vec![Seg::plain(
            "publishing: authorized in this clone   auto-push: off",
        )],
        // For a terminal that has room for the state but not for its long form.
        vec![Seg::plain("publishing: authorized   auto-push: off")],
        vec![Seg::plain("publishing: authorized")],
    ]
}

/// Right of the second header line: how many shared thoughts wait to be
/// published. Neutral at zero.
fn waiting_text(n: i64) -> Vec<String> {
    if n <= 0 {
        vec![
            "nothing waiting to publish".to_owned(),
            "nothing waiting".to_owned(),
        ]
    } else {
        vec![
            format!("{} waiting to publish", shared_thoughts(n)),
            format!("{n} waiting to publish"),
            format!("{n} waiting"),
        ]
    }
}

// ---------------------------------------------------------------------------
// Why an action is blocked
// ---------------------------------------------------------------------------

pub const NOT_ON_THIS_PLATFORM: &str = "Not available on this platform yet. Use cvc share, cvc unshare or cvc push --manual; on this platform they cannot discard keys typed ahead of the phrase.";
pub const IMPORTED_IS_READ_ONLY: &str = "Imported from a remote, so it is read-only here.";
pub const NO_DETAILS: &str =
    "This conversation could not be read, so nothing can be done with it from here.";
/// The typed phrase for a share or an unshare names the conversation by its id.
/// One that cannot be shown safely would be asked as a phrase that cannot be
/// read or typed, so such a conversation is not shared or unshared from here.
/// No captured conversation has one: the capture refuses an id outside 1 to 256
/// ASCII letters, digits, `.`, `_`, `-` and `:`, however its client chose it, so
/// this is for a conversation stored before that rule, and a backstop.
pub const UNSAFE_ID: &str = "This conversation's id contains characters that cannot be shown safely, so it cannot be shared or unshared from here.";

/// What ends the view when the terminal's input will not stop arriving (a paste
/// that goes on, a link that trickles): no key is acted on while it does, so
/// when it never stops the view is closed instead. Said as an error, in the
/// manner of the other messages of the command line.
pub const INPUT_WOULD_NOT_SETTLE: &str =
    "input kept arriving at the terminal, so the view was closed without acting on any of it";

/// Whether a conversation id can be shown as it is: it holds no control, format,
/// line-separator or paragraph-separator character.
pub fn id_is_safe(id: &str) -> bool {
    text::sanitize(id) == id
}

/// The remote's privacy state cannot be read, so every action of this view
/// stays off.
pub fn privacy_unreadable(reason: &str) -> String {
    format!(
        "CVC privacy state cannot be read ({}); sharing and publishing from this view are off.",
        text::sanitize(reason)
    )
}

pub fn all_shared_for_good(remote: &str) -> String {
    format!("All thoughts and future thoughts are already shared for '{remote}'.")
}

pub fn nothing_to_unshare(remote: &str) -> String {
    format!("Not shared for '{remote}', so there is nothing to unshare.")
}

/// Why `cvc unshare` would be refused for this conversation, or `None` when it
/// would go ahead. The unshare bar shows it, and `cvc unshare` itself fails with
/// it, before asking for a phrase.
pub fn unshare_refusal(check: &UnshareCheck, remote: &str) -> Option<String> {
    if check.published > 0 {
        Some(format!(
            "Cannot unshare: {} already published to '{remote}', and published thoughts cannot be recalled.",
            published_clause(check.published)
        ))
    } else if check.blocked() > 0 {
        Some(format!(
            "Cannot unshare yet: an earlier publish to '{remote}' did not finish cleanly. Run: cvc privacy reconcile --remote {remote}"
        ))
    } else {
        None
    }
}

fn published_clause(n: i64) -> String {
    if n == 1 {
        "1 thought is".to_owned()
    } else {
        format!("{n} thoughts are")
    }
}

// ---------------------------------------------------------------------------
// The pre-flight bar
// ---------------------------------------------------------------------------

/// The remote a bar is about, as the bar names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    pub name: String,
    pub location: Option<String>,
    pub fingerprint: String,
    pub auto_push: bool,
}

impl Destination {
    pub fn of(overview: &RemoteOverview, g: &Glyphs) -> Self {
        Self {
            name: text::sanitize(&overview.name),
            location: overview
                .location
                .as_deref()
                .map(|location| shown_location(location, LOCATION_WIDTH, g)),
            fingerprint: overview.fingerprint.clone().unwrap_or_default(),
            auto_push: overview.auto_push,
        }
    }

    /// `'origin'`.
    pub fn quoted(&self) -> String {
        format!("'{}'", self.name)
    }

    /// `'origin' (github.com/acme/repo)`, or just `'origin'` without a location.
    pub fn described(&self) -> String {
        match &self.location {
            Some(location) => format!("{} ({location})", self.quoted()),
            None => self.quoted(),
        }
    }
}

/// What a share bar asks about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareAsk {
    pub title: String,
    /// What a share covers now.
    pub thoughts: i64,
    /// Of those, the thoughts not yet shared.
    pub unshared: i64,
    /// Future thoughts are already being shared.
    pub future_on: bool,
}

/// One `key: label` of a bar. The label has a long form and, for a narrow
/// terminal, a short one.
struct KeyItem {
    key: &'static str,
    long: String,
    short: String,
}

impl KeyItem {
    fn new(key: &'static str, long: impl Into<String>, short: impl Into<String>) -> Self {
        Self {
            key,
            long: long.into(),
            short: short.into(),
        }
    }

    fn label(&self, short: bool) -> &str {
        if short {
            &self.short
        } else {
            &self.long
        }
    }

    fn width(&self, short: bool) -> usize {
        item_width(self.key, self.label(short))
    }
}

fn item_width(key: &str, label: &str) -> usize {
    text::width(key)
        + if label.is_empty() {
            0
        } else {
            2 + text::width(label)
        }
}

fn key_segs(items: &[(&str, &str)]) -> Vec<Seg> {
    let mut line = Vec::new();
    for (index, (key, label)) in items.iter().enumerate() {
        if index > 0 {
            line.push(Seg::plain("   "));
        }
        line.push(Seg::strong(*key));
        if !label.is_empty() {
            line.push(Seg::plain(format!(": {label}")));
        }
    }
    line
}

/// The keys of a bar, in as many lines as it takes: on one line in the long
/// form if that fits, else on one line in the short form, else packed into
/// lines, each key in the long form unless it does not fit a line by itself.
/// No key is ever cut off.
fn key_lines(items: &[KeyItem], width: usize) -> Vec<Vec<Seg>> {
    let total = |short: bool| {
        items.iter().map(|item| item.width(short)).sum::<usize>()
            + 3 * items.len().saturating_sub(1)
    };
    for short in [false, true] {
        if total(short) <= width {
            let pairs: Vec<_> = items.iter().map(|i| (i.key, i.label(short))).collect();
            return vec![key_segs(&pairs)];
        }
    }
    let mut lines = Vec::new();
    let mut current: Vec<(&str, &str)> = Vec::new();
    let mut used = 0;
    for item in items {
        let label = item.label(item.width(false) > width);
        let needed = item_width(item.key, label);
        if needed > width {
            // Too long for a line even in its short form: wrap it.
            if !current.is_empty() {
                lines.push(key_segs(&current));
                current.clear();
                used = 0;
            }
            let whole = format!("{}: {label}", item.key);
            for (index, part) in text::wrap(&whole, width).into_iter().enumerate() {
                lines.push(match part.strip_prefix(item.key) {
                    Some(rest) if index == 0 => vec![Seg::strong(item.key), Seg::plain(rest)],
                    _ => vec![Seg::plain(part)],
                });
            }
            continue;
        }
        if !current.is_empty() && used + 3 + needed > width {
            lines.push(key_segs(&current));
            current.clear();
            used = 0;
        }
        used = if current.is_empty() {
            needed
        } else {
            used + 3 + needed
        };
        current.push((item.key, label));
    }
    if !current.is_empty() {
        lines.push(key_segs(&current));
    }
    lines
}

/// The widest a conversation title is allowed to be in a question, so that a
/// very long title cannot push the destination off the screen.
const TITLE_CAP: usize = 40;
/// The least of a title a question keeps when it cuts the title to fit one line.
const TITLE_FLOOR: usize = 20;

/// A question that carries a title, as lines of the width given: on one line,
/// with the title cut but not below [`TITLE_FLOOR`] columns, when that fits;
/// otherwise the whole sentence wrapped, with the title cut to [`TITLE_CAP`].
/// The rest of the sentence (counts, destination) is never dropped: it is what
/// the person is deciding about.
fn question(
    width: usize,
    title: &str,
    sentence: &dyn Fn(&str) -> String,
    ellipsis: &str,
) -> Vec<Vec<Seg>> {
    let title_width = text::width(title);
    let full = sentence(title);
    let over = text::width(&full).saturating_sub(width);
    let lines = if over == 0 {
        vec![full]
    } else if title_width > over && title_width - over >= TITLE_FLOOR.min(title_width) {
        vec![sentence(&text::truncate(
            title,
            title_width - over,
            ellipsis,
        ))]
    } else {
        text::wrap(
            &sentence(&text::truncate(title, TITLE_CAP, ellipsis)),
            width,
        )
    };
    lines
        .into_iter()
        .map(|line| vec![Seg::strong(line)])
        .collect()
}

/// A sentence as lines of the width given.
fn sentence(text: &str, width: usize) -> Vec<Vec<Seg>> {
    text::wrap(text, width)
        .into_iter()
        .map(|line| vec![Seg::strong(line)])
        .collect()
}

/// The bar for `s`: the question, then what each key does.
pub fn share_bar(ask: &ShareAsk, dest: &Destination, width: usize, g: &Glyphs) -> Vec<Vec<Seg>> {
    let auto = |note: &str| {
        if dest.auto_push {
            note.to_owned()
        } else {
            String::new()
        }
    };
    let long_note = auto(" (auto-push publishes them at each git push)");
    let short_note = auto(" (auto-push publishes them)");
    if ask.unshared <= 0 {
        // Everything is shared already: only the future can still be added.
        let said = if ask.thoughts == 1 {
            format!("The thought is already shared for {}.", dest.quoted())
        } else {
            format!(
                "All {} are already shared for {}.",
                thoughts(ask.thoughts),
                dest.quoted()
            )
        };
        let mut lines = sentence(&said, width);
        lines.extend(key_lines(
            &[
                KeyItem::new(
                    "f",
                    format!("also share future thoughts{long_note}"),
                    format!("also share future{short_note}"),
                ),
                KeyItem::new("Esc", "cancel", "cancel"),
            ],
            width,
        ));
        return lines;
    }
    let described = dest.described();
    let sizes = format!(
        "{}, {} not yet shared",
        thoughts(ask.thoughts),
        ask.unshared
    );
    let mut lines = question(
        width,
        &ask.title,
        &|title| format!("Share \"{title}\" ({sizes}) for {described}?"),
        g.ellipsis(),
    );
    let (snapshot, snapshot_short) = if ask.future_on {
        (
            "this snapshot only (stops sharing future thoughts)",
            "snapshot only (stops future)",
        )
    } else {
        ("this snapshot only", "snapshot only")
    };
    let (future, future_short) = if ask.future_on {
        ("keep sharing future thoughts", "keep future")
    } else {
        ("also future thoughts", "also future")
    };
    lines.extend(key_lines(
        &[
            KeyItem::new("Enter", snapshot, snapshot_short),
            KeyItem::new(
                "f",
                format!("{future}{long_note}"),
                format!("{future_short}{short_note}"),
            ),
            KeyItem::new("Esc", "cancel", "cancel"),
        ],
        width,
    ));
    lines
}

/// The bar for `u`.
pub fn unshare_bar(
    title: &str,
    shared: i64,
    dest: &Destination,
    width: usize,
    g: &Glyphs,
) -> Vec<Vec<Seg>> {
    let remote = dest.quoted();
    let tail = format!(
        "{} {} private again.",
        shared_thoughts(shared),
        if shared == 1 { "becomes" } else { "become" }
    );
    let mut lines = question(
        width,
        title,
        &|title| format!("Unshare \"{title}\" for {remote}? Its {tail}"),
        g.ellipsis(),
    );
    lines.extend(key_lines(
        &[
            KeyItem::new("Enter", "continue to the phrase", "continue"),
            KeyItem::new("Esc", "cancel", "cancel"),
        ],
        width,
    ));
    lines
}

/// The bar for `p`. `waiting` is the clone's own count of shared thoughts not
/// yet published; the publish itself checks the remote first.
pub fn publish_bar(waiting: i64, dest: &Destination, width: usize) -> Vec<Vec<Seg>> {
    if waiting <= 0 {
        let mut lines = sentence(
            &format!(
                "Nothing is waiting by this clone's count. Check {} anyway?",
                dest.quoted()
            ),
            width,
        );
        lines.push(vec![
            Seg::strong("Enter"),
            Seg::plain(" / "),
            Seg::strong("Esc"),
        ]);
        return lines;
    }
    let mut lines = sentence(
        &format!(
            "Publish shared thoughts to {}? {waiting} waiting by this clone's count.",
            dest.described()
        ),
        width,
    );
    lines.extend(key_lines(
        &[
            KeyItem::new("Enter", "continue", "continue"),
            KeyItem::new("Esc", "cancel", "cancel"),
        ],
        width,
    ));
    lines
}

/// The offer that replaces the `s` and `p` bars while the remote is not
/// authorized for publishing.
pub fn authorize_bar(dest: &Destination, width: usize) -> Vec<Vec<Seg>> {
    let mut lines = sentence(
        &format!(
            "Publishing to {} is not authorized in this clone yet.",
            dest.described()
        ),
        width,
    );
    lines.extend(key_lines(
        &[
            KeyItem::new(
                "Enter",
                "authorize (you will type a phrase)",
                "authorize (type a phrase)",
            ),
            KeyItem::new("Esc", "cancel", "cancel"),
        ],
        width,
    ));
    lines
}

/// The bar that only says why nothing can be done, with the one way out. It
/// wraps to at most three lines so that no part of the reason is cut off on a
/// narrow terminal.
pub fn blocked_bar(reason: &str, width: usize) -> Vec<Vec<Seg>> {
    const HINT: &str = "any key: ok";
    let mut lines = text::wrap(reason, width);
    let fits = lines
        .last()
        .is_some_and(|last| text::width(last) + 3 + HINT.len() <= width);
    if fits {
        if let Some(last) = lines.last_mut() {
            last.push_str("   ");
        }
    } else {
        lines.push(String::new());
    }
    if lines.len() > 3 {
        lines.truncate(3);
        if let Some(last) = lines.last_mut() {
            *last = String::new();
        }
    }
    let last = lines.len() - 1;
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let mut segs = vec![Seg::plain(line)];
            if index == last {
                segs.push(Seg::strong(HINT));
            }
            segs
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// Which flow a result is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowKind {
    Share,
    Unshare,
    Publish,
    Authorize,
}

impl FlowKind {
    /// The verb the banner on the suspended screen is headed with.
    pub fn banner_name(self) -> &'static str {
        match self {
            FlowKind::Share => "share",
            FlowKind::Unshare => "unshare",
            FlowKind::Publish => "publish",
            FlowKind::Authorize => "authorize publishing",
        }
    }

    fn failed(self) -> &'static str {
        match self {
            FlowKind::Share => "Share failed",
            FlowKind::Unshare => "Unshare failed",
            FlowKind::Publish => "Publish failed",
            FlowKind::Authorize => "Authorization failed",
        }
    }
}

/// The notice after a share. `unshared_agents` is how many agent conversations
/// listed under the shared one have nothing shared from this clone. Sharing a
/// conversation never shares them as conversations of their own, but the
/// thoughts of the conversation that was shared include each agent's prompt and
/// result, so the notice says so.
pub fn shared_status(
    thoughts_shared: usize,
    remote: &str,
    future: FutureSharePolicy,
    auto_push: bool,
    unshared_agents: usize,
    g: &Glyphs,
) -> String {
    let mut said = format!(
        "{} marked shared for remote '{remote}'; future thoughts are {}.",
        thoughts(thoughts_shared as i64),
        match future {
            FutureSharePolicy::Shared => "shared too",
            FutureSharePolicy::Private => "private",
        }
    );
    said.push_str(&if auto_push {
        // A standing choice: it publishes at every push, not only the next one.
        format!(" Auto-push is on: every git push to '{remote}' publishes them.")
    } else {
        " Not published yet (p).".to_owned()
    });
    if unshared_agents > 0 {
        let n = unshared_agents as i64;
        said.push_str(&if n == 1 {
            format!(
                " 1 agent conversation is not shared from here, but this session's thoughts include its prompt and result ({} to see).",
                g.right()
            )
        } else {
            format!(
                " {n} agent conversations are not shared from here, but this session's thoughts include their prompts and results ({} to see).",
                g.right()
            )
        });
    }
    said
}

/// The notice after an unshare.
pub fn unshared_status(made_private: usize, remote: &str) -> String {
    if made_private == 0 {
        return format!("Nothing was shared for remote '{remote}', so nothing changed.");
    }
    format!(
        "{} made private again for remote '{remote}'.",
        thoughts(made_private as i64)
    )
}

/// The notice after a publish that did not fail.
pub fn published_status(outcome: &PublishOutcome, remote: &str) -> String {
    match outcome {
        PublishOutcome::Skipped => format!("Nothing was published to '{remote}'."),
        PublishOutcome::NothingNew { shared_total } if *shared_total > 0 => format!(
            "No new shared thoughts to publish to '{remote}' ({shared_total} shared in total)."
        ),
        PublishOutcome::NothingNew { .. } => {
            format!("No new shared thoughts to publish to '{remote}'.")
        }
        PublishOutcome::Published(publication) => {
            let new = publication.new_ids.len() as i64;
            let total = publication.shared_total;
            let extras = publication_extras(publication);
            match (new, extras) {
                (0, None) => format!(
                    "Published a format update to '{remote}'; no new thoughts, links or tombstones."
                ),
                (0, Some(extras)) => format!("Published to '{remote}': {extras}."),
                (new, extras) => {
                    // On a first publish the total is the new count; say it once.
                    let context = if extras.is_none() && new as usize == total {
                        String::new()
                    } else {
                        format!(" ({total} shared in total)")
                    };
                    let tail = extras
                        .map(|extras| format!(", plus {extras}"))
                        .unwrap_or_default();
                    format!(
                        "Published {} new shared {} to '{remote}'{context}{tail}.",
                        new,
                        if new == 1 { "thought" } else { "thoughts" }
                    )
                }
            }
        }
    }
}

/// Everything a publish carries besides new thoughts, as a noun phrase, in the
/// view's vocabulary. `None` when there is nothing else.
fn publication_extras(p: &cvc_core::sync::ProjectionPublication) -> Option<String> {
    let counted = |n: usize, one: &str, many: &str| (n > 0).then(|| count(n as i64, one, many));
    let removal = match (p.removed_interactions, p.removed_records) {
        (0, 0) => None,
        (n, 0) => Some(format!(
            "the removal of {} from the current tree (not erased from history)",
            count(n as i64, "tombstoned thought", "tombstoned thoughts")
        )),
        (n, records) if n > 0 => Some(format!(
            "the removal of {} and {} from the current tree (not erased from history)",
            count(n as i64, "tombstoned thought", "tombstoned thoughts"),
            count(records as i64, "attached record", "attached records")
        )),
        (_, records) => Some(format!(
            "the removal of {} of tombstoned thoughts from the current tree (not erased from history)",
            count(records as i64, "record", "records")
        )),
    };
    let parts: Vec<String> = [
        counted(p.new_link_records, "commit link", "commit links"),
        counted(p.new_tombstones, "tombstone", "tombstones"),
        counted(
            p.new_derivation_records,
            "derivation record",
            "derivation records",
        ),
        removal,
    ]
    .into_iter()
    .flatten()
    .collect();
    match parts.as_slice() {
        [] => None,
        [only] => Some(only.clone()),
        [head @ .., last] => Some(format!("{} and {last}", head.join(", "))),
    }
}

pub fn authorized_status(remote: &str) -> String {
    format!("Publishing to '{remote}' is now authorized. Auto-push stays off.")
}

/// A share or a publish was asked for on publishing settings that are not the
/// ones the screen showed: authorization or auto-push was changed elsewhere in
/// the meantime. Nothing ran; the screen has been refreshed.
pub fn settings_changed_status(remote: &str) -> String {
    format!("Publishing settings for '{remote}' changed; check them and press the key again.")
}

/// Authorizing was asked for, but the remote had been authorized in the
/// meantime; asking again would reset its auto-push setting, so nothing ran.
pub fn already_authorized_status(remote: &str) -> String {
    format!("Publishing to '{remote}' is already authorized. Nothing was changed.")
}

/// The notice after a flow that stopped. `remote` is the name of the remote the
/// flow was for. Nothing here shows an error's own text except `Other`, which
/// is cut to its first line and sanitized.
pub fn failure_status(kind: FlowKind, failure: &FlowFailure, remote: &str) -> String {
    match (failure, kind) {
        (FlowFailure::NotConfirmed, kind) => not_done(kind, "the typed phrase did not match", remote),
        // No phrase was asked at all, which is a different thing to say from a
        // phrase that was asked and did not match: nothing was typed, so nothing
        // can be said about what was.
        (FlowFailure::InputKeptArriving, kind) => not_done(
            kind,
            "input kept arriving at the terminal, so no phrase was asked",
            remote,
        ),
        (FlowFailure::TerminalNotRestored, kind) => not_done(
            kind,
            "the terminal could not be put back to normal input, so no phrase was asked",
            remote,
        ),
        (FlowFailure::ConversationChanged, _) => {
            "Not shared: this conversation gained new thoughts while you were confirming. Press s to try again."
                .to_owned()
        }
        (FlowFailure::UnshareBlocked, _) => format!(
            "Cannot unshare: some of this conversation is published, or a publish of it did not finish, for '{remote}'."
        ),
        (FlowFailure::ConsentMissing, _) => {
            format!("Publishing to '{remote}' is not authorized yet. Press p to set it up.")
        }
        // A publish can stop on an error after the challenge was answered and
        // the push began: the remote may then hold some of it. Nothing here may
        // say that nothing was sent. `OutcomeUnknown` is such an error that is
        // known to be one; the view never bundles a publish with a share, so
        // only a publish produces it.
        (FlowFailure::OutcomeUnknown(text), _) | (FlowFailure::Other(text), FlowKind::Publish) => {
            format!(
                "{} (error: {})",
                publish_unfinished(remote),
                text::sanitize(text::first_line(text))
            )
        }
        // The view never runs a share and a publish as one flow, so this does
        // not arise from it; the words are for the case that it ever does.
        (FlowFailure::SharedButNotPublished(inner), _) => format!(
            "The share was recorded, but publishing did not complete. {}",
            failure_status(FlowKind::Publish, inner, remote)
        ),
        (FlowFailure::Other(text), kind) => {
            format!(
                "{}: {}",
                kind.failed(),
                text::sanitize(text::first_line(text))
            )
        }
    }
}

/// What a flow that stopped at its consent gate says, whichever way the gate
/// was not satisfied: that it did not happen, `why` (a clause without its final
/// period), and what that leaves untouched. The four kinds have their own words
/// for each.
fn not_done(kind: FlowKind, why: &str, remote: &str) -> String {
    match kind {
        FlowKind::Share => format!("Not shared: {why}. Nothing was changed."),
        FlowKind::Unshare => format!("Not unshared: {why}. Nothing was changed."),
        // The fetch that comes first did contact the remote; nothing was published.
        FlowKind::Publish => {
            format!("Not published: {why}. Nothing was published to '{remote}'.")
        }
        FlowKind::Authorize => format!("Not authorized: {why}."),
    }
}

/// What to say, and do, about a publish that stopped on an error that is not
/// one of the typed refusals. The one place this is worded.
pub fn publish_unfinished(remote: &str) -> String {
    format!(
        "Publish to '{remote}' did not complete; the remote may hold some of it. Run: cvc privacy reconcile --remote {remote}"
    )
}

/// What is printed once the screen is restored, when something was shared this
/// session and is still waiting.
pub fn waiting_note(waiting: i64, remote: &str) -> String {
    format!(
        "{} still waiting to publish to '{remote}'. Press p in cvc conversations, or run cvc push --manual --remote {remote}.",
        if waiting == 1 {
            "1 shared thought is".to_owned()
        } else {
            format!("{waiting} shared thoughts are")
        }
    )
}

// ---------------------------------------------------------------------------
// The suspended screen
// ---------------------------------------------------------------------------

/// What is printed on the main screen before a flow runs: where it goes, what
/// it does, and how to back out. The flow's own output follows it unchanged, so
/// nothing sits between that output and the phrase prompt. `future` is what a
/// share will do about later thoughts.
pub fn banner(
    kind: FlowKind,
    dest: &Destination,
    future: Option<FutureSharePolicy>,
    g: &Glyphs,
) -> Vec<String> {
    let remote = dest.quoted();
    let heading = format!("-- cvc: {} ", kind.banner_name());
    let mut lines = vec![format!(
        "{heading}{}",
        "-".repeat(60usize.saturating_sub(heading.len()))
    )];
    let fingerprint = short_fingerprint(&dest.fingerprint, g);
    lines.push(match &dest.location {
        Some(location) => format!("Remote   {remote} -> {location}  (fingerprint {fingerprint})"),
        None => format!("Remote   {remote}  (fingerprint {fingerprint})"),
    });
    match kind {
        FlowKind::Share => {
            let mut effect = format!(
                "Marks this conversation's thoughts as shared for {remote}. CVC sends nothing to {remote} yet."
            );
            if dest.auto_push {
                effect.push_str(&format!(
                    " Auto-push is on: every git push to {remote}, including one an editor runs, publishes them."
                ));
            }
            lines.push(format!("Effect   {effect}"));
            lines.push(format!(
                "Future   Thoughts added later {}.",
                match future {
                    Some(FutureSharePolicy::Shared) => "are shared too",
                    _ => "stay private",
                }
            ));
        }
        FlowKind::Unshare => lines.push(format!(
            "Effect   Makes this conversation private again for {remote}. Only possible while none of it is published."
        )),
        FlowKind::Publish => {
            lines.push(format!(
                "Effect   Sends the shared thoughts to {remote}, where anyone who can read it can read them. Published thoughts cannot be recalled, and scrubbing can miss secrets."
            ));
        }
        FlowKind::Authorize => lines.push(format!(
            "Effect   Allows publishing to {remote} at this exact push URL; you are asked again if the URL changes. CVC checks only the URL, not who owns the repository or who can read it. Publishes nothing and does not turn on auto-push."
        )),
    }
    lines.push("         An empty line and Enter cancels.".to_owned());
    if kind == FlowKind::Publish {
        // The fetch that comes first may ask for credentials.
        lines.push(format!("Checking {remote} first{}", g.ellipsis()));
    }
    lines
}

// ---------------------------------------------------------------------------
// The key bar
// ---------------------------------------------------------------------------

/// Which keys do anything for the selected row and remote; the rest are dimmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Available {
    pub details: bool,
    pub share: bool,
    pub unshare: bool,
    pub publish: bool,
}

/// The key bar of the list, as wide as `width` allows: the full form, then
/// without the words after the arrows, then with `? help` last and the
/// leftmost keys dropped one by one.
pub fn key_bar(available: &Available, width: usize, g: &Glyphs) -> Vec<Seg> {
    struct Item {
        keys: String,
        label: &'static str,
        on: bool,
    }
    let item = |keys: &str, label: &'static str, on: bool| Item {
        keys: keys.to_owned(),
        label,
        on,
    };
    let items = |labels: bool, help_last: bool| {
        let mut items = vec![
            item(g.up_down(), if labels { "move" } else { "" }, true),
            item(g.left_right(), if labels { "agents" } else { "" }, true),
            item("enter", "details", available.details),
            item("s", "share", available.share),
            item("u", "unshare", available.unshare),
            item("p", "publish", available.publish),
            item("r", "remote", true),
        ];
        if help_last {
            items.push(item("q", "quit", true));
            items.push(item("?", "help", true));
        } else {
            items.push(item("?", "help", true));
            items.push(item("q", "quit", true));
        }
        items
    };
    let measure = |items: &[Item]| {
        1 + items
            .iter()
            .map(|i| text::width(&i.keys) + usize::from(!i.label.is_empty()) + i.label.len())
            .sum::<usize>()
            + 3 * items.len().saturating_sub(1)
    };
    let mut chosen = items(true, false);
    if measure(&chosen) > width {
        chosen = items(false, false);
    }
    if measure(&chosen) > width {
        chosen = items(false, true);
        while chosen.len() > 2 && measure(&chosen) > width {
            chosen.remove(0);
        }
    }
    let mut line = vec![Seg::plain(" ")];
    for (index, item) in chosen.iter().enumerate() {
        if index > 0 {
            line.push(Seg::plain("   "));
        }
        let (key, label) = if item.on {
            (
                Seg::strong(item.keys.clone()),
                Seg::plain(format!(" {}", item.label)),
            )
        } else {
            (
                Seg::dim(item.keys.clone()),
                Seg::dim(format!(" {}", item.label)),
            )
        };
        line.push(key);
        if !item.label.is_empty() {
            line.push(label);
        }
    }
    line
}

/// The bottom line while an overlay is open.
pub fn overlay_bar(scrolls: bool, g: &Glyphs) -> Vec<Seg> {
    let mut line = vec![Seg::plain(" ")];
    if scrolls {
        line.push(Seg::strong(g.up_down()));
        line.push(Seg::plain(" scroll   "));
    }
    line.push(Seg::strong("esc"));
    line.push(Seg::plain(" close"));
    line
}

/// The bottom line of the remote picker.
pub fn picker_bar(g: &Glyphs) -> Vec<Seg> {
    vec![
        Seg::plain(" "),
        Seg::strong(g.up_down()),
        Seg::plain(" move   "),
        Seg::strong("enter"),
        Seg::plain(" select   "),
        Seg::strong("esc"),
        Seg::plain(" back"),
    ]
}

// ---------------------------------------------------------------------------
// Details
// ---------------------------------------------------------------------------

/// One logical row of the details overlay or the help page; the view wraps it
/// to the width it has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    Title(String),
    Blank,
    Heading(String),
    Field {
        label: &'static str,
        value: String,
    },
    /// As `Field`, with a label that is not a literal.
    Item {
        label: String,
        value: String,
    },
    Text(String),
}

/// What kind of conversation a row is, beyond a plain one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lineage {
    None,
    /// An agent listed under this parent (its sanitized title).
    AgentOf(String),
    /// An agent whose parent is not in the list.
    OrphanAgent,
}

/// What the details of one conversation are built from.
pub struct DetailsInput<'a> {
    pub summary: &'a ConversationSummary,
    /// Read from the store for this destination; without it the rows that need
    /// it are left out.
    pub details: Option<&'a ConversationDetails>,
    pub lineage: Lineage,
    /// How many agents are listed under it.
    pub agents: usize,
    /// The selected remote, when it can be a destination.
    pub remote: Option<&'a RemoteOverview>,
}

/// `Share covers 29 thoughts: 27 already shared, 2 new`, and its other forms.
fn covers(details: &ConversationDetails) -> String {
    let total = details.snapshot_count as i64;
    let shared = details.snapshot_shared as i64;
    let new = details.snapshot_new as i64;
    if total == 0 {
        "nothing".to_owned()
    } else if new == 0 {
        format!("{}: all already shared", thoughts(total))
    } else if shared == 0 {
        format!("{}: all of them", thoughts(total))
    } else {
        format!("{}: {shared} already shared, {new} new", thoughts(total))
    }
}

/// The context files of a conversation: how many, the most used, and how many
/// more there are.
fn context_files(details: &ConversationDetails, g: &Glyphs) -> String {
    if details.context_path_total == 0 {
        return "none".to_owned();
    }
    let listed: Vec<String> = details
        .context_paths
        .iter()
        .map(|p| format!("{} ({})", text::sanitize(&p.path), p.interactions))
        .collect();
    let mut said = format!("{}: {}", details.context_path_total, listed.join(", "));
    let more = details
        .context_path_total
        .saturating_sub(details.context_paths.len());
    if more > 0 {
        said.push_str(&format!(", {} (+{more} more)", g.ellipsis()));
    }
    said
}

/// The details overlay of one conversation.
pub fn details_page(input: &DetailsInput, g: &Glyphs) -> Vec<Page> {
    let summary = input.summary;
    let mut page = vec![
        Page::Title(text::title(&summary.title)),
        Page::Blank,
        Page::Field {
            label: "ID",
            value: text::sanitize(&summary.id),
        },
    ];
    let lineage = match (&input.lineage, summary.imported) {
        (Lineage::AgentOf(parent), true) => {
            Some(format!("Subagent of \"{parent}\", imported from a remote"))
        }
        (Lineage::AgentOf(parent), false) => Some(format!("Subagent of \"{parent}\"")),
        (Lineage::OrphanAgent, imported) => Some(format!(
            "Subagent{}. Parent session is not captured on this machine.",
            if imported {
                ", imported from a remote"
            } else {
                ""
            }
        )),
        (Lineage::None, true) => Some("Imported from a remote".to_owned()),
        (Lineage::None, false) => None,
    };
    if let Some(lineage) = lineage {
        page.push(Page::Field {
            label: "Kind",
            value: lineage,
        });
    }
    page.push(Page::Field {
        label: "Activity",
        value: activity(summary.first_activity, summary.last_activity, g),
    });
    page.push(Page::Field {
        label: "Thoughts",
        value: summary.thoughts.to_string(),
    });
    page.push(Page::Field {
        label: "Linked",
        value: format!(
            "{} of {} linked to a commit",
            summary.linked,
            thoughts(summary.thoughts)
        ),
    });
    if let Some(details) = input.details {
        page.push(Page::Field {
            label: "Context files",
            value: context_files(details, g),
        });
    }
    if input.agents > 0 {
        page.push(Page::Field {
            label: "Subagents",
            value: if input.agents == 1 {
                "1 conversation, shared separately; its prompt and result are part of this one"
                    .to_owned()
            } else {
                format!(
                    "{} conversations, each shared separately; their prompts and results are part of this one",
                    input.agents
                )
            },
        });
    }
    if let Some(remote) = input.remote {
        let dest = Destination::of(remote, g);
        page.push(Page::Blank);
        page.push(Page::Heading(format!(
            "With {} (fingerprint {})",
            dest.quoted(),
            short_fingerprint(&dest.fingerprint, g)
        )));
        page.push(Page::Field {
            label: "State",
            value: state_label(summary, true, g).0,
        });
        if !summary.imported {
            page.push(Page::Field {
                label: "Future thoughts",
                value: if summary.share_future == Some(true) {
                    "shared too".to_owned()
                } else {
                    "private".to_owned()
                },
            });
            if let Some(details) = input.details {
                page.push(Page::Field {
                    label: "Share covers",
                    value: covers(details),
                });
            }
        }
        if summary.unconfirmed > 0 {
            page.push(Page::Field {
                label: "Unconfirmed",
                value: format!(
                    "{} from a publish that did not finish cleanly",
                    thoughts(summary.unconfirmed)
                ),
            });
        }
        page.push(Page::Field {
            label: "Auto-push",
            value: if dest.auto_push {
                format!(
                    "on: every git push to {} also publishes shared thoughts",
                    dest.quoted()
                )
            } else {
                "off".to_owned()
            },
        });
    }
    page.push(Page::Blank);
    page.push(Page::Text(
        "A thought is one recorded step (for Claude Code, one assistant response).".to_owned(),
    ));
    page.push(Page::Text(
        "Scrubbing can miss secrets. Published thoughts cannot be recalled.".to_owned(),
    ));
    page
}

/// The lines of the persistent strip under the list: the full title, the
/// counts and activity, what a share would cover and what it would do to future
/// thoughts, and what kind of conversation it is.
pub fn strip_lines(input: &DetailsInput, g: &Glyphs) -> Vec<String> {
    let summary = input.summary;
    let linked = if summary.linked == 1 {
        "1 linked to a commit".to_owned()
    } else {
        format!("{} linked to commits", summary.linked)
    };
    let mut lines = vec![
        text::title(&summary.title),
        format!(
            "{}, {linked}, {}",
            thoughts(summary.thoughts),
            activity(summary.first_activity, summary.last_activity, g)
        ),
    ];
    if input.remote.is_some() && !summary.imported {
        if let Some(details) = input.details {
            let shared = details.snapshot_shared as i64;
            let new = details.snapshot_new as i64;
            let total = details.snapshot_count as i64;
            let size = if total == 0 {
                "Share covers nothing as of now".to_owned()
            } else if new == 0 {
                format!(
                    "Share covers {} as of now (all already shared)",
                    thoughts(total)
                )
            } else if shared == 0 {
                format!(
                    "Share covers {} as of now (all of them new)",
                    thoughts(total)
                )
            } else {
                format!(
                    "Share covers {} as of now ({shared} already shared, {new} new)",
                    thoughts(total)
                )
            };
            lines.push(format!(
                "{size}. Future thoughts: {}.",
                if summary.share_future == Some(true) {
                    "shared"
                } else {
                    "private"
                }
            ));
        }
    }
    match (&input.lineage, summary.imported) {
        (Lineage::AgentOf(parent), _) => lines.push(format!("Subagent of \"{parent}\".")),
        (Lineage::OrphanAgent, _) => {
            lines.push("Parent session is not captured on this machine.".to_owned());
        }
        (Lineage::None, true) => lines.push(IMPORTED_IS_READ_ONLY.to_owned()),
        (Lineage::None, false) => {}
    }
    lines
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

/// The help page.
pub fn help_page(g: &Glyphs) -> Vec<Page> {
    let key = |keys: String, value: &str| Page::Item {
        label: keys,
        value: value.to_owned(),
    };
    let dot = g.dot();
    let state = |label: String, meaning: &str| Page::Text(format!("{label}: {meaning}"));
    vec![
        Page::Title("Help".to_owned()),
        Page::Blank,
        Page::Text(
            "private --s--> shared (in this clone) --p--> published (on the remote)".to_owned(),
        ),
        Page::Text("u works only before publish.".to_owned()),
        Page::Blank,
        Page::Heading("Keys".to_owned()),
        key(
            format!("{} j k", g.up_down()),
            "move; PgUp PgDn Home End jump",
        ),
        key(
            format!("{} l h", g.left_right()),
            "show or hide a conversation's agents; left on an agent goes to its session",
        ),
        key("enter".to_owned(), "details of the selected conversation"),
        key(
            "s".to_owned(),
            "share the selected conversation for the selected remote",
        ),
        key(
            "u".to_owned(),
            "unshare it again, while none of it is published",
        ),
        key(
            "p".to_owned(),
            "publish the shared thoughts to the selected remote",
        ),
        key("r".to_owned(), "choose another remote"),
        key("esc".to_owned(), "close a bar, a page or the remote list"),
        key(
            "q".to_owned(),
            "close a page or the remote list; quit from the list",
        ),
        Page::Blank,
        Page::Heading("States".to_owned()),
        state(
            format!("{} private", g.private()),
            "not shared from this clone for this remote, and the remote holds none of it.",
        ),
        state(
            format!("{} shared {dot} 2 waiting", g.waiting()),
            "shared here; 2 thoughts not yet published.",
        ),
        state(
            format!("{} published", g.published()),
            "everything shared is published.",
        ),
        state(
            format!("{} shared {dot} future only", g.waiting()),
            "nothing shared yet; later thoughts are shared as they are captured.",
        ),
        state(
            format!("{} published {dot} not shared here", g.published()),
            "the remote already holds it, and this clone has no share for it.",
        ),
        state(
            format!("{} unconfirmed {dot} not shared here", g.unconfirmed()),
            "a publish of it may have reached the remote; cvc privacy reconcile settles that.",
        ),
        state(
            format!("{dot} 3 not shared here"),
            "3 more thoughts of a shared conversation that the remote holds but this clone does not share.",
        ),
        state(
            format!("{dot} 3 private"),
            "3 more thoughts of a shared conversation that are not shared from here and that the remote holds none of.",
        ),
        state(
            "+future".to_owned(),
            "later thoughts are shared as they are captured.",
        ),
        state("imported".to_owned(), "came from a remote; read-only here."),
        Page::Blank,
        Page::Text(
            "Share, unshare, publish and authorize each wait for their typed phrase; no key does any of them by itself."
                .to_owned(),
        ),
        Page::Text(
            "Two standing choices act later without a new phrase: +future shares new thoughts as they are captured, and auto-push (set with cvc privacy set-auto-push, never from this view) publishes shared thoughts at every git push to that remote."
                .to_owned(),
        ),
        Page::Text(
            "Scrubbing and .thoughtignore can miss secrets; they are not a guarantee.".to_owned(),
        ),
        Page::Text(
            "Counts in the header are this clone's view; publish re-checks the remote first."
                .to_owned(),
        ),
        Page::Blank,
        Page::Text(
            "A thought is one recorded step (for Claude Code, one assistant response).".to_owned(),
        ),
    ]
}

// ---------------------------------------------------------------------------
// The remote picker
// ---------------------------------------------------------------------------

pub const PICKER_TITLE: &str = "Choose a remote";
pub const PICKER_SUBTITLE: &str = "Publishing is authorized per exact push URL, not per remote name: remotes with the same push URL share it.";

/// One remote of the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRow {
    pub name: String,
    pub location: String,
    pub fingerprint: String,
    pub publishing: &'static str,
    pub auto_push: &'static str,
    /// Why the remote cannot be selected; `None` when it can.
    pub unusable: Option<String>,
    pub current: bool,
}

pub fn remote_rows(
    remotes: &[RemoteOverview],
    current: Option<&str>,
    g: &Glyphs,
) -> Vec<RemoteRow> {
    remotes
        .iter()
        .map(|remote| {
            let (fingerprint, unusable) = match &remote.fingerprint {
                Ok(fingerprint) => (short_fingerprint(fingerprint, g), None),
                Err(reason) => (String::new(), Some(text::sanitize(reason))),
            };
            RemoteRow {
                name: text::sanitize(&remote.name),
                location: remote
                    .location
                    .as_deref()
                    .map(|location| shown_location(location, LOCATION_WIDTH, g))
                    .unwrap_or_else(|| g.dash().to_owned()),
                fingerprint,
                publishing: if remote.consent_error.is_some() {
                    "unknown"
                } else if remote.sharing_consented {
                    "authorized"
                } else {
                    "not authorized yet"
                },
                auto_push: if remote.auto_push { "on" } else { "off" },
                unusable,
                current: current == Some(remote.name.as_str()),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Empty states
// ---------------------------------------------------------------------------

/// What `cvc conversations` prints, one line each and before any screen is
/// entered, when there is nothing to list: the fact, and what to do next. With
/// capture on but no harness in this checkout, that names both ways to record
/// Claude Code sessions, since the user-scoped plugin leaves nothing in the
/// checkout to tell it from not recording at all. The plugin takes two
/// commands in Claude Code (the marketplace has to be added before `cvc@volute`
/// can be installed), so they are given one to a line, ready to paste. Every
/// line fits an 80-column terminal.
pub fn empty_state(capture_acknowledged: bool, harness_installed: bool) -> &'static [&'static str] {
    if !capture_acknowledged {
        &[
            "No conversations captured yet.",
            "Passive capture is off. Turn it on with: cvc privacy acknowledge-capture",
        ]
    } else if !harness_installed {
        &[
            "No conversations captured yet.",
            "To record Claude Code sessions, run these in Claude Code:",
            "  /plugin marketplace add meirka8/volute",
            "  /plugin install cvc@volute",
            "or, for this checkout only, run in a shell: cvc harness install claude-code",
        ]
    } else {
        &[
            "No conversations captured yet.",
            "Conversations appear here after a session has been captured.",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::tree::tests::conversation;
    use cvc_core::db::ContextPathUse;
    use cvc_core::sync::ProjectionPublication;

    const FP: &str = "8348b79e2c41aabbccddeeff00112233445566778899aabbccddeeff00112233";
    /// 2026-10-13 09:12 UTC and 2026-10-14 18:40 UTC.
    const FIRST: i64 = 1_791_882_720;
    const LAST: i64 = 1_792_003_200;
    const U: Glyphs = Glyphs::UNICODE;
    const A: Glyphs = Glyphs::ASCII;

    fn overview(name: &str) -> RemoteOverview {
        RemoteOverview {
            name: name.to_owned(),
            fingerprint: Ok(FP.to_owned()),
            location: Some("github.com/acme/repo".to_owned()),
            sharing_consented: true,
            auto_push: false,
            consent_error: None,
        }
    }

    /// A conversation with `thoughts` thoughts, `shared` of them shared and
    /// `published` of those published.
    fn state(thoughts: i64, shared: i64, published: i64) -> ConversationSummary {
        let mut s = conversation("c", 0, 0, thoughts);
        s.shared_count = shared;
        s.shared = shared > 0;
        s.published = published;
        // What a shared thought has published is held by the remote.
        s.held = published;
        s.held_published = published;
        s
    }

    /// A conversation of `thoughts` thoughts, none shared from here, that the
    /// remote holds `published` of for certain and `unconfirmed` of perhaps (a
    /// publish that did not conclude).
    fn held(thoughts: i64, published: i64, unconfirmed: i64) -> ConversationSummary {
        let mut s = conversation("c", 0, 0, thoughts);
        s.held = published + unconfirmed;
        s.held_published = published;
        s
    }

    fn label(s: &ConversationSummary) -> String {
        state_label(s, true, &U).0
    }

    #[test]
    fn the_state_label_table() {
        // S=0: private.
        assert_eq!(label(&state(27, 0, 0)), "○ private");
        // W>0, S=T: shared, waiting.
        assert_eq!(label(&state(27, 27, 25)), "◐ shared · 2 waiting");
        assert_eq!(label(&state(27, 27, 0)), "◐ shared · 27 waiting");
        // W=0, S=T: published.
        assert_eq!(label(&state(27, 27, 27)), "● published");
        // W=0, S<T: published with a private tail.
        assert_eq!(label(&state(27, 21, 21)), "● published · 6 private");
        // W>0, S<T: shared, waiting, and the private tail.
        assert_eq!(
            label(&state(27, 21, 19)),
            "◐ shared · 2 waiting · 6 private"
        );
    }

    /// A conversation of `thoughts` thoughts, `shared` of them shared, of which
    /// `published` are published and `unconfirmed` are uncertain, and
    /// `held_unshared` more that the remote holds with no share from here.
    fn beside_a_share(
        thoughts: i64,
        shared: i64,
        published: i64,
        unconfirmed: i64,
        held_unshared: i64,
    ) -> ConversationSummary {
        let mut s = state(thoughts, shared, published);
        s.unconfirmed = unconfirmed;
        s.held = published + unconfirmed + held_unshared;
        s.held_published = published + held_unshared;
        s
    }

    #[test]
    fn thoughts_the_remote_holds_beside_a_share_are_not_counted_as_private() {
        // 27 thoughts, 21 shared and 19 of those published; of the 6 that are
        // not shared, 4 are held by the remote all the same.
        let s = beside_a_share(27, 21, 19, 0, 4);
        assert_eq!(
            label(&s),
            "◐ shared · 2 waiting · 4 not shared here · 2 private"
        );
        assert_eq!(
            state_label(&s, true, &A).0,
            "~ shared - 2 waiting - 4 not shared here - 2 private"
        );
        // All 6 held: nothing is private. None held: all 6 are, as ever.
        assert_eq!(
            label(&beside_a_share(27, 21, 21, 0, 6)),
            "● published · 6 not shared here"
        );
        assert_eq!(
            label(&beside_a_share(27, 21, 21, 0, 0)),
            "● published · 6 private"
        );
        // Held by a shared thought whose publish did not conclude is not
        // "not shared here": it is shared, and counted with the shared ones.
        assert_eq!(
            label(&beside_a_share(10, 4, 1, 2, 3)),
            "◐ shared · 3 waiting · 3 not shared here · 3 private"
        );
        // `+future` follows the tails.
        let mut s = beside_a_share(27, 21, 21, 0, 4);
        s.share_future = Some(true);
        assert_eq!(
            label(&s),
            "● published · 4 not shared here · 2 private +future"
        );
        // The tone follows the shared ones, as ever.
        assert_eq!(state_label(&s, true, &U).1, Tone::Published);
        // Numbers that cannot be (more held than there are thoughts not shared)
        // never make the tails add up to more than the thoughts.
        let mut odd = beside_a_share(5, 5, 3, 0, 0);
        odd.held = 9;
        assert_eq!(label(&odd), "◐ shared · 2 waiting");
    }

    #[test]
    fn private_counts_exactly_the_thoughts_the_remote_holds_none_of() {
        for thoughts in 1..=5 {
            for shared in 1..=thoughts {
                for published in 0..=shared {
                    for unconfirmed in 0..=(shared - published) {
                        for held_unshared in 0..=(thoughts - shared) {
                            let summary = beside_a_share(
                                thoughts,
                                shared,
                                published,
                                unconfirmed,
                                held_unshared,
                            );
                            let private = thoughts - shared - held_unshared;
                            let mut tails = String::new();
                            if held_unshared > 0 {
                                tails.push_str(&format!(" · {held_unshared} not shared here"));
                            }
                            if private > 0 {
                                tails.push_str(&format!(" · {private} private"));
                            }
                            let said = label(&summary);
                            assert!(
                                said.ends_with(&tails),
                                "{thoughts} thoughts, {shared} shared, {published} published, {unconfirmed} unconfirmed, {held_unshared} held: {said:?} should end with {tails:?}"
                            );
                            // The label before the tails has none of them in it.
                            let head = &said[..said.len() - tails.len()];
                            assert!(!head.contains("not shared here"), "{said:?}");
                            assert!(!head.contains("private"), "{said:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn private_never_stands_alone_for_a_conversation_with_a_published_thought() {
        for (t, s, p) in [(27, 21, 21), (27, 21, 5), (3, 1, 1), (2, 2, 1)] {
            let text = label(&state(t, s, p));
            assert!(!text.starts_with("○ private"), "{t}/{s}/{p} reads {text:?}");
        }
    }

    #[test]
    fn future_sharing_is_appended_to_every_form() {
        let mut s = state(27, 27, 27);
        s.share_future = Some(true);
        assert_eq!(label(&s), "● published +future");
        let mut s = state(27, 21, 19);
        s.share_future = Some(true);
        assert_eq!(label(&s), "◐ shared · 2 waiting · 6 private +future");
        let mut s = state(27, 27, 27);
        s.share_future = Some(false);
        assert_eq!(label(&s), "● published");
        // With nothing shared yet there is no label to append it to; the label
        // itself says it (see the `future only` test).
        let mut s = state(27, 0, 0);
        s.share_future = Some(true);
        assert_eq!(label(&s), "◐ shared · future only");
        // A row the remote holds keeps its own label and the suffix.
        let mut s = held(27, 3, 0);
        s.share_future = Some(true);
        assert_eq!(label(&s), "● published · not shared here +future");
    }

    #[test]
    fn later_thoughts_shared_with_nothing_shared_yet_read_future_only() {
        let mut s = state(5, 0, 0);
        s.share_future = Some(true);
        assert_eq!(label(&s), "◐ shared · future only");
        assert_eq!(state_label(&s, true, &U).1, Tone::Waiting);
        assert_eq!(state_label(&s, true, &A).0, "~ shared - future only");
        // Not future: private. Unknown: private. Without a destination: a dash.
        s.share_future = Some(false);
        assert_eq!(label(&s), "○ private");
        s.share_future = None;
        assert_eq!(label(&s), "○ private");
        s.share_future = Some(true);
        assert_eq!(state_label(&s, false, &U).0, "–");
        // An imported conversation is imported, whatever else is set.
        s.imported = true;
        assert_eq!(label(&s), "imported");
    }

    #[test]
    fn an_imported_conversation_that_is_not_shared_reads_imported() {
        let mut s = state(5, 0, 0);
        s.imported = true;
        assert_eq!(label(&s), "imported");
        assert_eq!(state_label(&s, false, &U).0, "imported");
        // Once shared from here it reads like any other.
        let mut shared = state(5, 5, 0);
        shared.imported = true;
        assert_eq!(label(&shared), "◐ shared · 5 waiting");
    }

    #[test]
    fn without_a_destination_the_state_is_a_dash() {
        let (text, tone) = state_label(&state(5, 0, 0), false, &U);
        assert_eq!((text.as_str(), tone), ("–", Tone::Neutral));
        assert_eq!(state_label(&state(5, 0, 0), false, &A).0, "-");
    }

    #[test]
    fn state_labels_fall_back_to_ascii() {
        assert_eq!(state_label(&state(27, 0, 0), true, &A).0, "o private");
        assert_eq!(
            state_label(&state(27, 21, 19), true, &A).0,
            "~ shared - 2 waiting - 6 private"
        );
        assert_eq!(state_label(&state(27, 27, 27), true, &A).0, "* published");
        for case in [state(27, 21, 19), state(27, 27, 27), state(1, 0, 0)] {
            assert!(state_label(&case, true, &A).0.is_ascii());
        }
    }

    #[test]
    fn tones_follow_the_state() {
        assert_eq!(state_label(&state(5, 0, 0), true, &U).1, Tone::Private);
        assert_eq!(state_label(&state(5, 5, 3), true, &U).1, Tone::Waiting);
        assert_eq!(state_label(&state(5, 5, 5), true, &U).1, Tone::Published);
        assert_eq!(state_label(&state(5, 3, 3), true, &U).1, Tone::Published);
    }

    fn agent(shared: i64, published: i64) -> ConversationSummary {
        state(4, shared, published)
    }

    #[test]
    fn the_badge_names_the_most_pressing_state_of_the_agents() {
        let waiting = agent(4, 1);
        let private = agent(0, 0);
        let published = agent(4, 4);
        assert_eq!(
            agents_badge(&[&waiting, &private], true, &U),
            "+2 agents · 1 waiting"
        );
        assert_eq!(
            agents_badge(&[&private, &private, &published], true, &U),
            "+3 agents · 2 private"
        );
        assert_eq!(
            agents_badge(&[&published, &published], true, &U),
            "+2 agents · 2 published"
        );
        assert_eq!(agents_badge(&[&private], true, &U), "+1 agent · 1 private");
        // Priority is waiting, then private, then published.
        assert_eq!(
            agents_badge(&[&waiting, &private, &published], true, &U),
            "+3 agents · 1 waiting"
        );
        assert_eq!(
            agents_badge(&[&waiting, &private], true, &A),
            "+2 agents - 1 waiting"
        );
    }

    #[test]
    fn the_badge_has_no_state_without_a_destination_or_for_imported_agents() {
        let private = agent(0, 0);
        assert_eq!(agents_badge(&[&private, &private], false, &U), "+2 agents");
        let mut imported = agent(0, 0);
        imported.imported = true;
        assert_eq!(agents_badge(&[&imported], true, &U), "+1 agent");
    }

    #[test]
    fn times_are_utc_and_can_be_shortened() {
        // 2026-10-14 09:02:30 UTC.
        let at = 1_791_968_550;
        assert_eq!(format_when(at, false), "2026-10-14 09:02");
        assert_eq!(format_when(at, true), "10-14 09:02");
        assert_eq!(format_when(i64::MAX, false), "-");
    }

    #[test]
    fn activity_has_a_form_for_one_minute_one_day_and_many_days() {
        let start = 1_791_968_550; // 2026-10-14 09:02 UTC
        assert_eq!(activity(start, start + 5, &U), "2026-10-14 09:02 UTC");
        assert_eq!(
            activity(start, start + 9 * 3600 + 38 * 60, &U),
            "2026-10-14 09:02 – 18:40 UTC"
        );
        assert_eq!(
            activity(start - 86_400, start + 9 * 3600 + 38 * 60, &U),
            "2026-10-13 09:02 – 10-14 18:40 UTC"
        );
        assert_eq!(
            activity(start - 400 * 86_400, start, &U),
            "2025-09-09 09:02 – 2026-10-14 09:02 UTC"
        );
        assert_eq!(
            activity(start, start + 9 * 3600, &A),
            "2026-10-14 09:02 - 18:02 UTC"
        );
    }

    fn header_of(remote: &RemoteView, waiting: Option<i64>) -> HeaderText {
        header(remote, waiting, &U)
    }

    fn first(line: &[Seg]) -> String {
        plain_text(line)
    }

    #[test]
    fn the_header_of_an_authorized_remote() {
        let overview = overview("origin");
        let h = header_of(&RemoteView::Usable(&overview), Some(2));
        assert_eq!(
            h.remote,
            vec![
                "remote: origin -> github.com/acme/repo (8348b79e2c41…)",
                "remote: origin -> github.com/acme/repo",
                "remote: origin",
            ]
        );
        assert_eq!(
            first(&h.publishing[0]),
            "publishing: authorized in this clone   auto-push: off"
        );
        assert_eq!(
            first(&h.publishing[1]),
            "publishing: authorized   auto-push: off"
        );
        assert_eq!(first(&h.publishing[2]), "publishing: authorized");
        assert_eq!(h.waiting[0], "2 shared thoughts waiting to publish");
        assert_eq!(h.waiting[1], "2 waiting to publish");
    }

    #[test]
    fn a_remote_without_a_location_omits_it() {
        let mut overview = overview("origin");
        overview.location = None;
        let h = header_of(&RemoteView::Usable(&overview), Some(0));
        assert_eq!(
            h.remote,
            vec!["remote: origin (8348b79e2c41…)", "remote: origin"]
        );
        assert_eq!(h.waiting[0], "nothing waiting to publish");
    }

    #[test]
    fn the_waiting_count_has_a_singular() {
        let overview = overview("origin");
        let h = header_of(&RemoteView::Usable(&overview), Some(1));
        assert_eq!(h.waiting[0], "1 shared thought waiting to publish");
    }

    #[test]
    fn auto_push_is_the_only_loud_part_of_the_header() {
        let mut overview = overview("origin");
        overview.auto_push = true;
        let h = header_of(&RemoteView::Usable(&overview), Some(3));
        assert_eq!(
            first(&h.publishing[0]),
            "publishing: authorized in this clone   auto-push: ON - every git push to this remote also publishes shared thoughts"
        );
        let loud: Vec<_> = h.publishing[0]
            .iter()
            .filter(|seg| seg.emph == Emph::Loud)
            .collect();
        assert_eq!(loud.len(), 1);
        assert_eq!(
            loud[0].text,
            "auto-push: ON - every git push to this remote also publishes shared thoughts"
        );
        // A narrower form still says that it is every push, not the next one.
        assert_eq!(
            first(&h.publishing[1]),
            "publishing: authorized   auto-push: ON - every git push also publishes shared thoughts"
        );
        // Even the shortest form keeps the ON.
        assert_eq!(
            first(h.publishing.last().unwrap()),
            "publishing: authorized   auto-push: ON"
        );
        // And with auto-push off nothing is loud at all.
        let quiet = self::overview("origin");
        for line in header_of(&RemoteView::Usable(&quiet), Some(3)).publishing {
            assert!(line.iter().all(|seg| seg.emph != Emph::Loud));
        }
    }

    #[test]
    fn the_header_of_a_remote_that_is_not_authorized() {
        let mut overview = overview("origin");
        overview.sharing_consented = false;
        let h = header_of(&RemoteView::Usable(&overview), Some(0));
        assert_eq!(
            first(&h.publishing[0]),
            "publishing: not authorized yet (s or p will walk you through it)"
        );
        assert_eq!(first(&h.publishing[1]), "publishing: not authorized yet");
    }

    #[test]
    fn the_header_when_the_privacy_state_cannot_be_read() {
        let mut overview = overview("origin");
        overview.sharing_consented = false;
        overview.consent_error = Some("privacy state permissions are insecure".to_owned());
        let h = header_of(&RemoteView::Usable(&overview), Some(0));
        assert_eq!(
            first(&h.publishing[0]),
            "publishing: unknown (CVC privacy state cannot be read: privacy state permissions are insecure); sharing and publishing from this view are off."
        );
        assert_eq!(
            first(&h.publishing[1]),
            "publishing: unknown; sharing and publishing from this view are off."
        );
    }

    #[test]
    fn the_header_variants_for_remotes_that_cannot_be_used() {
        let unusable = overview("origin");
        let cases: Vec<(RemoteView, &str, &str)> = vec![
            (
                RemoteView::Missing {
                    name: "typo",
                    others_exist: true,
                },
                "remote: typo (not configured)",
                "Sharing and publishing need a configured remote. Press r to choose one.",
            ),
            (
                RemoteView::Missing {
                    name: "typo",
                    others_exist: false,
                },
                "remote: typo (not configured)",
                "Add a remote to share: git remote add origin <url>",
            ),
            (
                RemoteView::Unusable(&unusable),
                "remote: origin (no push URL)",
                "Sharing and publishing are off for this remote. Press r to choose another.",
            ),
            (
                RemoteView::NoneSelected,
                "remote: none selected (no remote named origin)",
                "Choose a remote to see share state. Press r.",
            ),
            (
                RemoteView::NoRemotes,
                "remote: none (no Git remotes)",
                "Add a remote to share: git remote add origin <url>",
            ),
        ];
        for (view, remote, line2) in cases {
            let h = header_of(&view, Some(4));
            assert_eq!(h.remote[0], remote);
            assert_eq!(first(&h.publishing[0]), line2);
            assert_eq!(view.problem().as_deref(), Some(line2));
            // Nothing to publish to, so nothing is said to wait.
            assert!(h.waiting.is_empty(), "{remote}");
        }
        assert_eq!(RemoteView::Usable(&unusable).problem(), None);
    }

    #[test]
    fn narrow_header_candidates_drop_parentheticals_first() {
        let overview = overview("origin");
        let h = header_of(&RemoteView::Usable(&overview), Some(2));
        let widths: Vec<_> = h.remote.iter().map(|c| text::width(c)).collect();
        assert!(
            widths.windows(2).all(|pair| pair[0] > pair[1]),
            "{widths:?}"
        );
        assert!(h.remote[1].starts_with("remote: origin -> "));
        assert!(!h.remote[1].contains('('));
    }

    #[test]
    fn remote_names_in_the_header_are_sanitized() {
        let overview = overview("or\u{1b}[2Jigin");
        let h = header_of(&RemoteView::Usable(&overview), Some(0));
        for candidate in &h.remote {
            assert!(!candidate.contains('\u{1b}'), "{candidate}");
        }
    }

    fn dest() -> Destination {
        Destination::of(&overview("origin"), &U)
    }

    fn lines(bar: &[Vec<Seg>]) -> Vec<String> {
        bar.iter().map(|line| plain_text(line)).collect()
    }

    fn ask(thoughts: i64, unshared: i64, future_on: bool) -> ShareAsk {
        ShareAsk {
            title: "Refactor the sync module".to_owned(),
            thoughts,
            unshared,
            future_on,
        }
    }

    #[test]
    fn the_share_bar_asks_about_this_snapshot_and_the_future() {
        let bar = share_bar(&ask(27, 6, false), &dest(), 200, &U);
        assert_eq!(
            lines(&bar),
            vec![
                "Share \"Refactor the sync module\" (27 thoughts, 6 not yet shared) for 'origin' (github.com/acme/repo)?",
                "Enter: this snapshot only   f: also future thoughts   Esc: cancel",
            ]
        );
    }

    #[test]
    fn the_share_bar_says_so_when_future_sharing_is_on() {
        let bar = share_bar(&ask(27, 6, true), &dest(), 200, &U);
        assert_eq!(
            lines(&bar)[1],
            "Enter: this snapshot only (stops sharing future thoughts)   f: keep sharing future thoughts   Esc: cancel"
        );
    }

    #[test]
    fn the_share_bar_warns_that_auto_push_publishes_future_thoughts() {
        let mut destination = dest();
        destination.auto_push = true;
        let bar = share_bar(&ask(27, 6, false), &destination, 300, &U);
        assert_eq!(
            lines(&bar)[1],
            "Enter: this snapshot only   f: also future thoughts (auto-push publishes them at each git push)   Esc: cancel"
        );
        let bar = share_bar(&ask(27, 6, true), &destination, 300, &U);
        assert!(lines(&bar)[1].contains(
            "f: keep sharing future thoughts (auto-push publishes them at each git push)"
        ));
    }

    #[test]
    fn a_fully_shared_conversation_offers_only_the_future() {
        let bar = share_bar(&ask(27, 0, false), &dest(), 200, &U);
        assert_eq!(
            lines(&bar),
            vec![
                "All 27 thoughts are already shared for 'origin'.",
                "f: also share future thoughts   Esc: cancel",
            ]
        );
        assert!(!lines(&bar)[1].contains("Enter"));
        let one = share_bar(&ask(1, 0, false), &dest(), 200, &U);
        assert_eq!(
            lines(&one)[0],
            "The thought is already shared for 'origin'."
        );
    }

    #[test]
    fn the_share_bar_cuts_the_title_before_the_destination() {
        let mut long = ask(27, 6, false);
        long.title =
            "A very long title that goes on and on about the whole refactoring of everything"
                .to_owned();
        // Where one line holds the question, only the title gives way.
        let bar = share_bar(&long, &dest(), 99, &U);
        let question = lines(&bar);
        assert_eq!(bar.len(), 2, "{question:?}");
        assert!(question[0]
            .contains("(27 thoughts, 6 not yet shared) for 'origin' (github.com/acme/repo)?"));
        assert!(question[0].contains('…'));
        assert!(text::width(&question[0]) <= 99);
    }

    #[test]
    fn a_narrow_share_bar_wraps_instead_of_dropping_the_destination() {
        let mut long = ask(27, 6, false);
        long.title =
            "A very long title that goes on and on about the whole refactoring of everything"
                .to_owned();
        for width in [59usize, 79] {
            let bar = lines(&share_bar(&long, &dest(), width, &U));
            let all = bar.join(" ");
            for part in [
                "27 thoughts",
                "6 not yet shared",
                "'origin'",
                "(github.com/acme/repo)?",
            ] {
                assert!(all.contains(part), "{width}: missing {part} in {bar:?}");
            }
            for line in &bar {
                assert!(text::width(line) <= width, "{width}: {line}");
            }
            // Every key is still on screen.
            let keys = bar.join(" ");
            assert!(keys.contains("Esc: cancel") && keys.contains("Enter:") && keys.contains("f:"));
        }
    }

    #[test]
    fn narrow_key_lines_shorten_then_pack_and_never_cut_a_key() {
        let mut auto = dest();
        auto.auto_push = true;
        // 59 columns: the long form does not fit one line, the short one does.
        let bar = lines(&share_bar(&ask(27, 6, false), &dest(), 59, &U));
        assert_eq!(
            bar.last().unwrap(),
            "Enter: snapshot only   f: also future   Esc: cancel"
        );
        // With auto-push on the warning survives, in its short form.
        let bar = lines(&share_bar(&ask(27, 6, false), &auto, 59, &U));
        let keys = bar.iter().skip(1).cloned().collect::<Vec<_>>().join(" | ");
        assert!(
            keys.contains("f: also future (auto-push publishes them)"),
            "{keys}"
        );
        assert!(keys.contains("Esc: cancel"), "{keys}");
        for line in &bar {
            assert!(text::width(line) <= 59, "{line}");
        }
        // With future sharing already on, the keys pack into several lines.
        let bar = lines(&share_bar(&ask(27, 6, true), &auto, 45, &U));
        let keys = bar.join(" | ");
        assert!(
            keys.contains("Esc: cancel") && keys.contains("auto-push publishes them"),
            "{keys}"
        );
        for line in &bar {
            assert!(text::width(line) <= 45, "{line}");
        }
        // Below any supported width a key that cannot fit a line wraps, whole.
        let bar = lines(&share_bar(&ask(27, 6, true), &auto, 30, &U));
        assert!(
            bar.join(" ").contains("(auto-push publishes them)"),
            "{bar:?}"
        );
        for line in &bar {
            assert!(text::width(line) <= 30, "{line}");
        }
    }

    #[test]
    fn the_other_bars_wrap_on_a_narrow_terminal() {
        for bar in [
            unshare_bar("Refactor the sync module", 21, &dest(), 59, &U),
            publish_bar(2, &dest(), 59),
            authorize_bar(&dest(), 59),
        ] {
            let bar = lines(&bar);
            assert!(bar.len() >= 2 && bar.len() <= 3, "{bar:?}");
            for line in &bar {
                assert!(text::width(line) <= 59, "{line}");
            }
            assert!(bar.join(" ").contains("Esc: cancel"));
        }
        let unshare = lines(&unshare_bar(
            "Refactor the sync module",
            21,
            &dest(),
            59,
            &U,
        ))
        .join(" ");
        assert!(
            unshare.contains("Its 21 shared thoughts become private again."),
            "{unshare}"
        );
        let authorize = lines(&authorize_bar(&dest(), 59)).join(" ");
        assert!(
            authorize.contains("authorize (type a phrase)")
                || authorize.contains("authorize (you will type a phrase)")
        );
    }

    #[test]
    fn the_unshare_bar_counts_what_becomes_private() {
        let bar = unshare_bar("Refactor the sync module", 6, &dest(), 200, &U);
        assert_eq!(
            lines(&bar),
            vec![
                "Unshare \"Refactor the sync module\" for 'origin'? Its 6 shared thoughts become private again.",
                "Enter: continue to the phrase   Esc: cancel",
            ]
        );
        let one = unshare_bar("T", 1, &dest(), 200, &U);
        assert!(lines(&one)[0].ends_with("Its 1 shared thought becomes private again."));
    }

    #[test]
    fn the_publish_bar_names_the_remote_and_the_waiting_count() {
        let bar = publish_bar(2, &dest(), 200);
        assert_eq!(
            lines(&bar),
            vec![
                "Publish shared thoughts to 'origin' (github.com/acme/repo)? 2 waiting by this clone's count.",
                "Enter: continue   Esc: cancel",
            ]
        );
        let none = publish_bar(0, &dest(), 200);
        assert_eq!(
            lines(&none),
            vec![
                "Nothing is waiting by this clone's count. Check 'origin' anyway?",
                "Enter / Esc",
            ]
        );
    }

    #[test]
    fn the_authorization_offer_names_the_exact_destination() {
        let bar = authorize_bar(&dest(), 200);
        assert_eq!(
            lines(&bar),
            vec![
                "Publishing to 'origin' (github.com/acme/repo) is not authorized in this clone yet.",
                "Enter: authorize (you will type a phrase)   Esc: cancel",
            ]
        );
    }

    #[test]
    fn a_blocked_bar_is_the_reason_and_the_way_out() {
        let bar = blocked_bar(IMPORTED_IS_READ_ONLY, 100);
        assert_eq!(
            lines(&bar),
            vec!["Imported from a remote, so it is read-only here.   any key: ok"]
        );
        // A reason too long for one line wraps and keeps the hint.
        let wrapped = blocked_bar(NOT_ON_THIS_PLATFORM, 60);
        assert!(wrapped.len() <= 3);
        assert!(lines(&wrapped).last().unwrap().ends_with("any key: ok"));
        for line in lines(&wrapped) {
            assert!(text::width(&line) <= 60, "{line}");
        }
        let all = lines(&wrapped).join(" ");
        assert!(all.contains("cvc push --manual"), "{all}");
    }

    #[test]
    fn the_reason_a_platform_cannot_share_survives_whole_on_the_narrowest_bar() {
        // The reason is what the person is told instead of a phrase to type, and
        // its end says why the commands it points to are not the same thing: none
        // of it may be cut off, down to the 60 columns of the smallest terminal
        // (and the one column the bar keeps free).
        assert_eq!(
            NOT_ON_THIS_PLATFORM,
            "Not available on this platform yet. Use cvc share, cvc unshare or cvc push --manual; on this platform they cannot discard keys typed ahead of the phrase."
        );
        for width in [59, 60, 70, 80, 100] {
            let bar = lines(&blocked_bar(NOT_ON_THIS_PLATFORM, width));
            assert!(bar.len() <= 3, "{width}: {bar:?}");
            let said = bar
                .join(" ")
                .trim_end()
                .trim_end_matches("any key: ok")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(said, NOT_ON_THIS_PLATFORM, "{width}: {bar:?}");
            assert!(bar.last().unwrap().ends_with("any key: ok"), "{width}");
        }
    }

    #[test]
    fn unshare_refusals_name_the_reason() {
        let published = UnshareCheck {
            published: 21,
            pending: 0,
            unknown: 0,
        };
        assert_eq!(
            unshare_refusal(&published, "origin").unwrap(),
            "Cannot unshare: 21 thoughts are already published to 'origin', and published thoughts cannot be recalled."
        );
        let one = UnshareCheck {
            published: 1,
            ..Default::default()
        };
        assert!(unshare_refusal(&one, "origin")
            .unwrap()
            .starts_with("Cannot unshare: 1 thought is already published"));
        let unfinished = UnshareCheck {
            published: 0,
            pending: 2,
            unknown: 1,
        };
        assert_eq!(
            unshare_refusal(&unfinished, "origin").unwrap(),
            "Cannot unshare yet: an earlier publish to 'origin' did not finish cleanly. Run: cvc privacy reconcile --remote origin"
        );
        assert_eq!(unshare_refusal(&UnshareCheck::default(), "origin"), None);
    }

    #[test]
    fn a_share_is_reported_with_what_comes_next() {
        assert_eq!(
            shared_status(27, "origin", FutureSharePolicy::Private, false, 0, &U),
            "27 thoughts marked shared for remote 'origin'; future thoughts are private. Not published yet (p)."
        );
        assert_eq!(
            shared_status(1, "origin", FutureSharePolicy::Shared, true, 0, &U),
            "1 thought marked shared for remote 'origin'; future thoughts are shared too. Auto-push is on: every git push to 'origin' publishes them."
        );
    }

    #[test]
    fn sharing_a_parent_says_once_that_its_agents_prompts_and_results_go_with_it() {
        assert_eq!(
            shared_status(27, "origin", FutureSharePolicy::Private, false, 2, &U),
            "27 thoughts marked shared for remote 'origin'; future thoughts are private. Not published yet (p). 2 agent conversations are not shared from here, but this session's thoughts include their prompts and results (→ to see)."
        );
        assert!(
            shared_status(27, "origin", FutureSharePolicy::Private, false, 1, &A).ends_with(
                " 1 agent conversation is not shared from here, but this session's thoughts include its prompt and result (-> to see)."
            )
        );
        assert!(
            !shared_status(27, "origin", FutureSharePolicy::Private, false, 0, &U)
                .contains("agent")
        );
    }

    #[test]
    fn an_unshare_is_reported_by_count() {
        assert_eq!(
            unshared_status(6, "origin"),
            "6 thoughts made private again for remote 'origin'."
        );
        assert_eq!(
            unshared_status(1, "origin"),
            "1 thought made private again for remote 'origin'."
        );
        assert_eq!(
            unshared_status(0, "origin"),
            "Nothing was shared for remote 'origin', so nothing changed."
        );
    }

    fn publication(new: usize, total: usize, others: [usize; 5]) -> PublishOutcome {
        PublishOutcome::Published(ProjectionPublication {
            new_ids: (0..new)
                .map(|_| cvc_core::models::InteractionId::new())
                .collect(),
            shared_total: total,
            new_link_records: others[0],
            new_tombstones: others[1],
            new_derivation_records: others[2],
            removed_interactions: others[3],
            removed_records: others[4],
        })
    }

    #[test]
    fn a_publish_is_reported_in_the_views_words() {
        assert_eq!(
            published_status(&publication(21, 42, [0; 5]), "origin"),
            "Published 21 new shared thoughts to 'origin' (42 shared in total)."
        );
        assert_eq!(
            published_status(&publication(1, 42, [0; 5]), "origin"),
            "Published 1 new shared thought to 'origin' (42 shared in total)."
        );
        // A first publish does not repeat the count as a total.
        assert_eq!(
            published_status(&publication(21, 21, [0; 5]), "origin"),
            "Published 21 new shared thoughts to 'origin'."
        );
        assert_eq!(
            published_status(&PublishOutcome::NothingNew { shared_total: 42 }, "origin"),
            "No new shared thoughts to publish to 'origin' (42 shared in total)."
        );
        assert_eq!(
            published_status(&PublishOutcome::NothingNew { shared_total: 0 }, "origin"),
            "No new shared thoughts to publish to 'origin'."
        );
    }

    #[test]
    fn a_publish_of_other_records_says_what_it_carried() {
        assert_eq!(
            published_status(&publication(0, 42, [3, 1, 0, 0, 0]), "origin"),
            "Published to 'origin': 3 commit links and 1 tombstone."
        );
        assert_eq!(
            published_status(&publication(5, 9, [1, 0, 2, 0, 0]), "origin"),
            "Published 5 new shared thoughts to 'origin' (9 shared in total), plus 1 commit link and 2 derivation records."
        );
        assert_eq!(
            published_status(&publication(0, 40, [0, 0, 0, 2, 0]), "origin"),
            "Published to 'origin': the removal of 2 tombstoned thoughts from the current tree (not erased from history)."
        );
        // A removal is suppression, not erasure, in each of its forms.
        for (shape, removal) in [
            (
                [0, 1, 0, 1, 2],
                "1 tombstone and the removal of 1 tombstoned thought and 2 attached records from the current tree",
            ),
            (
                [0, 0, 0, 0, 3],
                "the removal of 3 records of tombstoned thoughts from the current tree",
            ),
        ] {
            assert_eq!(
                published_status(&publication(0, 41, shape), "origin"),
                format!("Published to 'origin': {removal} (not erased from history).")
            );
        }
        assert_eq!(
            published_status(&publication(0, 42, [0; 5]), "origin"),
            "Published a format update to 'origin'; no new thoughts, links or tombstones."
        );
    }

    #[test]
    fn the_views_publish_text_never_says_interaction_or_turn() {
        let shapes = [
            publication(21, 42, [0; 5]),
            publication(0, 42, [3, 1, 2, 2, 4]),
            publication(2, 9, [3, 1, 2, 1, 4]),
            publication(0, 40, [0, 0, 0, 0, 3]),
            publication(0, 42, [0; 5]),
            PublishOutcome::NothingNew { shared_total: 3 },
            PublishOutcome::Skipped,
        ];
        for outcome in &shapes {
            let said = published_status(outcome, "origin");
            for word in ["interaction", "turn"] {
                assert!(!said.to_lowercase().contains(word), "{said}");
            }
        }
    }

    #[test]
    fn failures_are_worded_by_kind_and_never_by_the_raw_error() {
        let say = |kind, failure: FlowFailure| failure_status(kind, &failure, "origin");
        assert_eq!(
            say(FlowKind::Share, FlowFailure::NotConfirmed),
            "Not shared: the typed phrase did not match. Nothing was changed."
        );
        assert_eq!(
            say(FlowKind::Unshare, FlowFailure::NotConfirmed),
            "Not unshared: the typed phrase did not match. Nothing was changed."
        );
        assert_eq!(
            say(FlowKind::Publish, FlowFailure::NotConfirmed),
            "Not published: the typed phrase did not match. Nothing was published to 'origin'."
        );
        assert_eq!(
            say(FlowKind::Authorize, FlowFailure::NotConfirmed),
            "Not authorized: the typed phrase did not match."
        );
        assert_eq!(
            say(FlowKind::Share, FlowFailure::ConversationChanged),
            "Not shared: this conversation gained new thoughts while you were confirming. Press s to try again."
        );
        assert_eq!(
            say(FlowKind::Unshare, FlowFailure::UnshareBlocked),
            "Cannot unshare: some of this conversation is published, or a publish of it did not finish, for 'origin'."
        );
        assert_eq!(
            say(FlowKind::Publish, FlowFailure::ConsentMissing),
            "Publishing to 'origin' is not authorized yet. Press p to set it up."
        );
    }

    #[test]
    fn a_challenge_that_was_never_asked_does_not_read_as_a_phrase_that_did_not_match() {
        let say = |kind, failure: FlowFailure| failure_status(kind, &failure, "origin");
        assert_eq!(
            say(FlowKind::Share, FlowFailure::InputKeptArriving),
            "Not shared: input kept arriving at the terminal, so no phrase was asked. Nothing was changed."
        );
        assert_eq!(
            say(FlowKind::Unshare, FlowFailure::InputKeptArriving),
            "Not unshared: input kept arriving at the terminal, so no phrase was asked. Nothing was changed."
        );
        assert_eq!(
            say(FlowKind::Publish, FlowFailure::InputKeptArriving),
            "Not published: input kept arriving at the terminal, so no phrase was asked. Nothing was published to 'origin'."
        );
        assert_eq!(
            say(FlowKind::Authorize, FlowFailure::InputKeptArriving),
            "Not authorized: input kept arriving at the terminal, so no phrase was asked."
        );
        assert_eq!(
            say(FlowKind::Share, FlowFailure::TerminalNotRestored),
            "Not shared: the terminal could not be put back to normal input, so no phrase was asked. Nothing was changed."
        );
        assert_eq!(
            say(FlowKind::Authorize, FlowFailure::TerminalNotRestored),
            "Not authorized: the terminal could not be put back to normal input, so no phrase was asked."
        );
        // Whatever the kind, one that was not asked never says a phrase did not
        // match, and one that was asked always says so.
        for kind in [
            FlowKind::Share,
            FlowKind::Unshare,
            FlowKind::Publish,
            FlowKind::Authorize,
        ] {
            for not_asked in [
                FlowFailure::InputKeptArriving,
                FlowFailure::TerminalNotRestored,
            ] {
                let said = say(kind, not_asked.clone());
                assert!(said.contains("no phrase was asked"), "{kind:?}: {said}");
                assert!(!said.contains("did not match"), "{kind:?}: {said}");
                assert!(!said.contains("typed phrase"), "{kind:?}: {said}");
            }
            let said = say(kind, FlowFailure::NotConfirmed);
            assert!(
                said.contains("the typed phrase did not match"),
                "{kind:?}: {said}"
            );
            assert!(!said.contains("no phrase was asked"), "{kind:?}: {said}");
        }
        // Inside a share that was bundled with a publish, the same.
        assert_eq!(
            say(
                FlowKind::Share,
                FlowFailure::SharedButNotPublished(Box::new(FlowFailure::InputKeptArriving))
            ),
            "The share was recorded, but publishing did not complete. Not published: input kept arriving at the terminal, so no phrase was asked. Nothing was published to 'origin'."
        );
    }

    #[test]
    fn a_publish_whose_outcome_is_unknown_says_the_remote_may_hold_some_of_it() {
        let unknown = FlowFailure::OutcomeUnknown(
            "Git error: connection reset by peer\nsecond line\u{1b}[2J".to_owned(),
        );
        let said = failure_status(FlowKind::Publish, &unknown, "origin");
        assert_eq!(
            said,
            "Publish to 'origin' did not complete; the remote may hold some of it. Run: cvc privacy reconcile --remote origin (error: Git error: connection reset by peer)"
        );
        // The words are the ones every other unfinished publish uses.
        assert!(said.starts_with(&publish_unfinished("origin")));
        assert!(!said.to_lowercase().contains("nothing was sent"));
        // Control characters never reach the notice.
        let escaped = failure_status(
            FlowKind::Publish,
            &FlowFailure::OutcomeUnknown("bad\u{1b}[2J".to_owned()),
            "origin",
        );
        assert!(escaped.ends_with("(error: bad?[2J)"), "{escaped}");
        // Whatever the kind, an unsettled outcome is never worded as a refusal.
        for kind in [FlowKind::Share, FlowKind::Unshare, FlowKind::Authorize] {
            assert!(failure_status(kind, &unknown, "origin").contains("the remote may hold some"));
        }
    }

    #[test]
    fn a_share_whose_publication_stopped_never_reads_as_nothing_recorded() {
        let bundled = |inner| FlowFailure::SharedButNotPublished(Box::new(inner));
        for (inner, ending) in [
            (
                FlowFailure::NotConfirmed,
                "Not published: the typed phrase did not match. Nothing was published to 'origin'.",
            ),
            (
                FlowFailure::ConsentMissing,
                "Publishing to 'origin' is not authorized yet. Press p to set it up.",
            ),
            (
                FlowFailure::OutcomeUnknown("reset".to_owned()),
                "Publish to 'origin' did not complete; the remote may hold some of it. Run: cvc privacy reconcile --remote origin (error: reset)",
            ),
        ] {
            let said = failure_status(FlowKind::Share, &bundled(inner), "origin");
            assert_eq!(
                said,
                format!("The share was recorded, but publishing did not complete. {ending}")
            );
            assert!(!said.contains("Nothing was changed"), "{said}");
            assert!(!said.starts_with("Not shared"), "{said}");
        }
    }

    #[test]
    fn another_failure_shows_its_first_line_sanitized() {
        let failure = FlowFailure::Other("disk is full\u{1b}[2J\nsecond line".to_owned());
        assert_eq!(
            failure_status(FlowKind::Unshare, &failure, "origin"),
            "Unshare failed: disk is full?[2J"
        );
        assert_eq!(
            failure_status(
                FlowKind::Share,
                &FlowFailure::Other("x".to_owned()),
                "origin"
            ),
            "Share failed: x"
        );
        assert_eq!(
            failure_status(
                FlowKind::Authorize,
                &FlowFailure::Other("x".to_owned()),
                "o"
            ),
            "Authorization failed: x"
        );
    }

    #[test]
    fn the_share_banner_says_where_what_and_how_to_back_out() {
        let lines = banner(
            FlowKind::Share,
            &dest(),
            Some(FutureSharePolicy::Private),
            &U,
        );
        assert_eq!(lines[0], format!("-- cvc: share {}", "-".repeat(46)));
        assert_eq!(
            lines[1],
            "Remote   'origin' -> github.com/acme/repo  (fingerprint 8348b79e2c41…)"
        );
        assert_eq!(
            lines[2],
            "Effect   Marks this conversation's thoughts as shared for 'origin'. CVC sends nothing to 'origin' yet."
        );
        assert_eq!(lines[3], "Future   Thoughts added later stay private.");
        assert_eq!(lines[4], "         An empty line and Enter cancels.");
        assert_eq!(lines.len(), 5);
        let shared = banner(
            FlowKind::Share,
            &dest(),
            Some(FutureSharePolicy::Shared),
            &U,
        );
        assert_eq!(shared[3], "Future   Thoughts added later are shared too.");
    }

    #[test]
    fn the_share_banner_warns_when_auto_push_will_publish() {
        let mut destination = dest();
        destination.auto_push = true;
        let lines = banner(
            FlowKind::Share,
            &destination,
            Some(FutureSharePolicy::Private),
            &U,
        );
        assert!(lines[2].ends_with(
            "CVC sends nothing to 'origin' yet. Auto-push is on: every git push to 'origin', including one an editor runs, publishes them."
        ));
    }

    #[test]
    fn the_other_banners_have_their_own_effect() {
        let unshare = banner(FlowKind::Unshare, &dest(), None, &U);
        assert_eq!(unshare[0], format!("-- cvc: unshare {}", "-".repeat(44)));
        assert_eq!(
            unshare[2],
            "Effect   Makes this conversation private again for 'origin'. Only possible while none of it is published."
        );
        assert!(!unshare.iter().any(|line| line.starts_with("Future")));

        let publish = banner(FlowKind::Publish, &dest(), None, &U);
        assert_eq!(
            publish[2],
            "Effect   Sends the shared thoughts to 'origin', where anyone who can read it can read them. Published thoughts cannot be recalled, and scrubbing can miss secrets."
        );
        // The summary the command prints below counts thoughts too, so the
        // banner has no note about another name for them.
        assert_eq!(publish[3], "         An empty line and Enter cancels.");
        assert_eq!(publish.last().unwrap(), "Checking 'origin' first…");
        assert!(!publish.join("\n").contains("interaction"));
        // Only a publish says the thoughts cannot be recalled.
        for kind in [FlowKind::Share, FlowKind::Unshare, FlowKind::Authorize] {
            let text = banner(kind, &dest(), None, &U).join("\n");
            assert!(!text.contains("cannot be recalled"), "{kind:?}");
        }

        let authorize = banner(FlowKind::Authorize, &dest(), None, &U);
        assert!(authorize[0].starts_with("-- cvc: authorize publishing "));
        assert_eq!(
            authorize[2],
            "Effect   Allows publishing to 'origin' at this exact push URL; you are asked again if the URL changes. CVC checks only the URL, not who owns the repository or who can read it. Publishes nothing and does not turn on auto-push."
        );
    }

    #[test]
    fn a_banner_without_a_location_omits_it_and_falls_back_to_ascii() {
        let mut destination = dest();
        destination.location = None;
        let lines = banner(FlowKind::Share, &destination, None, &A);
        assert_eq!(lines[1], "Remote   'origin'  (fingerprint 8348b79e2c41...)");
        assert!(banner(FlowKind::Publish, &destination, None, &A)
            .join("\n")
            .is_ascii());
    }

    #[test]
    fn a_publish_that_failed_after_the_challenge_never_says_nothing_was_sent() {
        let failure = FlowFailure::Other("connection reset by peer\nsecond line".to_owned());
        let said = failure_status(FlowKind::Publish, &failure, "origin");
        assert_eq!(
            said,
            "Publish to 'origin' did not complete; the remote may hold some of it. Run: cvc privacy reconcile --remote origin (error: connection reset by peer)"
        );
        assert!(!said.to_lowercase().contains("nothing was sent"));
        assert_eq!(
            publish_unfinished("origin"),
            "Publish to 'origin' did not complete; the remote may hold some of it. Run: cvc privacy reconcile --remote origin"
        );
        // Only a typed refusal at the challenge, before anything is pushed, may say
        // so. (The fetch that comes first did reach the remote, which is why it
        // says nothing was published and not that nothing was sent.)
        assert!(
            failure_status(FlowKind::Publish, &FlowFailure::NotConfirmed, "origin")
                .contains("Nothing was published to 'origin'.")
        );
    }

    #[test]
    fn a_row_the_remote_holds_is_never_labeled_private() {
        // Nothing shared from here, but the remote holds some of it for certain.
        let held_only = held(5, 3, 0);
        assert_eq!(label(&held_only), "● published · not shared here");
        assert_eq!(state_label(&held_only, true, &U).1, Tone::Published);
        let mut future = held_only.clone();
        future.share_future = Some(true);
        assert_eq!(label(&future), "● published · not shared here +future");
        assert_eq!(
            state_label(&held_only, true, &A).0,
            "* published - not shared here"
        );
        // One published thought among unconfirmed ones is enough to say so.
        assert_eq!(label(&held(5, 1, 3)), "● published · not shared here");
        assert_eq!(label(&state(5, 0, 0)), "○ private");
    }

    #[test]
    fn a_row_held_only_by_publishes_that_did_not_conclude_is_never_called_published() {
        let unconfirmed = held(5, 0, 2);
        assert_eq!(label(&unconfirmed), "? unconfirmed · not shared here");
        assert_eq!(state_label(&unconfirmed, true, &U).1, Tone::Waiting);
        assert_eq!(
            state_label(&unconfirmed, true, &A).0,
            "? unconfirmed - not shared here"
        );
        let mut future = unconfirmed.clone();
        future.share_future = Some(true);
        assert_eq!(label(&future), "? unconfirmed · not shared here +future");
    }

    #[test]
    fn nothing_the_remote_holds_reads_private_and_nothing_unproven_reads_published() {
        // Every mix of held thoughts with nothing shared, in both glyph sets.
        for thoughts in 1..=4 {
            for published in 0..=thoughts {
                for unconfirmed in 0..=(thoughts - published) {
                    for glyphs in [&U, &A] {
                        let summary = held(thoughts, published, unconfirmed);
                        let (text, _) = state_label(&summary, true, glyphs);
                        let case =
                            format!("{published} published, {unconfirmed} unconfirmed: {text}");
                        assert_eq!(
                            text.contains("private"),
                            published + unconfirmed == 0,
                            "{case}"
                        );
                        assert_eq!(text.contains("published"), published > 0, "{case}");
                        assert_eq!(
                            text.contains("unconfirmed"),
                            published == 0 && unconfirmed > 0,
                            "{case}"
                        );
                        assert_eq!(
                            text.contains("not shared here"),
                            published + unconfirmed > 0,
                            "{case}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_label_is_derived_from_its_inputs_alone() {
        // `held` can exceed `published`: pending and unknown rows count.
        let input = LabelInput {
            thoughts: 5,
            shared: 0,
            published: 0,
            unconfirmed: 0,
            held: 2,
            held_published: 1,
            future: false,
            imported: false,
        };
        assert_eq!(
            label_of(&input, true, &U).0,
            "● published · not shared here"
        );
        let unproven = LabelInput {
            held_published: 0,
            ..input
        };
        assert_eq!(
            label_of(&unproven, true, &U).0,
            "? unconfirmed · not shared here"
        );
        let none_held = LabelInput {
            held: 0,
            held_published: 0,
            ..input
        };
        assert_eq!(label_of(&none_held, true, &U).0, "○ private");
        let future_only = LabelInput {
            future: true,
            ..none_held
        };
        assert_eq!(label_of(&future_only, true, &U).0, "◐ shared · future only");
        // Shared thoughts follow the table whatever else the remote holds.
        let shared = LabelInput {
            shared: 5,
            published: 3,
            unconfirmed: 1,
            held: 4,
            ..input
        };
        assert_eq!(label_of(&shared, true, &U).0, "◐ shared · 2 waiting");
        // Imported and not shared from here stays `imported`.
        let imported = LabelInput {
            imported: true,
            ..input
        };
        assert_eq!(label_of(&imported, true, &U).0, "imported");
        // Each number comes from its own field of the summary: what is held is
        // not what is published.
        let mut summary = state(5, 2, 1);
        summary.imported = true;
        summary.share_future = Some(true);
        summary.unconfirmed = 1;
        summary.held = 4;
        summary.held_published = 3;
        assert_eq!(
            LabelInput::of(&summary),
            LabelInput {
                thoughts: 5,
                shared: 2,
                published: 1,
                unconfirmed: 1,
                held: 4,
                held_published: 3,
                future: true,
                imported: true,
            }
        );
    }

    #[test]
    fn a_badge_does_not_count_a_row_the_remote_holds_as_private() {
        let held_agent = held(4, 2, 0);
        let private = agent(0, 0);
        assert_eq!(
            agents_badge(&[&held_agent, &private], true, &U),
            "+2 agents · 1 private"
        );
        assert_eq!(
            agents_badge(&[&held_agent], true, &U),
            "+1 agent · 1 published"
        );
    }

    #[test]
    fn a_badge_never_calls_an_unproven_row_published_or_a_future_only_row_private() {
        let unconfirmed = held(4, 0, 2);
        let mut future_only = agent(0, 0);
        future_only.share_future = Some(true);
        let private = agent(0, 0);
        // An unconfirmed row is unsettled, which the badge counts with waiting.
        assert_eq!(
            agents_badge(&[&unconfirmed], true, &U),
            "+1 agent · 1 waiting"
        );
        // A row that is only later thoughts shared says nothing either way.
        assert_eq!(agents_badge(&[&future_only], true, &U), "+1 agent");
        assert_eq!(
            agents_badge(&[&future_only, &private], true, &U),
            "+2 agents · 1 private"
        );
    }

    #[test]
    fn a_location_is_cut_by_display_width_wherever_it_is_shown() {
        let mut long = overview("origin");
        long.location = Some(format!("github.com/{}", "x".repeat(2000)));
        let h = header(&RemoteView::Usable(&long), Some(1), &U);
        for candidate in &h.remote {
            assert!(
                text::width(candidate) < "remote: origin -> ".len() + LOCATION_WIDTH + 20,
                "{candidate}"
            );
        }
        assert!(h.remote[0].contains('…'));
        let widths: Vec<_> = h.remote.iter().map(|c| text::width(c)).collect();
        assert!(
            widths.windows(2).all(|pair| pair[0] > pair[1]),
            "{widths:?}"
        );

        let destination = Destination::of(&long, &U);
        assert!(text::width(destination.location.as_deref().unwrap()) <= LOCATION_WIDTH);
        assert!(text::width(&destination.described()) < LOCATION_WIDTH + 20);
        let rows = remote_rows(&[long.clone()], None, &U);
        assert!(text::width(&rows[0].location) <= LOCATION_WIDTH);
        let banner = banner(FlowKind::Share, &destination, None, &U);
        assert!(
            text::width(&banner[1]) < LOCATION_WIDTH + 60,
            "{}",
            banner[1]
        );
        // A location with control characters is sanitized as well as cut.
        long.location = Some("evil\u{1b}[2J.example/\u{202e}repo".to_owned());
        let destination = Destination::of(&long, &U);
        assert_eq!(
            destination.location.as_deref(),
            Some("evil?[2J.example/?repo")
        );
    }

    #[test]
    fn a_missing_location_is_a_dash_in_the_picker_and_absent_elsewhere() {
        let mut bare = overview("origin");
        bare.location = None;
        assert_eq!(remote_rows(&[bare.clone()], None, &U)[0].location, "–");
        assert_eq!(remote_rows(&[bare.clone()], None, &A)[0].location, "-");
        let h = header(&RemoteView::Usable(&bare), None, &U);
        assert!(h.remote.iter().all(|c| !c.contains("->")));
        assert_eq!(Destination::of(&bare, &U).described(), "'origin'");
    }

    #[test]
    fn an_id_is_safe_unless_it_holds_a_control_or_format_character() {
        for id in [
            "run-0d2a",
            "session:agent-a1",
            "remote:abc:session-1:agent-7",
            "with spaces – and שלום",
        ] {
            assert!(id_is_safe(id), "{id:?}");
        }
        for id in [
            "a\u{1b}[2J",
            "a\nb",
            "a\u{202e}b",
            "a\u{200b}",
            "a\u{2028}",
            "a\u{7f}",
            "a\u{feff}",
        ] {
            assert!(!id_is_safe(id), "{id:?}");
        }
        assert_eq!(
            UNSAFE_ID,
            "This conversation's id contains characters that cannot be shown safely, so it cannot be shared or unshared from here."
        );
    }

    #[test]
    fn the_settings_changed_notice_names_the_remote() {
        assert_eq!(
            settings_changed_status("origin"),
            "Publishing settings for 'origin' changed; check them and press the key again."
        );
    }

    #[test]
    fn the_exit_note_tells_what_still_waits() {
        assert_eq!(
            waiting_note(3, "origin"),
            "3 shared thoughts are still waiting to publish to 'origin'. Press p in cvc conversations, or run cvc push --manual --remote origin."
        );
        assert!(waiting_note(1, "origin").starts_with("1 shared thought is still waiting"));
    }

    fn all_on() -> Available {
        Available {
            details: true,
            share: true,
            unshare: true,
            publish: true,
        }
    }

    #[test]
    fn the_key_bar_in_full_and_narrow_forms() {
        let full = plain_text(&key_bar(&all_on(), 200, &U));
        assert_eq!(
            full,
            " ↑↓ move   ←→ agents   enter details   s share   u unshare   p publish   r remote   ? help   q quit"
        );
        // Without the words after the arrows.
        let narrower = plain_text(&key_bar(&all_on(), 90, &U));
        assert_eq!(
            narrower,
            " ↑↓   ←→   enter details   s share   u unshare   p publish   r remote   ? help   q quit"
        );
        // Narrower still: help is last and the leftmost keys go first.
        for width in [60usize, 70, 80] {
            let line = plain_text(&key_bar(&all_on(), width, &U));
            assert!(text::width(&line) <= width, "{width}: {line}");
            assert!(line.ends_with("? help"), "{width}: {line}");
            assert!(line.contains("q quit"), "{width}: {line}");
        }
        let ascii = plain_text(&key_bar(&all_on(), 200, &A));
        assert!(ascii.starts_with(" up/down move   left/right agents"));
    }

    #[test]
    fn unavailable_keys_are_dimmed_not_hidden() {
        let bar = key_bar(
            &Available {
                details: true,
                share: false,
                unshare: false,
                publish: true,
            },
            200,
            &U,
        );
        let text = plain_text(&bar);
        assert!(text.contains("s share") && text.contains("u unshare"));
        let dim: Vec<_> = bar
            .iter()
            .filter(|seg| seg.emph == Emph::Dim)
            .map(|seg| seg.text.trim().to_owned())
            .collect();
        assert_eq!(dim, vec!["s", "share", "u", "unshare"]);
    }

    fn details(snapshot: (usize, usize, usize)) -> ConversationDetails {
        let mut summary = conversation("session-1", FIRST, LAST, 29);
        summary.title = "Refactor the sync module".to_owned();
        summary.linked = 26;
        ConversationDetails {
            summary,
            context_paths: vec![
                ContextPathUse {
                    path: "src/sync.rs".to_owned(),
                    interactions: 12,
                },
                ContextPathUse {
                    path: "src/db.rs".to_owned(),
                    interactions: 9,
                },
            ],
            context_path_total: 14,
            snapshot_count: snapshot.0,
            snapshot_shared: snapshot.1,
            snapshot_new: snapshot.2,
        }
    }

    fn page_lines(page: &[Page]) -> Vec<String> {
        page.iter()
            .map(|row| match row {
                Page::Title(t) | Page::Heading(t) | Page::Text(t) => t.clone(),
                Page::Blank => String::new(),
                Page::Field { label, value } => format!("{label}: {value}"),
                Page::Item { label, value } => format!("{label}: {value}"),
            })
            .collect()
    }

    #[test]
    fn the_details_page_says_a_subagents_prompt_and_result_are_part_of_its_parent() {
        let details = details((29, 0, 29));
        let summary = details.summary.clone();
        let subagents = |agents: usize| {
            let page = details_page(
                &DetailsInput {
                    summary: &summary,
                    details: Some(&details),
                    lineage: Lineage::None,
                    agents,
                    remote: None,
                },
                &U,
            );
            page_lines(&page)
                .into_iter()
                .find(|line| line.starts_with("Subagents"))
        };
        assert_eq!(
            subagents(1).as_deref(),
            Some(
                "Subagents: 1 conversation, shared separately; its prompt and result are part of this one"
            )
        );
        assert_eq!(
            subagents(3).as_deref(),
            Some(
                "Subagents: 3 conversations, each shared separately; their prompts and results are part of this one"
            )
        );
        assert_eq!(subagents(0), None);
    }

    #[test]
    fn the_details_page_says_auto_push_acts_at_every_push() {
        let details = details((29, 0, 29));
        let summary = details.summary.clone();
        let auto = |on: bool| {
            let mut remote = overview("origin");
            remote.auto_push = on;
            let page = details_page(
                &DetailsInput {
                    summary: &summary,
                    details: Some(&details),
                    lineage: Lineage::None,
                    agents: 0,
                    remote: Some(&remote),
                },
                &U,
            );
            page_lines(&page)
                .into_iter()
                .find(|line| line.starts_with("Auto-push"))
                .unwrap()
        };
        assert_eq!(
            auto(true),
            "Auto-push: on: every git push to 'origin' also publishes shared thoughts"
        );
        assert_eq!(auto(false), "Auto-push: off");
    }

    #[test]
    fn the_details_page_for_a_shared_conversation() {
        let details = details((29, 27, 2));
        let mut summary = details.summary.clone();
        summary.shared_count = 27;
        summary.published = 21;
        summary.unconfirmed = 3;
        summary.share_future = Some(false);
        let remote = overview("origin");
        let page = details_page(
            &DetailsInput {
                summary: &summary,
                details: Some(&details),
                lineage: Lineage::None,
                agents: 2,
                remote: Some(&remote),
            },
            &U,
        );
        let lines = page_lines(&page);
        assert_eq!(lines[0], "Refactor the sync module");
        for expected in [
            "ID: session-1",
            "Activity: 2026-10-13 09:12 – 10-14 18:40 UTC",
            "Thoughts: 29",
            "Linked: 26 of 29 thoughts linked to a commit",
            "Context files: 14: src/sync.rs (12), src/db.rs (9), … (+12 more)",
            "Subagents: 2 conversations, each shared separately; their prompts and results are part of this one",
            "With 'origin' (fingerprint 8348b79e2c41…)",
            "State: ◐ shared · 6 waiting · 2 private",
            "Future thoughts: private",
            "Share covers: 29 thoughts: 27 already shared, 2 new",
            "Unconfirmed: 3 thoughts from a publish that did not finish cleanly",
            "Auto-push: off",
            "A thought is one recorded step (for Claude Code, one assistant response).",
            "Scrubbing can miss secrets. Published thoughts cannot be recalled.",
        ] {
            assert!(
                lines
                    .iter()
                    .any(|line| line.starts_with(expected) || line == expected),
                "missing {expected:?} in {lines:#?}"
            );
        }
        assert!(!lines.iter().any(|line| line.starts_with("Kind")));
    }

    #[test]
    fn the_details_page_names_the_kind_of_conversation() {
        let details = details((29, 0, 29));
        let summary = details.summary.clone();
        let kind = |lineage: Lineage, imported: bool| {
            let mut summary = summary.clone();
            summary.imported = imported;
            let page = details_page(
                &DetailsInput {
                    summary: &summary,
                    details: Some(&details),
                    lineage,
                    agents: 0,
                    remote: None,
                },
                &U,
            );
            page_lines(&page)
                .into_iter()
                .find(|line| line.starts_with("Kind"))
        };
        assert_eq!(kind(Lineage::None, false), None);
        assert_eq!(
            kind(Lineage::None, true).as_deref(),
            Some("Kind: Imported from a remote")
        );
        assert_eq!(
            kind(Lineage::AgentOf("Refactor".to_owned()), false).as_deref(),
            Some("Kind: Subagent of \"Refactor\"")
        );
        assert_eq!(
            kind(Lineage::OrphanAgent, false).as_deref(),
            Some("Kind: Subagent. Parent session is not captured on this machine.")
        );
    }

    #[test]
    fn the_details_page_covers_every_share_form() {
        for (snapshot, expected) in [
            ((29, 27, 2), "29 thoughts: 27 already shared, 2 new"),
            ((29, 29, 0), "29 thoughts: all already shared"),
            ((29, 0, 29), "29 thoughts: all of them"),
            ((0, 0, 0), "nothing"),
        ] {
            assert_eq!(covers(&details(snapshot)), expected);
        }
    }

    #[test]
    fn an_imported_conversation_shows_no_share_plan() {
        let details = details((29, 0, 29));
        let mut summary = details.summary.clone();
        summary.imported = true;
        let remote = overview("origin");
        let page = details_page(
            &DetailsInput {
                summary: &summary,
                details: Some(&details),
                lineage: Lineage::None,
                agents: 0,
                remote: Some(&remote),
            },
            &U,
        );
        let lines = page_lines(&page);
        assert!(lines.iter().any(|l| l == "State: imported"), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.starts_with("Share covers")));
        assert!(!lines.iter().any(|l| l.starts_with("Future thoughts")));
    }

    #[test]
    fn details_text_from_the_store_is_sanitized() {
        let mut details = details((1, 0, 1));
        details.summary.title = "evil\u{1b}[2J\u{202e}title".to_owned();
        details.summary.id = "id\u{1b}]0;x\u{7}".to_owned();
        details.context_paths = vec![ContextPathUse {
            path: "dir/\u{1b}[31mred".to_owned(),
            interactions: 1,
        }];
        let page = details_page(
            &DetailsInput {
                summary: &details.summary,
                details: Some(&details),
                lineage: Lineage::AgentOf(text::sanitize("par\u{1b}ent")),
                agents: 0,
                remote: None,
            },
            &U,
        );
        for line in page_lines(&page) {
            assert!(
                !line.chars().any(text::is_unsafe),
                "unsanitized text in {line:?}"
            );
        }
    }

    #[test]
    fn the_strip_summarizes_the_selected_conversation() {
        let details = details((27, 21, 6));
        let mut summary = details.summary.clone();
        summary.thoughts = 27;
        summary.linked = 26;
        let remote = overview("origin");
        let lines = strip_lines(
            &DetailsInput {
                summary: &summary,
                details: Some(&details),
                lineage: Lineage::None,
                agents: 0,
                remote: Some(&remote),
            },
            &U,
        );
        assert_eq!(lines[0], "Refactor the sync module");
        assert_eq!(
            lines[1],
            "27 thoughts, 26 linked to commits, 2026-10-13 09:12 – 10-14 18:40 UTC"
        );
        assert_eq!(
            lines[2],
            "Share covers 27 thoughts as of now (21 already shared, 6 new). Future thoughts: private."
        );
    }

    #[test]
    fn the_strip_has_no_share_line_without_a_remote_or_for_an_imported_conversation() {
        let details = details((27, 21, 6));
        let mut summary = details.summary.clone();
        let without = strip_lines(
            &DetailsInput {
                summary: &summary,
                details: Some(&details),
                lineage: Lineage::OrphanAgent,
                agents: 0,
                remote: None,
            },
            &U,
        );
        assert_eq!(without.len(), 3);
        assert_eq!(
            without[2],
            "Parent session is not captured on this machine."
        );
        summary.imported = true;
        let remote = overview("origin");
        let imported = strip_lines(
            &DetailsInput {
                summary: &summary,
                details: Some(&details),
                lineage: Lineage::None,
                agents: 0,
                remote: Some(&remote),
            },
            &U,
        );
        assert_eq!(imported[2], IMPORTED_IS_READ_ONLY);
    }

    #[test]
    fn the_help_page_covers_the_pipeline_the_keys_the_states_and_the_cautions() {
        let text = page_lines(&help_page(&U)).join("\n");
        for expected in [
            "private --s--> shared (in this clone) --p--> published (on the remote)",
            "u works only before publish.",
            // `q` closes a page or the remote list first and quits only from
            // the list; `esc` never quits.
            "close a bar, a page or the remote list",
            "close a page or the remote list; quit from the list",
            // What private means is what the label means by it.
            "○ private: not shared from this clone for this remote, and the remote holds none of it.",
            "◐ shared · 2 waiting",
            "● published: everything shared is published.",
            "◐ shared · future only: nothing shared yet; later thoughts are shared as they are captured.",
            "● published · not shared here: the remote already holds it, and this clone has no share for it.",
            "? unconfirmed · not shared here: a publish of it may have reached the remote; cvc privacy reconcile settles that.",
            "· 3 not shared here: 3 more thoughts of a shared conversation that the remote holds but this clone does not share.",
            "· 3 private: 3 more thoughts of a shared conversation that are not shared from here and that the remote holds none of.",
            "+future",
            "Share, unshare, publish and authorize each wait for their typed phrase; no key does any of them by itself.",
            "Two standing choices act later without a new phrase: +future shares new thoughts as they are captured, and auto-push (set with cvc privacy set-auto-push, never from this view) publishes shared thoughts at every git push to that remote.",
            "Scrubbing and .thoughtignore can miss secrets; they are not a guarantee.",
            "Counts in the header are this clone's view; publish re-checks the remote first.",
            "A thought is one recorded step (for Claude Code, one assistant response).",
        ] {
            assert!(text.contains(expected), "missing {expected:?}");
        }
        // `q` closes an overlay before it quits, and quits only from the list;
        // the page must not promise more than that.
        assert!(!text.contains("q quits"), "{text}");
        assert!(page_lines(&help_page(&A)).join("\n").is_ascii());
    }

    #[test]
    fn the_remote_rows_say_what_each_remote_allows() {
        let mut unauthorized = overview("beta");
        unauthorized.sharing_consented = false;
        let mut auto = overview("origin");
        auto.auto_push = true;
        let mut unknown = overview("gamma");
        unknown.sharing_consented = false;
        unknown.consent_error = Some("insecure".to_owned());
        let mut broken = overview("delta");
        broken.fingerprint = Err("remote has an empty push URL".to_owned());
        broken.location = None;
        let mut unplaced = overview("eps");
        unplaced.location = None;
        let rows = remote_rows(
            &[unauthorized, auto, unknown, broken, unplaced],
            Some("origin"),
            &U,
        );
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["beta", "origin", "gamma", "delta", "eps"]);
        assert_eq!(rows[0].publishing, "not authorized yet");
        assert_eq!(
            (rows[1].publishing, rows[1].auto_push),
            ("authorized", "on")
        );
        assert!(rows[1].current && !rows[0].current);
        assert_eq!(rows[2].publishing, "unknown");
        assert_eq!(
            rows[3].unusable.as_deref(),
            Some("remote has an empty push URL")
        );
        assert_eq!(rows[4].location, "–");
        assert_eq!(rows[0].fingerprint, "8348b79e2c41…");
        assert_eq!(rows[0].location, "github.com/acme/repo");
    }

    #[test]
    fn the_empty_state_gives_one_hint() {
        assert_eq!(
            empty_state(false, false),
            [
                "No conversations captured yet.",
                "Passive capture is off. Turn it on with: cvc privacy acknowledge-capture"
            ]
        );
        // Capture on, no checkout harness: both routes, the plugin's two
        // commands one to a line.
        assert_eq!(
            empty_state(true, false),
            [
                "No conversations captured yet.",
                "To record Claude Code sessions, run these in Claude Code:",
                "  /plugin marketplace add meirka8/volute",
                "  /plugin install cvc@volute",
                "or, for this checkout only, run in a shell: cvc harness install claude-code",
            ]
        );
        assert_eq!(
            empty_state(true, true),
            [
                "No conversations captured yet.",
                "Conversations appear here after a session has been captured."
            ]
        );
    }

    #[test]
    fn every_empty_state_line_fits_an_80_column_terminal() {
        for (acknowledged, installed) in [(false, false), (true, false), (true, true)] {
            for line in empty_state(acknowledged, installed) {
                assert!(
                    line.chars().count() <= 80,
                    "{} columns: {line}",
                    line.chars().count()
                );
                assert_eq!(*line, line.trim_end());
            }
        }
    }
}
