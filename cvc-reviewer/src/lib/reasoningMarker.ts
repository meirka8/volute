/**
 * The reader of the reasoning marker (#187): a port of `cvc-core/src/reasoning.rs` (`read`), which is the
 * specification. The shared vectors in `cvc-core/tests/fixtures/reasoning-marker/v1.json` are checked
 * against both.
 *
 * Some agents store only the provider's SUMMARY of the model's reasoning (Codex's `reasoning.summary`,
 * OpenAI models through OpenCode). The capture puts a marker in front of such text, inside `model_cot`, so
 * that FORMAT 5 does not change:
 *
 *     marked := "[cvc: provider summary of the model's reasoning " B "]" "\n\n" body
 *     B      := "~cvc-" 16*16 [0-9a-f]
 *
 * and B is `~cvc-` and the first 8 bytes, in lowercase hex, of
 *
 *     SHA-256( "cvc.reasoning-boundary/v1" 0x00 id 0x00 kind 0x00 c )
 *
 * where `id` is the interaction id (36 lowercase ASCII characters), `kind` is `provider_summary`, and `c` is
 * a 32-bit big-endian number from 0 to 63 (the writer uses the smallest one whose token occurs nowhere in
 * the body). A reader trusts the marker only when it opens the text exactly, the body is not blank (white
 * space as Rust's `str::trim` counts it), the token occurs nowhere in the body, and the token is the
 * boundary of the node's own id for some `c`. Anything else is `unverified`: the whole text, shown as
 * reasoning the agent exposed, under today's label. There is deliberately no "full reasoning" kind.
 *
 * Like the stimulus boundary, the token is a consistency check over public values, not a signature: it says
 * the text was written for this node, not by whom. Imitation can only make a reader understate.
 *
 * Pure, and linear in the length of the text (one `includes` for the token, one scan for the body's blank).
 */
import { Sha256, asciiBytes, toHex } from './sha256';

/** The kinds a marker can state. */
export type MarkedReasoningKind = 'provider_summary';

export type ReasoningReading =
  /** A verified marker opened the text: the kind it states, and the body after it, marker stripped. */
  | { readonly kind: MarkedReasoningKind; readonly body: string }
  /** No verified marker: the text, all of it, to be shown as reasoning the agent exposed. */
  | { readonly kind: 'unverified'; readonly text: string };

/** How many boundary candidates a writer tries, and so a reader checks. */
export const REASONING_BOUNDARY_CANDIDATES = 64;

const PREFIX = '~cvc-';
const TOKEN_LENGTH = 21;
const DOMAIN = asciiBytes('cvc.reasoning-boundary/v1\u0000');
const ZERO = Uint8Array.of(0);
const MARKER_OPEN = '[cvc: ';
const MARKER_CLOSE = ']';
const SEPARATOR = '\n\n';

/** The kinds that are marked, in the order a reader tries them: the kind's name (a hash input) and its marker words. */
const MARKED_KINDS: readonly { readonly kind: MarkedReasoningKind; readonly words: string }[] = [
  { kind: 'provider_summary', words: "provider summary of the model's reasoning" },
];

const INTERACTION_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** What Rust's `char::is_whitespace` (the Unicode White_Space property) counts, which is not what `String.prototype.trim` counts. */
function isRustWhitespace(code: number): boolean {
  return (
    (code >= 0x09 && code <= 0x0d) ||
    code === 0x20 ||
    code === 0x85 ||
    code === 0xa0 ||
    code === 0x1680 ||
    (code >= 0x2000 && code <= 0x200a) ||
    code === 0x2028 ||
    code === 0x2029 ||
    code === 0x202f ||
    code === 0x205f ||
    code === 0x3000
  );
}

function isBlank(text: string): boolean {
  for (let at = 0; at < text.length; at++) if (!isRustWhitespace(text.charCodeAt(at))) return false;
  return true;
}

function isLowerHex(code: number): boolean {
  return (code >= 48 && code <= 57) || (code >= 97 && code <= 102);
}

/** Whether `text` is exactly one boundary token: `~cvc-` and 16 lowercase hex digits. */
function isToken(text: string): boolean {
  if (text.length !== TOKEN_LENGTH || !text.startsWith(PREFIX)) return false;
  for (let at = PREFIX.length; at < TOKEN_LENGTH; at++) if (!isLowerHex(text.charCodeAt(at))) return false;
  return true;
}

/**
 * Candidate `candidate` (0 to 63) of the boundary of reasoning of kind `kind` in interaction `id`: the
 * producer's `boundary()`. `id` must be an interaction id; it is hashed in lowercase.
 */
export function reasoningBoundary(id: string, kind: MarkedReasoningKind | 'exposed', candidate: number): string {
  const digest = new Sha256()
    .update(DOMAIN)
    .update(asciiBytes(id.toLowerCase()))
    .update(ZERO)
    .update(asciiBytes(kind))
    .update(ZERO)
    .update(Uint8Array.of((candidate >>> 24) & 255, (candidate >>> 16) & 255, (candidate >>> 8) & 255, candidate & 255))
    .digest();
  return PREFIX + toHex(digest.subarray(0, 8));
}

/** The body of the verified marker of kind `kind` that opens `text`, or `null`. */
function verifiedBody(id: string, kind: MarkedReasoningKind, words: string, text: string): string | null {
  if (!text.startsWith(MARKER_OPEN)) return null;
  let at = MARKER_OPEN.length;
  if (!text.startsWith(words, at)) return null;
  at += words.length;
  if (text.charCodeAt(at) !== 0x20) return null;
  at += 1;
  const token = text.slice(at, at + TOKEN_LENGTH);
  if (!isToken(token)) return null;
  at += TOKEN_LENGTH;
  if (!text.startsWith(MARKER_CLOSE, at)) return null;
  at += MARKER_CLOSE.length;
  if (!text.startsWith(SEPARATOR, at)) return null;
  at += SEPARATOR.length;
  const body = text.slice(at);
  if (isBlank(body) || body.includes(token)) return null;
  for (let candidate = 0; candidate < REASONING_BOUNDARY_CANDIDATES; candidate++) {
    if (reasoningBoundary(id, kind, candidate) === token) return body;
  }
  return null;
}

/** Reads the `model_cot` of interaction `id`. */
export function readReasoning(id: string, modelCot: string): ReasoningReading {
  // The id as the boundary hashes it: 36 lowercase ASCII characters. Anything else cannot verify.
  if (typeof id === 'string' && INTERACTION_ID.test(id) && typeof modelCot === 'string') {
    for (const { kind, words } of MARKED_KINDS) {
      const body = verifiedBody(id.toLowerCase(), kind, words, modelCot);
      if (body !== null) return { kind, body };
    }
  }
  return { kind: 'unverified', text: typeof modelCot === 'string' ? modelCot : '' };
}
