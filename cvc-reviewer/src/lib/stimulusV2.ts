/**
 * Version 2 of the stimulus encoding (issue #133): a reader.
 *
 * The `user_prompt` of a Claude Code node is the STIMULUS of one assistant response: what the model
 * was reacting to, as ordered parts (a person's text, a subagent's delegated task, context the
 * harness injected, a compaction summary, and the results of tools). Version 1 joined the parts with
 * bracketed prefixes (`[tool_result Bash] ...`) that a tool's own output can imitate, so a reader
 * could not tell a person's words from text a tool returned (`transcriptNode.ts`, `parseStimulus`,
 * is that reader, and is unchanged). Version 2 starts every part with a marker that carries a
 * BOUNDARY TOKEN `B`, which tool output cannot have contained when it was written, and a reader
 * trusts a marker only when `B` verifies against the node's own identity.
 *
 * This module mirrors `cvc-core/src/stimulus.rs` (`read` and `verify`), which is the specification,
 * rule for rule, including the edges. Its grammar:
 *
 *     stimulus := "[no stimulus recorded]" | part ( "\n\n" part )* [ "\n\n" omitted ]
 *     part     := marker content
 *     marker   := "[prompt " B "] "  |  "[task " B "] "  |  "[meta " B "] "
 *              |  "[compaction summary " B "]\n"
 *              |  "[tool_result " NAME " " B "] "  |  "[tool_result " NAME " error " B "] "
 *     B        := "~cvc-" 16*16 [0-9a-f]
 *     NAME     := 1*200 [A-Za-z0-9_.:-]
 *     omitted  := "[cvc: omitted " N " parts " B "]"
 *     note     := "…[cvc: truncated " N " bytes " B "]"  |  "[cvc: withheld " B "]"     (N: [1-9][0-9]*, at most 18 digits)
 *
 * A marker is at offset 0 or right after a blank line, the blank line being the LAST two bytes before
 * it (the content of the part before may itself end in newlines, and a blank line cannot overlap the
 * terminator of the marker before it, which the compaction marker ends in a newline). A part's content
 * runs from the end of its marker to the blank line before the next one. A part cut by the capture's
 * size bounds is `head "\n" note "\n" tail`, `head "\n" note`, `note "\n" tail` or `note`; a part
 * that could not be scrubbed is the `withheld` note as its whole content.
 *
 * The boundary is `~cvc-` followed by the first 8 bytes of
 *
 *     SHA-256( "cvc.stimulus-boundary/v2" 0x00 id 0x00 shape 0x00 c )
 *
 * in lowercase hex, where `id` is the interaction id (36 lowercase ASCII characters), `shape` the
 * part kinds in order joined by `,`, and `c` a 32-bit big-endian number from 0 to 63 that the writer
 * chooses as the smallest one whose token occurs nowhere in the content.
 *
 * Three outcomes (`StimulusReading`):
 *
 * - `verified`: the text opens with a marker, every occurrence of its token is a marker or a note
 *   where the writer puts one, and the token equals the boundary of (id, the parsed shape, some c).
 *   Its parts are then known: each part's kind, and for a tool result its name and whether it failed,
 *   are the capture's own, and nothing a tool printed can have forged them.
 * - `undivided`: the text carries a token (`~cvc-` and 16 lowercase hex digits) but does not verify.
 *   Show it as one block and NEVER parse it as version 1: a marker whose token fails is text, and so is
 *   everything around it. (The writer's fail-safe, `[cvc: stimulus parts not marked]`, reads this way.)
 * - `legacy`: no token at all. Version 1, or text no harness wrote: read as before.
 *
 * The token is a consistency check, not a signature. It is computed from public values, so it exposes
 * tool output that imitates the structure (the id does not exist until after the output was written)
 * and stored text changed without recomputing it; it does not say who wrote the text.
 *
 * Everything here is pure, and linear in the length of the text however hostile it is: markers and
 * notes are found by one scan each, a note is recognised from the position of its token in constant
 * time, the part that holds a note by a binary search, and the 64 candidates share the hash of their
 * common prefix (see `sha256.ts`).
 */
import { Sha256, asciiBytes, toHex } from './sha256';

/** What a stimulus with no parts is stored as, in every version. */
export const NO_STIMULUS = '[no stimulus recorded]';

/** The name a tool result carries when its tool is not known, or its name is not one a marker can hold. */
export const UNKNOWN_TOOL = 'unknown-tool';
/** The name a tool result carries when the scrubber would have changed the tool's name. */
export const REDACTED_TOOL = 'redacted-tool';

const BOUNDARY_PREFIX = '~cvc-';
/** `~cvc-` and 16 lowercase hex digits. */
const BOUNDARY_LEN = 21;
const MAX_NAME = 200;
/** How many boundary candidates a writer tries, and so a reader checks. */
const CANDIDATES = 64;
const TRUNCATED_OPEN = '…[cvc: truncated ';
const WITHHELD_OPEN = '[cvc: withheld ';
const OMITTED_OPEN = '[cvc: omitted ';
const BYTES = ' bytes ';
const PARTS = ' parts ';
const DOMAIN = 'cvc.stimulus-boundary/v2\u0000';

const NEWLINE = 10;
const OPEN_BRACKET = 91;
const CLOSE_BRACKET = 93;
const SPACE = 32;

/** What kind of thing a part is. The spelling is the one the boundary's `shape` uses. */
export type PartKind = 'prompt' | 'task' | 'meta' | 'compaction_summary' | 'tool_result';

/**
 * A stretch of a part's content: its text, or a note the capture left where it cut the part or
 * could not keep it. A note says what happened in the capture's own words, never in text of the
 * node's; the token it carries is not part of it.
 */
export type ContentPiece =
  | { kind: 'text'; text: string }
  /** This many bytes of the scrubbed part were removed here. */
  | { kind: 'truncated'; bytes: bigint }
  /** The part could not be scrubbed and is not recorded: it is the part's only piece. */
  | { kind: 'withheld' };

/** One part of a verified stimulus. */
export interface ReadPart {
  kind: PartKind;
  /** The tool's name, for a tool result (`unknown-tool` or `redacted-tool` when it is not known or not shown). `null` otherwise. */
  name: string | null;
  /** The harness flagged the tool result as an error. */
  isError: boolean;
  /** The part's content in order; empty when the part is. A note stands where the capture cut. */
  content: ContentPiece[];
}

export type StimulusReading =
  | {
      kind: 'verified';
      /** The boundary token as it appears in the text. It is never drawn. */
      boundary: string;
      /** The candidate number `c` the token verified with. */
      candidate: number;
      parts: ReadPart[];
      /** How many parts after the last one shown were not recorded; `null` when none. */
      omittedParts: bigint | null;
    }
  | { kind: 'undivided' }
  | { kind: 'legacy' };

const LEGACY: StimulusReading = { kind: 'legacy' };
const UNDIVIDED: StimulusReading = { kind: 'undivided' };

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

function isLowerHex(code: number): boolean {
  return (code >= 48 && code <= 57) || (code >= 97 && code <= 102);
}

function isDigit(code: number): boolean {
  return code >= 48 && code <= 57;
}

/** `[A-Za-z0-9_.:-]` */
function isNameCode(code: number): boolean {
  return (
    (code >= 48 && code <= 58) || // 0-9 and ':'
    (code >= 65 && code <= 90) || // A-Z
    (code >= 97 && code <= 122) || // a-z
    code === 95 || // _
    code === 46 || // .
    code === 45 // -
  );
}

/** Whether a boundary token (`~cvc-` and 16 lowercase hex digits) starts at `at`. */
function tokenAt(text: string, at: number): boolean {
  if (at < 0 || at + BOUNDARY_LEN > text.length || !text.startsWith(BOUNDARY_PREFIX, at)) return false;
  for (let i = at + BOUNDARY_PREFIX.length; i < at + BOUNDARY_LEN; i++) {
    if (!isLowerHex(text.charCodeAt(i))) return false;
  }
  return true;
}

/** Whether any boundary-shaped token occurs in `text`. */
export function containsToken(text: string): boolean {
  for (let at = text.indexOf(BOUNDARY_PREFIX); at !== -1; at = text.indexOf(BOUNDARY_PREFIX, at + 1)) {
    if (tokenAt(text, at)) return true;
  }
  return false;
}

/** Whether `name` is one a marker can carry: `[A-Za-z0-9_.:-]{1,200}`. */
export function isValidToolName(name: string): boolean {
  if (name.length === 0 || name.length > MAX_NAME) return false;
  for (let i = 0; i < name.length; i++) if (!isNameCode(name.charCodeAt(i))) return false;
  return true;
}

// ---------------------------------------------------------------------------
// Markers and notes
// ---------------------------------------------------------------------------

interface Marker {
  kind: PartKind;
  name: string | null;
  isError: boolean;
  token: string;
  /** Where the marker starts. */
  start: number;
  /** Where its token starts. */
  tokenAt: number;
  /** Just past its terminator: where the part's content starts. */
  end: number;
}

/**
 * The marker that starts at `at` (which holds a `[`), if the strict grammar holds. A name is a run of
 * at most MAX_NAME characters and is scanned once, so this costs the same whatever follows it.
 */
function markerAt(text: string, at: number): Marker | null {
  let kind: PartKind;
  let cursor: number;
  if (text.startsWith('[prompt ', at)) {
    kind = 'prompt';
    cursor = at + 8;
  } else if (text.startsWith('[task ', at)) {
    kind = 'task';
    cursor = at + 6;
  } else if (text.startsWith('[meta ', at)) {
    kind = 'meta';
    cursor = at + 6;
  } else if (text.startsWith('[compaction summary ', at)) {
    kind = 'compaction_summary';
    cursor = at + 20;
  } else if (text.startsWith('[tool_result ', at)) {
    kind = 'tool_result';
    cursor = at + 13;
  } else {
    return null;
  }

  let name: string | null = null;
  let isError = false;
  if (kind === 'tool_result') {
    const limit = Math.min(text.length, cursor + MAX_NAME + 1);
    let end = cursor;
    while (end < limit && isNameCode(text.charCodeAt(end))) end++;
    if (end === cursor || end - cursor > MAX_NAME) return null;
    name = text.slice(cursor, end);
    // One space follows the name. `error ` is the flag only when a token follows it: a tool may be named `error`.
    if (text.charCodeAt(end) !== SPACE) return null;
    cursor = end + 1;
    if (text.startsWith('error ', cursor) && text.startsWith(BOUNDARY_PREFIX, cursor + 6)) {
      isError = true;
      cursor += 6;
    }
  }

  if (!tokenAt(text, cursor)) return null;
  const terminator = kind === 'compaction_summary' ? ']\n' : '] ';
  if (!text.startsWith(terminator, cursor + BOUNDARY_LEN)) return null;
  return {
    kind,
    name,
    isError,
    token: text.slice(cursor, cursor + BOUNDARY_LEN),
    start: at,
    tokenAt: cursor,
    end: cursor + BOUNDARY_LEN + terminator.length,
  };
}

/** Whether `text` opens with a version 2 marker (token-shaped, not yet verified): the first line of a stimulus the capture marked. */
export function opensWithMarker(text: string): boolean {
  return text.charCodeAt(0) === OPEN_BRACKET && markerAt(text, 0) !== null;
}

type NoteKind = 'truncated' | 'withheld' | 'omitted';

interface Note {
  kind: NoteKind;
  /** Bytes removed (`truncated`) or parts left out (`omitted`); 0 for `withheld`. Exact up to 18 digits. */
  count: bigint;
  /** Where the note's own text starts (the `…` or `[`), and just past its closing `]`. */
  start: number;
  end: number;
}

/**
 * Where the run of digits that ends at `end` starts, when it is a count a note can carry: one to 18
 * digits and no leading zero. Never looks further back than 19 characters.
 */
function countStart(text: string, end: number): number | null {
  let start = end;
  while (start > 0 && start > end - 19 && isDigit(text.charCodeAt(start - 1))) start--;
  const digits = end - start;
  if (digits === 0 || digits > 18 || text.charCodeAt(start) === 48) return null;
  return start;
}

/**
 * The note whose token starts at `at`, if the text around the token spells one. In constant time:
 * it looks only at the characters just before and after the token.
 */
function noteAt(text: string, at: number): Note | null {
  if (text.charCodeAt(at + BOUNDARY_LEN) !== CLOSE_BRACKET) return null;
  const end = at + BOUNDARY_LEN + 1;

  if (at >= BYTES.length && text.startsWith(BYTES, at - BYTES.length)) {
    const digitsEnd = at - BYTES.length;
    const start = countStart(text, digitsEnd);
    if (start !== null && start >= TRUNCATED_OPEN.length && text.startsWith(TRUNCATED_OPEN, start - TRUNCATED_OPEN.length)) {
      return { kind: 'truncated', count: BigInt(text.slice(start, digitsEnd)), start: start - TRUNCATED_OPEN.length, end };
    }
  }
  if (at >= WITHHELD_OPEN.length && text.startsWith(WITHHELD_OPEN, at - WITHHELD_OPEN.length)) {
    return { kind: 'withheld', count: 0n, start: at - WITHHELD_OPEN.length, end };
  }
  if (at >= PARTS.length && text.startsWith(PARTS, at - PARTS.length)) {
    const digitsEnd = at - PARTS.length;
    const start = countStart(text, digitsEnd);
    if (start !== null && start >= OMITTED_OPEN.length && text.startsWith(OMITTED_OPEN, start - OMITTED_OPEN.length)) {
      return { kind: 'omitted', count: BigInt(text.slice(start, digitsEnd)), start: start - OMITTED_OPEN.length, end };
    }
  }
  return null;
}

// ---------------------------------------------------------------------------
// The boundary
// ---------------------------------------------------------------------------

const DOMAIN_BYTES = asciiBytes(DOMAIN);
const ZERO = Uint8Array.of(0);

/** The hash of everything the candidates have in common: the domain, the id and the shape, each followed by a zero byte. */
function prefixHash(id: string, shape: string): Sha256 {
  return new Sha256().update(DOMAIN_BYTES).update(asciiBytes(id)).update(ZERO).update(asciiBytes(shape)).update(ZERO);
}

/** The token a prefix hash gives candidate `candidate` (0 to 63). Does not change `prefix`. */
function tokenFor(prefix: Sha256, candidate: number): string {
  const digest = prefix.clone().update(Uint8Array.of(0, 0, 0, candidate)).digest();
  return BOUNDARY_PREFIX + toHex(digest.subarray(0, 8));
}

/**
 * Candidate `candidate` (0 to 63) of the boundary of the stimulus of interaction `id` whose parts have
 * the kinds `shape` (`prompt`, `task`, `meta`, `compaction_summary` or `tool_result`, in order, joined by
 * `,`): the producer's `boundary()`. `id` is 36 lowercase ASCII characters.
 */
export function boundaryToken(id: string, shape: string, candidate: number): string {
  return tokenFor(prefixHash(id, shape), candidate);
}

/** The smallest candidate whose token is `token`, or `null`. */
function verifiedCandidate(id: string, shape: string, token: string): number | null {
  const prefix = prefixHash(id, shape);
  for (let candidate = 0; candidate < CANDIDATES; candidate++) {
    if (tokenFor(prefix, candidate) === token) return candidate;
  }
  return null;
}

const INTERACTION_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** The id as the boundary hashes it (lowercase), or `null` for a string that is not an interaction id. */
function canonicalId(id: unknown): string | null {
  return typeof id === 'string' && INTERACTION_ID.test(id) ? id.toLowerCase() : null;
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/**
 * Reads the stimulus `text` of interaction `id`. See the module description for the three outcomes.
 * `id` is the node's own id: a stimulus verifies only for the interaction it was written for.
 */
export function readStimulusV2(id: string, text: string): StimulusReading {
  if (text === NO_STIMULUS) return LEGACY;
  const first = text.charCodeAt(0) === OPEN_BRACKET ? markerAt(text, 0) : null;
  if (first === null) return containsToken(text) ? UNDIVIDED : LEGACY;
  const interaction = canonicalId(id);
  if (interaction === null) return UNDIVIDED;
  return verify(interaction, text, first) ?? UNDIVIDED;
}

/** Index of the last of the ascending `starts` that is at or before `position`, or -1. */
function lastAtOrBefore(starts: readonly number[], position: number): number {
  let low = 0;
  let high = starts.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (starts[middle] <= position) low = middle + 1;
    else high = middle;
  }
  return low - 1;
}

function verify(id: string, text: string, first: Marker): StimulusReading | null {
  const boundary = first.token;

  // The markers: every `[` that follows a blank line (the LAST two characters before it) and starts a
  // marker with the first one's token. The blank line cannot overlap the previous marker's terminator.
  const markers: Marker[] = [first];
  for (let at = text.indexOf('[', 2); at !== -1; at = text.indexOf('[', at + 1)) {
    if (text.charCodeAt(at - 1) !== NEWLINE || text.charCodeAt(at - 2) !== NEWLINE) continue;
    if (at - 2 < markers[markers.length - 1].end) continue;
    const marker = markerAt(text, at);
    if (marker !== null && marker.token === boundary) markers.push(marker);
  }

  // Every occurrence of the boundary is a marker's or a note's.
  const markerTokens = new Set<number>();
  for (const marker of markers) markerTokens.add(marker.tokenAt);
  const notes: Note[] = [];
  for (let at = text.indexOf(boundary); at !== -1; at = text.indexOf(boundary, at + BOUNDARY_LEN)) {
    if (markerTokens.has(at)) continue;
    const note = noteAt(text, at);
    if (note === null) return null;
    notes.push(note);
  }

  // The closing `omitted` note, if there is one, is the only one, ends the text and follows a blank line.
  const lastMarkerEnd = markers[markers.length - 1].end;
  let endOfParts = text.length;
  let omittedParts: bigint | null = null;
  const closing = notes.filter((note) => note.kind === 'omitted');
  if (closing.length > 0) {
    const note = closing[0];
    const separated =
      note.start >= 2 &&
      text.charCodeAt(note.start - 1) === NEWLINE &&
      text.charCodeAt(note.start - 2) === NEWLINE &&
      note.start - 2 >= lastMarkerEnd;
    if (closing.length > 1 || note.end !== text.length || !separated) return null;
    endOfParts = note.start - 2;
    omittedParts = note.count;
  }

  // A part's content runs from its marker to the blank line before the next one.
  const contentStart = markers.map((marker) => marker.end);
  const contentEnd = markers.map((_, index) => (index + 1 < markers.length ? markers[index + 1].start - 2 : endOfParts));

  // A `withheld` note is a part's whole content and a `truncated` note lies inside one. The parts are
  // in order, so the part that holds a note is the last one that starts before it: a lookup, not a scan.
  const noteLists: Array<Note[] | undefined> = [];
  for (const note of notes) {
    if (note.kind === 'omitted') continue;
    const holder = lastAtOrBefore(contentStart, note.start);
    if (holder < 0 || note.end > contentEnd[holder]) return null;
    if (note.kind === 'withheld' && (contentStart[holder] !== note.start || contentEnd[holder] !== note.end)) return null;
    (noteLists[holder] ??= []).push(note);
  }

  const shape = markers.map((marker) => marker.kind).join(',');
  const candidate = verifiedCandidate(id, shape, boundary);
  if (candidate === null) return null;

  const parts: ReadPart[] = markers.map((marker, index) => ({
    kind: marker.kind,
    name: marker.name,
    isError: marker.isError,
    content: piecesOf(text, contentStart[index], contentEnd[index], noteLists[index]),
  }));
  return { kind: 'verified', boundary, candidate, parts, omittedParts };
}

/**
 * The content `text[start, end)` of one part as text and notes. The writer puts a newline on each
 * side of a `truncated` note that has text beside it; those two newlines belong to the note.
 */
function piecesOf(text: string, start: number, end: number, notes: readonly Note[] | undefined): ContentPiece[] {
  if (notes === undefined) return start === end ? [] : [{ kind: 'text', text: text.slice(start, end) }];
  const pieces: ContentPiece[] = [];
  let cursor = start;
  for (const note of notes) {
    if (note.kind === 'withheld') {
      pieces.push({ kind: 'withheld' });
      cursor = note.end;
      continue;
    }
    let textEnd = note.start;
    if (textEnd > cursor && text.charCodeAt(textEnd - 1) === NEWLINE) textEnd--;
    if (textEnd > cursor) pieces.push({ kind: 'text', text: text.slice(cursor, textEnd) });
    pieces.push({ kind: 'truncated', bytes: note.count });
    cursor = note.end;
    if (cursor < end && text.charCodeAt(cursor) === NEWLINE) cursor++;
  }
  if (cursor < end) pieces.push({ kind: 'text', text: text.slice(cursor, end) });
  return pieces;
}
