//! The stimulus encoding of Claude Code captures (version 2), its renderer and
//! its reference reader.
//!
//! The `user_prompt` of a harness capture is the *stimulus* of one assistant
//! response: what the model was reacting to. It is made of ordered **parts**.
//! Version 1 joined the parts with bracketed prefixes that tool output could
//! imitate, so a reader could not tell a person's words from text a tool
//! returned. Version 2 marks every part with a **boundary token** `B` that
//! content cannot have contained when the stimulus was written, and a reader
//! trusts a marker only when `B` verifies against the capture's own identity.
//!
//! # Part kinds
//!
//! | kind                | what the part holds                                           |
//! |---------------------|---------------------------------------------------------------|
//! | `prompt`            | a person's non-meta text, including the `[image]` and         |
//! |                     | `[document]` placeholders and Claude Code's interruption lines |
//! | `task`              | in a subagent conversation, text its delegating agent sent    |
//! | `meta`              | context the harness injected (system reminders and the like)  |
//! | `compaction_summary`| the summary Claude Code writes when it compacts a session     |
//! | `tool_result`       | what a tool the model called returned                         |
//!
//! # Layout
//!
//! ```text
//! stimulus  := NO-STIMULUS | part ( SEP part )* [ SEP omitted ]
//! NO-STIMULUS := "[no stimulus recorded]"        (also what version 1 wrote)
//! SEP       := "\n\n"
//! part      := marker content
//! marker    := "[prompt " B "] "
//!            | "[task " B "] "
//!            | "[meta " B "] "
//!            | "[compaction summary " B "]\n"
//!            | "[tool_result " NAME " " B "] "
//!            | "[tool_result " NAME " error " B "] "
//! B         := "~cvc-" 16*16 [0-9a-f]
//! NAME      := 1*200 [A-Za-z0-9_.:-]
//! omitted   := "[cvc: omitted " N " parts " B "]"
//! content   := any text; it may hold notes
//! note      := "…[cvc: truncated " N " bytes " B "]"      (U+2026 first)
//!            | "[cvc: withheld " B "]"
//! N         := [1-9][0-9]*
//! ```
//!
//! A marker is one line, at offset 0 or right after a `SEP`. A part's content
//! runs from the end of its marker to the `SEP` before the next marker (or to
//! the end, before the `omitted` note). Names are the tool's own name when it
//! matches `NAME`; otherwise `unknown-tool`; a name the scrubber would change
//! is `redacted-tool`.
//!
//! A part cut by the size bounds keeps its head, then a `truncated` note on its
//! own line, then (for a tool result only) its tail: `head "\n" note "\n" tail`,
//! `head "\n" note`, `note "\n" tail` or `note` alone. `N` is exactly the
//! number of bytes of the scrubbed part that were removed. A part that could
//! not be scrubbed has the `withheld` note as its entire content.
//!
//! # The boundary
//!
//! ```text
//! B = "~cvc-" ‖ lowercase-hex( SHA-256(
//!         "cvc.stimulus-boundary/v2" ‖ 0x00 ‖ id ‖ 0x00 ‖ shape ‖ 0x00 ‖ c )[0..8] )
//! ```
//!
//! where `id` is the interaction id as 36 lowercase ASCII characters, `shape`
//! is the part kinds in order (`prompt`, `task`, `meta`, `compaction_summary`,
//! `tool_result`; a failed tool result is still `tool_result`) joined by `,`,
//! and `c` is a `u32` in big-endian (4 bytes). The writer uses the smallest
//! `c` in `0..=63` whose `B` occurs in no part content and no tool name. `B`
//! is never derived from content: a hash of content would let anyone test a
//! guess at redacted text against it.
//!
//! # Writing: scrub, bound, then mark
//!
//! [`render`] scrubs the full content of each part first (and each tool name
//! on its own), so a secret cannot straddle a cut; a part that cannot be
//! scrubbed is withheld and the rest proceed. It then bounds: tool results keep
//! at most [`TOOL_RESULT_HEAD_BYTES`] head and [`TOOL_RESULT_TAIL_BYTES`] tail,
//! and the whole stimulus fits [`MAX_STIMULUS_BYTES`] without a second cut:
//! markers and notes are reserved first, tool results then shrink evenly
//! (never below [`TOOL_RESULT_FLOOR_BYTES`]) before a person's text does, and
//! parts that still do not fit are replaced by one `omitted` note. A cut never
//! lands inside a `[CVC_REDACTED:v1:…]` marker or a multibyte character. Only
//! then is the boundary chosen, on the final bytes, and the text assembled.
//!
//! # Reading
//!
//! [`read`] is the reference reader:
//!
//! 1. The text must open with a marker of the strict grammar; its `B` is the
//!    stimulus' boundary.
//! 2. Every occurrence of that `B` must sit in a marker (at offset 0 or after a
//!    `SEP`), a `truncated` note inside a part, a `withheld` note that is a
//!    part's whole content, or the closing `omitted` note.
//! 3. `B` must equal the boundary of `(id, the parsed shape, c)` for some `c`
//!    in `0..=63`.
//!
//! Passing all three yields [`Reading::Verified`]. Failing any of them while
//! some `~cvc-` token of 16 hex digits is present yields [`Reading::Undivided`]
//! (show the text as one block, never parse it as version 1). Text with no such
//! token at all is [`Reading::Legacy`] (version 1, or text no harness wrote).
//!
//! The token is a consistency check, not a signature: it is computed from
//! public values, so it exposes tool output that imitates the structure (the
//! interaction id does not exist until after the output was written) and
//! stored text changed without recomputing it, and it does not authenticate who
//! wrote the text. Tampering with an agent's transcript before capture is not a
//! threat this encoding addresses.
use crate::models::InteractionId;
use crate::privacy::{redaction_marker_spans, PrivacyError};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::ops::Range;

/// A scrubber: the policy's, or the built-in one.
pub type Scrub<'a> = &'a dyn Fn(&str) -> Result<String, PrivacyError>;

/// What a stimulus with no parts is stored as, in every version.
pub const NO_STIMULUS: &str = "[no stimulus recorded]";
/// A stimulus never exceeds this many bytes.
pub const MAX_STIMULUS_BYTES: usize = 64 * 1024;
/// A stimulus holds at most this many parts; the rest are summarized.
pub const MAX_PARTS: usize = 512;
/// The longest tool name a marker carries.
pub const MAX_NAME_BYTES: usize = 200;
/// A tool result keeps at most this much of its head...
pub const TOOL_RESULT_HEAD_BYTES: usize = 6 * 1024;
/// ...and this much of its tail.
pub const TOOL_RESULT_TAIL_BYTES: usize = 2 * 1024;
/// Under pressure a tool result still keeps this much (or all of it, if shorter).
pub const TOOL_RESULT_FLOOR_BYTES: usize = 256;
/// How many boundary candidates a writer tries.
pub const BOUNDARY_CANDIDATES: u32 = 64;
/// The prefix of every boundary token.
pub const BOUNDARY_PREFIX: &str = "~cvc-";
/// The length of a boundary token: the prefix and 16 lowercase hex digits.
pub const BOUNDARY_LEN: usize = 21;
/// The name of a tool whose name did not match the grammar, or is unknown.
pub const UNKNOWN_TOOL: &str = "unknown-tool";
/// The name of a tool whose name the scrubber would change.
pub const REDACTED_TOOL: &str = "redacted-tool";
/// Separates parts.
pub const SEPARATOR: &str = "\n\n";

const BOUNDARY_DOMAIN: &[u8] = b"cvc.stimulus-boundary/v2\0";
const TRUNCATED_OPEN: &str = "…[cvc: truncated ";
const WITHHELD_OPEN: &str = "[cvc: withheld ";
const OMITTED_OPEN: &str = "[cvc: omitted ";
/// Stands in for the boundary while sizes are computed.
const PLACEHOLDER_TOKEN: &str = "~cvc-0000000000000000";
/// Bytes a `truncated` note may take, with the newlines around it.
const NOTE_RESERVE: usize = 64;
/// Bytes the closing `omitted` note may take, with its separator.
const OMITTED_RESERVE: usize = 64;
/// The most bytes one stimulus asks the scrubber to scan; a part that would
/// take it past that is withheld without being read. A part the scrubber rejects
/// unread (over its size limit) scans nothing and is not charged.
const MAX_SCRUBBED_BYTES: usize = 8 * 1024 * 1024;
/// Opens a stimulus none of whose parts could be marked.
const UNDIVIDED_NOTE: &str = "[cvc: stimulus parts not marked]";
const WITHHELD_NOTE_BYTES: usize = WITHHELD_OPEN.len() + BOUNDARY_LEN + 1;

/// What kind of thing a part is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PartKind {
    Prompt,
    Task,
    Meta,
    CompactionSummary,
    ToolResult,
}

impl PartKind {
    /// The kind's name in a stimulus' `shape`.
    pub const fn shape_name(self) -> &'static str {
        match self {
            PartKind::Prompt => "prompt",
            PartKind::Task => "task",
            PartKind::Meta => "meta",
            PartKind::CompactionSummary => "compaction_summary",
            PartKind::ToolResult => "tool_result",
        }
    }
}

/// One part of a stimulus as the planner found it: the text is raw, neither
/// scrubbed nor bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StimulusPart {
    pub kind: PartKind,
    /// The tool's name, for a tool result; `None` when it is not known.
    pub name: Option<String>,
    /// The harness flagged the tool result as an error.
    pub is_error: bool,
    pub text: String,
}

impl StimulusPart {
    pub fn prompt(text: impl Into<String>) -> Self {
        Self::of(PartKind::Prompt, text)
    }
    pub fn task(text: impl Into<String>) -> Self {
        Self::of(PartKind::Task, text)
    }
    pub fn meta(text: impl Into<String>) -> Self {
        Self::of(PartKind::Meta, text)
    }
    pub fn compaction_summary(text: impl Into<String>) -> Self {
        Self::of(PartKind::CompactionSummary, text)
    }
    pub fn tool_result(name: Option<&str>, is_error: bool, text: impl Into<String>) -> Self {
        Self {
            kind: PartKind::ToolResult,
            name: name.map(str::to_owned),
            is_error,
            text: text.into(),
        }
    }
    fn of(kind: PartKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            name: None,
            is_error: false,
            text: text.into(),
        }
    }
}

// ---- tokens -----------------------------------------------------------------

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-')
}

/// Whether `name` matches `[A-Za-z0-9_.:-]{1,200}`.
pub fn is_valid_name(name: &str) -> bool {
    (1..=MAX_NAME_BYTES).contains(&name.len()) && name.bytes().all(is_name_byte)
}

/// Whether a boundary token (the prefix and 16 lowercase hex digits) starts at
/// `at`.
fn token_at(bytes: &[u8], at: usize) -> bool {
    bytes.len() >= at + BOUNDARY_LEN
        && bytes[at..at + BOUNDARY_PREFIX.len()] == *BOUNDARY_PREFIX.as_bytes()
        && bytes[at + BOUNDARY_PREFIX.len()..at + BOUNDARY_LEN]
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Every boundary-shaped token in `text`, in one linear pass.
pub fn tokens_in(text: &str) -> impl Iterator<Item = &str> {
    let bytes = text.as_bytes();
    text.match_indices(BOUNDARY_PREFIX)
        .filter(move |(at, _)| token_at(bytes, *at))
        .map(move |(at, _)| &text[at..at + BOUNDARY_LEN])
}

/// Whether any boundary-shaped token occurs in `text`.
pub fn contains_token(text: &str) -> bool {
    tokens_in(text).next().is_some()
}

/// The part kinds in order, as the hash input spells them.
pub fn shape_of(kinds: impl IntoIterator<Item = PartKind>) -> String {
    kinds
        .into_iter()
        .map(PartKind::shape_name)
        .collect::<Vec<_>>()
        .join(",")
}

/// Boundary candidate `candidate` of the stimulus of interaction `id` whose
/// parts have the kinds `shape` (see [`shape_of`]).
pub fn boundary(id: &InteractionId, shape: &str, candidate: u32) -> String {
    let mut hash = Sha256::new();
    hash.update(BOUNDARY_DOMAIN);
    hash.update(id.to_string().as_bytes());
    hash.update([0u8]);
    hash.update(shape.as_bytes());
    hash.update([0u8]);
    hash.update(candidate.to_be_bytes());
    let digest = hash.finalize();
    let mut token = String::with_capacity(BOUNDARY_LEN);
    token.push_str(BOUNDARY_PREFIX);
    for byte in &digest[..8] {
        let _ = write!(token, "{byte:02x}");
    }
    token
}

// ---- markers and notes ------------------------------------------------------

struct Marker<'a> {
    kind: PartKind,
    name: Option<&'a str>,
    is_error: bool,
    token: &'a str,
    start: usize,
    token_at: usize,
    end: usize,
}

fn render_marker(kind: PartKind, name: Option<&str>, is_error: bool, token: &str) -> String {
    match kind {
        PartKind::Prompt => format!("[prompt {token}] "),
        PartKind::Task => format!("[task {token}] "),
        PartKind::Meta => format!("[meta {token}] "),
        PartKind::CompactionSummary => format!("[compaction summary {token}]\n"),
        PartKind::ToolResult => format!(
            "[tool_result {} {}{token}] ",
            name.unwrap_or(UNKNOWN_TOOL),
            if is_error { "error " } else { "" }
        ),
    }
}

/// The marker that starts at byte `at` of `text`, if the strict grammar holds.
fn marker_at(text: &str, at: usize) -> Option<Marker<'_>> {
    let rest = text.get(at..)?;
    let (kind, tail) = if let Some(tail) = rest.strip_prefix("[prompt ") {
        (PartKind::Prompt, tail)
    } else if let Some(tail) = rest.strip_prefix("[task ") {
        (PartKind::Task, tail)
    } else if let Some(tail) = rest.strip_prefix("[meta ") {
        (PartKind::Meta, tail)
    } else if let Some(tail) = rest.strip_prefix("[compaction summary ") {
        (PartKind::CompactionSummary, tail)
    } else {
        (PartKind::ToolResult, rest.strip_prefix("[tool_result ")?)
    };
    let (name, is_error, tail) = if kind == PartKind::ToolResult {
        let length = tail.bytes().take_while(|byte| is_name_byte(*byte)).count();
        if length == 0 || length > MAX_NAME_BYTES {
            return None;
        }
        let (name, after) = tail.split_at(length);
        let after = after.strip_prefix(' ')?;
        match after.strip_prefix("error ") {
            Some(flagged) if flagged.starts_with(BOUNDARY_PREFIX) => (Some(name), true, flagged),
            _ => (Some(name), false, after),
        }
    } else {
        (None, false, tail)
    };
    if !token_at(tail.as_bytes(), 0) {
        return None;
    }
    let (token, after) = tail.split_at(BOUNDARY_LEN);
    let terminator = if kind == PartKind::CompactionSummary {
        "]\n"
    } else {
        "] "
    };
    after.strip_prefix(terminator)?;
    let token_at = at + (rest.len() - tail.len());
    Some(Marker {
        kind,
        name,
        is_error,
        token,
        start: at,
        token_at,
        end: token_at + BOUNDARY_LEN + terminator.len(),
    })
}

/// What a note says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    /// This many bytes of the scrubbed part were removed.
    Truncated(u64),
    /// The part could not be scrubbed and is not recorded.
    Withheld,
    /// This many parts after the last one shown are not recorded.
    Omitted(u64),
}

/// A note and where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub kind: NoteKind,
    pub range: Range<usize>,
}

/// The maximal run of ASCII digits ending `text`, as a count with no leading
/// zero, and how many digits it has.
fn trailing_count(text: &str) -> Option<(u64, usize)> {
    let digits = text.bytes().rev().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 18 {
        return None;
    }
    let run = &text[text.len() - digits..];
    if run.starts_with('0') {
        return None;
    }
    Some((run.parse().ok()?, digits))
}

/// The note whose boundary token starts at `at` of `text`, if the text around
/// the token spells one.
pub fn note_at(text: &str, at: usize) -> Option<Note> {
    let before = text.get(..at)?;
    let after = text.get(at + BOUNDARY_LEN..)?;
    if !after.starts_with(']') {
        return None;
    }
    let end = at + BOUNDARY_LEN + 1;
    if let Some(head) = before.strip_suffix(" bytes ") {
        if let Some((count, digits)) = trailing_count(head) {
            let head = &head[..head.len() - digits];
            if head.ends_with(TRUNCATED_OPEN) {
                return Some(Note {
                    kind: NoteKind::Truncated(count),
                    range: head.len() - TRUNCATED_OPEN.len()..end,
                });
            }
        }
    }
    if before.ends_with(WITHHELD_OPEN) {
        return Some(Note {
            kind: NoteKind::Withheld,
            range: before.len() - WITHHELD_OPEN.len()..end,
        });
    }
    if let Some(head) = before.strip_suffix(" parts ") {
        if let Some((count, digits)) = trailing_count(head) {
            let head = &head[..head.len() - digits];
            if head.ends_with(OMITTED_OPEN) {
                return Some(Note {
                    kind: NoteKind::Omitted(count),
                    range: head.len() - OMITTED_OPEN.len()..end,
                });
            }
        }
    }
    None
}

/// The notes in `text` that carry `boundary`, in order.
pub fn notes_in(text: &str, boundary: &str) -> Vec<Note> {
    text.match_indices(boundary)
        .filter_map(|(at, _)| note_at(text, at))
        .collect()
}

// ---- reading ----------------------------------------------------------------

/// A stimulus read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading<'a> {
    /// The boundary verified: the parts, in order.
    Verified(Verified<'a>),
    /// The text carries version 2 tokens that did not verify. Show it as one
    /// block; never parse it as version 1.
    Undivided,
    /// No version 2 token at all: version 1, or text no harness wrote.
    Legacy,
}

/// A stimulus whose boundary verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified<'a> {
    /// The boundary token, as it appears in the text.
    pub boundary: &'a str,
    pub parts: Vec<ReadPart<'a>>,
    /// How many parts after the last one shown were not recorded.
    pub omitted_parts: Option<u64>,
}

/// One part of a verified stimulus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPart<'a> {
    pub kind: PartKind,
    /// The tool's name, for a tool result.
    pub name: Option<&'a str>,
    pub is_error: bool,
    /// Where its marker is in the text, terminator included.
    pub marker: Range<usize>,
    /// Where its content is in the text, notes inline.
    pub content: Range<usize>,
}

impl ReadPart<'_> {
    /// The part's content in `text`, the text that was read.
    pub fn content_of<'t>(&self, text: &'t str) -> &'t str {
        &text[self.content.clone()]
    }
}

/// Reads the stimulus `text` of interaction `id`. See the module docs for the
/// rules.
pub fn read<'a>(id: &InteractionId, text: &'a str) -> Reading<'a> {
    if text == NO_STIMULUS {
        return Reading::Legacy;
    }
    let Some(first) = marker_at(text, 0) else {
        return if contains_token(text) {
            Reading::Undivided
        } else {
            Reading::Legacy
        };
    };
    match verify(id, text, first) {
        Some(verified) => Reading::Verified(verified),
        None => Reading::Undivided,
    }
}

fn verify<'a>(id: &InteractionId, text: &'a str, first: Marker<'a>) -> Option<Verified<'a>> {
    let boundary = first.token;
    let bytes = text.as_bytes();
    let mut markers = vec![first];
    for (at, _) in text.match_indices('[') {
        if at < 2 || &bytes[at - 2..at] != SEPARATOR.as_bytes() {
            continue;
        }
        // A separator cannot overlap the marker before it.
        if at - 2 < markers.last().map_or(0, |marker| marker.end) {
            continue;
        }
        if let Some(marker) = marker_at(text, at) {
            if marker.token == boundary {
                markers.push(marker);
            }
        }
    }

    // Every occurrence of the boundary is a marker's or a note's.
    let marker_tokens: HashSet<usize> = markers.iter().map(|marker| marker.token_at).collect();
    let mut notes = Vec::new();
    for (at, _) in text.match_indices(boundary) {
        if !marker_tokens.contains(&at) {
            notes.push(note_at(text, at)?);
        }
    }

    // The closing `omitted` note, if any, ends the text after a separator.
    let last_marker_end = markers.last().map_or(0, |marker| marker.end);
    let mut end_of_parts = text.len();
    let mut omitted_parts = None;
    let mut closing_notes = notes
        .iter()
        .filter(|note| matches!(note.kind, NoteKind::Omitted(_)));
    if let Some(note) = closing_notes.next() {
        let NoteKind::Omitted(count) = note.kind else {
            return None;
        };
        let separated = note.range.start >= 2
            && &bytes[note.range.start - 2..note.range.start] == SEPARATOR.as_bytes()
            && note.range.start - 2 >= last_marker_end;
        if closing_notes.next().is_some() || note.range.end != text.len() || !separated {
            return None;
        }
        end_of_parts = note.range.start - 2;
        omitted_parts = Some(count);
    }

    let mut parts = Vec::with_capacity(markers.len());
    for (index, marker) in markers.iter().enumerate() {
        let content_end = markers
            .get(index + 1)
            .map_or(end_of_parts, |next| next.start - SEPARATOR.len());
        parts.push(ReadPart {
            kind: marker.kind,
            name: marker.name,
            is_error: marker.is_error,
            marker: marker.start..marker.end,
            content: marker.end..content_end,
        });
    }

    // A `withheld` note is a part's whole content, a `truncated` note lies
    // inside one. The parts are in order, so the part holding a note is the
    // last one that starts before it: a lookup, not a scan, because the text
    // may come from anywhere.
    for note in &notes {
        let holder = parts.partition_point(|part| part.content.start <= note.range.start);
        let inside = holder
            .checked_sub(1)
            .map(|index| &parts[index])
            .filter(|part| note.range.end <= part.content.end);
        match (note.kind, inside) {
            (NoteKind::Omitted(_), _) => {}
            (NoteKind::Withheld, Some(part)) if part.content == note.range => {}
            (NoteKind::Truncated(_), Some(_)) => {}
            _ => return None,
        }
    }

    let shape = shape_of(parts.iter().map(|part| part.kind));
    if !(0..BOUNDARY_CANDIDATES).any(|candidate| boundary_matches(id, &shape, candidate, boundary))
    {
        return None;
    }
    Some(Verified {
        boundary,
        parts,
        omitted_parts,
    })
}

fn boundary_matches(id: &InteractionId, shape: &str, candidate: u32, token: &str) -> bool {
    boundary(id, shape, candidate) == token
}

// ---- scrubbing a stimulus ---------------------------------------------------

/// Scrubs the stimulus `text` of interaction `id`.
///
/// A stimulus that verifies is scrubbed part by part, and within a part around
/// its notes: markers, separators and notes are copied as they are, so no
/// detector can reach across a part boundary or damage the structure, and the
/// text comes back byte for byte when nothing in it needed redacting. A part
/// whose content cannot be scrubbed is replaced by a `withheld` note; the
/// others are kept. Any other text, including a stimulus that does not
/// verify, is scrubbed whole, and an error there is an error here.
pub fn scrub_stimulus(
    id: &InteractionId,
    text: &str,
    scrub: Scrub<'_>,
) -> Result<String, PrivacyError> {
    match read(id, text) {
        Reading::Verified(verified) => Ok(scrub_verified(text, &verified, scrub)),
        Reading::Undivided | Reading::Legacy => scrub(text),
    }
}

fn scrub_verified(text: &str, verified: &Verified<'_>, scrub: Scrub<'_>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for part in &verified.parts {
        out.push_str(&text[copied..part.marker.start]);
        out.push_str(&scrub_marker(text, part, verified.boundary, scrub));
        out.push_str(&scrub_content(
            part.content_of(text),
            verified.boundary,
            scrub,
        ));
        copied = part.content.end;
    }
    out.push_str(&text[copied..]);
    out
}

/// A part's marker, with its tool name scrubbed on its own: the name a marker
/// carries is text too, and one the scrubber would change becomes `redacted-tool`
/// as it does when a stimulus is written. Names are not part of what the boundary
/// is computed from, so the stimulus still verifies.
fn scrub_marker(text: &str, part: &ReadPart<'_>, boundary: &str, scrub: Scrub<'_>) -> String {
    let marker = &text[part.marker.clone()];
    let Some(name) = part.name else {
        return marker.to_owned();
    };
    let safe = safe_name(Some(name), scrub);
    if safe == name {
        marker.to_owned()
    } else {
        render_marker(part.kind, Some(&safe), part.is_error, boundary)
    }
}

/// Scrubs one part's content around its notes.
fn scrub_content(content: &str, boundary: &str, scrub: Scrub<'_>) -> String {
    let mut out = String::with_capacity(content.len());
    let mut at = 0;
    for note in notes_in(content, boundary) {
        let Some(scrubbed) = scrub_segment(&content[at..note.range.start], scrub) else {
            return withheld_note(boundary);
        };
        out.push_str(&scrubbed);
        out.push_str(&content[note.range.clone()]);
        at = note.range.end;
    }
    match scrub_segment(&content[at..], scrub) {
        Some(scrubbed) => out.push_str(&scrubbed),
        None => return withheld_note(boundary),
    }
    out
}

fn scrub_segment(segment: &str, scrub: Scrub<'_>) -> Option<String> {
    if segment.is_empty() {
        return Some(String::new());
    }
    scrub(segment).ok()
}

// ---- rendering --------------------------------------------------------------

/// A rendered stimulus and what the writer decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    /// The `c` of the boundary; `None` when there are no parts or no boundary
    /// was free.
    pub candidate: Option<u32>,
    /// How many parts are in the text.
    pub kept_parts: usize,
    /// How many parts were left out.
    pub omitted_parts: usize,
}

/// Renders `parts`, the stimulus of interaction `id`: scrub each part, bound
/// the whole, choose the boundary, mark. See the module docs.
pub fn render(id: &InteractionId, parts: &[StimulusPart], scrub: Scrub<'_>) -> String {
    render_detailed(id, parts, scrub).text
}

/// A stimulus whose one part is withheld, for a capture that could not be
/// scrubbed: it verifies like any other, and a reader shows it as a withheld
/// part.
pub(crate) fn withheld_stimulus(id: &InteractionId) -> String {
    render(id, &[StimulusPart::meta("")], &|_| {
        Err(PrivacyError::TooLarge)
    })
}

/// [`render`], with what the writer decided.
pub fn render_detailed(id: &InteractionId, parts: &[StimulusPart], scrub: Scrub<'_>) -> Rendered {
    if parts.is_empty() {
        return Rendered {
            text: NO_STIMULUS.to_owned(),
            candidate: None,
            kept_parts: 0,
            omitted_parts: 0,
        };
    }
    let total = parts.len();
    let mut unscrubbed = MAX_SCRUBBED_BYTES;
    // A person's words (and a subagent's task) are scrubbed first, so that tool
    // output, which can be as large as it likes, never uses up what they need.
    let shown = total.min(MAX_PARTS);
    let is_person = |index: &usize| matches!(parts[*index].kind, PartKind::Prompt | PartKind::Task);
    let mut prepared: Vec<Option<Prepared>> = (0..shown).map(|_| None).collect();
    for index in (0..shown)
        .filter(is_person)
        .chain((0..shown).filter(|index| !is_person(index)))
    {
        prepared[index] = Some(prepare(&parts[index], scrub, &mut unscrubbed));
    }
    let prepared: Vec<Prepared> = prepared.into_iter().flatten().collect();
    let slots: Vec<Slot> = prepared.iter().map(slot).collect();
    let (kept, allowances) = allocate(&slots, total, MAX_STIMULUS_BYTES);
    let shown = &prepared[..kept];
    let contents: Vec<Content<'_>> = shown
        .iter()
        .zip(&allowances)
        .map(|(item, allowance)| match &item.body {
            None => Content::Withheld,
            Some(body) => fit(body, *allowance, item.kind == PartKind::ToolResult),
        })
        .collect();
    let omitted = total - kept;

    // The boundary is chosen on the final bytes of every part's content and
    // every name, in one pass.
    let mut present: HashSet<&str> = HashSet::new();
    for (item, content) in shown.iter().zip(&contents) {
        if let Some(name) = &item.name {
            present.extend(tokens_in(name));
        }
        match content {
            Content::Whole(text) => present.extend(tokens_in(text)),
            Content::Cut { head, tail, .. } => {
                present.extend(tokens_in(head));
                present.extend(tokens_in(tail));
            }
            Content::Withheld => {}
        }
    }
    let shape = shape_of(shown.iter().map(|item| item.kind));
    let chosen = (0..BOUNDARY_CANDIDATES).find_map(|candidate| {
        let token = boundary(id, &shape, candidate);
        (!present.contains(token.as_str())).then_some((candidate, token))
    });
    match chosen {
        Some((candidate, token)) => Rendered {
            text: assemble(shown, &contents, &token, omitted),
            candidate: Some(candidate),
            kept_parts: kept,
            omitted_parts: omitted,
        },
        // No candidate is free (content holds all 64): mark nothing. Readers
        // see tokens without a verifying marker and show the text undivided.
        None => Rendered {
            text: assemble_undivided(&contents, omitted),
            candidate: None,
            kept_parts: kept,
            omitted_parts: omitted,
        },
    }
}

struct Prepared {
    kind: PartKind,
    name: Option<String>,
    is_error: bool,
    /// The scrubbed content; `None` when it was withheld.
    body: Option<String>,
}

/// Scrubs one part, and its tool name on its own. A part that cannot be
/// scrubbed is withheld; nothing here can fail the stimulus. `unscrubbed` is
/// what is left of the budget of bytes to scan, and only bytes that were scanned
/// are taken from it.
fn prepare(part: &StimulusPart, scrub: Scrub<'_>, unscrubbed: &mut usize) -> Prepared {
    let is_tool = part.kind == PartKind::ToolResult;
    let name = is_tool.then(|| safe_name(part.name.as_deref(), scrub));
    let body = if part.text.len() > *unscrubbed {
        // Not read: it would take the stimulus past its budget.
        None
    } else {
        match scrub(&part.text) {
            Ok(scrubbed) => {
                *unscrubbed -= part.text.len();
                Some(scrubbed)
            }
            // Rejected unread, for its size alone: nothing was scanned.
            Err(PrivacyError::TooLarge) => None,
            // Scanned, then refused: the work was done.
            Err(_) => {
                *unscrubbed -= part.text.len();
                None
            }
        }
    };
    Prepared {
        kind: part.kind,
        name,
        is_error: is_tool && part.is_error,
        body,
    }
}

/// A tool name a marker can carry: the name itself when it matches the grammar
/// and the scrubber leaves it alone.
fn safe_name(name: Option<&str>, scrub: Scrub<'_>) -> String {
    let Some(name) = name.filter(|name| is_valid_name(name)) else {
        return UNKNOWN_TOOL.to_owned();
    };
    match scrub(name) {
        Ok(scrubbed) if scrubbed == name => name.to_owned(),
        _ => REDACTED_TOOL.to_owned(),
    }
}

fn truncated_note(removed: usize, token: &str) -> String {
    format!("{TRUNCATED_OPEN}{removed} bytes {token}]")
}

fn withheld_note(token: &str) -> String {
    format!("{WITHHELD_OPEN}{token}]")
}

fn omitted_note(count: usize, token: &str) -> String {
    format!("{OMITTED_OPEN}{count} parts {token}]")
}

/// What a part's content becomes once bounded.
enum Content<'a> {
    Whole(&'a str),
    Cut {
        head: &'a str,
        tail: &'a str,
        removed: usize,
    },
    Withheld,
}

fn assemble(shown: &[Prepared], contents: &[Content<'_>], token: &str, omitted: usize) -> String {
    let mut out = String::new();
    for (index, (item, content)) in shown.iter().zip(contents).enumerate() {
        if index > 0 {
            out.push_str(SEPARATOR);
        }
        out.push_str(&render_marker(
            item.kind,
            item.name.as_deref(),
            item.is_error,
            token,
        ));
        match content {
            Content::Whole(text) => out.push_str(text),
            Content::Cut {
                head,
                tail,
                removed,
            } => {
                if !head.is_empty() {
                    out.push_str(head);
                    out.push('\n');
                }
                out.push_str(&truncated_note(*removed, token));
                if !tail.is_empty() {
                    out.push('\n');
                    out.push_str(tail);
                }
            }
            Content::Withheld => out.push_str(&withheld_note(token)),
        }
    }
    if omitted > 0 {
        out.push_str(SEPARATOR);
        out.push_str(&omitted_note(omitted, token));
    }
    out
}

/// The degenerate layout when no boundary is free: the contents, unmarked,
/// with plain notes (a note cannot carry a boundary there is none of).
fn assemble_undivided(contents: &[Content<'_>], omitted: usize) -> String {
    let mut out = String::from(UNDIVIDED_NOTE);
    for content in contents {
        out.push_str(SEPARATOR);
        match content {
            Content::Whole(text) => out.push_str(text),
            Content::Cut {
                head,
                tail,
                removed,
            } => {
                if !head.is_empty() {
                    out.push_str(head);
                    out.push('\n');
                }
                let _ = write!(out, "…[cvc: truncated {removed} bytes]");
                if !tail.is_empty() {
                    out.push('\n');
                    out.push_str(tail);
                }
            }
            Content::Withheld => out.push_str("[cvc: withheld]"),
        }
    }
    if omitted > 0 {
        let _ = write!(out, "{SEPARATOR}[cvc: omitted {omitted} parts]");
    }
    out
}

// ---- the size budget --------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Cannot be cut: a withheld note.
    Fixed,
    /// A tool result: shrinks before anything else does.
    Tool,
    /// A person's text, a task, meta text or a summary.
    Other,
}

struct Slot {
    /// The marker's bytes, boundary included.
    marker_len: usize,
    /// The content bytes the part would like to keep.
    want: usize,
    class: Class,
}

fn slot(item: &Prepared) -> Slot {
    let marker_len = render_marker(
        item.kind,
        item.name.as_deref(),
        item.is_error,
        PLACEHOLDER_TOKEN,
    )
    .len();
    match &item.body {
        None => Slot {
            marker_len,
            want: WITHHELD_NOTE_BYTES,
            class: Class::Fixed,
        },
        Some(body) if item.kind == PartKind::ToolResult => Slot {
            marker_len,
            want: body
                .len()
                .min(TOOL_RESULT_HEAD_BYTES + TOOL_RESULT_TAIL_BYTES),
            class: Class::Tool,
        },
        Some(body) => Slot {
            marker_len,
            want: body.len(),
            class: Class::Other,
        },
    }
}

/// The largest level in `low..=high` at which `total(level) <= capacity`,
/// `total` being non-decreasing and `total(low)` known to fit.
fn largest_level(
    mut low: usize,
    mut high: usize,
    capacity: usize,
    total: impl Fn(usize) -> usize,
) -> usize {
    if high <= low {
        return low;
    }
    if total(high) <= capacity {
        return high;
    }
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if total(middle) <= capacity {
            low = middle;
        } else {
            high = middle;
        }
    }
    low
}

/// How many leading parts are shown and how many content bytes each may keep,
/// so that markers, separators, a note per part and the closing note come to
/// at most `budget`.
///
/// Tool results shrink first, evenly, down to their floor; only then do the
/// other parts shrink, evenly; parts that still do not fit at their floors are
/// left out, from the end.
fn allocate(slots: &[Slot], total_parts: usize, budget: usize) -> (usize, Vec<usize>) {
    let separator = |index: usize| if index == 0 { 0 } else { SEPARATOR.len() };
    let overhead = |index: usize| slots[index].marker_len + separator(index) + NOTE_RESERVE;
    let floor = |slot: &Slot| match slot.class {
        Class::Fixed => slot.want,
        _ => slot.want.min(TOOL_RESULT_FLOOR_BYTES),
    };

    // How many leading parts fit even at their floors.
    let mut minimum = vec![0usize; slots.len() + 1];
    for (index, slot) in slots.iter().enumerate() {
        minimum[index + 1] = minimum[index] + overhead(index) + floor(slot);
    }
    let mut kept = slots.len();
    while kept > 1 {
        let closing = if kept < total_parts {
            OMITTED_RESERVE
        } else {
            0
        };
        if minimum[kept] + closing <= budget {
            break;
        }
        kept -= 1;
    }
    let slots = &slots[..kept];
    let closing = if kept < total_parts {
        OMITTED_RESERVE
    } else {
        0
    };
    let reserved: usize = (0..kept).map(overhead).sum::<usize>() + closing;
    let available = budget.saturating_sub(reserved);

    if slots.iter().map(|slot| slot.want).sum::<usize>() <= available {
        return (kept, slots.iter().map(|slot| slot.want).collect());
    }
    let sum_of = |class: Class| -> usize {
        slots
            .iter()
            .filter(|slot| slot.class == class)
            .map(|slot| slot.want)
            .sum()
    };
    let (fixed, other) = (sum_of(Class::Fixed), sum_of(Class::Other));
    let tools: Vec<usize> = slots
        .iter()
        .filter(|slot| slot.class == Class::Tool)
        .map(|slot| slot.want)
        .collect();
    let tools_at = |level: usize| -> usize { tools.iter().map(|want| (*want).min(level)).sum() };
    let ceiling = TOOL_RESULT_HEAD_BYTES + TOOL_RESULT_TAIL_BYTES;

    let allowances = if fixed + other + tools_at(TOOL_RESULT_FLOOR_BYTES) <= available {
        // Only tool results give way.
        let level = largest_level(TOOL_RESULT_FLOOR_BYTES, ceiling, available, |level| {
            fixed + other + tools_at(level)
        });
        slots
            .iter()
            .map(|slot| match slot.class {
                Class::Tool => slot.want.min(level),
                _ => slot.want,
            })
            .collect()
    } else {
        // Tool results sit at their floor and the rest gives way.
        let room = available.saturating_sub(fixed + tools_at(TOOL_RESULT_FLOOR_BYTES));
        let others: Vec<usize> = slots
            .iter()
            .filter(|slot| slot.class == Class::Other)
            .map(|slot| slot.want)
            .collect();
        let widest = others.iter().copied().max().unwrap_or(0);
        let level = largest_level(TOOL_RESULT_FLOOR_BYTES, widest, room, |level| {
            others.iter().map(|want| (*want).min(level)).sum()
        });
        slots
            .iter()
            .map(|slot| match slot.class {
                Class::Tool => slot.want.min(TOOL_RESULT_FLOOR_BYTES),
                Class::Other => slot.want.min(level),
                Class::Fixed => slot.want,
            })
            .collect()
    };
    (kept, allowances)
}

/// The largest index at most `limit` that is a character boundary of `text`
/// and not inside a redaction marker.
fn head_end(text: &str, markers: &[Range<usize>], limit: usize) -> usize {
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    match markers
        .iter()
        .find(|span| span.start < end && end < span.end)
    {
        Some(span) => span.start,
        None => end,
    }
}

/// Where to cut `text` so that at most `limit` bytes remain before the cut: a
/// character boundary that is not inside a redaction marker. For bounding a
/// field that has already been scrubbed.
pub(crate) fn head_len(text: &str, limit: usize) -> usize {
    head_end(text, &redaction_marker_spans(text), limit)
}

/// The smallest index at least `from` that is a character boundary of `text`
/// and not inside a redaction marker.
fn tail_start(text: &str, markers: &[Range<usize>], from: usize) -> usize {
    let mut start = from.min(text.len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    match markers
        .iter()
        .find(|span| span.start < start && start < span.end)
    {
        Some(span) => span.end,
        None => start,
    }
}

/// Bounds `body` to `allowance` bytes: a tool result keeps a head and a tail,
/// anything else a head. The removed count is what was actually removed, after
/// the cuts moved off markers and characters.
fn fit(body: &str, allowance: usize, is_tool: bool) -> Content<'_> {
    if body.len() <= allowance {
        return Content::Whole(body);
    }
    let markers = redaction_marker_spans(body);
    let (head_budget, tail_budget) = if is_tool {
        let head = allowance * 3 / 4;
        (head, allowance - head)
    } else {
        (allowance, 0)
    };
    let head = head_end(body, &markers, head_budget);
    let tail = if tail_budget == 0 {
        body.len()
    } else {
        tail_start(body, &markers, body.len() - tail_budget)
    };
    Content::Cut {
        head: &body[..head],
        tail: &body[tail..],
        removed: tail - head,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> InteractionId {
        "3c2b1a09-8f7e-8d6c-9b5a-4f3e2d1c0b9a".parse().unwrap()
    }

    fn keep(text: &str) -> Result<String, PrivacyError> {
        Ok(text.to_owned())
    }

    #[test]
    fn boundary_hash_input_is_laid_out_byte_for_byte() {
        // Rebuilt by hand, independently of `boundary`.
        let mut input = Vec::new();
        input.extend_from_slice(b"cvc.stimulus-boundary/v2");
        input.push(0);
        input.extend_from_slice(b"3c2b1a09-8f7e-8d6c-9b5a-4f3e2d1c0b9a");
        input.push(0);
        input.extend_from_slice(b"prompt,tool_result");
        input.push(0);
        input.extend_from_slice(&5u32.to_be_bytes());
        let digest = Sha256::digest(&input);
        let expected = format!(
            "~cvc-{}",
            digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        assert_eq!(boundary(&id(), "prompt,tool_result", 5), expected);
        assert_eq!(expected.len(), BOUNDARY_LEN);
        assert!(token_at(expected.as_bytes(), 0));
        // Domain, id, shape and candidate all matter.
        assert_ne!(boundary(&id(), "prompt,tool_result", 6), expected);
        assert_ne!(boundary(&id(), "prompt", 5), expected);
        let other: InteractionId = "3c2b1a09-8f7e-8d6c-9b5a-4f3e2d1c0b9b".parse().unwrap();
        assert_ne!(boundary(&other, "prompt,tool_result", 5), expected);
    }

    #[test]
    fn tokens_are_found_in_one_pass_and_only_when_well_formed() {
        let text = "a ~cvc-0123456789abcdef b ~cvc-0123456789ABCDEF ~cvc-short ~cvc-0123456789abcdef0 ~cvc-~cvc-fedcba9876543210";
        let found: Vec<&str> = tokens_in(text).collect();
        assert_eq!(
            found,
            vec![
                "~cvc-0123456789abcdef",
                "~cvc-0123456789abcdef",
                "~cvc-fedcba9876543210"
            ]
        );
        assert!(!contains_token("~cvc-0123456789abcde"));
    }

    #[test]
    fn names_follow_the_grammar() {
        assert!(is_valid_name("Bash"));
        assert!(is_valid_name("mcp__srv__do-it.v2:beta"));
        assert!(is_valid_name(&"a".repeat(200)));
        assert!(!is_valid_name(&"a".repeat(201)));
        assert!(!is_valid_name(""));
        for bad in ["a b", "a]", "a\n", "é", "a/b", "[x", "~cvc-"] {
            assert!(!is_valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn cuts_never_land_inside_markers_or_characters() {
        let marker = "[CVC_REDACTED:v1:pem:11111111-2222-3333-4444-555555555555]";
        let text = format!("ab{marker}é{marker}cd");
        let spans = redaction_marker_spans(&text);
        assert_eq!(spans.len(), 2);
        // Inside the first marker: back to its start.
        assert_eq!(head_end(&text, &spans, 10), 2);
        // Inside the multibyte character: back to before it.
        let multibyte = 2 + marker.len() + 1;
        assert_eq!(head_end(&text, &spans, multibyte), 2 + marker.len());
        // A tail starting inside a marker moves to its end.
        assert_eq!(tail_start(&text, &spans, 10), 2 + marker.len());
        assert_eq!(
            tail_start(&text, &spans, 2 + marker.len() + "é".len() + 5),
            2 + marker.len() + "é".len() + marker.len()
        );
        // Outside any marker the limit is honoured.
        assert_eq!(head_end(&text, &spans, 1), 1);
        assert_eq!(tail_start(&text, &spans, text.len()), text.len());
    }

    #[test]
    fn water_level_is_the_fairest_that_fits() {
        let wants = [8192usize, 8192, 100, 5000];
        let total = |level: usize| -> usize { wants.iter().map(|want| (*want).min(level)).sum() };
        for capacity in [1_000, 5_000, 12_000, 21_484, 30_000] {
            let level = largest_level(256, 8192, capacity.max(total(256)), total);
            assert!(total(level) <= capacity.max(total(256)));
            if level < 8192 {
                assert!(total(level + 1) > capacity.max(total(256)), "{capacity}");
            }
        }
    }

    #[test]
    fn allocation_respects_the_budget_floors_and_priorities() {
        let slot = |want, class| Slot {
            marker_len: 40,
            want,
            class,
        };
        // Two big tool results and a person's text that fits: only tools shrink.
        let slots = [
            slot(10_000, Class::Other),
            slot(8192, Class::Tool),
            slot(8192, Class::Tool),
        ];
        let (kept, allowances) = allocate(&slots, 3, 20_000);
        assert_eq!(kept, 3);
        assert_eq!(allowances[0], 10_000);
        assert_eq!(allowances[1], allowances[2]);
        assert!(allowances[1] < 8192 && allowances[1] >= TOOL_RESULT_FLOOR_BYTES);
        let used: usize = allowances.iter().sum::<usize>()
            + slots
                .iter()
                .map(|s| s.marker_len + NOTE_RESERVE)
                .sum::<usize>()
            + 2 * SEPARATOR.len();
        assert!(used <= 20_000, "{used}");
        // A text too big even with tool results at their floor shrinks too.
        let slots = [slot(60_000, Class::Other), slot(8192, Class::Tool)];
        let (_, allowances) = allocate(&slots, 2, 30_000);
        assert_eq!(allowances[1], TOOL_RESULT_FLOOR_BYTES);
        assert!(allowances[0] < 60_000);
        // Parts that cannot all keep their floors are left out, from the end.
        let many: Vec<Slot> = (0..100).map(|_| slot(500, Class::Tool)).collect();
        let (kept, allowances) = allocate(&many, 100, 10_000);
        assert!((1..100).contains(&kept));
        assert_eq!(allowances.len(), kept);
        let used: usize = allowances.iter().sum::<usize>()
            + kept * (40 + NOTE_RESERVE)
            + (kept - 1) * SEPARATOR.len()
            + OMITTED_RESERVE;
        assert!(used <= 10_000, "{used}");
    }

    #[test]
    fn rendering_without_parts_is_the_no_stimulus_line() {
        let rendered = render_detailed(&id(), &[], &keep);
        assert_eq!(rendered.text, NO_STIMULUS);
        assert_eq!(rendered.candidate, None);
        assert_eq!(read(&id(), &rendered.text), Reading::Legacy);
    }
}
