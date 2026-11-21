//! What kind of reasoning a capture's `model_cot` holds, carried in band.
//!
//! Most harnesses expose the model's reasoning as text and do not say whether
//! the provider summarised it; that text is stored as it is and is
//! [`ReasoningKind::Exposed`]. Some records say outright that what they hold
//! is the provider's *summary* of reasoning it withheld or encrypted (Codex's
//! `reasoning.summary`, OpenAI models through OpenCode). Such text is stored
//! behind a **marker** so that every reader can label it as a summary, with no
//! change to the wire format: the bytes of `model_cot` stay the one source of
//! truth, locally and under `refs/cvc/main` (FORMAT 5).
//!
//! There is deliberately no "full reasoning" kind. Unmarked text means
//! "reasoning as the agent exposed it", never "the full reasoning": a
//! provider may well have summarised it without saying so.
//!
//! # Layout
//!
//! ```text
//! marked    := MARKER SEP body
//! MARKER    := "[cvc: provider summary of the model's reasoning " B "]"
//! SEP       := "\n\n"
//! body      := the scrubbed, bounded text; non-empty; B occurs nowhere in it
//! B         := "~cvc-" 16*16 [0-9a-f]
//! ```
//!
//! # The boundary
//!
//! ```text
//! B = "~cvc-" ‖ lowercase-hex( SHA-256(
//!         "cvc.reasoning-boundary/v1" ‖ 0x00 ‖ id ‖ 0x00 ‖ kind ‖ 0x00 ‖ c )[0..8] )
//! ```
//!
//! where `id` is the interaction id as 36 lowercase ASCII characters, `kind`
//! is the kind's [name](ReasoningKind::name) (`provider_summary`), and `c` is
//! a `u32` in big-endian (4 bytes). The writer uses the smallest `c` in
//! `0..64` whose `B` occurs nowhere in the body. `B` is never derived from
//! content, for the reason given in [`crate::stimulus`].
//!
//! # Writing: scrub, bound, then mark
//!
//! [`mark`] takes text that has already been scrubbed and bounded, so the
//! marker sits outside the text's size bound and no scrubber has to be
//! trusted not to cut it. It marks nothing that is empty (or only white
//! space), nothing that is a note standing in for text that could not be
//! scrubbed, and nothing of kind [`ReasoningKind::Exposed`]. The marker uses
//! the boundary-token shape of stimulus version 2, which the built-in
//! scrubber leaves alone (a test pins it), so it survives every later scrub
//! byte for byte.
//!
//! # Reading
//!
//! [`read`] trusts a marker only when it opens the text exactly, its `B`
//! occurs nowhere in the body, and `B` verifies against the interaction's own
//! id for some `c` in `0..64`. Anything else is [`ReasoningView::Unverified`]
//! and is shown as it is, as reasoning the agent exposed.
//!
//! Imitation can only make a reader understate: text that copies a marker and
//! does not verify is shown literally, and still reads "provider summary".
//! The harmful direction, a summary presented as the full reasoning, cannot
//! be produced by forging a marker, because no reader calls unmarked text
//! full. Like the stimulus boundary, the token is a consistency check over
//! public values, not a signature.
use crate::models::InteractionId;
use crate::stimulus::{tokens_in, BOUNDARY_LEN, BOUNDARY_PREFIX};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// What kind of reasoning text a capture holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReasoningKind {
    /// The reasoning text as the harness exposed it. Whether the provider
    /// summarised it, the record does not say. Every unmarked text is this.
    Exposed,
    /// The record itself says this is the provider's summary, and the full
    /// reasoning was withheld or encrypted.
    ProviderSummary,
}

impl ReasoningKind {
    /// The kind's name in the boundary's hash input.
    pub const fn name(self) -> &'static str {
        match self {
            ReasoningKind::Exposed => "exposed",
            ReasoningKind::ProviderSummary => "provider_summary",
        }
    }

    /// What a reader calls text of this kind.
    pub const fn label(self) -> &'static str {
        match self {
            ReasoningKind::Exposed => "Reasoning as the agent exposed it",
            ReasoningKind::ProviderSummary => "Provider summary of the model's reasoning",
        }
    }

    /// The words of this kind's marker, before the boundary. `None` for a kind
    /// that is never marked.
    const fn marker_words(self) -> Option<&'static str> {
        match self {
            ReasoningKind::Exposed => None,
            ReasoningKind::ProviderSummary => Some("provider summary of the model's reasoning"),
        }
    }
}

/// The kinds that are marked, in the order a reader tries them.
const MARKED_KINDS: [ReasoningKind; 1] = [ReasoningKind::ProviderSummary];

/// How many boundary candidates a writer tries.
pub const BOUNDARY_CANDIDATES: u32 = 64;

const BOUNDARY_DOMAIN: &[u8] = b"cvc.reasoning-boundary/v1\0";
const MARKER_OPEN: &str = "[cvc: ";
const MARKER_CLOSE: &str = "]";
/// Separates the marker from the body.
pub const SEPARATOR: &str = "\n\n";

/// Boundary candidate `candidate` of reasoning of kind `kind` in interaction
/// `id`.
pub fn boundary(id: &InteractionId, kind: ReasoningKind, candidate: u32) -> String {
    let mut hash = Sha256::new();
    hash.update(BOUNDARY_DOMAIN);
    hash.update(id.to_string().as_bytes());
    hash.update([0u8]);
    hash.update(kind.name().as_bytes());
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

fn marker(words: &str, token: &str) -> String {
    format!("{MARKER_OPEN}{words} {token}{MARKER_CLOSE}{SEPARATOR}")
}

/// Whether `text` is, in full, the note a capture stores for a text field it
/// could not scrub: `[cvc: withheld N bytes that could not be scrubbed]`.
pub fn is_withheld_note(text: &str) -> bool {
    text.strip_prefix("[cvc: withheld ")
        .and_then(|rest| rest.strip_suffix(" bytes that could not be scrubbed]"))
        .is_some_and(|count| {
            !count.is_empty()
                && count.bytes().all(|b| b.is_ascii_digit())
                && (count == "0" || !count.starts_with('0'))
        })
}

/// `text` as it is stored for reasoning of kind `kind` in interaction `id`.
///
/// `text` must already be scrubbed and bounded. It comes back unchanged when
/// `kind` is not marked ([`ReasoningKind::Exposed`]), when it is empty or only
/// white space, when it is a withheld note ([`is_withheld_note`]), or when it
/// already opens with this interaction's verified marker of that kind.
/// Otherwise it is prefixed with the marker. In the degenerate case where all
/// [`BOUNDARY_CANDIDATES`] tokens occur in the text, the first is used
/// anyway: the marker then does not verify, and readers show the text
/// literally, which still says "provider summary".
pub fn mark(id: &InteractionId, kind: ReasoningKind, bounded_scrubbed: String) -> String {
    let Some(words) = kind.marker_words() else {
        return bounded_scrubbed;
    };
    if bounded_scrubbed.trim().is_empty() || is_withheld_note(&bounded_scrubbed) {
        return bounded_scrubbed;
    }
    if matches!(read(id, &bounded_scrubbed), ReasoningView::Verified { kind: found, .. } if found == kind)
    {
        return bounded_scrubbed;
    }
    let present: std::collections::HashSet<&str> = tokens_in(&bounded_scrubbed).collect();
    let token = (0..BOUNDARY_CANDIDATES)
        .map(|candidate| boundary(id, kind, candidate))
        .find(|token| !present.contains(token.as_str()))
        .unwrap_or_else(|| boundary(id, kind, 0));
    let mut out = marker(words, &token);
    out.push_str(&bounded_scrubbed);
    out
}

/// What a reader makes of a capture's `model_cot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningView<'a> {
    /// A marker opened the text and verified: the kind it states, and the body
    /// after it.
    Verified { kind: ReasoningKind, body: &'a str },
    /// No verified marker: the text, all of it, to be shown as reasoning the
    /// agent exposed.
    Unverified(&'a str),
}

impl<'a> ReasoningView<'a> {
    /// The kind a reader shows: what a verified marker states, else
    /// [`ReasoningKind::Exposed`].
    pub fn kind(&self) -> ReasoningKind {
        match self {
            ReasoningView::Verified { kind, .. } => *kind,
            ReasoningView::Unverified(_) => ReasoningKind::Exposed,
        }
    }

    /// The text a reader shows under the kind's label: the body of a verified
    /// marker, else all of the text.
    pub fn body(&self) -> &'a str {
        match self {
            ReasoningView::Verified { body, .. } => body,
            ReasoningView::Unverified(text) => text,
        }
    }

    /// The label a reader shows ([`ReasoningKind::label`] of [`Self::kind`]).
    pub fn label(&self) -> &'static str {
        self.kind().label()
    }
}

/// Reads the `model_cot` of interaction `id` (see the module docs).
pub fn read<'a>(id: &InteractionId, model_cot: &'a str) -> ReasoningView<'a> {
    for kind in MARKED_KINDS {
        if let Some(body) = verified_body(id, kind, model_cot) {
            return ReasoningView::Verified { kind, body };
        }
    }
    ReasoningView::Unverified(model_cot)
}

fn verified_body<'a>(id: &InteractionId, kind: ReasoningKind, text: &'a str) -> Option<&'a str> {
    let words = kind.marker_words()?;
    let rest = text.strip_prefix(MARKER_OPEN)?.strip_prefix(words)?;
    let rest = rest.strip_prefix(' ')?;
    let token = rest.get(..BOUNDARY_LEN)?;
    if tokens_in(token).next() != Some(token) {
        return None;
    }
    let body = rest[BOUNDARY_LEN..]
        .strip_prefix(MARKER_CLOSE)?
        .strip_prefix(SEPARATOR)?;
    if body.trim().is_empty() || body.contains(token) {
        return None;
    }
    (0..BOUNDARY_CANDIDATES)
        .any(|candidate| boundary(id, kind, candidate) == token)
        .then_some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> InteractionId {
        "6b1f0c0e-1111-4aaa-8bbb-000000000001".parse().unwrap()
    }

    #[test]
    fn boundary_hash_input_is_laid_out_byte_for_byte() {
        let mut input = Vec::new();
        input.extend_from_slice(b"cvc.reasoning-boundary/v1");
        input.push(0);
        input.extend_from_slice(b"6b1f0c0e-1111-4aaa-8bbb-000000000001");
        input.push(0);
        input.extend_from_slice(b"provider_summary");
        input.push(0);
        input.extend_from_slice(&3u32.to_be_bytes());
        let expected = format!("~cvc-{}", hex::encode(&Sha256::digest(&input)[..8]));
        assert_eq!(boundary(&id(), ReasoningKind::ProviderSummary, 3), expected);
        assert_ne!(boundary(&id(), ReasoningKind::Exposed, 3), expected);
        assert_ne!(boundary(&id(), ReasoningKind::ProviderSummary, 4), expected);
    }

    #[test]
    fn a_marked_summary_reads_back_with_its_kind() {
        let marked = mark(
            &id(),
            ReasoningKind::ProviderSummary,
            "Looking at the loop".into(),
        );
        let token = boundary(&id(), ReasoningKind::ProviderSummary, 0);
        assert_eq!(
            marked,
            format!(
                "[cvc: provider summary of the model's reasoning {token}]\n\nLooking at the loop"
            )
        );
        let view = read(&id(), &marked);
        assert_eq!(
            view,
            ReasoningView::Verified {
                kind: ReasoningKind::ProviderSummary,
                body: "Looking at the loop"
            }
        );
        assert_eq!(view.label(), "Provider summary of the model's reasoning");
        // Marking twice changes nothing.
        assert_eq!(
            mark(&id(), ReasoningKind::ProviderSummary, marked.clone()),
            marked
        );
    }

    #[test]
    fn what_is_never_marked() {
        for text in [
            "",
            "  \n",
            "[cvc: withheld 12 bytes that could not be scrubbed]",
        ] {
            assert_eq!(
                mark(&id(), ReasoningKind::ProviderSummary, text.into()),
                text
            );
        }
        assert_eq!(
            mark(&id(), ReasoningKind::Exposed, "thinking".into()),
            "thinking"
        );
        let view = read(&id(), "thinking");
        assert_eq!(view, ReasoningView::Unverified("thinking"));
        assert_eq!(view.kind(), ReasoningKind::Exposed);
        assert_eq!(view.label(), "Reasoning as the agent exposed it");
    }

    #[test]
    fn withheld_notes_are_recognised_exactly() {
        assert!(is_withheld_note(
            "[cvc: withheld 0 bytes that could not be scrubbed]"
        ));
        assert!(is_withheld_note(
            "[cvc: withheld 7000000 bytes that could not be scrubbed]"
        ));
        for text in [
            "[cvc: withheld bytes that could not be scrubbed]",
            "[cvc: withheld 01 bytes that could not be scrubbed]",
            "[cvc: withheld 1 bytes that could not be scrubbed] and more",
            "x[cvc: withheld 1 bytes that could not be scrubbed]",
            "[cvc: withheld -1 bytes that could not be scrubbed]",
        ] {
            assert!(!is_withheld_note(text), "{text}");
        }
    }

    #[test]
    fn the_writer_avoids_tokens_already_in_the_text() {
        let first = boundary(&id(), ReasoningKind::ProviderSummary, 0);
        let second = boundary(&id(), ReasoningKind::ProviderSummary, 1);
        let text = format!("mentions {first} once");
        let marked = mark(&id(), ReasoningKind::ProviderSummary, text.clone());
        assert!(marked.starts_with(&format!(
            "[cvc: provider summary of the model's reasoning {second}]\n\n"
        )));
        assert_eq!(read(&id(), &marked).body(), text);

        // Every candidate taken: marked anyway, and the marker does not verify.
        let crowded: String = (0..BOUNDARY_CANDIDATES)
            .map(|c| boundary(&id(), ReasoningKind::ProviderSummary, c))
            .collect::<Vec<_>>()
            .join(" ");
        let marked = mark(&id(), ReasoningKind::ProviderSummary, crowded);
        assert!(marked.starts_with("[cvc: provider summary of the model's reasoning ~cvc-"));
        assert_eq!(read(&id(), &marked), ReasoningView::Unverified(&marked));
    }

    #[test]
    fn imitations_do_not_verify() {
        let other: InteractionId = "6b1f0c0e-1111-4aaa-8bbb-000000000002".parse().unwrap();
        let genuine = mark(&id(), ReasoningKind::ProviderSummary, "summary".into());
        let token = boundary(&id(), ReasoningKind::ProviderSummary, 0);
        let imitations = [
            // Another interaction's marker.
            mark(&other, ReasoningKind::ProviderSummary, "summary".into()),
            // A made-up token.
            "[cvc: provider summary of the model's reasoning ~cvc-0123456789abcdef]\n\nsummary"
                .to_owned(),
            // Not at the start.
            format!(" {genuine}"),
            format!("x\n\n{genuine}"),
            // A token outside the 64 candidates.
            format!(
                "[cvc: provider summary of the model's reasoning {}]\n\nsummary",
                boundary(&id(), ReasoningKind::ProviderSummary, BOUNDARY_CANDIDATES)
            ),
            // The token again in the body.
            format!("{genuine} {token}"),
            // Upper-case hex, wrong separator, wrong words, no body.
            genuine.replace(&token, &token.to_uppercase().replace("~CVC-", "~cvc-")),
            genuine.replace("]\n\n", "]\n"),
            genuine.replace("provider summary", "provider-summary"),
            format!("[cvc: provider summary of the model's reasoning {token}]\n\n"),
            format!("[cvc: provider summary of the model's reasoning {token}]\n\n \n"),
        ];
        for text in &imitations {
            assert_eq!(
                read(&id(), text),
                ReasoningView::Unverified(text),
                "{text:?}"
            );
        }
        assert!(matches!(
            read(&id(), &genuine),
            ReasoningView::Verified { .. }
        ));
    }

    #[test]
    fn the_marker_survives_the_built_in_scrubbers_byte_for_byte() {
        let marked = mark(
            &id(),
            ReasoningKind::ProviderSummary,
            "plain words, a path src/lib.rs and a hash 0123456789abcdef0123".into(),
        );
        assert_eq!(crate::privacy::scrub(&marked).unwrap(), marked);
        let policy = crate::privacy::PreparedPolicy::built_ins_only();
        assert_eq!(policy.scrub(&marked).unwrap(), marked);

        // A secret in the body is redacted; the marker stays and still verifies.
        let secret = ["ghp_", "abcdefghijklmnopqrstuvwxyz0123456789"].concat();
        let marked = mark(
            &id(),
            ReasoningKind::ProviderSummary,
            format!("found {secret} in env"),
        );
        let scrubbed = crate::privacy::scrub(&marked).unwrap();
        assert_ne!(scrubbed, marked);
        let view = read(&id(), &scrubbed);
        assert_eq!(view.kind(), ReasoningKind::ProviderSummary);
        assert!(!view.body().contains(&secret));
        let (marker, _) = marked.split_once("\n\n").unwrap();
        assert!(scrubbed.starts_with(marker));
    }
}
