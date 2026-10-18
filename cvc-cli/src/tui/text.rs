//! Text handling for the interactive view: what may be drawn at all, which
//! glyphs to draw it with, and how to fit it into a given number of terminal
//! columns.
//!
//! Everything the view shows that did not come from this file's own string
//! literals (titles, ids, context paths, remote names, reasons and error text)
//! is untrusted: an imported conversation is whatever a remote's projection
//! says, and a title can carry anything a prompt did. [`sanitize`] is the one
//! gate; the model applies it where it builds a string, so widths are computed
//! on what will really be drawn, and the renderer applies it again to every
//! string it puts in the screen buffer, so a missed call cannot become an
//! escape sequence. The rule itself lives in `crate::terminal_text`, which the
//! rest of the CLI uses for the text it prints.

use unicode_width::UnicodeWidthChar;

/// What a title shows as when it has no text.
pub const UNTITLED: &str = "(untitled)";

/// `text` with every character the terminal must not receive as itself
/// replaced by `?`: the control, format and separator characters that
/// `terminal_text` defines, shared with everything else in the CLI that prints
/// text from the store or from Git.
pub fn sanitize(text: &str) -> String {
    crate::terminal_text::sanitize_for_terminal(text).into_owned()
}

/// Whether `c` is one of those characters.
#[cfg(test)]
pub fn is_unsafe(c: char) -> bool {
    crate::terminal_text::is_unsafe_for_terminal(c)
}

/// A conversation title as shown: sanitized, and [`UNTITLED`] when it has no
/// text.
pub fn title(text: &str) -> String {
    if text.trim().is_empty() {
        UNTITLED.to_owned()
    } else {
        sanitize(text)
    }
}

/// The first line of `text`, which is all a one-line notice can carry.
pub fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

/// Terminal columns `c` takes.
pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Terminal columns `text` takes.
pub fn width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// `text` cut to at most `max` columns, ending in `ellipsis` when something
/// was cut. A wide character is never split.
pub fn truncate(text: &str, max: usize, ellipsis: &str) -> String {
    if width(text) <= max {
        return text.to_owned();
    }
    let ellipsis_width = width(ellipsis);
    if max <= ellipsis_width {
        // No room for any of the text: as much of the ellipsis as fits.
        return take_columns(ellipsis, max);
    }
    let mut kept = take_columns(text, max - ellipsis_width);
    kept.push_str(ellipsis);
    kept
}

/// The longest prefix of `text` that fits in `max` columns.
fn take_columns(text: &str, max: usize) -> String {
    let mut used = 0;
    let mut kept = String::new();
    for c in text.chars() {
        let w = char_width(c);
        if used + w > max {
            break;
        }
        used += w;
        kept.push(c);
    }
    kept
}

/// `text` padded with spaces on the left to `columns` columns (never cut).
pub fn pad_start(text: &str, columns: usize) -> String {
    let mut padded: String =
        std::iter::repeat_n(' ', columns.saturating_sub(width(text))).collect();
    padded.push_str(text);
    padded
}

/// `text` broken into lines of at most `columns` columns: at spaces where it
/// can, inside a word that is longer than a line where it must. Always at least
/// one line, so an empty value still takes its row.
pub fn wrap(text: &str, columns: usize) -> Vec<String> {
    let columns = columns.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0;
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        let word_width = width(word);
        if line_width > 0 && line_width + 1 + word_width <= columns {
            line.push(' ');
            line.push_str(word);
            line_width += 1 + word_width;
            continue;
        }
        if line_width > 0 {
            lines.push(std::mem::take(&mut line));
        }
        // A word longer than a whole line is broken wherever the line ends.
        let mut rest = word;
        while width(rest) > columns {
            let head = take_columns(rest, columns);
            if head.is_empty() {
                // A character wider than the line itself: drop it rather than loop.
                let mut chars = rest.chars();
                chars.next();
                rest = chars.as_str();
                continue;
            }
            rest = &rest[head.len()..];
            lines.push(head);
        }
        line.push_str(rest);
        line_width = width(rest);
    }
    if line_width > 0 || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// The glyphs the view draws with: Unicode where the terminal is expected to
/// have them, plain ASCII otherwise. One word of meaning accompanies every
/// glyph elsewhere, so the ASCII forms lose nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyphs {
    unicode: bool,
}

impl Glyphs {
    pub const UNICODE: Glyphs = Glyphs { unicode: true };
    pub const ASCII: Glyphs = Glyphs { unicode: false };

    fn pick(self, unicode: &'static str, ascii: &'static str) -> &'static str {
        if self.unicode {
            unicode
        } else {
            ascii
        }
    }

    /// A parent whose agents are hidden.
    pub fn collapsed(self) -> &'static str {
        self.pick("▸", ">")
    }
    /// A parent whose agents are shown.
    pub fn expanded(self) -> &'static str {
        self.pick("▾", "v")
    }
    /// Leads an agent row.
    pub fn child(self) -> &'static str {
        self.pick("└─", "`-")
    }
    /// Separates the parts of a label.
    pub fn dot(self) -> &'static str {
        self.pick("·", "-")
    }
    pub fn ellipsis(self) -> &'static str {
        self.pick("…", "...")
    }
    /// A horizontal rule.
    pub fn rule(self) -> &'static str {
        self.pick("─", "-")
    }
    /// Whether box-drawing and the other Unicode glyphs are in use.
    pub fn is_unicode(self) -> bool {
        self.unicode
    }
    /// A range, and "no value".
    pub fn dash(self) -> &'static str {
        self.pick("–", "-")
    }
    pub fn up_down(self) -> &'static str {
        self.pick("↑↓", "up/down")
    }
    pub fn left_right(self) -> &'static str {
        self.pick("←→", "left/right")
    }
    pub fn right(self) -> &'static str {
        self.pick("→", "->")
    }
    /// Nothing is shared.
    pub fn private(self) -> &'static str {
        self.pick("○", "o")
    }
    /// Shared here, some of it not yet published.
    pub fn waiting(self) -> &'static str {
        self.pick("◐", "~")
    }
    /// Everything shared is published.
    pub fn published(self) -> &'static str {
        self.pick("●", "*")
    }
}

/// What the terminal is expected to handle, read once from the environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Environment {
    pub glyphs: Glyphs,
    /// Colors are used. `NO_COLOR` turns them off; bold, dim and reverse video
    /// stay.
    pub color: bool,
}

impl Environment {
    pub fn detect() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// As [`Environment::detect`], reading variables through `lookup`.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let unicode = lookup("TERM").as_deref() != Some("linux") && locale_is_utf8(&lookup);
        let color = lookup("NO_COLOR").is_none_or(|value| value.is_empty());
        Self {
            glyphs: if unicode {
                Glyphs::UNICODE
            } else {
                Glyphs::ASCII
            },
            color,
        }
    }
}

/// Whether the locale in effect (the first of `LC_ALL`, `LC_CTYPE` and `LANG`
/// that is set and not empty, which is the order the C library applies) names
/// the UTF-8 character set.
fn locale_is_utf8(lookup: &impl Fn(&str) -> Option<String>) -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .filter_map(lookup)
        .find(|value| !value.is_empty())
        .is_some_and(|value| codeset_is_utf8(&value))
}

/// `language[_TERRITORY][.codeset][@modifier]`. A name with a codeset says what
/// it is. A name without one (`en_IL`) is whatever the system maps it to, which
/// is UTF-8 on every current glibc and macOS; only `C` and `POSIX`, the
/// locales that are ASCII, are not.
fn codeset_is_utf8(locale: &str) -> bool {
    let name = locale.split('@').next().unwrap_or("");
    match name.split_once('.') {
        Some((_, codeset)) => {
            let codeset: String = codeset
                .chars()
                .filter(|c| !matches!(c, '-' | '_'))
                .map(|c| c.to_ascii_lowercase())
                .collect();
            codeset == "utf8"
        }
        None => !matches!(name, "" | "C" | "POSIX"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn environment(vars: &[(&str, &str)]) -> Environment {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        Environment::from_lookup(|name| vars.get(name).cloned())
    }

    #[test]
    fn the_views_gate_is_the_shared_terminal_text_rule() {
        assert_eq!(sanitize("plain text"), "plain text");
        assert_eq!(sanitize("a\u{1b}[2Jb"), "a?[2Jb");
        assert_eq!(sanitize("a\u{202e}b\u{2066}c\u{200b}d"), "a?b?c?d");
        assert_eq!(sanitize("שלום – café – 日本語"), "שלום – café – 日本語");
        assert!(is_unsafe('\u{1b}') && is_unsafe('\u{202e}') && !is_unsafe('a'));
        for c in ['\u{1b}', '\u{85}', '\u{2028}', '\u{feff}', '\u{e0041}'] {
            assert_eq!(
                is_unsafe(c),
                crate::terminal_text::is_unsafe_for_terminal(c)
            );
        }
    }

    #[test]
    fn an_empty_title_becomes_untitled() {
        assert_eq!(title(""), UNTITLED);
        assert_eq!(title("  \n\t "), UNTITLED);
        assert_eq!(title("Fix the build"), "Fix the build");
        assert_eq!(title("odd\u{1b}[31m"), "odd?[31m");
    }

    #[test]
    fn widths_count_columns_not_characters() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("日本"), 4);
        assert_eq!(width("é"), 1);
        assert_eq!(width("a\u{301}"), 1);
    }

    #[test]
    fn truncation_is_by_display_width_with_an_ellipsis() {
        assert_eq!(truncate("short", 10, "…"), "short");
        assert_eq!(truncate("exactly10!", 10, "…"), "exactly10!");
        assert_eq!(truncate("a longer title", 8, "…"), "a longe…");
        assert_eq!(truncate("a longer title", 8, "..."), "a lon...");
        assert_eq!(truncate("日本語のタイトル", 7, "…"), "日本語…");
        assert_eq!(width(&truncate("日本語のタイトル", 7, "…")), 7);
        // A wide character that does not fit is left out, not split.
        assert_eq!(truncate("日本語のタイトル", 6, "…"), "日本…");
        assert_eq!(truncate("abcdef", 1, "…"), "…");
        assert_eq!(truncate("abcdef", 2, "..."), "..");
        assert_eq!(truncate("abcdef", 0, "…"), "");
    }

    #[test]
    fn padding_aligns_on_display_width() {
        assert_eq!(pad_start("ab", 5), "   ab");
        assert_eq!(pad_start("日本", 6), "  日本");
        assert_eq!(pad_start("toolong", 3), "toolong");
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_inside_long_words() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap("one two three", 20), vec!["one two three"]);
        assert_eq!(wrap("", 10), vec![""]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("ab abcdefgh", 4), vec!["ab", "abcd", "efgh"]);
        for line in wrap("日本語のタイトルです", 5) {
            assert!(width(&line) <= 5, "{line}");
        }
    }

    #[test]
    fn unicode_and_ascii_glyphs_carry_the_same_meaning() {
        let u = Glyphs::UNICODE;
        let a = Glyphs::ASCII;
        assert_eq!((u.collapsed(), a.collapsed()), ("▸", ">"));
        assert_eq!((u.expanded(), a.expanded()), ("▾", "v"));
        assert_eq!((u.child(), a.child()), ("└─", "`-"));
        assert_eq!((u.dot(), a.dot()), ("·", "-"));
        assert_eq!((u.ellipsis(), a.ellipsis()), ("…", "..."));
        assert_eq!((u.dash(), a.dash()), ("–", "-"));
        assert_eq!((u.up_down(), a.up_down()), ("↑↓", "up/down"));
        assert_eq!((u.private(), a.private()), ("○", "o"));
        assert_eq!((u.waiting(), a.waiting()), ("◐", "~"));
        assert_eq!((u.published(), a.published()), ("●", "*"));
        for glyph in [
            a.collapsed(),
            a.expanded(),
            a.child(),
            a.dot(),
            a.ellipsis(),
            a.dash(),
            a.up_down(),
            a.left_right(),
            a.right(),
            a.private(),
            a.waiting(),
            a.published(),
        ] {
            assert!(glyph.is_ascii(), "{glyph}");
        }
    }

    #[test]
    fn a_utf8_locale_gets_unicode_and_anything_else_ascii() {
        assert_eq!(
            environment(&[("LANG", "en_US.UTF-8")]).glyphs,
            Glyphs::UNICODE
        );
        assert_eq!(environment(&[("LC_ALL", "C.utf8")]).glyphs, Glyphs::UNICODE);
        assert_eq!(
            environment(&[("LANG", "de_DE.UTF-8@euro")]).glyphs,
            Glyphs::UNICODE
        );
        // A named locale without a codeset is UTF-8 on current systems.
        assert_eq!(environment(&[("LANG", "en_IL")]).glyphs, Glyphs::UNICODE);
        assert_eq!(environment(&[]).glyphs, Glyphs::ASCII);
        assert_eq!(environment(&[("LANG", "C")]).glyphs, Glyphs::ASCII);
        assert_eq!(environment(&[("LANG", "POSIX")]).glyphs, Glyphs::ASCII);
        assert_eq!(
            environment(&[("LANG", "en_US.ISO-8859-1")]).glyphs,
            Glyphs::ASCII
        );
        // The order is LC_ALL, LC_CTYPE, LANG, and an empty one does not count.
        assert_eq!(
            environment(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")]).glyphs,
            Glyphs::ASCII
        );
        assert_eq!(
            environment(&[("LC_CTYPE", "en_US.UTF-8"), ("LANG", "C")]).glyphs,
            Glyphs::UNICODE
        );
        assert_eq!(
            environment(&[("LC_ALL", ""), ("LANG", "en_US.UTF-8")]).glyphs,
            Glyphs::UNICODE
        );
    }

    #[test]
    fn the_linux_console_gets_ascii_even_with_a_utf8_locale() {
        assert_eq!(
            environment(&[("TERM", "linux"), ("LANG", "en_US.UTF-8")]).glyphs,
            Glyphs::ASCII
        );
        assert_eq!(
            environment(&[("TERM", "xterm-256color"), ("LANG", "en_US.UTF-8")]).glyphs,
            Glyphs::UNICODE
        );
    }

    #[test]
    fn no_color_turns_off_color_and_nothing_else() {
        let plain = environment(&[("LANG", "en_US.UTF-8")]);
        assert!(plain.color);
        let no_color = environment(&[("LANG", "en_US.UTF-8"), ("NO_COLOR", "1")]);
        assert!(!no_color.color);
        assert_eq!(no_color.glyphs, Glyphs::UNICODE);
        // An empty NO_COLOR does not count, as the convention says.
        assert!(environment(&[("NO_COLOR", "")]).color);
    }
}
