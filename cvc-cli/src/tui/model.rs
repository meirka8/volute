//! The state of the interactive conversations view, and how keys change it.
//!
//! Nothing in here touches a terminal, the store or Git: the event loop loads
//! data and hands it in, the model answers key presses with at most one
//! [`Action`] for the loop to carry out, and the renderer draws what the model
//! reports. That is what keeps the consent rules testable:
//!
//! - A key press is never a grant. `s`, `u` and `p` open a pre-flight bar and
//!   nothing else; `Enter` or `f` in a bar only asks the loop to run the flow,
//!   and the flow asks for its typed phrase itself.
//! - Every other path out of a bar is a cancel, and a bar ignores every key it
//!   does not list, so a held key cannot walk through it.
//! - Where consent actions are not available (the platform cannot keep type-
//!   ahead from answering a challenge), no bar that leads to a flow opens.

use std::collections::{HashMap, HashSet};

use cvc_core::db::{
    ConversationDetails, ConversationSummary, DestinationPublicationCounts, UnshareCheck,
};
use cvc_core::models::FutureSharePolicy;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::text::{self, Glyphs};
use super::tree::{Entry, Kind, Tree};
use super::words::{
    self, Available, Destination, DetailsInput, FlowKind, Lineage, Page, RemoteRow, RemoteView,
    Seg, ShareAsk, Tone,
};
use crate::commands::conversations::RemoteOverview;
use crate::commands::failure::FlowFailure;
use crate::commands::sync::{PublishOutcome, ShareOutcome};

/// The smallest terminal the view draws in.
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 12;
/// The terminal height from which the strip under the list is drawn.
pub const STRIP_MIN_HEIGHT: u16 = 26;
/// The strip is a rule and up to four lines.
pub const STRIP_ROWS: usize = 5;
/// The header is two lines; a notice adds one or two more.
pub const HEADER_ROWS: usize = 2;
/// The most lines a notice takes.
pub const NOTICE_MAX_ROWS: usize = 2;
/// The widest an overlay is drawn, so lines stay readable on wide terminals.
pub const OVERLAY_MAX_WIDTH: usize = 100;

/// What the loop loads from the store and from Git for the selected remote.
#[derive(Clone, Debug, Default)]
pub struct Data {
    pub summaries: Vec<ConversationSummary>,
    pub remotes: Vec<RemoteOverview>,
    /// Shared and published thoughts of the selected remote, over all
    /// conversations; `None` without a usable remote.
    pub counts: Option<DestinationPublicationCounts>,
}

/// What is read about the selected conversation each time it matters: the
/// details for the selected remote, and whether an unshare would be refused.
#[derive(Clone, Debug)]
pub struct RowInfo {
    pub details: ConversationDetails,
    /// `None` without a usable remote.
    pub unshare: Option<UnshareCheck>,
}

/// How the model is set up.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Whether share, unshare, publish and authorize may be started from the
    /// view (`commands::conversations::consent_actions_allowed`). A flag, not a
    /// `cfg`, so both sides are testable.
    pub consent_actions: bool,
    pub glyphs: Glyphs,
}

/// A flow for the loop to run. Each is the very function its subcommand runs,
/// which asks for its own typed phrase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Flow {
    Share {
        id: String,
        future: FutureSharePolicy,
    },
    Unshare {
        id: String,
    },
    Publish,
    Authorize,
}

impl Flow {
    pub fn kind(&self) -> FlowKind {
        match self {
            Flow::Share { .. } => FlowKind::Share,
            Flow::Unshare { .. } => FlowKind::Unshare,
            Flow::Publish => FlowKind::Publish,
            Flow::Authorize => FlowKind::Authorize,
        }
    }
}

/// What a key press asks the event loop to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Suspend the screen and run a flow.
    Run(Flow),
    /// The person chose another remote (by name): load everything for it, and
    /// if that succeeds hand the data to [`Model::switch_to`]. Until then the
    /// view keeps showing the remote it was showing.
    Switch(String),
}

/// What a flow came to, for the notice that follows it.
#[derive(Clone, Debug)]
pub enum Report {
    Shared {
        id: String,
        outcome: ShareOutcome,
    },
    Unshared {
        made_private: usize,
    },
    Published(PublishOutcome),
    Authorized,
    Failed {
        kind: FlowKind,
        failure: FlowFailure,
    },
    /// The remote no longer resolves to the destination that was listed, so
    /// nothing was run.
    RemoteChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Esc,
    /// A character, lowercased: letters are case-insensitive.
    Char(char),
    /// Ctrl-C, which in raw mode arrives as a key.
    Interrupt,
    Other,
}

impl Key {
    fn of(event: KeyEvent) -> Self {
        let modifiers = event.modifiers;
        if modifiers.contains(KeyModifiers::CONTROL) {
            return match event.code {
                KeyCode::Char(c) if c.eq_ignore_ascii_case(&'c') => Key::Interrupt,
                _ => Key::Other,
            };
        }
        if modifiers.intersects(
            KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META,
        ) {
            return Key::Other;
        }
        match event.code {
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::Enter => Key::Enter,
            KeyCode::Esc => Key::Esc,
            KeyCode::Char(c) => Key::Char(c.to_ascii_lowercase()),
            _ => Key::Other,
        }
    }
}

/// The pre-flight bar that replaces the key bar after `s`, `u` or `p`. It is a
/// question with the keys that answer it; the only key that leads anywhere is
/// the one the bar lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bar {
    Share {
        id: String,
        ask: ShareAsk,
    },
    Unshare {
        id: String,
        title: String,
        shared: i64,
    },
    Publish {
        waiting: i64,
    },
    /// Offered instead of the share and publish bars while the remote is not
    /// authorized for publishing.
    Authorize,
    /// Why nothing can be done; any key closes it.
    Blocked(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    List,
    Details {
        scroll: usize,
    },
    Help {
        scroll: usize,
    },
    /// The remote picker; the cursor is an index into the remotes.
    Remotes {
        cursor: usize,
    },
    Bar(Bar),
}

/// The rows of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub header: usize,
    pub notice: usize,
    pub head: usize,
    /// Rows for the list itself.
    pub list: usize,
    pub strip: usize,
    pub bottom: usize,
}

/// A parent's disclosure triangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disclosure {
    None,
    Collapsed,
    Expanded,
}

/// What the first column of a row holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    At(i64),
    /// An agent under its parent.
    Agent,
    /// An agent whose parent is not listed.
    OrphanAt(i64),
}

/// The text of a first-column cell: the main part, and a dim tag after it.
pub fn when_cell(when: When, short: bool, g: &Glyphs) -> (String, Option<String>) {
    match when {
        When::At(seconds) => (words::format_when(seconds, short), None),
        When::Agent => (g.child().to_owned(), Some(" agent".to_owned())),
        When::OrphanAt(seconds) => (
            words::format_when(seconds, short),
            Some(" agent".to_owned()),
        ),
    }
}

/// One row of the list as drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowView {
    pub id: String,
    pub disclosure: Disclosure,
    pub when: When,
    pub title: String,
    pub thoughts: i64,
    pub linked: i64,
    pub state: String,
    pub tone: Tone,
    /// The dim badge that ends a collapsed parent's row.
    pub badge: Option<String>,
    /// Read-only here: drawn dim.
    pub imported: bool,
    pub selected: bool,
}

/// Column widths that depend on what is listed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Columns {
    pub when_full: usize,
    pub when_short: usize,
    pub thoughts: usize,
    pub linked: usize,
    pub state: usize,
}

/// What an overlay holds.
pub enum Overlay {
    Page { page: Vec<Page>, scroll: usize },
    Remotes { rows: Vec<RemoteRow>, cursor: usize },
}

/// The state of the whole view.
pub struct Model {
    glyphs: Glyphs,
    consent_actions: bool,
    size: (u16, u16),
    remotes: Vec<RemoteOverview>,
    counts: Option<DestinationPublicationCounts>,
    /// The selected remote's name; `None` when none is selected.
    remote: Option<String>,
    tree: Tree,
    expanded: HashSet<String>,
    /// Indices into the tree's entries, in the order they are shown.
    visible: Vec<usize>,
    columns: Columns,
    cursor: usize,
    top: usize,
    /// What was read about each conversation; `None` where the read failed.
    info: HashMap<String, Option<RowInfo>>,
    mode: Mode,
    status: Option<String>,
    /// The remote something was shared for during this session.
    shared_remote: Option<String>,
}

/// The remote the view starts on: `--remote` if given (whether or not it
/// exists, so the header can say so), else `origin` if there is one, else none.
/// Never remembered between runs.
pub fn initial_remote(remotes: &[RemoteOverview], requested: Option<&str>) -> Option<String> {
    match requested {
        Some(name) => Some(name.to_owned()),
        None => remotes
            .iter()
            .find(|remote| remote.name == "origin")
            .map(|remote| remote.name.clone()),
    }
}

/// The destination fingerprint of the remote called `name`, when it can be one.
pub fn fingerprint_of<'a>(remotes: &'a [RemoteOverview], name: Option<&str>) -> Option<&'a str> {
    let name = name?;
    remotes
        .iter()
        .find(|remote| remote.name == name)
        .and_then(|remote| remote.fingerprint.as_deref().ok())
}

impl Model {
    pub fn new(data: Data, requested: Option<&str>, settings: Settings) -> Self {
        let remote = initial_remote(&data.remotes, requested);
        let mut model = Self {
            glyphs: settings.glyphs,
            consent_actions: settings.consent_actions,
            size: (100, 30),
            remotes: data.remotes,
            counts: data.counts,
            remote,
            tree: Tree::build(&data.summaries),
            expanded: HashSet::new(),
            visible: Vec::new(),
            columns: Columns::default(),
            cursor: 0,
            top: 0,
            info: HashMap::new(),
            mode: Mode::List,
            status: None,
            shared_remote: None,
        };
        model.rebuild_visible(None);
        model
    }

    /// Makes `name` the selected remote, together with the data loaded for it,
    /// so that the remote shown and the share state shown never disagree.
    pub fn switch_to(&mut self, name: String, data: Data) {
        self.remote = Some(name);
        self.apply(data);
    }

    /// Replaces what is listed with freshly loaded data for the selected
    /// remote. The selection stays on the same conversation when it is still
    /// listed, and open parents stay open. Anything read about a row is dropped.
    pub fn apply(&mut self, data: Data) {
        let keep = self.selected_id().map(str::to_owned);
        self.remotes = data.remotes;
        self.counts = data.counts;
        self.tree = Tree::build(&data.summaries);
        // The old indices point into the old tree.
        self.visible.clear();
        self.info.clear();
        self.rebuild_visible(keep);
    }

    pub fn glyphs(&self) -> &Glyphs {
        &self.glyphs
    }

    #[cfg(test)]
    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    /// Records the terminal size. A bar that was open is cancelled, so that
    /// nothing is left half-visible to be answered blind.
    pub fn set_size(&mut self, width: u16, height: u16) {
        if (width, height) == self.size {
            return;
        }
        self.size = (width, height);
        if self.too_small() {
            if let Mode::Bar(_) = self.mode {
                self.mode = Mode::List;
            }
        }
        self.ensure_visible();
        self.clamp_scroll();
    }

    pub fn too_small(&self) -> bool {
        self.size.0 < MIN_WIDTH || self.size.1 < MIN_HEIGHT
    }

    pub fn too_small_message(&self) -> String {
        format!(
            "Terminal too small (need {MIN_WIDTH}x{MIN_HEIGHT}, have {}x{}).",
            self.size.0, self.size.1
        )
    }

    // ----- the remote -----

    pub fn remote_name(&self) -> Option<&str> {
        self.remote.as_deref()
    }

    pub fn remote_view(&self) -> RemoteView<'_> {
        match &self.remote {
            Some(name) => match self.remotes.iter().find(|remote| &remote.name == name) {
                Some(overview) if overview.fingerprint.is_ok() => RemoteView::Usable(overview),
                Some(overview) => RemoteView::Unusable(overview),
                None => RemoteView::Missing {
                    name,
                    others_exist: !self.remotes.is_empty(),
                },
            },
            None if self.remotes.is_empty() => RemoteView::NoRemotes,
            None => RemoteView::NoneSelected,
        }
    }

    /// The selected remote when it can be a destination.
    fn usable_remote(&self) -> Option<&RemoteOverview> {
        match self.remote_view() {
            RemoteView::Usable(overview) => Some(overview),
            _ => None,
        }
    }

    /// The overview of the selected remote, as listed, for the loop to resolve
    /// at the moment of an action. A copy: the model keeps changing.
    pub fn selected_remote(&self) -> Option<RemoteOverview> {
        self.usable_remote().cloned()
    }

    pub fn destination_fingerprint(&self) -> Option<&str> {
        fingerprint_of(&self.remotes, self.remote.as_deref())
    }

    /// The overview of a remote by name, for the note printed at exit.
    pub fn overview_named(&self, name: &str) -> Option<&RemoteOverview> {
        self.remotes.iter().find(|remote| remote.name == name)
    }

    /// The remote something was shared for during this session, if anything was.
    pub fn shared_remote(&self) -> Option<&str> {
        self.shared_remote.as_deref()
    }

    // ----- the selection -----

    fn selected_entry(&self) -> Option<&Entry> {
        self.visible
            .get(self.cursor)
            .map(|&index| &self.tree.entries[index])
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.selected_entry().map(|entry| entry.summary.id.as_str())
    }

    /// The selected conversation, when what was read about it is not loaded yet.
    pub fn wants_info(&self) -> Option<String> {
        let id = self.selected_id()?;
        (!self.info.contains_key(id)).then(|| id.to_owned())
    }

    /// Stores what was read about a conversation, replacing what was there.
    /// `None` records that it could not be read: nothing is then offered for
    /// that conversation, and it is not asked for again until the next reload.
    pub fn set_info(&mut self, id: &str, info: Option<RowInfo>) {
        self.info.insert(id.to_owned(), info);
    }

    fn selected_info(&self) -> Option<&RowInfo> {
        self.info.get(self.selected_id()?)?.as_ref()
    }

    fn has_destination(&self) -> bool {
        self.usable_remote().is_some()
    }

    // ----- notices -----

    /// Shows a notice until the next key.
    pub fn set_status(&mut self, text: &str) {
        self.status = Some(text::sanitize(text));
    }

    #[cfg(test)]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// The notice as lines of the terminal's width, each with the space that
    /// starts a line of the header block. A notice is one line when it fits. The
    /// statuses that follow a flow run to 150 characters and one of them ends in
    /// a command to run, so rather than cutting a long one off it takes a second
    /// line; only what does not fit in two lines ends in an ellipsis.
    pub fn notice(&self) -> Vec<String> {
        let Some(status) = self.status.as_deref() else {
            return Vec::new();
        };
        let width = (self.size.0 as usize).saturating_sub(1).max(1);
        let mut lines = text::wrap(status, width);
        if lines.len() > NOTICE_MAX_ROWS {
            lines.truncate(NOTICE_MAX_ROWS);
            if let Some(last) = lines.last_mut() {
                let ellipsis = self.glyphs.ellipsis();
                *last = text::truncate(&format!("{last}{ellipsis}"), width, ellipsis);
            }
        }
        lines.into_iter().map(|line| format!(" {line}")).collect()
    }

    /// Shows why an action cannot go ahead, as the blocked bar. The loop uses
    /// this when a check right before running a flow finds the conversation
    /// has become read-only.
    pub fn show_blocked(&mut self, reason: &str) {
        self.mode = Mode::Bar(Bar::Blocked(text::sanitize(reason)));
    }

    /// Words what a flow came to. Call it after reloading, so it can see what
    /// the flow changed.
    pub fn report(&mut self, report: Report) {
        let remote = self.remote_label();
        let said = match report {
            Report::Shared { id, outcome } => {
                self.shared_remote = self.remote.clone();
                let auto_push = self.usable_remote().is_some_and(|o| o.auto_push);
                let private_agents = self.private_agents_of(&id);
                words::shared_status(
                    outcome.snapshot_count,
                    &remote,
                    outcome.future,
                    auto_push,
                    private_agents,
                    &self.glyphs,
                )
            }
            Report::Unshared { made_private } => words::unshared_status(made_private, &remote),
            Report::Published(outcome) => words::published_status(&outcome, &remote),
            Report::Authorized => words::authorized_status(&remote),
            Report::Failed { kind, failure } => words::failure_status(kind, &failure, &remote),
            Report::RemoteChanged => format!(
                "Remote '{remote}' changed while this view was open, so nothing was done. The remote list was refreshed."
            ),
        };
        self.status = Some(text::sanitize(&said));
    }

    /// The selected remote's name as a sentence shows it.
    fn remote_label(&self) -> String {
        self.remote
            .as_deref()
            .map(text::sanitize)
            .unwrap_or_default()
    }

    /// How many agent conversations listed under `id` are not shared at all.
    fn private_agents_of(&self, id: &str) -> usize {
        let Some(index) = self.tree.position(id) else {
            return 0;
        };
        self.tree
            .children_of(index)
            .iter()
            .filter(|&&child| {
                let summary = &self.tree.entries[child].summary;
                !summary.imported && summary.shared_count == 0
            })
            .count()
    }

    // ----- the list -----

    /// The rows of the screen for the current size, mode and notice.
    pub fn layout(&self) -> Layout {
        if self.too_small() {
            return Layout {
                header: 0,
                notice: 0,
                head: 0,
                list: 0,
                strip: 0,
                bottom: 0,
            };
        }
        let header = HEADER_ROWS;
        let notice = self.notice().len();
        let head = 1;
        let strip = if self.size.1 >= STRIP_MIN_HEIGHT {
            STRIP_ROWS
        } else {
            0
        };
        let bottom = self.bottom_lines().len();
        let list = (self.size.1 as usize).saturating_sub(header + notice + head + strip + bottom);
        Layout {
            header,
            notice,
            head,
            list,
            strip,
            bottom,
        }
    }

    pub fn columns(&self) -> &Columns {
        &self.columns
    }

    /// How many rows are listed (collapsed agents are not).
    #[cfg(test)]
    pub fn row_count(&self) -> usize {
        self.visible.len()
    }

    /// The first listed row that is drawn.
    pub fn top(&self) -> usize {
        let rows = self.layout().list.max(1);
        if self.cursor < self.top {
            self.cursor
        } else if self.cursor >= self.top + rows {
            self.cursor + 1 - rows
        } else {
            self.top
        }
    }

    fn ensure_visible(&mut self) {
        self.top = self.top();
        let rows = self.layout().list.max(1);
        let last_top = self.visible.len().saturating_sub(rows);
        self.top = self.top.min(last_top);
    }

    /// The rows in view, with their text.
    pub fn rows(&self) -> Vec<RowView> {
        let has_destination = self.has_destination();
        let top = self.top();
        let rows = self.layout().list;
        self.visible
            .iter()
            .enumerate()
            .skip(top)
            .take(rows)
            .map(|(position, &index)| {
                self.row_view(index, position == self.cursor, has_destination)
            })
            .collect()
    }

    fn row_view(&self, index: usize, selected: bool, has_destination: bool) -> RowView {
        let entry = &self.tree.entries[index];
        let summary = &entry.summary;
        let (state, tone) = words::state_label(summary, has_destination, &self.glyphs);
        let (disclosure, badge) = match &entry.kind {
            Kind::Parent { children } => {
                if self.expanded.contains(&summary.id) {
                    (Disclosure::Expanded, None)
                } else {
                    let agents: Vec<&ConversationSummary> = children
                        .iter()
                        .map(|&child| &self.tree.entries[child].summary)
                        .collect();
                    (
                        Disclosure::Collapsed,
                        Some(words::agents_badge(&agents, has_destination, &self.glyphs)),
                    )
                }
            }
            _ => (Disclosure::None, None),
        };
        let when = match entry.kind {
            Kind::Child { .. } => When::Agent,
            Kind::Orphan => When::OrphanAt(summary.last_activity),
            _ => When::At(summary.last_activity),
        };
        RowView {
            id: text::sanitize(&summary.id),
            disclosure,
            when,
            title: text::title(&summary.title),
            thoughts: summary.thoughts,
            linked: summary.linked,
            state,
            tone,
            badge,
            imported: summary.imported,
            selected,
        }
    }

    fn rebuild_visible(&mut self, keep: Option<String>) {
        let keep = keep.or_else(|| self.selected_id().map(str::to_owned));
        self.visible = self
            .tree
            .visible(|entry| self.expanded.contains(&entry.summary.id));
        self.compute_columns();
        self.restore_cursor(keep);
    }

    fn compute_columns(&mut self) {
        let has_destination = self.has_destination();
        let g = self.glyphs;
        let mut columns = Columns {
            // The header label.
            when_full: 10,
            when_short: 10,
            thoughts: 8,
            linked: 6,
            state: 0,
        };
        for &index in &self.visible {
            let entry = &self.tree.entries[index];
            let summary = &entry.summary;
            let when = match entry.kind {
                Kind::Child { .. } => When::Agent,
                Kind::Orphan => When::OrphanAt(summary.last_activity),
                _ => When::At(summary.last_activity),
            };
            let cell = |short| {
                let (main, tag) = when_cell(when, short, &g);
                text::width(&main) + tag.map_or(0, |tag| text::width(&tag))
            };
            columns.when_full = columns.when_full.max(cell(false));
            columns.when_short = columns.when_short.max(cell(true));
            columns.thoughts = columns.thoughts.max(summary.thoughts.to_string().len());
            columns.linked = columns.linked.max(summary.linked.to_string().len());
            let (state, _) = words::state_label(summary, has_destination, &g);
            columns.state = columns.state.max(text::width(&state));
        }
        self.columns = columns;
    }

    fn restore_cursor(&mut self, keep: Option<String>) {
        let len = self.visible.len();
        if len == 0 {
            self.cursor = 0;
            self.top = 0;
            return;
        }
        let position_of = |model: &Self, id: &str| {
            model
                .visible
                .iter()
                .position(|&index| model.tree.entries[index].summary.id == id)
        };
        self.cursor = match keep {
            Some(id) => position_of(self, &id)
                .or_else(|| {
                    // Listed but hidden under a collapsed parent: its parent.
                    let parent = self.tree.parent_of(self.tree.position(&id)?)?;
                    position_of(self, &self.tree.entries[parent].summary.id)
                })
                .unwrap_or_else(|| self.cursor.min(len - 1)),
            None => self.cursor.min(len - 1),
        };
        self.ensure_visible();
    }

    fn move_to(&mut self, position: usize) {
        if self.visible.is_empty() {
            return;
        }
        self.cursor = position.min(self.visible.len() - 1);
        self.ensure_visible();
    }

    fn move_by(&mut self, delta: isize) {
        let target = self.cursor.saturating_add_signed(delta);
        self.move_to(target);
    }

    fn page(&self) -> isize {
        (self.layout().list.max(2) - 1) as isize
    }

    fn expand_selected(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if matches!(entry.kind, Kind::Parent { .. }) {
            let id = entry.summary.id.clone();
            if self.expanded.insert(id) {
                self.rebuild_visible(None);
            }
        }
    }

    /// Left: hides an open parent's agents; on an agent, goes to its parent.
    fn collapse_or_ascend(&mut self) {
        let Some(&index) = self.visible.get(self.cursor) else {
            return;
        };
        match self.tree.entries[index].kind {
            Kind::Parent { .. } => {
                let id = self.tree.entries[index].summary.id.clone();
                if self.expanded.remove(&id) {
                    self.rebuild_visible(None);
                }
            }
            Kind::Child { parent } => {
                if let Some(position) = self.visible.iter().position(|&i| i == parent) {
                    self.move_to(position);
                }
            }
            _ => {}
        }
    }

    // ----- what the bottom line, the header and the overlays say -----

    /// The conversation the strip, the details and the bars are about, with
    /// what is known of its place in the tree.
    fn details_input<'a>(
        &'a self,
        entry: &'a Entry,
        info: Option<&'a RowInfo>,
    ) -> DetailsInput<'a> {
        let lineage = match entry.kind {
            Kind::Child { parent } => {
                Lineage::AgentOf(text::title(&self.tree.entries[parent].summary.title))
            }
            Kind::Orphan => Lineage::OrphanAgent,
            _ => Lineage::None,
        };
        // The numbers read for this row just now beat the ones the list was
        // loaded with.
        let summary = info.map_or(&entry.summary, |info| &info.details.summary);
        DetailsInput {
            summary,
            details: info.map(|info| &info.details),
            lineage,
            agents: match &entry.kind {
                Kind::Parent { children } => children.len(),
                _ => 0,
            },
            remote: self.usable_remote(),
        }
    }

    pub fn header(&self) -> words::HeaderText {
        let waiting = self.counts.map(|counts| counts.unpublished());
        words::header(&self.remote_view(), waiting, &self.glyphs)
    }

    /// The lines under the list while the strip is drawn.
    pub fn strip(&self) -> Vec<String> {
        let Some(entry) = self.selected_entry() else {
            return Vec::new();
        };
        let input = self.details_input(entry, self.selected_info());
        words::strip_lines(&input, &self.glyphs)
    }

    fn available(&self) -> Available {
        let open = |bar: Bar| !matches!(bar, Bar::Blocked(_));
        Available {
            details: self.selected_entry().is_some(),
            share: open(self.share_bar()),
            unshare: open(self.unshare_bar()),
            publish: open(self.publish_bar()),
        }
    }

    /// What the bottom of the screen shows: the key bar of the list, the hint
    /// of an overlay, or a pre-flight bar. One to three lines.
    pub fn bottom_lines(&self) -> Vec<Vec<Seg>> {
        let width = self.size.0 as usize;
        match &self.mode {
            Mode::List => vec![words::key_bar(&self.available(), width, &self.glyphs)],
            Mode::Details { .. } | Mode::Help { .. } => {
                vec![words::overlay_bar(true, &self.glyphs)]
            }
            Mode::Remotes { .. } => vec![words::picker_bar(&self.glyphs)],
            // The view puts a space before a bar's lines, as before the key bar.
            Mode::Bar(bar) => self.bar_lines(bar, width.saturating_sub(1)),
        }
    }

    fn bar_lines(&self, bar: &Bar, width: usize) -> Vec<Vec<Seg>> {
        let g = &self.glyphs;
        let Some(remote) = self.usable_remote() else {
            return match bar {
                Bar::Blocked(reason) => words::blocked_bar(reason, width),
                _ => words::blocked_bar(&self.remote_view().problem().unwrap_or_default(), width),
            };
        };
        let dest = Destination::of(remote, g);
        match bar {
            Bar::Share { ask, .. } => words::share_bar(ask, &dest, width, g),
            Bar::Unshare { title, shared, .. } => {
                words::unshare_bar(title, *shared, &dest, width, g)
            }
            Bar::Publish { waiting } => words::publish_bar(*waiting, &dest, width),
            Bar::Authorize => words::authorize_bar(&dest, width),
            Bar::Blocked(reason) => words::blocked_bar(reason, width),
        }
    }

    /// The bar open now, if any.
    pub fn bar(&self) -> Option<&Bar> {
        match &self.mode {
            Mode::Bar(bar) => Some(bar),
            _ => None,
        }
    }

    pub fn overlay(&self) -> Option<Overlay> {
        match &self.mode {
            Mode::Details { scroll } => {
                let entry = self.selected_entry()?;
                let input = self.details_input(entry, self.selected_info());
                Some(Overlay::Page {
                    page: words::details_page(&input, &self.glyphs),
                    scroll: *scroll,
                })
            }
            Mode::Help { scroll } => Some(Overlay::Page {
                page: words::help_page(&self.glyphs),
                scroll: *scroll,
            }),
            Mode::Remotes { cursor } => Some(Overlay::Remotes {
                rows: words::remote_rows(&self.remotes, self.remote.as_deref(), &self.glyphs),
                cursor: *cursor,
            }),
            _ => None,
        }
    }

    /// The columns and rows of text an overlay can hold at the current size.
    pub fn overlay_text_area(&self) -> (usize, usize) {
        let width = (self.size.0 as usize).min(OVERLAY_MAX_WIDTH);
        let height = (self.size.1 as usize).saturating_sub(HEADER_ROWS + self.bottom_lines().len());
        // A border and a column of padding on each side; a border above and below.
        (width.saturating_sub(4), height.saturating_sub(2))
    }

    fn page_lines(&self) -> usize {
        let (width, _) = self.overlay_text_area();
        match self.overlay() {
            Some(Overlay::Page { page, .. }) => layout_page(&page, width).len(),
            _ => 0,
        }
    }

    fn clamp_scroll(&mut self) {
        let (_, height) = self.overlay_text_area();
        let max = self.page_lines().saturating_sub(height);
        match &mut self.mode {
            Mode::Details { scroll } | Mode::Help { scroll } => *scroll = (*scroll).min(max),
            _ => {}
        }
    }

    fn scroll_page(&mut self, key: Key) {
        let (_, height) = self.overlay_text_area();
        let max = self.page_lines().saturating_sub(height);
        let page = height.saturating_sub(1).max(1);
        if let Mode::Details { scroll } | Mode::Help { scroll } = &mut self.mode {
            *scroll = match key {
                Key::Up | Key::Char('k') => scroll.saturating_sub(1),
                Key::Down | Key::Char('j') => (*scroll + 1).min(max),
                Key::PageUp => scroll.saturating_sub(page),
                Key::PageDown => (*scroll + page).min(max),
                Key::Home => 0,
                Key::End => max,
                _ => *scroll,
            };
        }
    }

    // ----- keys -----

    /// Applies a key press. Returns what the event loop is to do about it, if
    /// anything. A notice from before lasts until this call.
    pub fn handle_key(&mut self, event: KeyEvent) -> Option<Action> {
        self.status = None;
        let key = Key::of(event);
        if key == Key::Interrupt {
            return Some(Action::Quit);
        }
        if self.too_small() {
            // Nothing else is drawn, so nothing else may happen.
            return (key == Key::Char('q')).then_some(Action::Quit);
        }
        match self.mode {
            Mode::List => self.list_key(key),
            Mode::Details { .. } | Mode::Help { .. } => self.page_key(key),
            Mode::Remotes { .. } => self.picker_key(key),
            Mode::Bar(_) => self.bar_key(key),
        }
    }

    fn list_key(&mut self, key: Key) -> Option<Action> {
        match key {
            Key::Up | Key::Char('k') => self.move_by(-1),
            Key::Down | Key::Char('j') => self.move_by(1),
            Key::PageUp => self.move_by(-self.page()),
            Key::PageDown => self.move_by(self.page()),
            Key::Home => self.move_to(0),
            Key::End => self.move_to(usize::MAX),
            Key::Right | Key::Char('l') => self.expand_selected(),
            Key::Left | Key::Char('h') => self.collapse_or_ascend(),
            Key::Enter => {
                if self.selected_entry().is_some() {
                    self.mode = Mode::Details { scroll: 0 };
                }
            }
            Key::Char('s') => self.mode = Mode::Bar(self.share_bar()),
            Key::Char('u') => self.mode = Mode::Bar(self.unshare_bar()),
            Key::Char('p') => self.mode = Mode::Bar(self.publish_bar()),
            Key::Char('r') => self.open_picker(),
            Key::Char('?') => self.mode = Mode::Help { scroll: 0 },
            Key::Char('q') => return Some(Action::Quit),
            _ => {}
        }
        None
    }

    fn page_key(&mut self, key: Key) -> Option<Action> {
        let is_help = matches!(self.mode, Mode::Help { .. });
        match key {
            Key::Enter | Key::Esc | Key::Char('q') => self.mode = Mode::List,
            Key::Char('?') if is_help => self.mode = Mode::List,
            Key::Up
            | Key::Down
            | Key::PageUp
            | Key::PageDown
            | Key::Home
            | Key::End
            | Key::Char('k')
            | Key::Char('j') => self.scroll_page(key),
            _ => {}
        }
        None
    }

    fn open_picker(&mut self) {
        let usable = |remote: &RemoteOverview| remote.fingerprint.is_ok();
        let cursor = self
            .remote
            .as_deref()
            .and_then(|name| {
                self.remotes
                    .iter()
                    .position(|r| r.name == name && usable(r))
            })
            .or_else(|| self.remotes.iter().position(usable))
            .unwrap_or(0);
        self.mode = Mode::Remotes { cursor };
    }

    fn picker_key(&mut self, key: Key) -> Option<Action> {
        let Mode::Remotes { cursor } = self.mode else {
            return None;
        };
        let usable = |index: usize| {
            self.remotes
                .get(index)
                .is_some_and(|remote| remote.fingerprint.is_ok())
        };
        match key {
            Key::Esc | Key::Char('q') => self.mode = Mode::List,
            Key::Up | Key::Char('k') => {
                if let Some(previous) = (0..cursor).rev().find(|&i| usable(i)) {
                    self.mode = Mode::Remotes { cursor: previous };
                }
            }
            Key::Down | Key::Char('j') => {
                if let Some(next) = (cursor + 1..self.remotes.len()).find(|&i| usable(i)) {
                    self.mode = Mode::Remotes { cursor: next };
                }
            }
            Key::Home => {
                if let Some(first) = (0..self.remotes.len()).find(|&i| usable(i)) {
                    self.mode = Mode::Remotes { cursor: first };
                }
            }
            Key::End => {
                if let Some(last) = (0..self.remotes.len()).rev().find(|&i| usable(i)) {
                    self.mode = Mode::Remotes { cursor: last };
                }
            }
            Key::Enter => {
                self.mode = Mode::List;
                // Choosing a remote selects it. It authorizes nothing.
                if usable(cursor) {
                    let name = self.remotes[cursor].name.clone();
                    if self.remote.as_deref() != Some(name.as_str()) {
                        return Some(Action::Switch(name));
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn bar_key(&mut self, key: Key) -> Option<Action> {
        let Mode::Bar(bar) = self.mode.clone() else {
            return None;
        };
        // A blocked bar says why and closes on any key.
        if let Bar::Blocked(_) = bar {
            self.mode = Mode::List;
            return None;
        }
        if key == Key::Esc {
            self.mode = Mode::List;
            return None;
        }
        // Everything else is ignored unless the bar lists it.
        let flow = match (&bar, key) {
            (Bar::Share { id, ask }, Key::Enter) if ask.unshared > 0 => Some(Flow::Share {
                id: id.clone(),
                future: FutureSharePolicy::Private,
            }),
            (Bar::Share { id, .. }, Key::Char('f')) => Some(Flow::Share {
                id: id.clone(),
                future: FutureSharePolicy::Shared,
            }),
            (Bar::Unshare { id, .. }, Key::Enter) => Some(Flow::Unshare { id: id.clone() }),
            (Bar::Publish { .. }, Key::Enter) => Some(Flow::Publish),
            (Bar::Authorize, Key::Enter) => Some(Flow::Authorize),
            _ => None,
        };
        flow.map(|flow| {
            self.mode = Mode::List;
            Action::Run(flow)
        })
    }

    // ----- opening a bar -----

    /// The remote an action is for, or why there is none: this platform cannot
    /// run consent actions, the remote cannot be a destination, or its privacy
    /// state cannot be read.
    fn action_remote(&self) -> Result<&RemoteOverview, String> {
        if !self.consent_actions {
            return Err(words::NOT_ON_THIS_PLATFORM.to_owned());
        }
        match self.remote_view() {
            RemoteView::Usable(overview) => match &overview.consent_error {
                Some(reason) => Err(words::privacy_unreadable(reason)),
                None => Ok(overview),
            },
            other => Err(other.problem().unwrap_or_default()),
        }
    }

    /// The selected conversation and what was read about it, or why neither is
    /// there to act on.
    fn selected_for_action(&self) -> Result<(&Entry, &RowInfo), String> {
        let entry = self
            .selected_entry()
            .ok_or_else(|| "There is no conversation to act on.".to_owned())?;
        let info = self
            .selected_info()
            .ok_or_else(|| words::NO_DETAILS.to_owned())?;
        Ok((entry, info))
    }

    fn share_bar(&self) -> Bar {
        let decide = || -> Result<Bar, String> {
            let remote = self.action_remote()?;
            let (entry, info) = self.selected_for_action()?;
            let summary = &info.details.summary;
            if summary.imported {
                return Err(words::IMPORTED_IS_READ_ONLY.to_owned());
            }
            let future_on = summary.share_future == Some(true);
            let unshared = info.details.snapshot_new as i64;
            if unshared == 0 && future_on {
                return Err(words::all_shared_for_good(&text::sanitize(&remote.name)));
            }
            if !remote.sharing_consented {
                return Ok(Bar::Authorize);
            }
            Ok(Bar::Share {
                id: entry.summary.id.clone(),
                ask: ShareAsk {
                    title: text::title(&summary.title),
                    thoughts: info.details.snapshot_count as i64,
                    unshared,
                    future_on,
                },
            })
        };
        decide().unwrap_or_else(Bar::Blocked)
    }

    fn unshare_bar(&self) -> Bar {
        let decide = || -> Result<Bar, String> {
            let remote = self.action_remote()?;
            let name = text::sanitize(&remote.name);
            let (entry, info) = self.selected_for_action()?;
            let summary = &info.details.summary;
            if summary.imported {
                return Err(words::IMPORTED_IS_READ_ONLY.to_owned());
            }
            if summary.shared_count == 0 {
                return Err(words::nothing_to_unshare(&name));
            }
            if let Some(refusal) = info
                .unshare
                .as_ref()
                .and_then(|check| words::unshare_refusal(check, &name))
            {
                return Err(refusal);
            }
            Ok(Bar::Unshare {
                id: entry.summary.id.clone(),
                title: text::title(&summary.title),
                shared: summary.shared_count,
            })
        };
        decide().unwrap_or_else(Bar::Blocked)
    }

    fn publish_bar(&self) -> Bar {
        let decide = || -> Result<Bar, String> {
            let remote = self.action_remote()?;
            if !remote.sharing_consented {
                return Ok(Bar::Authorize);
            }
            Ok(Bar::Publish {
                waiting: self.counts.map_or(0, |counts| counts.unpublished()),
            })
        };
        decide().unwrap_or_else(Bar::Blocked)
    }
}

/// A line of an overlay after wrapping: its text and how it is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageLine {
    pub kind: PageLineKind,
    /// The label of a field, on its first line only.
    pub label: Option<String>,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageLineKind {
    Title,
    Blank,
    Heading,
    Field,
    Text,
}

/// The width of the label column of a field.
pub const FIELD_LABEL_WIDTH: usize = 17;

/// A field: the label on the first line, and the value wrapped past the label
/// column.
fn field_lines(label: &str, value: &str, width: usize) -> Vec<PageLine> {
    let value_width = width.saturating_sub(FIELD_LABEL_WIDTH).max(8);
    text::wrap(value, value_width)
        .into_iter()
        .enumerate()
        .map(|(index, text)| PageLine {
            kind: PageLineKind::Field,
            label: (index == 0).then(|| label.to_owned()),
            text,
        })
        .collect()
}

/// A page wrapped to `width` columns. A field's value wraps under its own
/// first line, past the label column.
pub fn layout_page(page: &[Page], width: usize) -> Vec<PageLine> {
    let mut lines = Vec::new();
    for row in page {
        match row {
            Page::Blank => lines.push(PageLine {
                kind: PageLineKind::Blank,
                label: None,
                text: String::new(),
            }),
            Page::Title(title) => {
                lines.extend(text::wrap(title, width).into_iter().map(|text| PageLine {
                    kind: PageLineKind::Title,
                    label: None,
                    text,
                }))
            }
            Page::Heading(heading) => {
                lines.extend(text::wrap(heading, width).into_iter().map(|text| PageLine {
                    kind: PageLineKind::Heading,
                    label: None,
                    text,
                }))
            }
            Page::Text(body) => {
                lines.extend(text::wrap(body, width).into_iter().map(|text| PageLine {
                    kind: PageLineKind::Text,
                    label: None,
                    text,
                }))
            }
            Page::Field { label, value } => {
                lines.extend(field_lines(label, value, width));
            }
            Page::Item { label, value } => {
                lines.extend(field_lines(label, value, width));
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::tree::tests::conversation;
    use cvc_core::db::ContextPathUse;
    use ratatui::crossterm::event::KeyEventKind;

    const FP: &str = "8348b79e2c41aabbccddeeff00112233445566778899aabbccddeeff00112233";

    fn remote(name: &str) -> RemoteOverview {
        RemoteOverview {
            name: name.to_owned(),
            fingerprint: Ok(FP.to_owned()),
            location: Some("github.com/acme/repo".to_owned()),
            sharing_consented: true,
            auto_push: false,
            consent_error: None,
        }
    }

    fn data(summaries: Vec<ConversationSummary>, remotes: Vec<RemoteOverview>) -> Data {
        Data {
            summaries,
            remotes,
            counts: Some(DestinationPublicationCounts {
                shared: 4,
                published: 2,
                unconfirmed: 0,
            }),
        }
    }

    fn settings(consent_actions: bool) -> Settings {
        Settings {
            consent_actions,
            glyphs: Glyphs::UNICODE,
        }
    }

    fn model_with(summaries: Vec<ConversationSummary>, remotes: Vec<RemoteOverview>) -> Model {
        let mut model = Model::new(data(summaries, remotes), None, settings(true));
        model.set_size(100, 30);
        model
    }

    /// Three conversations, newest first, none shared.
    fn simple() -> Vec<ConversationSummary> {
        vec![
            conversation("alpha", 100, 900, 5),
            conversation("beta", 100, 800, 3),
            conversation("gamma", 100, 700, 7),
        ]
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    fn ch(c: char) -> KeyEvent {
        press(KeyCode::Char(c))
    }

    fn selected(model: &Model) -> String {
        model.selected_id().unwrap().to_owned()
    }

    /// What reading `id` for the selected remote gives.
    fn info_of(summary: &ConversationSummary, new: usize, check: UnshareCheck) -> RowInfo {
        let count = summary.thoughts as usize;
        RowInfo {
            details: ConversationDetails {
                summary: summary.clone(),
                context_paths: vec![ContextPathUse {
                    path: "src/lib.rs".to_owned(),
                    interactions: 2,
                }],
                context_path_total: 1,
                snapshot_count: count,
                snapshot_shared: count - new,
                snapshot_new: new,
            },
            unshare: Some(check),
        }
    }

    /// Reads the selected row, the way the loop does before a key.
    fn read_selected(model: &mut Model, new: usize, check: UnshareCheck) {
        let id = selected(model);
        let summary = model
            .tree
            .entries
            .iter()
            .find(|e| e.summary.id == id)
            .unwrap()
            .summary
            .clone();
        model.set_info(&id, Some(info_of(&summary, new, check)));
    }

    fn open_bar(model: &mut Model, key: char) -> Bar {
        assert_eq!(
            model.handle_key(ch(key)),
            None,
            "opening a bar runs nothing"
        );
        model.bar().cloned().expect("a bar should be open")
    }

    #[test]
    fn the_list_is_selected_at_the_newest_conversation() {
        let model = model_with(simple(), vec![remote("origin")]);
        assert_eq!(selected(&model), "alpha");
        assert_eq!(model.row_count(), 3);
        let ids: Vec<_> = model.rows().into_iter().map(|row| row.id).collect();
        assert_eq!(ids, vec!["alpha", "beta", "gamma"]);
        assert!(model.rows()[0].selected && !model.rows()[1].selected);
    }

    #[test]
    fn conversations_without_thoughts_are_hidden() {
        let mut summaries = simple();
        summaries.push(conversation("empty", 100, 950, 0));
        let model = model_with(summaries, vec![remote("origin")]);
        assert_eq!(model.row_count(), 3);
        assert!(model.rows().iter().all(|row| row.id != "empty"));
    }

    #[test]
    fn movement_keys_move_the_selection() {
        let mut model = model_with(simple(), vec![remote("origin")]);
        assert_eq!(model.handle_key(press(KeyCode::Down)), None);
        assert_eq!(selected(&model), "beta");
        model.handle_key(ch('j'));
        assert_eq!(selected(&model), "gamma");
        // The end of the list holds.
        model.handle_key(press(KeyCode::Down));
        assert_eq!(selected(&model), "gamma");
        model.handle_key(press(KeyCode::Up));
        model.handle_key(ch('k'));
        assert_eq!(selected(&model), "alpha");
        model.handle_key(press(KeyCode::Up));
        assert_eq!(selected(&model), "alpha");
        model.handle_key(press(KeyCode::End));
        assert_eq!(selected(&model), "gamma");
        model.handle_key(press(KeyCode::Home));
        assert_eq!(selected(&model), "alpha");
        model.handle_key(press(KeyCode::PageDown));
        assert_eq!(selected(&model), "gamma");
        model.handle_key(press(KeyCode::PageUp));
        assert_eq!(selected(&model), "alpha");
    }

    #[test]
    fn paging_moves_by_a_screen_and_keeps_the_selection_in_view() {
        let summaries: Vec<_> = (0..100)
            .map(|i| conversation(&format!("c{i:03}"), 0, 10_000 - i, 1))
            .collect();
        let mut model = model_with(summaries, vec![remote("origin")]);
        model.set_size(100, 20);
        let list = model.layout().list;
        assert!(list > 5 && list < 20);
        model.handle_key(press(KeyCode::PageDown));
        assert_eq!(model.cursor, list - 1);
        model.handle_key(press(KeyCode::PageDown));
        assert_eq!(model.cursor, 2 * (list - 1));
        let shown: Vec<_> = model.rows().into_iter().map(|row| row.id).collect();
        assert_eq!(shown.len(), list);
        assert!(shown.contains(&selected(&model)));
        model.handle_key(press(KeyCode::End));
        assert_eq!(selected(&model), "c099");
        assert!(model.rows().last().unwrap().selected);
        model.handle_key(press(KeyCode::Home));
        assert_eq!(model.top(), 0);
    }

    fn with_agents() -> Vec<ConversationSummary> {
        vec![
            conversation("session", 100, 900, 10),
            conversation("session:agent-a", 200, 300, 2),
            conversation("session:agent-b", 250, 400, 3),
            conversation("other", 100, 500, 1),
        ]
    }

    #[test]
    fn parents_start_collapsed_with_a_badge_and_expand_with_right() {
        let mut model = model_with(with_agents(), vec![remote("origin")]);
        let rows = model.rows();
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["session", "other"]
        );
        assert_eq!(rows[0].disclosure, Disclosure::Collapsed);
        assert_eq!(rows[0].badge.as_deref(), Some("+2 agents · 2 private"));
        assert_eq!(rows[1].disclosure, Disclosure::None);
        assert_eq!(rows[1].badge, None);

        model.handle_key(press(KeyCode::Right));
        let rows = model.rows();
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["session", "session:agent-a", "session:agent-b", "other"]
        );
        assert_eq!(rows[0].disclosure, Disclosure::Expanded);
        assert_eq!(rows[0].badge, None);
        assert_eq!(rows[1].when, When::Agent);
        // Selecting stays on the parent.
        assert_eq!(selected(&model), "session");
    }

    #[test]
    fn collapsing_and_expanding_use_either_key_style() {
        let mut model = model_with(with_agents(), vec![remote("origin")]);
        model.handle_key(ch('l'));
        assert_eq!(model.row_count(), 4);
        model.handle_key(ch('h'));
        assert_eq!(model.row_count(), 2);
        // Right on a row without agents does nothing.
        model.handle_key(press(KeyCode::Down));
        model.handle_key(press(KeyCode::Right));
        assert_eq!(model.row_count(), 2);
        assert_eq!(selected(&model), "other");
    }

    #[test]
    fn navigation_skips_collapsed_agents() {
        let mut model = model_with(with_agents(), vec![remote("origin")]);
        model.handle_key(press(KeyCode::Down));
        assert_eq!(selected(&model), "other");
        model.handle_key(press(KeyCode::Up));
        assert_eq!(selected(&model), "session");
        model.handle_key(press(KeyCode::End));
        assert_eq!(selected(&model), "other");
        // Expanded, the agents are visited in order.
        model.handle_key(press(KeyCode::Home));
        model.handle_key(ch('l'));
        let mut visited = vec![selected(&model)];
        for _ in 0..3 {
            model.handle_key(press(KeyCode::Down));
            visited.push(selected(&model));
        }
        assert_eq!(
            visited,
            vec!["session", "session:agent-a", "session:agent-b", "other"]
        );
    }

    #[test]
    fn left_on_an_agent_goes_to_its_parent_and_a_second_left_collapses() {
        let mut model = model_with(with_agents(), vec![remote("origin")]);
        model.handle_key(ch('l'));
        model.handle_key(press(KeyCode::Down));
        model.handle_key(press(KeyCode::Down));
        assert_eq!(selected(&model), "session:agent-b");
        model.handle_key(press(KeyCode::Left));
        assert_eq!(selected(&model), "session");
        assert_eq!(model.row_count(), 4, "going up does not collapse");
        model.handle_key(press(KeyCode::Left));
        assert_eq!(model.row_count(), 2);
        assert_eq!(selected(&model), "session");
    }

    #[test]
    fn an_orphan_agent_is_a_top_level_row_with_a_tag() {
        let model = model_with(
            vec![
                conversation("lonely:agent-x", 100, 900, 2),
                conversation("plain", 100, 500, 1),
            ],
            vec![remote("origin")],
        );
        let rows = model.rows();
        assert_eq!(rows[0].id, "lonely:agent-x");
        assert_eq!(rows[0].when, When::OrphanAt(900));
        assert_eq!(rows[0].disclosure, Disclosure::None);
        let (main, tag) = when_cell(rows[0].when, false, &Glyphs::UNICODE);
        assert_eq!(main, words::format_when(900, false));
        assert_eq!(tag.as_deref(), Some(" agent"));
        // And the child cell.
        assert_eq!(
            when_cell(When::Agent, false, &Glyphs::UNICODE),
            ("└─".to_owned(), Some(" agent".to_owned()))
        );
        assert_eq!(
            when_cell(When::Agent, false, &Glyphs::ASCII),
            ("`-".to_owned(), Some(" agent".to_owned()))
        );
    }

    #[test]
    fn column_widths_follow_what_is_listed() {
        let mut summaries = simple();
        summaries[0].shared_count = 5;
        summaries[0].published = 5;
        summaries[0].shared = true;
        summaries[1].thoughts = 12_345;
        let model = model_with(summaries, vec![remote("origin")]);
        let columns = model.columns();
        assert_eq!(columns.when_full, 16);
        assert_eq!(columns.when_short, 11);
        assert_eq!(columns.thoughts, 8);
        assert_eq!(columns.linked, 6);
        // The longest label among the rows: "● published".
        assert_eq!(columns.state, text::width("● published"));
        // A collapsed parent's agents do not count; expanding it grows the
        // when column for an orphan's tag.
        let model = model_with(
            vec![conversation("a:agent-x", 100, 900, 2)],
            vec![remote("origin")],
        );
        assert_eq!(model.columns().when_full, 16 + 6);
    }

    #[test]
    fn rows_show_the_state_for_the_selected_remote() {
        let mut summaries = simple();
        summaries[0].shared_count = 5;
        summaries[0].published = 3;
        summaries[0].shared = true;
        summaries[1].imported = true;
        let model = model_with(summaries, vec![remote("origin")]);
        let rows = model.rows();
        assert_eq!(rows[0].state, "◐ shared · 2 waiting");
        assert_eq!(rows[0].tone, Tone::Waiting);
        assert_eq!(rows[1].state, "imported");
        assert!(rows[1].imported);
        assert_eq!(rows[2].state, "○ private");
    }

    #[test]
    fn without_a_usable_remote_the_state_column_is_a_dash() {
        let model = model_with(simple(), vec![]);
        assert!(model.rows().iter().all(|row| row.state == "–"));
        let model = model_with(simple(), vec![remote("upstream")]);
        // No origin: nothing is selected.
        assert!(model.rows().iter().all(|row| row.state == "–"));
    }

    #[test]
    fn titles_are_sanitized_and_an_empty_one_is_untitled() {
        let mut summaries = simple();
        summaries[0].title = "evil\u{1b}[2J\u{202e}title".to_owned();
        summaries[1].title = "   ".to_owned();
        let model = model_with(summaries, vec![remote("origin")]);
        let rows = model.rows();
        assert_eq!(rows[0].title, "evil?[2J?title");
        assert_eq!(rows[1].title, "(untitled)");
    }

    // ----- the remote -----

    #[test]
    fn the_view_starts_on_the_requested_remote_else_origin_else_none() {
        let remotes = vec![remote("origin"), remote("upstream")];
        let model = Model::new(
            data(simple(), remotes.clone()),
            Some("upstream"),
            settings(true),
        );
        assert_eq!(model.remote_name(), Some("upstream"));
        let model = Model::new(data(simple(), remotes.clone()), None, settings(true));
        assert_eq!(model.remote_name(), Some("origin"));
        let model = Model::new(
            data(simple(), vec![remote("upstream")]),
            None,
            settings(true),
        );
        assert_eq!(model.remote_name(), None);
        assert!(matches!(model.remote_view(), RemoteView::NoneSelected));
        let model = Model::new(data(simple(), vec![]), None, settings(true));
        assert!(matches!(model.remote_view(), RemoteView::NoRemotes));
        let model = Model::new(data(simple(), remotes), Some("typo"), settings(true));
        assert!(matches!(
            model.remote_view(),
            RemoteView::Missing {
                name: "typo",
                others_exist: true
            }
        ));
    }

    #[test]
    fn an_unusable_remote_is_shown_as_such() {
        let mut broken = remote("origin");
        broken.fingerprint = Err("remote has no push URL".to_owned());
        let model = model_with(simple(), vec![broken]);
        assert!(matches!(model.remote_view(), RemoteView::Unusable(_)));
        assert_eq!(model.destination_fingerprint(), None);
        assert!(model.selected_remote().is_none());
    }

    #[test]
    fn the_picker_selects_a_remote_without_authorizing_anything() {
        let mut other = remote("upstream");
        other.sharing_consented = false;
        let mut model = model_with(simple(), vec![remote("origin"), other]);
        assert_eq!(model.handle_key(ch('r')), None);
        let Some(Overlay::Remotes { rows, cursor }) = model.overlay() else {
            panic!("the picker should be open");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(cursor, 0, "starts on the current remote");
        model.handle_key(press(KeyCode::Down));
        // Enter chooses the remote and asks for its data; it runs no flow.
        assert_eq!(
            model.handle_key(press(KeyCode::Enter)),
            Some(Action::Switch("upstream".to_owned()))
        );
        assert!(model.overlay().is_none());
        assert!(model.bar().is_none());
        // Until the data is loaded the view stays on the remote it shows.
        assert_eq!(model.remote_name(), Some("origin"));
        let mut unauthorized = remote("upstream");
        unauthorized.sharing_consented = false;
        model.switch_to(
            "upstream".to_owned(),
            data(simple(), vec![remote("origin"), unauthorized]),
        );
        assert_eq!(model.remote_name(), Some("upstream"));
        // Not authorized, and still not.
        assert!(!model.selected_remote().unwrap().sharing_consented);
    }

    #[test]
    fn choosing_the_current_remote_again_reloads_nothing() {
        let mut model = model_with(simple(), vec![remote("origin"), remote("upstream")]);
        model.handle_key(ch('r'));
        assert_eq!(model.handle_key(press(KeyCode::Enter)), None);
        assert_eq!(model.remote_name(), Some("origin"));
    }

    #[test]
    fn the_picker_skips_unusable_remotes_and_esc_goes_back() {
        let mut broken = remote("broken");
        broken.fingerprint = Err("remote has an empty push URL".to_owned());
        let mut model = model_with(simple(), vec![broken, remote("origin"), remote("zeta")]);
        model.handle_key(ch('r'));
        let cursor = |model: &Model| match model.overlay() {
            Some(Overlay::Remotes { cursor, .. }) => cursor,
            _ => panic!("no picker"),
        };
        assert_eq!(cursor(&model), 1, "starts on origin, not the broken remote");
        model.handle_key(press(KeyCode::Up));
        assert_eq!(cursor(&model), 1, "the broken remote cannot be reached");
        model.handle_key(press(KeyCode::Down));
        assert_eq!(cursor(&model), 2);
        model.handle_key(press(KeyCode::Down));
        assert_eq!(cursor(&model), 2);
        model.handle_key(press(KeyCode::Esc));
        assert!(model.overlay().is_none());
        assert_eq!(model.remote_name(), Some("origin"));
        assert_eq!(model.handle_key(ch('r')), None);
        model.handle_key(ch('q'));
        assert!(
            model.overlay().is_none(),
            "q closes the picker, not the view"
        );
    }

    #[test]
    fn the_picker_opens_with_no_remotes_and_enter_just_closes_it() {
        let mut model = model_with(simple(), vec![]);
        model.handle_key(ch('r'));
        assert!(matches!(model.overlay(), Some(Overlay::Remotes { .. })));
        assert_eq!(model.handle_key(press(KeyCode::Enter)), None);
        assert!(model.overlay().is_none());
    }

    #[test]
    fn a_reload_keeps_the_selection_on_the_same_conversation() {
        let mut model = model_with(with_agents(), vec![remote("origin")]);
        model.handle_key(ch('l'));
        model.handle_key(press(KeyCode::Down));
        model.handle_key(press(KeyCode::Down));
        assert_eq!(selected(&model), "session:agent-b");
        // A newer conversation arrives above it.
        let mut summaries = with_agents();
        summaries.push(conversation("newest", 100, 9_999, 1));
        model.apply(data(summaries, vec![remote("origin")]));
        assert_eq!(selected(&model), "session:agent-b");
        assert_eq!(model.row_count(), 5, "the open parent stays open");
        // If it is gone, the selection stays at the same place in the list.
        model.apply(data(simple(), vec![remote("origin")]));
        assert!(model.selected_id().is_some());
        // A reload drops what was read about rows.
        model.set_info(&selected(&model), None);
        assert_eq!(model.wants_info(), None);
        model.apply(data(simple(), vec![remote("origin")]));
        assert!(model.wants_info().is_some());
    }

    #[test]
    fn a_selected_agent_whose_parent_is_collapsed_by_a_reload_selects_the_parent() {
        let mut model = model_with(with_agents(), vec![remote("origin")]);
        model.handle_key(ch('l'));
        model.handle_key(press(KeyCode::Down));
        assert_eq!(selected(&model), "session:agent-a");
        model.expanded.clear();
        model.rebuild_visible(None);
        assert_eq!(selected(&model), "session");
    }

    // ----- keys that open bars never run a flow -----

    fn shared_summary(thoughts: i64, shared: i64, published: i64) -> ConversationSummary {
        let mut s = conversation("alpha", 100, 900, thoughts);
        s.shared_count = shared;
        s.shared = shared > 0;
        s.published = published;
        s
    }

    fn model_for_actions(summary: ConversationSummary) -> Model {
        let mut model = model_with(vec![summary], vec![remote("origin")]);
        read_selected(&mut model, 0, UnshareCheck::default());
        model
    }

    /// A model whose one conversation has `new` thoughts a share would add.
    fn model_with_new(summary: ConversationSummary, new: usize) -> Model {
        let mut model = model_with(vec![summary], vec![remote("origin")]);
        read_selected(&mut model, new, UnshareCheck::default());
        model
    }

    #[test]
    fn s_u_and_p_only_open_a_bar_in_the_list() {
        let mut model = model_with_new(shared_summary(27, 21, 21), 6);
        for key in ['s', 'u', 'p', 'S', 'U', 'P'] {
            assert_eq!(model.handle_key(ch(key)), None, "{key}");
            assert!(
                matches!(model.bar(), Some(bar) if !matches!(bar, Bar::Blocked(_))),
                "{key}"
            );
            // Esc closes it again.
            assert_eq!(model.handle_key(press(KeyCode::Esc)), None);
            assert!(model.bar().is_none());
        }
    }

    #[test]
    fn the_share_bar_emits_a_flow_only_on_the_keys_it_lists() {
        let mut model = model_with_new(shared_summary(27, 21, 21), 6);
        let Bar::Share { id, ask } = open_bar(&mut model, 's') else {
            panic!("expected the share bar");
        };
        assert_eq!(id, "alpha");
        assert_eq!((ask.thoughts, ask.unshared, ask.future_on), (27, 6, false));

        // Keys the bar does not list do nothing, however often they repeat.
        for key in [
            ch('s'),
            ch('u'),
            ch('p'),
            ch('x'),
            ch(' '),
            press(KeyCode::Down),
            press(KeyCode::Right),
            press(KeyCode::Tab),
            press(KeyCode::Backspace),
        ] {
            assert_eq!(model.handle_key(key), None);
            assert!(matches!(model.bar(), Some(Bar::Share { .. })), "{key:?}");
        }
        // Modified keys are not the listed ones either.
        for modifier in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let event = KeyEvent::new(KeyCode::Char('f'), modifier);
            assert_eq!(model.handle_key(event), None);
            assert!(matches!(model.bar(), Some(Bar::Share { .. })));
        }

        // Enter: this snapshot only.
        assert_eq!(
            model.handle_key(press(KeyCode::Enter)),
            Some(Action::Run(Flow::Share {
                id: "alpha".to_owned(),
                future: FutureSharePolicy::Private
            }))
        );
        assert!(
            model.bar().is_none(),
            "the bar is closed once the flow is asked for"
        );
    }

    #[test]
    fn f_in_the_share_bar_also_shares_future_thoughts_and_uppercase_does_too() {
        for key in ['f', 'F'] {
            let mut model = model_with_new(shared_summary(27, 21, 21), 6);
            open_bar(&mut model, 's');
            assert_eq!(
                model.handle_key(ch(key)),
                Some(Action::Run(Flow::Share {
                    id: "alpha".to_owned(),
                    future: FutureSharePolicy::Shared
                }))
            );
        }
    }

    #[test]
    fn esc_cancels_a_share_bar_without_a_flow() {
        let mut model = model_with_new(shared_summary(27, 21, 21), 6);
        open_bar(&mut model, 's');
        assert_eq!(model.handle_key(press(KeyCode::Esc)), None);
        assert!(model.bar().is_none());
        // And the same keys now act on the list, not on a leftover bar.
        assert_eq!(model.handle_key(press(KeyCode::Enter)), None);
        assert!(model.overlay().is_some(), "Enter opens the details");
    }

    #[test]
    fn q_does_not_dismiss_a_bar_but_ctrl_c_quits_from_anywhere() {
        let mut model = model_with_new(shared_summary(27, 21, 21), 6);
        open_bar(&mut model, 's');
        assert_eq!(model.handle_key(ch('q')), None);
        assert!(matches!(model.bar(), Some(Bar::Share { .. })));
        let interrupt = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(model.handle_key(interrupt), Some(Action::Quit));
        let mut list = model_with_new(shared_summary(27, 21, 21), 6);
        assert_eq!(list.handle_key(interrupt), Some(Action::Quit));
    }

    #[test]
    fn future_sharing_changes_what_enter_and_f_mean_but_not_what_they_emit() {
        let mut summary = shared_summary(27, 21, 21);
        summary.share_future = Some(true);
        let mut model = model_with_new(summary, 6);
        let Bar::Share { ask, .. } = open_bar(&mut model, 's') else {
            panic!("expected the share bar");
        };
        assert!(ask.future_on);
        // Enter re-snapshots with future thoughts private.
        assert_eq!(
            model.handle_key(press(KeyCode::Enter)),
            Some(Action::Run(Flow::Share {
                id: "alpha".to_owned(),
                future: FutureSharePolicy::Private
            }))
        );
    }

    #[test]
    fn a_fully_shared_conversation_offers_no_enter() {
        let mut model = model_with_new(shared_summary(27, 27, 27), 0);
        let Bar::Share { ask, .. } = open_bar(&mut model, 's') else {
            panic!("expected the share bar");
        };
        assert_eq!(ask.unshared, 0);
        assert_eq!(model.handle_key(press(KeyCode::Enter)), None);
        assert!(matches!(model.bar(), Some(Bar::Share { .. })), "still open");
        assert_eq!(
            model.handle_key(ch('f')),
            Some(Action::Run(Flow::Share {
                id: "alpha".to_owned(),
                future: FutureSharePolicy::Shared
            }))
        );
    }

    #[test]
    fn a_fully_shared_conversation_with_future_sharing_on_is_blocked() {
        let mut summary = shared_summary(27, 27, 27);
        summary.share_future = Some(true);
        let mut model = model_with_new(summary, 0);
        assert_eq!(
            open_bar(&mut model, 's'),
            Bar::Blocked(
                "All thoughts and future thoughts are already shared for 'origin'.".to_owned()
            )
        );
    }

    #[test]
    fn the_unshare_bar_emits_the_unshare_flow_on_enter_only() {
        let mut model = model_with_new(shared_summary(27, 21, 0), 6);
        let Bar::Unshare { id, shared, .. } = open_bar(&mut model, 'u') else {
            panic!("expected the unshare bar");
        };
        assert_eq!((id.as_str(), shared), ("alpha", 21));
        for key in [ch('f'), ch('u'), ch('s'), ch('y'), press(KeyCode::Down)] {
            assert_eq!(model.handle_key(key), None);
            assert!(matches!(model.bar(), Some(Bar::Unshare { .. })));
        }
        assert_eq!(
            model.handle_key(press(KeyCode::Enter)),
            Some(Action::Run(Flow::Unshare {
                id: "alpha".to_owned()
            }))
        );
    }

    #[test]
    fn the_publish_bar_emits_the_publish_flow_on_enter_only() {
        let mut model = model_with_new(shared_summary(27, 21, 21), 6);
        assert_eq!(open_bar(&mut model, 'p'), Bar::Publish { waiting: 2 });
        for key in [ch('f'), ch('p'), ch('y'), press(KeyCode::Down)] {
            assert_eq!(model.handle_key(key), None);
            assert!(matches!(model.bar(), Some(Bar::Publish { .. })));
        }
        assert_eq!(
            model.handle_key(press(KeyCode::Enter)),
            Some(Action::Run(Flow::Publish))
        );
    }

    #[test]
    fn publish_with_nothing_waiting_still_offers_a_check() {
        let mut model = model_with_new(shared_summary(27, 21, 21), 6);
        model.counts = Some(DestinationPublicationCounts {
            shared: 4,
            published: 4,
            unconfirmed: 0,
        });
        assert_eq!(open_bar(&mut model, 'p'), Bar::Publish { waiting: 0 });
        assert_eq!(
            model.handle_key(press(KeyCode::Enter)),
            Some(Action::Run(Flow::Publish))
        );
    }

    // ----- authorization -----

    fn unauthorized_model() -> Model {
        let mut remote = remote("origin");
        remote.sharing_consented = false;
        let mut model = model_with(vec![shared_summary(27, 21, 21)], vec![remote]);
        read_selected(&mut model, 6, UnshareCheck::default());
        model
    }

    #[test]
    fn the_authorization_offer_replaces_the_share_and_publish_bars() {
        for key in ['s', 'p'] {
            let mut model = unauthorized_model();
            assert_eq!(open_bar(&mut model, key), Bar::Authorize, "{key}");
        }
    }

    #[test]
    fn the_authorization_offer_emits_authorize_and_never_a_share_or_publish() {
        let mut model = unauthorized_model();
        open_bar(&mut model, 's');
        for key in [ch('f'), ch('s'), ch('p'), ch('a'), press(KeyCode::Down)] {
            assert_eq!(model.handle_key(key), None);
            assert_eq!(model.bar(), Some(&Bar::Authorize));
        }
        assert_eq!(
            model.handle_key(press(KeyCode::Enter)),
            Some(Action::Run(Flow::Authorize))
        );
        assert!(
            model.bar().is_none(),
            "returns to the list; nothing is chained"
        );

        let mut model = unauthorized_model();
        open_bar(&mut model, 'p');
        assert_eq!(
            model.handle_key(press(KeyCode::Enter)),
            Some(Action::Run(Flow::Authorize))
        );
    }

    #[test]
    fn unshare_does_not_need_publishing_to_be_authorized() {
        let mut remote = remote("origin");
        remote.sharing_consented = false;
        let mut model = model_with(vec![shared_summary(27, 21, 0)], vec![remote]);
        read_selected(&mut model, 6, UnshareCheck::default());
        assert!(matches!(open_bar(&mut model, 'u'), Bar::Unshare { .. }));
    }

    #[test]
    fn authorization_is_never_offered_on_an_authorized_remote() {
        let mut model = model_with_new(shared_summary(27, 21, 21), 6);
        for key in ['s', 'u', 'p'] {
            assert_ne!(open_bar(&mut model, key), Bar::Authorize, "{key}");
            model.handle_key(press(KeyCode::Esc));
        }
        // There is no key bound to authorize in the list.
        for key in ['a', 'A', 'o', 't'] {
            assert_eq!(model.handle_key(ch(key)), None);
            assert!(model.bar().is_none());
        }
    }

    // ----- blocked forms -----

    fn blocked_reason(model: &mut Model, key: char) -> String {
        match open_bar(model, key) {
            Bar::Blocked(reason) => reason,
            other => panic!("expected a blocked bar, got {other:?}"),
        }
    }

    #[test]
    fn a_blocked_bar_closes_on_any_key_and_runs_nothing() {
        let mut summary = shared_summary(5, 0, 0);
        summary.imported = true;
        let mut model = model_with_new(summary, 5);
        for dismiss in [ch('x'), press(KeyCode::Enter), press(KeyCode::Esc), ch('f')] {
            blocked_reason(&mut model, 's');
            assert_eq!(model.handle_key(dismiss), None);
            assert!(model.bar().is_none());
        }
    }

    #[test]
    fn on_a_platform_without_consent_actions_nothing_opens_a_flow_bar() {
        let mut model = Model::new(
            data(vec![shared_summary(27, 21, 21)], vec![remote("origin")]),
            None,
            settings(false),
        );
        model.set_size(100, 30);
        read_selected(&mut model, 6, UnshareCheck::default());
        for key in ['s', 'u', 'p'] {
            assert_eq!(
                blocked_reason(&mut model, key),
                "Sharing and publishing from this view are not available on this platform yet. Use cvc share, cvc unshare and cvc push --manual."
            );
            model.handle_key(ch('x'));
        }
        // Browsing still works.
        assert_eq!(model.handle_key(press(KeyCode::Enter)), None);
        assert!(model.overlay().is_some());
    }

    #[test]
    fn an_imported_conversation_is_read_only() {
        let mut summary = shared_summary(5, 0, 0);
        summary.imported = true;
        let mut model = model_with_new(summary, 5);
        assert_eq!(
            blocked_reason(&mut model, 's'),
            "Imported from a remote, so it is read-only here."
        );
        model.handle_key(ch('x'));
        assert_eq!(
            blocked_reason(&mut model, 'u'),
            "Imported from a remote, so it is read-only here."
        );
        model.handle_key(ch('x'));
        // Publishing is not about a conversation.
        assert!(matches!(open_bar(&mut model, 'p'), Bar::Publish { .. }));
    }

    #[test]
    fn the_imported_check_uses_what_was_just_read_not_what_was_listed() {
        // Listed as an ordinary conversation, read as imported.
        let mut model = model_for_actions(shared_summary(5, 0, 0));
        let mut read = shared_summary(5, 0, 0);
        read.imported = true;
        model.set_info("alpha", Some(info_of(&read, 5, UnshareCheck::default())));
        assert_eq!(
            blocked_reason(&mut model, 's'),
            "Imported from a remote, so it is read-only here."
        );
    }

    #[test]
    fn each_remote_problem_blocks_every_action_with_the_headers_words() {
        let cases: Vec<(Vec<RemoteOverview>, Option<&str>, &str)> = vec![
            (
                vec![remote("origin"), remote("beta")],
                Some("typo"),
                "Sharing and publishing need a configured remote. Press r to choose one.",
            ),
            (
                vec![],
                Some("typo"),
                "Add a remote to share: git remote add origin <url>",
            ),
            (
                vec![remote("beta")],
                None,
                "Choose a remote to see share state. Press r.",
            ),
            (
                vec![],
                None,
                "Add a remote to share: git remote add origin <url>",
            ),
        ];
        for (remotes, requested, reason) in cases {
            let mut model = Model::new(
                data(vec![shared_summary(27, 21, 21)], remotes),
                requested,
                settings(true),
            );
            model.set_size(100, 30);
            read_selected(&mut model, 6, UnshareCheck::default());
            for key in ['s', 'u', 'p'] {
                assert_eq!(blocked_reason(&mut model, key), reason);
                model.handle_key(ch('x'));
            }
        }
        let mut broken = remote("origin");
        broken.fingerprint = Err("remote has no push URL".to_owned());
        let mut model = model_with(vec![shared_summary(27, 21, 21)], vec![broken]);
        read_selected(&mut model, 6, UnshareCheck::default());
        assert_eq!(
            blocked_reason(&mut model, 's'),
            "Sharing and publishing are off for this remote. Press r to choose another."
        );
    }

    #[test]
    fn unreadable_privacy_state_blocks_every_action() {
        let mut remote = remote("origin");
        remote.sharing_consented = false;
        remote.consent_error = Some("privacy state permissions are insecure".to_owned());
        let mut model = model_with(vec![shared_summary(27, 21, 21)], vec![remote]);
        read_selected(&mut model, 6, UnshareCheck::default());
        for key in ['s', 'u', 'p'] {
            assert_eq!(
                blocked_reason(&mut model, key),
                "CVC privacy state cannot be read (privacy state permissions are insecure); sharing and publishing are off."
            );
            model.handle_key(ch('x'));
        }
    }

    #[test]
    fn unshare_is_blocked_when_nothing_is_shared() {
        let mut model = model_with_new(shared_summary(5, 0, 0), 5);
        assert_eq!(
            blocked_reason(&mut model, 'u'),
            "Not shared for 'origin', so there is nothing to unshare."
        );
    }

    #[test]
    fn unshare_is_blocked_by_published_thoughts_with_their_count() {
        let mut model = model_with(vec![shared_summary(27, 27, 21)], vec![remote("origin")]);
        read_selected(
            &mut model,
            0,
            UnshareCheck {
                published: 21,
                pending: 0,
                unknown: 0,
            },
        );
        assert_eq!(
            blocked_reason(&mut model, 'u'),
            "Cannot unshare: 21 thoughts are already published to 'origin', and published thoughts cannot be recalled."
        );
    }

    #[test]
    fn unshare_is_blocked_by_an_unfinished_publish_with_the_command_to_run() {
        let mut model = model_with(vec![shared_summary(27, 27, 0)], vec![remote("origin")]);
        read_selected(
            &mut model,
            0,
            UnshareCheck {
                published: 0,
                pending: 3,
                unknown: 0,
            },
        );
        assert_eq!(
            blocked_reason(&mut model, 'u'),
            "Cannot unshare yet: an earlier publish to 'origin' did not finish cleanly. Run: cvc privacy reconcile --remote origin"
        );
    }

    #[test]
    fn without_what_was_read_about_a_row_s_and_u_say_so() {
        let mut model = model_with(vec![shared_summary(27, 21, 21)], vec![remote("origin")]);
        assert!(model.wants_info().is_some());
        assert_eq!(blocked_reason(&mut model, 's'), words::NO_DETAILS);
        model.handle_key(ch('x'));
        assert_eq!(blocked_reason(&mut model, 'u'), words::NO_DETAILS);
    }

    #[test]
    fn the_key_bar_dims_what_would_be_blocked() {
        let mut summary = shared_summary(5, 0, 0);
        summary.imported = true;
        let model = model_with_new(summary, 5);
        let available = model.available();
        assert!(available.details && available.publish);
        assert!(!available.share && !available.unshare);

        let model = model_with_new(shared_summary(27, 21, 21), 6);
        let available = model.available();
        assert!(available.share && available.unshare && available.publish);
        let model = model_with_new(shared_summary(5, 0, 0), 5);
        assert!(!model.available().unshare, "nothing to unshare");
        let model = unauthorized_model();
        let available = model.available();
        assert!(
            available.share && available.publish,
            "the offer is available, not blocked"
        );
    }

    #[test]
    fn imported_conversations_stay_read_only_even_when_listed_last() {
        // The row is the selected one wherever it is.
        let mut summaries = simple();
        summaries[2].imported = true;
        let mut model = model_with(summaries, vec![remote("origin")]);
        model.handle_key(press(KeyCode::End));
        read_selected(&mut model, 7, UnshareCheck::default());
        assert_eq!(selected(&model), "gamma");
        assert!(blocked_reason(&mut model, 's').starts_with("Imported"));
    }

    // ----- overlays -----

    #[test]
    fn enter_opens_the_details_and_enter_esc_and_q_close_them() {
        for close in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('q')] {
            let mut model = model_for_actions(shared_summary(5, 0, 0));
            assert_eq!(model.handle_key(press(KeyCode::Enter)), None);
            assert!(matches!(model.overlay(), Some(Overlay::Page { .. })));
            assert_eq!(
                model.handle_key(press(close)),
                None,
                "{close:?} must not quit"
            );
            assert!(model.overlay().is_none());
        }
    }

    #[test]
    fn s_u_p_do_nothing_inside_an_overlay() {
        let mut model = model_for_actions(shared_summary(27, 21, 21));
        model.handle_key(press(KeyCode::Enter));
        for key in ['s', 'u', 'p', 'f', 'r'] {
            assert_eq!(model.handle_key(ch(key)), None);
            assert!(
                matches!(model.overlay(), Some(Overlay::Page { .. })),
                "{key}"
            );
            assert!(model.bar().is_none());
        }
    }

    #[test]
    fn help_opens_with_a_question_mark_and_closes_with_it_too() {
        let mut model = model_with(simple(), vec![remote("origin")]);
        model.handle_key(ch('?'));
        assert!(matches!(model.overlay(), Some(Overlay::Page { .. })));
        model.handle_key(ch('?'));
        assert!(model.overlay().is_none());
        model.handle_key(ch('?'));
        model.handle_key(press(KeyCode::Esc));
        assert!(model.overlay().is_none());
    }

    #[test]
    fn q_quits_from_the_list_only() {
        let mut model = model_with(simple(), vec![remote("origin")]);
        assert_eq!(model.handle_key(ch('q')), Some(Action::Quit));
        assert_eq!(model.handle_key(ch('Q')), Some(Action::Quit));
        assert_eq!(
            model.handle_key(press(KeyCode::Esc)),
            None,
            "Esc never quits"
        );
        model.handle_key(ch('?'));
        assert_eq!(model.handle_key(ch('q')), None);
        assert_eq!(model.handle_key(ch('q')), Some(Action::Quit));
    }

    #[test]
    fn a_page_scrolls_within_its_length() {
        let mut model = model_for_actions(shared_summary(27, 21, 21));
        model.set_size(60, 12);
        model.handle_key(press(KeyCode::Enter));
        let scroll = |model: &Model| match model.overlay() {
            Some(Overlay::Page { scroll, .. }) => scroll,
            _ => panic!("no page"),
        };
        assert_eq!(scroll(&model), 0);
        model.handle_key(press(KeyCode::Up));
        assert_eq!(scroll(&model), 0);
        model.handle_key(press(KeyCode::Down));
        assert_eq!(scroll(&model), 1);
        model.handle_key(press(KeyCode::End));
        let end = scroll(&model);
        assert!(end > 1);
        model.handle_key(press(KeyCode::Down));
        assert_eq!(scroll(&model), end, "cannot scroll past the end");
        model.handle_key(press(KeyCode::Home));
        assert_eq!(scroll(&model), 0);
        // Everything fits on a tall terminal.
        model.set_size(100, 50);
        model.handle_key(press(KeyCode::End));
        assert_eq!(scroll(&model), 0);
    }

    // ----- sizes -----

    #[test]
    fn below_the_minimum_size_only_quit_works_and_a_bar_is_cancelled() {
        let mut model = model_with_new(shared_summary(27, 21, 21), 6);
        open_bar(&mut model, 's');
        model.set_size(52, 9);
        assert!(model.too_small());
        assert!(model.bar().is_none(), "the bar was cancelled");
        assert_eq!(
            model.too_small_message(),
            "Terminal too small (need 60x12, have 52x9)."
        );
        for key in [
            ch('s'),
            ch('u'),
            ch('p'),
            press(KeyCode::Enter),
            ch('f'),
            ch('r'),
        ] {
            assert_eq!(model.handle_key(key), None);
            assert!(model.bar().is_none() && model.overlay().is_none());
        }
        assert_eq!(model.handle_key(ch('q')), Some(Action::Quit));
        // State is kept: growing back shows the same selection.
        model.set_size(100, 30);
        assert!(!model.too_small());
        assert_eq!(selected(&model), "alpha");
        assert!(model.handle_key(ch('s')).is_none());
        assert!(model.bar().is_some());
    }

    #[test]
    fn the_minimum_size_is_60_by_12() {
        let mut model = model_with(simple(), vec![remote("origin")]);
        model.set_size(60, 12);
        assert!(!model.too_small());
        model.set_size(59, 12);
        assert!(model.too_small());
        model.set_size(60, 11);
        assert!(model.too_small());
    }

    #[test]
    fn the_strip_appears_from_26_rows() {
        let mut model = model_with(simple(), vec![remote("origin")]);
        model.set_size(100, 25);
        assert_eq!(model.layout().strip, 0);
        let without = model.layout().list;
        model.set_size(100, 26);
        assert_eq!(model.layout().strip, STRIP_ROWS);
        assert!(model.layout().list < without + 1);
    }

    #[test]
    fn a_notice_takes_a_row_from_the_list() {
        let mut model = model_with(simple(), vec![remote("origin")]);
        let before = model.layout().list;
        model.set_status("something happened");
        assert_eq!(model.layout().notice, 1);
        assert_eq!(model.layout().list, before - 1);
        model.handle_key(press(KeyCode::Down));
        assert_eq!(model.layout().list, before, "the next key clears it");
    }

    // ----- results -----

    #[test]
    fn a_long_notice_wraps_to_a_second_line_and_only_then_ends_in_an_ellipsis() {
        let mut model = model_with(simple(), vec![remote("origin")]);
        model.report(Report::Failed {
            kind: FlowKind::Publish,
            failure: FlowFailure::Other("connection reset".to_owned()),
        });
        let notice = model.notice();
        assert_eq!(notice.len(), 2, "{notice:?}");
        assert_eq!(model.layout().notice, 2);
        let joined = notice
            .iter()
            .map(|line| line.trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            joined.contains("Run: cvc privacy reconcile --remote origin"),
            "{joined}"
        );
        for line in &notice {
            assert!(text::width(line) <= 100, "{line}");
            assert!(line.starts_with(' '));
        }
        // One that does not fit in two lines is cut, and says so.
        model.set_status(&"word ".repeat(100));
        let notice = model.notice();
        assert_eq!(notice.len(), 2);
        assert!(notice[1].ends_with('…'), "{notice:?}");
        // One that fits is one line, and none is no line.
        model.set_status("short");
        assert_eq!(model.notice(), vec![" short".to_owned()]);
        model.handle_key(press(KeyCode::Down));
        assert!(model.notice().is_empty());
    }

    #[test]
    fn the_notice_after_a_share_names_the_count_and_what_comes_next() {
        let mut model = model_with(vec![shared_summary(27, 27, 0)], vec![remote("origin")]);
        model.report(Report::Shared {
            id: "alpha".to_owned(),
            outcome: ShareOutcome {
                snapshot_count: 27,
                future: FutureSharePolicy::Private,
            },
        });
        assert_eq!(
            model.status(),
            Some("27 thoughts marked shared for remote 'origin'; future thoughts are private. Not published yet (p).")
        );
        assert_eq!(model.shared_remote(), Some("origin"));
    }

    #[test]
    fn sharing_a_parent_mentions_the_agents_that_stay_private_once() {
        let mut summaries = with_agents();
        summaries[0].shared_count = 10;
        summaries[0].shared = true;
        let mut model = model_with(summaries, vec![remote("origin")]);
        model.report(Report::Shared {
            id: "session".to_owned(),
            outcome: ShareOutcome {
                snapshot_count: 10,
                future: FutureSharePolicy::Private,
            },
        });
        let status = model.status().unwrap();
        assert!(
            status.ends_with(" 2 agent conversations remain private (→ to see)."),
            "{status}"
        );
        // An agent that was shared too is not counted.
        let mut summaries = with_agents();
        summaries[1].shared_count = 2;
        summaries[1].shared = true;
        let mut model = model_with(summaries, vec![remote("origin")]);
        model.report(Report::Shared {
            id: "session".to_owned(),
            outcome: ShareOutcome {
                snapshot_count: 10,
                future: FutureSharePolicy::Private,
            },
        });
        assert!(model
            .status()
            .unwrap()
            .ends_with(" 1 agent conversation remains private (→ to see)."));
        // Sharing an agent mentions none.
        model.report(Report::Shared {
            id: "session:agent-a".to_owned(),
            outcome: ShareOutcome {
                snapshot_count: 2,
                future: FutureSharePolicy::Private,
            },
        });
        assert!(!model.status().unwrap().contains("agent conversation"));
    }

    #[test]
    fn the_notice_follows_the_current_auto_push_setting() {
        let mut remote = remote("origin");
        remote.auto_push = true;
        let mut model = model_with(vec![shared_summary(2, 2, 0)], vec![remote]);
        model.report(Report::Shared {
            id: "alpha".to_owned(),
            outcome: ShareOutcome {
                snapshot_count: 2,
                future: FutureSharePolicy::Shared,
            },
        });
        assert_eq!(
            model.status(),
            Some("2 thoughts marked shared for remote 'origin'; future thoughts are shared too. Auto-push is on.")
        );
    }

    #[test]
    fn every_report_becomes_a_notice_that_the_next_key_clears() {
        let mut model = model_with(simple(), vec![remote("origin")]);
        let reports = vec![
            Report::Unshared { made_private: 6 },
            Report::Published(PublishOutcome::NothingNew { shared_total: 42 }),
            Report::Authorized,
            Report::Failed {
                kind: FlowKind::Share,
                failure: FlowFailure::NotConfirmed,
            },
            Report::RemoteChanged,
        ];
        for report in reports {
            model.report(report);
            assert!(model.status().is_some());
            model.handle_key(press(KeyCode::Down));
            assert_eq!(model.status(), None);
        }
        model.report(Report::RemoteChanged);
        assert_eq!(
            model.status(),
            Some("Remote 'origin' changed while this view was open, so nothing was done. The remote list was refreshed.")
        );
    }

    #[test]
    fn notices_are_sanitized() {
        let mut model = model_with(simple(), vec![remote("origin")]);
        model.set_status("bad\u{1b}[2J\u{202e}");
        assert_eq!(model.status(), Some("bad?[2J?"));
        model.report(Report::Failed {
            kind: FlowKind::Share,
            failure: FlowFailure::Other("oops\u{1b}[31m".to_owned()),
        });
        assert_eq!(model.status(), Some("Share failed: oops?[31m"));
        model.report(Report::Failed {
            kind: FlowKind::Publish,
            failure: FlowFailure::Other("oops\u{1b}[31m".to_owned()),
        });
        let status = model.status().unwrap();
        assert!(!status.chars().any(text::is_unsafe), "{status}");
        assert!(status.ends_with("(error: oops?[31m)"), "{status}");
    }

    #[test]
    fn show_blocked_opens_the_blocked_bar() {
        let mut model = model_with(simple(), vec![remote("origin")]);
        model.show_blocked(words::IMPORTED_IS_READ_ONLY);
        assert!(matches!(model.bar(), Some(Bar::Blocked(_))));
    }

    // ----- text -----

    #[test]
    fn the_header_reflects_the_remote_and_the_waiting_count() {
        let model = model_with(simple(), vec![remote("origin")]);
        let header = model.header();
        assert_eq!(
            header.remote[0],
            "remote: origin -> github.com/acme/repo (8348b79e2c41…)"
        );
        // 4 shared, 2 published.
        assert_eq!(header.waiting[0], "2 shared thoughts waiting to publish");
        let none = model_with(simple(), vec![]);
        assert_eq!(none.header().remote[0], "remote: none (no Git remotes)");
        assert!(none.header().waiting.is_empty());
    }

    #[test]
    fn the_bottom_line_is_the_key_bar_until_a_bar_replaces_it() {
        let mut model = model_with_new(shared_summary(27, 21, 21), 6);
        let keys = words::plain_text(&model.bottom_lines()[0]);
        assert!(keys.contains("s share") && keys.contains("q quit"));
        open_bar(&mut model, 's');
        let lines: Vec<_> = model
            .bottom_lines()
            .iter()
            .map(|line| words::plain_text(line))
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0]
            .starts_with("Share \"title of alpha\" (27 thoughts, 6 not yet shared) for 'origin'"));
        assert!(lines[1].contains("Esc: cancel"));
        assert_eq!(model.layout().bottom, 2);
    }

    #[test]
    fn the_strip_and_details_describe_the_selected_agent() {
        let mut model = model_with(with_agents(), vec![remote("origin")]);
        model.handle_key(ch('l'));
        model.handle_key(press(KeyCode::Down));
        assert_eq!(selected(&model), "session:agent-a");
        let strip = model.strip();
        assert_eq!(strip[0], "title of session:agent-a");
        assert!(strip
            .last()
            .unwrap()
            .starts_with("Subagent of \"title of session\""));
        model.handle_key(press(KeyCode::Enter));
        let Some(Overlay::Page { page, .. }) = model.overlay() else {
            panic!("details should be open");
        };
        assert!(page.iter().any(|row| matches!(
            row,
            Page::Field { label: "Kind", value } if value == "Subagent of \"title of session\""
        )));
    }

    #[test]
    fn a_parents_details_count_its_agents() {
        let mut model = model_with(with_agents(), vec![remote("origin")]);
        model.handle_key(press(KeyCode::Enter));
        let Some(Overlay::Page { page, .. }) = model.overlay() else {
            panic!("details should be open");
        };
        assert!(page.iter().any(|row| matches!(
            row,
            Page::Field { label: "Subagents", value } if value == "2 conversations, each shared separately"
        )));
    }

    #[test]
    fn pages_wrap_fields_under_their_values() {
        let page = vec![
            Page::Field {
                label: "Context files",
                value: "14: a.rs (12), b.rs (9), c.rs (7), d.rs (3), e.rs (2), f.rs (1)".to_owned(),
            },
            Page::Blank,
            Page::Text("short".to_owned()),
        ];
        let lines = layout_page(&page, 40);
        assert!(lines.len() > 3);
        assert_eq!(lines[0].label.as_deref(), Some("Context files"));
        assert!(lines[1].label.is_none() && lines[1].kind == PageLineKind::Field);
        for line in &lines {
            assert!(
                text::width(&line.text) + FIELD_LABEL_WIDTH <= 40
                    || line.kind != PageLineKind::Field,
                "{line:?}"
            );
        }
    }
}
