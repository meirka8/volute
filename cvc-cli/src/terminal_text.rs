//! What text may reach a terminal as itself.
//!
//! A terminal obeys what it is sent: an ESC starts an escape sequence that can
//! move the cursor, clear the screen, retitle the window or set the clipboard;
//! a bidirectional override reverses the text around it; a zero-width character
//! hides. Titles (a Claude Code `ai-title`, a subagent description), conversation
//! ids, context paths, remote names and the text of errors all come from places
//! a person did not write by hand, and an imported conversation is whatever a
//! remote's projection says. Anything of that kind that is printed or drawn goes
//! through [`sanitize_for_terminal`] first.
//!
//! Defense in depth, not a promise: this keeps control and format characters off
//! the terminal, and says nothing about whether the visible text is true.

use std::borrow::Cow;

/// Whether `c` must not reach a terminal as itself: any character of the
/// Unicode general categories
///
/// - `Cc`: the C0 and C1 controls, which include ESC, DEL, CSI and NEL;
/// - `Cf`: the format characters, which include the bidirectional controls and
///   isolates, the Arabic letter mark, the soft hyphen, the zero-width space,
///   joiners and no-break space (BOM), the interlinear annotation anchors and
///   the tag characters;
/// - `Zl` and `Zp`: the line and paragraph separators U+2028 and U+2029.
pub(crate) fn is_unsafe_for_terminal(c: char) -> bool {
    c.is_control() || is_format(c) || matches!(c, '\u{2028}' | '\u{2029}')
}

/// Unicode general category `Cf`.
fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

/// `text` with every character [`is_unsafe_for_terminal`] replaced by `?`.
/// Borrows when there is nothing to replace.
pub(crate) fn sanitize_for_terminal(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_unsafe_for_terminal) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|c| if is_unsafe_for_terminal(c) { '?' } else { c })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_is_borrowed_unchanged() {
        for text in [
            "",
            "plain text",
            "שלום – café – 日本語 – ✓",
            "emoji 🙂 and ünïcödé",
        ] {
            assert!(matches!(sanitize_for_terminal(text), Cow::Borrowed(t) if t == text));
        }
    }

    #[test]
    fn controls_escape_sequences_and_osc_are_replaced() {
        // ESC starts every escape sequence; none can start.
        assert_eq!(sanitize_for_terminal("a\u{1b}[2Jb"), "a?[2Jb");
        assert_eq!(
            sanitize_for_terminal("\u{1b}]0;retitled\u{7}x"),
            "?]0;retitled?x"
        );
        assert_eq!(
            sanitize_for_terminal("\u{1b}]52;c;AAAA\u{1b}\\"),
            "?]52;c;AAAA?\\"
        );
        // C0, DEL, and C1 (including NEL and the single-byte CSI).
        assert_eq!(
            sanitize_for_terminal("\u{0}\u{7}\u{8}\u{b}\u{c}\u{7f}\u{80}\u{85}\u{9b}\u{9f}"),
            "??????????"
        );
        // Line breaks and tabs are controls too: one line stays one line.
        assert_eq!(
            sanitize_for_terminal("one\ntwo\r\nthree\tfour"),
            "one?two??three?four"
        );
    }

    #[test]
    fn format_characters_are_replaced() {
        // Bidirectional overrides, embeddings, isolates and marks.
        assert_eq!(
            sanitize_for_terminal("a\u{202a}b\u{202b}c\u{202c}d\u{202d}e\u{202e}f"),
            "a?b?c?d?e?f"
        );
        assert_eq!(
            sanitize_for_terminal("a\u{2066}b\u{2067}c\u{2068}d\u{2069}e"),
            "a?b?c?d?e"
        );
        assert_eq!(
            sanitize_for_terminal("a\u{200e}b\u{200f}c\u{061c}d"),
            "a?b?c?d"
        );
        // Zero-width space, joiners, word joiner, invisible operators, BOM.
        assert_eq!(
            sanitize_for_terminal("a\u{200b}b\u{200c}c\u{200d}d\u{2060}e\u{2062}f\u{feff}g"),
            "a?b?c?d?e?f?g"
        );
        // Soft hyphen, Mongolian vowel separator, deprecated format characters.
        assert_eq!(
            sanitize_for_terminal("a\u{ad}b\u{180e}c\u{206a}d\u{206f}e"),
            "a?b?c?d?e"
        );
        // Interlinear annotation anchors and tag characters.
        assert_eq!(
            sanitize_for_terminal("a\u{fff9}b\u{fffa}c\u{fffb}d\u{e0001}e\u{e0041}f\u{e007f}g"),
            "a?b?c?d?e?f?g"
        );
        // Format characters of other scripts.
        assert_eq!(
            sanitize_for_terminal("\u{600}\u{605}\u{6dd}\u{70f}\u{8e2}\u{110bd}\u{1d173}\u{1bca0}"),
            "????????"
        );
    }

    #[test]
    fn line_and_paragraph_separators_are_replaced() {
        assert_eq!(sanitize_for_terminal("x\u{2028}y\u{2029}z"), "x?y?z");
    }

    #[test]
    fn text_around_replaced_characters_is_kept() {
        assert_eq!(
            sanitize_for_terminal("pre \u{202e}gnp.exe\u{202c} post"),
            "pre ?gnp.exe? post"
        );
        assert_eq!(sanitize_for_terminal("שלום\u{200f} עולם"), "שלום? עולם");
    }

    /// The table above is the Unicode definition: every scalar value agrees with
    /// the regex crate's tables for the four categories.
    #[test]
    fn the_unsafe_set_is_exactly_the_four_general_categories() {
        let categories = regex::Regex::new(r"^[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]$").unwrap();
        let mut buffer = [0u8; 4];
        let mut unsafe_count = 0;
        for value in 0..=0x10_ffffu32 {
            let Some(c) = char::from_u32(value) else {
                continue;
            };
            let expected = categories.is_match(c.encode_utf8(&mut buffer));
            assert_eq!(
                is_unsafe_for_terminal(c),
                expected,
                "U+{value:04X} disagrees with the Unicode categories"
            );
            unsafe_count += usize::from(expected);
        }
        assert!(unsafe_count > 150, "{unsafe_count}");
    }
}
