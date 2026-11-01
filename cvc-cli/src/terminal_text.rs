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
//! The text of an error can also quote a URL as it was configured, with its user
//! information (a user name and a password or token): libgit2 does so for a
//! `file://` URL it cannot resolve. What `cvc` prints of an error goes through
//! [`sanitize_error_for_terminal`], which also takes the user information out of
//! every URL in it ([`without_url_userinfo`]).
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
    replace_where(text, is_unsafe_for_terminal)
}

/// As [`sanitize_for_terminal`], for text that is several lines by nature, such
/// as an error and the causes under it, where a line feed is the structure of
/// the text and not a control that could act on the terminal. Every other
/// character of the set is replaced, a carriage return included: it would move
/// the cursor back over what was just printed.
///
/// A line feed cannot redraw anything that was printed before it; what it can
/// do is start a line, which a message with several lines does anyway. Text that
/// is one line by nature (a title, an id, a name) goes through
/// [`sanitize_for_terminal`], so that it stays one line.
pub(crate) fn sanitize_lines_for_terminal(text: &str) -> Cow<'_, str> {
    replace_where(text, |c| c != '\n' && is_unsafe_for_terminal(c))
}

/// The text of an error as it may be printed: every control and format character
/// but the line feed replaced, as [`sanitize_lines_for_terminal`] does, then the
/// user information taken out of every URL in it ([`without_url_userinfo`]).
/// Borrows when there is nothing to change.
///
/// Replacing first means that a control character inside a URL's user
/// information (a tab, a carriage return) cannot end the URL early and keep the
/// rest of the user information in.
pub(crate) fn sanitize_error_for_terminal(text: &str) -> Cow<'_, str> {
    match sanitize_lines_for_terminal(text) {
        Cow::Borrowed(text) => without_url_userinfo(text),
        Cow::Owned(text) => Cow::Owned(without_url_userinfo(&text).into_owned()),
    }
}

/// `text` with the user information taken out of every URL in it, the way
/// `cvc_core::privacy::url_without_userinfo` takes it out of a push URL: in each
/// `<scheme>://<authority>`, everything in the authority up to and including its
/// last `@` goes (libgit2 takes the host from after the last `@`), and nothing
/// else changes. Borrows when there is nothing to take out.
///
/// libgit2 ends an authority at the first `/` only. A URL quoted in a message has
/// words around it, so here the authority also ends at white space, and at the
/// end of the text. What follows a URL that has no path (a closing quote, a
/// colon) is then read as part of its authority, which changes anything only
/// when an `@` comes before the next `/` or white space: the text up to that `@`
/// goes too, so this can show less than the message said, never more.
pub(crate) fn without_url_userinfo(text: &str) -> Cow<'_, str> {
    let mut stripped: Option<String> = None;
    // Everything before `kept_until` is in `stripped` already, or stays as it is.
    let mut kept_until = 0;
    let mut from = 0;
    while let Some(found) = text[from..].find("://") {
        let authority_start = from + found + "://".len();
        let authority_end = text[authority_start..]
            .find(|c: char| c == '/' || c.is_whitespace())
            .map_or(text.len(), |end| authority_start + end);
        if let Some(at) = text[authority_start..authority_end].rfind('@') {
            let out = stripped.get_or_insert_with(String::new);
            out.push_str(&text[kept_until..authority_start]);
            kept_until = authority_start + at + 1;
        }
        from = authority_end;
    }
    match stripped {
        None => Cow::Borrowed(text),
        Some(mut out) => {
            out.push_str(&text[kept_until..]);
            Cow::Owned(out)
        }
    }
}

/// `text` with every character `is_unsafe` accepts replaced by `?`. Borrows when
/// there is nothing to replace.
fn replace_where(text: &str, is_unsafe: impl Fn(char) -> bool) -> Cow<'_, str> {
    if !text.chars().any(&is_unsafe) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|c| if is_unsafe(c) { '?' } else { c })
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

    #[test]
    fn lines_keep_their_line_feeds_and_lose_everything_else_that_is_unsafe() {
        let several = "error: first\n  caused by: second\nthird";
        assert!(matches!(
            sanitize_lines_for_terminal(several),
            Cow::Borrowed(t) if t == several
        ));
        // The same text with an escape sequence and a carriage return in it.
        assert_eq!(
            sanitize_lines_for_terminal("a\u{1b}[2Kb\r\nc\u{202e}d\n\u{1b}]0;title\u{7}e"),
            "a?[2Kb?\nc?d\n?]0;title?e"
        );
        // A single line is still a single line where it must be.
        assert_eq!(sanitize_for_terminal("a\nb"), "a?b");
        // Tabs are controls too, and nothing else is spared.
        assert_eq!(sanitize_lines_for_terminal("a\tb\u{85}c"), "a?b?c");
        assert!(matches!(sanitize_lines_for_terminal(""), Cow::Borrowed("")));
    }

    #[test]
    fn user_information_is_taken_out_of_every_url_and_nothing_else_changes() {
        for text in [
            "",
            "failed to connect to 127.0.0.1: Connection refused",
            "https://github.com/acme/repo.git",
            "git@github.com:acme/repo.git",
            "file:///srv/repo.git and mailto:someone@example.invalid",
            "https://example.invalid/@scope/package, from user@example.invalid",
        ] {
            assert!(
                matches!(without_url_userinfo(text), Cow::Borrowed(t) if t == text),
                "{text}"
            );
        }
        // How libgit2 quotes a `file://` URL it cannot resolve.
        assert_eq!(
            without_url_userinfo(
                "failed to resolve path 'file://user:s3cret@localhost/srv/x.git': No such file or directory"
            ),
            "failed to resolve path 'file://localhost/srv/x.git': No such file or directory"
        );
        // The host is what follows the last `@`, as libgit2 reads it.
        assert_eq!(
            without_url_userinfo("https://a@b:c@example.invalid:8443/x"),
            "https://example.invalid:8443/x"
        );
        // A URL without a path ends at white space.
        assert_eq!(
            without_url_userinfo("'https://deploy:tok@example.invalid': refused"),
            "'https://example.invalid': refused"
        );
        // Every URL in the text, whatever its scheme.
        assert_eq!(
            without_url_userinfo(
                "ssh://git@example.invalid:2222/o/r.git, then http://u:p@10.0.0.1/r"
            ),
            "ssh://example.invalid:2222/o/r.git, then http://10.0.0.1/r"
        );
        // An `@` after the authority is part of the path and stays.
        assert_eq!(
            without_url_userinfo("https://u:p@example.invalid/@scope/x"),
            "https://example.invalid/@scope/x"
        );
    }

    #[test]
    fn an_error_cannot_act_on_the_terminal_and_names_no_user_information() {
        assert_eq!(
            sanitize_error_for_terminal(
                "push failed\nGit error: failed to resolve path 'file://user:s3cret@localhost/x\u{1b}[2J.git'\u{202e}"
            ),
            "push failed\nGit error: failed to resolve path 'file://localhost/x?[2J.git'?"
        );
        // A control character inside the user information does not keep the
        // rest of it in.
        assert_eq!(
            sanitize_error_for_terminal("https://us\ter:p\rw@example.invalid/r"),
            "https://example.invalid/r"
        );
        // Text with nothing to change, several lines and a URL included.
        let plain = "error: first\n  caused by: https://example.invalid/r";
        assert!(matches!(
            sanitize_error_for_terminal(plain),
            Cow::Borrowed(t) if t == plain
        ));
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
