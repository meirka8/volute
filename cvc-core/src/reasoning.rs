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
//! [`mark`] takes [`SealedText`]: text that [`crate::harness::seal`] has
//! scrubbed with the capture's policy and then bounded, and that is not a
//! note standing in for text that could not be scrubbed. Only the seal makes
//! one, after the last policy scrub, so the marker sits outside the text's
//! size bound, no scrubber has to be trusted not to cut it, and a withheld
//! field is never marked (the seal knows it withheld one from the failed
//! scrub itself, not from the note's words). [`mark`] marks nothing that is
//! empty (or only white space) and nothing of kind
//! [`ReasoningKind::Exposed`]. The marker uses the boundary-token shape of
//! stimulus version 2, which the built-in scrubber leaves alone (a test pins
//! it), so it survives every later scrub byte for byte.
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
use crate::privacy::PrivacyError;
use crate::stimulus::{tokens_in, Scrub, BOUNDARY_LEN, BOUNDARY_PREFIX};
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

/// Reasoning text the seal scrubbed in full with the capture's policy and
/// then bounded: what [`mark`] takes. Only [`crate::harness::seal`] makes
/// one, and never of a note standing in for text that could not be scrubbed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedText(String);

impl SealedText {
    /// For the seal only: `text` is scrubbed with the capture's policy,
    /// bounded, and not a withheld note.
    pub(crate) fn sealed(text: String) -> Self {
        Self(text)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn for_tests(text: impl Into<String>) -> Self {
        Self(text.into())
    }
}

/// `text` as it is stored for reasoning of kind `kind` in interaction `id`.
///
/// It comes back unchanged when `kind` is not marked
/// ([`ReasoningKind::Exposed`]), when it is empty or only white space, or
/// when it already opens with this interaction's verified marker of that
/// kind. Otherwise it is prefixed with the marker. In the degenerate case
/// where all [`BOUNDARY_CANDIDATES`] tokens occur in the text, the first is
/// used anyway: the marker then does not verify, and readers show the text
/// literally, which still says "provider summary".
pub fn mark(id: &InteractionId, kind: ReasoningKind, text: SealedText) -> String {
    mark_scrubbed(id, kind, text.0)
}

/// [`mark`] of text already scrubbed: the seal's, or a later scrub's of a
/// verified body ([`scrub_reasoning`]).
fn mark_scrubbed(id: &InteractionId, kind: ReasoningKind, scrubbed: String) -> String {
    let Some(words) = kind.marker_words() else {
        return scrubbed;
    };
    if scrubbed.trim().is_empty() {
        return scrubbed;
    }
    if matches!(read(id, &scrubbed), ReasoningView::Verified { kind: found, .. } if found == kind) {
        return scrubbed;
    }
    let present: std::collections::HashSet<&str> = tokens_in(&scrubbed).collect();
    let token = (0..BOUNDARY_CANDIDATES)
        .map(|candidate| boundary(id, kind, candidate))
        .find(|token| !present.contains(token.as_str()))
        .unwrap_or_else(|| boundary(id, kind, 0));
    let mut out = marker(words, &token);
    out.push_str(&scrubbed);
    out
}

/// `model_cot` of interaction `id` scrubbed with `scrub`, its marker kept.
///
/// Every later scrub of stored reasoning goes through here (the capture
/// boundary with the repository's policy, and the final scrub before
/// publication), for the reason `stimulus::scrub_stimulus` exists: a policy
/// rule may match the marker's words or its hex token, or reach from the
/// marker into the body, and scrubbing the text as one string would then
/// garble the marker, and a provider summary would read as exposed
/// reasoning. When a verified marker opens the text, only the body is
/// scrubbed, and the result is marked again with the same token when the
/// scrubbed body still does not contain it (so a scrub that changes nothing
/// leaves the bytes as they were), or with the first free candidate. Any
/// other text is scrubbed as one string.
pub fn scrub_reasoning(
    id: &InteractionId,
    model_cot: &str,
    scrub: Scrub<'_>,
) -> Result<String, PrivacyError> {
    let Some((kind, token, body)) = verified_marker(id, model_cot) else {
        return scrub(model_cot);
    };
    // A scrub that fails fails the whole value (the capture boundary then
    // stores a stand-in), so `scrubbed` is always text, never a note.
    let scrubbed = scrub(body)?;
    match kind.marker_words() {
        Some(words) if !scrubbed.trim().is_empty() && !scrubbed.contains(token) => {
            let mut out = marker(words, token);
            out.push_str(&scrubbed);
            Ok(out)
        }
        _ => Ok(mark_scrubbed(id, kind, scrubbed)),
    }
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
    match verified_marker(id, model_cot) {
        Some((kind, _, body)) => ReasoningView::Verified { kind, body },
        None => ReasoningView::Unverified(model_cot),
    }
}

/// The kind, token and body of the verified marker that opens `text`.
fn verified_marker<'a>(
    id: &InteractionId,
    text: &'a str,
) -> Option<(ReasoningKind, &'a str, &'a str)> {
    MARKED_KINDS
        .into_iter()
        .find_map(|kind| verified_body(id, kind, text).map(|(token, body)| (kind, token, body)))
}

fn verified_body<'a>(
    id: &InteractionId,
    kind: ReasoningKind,
    text: &'a str,
) -> Option<(&'a str, &'a str)> {
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
        .then_some((token, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`super::mark`] of `text` as if the seal had sealed it.
    fn mark(id: &InteractionId, kind: ReasoningKind, text: String) -> String {
        super::mark(id, kind, SealedText::for_tests(text))
    }

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
        for text in ["", "  \n"] {
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

    /// Sealed text that happens to read like a withheld note is text: the
    /// seal decides what it withheld from the scrub, not from the words. A
    /// later scrub keeps its marker as it keeps any other.
    #[test]
    fn sealed_text_that_reads_like_a_note_is_marked_like_any_other() {
        let lookalike = "[cvc: withheld 12 bytes that could not be scrubbed]";
        let marked = mark(&id(), ReasoningKind::ProviderSummary, lookalike.into());
        assert_eq!(
            read(&id(), &marked),
            ReasoningView::Verified {
                kind: ReasoningKind::ProviderSummary,
                body: lookalike
            }
        );
        let policy = crate::privacy::PreparedPolicy::built_ins_only();
        assert_eq!(
            scrub_reasoning(&id(), &marked, &|t| policy.scrub(t)).unwrap(),
            marked
        );
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

    /// `text` with every redaction marker's random id replaced, so that two
    /// scrubs of the same text compare equal.
    fn without_marker_ids(text: &str) -> String {
        regex::Regex::new(r"\[CVC_REDACTED:v1:([a-z_]+):[0-9a-f-]+\]")
            .unwrap()
            .replace_all(text, "[CVC_REDACTED:v1:$1:_]")
            .into_owned()
    }

    /// A repository policy built from `rules`, as `.thoughtignore` lines.
    fn policy(rules: &str) -> crate::privacy::PreparedPolicy {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join(".thoughtignore"), rules).unwrap();
        crate::privacy::PreparedPolicy::load(dir.path()).unwrap()
    }

    /// Rules that match the marker's words, its token's hex, or a span from
    /// the marker into the body leave the marker as it was: only the body is
    /// scrubbed, and a summary still reads as one.
    #[test]
    fn policy_rules_scrub_the_body_and_never_the_marker() {
        let body = "summary of cvc work: checked 0123abcd and the retry loop";
        let marked = mark(&id(), ReasoningKind::ProviderSummary, body.into());
        let (marker_line, _) = marked.split_once("\n\n").unwrap();
        for rules in [
            "literal:summary\n",
            "literal:cvc\n",
            "literal:reasoning\n",
            "regex:[0-9a-f]{8}\n",
            "regex:~cvc-[0-9a-f]+\n",
            "regex:reasoning[^\\n]*\\n\\nsummary\n",
            "regex:\\]\\s+summary\n",
        ] {
            let policy = policy(rules);
            let scrubbed = scrub_reasoning(&id(), &marked, &|t| policy.scrub(t)).unwrap();
            assert!(scrubbed.starts_with(marker_line), "{rules:?}: {scrubbed:?}");
            let view = read(&id(), &scrubbed);
            assert_eq!(view.kind(), ReasoningKind::ProviderSummary, "{rules:?}");
            // The body is scrubbed by the rule, as text anywhere else would be.
            assert_eq!(
                without_marker_ids(view.body()),
                without_marker_ids(&policy.scrub(body).unwrap()),
                "{rules:?}"
            );
            // Scrubbing again changes nothing.
            assert_eq!(
                scrub_reasoning(&id(), &scrubbed, &|t| policy.scrub(t)).unwrap(),
                scrubbed,
                "{rules:?}"
            );
        }
        // The same rules on the text as one string would have broken it.
        let policy = policy("literal:summary\n");
        let whole = policy.scrub(&marked).unwrap();
        assert_eq!(read(&id(), &whole).kind(), ReasoningKind::Exposed);
    }

    #[test]
    fn unmarked_and_imitating_text_is_scrubbed_as_one_string() {
        let policy = policy("literal:secret-word\n");
        let scrub = |t: &str| policy.scrub(t);
        let plain = "thinking about secret-word";
        assert_eq!(
            without_marker_ids(&scrub_reasoning(&id(), plain, &scrub).unwrap()),
            without_marker_ids(&policy.scrub(plain).unwrap())
        );
        let other: InteractionId = "6b1f0c0e-1111-4aaa-8bbb-000000000002".parse().unwrap();
        let imitation = mark(&other, ReasoningKind::ProviderSummary, "secret-word".into());
        assert_eq!(
            without_marker_ids(&scrub_reasoning(&id(), &imitation, &scrub).unwrap()),
            without_marker_ids(&policy.scrub(&imitation).unwrap())
        );
        // A scrub that fails fails the whole value.
        let failing = |_: &str| -> Result<String, PrivacyError> { Err(PrivacyError::TooLarge) };
        let marked = mark(&id(), ReasoningKind::ProviderSummary, "x".into());
        assert!(scrub_reasoning(&id(), &marked, &failing).is_err());
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
