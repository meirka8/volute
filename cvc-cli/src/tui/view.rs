//! Drawing the interactive view with ratatui: a function of the model and
//! nothing else.
//!
//! Every string goes through `text::sanitize` on its way into the screen
//! buffer, whatever the model already did, so that nothing drawn can start an
//! escape sequence. Each column is drawn into a rectangle of its own, so text
//! that is wider than the width it was fitted to can only be clipped, never
//! push its neighbours around.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::widgets::{Block, Clear, Widget};
use ratatui::Frame;

use super::model::{
    layout_page, when_cell, Columns, Disclosure, Model, Overlay, PageLine, PageLineKind, RowView,
    FIELD_LABEL_WIDTH, HEADER_ROWS, OVERLAY_MAX_WIDTH,
};
use super::text::{self, Glyphs};
use super::words::{Emph, RemoteRow, Seg, Tone};

/// How the view is dressed. Bold, dim and reverse video are always available;
/// color only when the terminal is expected to take it (`NO_COLOR` turns it off).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub color: bool,
}

/// The border of an overlay where the terminal is not expected to draw boxes.
const ASCII_BORDER: symbols::border::Set = symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// Columns the disclosure triangle and its space take.
const MARK: usize = 2;
/// Columns between two table columns.
const GAP: usize = 2;
/// The title width the table tries to keep: when the columns beside it would
/// leave less, `linked` goes first, then the year of the date.
const COMFORTABLE_TITLE: usize = 30;
/// The least the title column is given before the state column gives up width.
const MIN_TITLE: usize = 16;
/// The least the state column is cut to on a narrow terminal.
const MIN_STATE: usize = 12;
/// A badge is drawn only if the title keeps at least this much beside it.
const MIN_TITLE_WITH_BADGE: usize = 14;

/// Draws the whole screen into a frame.
pub fn render(frame: &mut Frame, model: &Model, theme: Theme) {
    let area = frame.area();
    draw(frame.buffer_mut(), area, model, theme);
}

/// Draws the whole screen into a buffer.
pub fn draw(buf: &mut Buffer, area: Rect, model: &Model, theme: Theme) {
    let mut canvas = Canvas { buf, theme };
    canvas.fill(area, Style::default());
    if model.too_small() {
        canvas.put(
            area.x,
            area.y,
            &model.too_small_message(),
            area.width as usize,
            Style::default(),
        );
        return;
    }
    let layout = model.layout();
    let mut y = area.y;
    canvas.header(model, area, y);
    y += HEADER_ROWS as u16;
    for line in model.notice() {
        canvas.put(area.x, y, &line, area.width as usize, Style::default());
        y += 1;
    }
    let plan = Plan::of(area.width as usize, model.columns());
    canvas.table_head(&plan, area, y);
    y += 1;
    canvas.list(model, &plan, area, y, layout.list);
    y += layout.list as u16;
    if layout.strip > 0 {
        canvas.strip(model, area, y, layout.strip);
        y += layout.strip as u16;
    }
    canvas.bottom(model, area, y);
    if let Some(overlay) = model.overlay() {
        let height = (area.height as usize).saturating_sub(HEADER_ROWS + layout.bottom);
        let width = (area.width as usize).min(OVERLAY_MAX_WIDTH);
        let rect = Rect {
            x: area.x + ((area.width as usize - width) / 2) as u16,
            y: area.y + HEADER_ROWS as u16,
            width: width as u16,
            height: height as u16,
        };
        // Nothing of the list shows around a box that is narrower than the
        // terminal.
        canvas.fill(
            Rect {
                x: area.x,
                width: area.width,
                ..rect
            },
            Style::default(),
        );
        canvas.overlay(model, &overlay, rect);
    }
}

/// Where the table's columns go, for one terminal width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub short_date: bool,
    pub show_linked: bool,
    pub when: usize,
    pub title: usize,
    pub thoughts: usize,
    pub linked: usize,
    pub state: usize,
}

impl Plan {
    /// The title takes all the width the other columns leave. On a narrow
    /// terminal `linked` goes first, then the year of the date; `thoughts` is
    /// never dropped, and the state column is cut only as a last resort.
    pub fn of(width: usize, columns: &Columns) -> Self {
        let build = |show_linked: bool, short_date: bool, state: usize| {
            let when = if short_date {
                columns.when_short
            } else {
                columns.when_full
            };
            let fixed = MARK
                + when
                + GAP
                + GAP
                + columns.thoughts
                + if show_linked { GAP + columns.linked } else { 0 }
                + GAP
                + state;
            Plan {
                short_date,
                show_linked,
                when,
                title: width.saturating_sub(fixed),
                thoughts: columns.thoughts,
                linked: columns.linked,
                state,
            }
        };
        for (show_linked, short_date) in [(true, false), (false, false)] {
            let plan = build(show_linked, short_date, columns.state);
            if plan.title >= COMFORTABLE_TITLE {
                return plan;
            }
        }
        let plan = build(false, true, columns.state);
        if plan.title >= MIN_TITLE {
            return plan;
        }
        // Still too narrow: the state column gives up width, but not below what
        // keeps its first words readable.
        let missing = MIN_TITLE - plan.title;
        let state = columns
            .state
            .saturating_sub(missing)
            .max(MIN_STATE.min(columns.state));
        build(false, true, state)
    }
}

struct Canvas<'a> {
    buf: &'a mut Buffer,
    theme: Theme,
}

impl Canvas<'_> {
    /// Gives every cell of `rect` the style (and clears its text).
    fn fill(&mut self, rect: Rect, style: Style) {
        let rect = rect.intersection(*self.buf.area());
        for y in rect.top()..rect.bottom() {
            for x in rect.left()..rect.right() {
                let cell = &mut self.buf[(x, y)];
                cell.reset();
                cell.set_style(style);
            }
        }
    }

    /// Writes `text` at `(x, y)`, at most `max` columns, and returns the column
    /// after it. The text is sanitized here, whatever happened before.
    fn put(&mut self, x: u16, y: u16, text: &str, max: usize, style: Style) -> u16 {
        if max == 0 || y >= self.buf.area().bottom() || x >= self.buf.area().right() {
            return x;
        }
        let clean = text::sanitize(text);
        let (end, _) = self.buf.set_stringn(x, y, clean, max, style);
        end
    }

    fn emph(&self, emph: Emph) -> Style {
        match emph {
            Emph::Plain => Style::default(),
            Emph::Strong => Style::default().add_modifier(Modifier::BOLD),
            Emph::Dim => Style::default().add_modifier(Modifier::DIM),
            Emph::Loud => {
                let style = Style::default().add_modifier(Modifier::BOLD);
                if self.theme.color {
                    style.fg(Color::Yellow)
                } else {
                    style
                }
            }
        }
    }

    fn tone(&self, tone: Tone) -> Style {
        match tone {
            Tone::Neutral => Style::default().add_modifier(Modifier::DIM),
            Tone::Private => Style::default(),
            Tone::Waiting if self.theme.color => Style::default().fg(Color::Yellow),
            Tone::Published if self.theme.color => Style::default().fg(Color::Green),
            Tone::Waiting | Tone::Published => Style::default(),
        }
    }

    /// A line of segments, cut to `max` columns.
    fn segs(&mut self, x: u16, y: u16, line: &[Seg], max: usize) -> u16 {
        let mut x = x;
        let end = x as usize + max;
        for seg in line {
            if x as usize >= end {
                break;
            }
            let style = self.emph(seg.emph);
            x = self.put(x, y, &seg.text, end - x as usize, style);
        }
        x
    }

    // ----- the header -----

    fn header(&mut self, model: &Model, area: Rect, y: u16) {
        let width = area.width as usize;
        let header = model.header();
        let g = model.glyphs();

        // First line: the title on the left, the remote on the right.
        let title = "CVC conversations";
        self.put(
            area.x,
            y,
            title,
            width,
            Style::default().add_modifier(Modifier::BOLD),
        );
        let room = width.saturating_sub(text::width(title) + GAP);
        let remote = header
            .remote
            .iter()
            .find(|candidate| text::width(candidate) <= room)
            .or(header.remote.last());
        if let Some(remote) = remote {
            let remote = text::truncate(remote, room, g.ellipsis());
            let x = area.x + (width - text::width(&remote)) as u16;
            self.put(x, y, &remote, width, Style::default());
        }

        // Second line: publishing on the left, what waits on the right.
        let y = y + 1;
        let mut rights: Vec<&str> = header.waiting.iter().map(String::as_str).collect();
        rights.push("");
        let chosen = header.publishing.iter().find_map(|left| {
            let left_width: usize = left.iter().map(|seg| text::width(&seg.text)).sum();
            rights
                .iter()
                .find(|right| {
                    let gap = if right.is_empty() { 0 } else { GAP };
                    left_width + gap + text::width(right) <= width
                })
                .map(|right| (left, *right))
        });
        match chosen {
            Some((left, right)) => {
                self.segs(area.x, y, left, width);
                if !right.is_empty() {
                    let x = area.x + (width - text::width(right)) as u16;
                    self.put(x, y, right, width, Style::default());
                }
            }
            None => {
                // Nothing fits: the shortest text, cut.
                if let Some(left) = header.publishing.last() {
                    let line = left.iter().map(|seg| seg.text.as_str()).collect::<String>();
                    let emph = left.last().map_or(Emph::Plain, |seg| seg.emph);
                    let style = self.emph(emph);
                    self.put(
                        area.x,
                        y,
                        &text::truncate(&line, width, g.ellipsis()),
                        width,
                        style,
                    );
                }
            }
        }
    }

    // ----- the table -----

    fn table_head(&mut self, plan: &Plan, area: Rect, y: u16) {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut x = area.x + MARK as u16;
        self.put(x, y, "when (UTC)", plan.when, dim);
        x += (plan.when + GAP) as u16;
        self.put(x, y, "title", plan.title, dim);
        x += (plan.title + GAP) as u16;
        self.put(
            x,
            y,
            &text::pad_start("thoughts", plan.thoughts),
            plan.thoughts,
            dim,
        );
        x += (plan.thoughts + GAP) as u16;
        if plan.show_linked {
            self.put(
                x,
                y,
                &text::pad_start("linked", plan.linked),
                plan.linked,
                dim,
            );
            x += (plan.linked + GAP) as u16;
        }
        self.put(x, y, "state", plan.state, dim);
    }

    fn list(&mut self, model: &Model, plan: &Plan, area: Rect, top: u16, rows: usize) {
        let rows_in_view = model.rows();
        if rows_in_view.is_empty() {
            self.put(
                area.x + MARK as u16,
                top,
                "No conversations to show.",
                area.width as usize,
                Style::default().add_modifier(Modifier::DIM),
            );
            return;
        }
        let g = *model.glyphs();
        for (offset, row) in rows_in_view.iter().take(rows).enumerate() {
            self.row(&g, plan, area, top + offset as u16, row);
        }
    }

    fn row(&mut self, g: &Glyphs, plan: &Plan, area: Rect, y: u16, row: &RowView) {
        let mut base = Style::default();
        if row.imported {
            base = base.add_modifier(Modifier::DIM);
        }
        if row.selected {
            base = base.add_modifier(Modifier::REVERSED);
        }
        let line = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        // The base style first, so a selected row is reversed across its width.
        self.fill(line, base);

        let mut x = area.x;
        let glyph = match row.disclosure {
            Disclosure::Collapsed => g.collapsed(),
            Disclosure::Expanded => g.expanded(),
            Disclosure::None => " ",
        };
        self.put(x, y, glyph, 1, base);
        x += MARK as u16;

        let (main, tag) = when_cell(row.when, plan.short_date, g);
        let after = self.put(x, y, &main, plan.when, base);
        if let Some(tag) = tag {
            let room = (x as usize + plan.when).saturating_sub(after as usize);
            self.put(after, y, &tag, room, base.add_modifier(Modifier::DIM));
        }
        x += (plan.when + GAP) as u16;

        // The title, with the badge of a collapsed parent at the right edge of
        // its cell when there is room for both.
        let badge = row
            .badge
            .as_deref()
            .filter(|badge| plan.title >= MIN_TITLE_WITH_BADGE + 1 + text::width(badge));
        let title_room = plan.title - badge.map_or(0, |badge| text::width(badge) + 1);
        let title = text::truncate(&row.title, title_room, g.ellipsis());
        self.put(x, y, &title, title_room, base);
        if let Some(badge) = badge {
            let badge_x = x + (plan.title - text::width(badge)) as u16;
            self.put(
                badge_x,
                y,
                badge,
                plan.title,
                base.add_modifier(Modifier::DIM),
            );
        }
        x += (plan.title + GAP) as u16;

        self.put(
            x,
            y,
            &text::pad_start(&row.thoughts.to_string(), plan.thoughts),
            plan.thoughts,
            base,
        );
        x += (plan.thoughts + GAP) as u16;
        if plan.show_linked {
            self.put(
                x,
                y,
                &text::pad_start(&row.linked.to_string(), plan.linked),
                plan.linked,
                base,
            );
            x += (plan.linked + GAP) as u16;
        }
        let state = text::truncate(&row.state, plan.state, g.ellipsis());
        self.put(x, y, &state, plan.state, base.patch(self.tone(row.tone)));
    }

    // ----- the strip and the bottom line -----

    fn strip(&mut self, model: &Model, area: Rect, top: u16, rows: usize) {
        let width = area.width as usize;
        let g = model.glyphs();
        let dim = Style::default().add_modifier(Modifier::DIM);
        self.put(area.x, top, &g.rule().repeat(width), width, dim);

        let lines = model.strip();
        let Some((title, others)) = lines.split_first() else {
            return;
        };
        let budget = rows.saturating_sub(1);
        let title_rows = budget.saturating_sub(others.len()).clamp(1, 2);
        let inner = width.saturating_sub(2);
        let mut wrapped = text::wrap(title, inner);
        if wrapped.len() > title_rows {
            wrapped.truncate(title_rows);
            if let Some(last) = wrapped.last_mut() {
                // The title goes on past the rows it has: say so.
                *last = text::truncate(&format!("{last}{}", g.ellipsis()), inner, g.ellipsis());
            }
        }
        let mut y = top + 1;
        for line in wrapped {
            self.put(
                area.x + 2,
                y,
                &line,
                inner,
                Style::default().add_modifier(Modifier::BOLD),
            );
            y += 1;
        }
        for (index, line) in others.iter().enumerate() {
            if y >= top + rows as u16 {
                break;
            }
            let style = if index == 0 { Style::default() } else { dim };
            let line = text::truncate(line, inner, g.ellipsis());
            self.put(area.x + 2, y, &line, inner, style);
            y += 1;
        }
    }

    fn bottom(&mut self, model: &Model, area: Rect, top: u16) {
        let width = area.width as usize;
        let lines = model.bottom_lines();
        let in_bar = model.bar().is_some();
        for (offset, line) in lines.iter().enumerate() {
            let y = top + offset as u16;
            if in_bar {
                // A bar's lines come without the space the key bar starts with.
                let mut bar = vec![Seg::plain(" ")];
                bar.extend(line.iter().cloned());
                self.segs(area.x, y, &bar, width);
            } else {
                self.segs(area.x, y, line, width);
            }
        }
    }

    // ----- overlays -----

    fn overlay(&mut self, model: &Model, overlay: &Overlay, rect: Rect) {
        self.fill(rect, Style::default());
        Clear.render(rect, self.buf);
        let border = if model.glyphs().is_unicode() {
            symbols::border::PLAIN
        } else {
            ASCII_BORDER
        };
        Block::bordered().border_set(border).render(rect, self.buf);
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        match overlay {
            Overlay::Page { page, scroll } => {
                let lines = layout_page(page, inner.width as usize);
                self.page(&lines, *scroll, inner);
            }
            Overlay::Remotes { rows, cursor } => {
                self.remotes(model, rows, *cursor, inner);
            }
        }
    }

    fn page(&mut self, lines: &[PageLine], scroll: usize, inner: Rect) {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let bold = Style::default().add_modifier(Modifier::BOLD);
        for (offset, line) in lines
            .iter()
            .skip(scroll)
            .take(inner.height as usize)
            .enumerate()
        {
            let y = inner.y + offset as u16;
            let width = inner.width as usize;
            match line.kind {
                PageLineKind::Blank => {}
                PageLineKind::Title | PageLineKind::Heading => {
                    self.put(inner.x, y, &line.text, width, bold);
                }
                PageLineKind::Text => {
                    self.put(inner.x, y, &line.text, width, Style::default());
                }
                PageLineKind::Field => {
                    if let Some(label) = &line.label {
                        self.put(inner.x, y, label, FIELD_LABEL_WIDTH, dim);
                    }
                    self.put(
                        inner.x + FIELD_LABEL_WIDTH as u16,
                        y,
                        &line.text,
                        width.saturating_sub(FIELD_LABEL_WIDTH),
                        Style::default(),
                    );
                }
            }
        }
        // Say so when there is more above or below.
        let total = lines.len();
        let shown = inner.height as usize;
        if total > shown {
            let bottom = inner.y + inner.height;
            let note = if scroll + shown < total {
                "more below"
            } else {
                "end"
            };
            let position = format!(" {note} ");
            let x = inner.x + inner.width.saturating_sub(position.len() as u16);
            self.put(x, bottom, &position, position.len(), dim);
        }
    }

    fn remotes(&mut self, model: &Model, rows: &[RemoteRow], cursor: usize, inner: Rect) {
        let g = *model.glyphs();
        let width = inner.width as usize;
        let dim = Style::default().add_modifier(Modifier::DIM);
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut y = inner.y;
        let bottom = inner.y + inner.height;

        self.put(inner.x, y, super::words::PICKER_TITLE, width, bold);
        y += 1;
        for line in text::wrap(super::words::PICKER_SUBTITLE, width) {
            if y >= bottom {
                return;
            }
            self.put(inner.x, y, &line, width, dim);
            y += 1;
        }
        y += 1;
        if rows.is_empty() {
            if y < bottom {
                self.put(
                    inner.x,
                    y,
                    "This repository has no Git remotes.",
                    width,
                    Style::default(),
                );
            }
            return;
        }

        // Column widths: names and fingerprints as they are, the location
        // gives way on a narrow terminal.
        let name_w = rows
            .iter()
            .map(|r| text::width(&r.name))
            .max()
            .unwrap_or(0)
            .max(6);
        let fp_w = rows
            .iter()
            .map(|r| text::width(&r.fingerprint))
            .max()
            .unwrap_or(0)
            .max(11);
        let pub_w = rows
            .iter()
            .map(|r| r.publishing.len())
            .max()
            .unwrap_or(0)
            .max(10);
        let auto_w = 9;
        let location_wanted = rows
            .iter()
            .map(|r| text::width(&r.location))
            .max()
            .unwrap_or(0)
            .max(8);
        let fixed = MARK + name_w + GAP + GAP + fp_w + GAP + pub_w + GAP + auto_w;
        let location_w = location_wanted.min(width.saturating_sub(fixed)).max(4);

        let columns = |x0: u16| {
            let mut x = x0 + MARK as u16;
            let name = x;
            x += (name_w + GAP) as u16;
            let location = x;
            x += (location_w + GAP) as u16;
            let fingerprint = x;
            x += (fp_w + GAP) as u16;
            let publishing = x;
            x += (pub_w + GAP) as u16;
            (name, location, fingerprint, publishing, x)
        };
        let (cn, cl, cf, cp, ca) = columns(inner.x);
        if y < bottom {
            self.put(cn, y, "remote", name_w, dim);
            self.put(cl, y, "location", location_w, dim);
            self.put(cf, y, "fingerprint", fp_w, dim);
            self.put(cp, y, "publishing", pub_w, dim);
            self.put(ca, y, "auto-push", auto_w, dim);
            y += 1;
        }
        // A repository with more remotes than rows scrolls to keep the cursor in view.
        let capacity = (bottom.saturating_sub(y) as usize).max(1);
        let skip = (cursor + 1).saturating_sub(capacity);
        for (index, row) in rows.iter().enumerate().skip(skip) {
            if y >= bottom {
                break;
            }
            let mut style = Style::default();
            if row.unusable.is_some() {
                style = style.add_modifier(Modifier::DIM);
            }
            if index == cursor && row.unusable.is_none() {
                style = style.add_modifier(Modifier::REVERSED);
            }
            self.fill(
                Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: 1,
                },
                style,
            );
            let marker = if row.current { g.published() } else { " " };
            self.put(inner.x, y, marker, 1, style);
            self.put(
                cn,
                y,
                &text::truncate(&row.name, name_w, g.ellipsis()),
                name_w,
                style,
            );
            match &row.unusable {
                Some(reason) => {
                    let room = width.saturating_sub(MARK + name_w + GAP);
                    self.put(
                        cl,
                        y,
                        &text::truncate(reason, room, g.ellipsis()),
                        room,
                        style,
                    );
                }
                None => {
                    self.put(
                        cl,
                        y,
                        &text::truncate(&row.location, location_w, g.ellipsis()),
                        location_w,
                        style,
                    );
                    self.put(cf, y, &row.fingerprint, fp_w, style);
                    self.put(cp, y, row.publishing, pub_w, style);
                    self.put(ca, y, row.auto_push, auto_w, style);
                }
            }
            y += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::conversations::RemoteOverview;
    use crate::tui::model::{Data, RowInfo, Settings};
    use crate::tui::text::Glyphs;
    use crate::tui::tree::tests::conversation;
    use cvc_core::db::{
        ContextPathUse, ConversationDetails, ConversationSummary, DestinationPublicationCounts,
        UnshareCheck,
    };
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    const FP: &str = "8348b79e2c41aabbccddeeff00112233445566778899aabbccddeeff00112233";
    const COLOR: Theme = Theme { color: true };

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

    fn settings(glyphs: Glyphs) -> Settings {
        Settings {
            consent_actions: true,
            glyphs,
        }
    }

    fn model_of(
        summaries: Vec<ConversationSummary>,
        remotes: Vec<RemoteOverview>,
        size: (u16, u16),
        glyphs: Glyphs,
    ) -> Model {
        let mut model = Model::new(
            Data {
                summaries,
                remotes,
                counts: Some(DestinationPublicationCounts {
                    shared: 4,
                    published: 2,
                    unconfirmed: 0,
                }),
            },
            None,
            settings(glyphs),
        );
        model.set_size(size.0, size.1);
        model
    }

    /// The conversations of the spec's examples.
    fn conversations() -> Vec<ConversationSummary> {
        let mut parent = conversation("session-1", 1_791_882_720, 1_792_003_200, 27);
        parent.title = "Refactor the sync module".to_owned();
        parent.linked = 26;
        parent.shared_count = 21;
        parent.shared = true;
        parent.published = 19;
        let mut agent_a = conversation("session-1:agent-a", 1_791_890_000, 1_791_891_000, 4);
        agent_a.title = "Explore: find the config loader".to_owned();
        agent_a.linked = 4;
        let mut agent_b = conversation("session-1:agent-b", 1_791_895_000, 1_791_896_000, 6);
        agent_b.title = "Plan: split the module".to_owned();
        agent_b.linked = 5;
        agent_b.shared_count = 6;
        agent_b.shared = true;
        agent_b.published = 2;
        let mut published = conversation("session-2", 1_791_800_000, 1_791_900_000, 3);
        published.title = "Fix flaky test".to_owned();
        published.linked = 3;
        published.shared_count = 3;
        published.shared = true;
        published.published = 3;
        let mut imported = conversation(
            &format!("remote:{}:session-3", "a".repeat(64)),
            1_791_700_000,
            1_791_700_100,
            9,
        );
        imported.title = "Imported from a teammate".to_owned();
        imported.imported = true;
        vec![parent, agent_a, agent_b, published, imported]
    }

    fn screen_of(
        model: &Model,
        glyphs: Glyphs,
        theme: Theme,
    ) -> (Terminal<TestBackend>, Vec<String>) {
        let _ = glyphs;
        let (width, height) = model.size();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, model, theme)).unwrap();
        let lines = buffer_lines(terminal.backend().buffer());
        (terminal, lines)
    }

    /// The text of each row of a buffer, with the cell after a wide character
    /// left out.
    fn buffer_lines(buffer: &Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|y| {
                let mut line = String::new();
                let mut skip = 0;
                for x in 0..buffer.area.width {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    let symbol = buffer[(x, y)].symbol();
                    skip = text::width(symbol).saturating_sub(1);
                    line.push_str(symbol);
                }
                line.trim_end().to_owned()
            })
            .collect()
    }

    fn rendered(model: &Model) -> Vec<String> {
        screen_of(model, *model.glyphs(), COLOR).1
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn read_info(model: &mut Model, id: &str, new: usize) {
        let summary = conversations()
            .into_iter()
            .find(|s| s.id == id)
            .expect("known conversation");
        let count = summary.thoughts as usize;
        model.set_info(
            id,
            Some(RowInfo {
                details: ConversationDetails {
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
                    snapshot_count: count,
                    snapshot_shared: count - new,
                    snapshot_new: new,
                },
                unshare: Some(UnshareCheck::default()),
            }),
        );
    }

    fn full_model() -> Model {
        let mut model = model_of(
            conversations(),
            vec![remote("origin")],
            (100, 30),
            Glyphs::UNICODE,
        );
        read_info(&mut model, "session-1", 6);
        model
    }

    fn contains(lines: &[String], needle: &str) -> bool {
        lines.iter().any(|line| line.contains(needle))
    }

    /// The first row of the list: the line under the column labels.
    fn first_row(lines: &[String]) -> &String {
        let head = lines
            .iter()
            .position(|line| line.contains("when (UTC)"))
            .expect("the column labels");
        &lines[head + 1]
    }

    fn dump(lines: &[String]) -> String {
        lines.join("\n")
    }

    // ----- the header -----

    #[test]
    fn the_header_shows_the_remote_publishing_and_what_waits() {
        let lines = rendered(&full_model());
        assert!(
            lines[0].starts_with("CVC conversations"),
            "{}",
            dump(&lines)
        );
        assert!(
            lines[0].ends_with("remote: origin -> github.com/acme/repo (8348b79e2c41…)"),
            "{}",
            lines[0]
        );
        assert!(lines[1].starts_with("publishing: authorized by you   auto-push: off"));
        assert!(
            lines[1].ends_with("2 shared thoughts waiting to publish"),
            "{}",
            lines[1]
        );
        assert_eq!(text::width(&lines[0]), 100, "right-aligned to the edge");
    }

    #[test]
    fn auto_push_on_is_the_only_bold_loud_text_of_the_header() {
        let mut origin = remote("origin");
        origin.auto_push = true;
        let model = model_of(conversations(), vec![origin], (110, 30), Glyphs::UNICODE);
        let (terminal, lines) = screen_of(&model, Glyphs::UNICODE, COLOR);
        // Not the next push only: every push, and the line says so as long as it
        // can be made to fit.
        assert!(
            lines[1].contains(
                "auto-push: ON - every git push to this remote also publishes shared thoughts"
            ),
            "{}",
            lines[1]
        );
        let buffer = terminal.backend().buffer();
        let x = lines[1].find("auto-push: ON").unwrap();
        let x = text::width(&lines[1][..x]) as u16;
        let cell = &buffer[(x, 1)];
        assert!(cell.style().add_modifier.contains(Modifier::BOLD));
        assert_eq!(cell.style().fg, Some(Color::Yellow));
        // "publishing: authorized" is quiet.
        assert!(!buffer[(0, 1)].style().add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_auto_push_warning_shortens_with_the_terminal_but_stays_a_warning() {
        let mut origin = remote("origin");
        origin.auto_push = true;
        let mut model = model_of(conversations(), vec![origin], (100, 30), Glyphs::UNICODE);
        // Too narrow for the whole sentence: it still says every push, and the
        // waiting count is kept.
        let lines = rendered(&model);
        assert!(
            lines[1].starts_with(
                "publishing: authorized   auto-push: ON - every git push also publishes shared thoughts"
            ),
            "{}",
            lines[1]
        );
        assert!(lines[1].ends_with("2 waiting"), "{}", lines[1]);
        // Narrower still: only the ON is left.
        model.set_size(60, 30);
        let lines = rendered(&model);
        assert!(
            lines[1].starts_with("publishing: authorized   auto-push: ON"),
            "{}",
            lines[1]
        );
        assert!(!lines[1].contains("every git push"), "{}", lines[1]);
    }

    #[test]
    fn narrow_headers_drop_parentheticals_then_the_location() {
        let mut model = full_model();
        model.set_size(70, 30);
        let lines = rendered(&model);
        assert!(
            lines[0].ends_with("remote: origin -> github.com/acme/repo"),
            "{}",
            lines[0]
        );
        assert!(!lines[0].contains('('));
        model.set_size(60, 30);
        let lines = rendered(&model);
        for line in &lines[..2] {
            assert!(text::width(line) <= 60, "{line}");
        }
        assert!(lines[0].starts_with("CVC conversations"));
        assert!(lines[0].contains("remote: origin"), "{}", lines[0]);
        // The waiting count shortens before it goes.
        assert!(lines[1].ends_with("2 waiting"), "{}", lines[1]);
        model.set_size(76, 30);
        let lines = rendered(&model);
        assert!(lines[1].ends_with("2 waiting to publish"), "{}", lines[1]);
        model.set_size(100, 30);
        let lines = rendered(&model);
        assert!(
            lines[1].ends_with("2 shared thoughts waiting to publish"),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn a_very_long_location_is_cut_by_display_width() {
        let mut origin = remote("origin");
        origin.location = Some(format!("github.com/{}", "a-very-long-path/".repeat(100)));
        let model = model_of(conversations(), vec![origin], (100, 30), Glyphs::UNICODE);
        let lines = rendered(&model);
        assert!(text::width(&lines[0]) <= 100);
        assert!(lines[0].contains('…'), "{}", lines[0]);
        assert!(lines[0].starts_with("CVC conversations"));
    }

    fn header_of(remotes: Vec<RemoteOverview>, requested: Option<&str>) -> Vec<String> {
        header_at(remotes, requested, 100)
    }

    fn header_at(remotes: Vec<RemoteOverview>, requested: Option<&str>, width: u16) -> Vec<String> {
        let mut model = Model::new(
            Data {
                summaries: conversations(),
                remotes,
                counts: None,
            },
            requested,
            settings(Glyphs::UNICODE),
        );
        model.set_size(width, 30);
        rendered(&model)
    }

    #[test]
    fn the_header_variants() {
        // A remote that is not authorized.
        let mut unauthorized = remote("origin");
        unauthorized.sharing_consented = false;
        let lines = header_of(vec![unauthorized], None);
        assert!(lines[1]
            .starts_with("publishing: not authorized yet (s or p will walk you through it)"));

        // `--remote` names a remote that is not configured.
        let lines = header_of(vec![remote("origin")], Some("typo"));
        assert!(lines[0].ends_with("remote: typo (not configured)"));
        assert!(lines[1].starts_with(
            "Sharing and publishing need a configured remote. Press r to choose one."
        ));

        // No push URL.
        let mut broken = remote("origin");
        broken.fingerprint = Err("remote has no push URL".to_owned());
        let lines = header_of(vec![broken], None);
        assert!(lines[0].ends_with("remote: origin (no push URL)"));
        assert!(lines[1].starts_with(
            "Sharing and publishing are off for this remote. Press r to choose another."
        ));

        // No origin, others exist.
        let lines = header_of(vec![remote("upstream")], None);
        assert!(lines[0].ends_with("remote: none selected (no remote named origin)"));
        assert!(lines[1].starts_with("Choose a remote to see share state. Press r."));

        // No remotes.
        let lines = header_of(vec![], None);
        assert!(lines[0].ends_with("remote: none (no Git remotes)"));
        assert!(lines[1].starts_with("Add a remote to share: git remote add origin <url>"));

        // Privacy state unreadable.
        let mut unreadable = remote("origin");
        unreadable.sharing_consented = false;
        unreadable.consent_error = Some("privacy state permissions are insecure".to_owned());
        let lines = header_at(vec![unreadable.clone()], None, 150);
        assert!(lines[1].starts_with(
            "publishing: unknown (CVC privacy state cannot be read: privacy state permissions are insecure); sharing and publishing from this view are off."
        ), "{}", lines[1]);
        // On a narrower terminal the parenthetical goes first.
        let lines = header_of(vec![unreadable], None);
        assert!(
            lines[1]
                .starts_with("publishing: unknown; sharing and publishing from this view are off."),
            "{}",
            lines[1]
        );
    }

    // ----- the list -----

    #[test]
    fn rows_show_the_columns_and_the_state() {
        let mut model = full_model();
        model.set_size(130, 30);
        let lines = rendered(&model);
        let head = lines.iter().find(|l| l.contains("when (UTC)")).unwrap();
        for label in ["when (UTC)", "title", "thoughts", "linked", "state"] {
            assert!(head.contains(label), "{head}");
        }
        let parent = lines
            .iter()
            .find(|l| l.contains("Refactor the sync module"))
            .unwrap();
        assert!(parent.contains("2026-10-14 18:40"), "{parent}");
        assert!(
            parent.contains("◐ shared · 2 waiting · 6 private"),
            "{parent}"
        );
        assert!(parent.starts_with("▸ "), "{parent}");
        let published = lines.iter().find(|l| l.contains("Fix flaky test")).unwrap();
        assert!(published.contains("● published"), "{published}");
        let imported = lines
            .iter()
            .find(|l| l.contains("Imported from a teammate"))
            .unwrap();
        assert!(imported.trim_end().ends_with("imported"), "{imported}");
    }

    #[test]
    fn a_collapsed_parent_ends_in_a_badge_and_an_open_one_shows_its_agents() {
        let mut model = full_model();
        let lines = rendered(&model);
        let parent = first_row(&lines);
        assert!(parent.starts_with("▸ "), "{parent}");
        assert!(parent.contains("+2 agents · 1 waiting"), "{parent}");
        assert!(
            parent.contains("◐ shared · 2 waiting · 6 private"),
            "{parent}"
        );
        assert!(!contains(&lines, "Explore: find the config loader"));

        model.handle_key(key(KeyCode::Right));
        let lines = rendered(&model);
        let parent = first_row(&lines);
        assert!(parent.starts_with("▾ "), "{parent}");
        assert!(parent.contains("Refactor the sync module"), "{parent}");
        assert!(!parent.contains("+2 agents"), "{parent}");
        let agent = lines
            .iter()
            .find(|l| l.contains("Explore: find the config loader"))
            .unwrap();
        assert!(agent.contains("└─ agent"), "{agent}");
        assert!(agent.contains("○ private"), "{agent}");
        let other = lines
            .iter()
            .find(|l| l.contains("Plan: split the module"))
            .unwrap();
        assert!(other.contains("◐ shared · 4 waiting"), "{other}");
    }

    #[test]
    fn the_selected_row_is_reversed_and_an_imported_row_is_dim() {
        let mut model = full_model();
        let (terminal, lines) = screen_of(&model, Glyphs::UNICODE, COLOR);
        let buffer = terminal.backend().buffer();
        let selected_y = lines.iter().position(|l| l.contains("when (UTC)")).unwrap() as u16 + 1;
        for x in [0u16, 10, 50, 99] {
            assert!(
                buffer[(x, selected_y)]
                    .style()
                    .add_modifier
                    .contains(Modifier::REVERSED),
                "x={x}"
            );
        }
        let imported_y = lines
            .iter()
            .position(|l| l.contains("Imported from a teammate"))
            .unwrap() as u16;
        assert!(buffer[(20, imported_y)]
            .style()
            .add_modifier
            .contains(Modifier::DIM));
        assert!(!buffer[(20, imported_y)]
            .style()
            .add_modifier
            .contains(Modifier::REVERSED));
        // There is no cursor glyph: the marker column of a plain row is blank.
        assert_eq!(buffer[(0, imported_y)].symbol(), " ");
        model.handle_key(key(KeyCode::Down));
        let (terminal, lines) = screen_of(&model, Glyphs::UNICODE, COLOR);
        let y = lines
            .iter()
            .position(|l| l.contains("Fix flaky test"))
            .unwrap() as u16;
        assert!(terminal.backend().buffer()[(30, y)]
            .style()
            .add_modifier
            .contains(Modifier::REVERSED));
    }

    #[test]
    fn color_marks_the_state_and_no_color_leaves_only_the_glyphs() {
        let model = full_model();
        let (terminal, lines) = screen_of(&model, Glyphs::UNICODE, COLOR);
        let y = lines
            .iter()
            .position(|l| l.contains("Fix flaky test"))
            .unwrap() as u16;
        let x = text::width(&lines[y as usize][..lines[y as usize].find("● published").unwrap()])
            as u16;
        assert_eq!(
            terminal.backend().buffer()[(x, y)].style().fg,
            Some(Color::Green)
        );

        let (terminal, lines) = screen_of(&model, Glyphs::UNICODE, Theme { color: false });
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let style = buffer[(x, y)].style();
                assert!(
                    matches!(style.fg, None | Some(Color::Reset)),
                    "({x},{y}) {style:?}"
                );
                assert!(
                    matches!(style.bg, None | Some(Color::Reset)),
                    "({x},{y}) {style:?}"
                );
            }
        }
        // The same words are on screen.
        assert!(contains(&lines, "● published"));
    }

    fn columns() -> Columns {
        Columns {
            when_full: 16,
            when_short: 11,
            thoughts: 8,
            linked: 6,
            state: 20,
        }
    }

    #[test]
    fn the_plan_gives_the_title_the_spare_width() {
        let plan = Plan::of(120, &columns());
        assert!(plan.show_linked && !plan.short_date);
        // marker, when, gaps, thoughts, linked, state.
        assert_eq!(plan.title, 120 - (2 + 16 + 2 + 2 + 8 + 2 + 6 + 2 + 20));
        assert_eq!(plan.state, 20);
    }

    #[test]
    fn the_plan_drops_linked_first_then_the_year_and_never_thoughts() {
        // Without room for a comfortable title: linked goes.
        let plan = Plan::of(88, &columns());
        assert!(!plan.show_linked && !plan.short_date, "{plan:?}");
        assert!(plan.title >= COMFORTABLE_TITLE);
        // Still not: the year of the date goes.
        let plan = Plan::of(80, &columns());
        assert!(!plan.show_linked && plan.short_date, "{plan:?}");
        assert!(plan.title >= COMFORTABLE_TITLE);
        // The narrowest terminal: the state column gives up some width.
        let plan = Plan::of(60, &columns());
        assert!(!plan.show_linked && plan.short_date);
        assert_eq!(plan.title, MIN_TITLE);
        assert!(plan.state < 20 && plan.state >= MIN_STATE);
        assert_eq!(plan.thoughts, 8, "thoughts is never dropped");
    }

    /// A model whose rows all read `○ private`, so the state column is short.
    fn quiet_model(width: u16) -> Model {
        let summaries = vec![
            conversation("a", 1_791_882_720, 1_792_003_200, 5),
            conversation("b", 1_791_882_720, 1_792_000_000, 3),
        ];
        model_of(
            summaries,
            vec![remote("origin")],
            (width, 30),
            Glyphs::UNICODE,
        )
    }

    #[test]
    fn narrow_terminals_drop_linked_then_the_year_but_never_thoughts() {
        let wide = rendered(&quiet_model(100));
        assert!(contains(&wide, "linked"));
        assert!(contains(&wide, "2026-10-14 18:40"));

        let lines = rendered(&quiet_model(72));
        let head = lines.iter().find(|l| l.contains("when (UTC)")).unwrap();
        assert!(!head.contains("linked"), "{head}");
        assert!(head.contains("thoughts"), "{head}");
        assert!(contains(&lines, "2026-10-14 18:40"), "{}", dump(&lines));

        let lines = rendered(&quiet_model(60));
        let head = lines.iter().find(|l| l.contains("when (UTC)")).unwrap();
        assert!(
            !head.contains("linked") && head.contains("thoughts"),
            "{head}"
        );
        assert!(contains(&lines, "10-14 18:40"));
        assert!(!contains(&lines, "2026-10-14 18:40"));
        for line in &lines {
            assert!(text::width(line) <= 60, "{line}");
        }
    }

    #[test]
    fn the_title_takes_the_spare_width_and_is_cut_with_an_ellipsis() {
        let mut summaries = conversations();
        summaries[3].title =
            "An extraordinarily long title that cannot possibly fit beside its columns".to_owned();
        let mut model = model_of(summaries, vec![remote("origin")], (80, 30), Glyphs::UNICODE);
        let lines = rendered(&model);
        let row = lines.iter().find(|l| l.contains("An extraordina")).unwrap();
        assert!(row.contains('…'), "{row}");
        assert!(row.contains("● published"), "{row}");
        assert!(text::width(row) <= 80);
        model.set_size(160, 30);
        let lines = rendered(&model);
        let row = lines
            .iter()
            .find(|l| l.contains("An extraordinarily"))
            .unwrap();
        assert!(row.contains("beside its columns"), "{row}");
    }

    #[test]
    fn the_columns_line_up_across_rows() {
        let mut model = full_model();
        model.set_size(130, 30);
        let lines = rendered(&model);
        let head = lines.iter().position(|l| l.contains("when (UTC)")).unwrap();
        let column = |line: &str, label: &str| text::width(&line[..line.find(label).unwrap()]);
        let state_x = column(&lines[head], "state");
        let thoughts_end = column(&lines[head], "thoughts") + "thoughts".len();
        for row in &lines[head + 1..head + 4] {
            // The state label starts exactly in the state column.
            let start = ["◐", "●", "imported"]
                .iter()
                .filter_map(|glyph| row.find(glyph))
                .min()
                .expect("a state label");
            assert_eq!(text::width(&row[..start]), state_x, "{row}");
            // The numbers end where the column label ends.
            assert!(text::width(&row[..row.find(char::is_numeric).unwrap_or(0)]) <= thoughts_end);
        }
    }

    // ----- sanitizing -----

    #[test]
    fn hostile_titles_ids_and_notices_are_drawn_sanitized() {
        let mut summaries = conversations();
        summaries[3].title = "\u{1b}[2J\u{1b}]0;pwned\u{7}evil \u{202e}gnp.exe".to_owned();
        summaries[3].id = "bad\u{1b}[31mid".to_owned();
        let mut model = model_of(
            summaries,
            vec![remote("origin")],
            (100, 30),
            Glyphs::UNICODE,
        );
        model.set_status("note \u{1b}[2J with escapes \u{202e}");
        let (terminal, lines) = screen_of(&model, Glyphs::UNICODE, COLOR);
        for line in &lines {
            assert!(
                !line.chars().any(text::is_unsafe),
                "unsanitized text drawn: {line:?}"
            );
        }
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                assert!(
                    !buffer[(x, y)].symbol().chars().any(text::is_unsafe),
                    "({x},{y}) {:?}",
                    buffer[(x, y)].symbol()
                );
            }
        }
        assert!(
            contains(&lines, "?[2J?]0;pwned?evil ?gnp.exe"),
            "{}",
            dump(&lines)
        );
        assert!(contains(&lines, "note ?[2J with escapes ?"));
    }

    #[test]
    fn the_renderer_sanitizes_even_what_the_model_forgot() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 1));
        let mut canvas = Canvas {
            buf: &mut buffer,
            theme: COLOR,
        };
        canvas.put(0, 0, "a\u{1b}[2Jb\u{202e}c", 40, Style::default());
        let drawn = buffer_lines(&buffer);
        assert_eq!(drawn[0], "a?[2Jb?c");
    }

    // ----- the bottom line and the bars -----

    #[test]
    fn the_key_bar_in_full_and_narrow_forms() {
        let model = full_model();
        let lines = rendered(&model);
        assert_eq!(
            lines.last().unwrap(),
            " ↑↓ move   ←→ agents   enter details   s share   u unshare   p publish   r remote   ? help   q quit"
        );
        let mut narrow = full_model();
        narrow.set_size(60, 30);
        let last = rendered(&narrow).pop().unwrap();
        assert!(
            text::width(&last) <= 60 && last.ends_with("? help"),
            "{last}"
        );
    }

    #[test]
    fn unavailable_keys_are_dimmed_on_screen() {
        let mut model = full_model();
        // Move to the imported row: share and unshare are off.
        model.handle_key(key(KeyCode::End));
        let id = model.selected_id().unwrap().to_owned();
        let summary = conversations().into_iter().find(|s| s.id == id).unwrap();
        model.set_info(
            &id,
            Some(RowInfo {
                details: ConversationDetails {
                    summary: summary.clone(),
                    context_paths: vec![],
                    context_path_total: 0,
                    snapshot_count: 9,
                    snapshot_shared: 0,
                    snapshot_new: 9,
                },
                unshare: Some(UnshareCheck::default()),
            }),
        );
        let (terminal, lines) = screen_of(&model, Glyphs::UNICODE, COLOR);
        let y = lines.len() as u16 - 1;
        let bar = &lines[y as usize];
        let at = |needle: &str| text::width(&bar[..bar.find(needle).unwrap()]) as u16;
        let buffer = terminal.backend().buffer();
        assert!(buffer[(at("s share"), y)]
            .style()
            .add_modifier
            .contains(Modifier::DIM));
        assert!(buffer[(at("u unshare"), y)]
            .style()
            .add_modifier
            .contains(Modifier::DIM));
        assert!(!buffer[(at("p publish"), y)]
            .style()
            .add_modifier
            .contains(Modifier::DIM));
        assert!(buffer[(at("p publish"), y)]
            .style()
            .add_modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn the_share_bar_replaces_the_key_bar_in_two_lines() {
        let mut model = full_model();
        model.set_size(120, 30);
        model.handle_key(key(KeyCode::Char('s')));
        let lines = rendered(&model);
        let n = lines.len();
        assert_eq!(
            lines[n - 2],
            " Share \"Refactor the sync module\" (27 thoughts, 6 not yet shared) for 'origin' (github.com/acme/repo)?"
        );
        assert_eq!(
            lines[n - 1],
            " Enter: this snapshot only   f: also future thoughts   Esc: cancel"
        );
        assert!(!contains(&lines, "q quit"));
    }

    #[test]
    fn the_other_bars_in_two_lines() {
        let mut model = full_model();
        model.handle_key(key(KeyCode::Char('p')));
        let lines = rendered(&model);
        let n = lines.len();
        assert_eq!(
            lines[n - 2],
            " Publish shared thoughts to 'origin' (github.com/acme/repo)? 2 waiting by this clone's count."
        );
        assert_eq!(lines[n - 1], " Enter: continue   Esc: cancel");
        model.handle_key(key(KeyCode::Esc));

        model.handle_key(key(KeyCode::Char('u')));
        let lines = rendered(&model);
        let n = lines.len();
        assert!(lines[n - 2].starts_with(" Unshare \"Refactor the sync module\" for 'origin'? Its 21 shared thoughts become private again."), "{}", lines[n - 2]);
        assert_eq!(lines[n - 1], " Enter: continue to the phrase   Esc: cancel");
    }

    #[test]
    fn the_authorization_offer_and_a_blocked_bar() {
        let mut unauthorized = remote("origin");
        unauthorized.sharing_consented = false;
        let mut model = model_of(
            conversations(),
            vec![unauthorized],
            (100, 30),
            Glyphs::UNICODE,
        );
        read_info(&mut model, "session-1", 6);
        model.handle_key(key(KeyCode::Char('p')));
        let lines = rendered(&model);
        let n = lines.len();
        assert_eq!(
            lines[n - 2],
            " Publishing to 'origin' (github.com/acme/repo) is not authorized in this clone yet."
        );
        assert_eq!(
            lines[n - 1],
            " Enter: authorize (you will type a phrase)   Esc: cancel"
        );
        model.handle_key(key(KeyCode::Esc));

        let mut model = full_model();
        model.handle_key(key(KeyCode::Down));
        model.handle_key(key(KeyCode::Down));
        model.handle_key(key(KeyCode::Down));
        // The imported row has nothing read about it yet.
        model.handle_key(key(KeyCode::Char('s')));
        let lines = rendered(&model);
        assert!(
            lines.last().unwrap().contains("any key: ok"),
            "{}",
            dump(&lines)
        );
    }

    #[test]
    fn the_bar_text_is_bold_on_a_quiet_screen() {
        let mut model = full_model();
        model.handle_key(key(KeyCode::Char('s')));
        let (terminal, lines) = screen_of(&model, Glyphs::UNICODE, COLOR);
        let y = lines.len() as u16 - 2;
        assert!(terminal.backend().buffer()[(3, y)]
            .style()
            .add_modifier
            .contains(Modifier::BOLD));
    }

    // ----- the strip and the notice -----

    #[test]
    fn the_strip_describes_the_selected_conversation() {
        let model = full_model();
        let lines = rendered(&model);
        assert!(contains(&lines, "  Refactor the sync module"));
        assert!(contains(
            &lines,
            "  27 thoughts, 26 linked to commits, 2026-10-13 09:12 – 10-14 18:40 UTC"
        ));
        assert!(contains(
            &lines,
            "  Share covers 27 thoughts as of now (21 already shared, 6 new). Future thoughts: private."
        ));
    }

    #[test]
    fn a_long_notice_is_not_cut_off_in_the_middle_of_a_command() {
        use crate::commands::failure::FlowFailure;
        use crate::tui::model::Report;
        use crate::tui::words::FlowKind;

        let mut model = full_model();
        model.set_size(110, 30);
        model.report(Report::Failed {
            kind: FlowKind::Publish,
            failure: FlowFailure::Other("connection reset by peer".to_owned()),
        });
        let lines = rendered(&model);
        assert!(
            lines[2].starts_with(" Publish to 'origin' did not complete;"),
            "{}",
            dump(&lines)
        );
        let notice = format!("{} {}", lines[2].trim(), lines[3].trim());
        assert!(
            notice.contains("Run: cvc privacy reconcile --remote origin"),
            "{notice}"
        );
        assert!(lines[4].contains("when (UTC)"), "{}", dump(&lines));
    }

    #[test]
    fn there_is_no_strip_below_26_rows() {
        let mut model = full_model();
        model.set_size(100, 25);
        let lines = rendered(&model);
        assert!(!contains(&lines, "Share covers"));
        model.set_size(100, 26);
        assert!(contains(&rendered(&model), "Share covers"));
    }

    #[test]
    fn a_notice_is_one_line_under_the_header() {
        let mut model = full_model();
        model.set_status("27 thoughts marked shared for remote 'origin'; future thoughts are private. Not published yet (p).");
        let lines = rendered(&model);
        assert_eq!(
            lines[2],
            " 27 thoughts marked shared for remote 'origin'; future thoughts are private. Not published yet (p)."
        );
        assert!(lines[3].contains("when (UTC)"));
        model.handle_key(key(KeyCode::Down));
        let lines = rendered(&model);
        assert!(lines[2].contains("when (UTC)"));
    }

    // ----- overlays -----

    #[test]
    fn the_details_overlay_has_the_spec_s_rows() {
        let mut model = full_model();
        model.handle_key(key(KeyCode::Enter));
        let lines = rendered(&model);
        let text = dump(&lines);
        for expected in [
            "Refactor the sync module",
            "ID               session-1",
            "Activity         2026-10-13 09:12 – 10-14 18:40 UTC",
            "Thoughts         27",
            "Linked           26 of 27 thoughts linked to a commit",
            "Context files    14: src/sync.rs (12), src/db.rs (9), … (+12 more)",
            "Subagents        2 conversations, each shared separately; their prompts and results are part of",
            "this one",
            "With 'origin' (fingerprint 8348b79e2c41…)",
            "State            ◐ shared · 2 waiting · 6 private",
            "Future thoughts  private",
            "Share covers     27 thoughts: 21 already shared, 6 new",
            "Auto-push        off",
            "A thought is one recorded step (for Claude Code, one assistant response).",
            "cvc share counts thoughts as \"turns\"; cvc push counts them as \"interactions\".",
            "Scrubbing can miss secrets. Published thoughts cannot be recalled.",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
        assert!(lines.last().unwrap().contains("esc close"));
    }

    #[test]
    fn the_details_of_an_agent_and_an_orphan() {
        let mut model = full_model();
        model.handle_key(key(KeyCode::Right));
        model.handle_key(key(KeyCode::Down));
        model.handle_key(key(KeyCode::Enter));
        let text = dump(&rendered(&model));
        assert!(
            text.contains("Kind             Subagent of \"Refactor the sync module\""),
            "{text}"
        );

        let model = {
            let mut m = model_of(
                vec![conversation("lonely:agent-x", 100, 900, 2)],
                vec![remote("origin")],
                (100, 30),
                Glyphs::UNICODE,
            );
            m.handle_key(key(KeyCode::Enter));
            m
        };
        let text = dump(&rendered(&model));
        assert!(
            text.contains("Subagent. Parent session is not captured on this machine."),
            "{text}"
        );
    }

    #[test]
    fn a_tall_overlay_scrolls_on_a_short_terminal() {
        let mut model = full_model();
        model.set_size(60, 12);
        read_info(&mut model, "session-1", 6);
        model.handle_key(key(KeyCode::Enter));
        let first = rendered(&model);
        assert!(contains(&first, "Refactor the sync module"));
        assert!(
            !contains(&first, "Scrubbing can miss secrets"),
            "{}",
            dump(&first)
        );
        assert!(contains(&first, "more below"));
        model.handle_key(key(KeyCode::End));
        let last = rendered(&model);
        assert!(
            contains(&last, "Scrubbing can miss secrets"),
            "{}",
            dump(&last)
        );
        for line in &last {
            assert!(text::width(line) <= 60, "{line}");
        }
    }

    #[test]
    fn the_remote_picker_lists_remotes_with_their_state() {
        let mut other = remote("upstream");
        other.sharing_consented = false;
        other.location = None;
        let mut auto = remote("origin");
        auto.auto_push = true;
        let mut broken = remote("broken");
        broken.fingerprint = Err("remote has an empty push URL".to_owned());
        let mut model = model_of(
            conversations(),
            vec![auto, other, broken],
            (100, 30),
            Glyphs::UNICODE,
        );
        model.handle_key(key(KeyCode::Char('r')));
        let lines = rendered(&model);
        let text = dump(&lines);
        assert!(text.contains("Choose a remote"), "{text}");
        let flat = lines
            .join(" ")
            .replace(['│', '┌', '┐', '└', '┘', '─'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            flat.contains("Publishing is authorized per exact push URL, not per remote name:"),
            "{text}"
        );
        assert!(
            flat.contains("remotes with the same push URL share it."),
            "{text}"
        );
        for column in [
            "remote",
            "location",
            "fingerprint",
            "publishing",
            "auto-push",
        ] {
            assert!(text.contains(column), "{column}");
        }
        let origin = lines
            .iter()
            .find(|l| l.contains("origin") && l.contains("authorized"))
            .unwrap();
        assert!(
            origin.contains("github.com/acme/repo")
                && origin.contains("8348b79e2c41…")
                && origin.contains(" on"),
            "{origin}"
        );
        let upstream = lines.iter().find(|l| l.contains("upstream")).unwrap();
        assert!(
            upstream.contains("–")
                && upstream.contains("not authorized yet")
                && upstream.contains("off"),
            "{upstream}"
        );
        let broken = lines.iter().find(|l| l.contains("broken")).unwrap();
        assert!(broken.contains("remote has an empty push URL"), "{broken}");
        assert!(lines.last().unwrap().contains("enter select"));
    }

    #[test]
    fn the_remote_picker_keeps_the_cursor_in_view_with_many_remotes() {
        let remotes: Vec<_> = (0..30).map(|n| remote(&format!("remote-{n:02}"))).collect();
        let mut model = model_of(conversations(), remotes, (60, 12), Glyphs::UNICODE);
        model.handle_key(key(KeyCode::Char('r')));
        let before = dump(&rendered(&model));
        assert!(!before.contains("remote-29"), "{before}");
        model.handle_key(key(KeyCode::End));
        let after = rendered(&model);
        assert!(contains(&after, "remote-29"), "{}", dump(&after));
        for line in &after {
            assert!(text::width(line) <= 60, "{line}");
        }
    }

    #[test]
    fn the_help_page_fits_and_scrolls() {
        let mut model = full_model();
        model.handle_key(key(KeyCode::Char('?')));
        let text = dump(&rendered(&model));
        assert!(
            text.contains("private --s--> shared (in this clone) --p--> published (on the remote)"),
            "{text}"
        );
        assert!(text.contains("u works only before publish."));
        // On a tall terminal the whole page is in view, with what it says about
        // consent: no key does it alone, and two choices act later.
        model.set_size(100, 60);
        let text = dump(&rendered(&model));
        for expected in [
            "Share, unshare, publish and authorize each wait for their typed phrase; no key does any of them",
            "by itself.",
            "Two standing choices act later without a new phrase: +future shares new thoughts as they are",
            "captured, and auto-push (set only with cvc privacy set-auto-push) publishes shared thoughts at",
            "every git push to that remote.",
            "Scrubbing and .thoughtignore can miss secrets; they are not a guarantee.",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
        model.set_size(60, 12);
        model.handle_key(key(KeyCode::End));
        let text = dump(&rendered(&model));
        assert!(text.contains("\"interactions\""), "{text}");
    }

    // ----- sizes -----

    #[test]
    fn a_terminal_below_60_by_12_shows_only_the_notice() {
        let mut model = full_model();
        model.set_size(52, 9);
        let lines = rendered(&model);
        assert_eq!(lines[0], "Terminal too small (need 60x12, have 52x9).");
        assert!(lines[1..].iter().all(|l| l.is_empty()));
    }

    #[test]
    fn the_smallest_supported_terminal_draws_everything_that_matters() {
        let mut model = full_model();
        model.set_size(60, 12);
        let lines = rendered(&model);
        assert_eq!(lines.len(), 12);
        for line in &lines {
            assert!(text::width(line) <= 60, "{line}");
        }
        assert!(lines[0].starts_with("CVC conversations"));
        assert!(contains(&lines, "when (UTC)"));
        assert!(contains(&lines, "Refactor the"), "{}", dump(&lines));
        assert!(contains(&lines, "q quit"), "{}", dump(&lines));
        // A bar at this size still shows its question and its keys.
        model.handle_key(key(KeyCode::Char('s')));
        let lines = rendered(&model);
        let bar = lines[lines.len() - model.layout().bottom..].join(" ");
        assert!(
            bar.contains("'origin'") && bar.contains("(github.com/acme/repo)?"),
            "{bar}"
        );
        assert!(
            bar.contains("Enter:") && bar.contains("f:") && bar.contains("Esc: cancel"),
            "{bar}"
        );
        for line in &lines {
            assert!(text::width(line) <= 60, "{line}");
        }
    }

    // ----- random use -----

    /// A small deterministic generator, so a failure can be replayed.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) % n as u64) as usize
        }
    }

    /// Whatever keys are pressed and however the window is resized, nothing
    /// panics and the screen can always be drawn; and a flow is asked for only
    /// by the key a bar lists, while that bar is open: no sequence of keys gets
    /// to a flow any other way.
    #[test]
    fn no_key_sequence_asks_for_a_flow_without_its_bar_and_nothing_panics() {
        use crate::tui::model::{Action, Bar, Flow};
        use cvc_core::models::FutureSharePolicy;

        let codes = [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::Backspace,
        ];
        let characters: Vec<char> = "sufprq?jkhlxSUPFQ aA".chars().collect();
        let mut rng = Lcg(7);
        let mut flows = 0;
        let mut quits = 0;
        for _round in 0..24 {
            let mut model = full_model();
            for _step in 0..100 {
                if rng.below(9) == 0 {
                    let (width, height) = (40 + rng.below(100) as u16, 6 + rng.below(40) as u16);
                    model.set_size(width, height);
                }
                // As the event loop does before each key: the selected row, read now.
                if let Some(id) = model.selected_id().map(str::to_owned) {
                    read_info(&mut model, &id, rng.below(3));
                }
                let code = if rng.below(3) == 0 {
                    KeyCode::Char(characters[rng.below(characters.len())])
                } else {
                    codes[rng.below(codes.len())]
                };
                let event = KeyEvent::new(code, KeyModifiers::NONE);
                let bar_before = model.bar().cloned();
                let too_small = model.too_small();
                match model.handle_key(event) {
                    Some(Action::Run(flow)) => {
                        flows += 1;
                        assert!(!too_small, "a flow from a window too small to read");
                        let listed = match (&bar_before, &flow, code) {
                            (
                                Some(Bar::Share { id, .. }),
                                Flow::Share { id: asked, future },
                                KeyCode::Enter,
                            ) => id == asked && *future == FutureSharePolicy::Private,
                            (
                                Some(Bar::Share { id, .. }),
                                Flow::Share { id: asked, future },
                                KeyCode::Char('f' | 'F'),
                            ) => id == asked && *future == FutureSharePolicy::Shared,
                            (
                                Some(Bar::Unshare { id, .. }),
                                Flow::Unshare { id: asked },
                                KeyCode::Enter,
                            ) => id == asked,
                            (Some(Bar::Publish { .. }), Flow::Publish, KeyCode::Enter) => true,
                            (Some(Bar::Authorize), Flow::Authorize, KeyCode::Enter) => true,
                            _ => false,
                        };
                        assert!(
                            listed,
                            "{flow:?} asked for by {code:?} with the bar {bar_before:?}"
                        );
                        assert!(model.bar().is_none(), "the bar closes when it asks");
                    }
                    Some(Action::Quit) => quits += 1,
                    Some(Action::Switch(_)) | None => {}
                }
                let (width, height) = model.size();
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| render(frame, &model, COLOR)).unwrap();
            }
        }
        assert!(flows > 5, "the walk should reach flows, reached {flows}");
        assert!(quits > 0);
    }

    // ----- ASCII fallback -----

    #[test]
    fn the_ascii_fallback_draws_no_non_ascii_glyphs_for_data_it_controls() {
        let mut model = model_of(
            conversations(),
            vec![remote("origin")],
            (130, 30),
            Glyphs::ASCII,
        );
        read_info(&mut model, "session-1", 6);
        model.handle_key(key(KeyCode::Right));
        let lines = rendered(&model);
        let text = dump(&lines);
        // The only non-ASCII on screen is the data.
        assert!(
            text.is_ascii(),
            "{}",
            text.chars().filter(|c| !c.is_ascii()).collect::<String>()
        );
        assert!(text.contains("v "), "an open parent is v");
        assert!(text.contains("`- agent"));
        assert!(text.contains("o private"));
        assert!(text.contains("~ shared - 2 waiting - 6 private"));
        assert!(text.contains("* published"));
        assert!(text.contains("up/down move"));
        assert!(text.contains("remote: origin -> github.com/acme/repo (8348b79e2c41...)"));

        model.handle_key(key(KeyCode::Left));
        let text = dump(&rendered(&model));
        assert!(text.contains("> "), "a closed parent is >");
        assert!(text.contains("+2 agents - 1 waiting"));
        model.handle_key(key(KeyCode::Enter));
        let text = dump(&rendered(&model));
        assert!(
            text.is_ascii(),
            "{}",
            text.chars().filter(|c| !c.is_ascii()).collect::<String>()
        );
        assert!(text.contains("2026-10-13 09:12 - 10-14 18:40 UTC"));
        assert!(text.contains("(+12 more)"));
    }
}
