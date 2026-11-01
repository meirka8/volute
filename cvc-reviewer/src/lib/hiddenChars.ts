/**
 * Hidden and bidirectional characters (issue #135).
 *
 * A command, a path, a diff line or a tool's output can hold characters that draw
 * nothing, or that reorder the text around them: a right-to-left override can make
 * `rm -rf ./build` read as something else ("Trojan Source"), a zero-width space
 * makes two different lines look identical. This module finds them, so the
 * timeline can draw each one as a visible token such as `⟨U+202E⟩`.
 *
 * Pure functions only; no React, no DOM. Everything is ONE pass over the text with
 * a lookup table per code unit: no regular expression, so no backtracking, and the
 * cost is linear in the length of the text whatever it holds.
 *
 * The class follows Unicode's Default_Ignorable_Code_Point property: every code point
 * that property lists is in it (the unassigned ones among them too, which no font
 * draws and a later version of Unicode may give a meaning), together with the C0 and
 * C1 controls and the interlinear annotation characters. The tests walk the whole
 * code space against that property, so a new member of it is caught.
 *
 * Two levels of strictness, because some of these characters have honest uses:
 *
 * - `strict`, for code-like text (commands, paths, diffs, tool output, names):
 *   the whole class;
 * - `prose`, for text a person or the model wrote (prompts, reasoning, replies,
 *   todo items): the class WITHOUT the zero-width joiner and non-joiner (emoji
 *   sequences, Persian and Indic text) and the variation selectors U+FE00-U+FE0F
 *   (which only choose between text and emoji presentation).
 */

export type HiddenMode = 'strict' | 'prose';

/**
 * The most hidden characters drawn one by one in a single block of text. Past
 * that, the rest are counted in a single summary token and left out of the text,
 * so a block made of nothing but such characters cannot become a million elements.
 */
export const MAX_HIDDEN_TOKENS = 200;

/** A piece of text, in order: plain text, a hidden character, or the summary of those past the limit. */
export type HiddenSegment =
  | { kind: 'text'; text: string }
  | { kind: 'hidden'; codePoint: number }
  | { kind: 'more'; count: number };

// ---------------------------------------------------------------------------
// The class
// ---------------------------------------------------------------------------

/** Inclusive ranges of the Basic Multilingual Plane that are hidden in BOTH modes. */
const BASELINE_RANGES: ReadonlyArray<readonly [number, number]> = [
  [0x0000, 0x0008], // C0 controls, except TAB (09), LF (0A) and CR (0D)
  [0x000b, 0x000c],
  [0x000e, 0x001f],
  [0x007f, 0x009f], // DEL and the C1 controls
  [0x00ad, 0x00ad], // soft hyphen
  [0x034f, 0x034f], // combining grapheme joiner
  [0x061c, 0x061c], // Arabic letter mark
  [0x115f, 0x1160], // Hangul fillers
  [0x17b4, 0x17b5], // Khmer inherent vowels
  [0x180b, 0x180f], // Mongolian variation selectors and vowel separator
  [0x200b, 0x200f], // zero-width space, joiners, direction marks
  [0x202a, 0x202e], // bidirectional embeddings and overrides
  [0x2060, 0x206f], // word joiner, invisible operators, the unassigned U+2065, isolates and the deprecated format controls
  [0x3164, 0x3164], // Hangul filler
  [0xfeff, 0xfeff], // zero-width no-break space (byte order mark)
  [0xffa0, 0xffa0], // halfwidth Hangul filler
  [0xfff0, 0xfff8], // unassigned code points of the Specials block that the property lists
  [0xfff9, 0xfffb], // interlinear annotation
];

/** Hidden in `strict` only, because they have honest uses in prose. */
const PROSE_EXCEPTIONS: ReadonlyArray<readonly [number, number]> = [[0x200c, 0x200d]];
const STRICT_EXTRAS: ReadonlyArray<readonly [number, number]> = [[0xfe00, 0xfe0f]];

/**
 * The members of the class outside the Basic Multilingual Plane, which UTF-16 writes as a high
 * surrogate followed by a low one:
 *
 * - U+E0000-U+E0FFF (the tag characters, the variation selectors supplement and the code points
 *   between and after them): the high surrogates 0xDB40-0xDB43 with any low surrogate;
 * - U+1D173-U+1D17A (musical symbols that mark beams, ties, slurs and phrases): 0xD834 with
 *   0xDD73-0xDD7A;
 * - U+1BCA0-U+1BCA3 (shorthand format controls): 0xD82F with 0xDCA0-0xDCA3.
 */
const TAGS_HIGH = [0xdb40, 0xdb43] as const;
const MUSICAL_HIGH = 0xd834;
const MUSICAL_LOW = [0xdd73, 0xdd7a] as const;
const SHORTHAND_HIGH = 0xd82f;
const SHORTHAND_LOW = [0xdca0, 0xdca3] as const;

/** Whether a high surrogate and the code unit after it are a member of the class. */
function pairIsHidden(high: number, low: number): boolean {
  if (!(low >= 0xdc00 && low <= 0xdfff)) return false; // NaN past the end of the text, which no comparison accepts
  if (high >= TAGS_HIGH[0] && high <= TAGS_HIGH[1]) return true;
  if (high === MUSICAL_HIGH) return low >= MUSICAL_LOW[0] && low <= MUSICAL_LOW[1];
  if (high === SHORTHAND_HIGH) return low >= SHORTHAND_LOW[0] && low <= SHORTHAND_LOW[1];
  return false;
}

/** The code point a surrogate pair stands for. */
function pairCodePoint(high: number, low: number): number {
  return 0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00);
}

function fill(table: Uint8Array, ranges: ReadonlyArray<readonly [number, number]>, value: 0 | 1) {
  for (const [low, high] of ranges) table.fill(value, low, high + 1);
}

/** One byte per UTF-16 code unit of the Basic Multilingual Plane: 1 when it is hidden. */
const STRICT_TABLE = new Uint8Array(0x10000);
const PROSE_TABLE = new Uint8Array(0x10000);
fill(STRICT_TABLE, BASELINE_RANGES, 1);
fill(STRICT_TABLE, STRICT_EXTRAS, 1);
fill(PROSE_TABLE, BASELINE_RANGES, 1);
fill(PROSE_TABLE, PROSE_EXCEPTIONS, 0);

const TABLES: Record<HiddenMode, Uint8Array> = { strict: STRICT_TABLE, prose: PROSE_TABLE };

/**
 * How many UTF-16 code units the hidden character at `index` takes: 0 when the
 * character there is not hidden, 1 for the Basic Multilingual Plane, 2 for a member
 * of the class written as a surrogate pair.
 */
function hiddenUnitsAt(text: string, index: number, table: Uint8Array): 0 | 1 | 2 {
  const unit = text.charCodeAt(index);
  if (unit >= 0xd800 && unit <= 0xdbff) return pairIsHidden(unit, text.charCodeAt(index + 1)) ? 2 : 0;
  return table[unit] === 1 ? 1 : 0;
}

/**
 * How many UTF-16 code units the member of the strict class at `index` takes (0 when there is none
 * there): for code that has to read text the way the cards draw it, which is the strict class.
 */
export function strictHiddenUnitsAt(text: string, index: number): 0 | 1 | 2 {
  return hiddenUnitsAt(text, index, STRICT_TABLE);
}

/** Whether a code point is in the class at this level (the strict one unless said otherwise). */
export function isHiddenCodePoint(codePoint: number, mode: HiddenMode = 'strict'): boolean {
  if (codePoint < 0x10000) return TABLES[mode][codePoint] === 1;
  return (
    (codePoint >= 0xe0000 && codePoint <= 0xe0fff) ||
    (codePoint >= 0x1d173 && codePoint <= 0x1d17a) ||
    (codePoint >= 0x1bca0 && codePoint <= 0x1bca3)
  );
}

/** Whether `text` holds any hidden character of this level. Stops at the first one. */
export function hasHiddenChars(text: string, mode: HiddenMode): boolean {
  const table = TABLES[mode];
  for (let index = 0; index < text.length; index++) {
    const unit = text.charCodeAt(index);
    if (unit >= 0x20 && unit < 0x7f) continue; // printable ASCII: nearly all text
    if (hiddenUnitsAt(text, index, table) !== 0) return true;
  }
  return false;
}

/**
 * `text` with every character of the class left out, in one pass: for matching, where a
 * character that draws nothing must not be able to hide a match (`pass<ZWSP>word` reads as
 * "password" to whoever searches for it, and is drawn as ⟨U+200B⟩ between two halves).
 * Tab, line feed and carriage return are not in the class and stay.
 *
 * Returns `text` itself when there is nothing to leave out, so the common case allocates
 * nothing. Like the rest of this module it is one pass with a lookup table: no regular
 * expression, and linear whatever the text holds.
 */
export function stripHidden(text: string, mode: HiddenMode = 'strict'): string {
  const table = TABLES[mode];
  const length = text.length;
  let kept: string[] | null = null;
  let runStart = 0;
  for (let index = 0; index < length; ) {
    const unit = text.charCodeAt(index);
    if (unit >= 0x20 && unit < 0x7f) {
      index++;
      continue;
    }
    const units = hiddenUnitsAt(text, index, table);
    if (units === 0) {
      index++;
      continue;
    }
    kept ??= [];
    if (index > runStart) kept.push(text.slice(runStart, index));
    index += units;
    runStart = index;
  }
  if (kept === null) return text;
  if (runStart < length) kept.push(text.slice(runStart));
  return kept.join('');
}

/**
 * Splits `text` into plain runs and hidden characters, in order, in one pass. It
 * returns `null` when there is no hidden character at all, so the common case
 * costs no allocation and the caller can keep drawing the string as it is.
 *
 * At most `maxTokens` hidden characters come back as `hidden` segments. The ones
 * after that are dropped from the text and counted in one `more` segment, put where
 * the first of them was; everything after it is one plain run.
 */
export function splitHidden(
  text: string,
  mode: HiddenMode,
  maxTokens: number = MAX_HIDDEN_TOKENS,
): HiddenSegment[] | null {
  const table = TABLES[mode];
  const length = text.length;
  let segments: HiddenSegment[] | null = null;
  let runStart = 0;
  let tokens = 0;
  let more: { kind: 'more'; count: number } | null = null;
  // Plain runs that come after the limit; joined once at the end instead of one segment each.
  let rest: string[] | null = null;

  for (let index = 0; index < length; ) {
    const unit = text.charCodeAt(index);
    // Printable ASCII is the overwhelming case: skip it without a table lookup.
    if (unit >= 0x20 && unit < 0x7f) {
      index++;
      continue;
    }
    const units = hiddenUnitsAt(text, index, table);
    if (units === 0) {
      index++;
      continue;
    }

    segments ??= [];
    if (rest !== null) {
      if (index > runStart) rest.push(text.slice(runStart, index));
      more!.count++;
    } else {
      if (index > runStart) segments.push({ kind: 'text', text: text.slice(runStart, index) });
      if (tokens < maxTokens) {
        const codePoint = units === 2 ? pairCodePoint(unit, text.charCodeAt(index + 1)) : unit;
        segments.push({ kind: 'hidden', codePoint });
        tokens++;
      } else {
        more = { kind: 'more', count: 1 };
        segments.push(more);
        rest = [];
      }
    }
    index += units;
    runStart = index;
  }

  if (segments === null) return null;
  if (rest !== null) {
    if (runStart < length) rest.push(text.slice(runStart));
    const remainder = rest.join('');
    if (remainder !== '') segments.push({ kind: 'text', text: remainder });
  } else if (runStart < length) {
    segments.push({ kind: 'text', text: text.slice(runStart) });
  }
  return segments;
}

// ---------------------------------------------------------------------------
// Showing one
// ---------------------------------------------------------------------------

/** `U+202E`: at least four digits, in capitals. */
export function hiddenCodePointLabel(codePoint: number): string {
  return `U+${codePoint.toString(16).toUpperCase().padStart(4, '0')}`;
}

/** The visible token for a hidden character: `⟨U+202E⟩`. */
export function hiddenToken(codePoint: number): string {
  return `⟨${hiddenCodePointLabel(codePoint)}⟩`;
}

const C0_NAMES = [
  'NULL', 'START OF HEADING', 'START OF TEXT', 'END OF TEXT', 'END OF TRANSMISSION', 'ENQUIRY', 'ACKNOWLEDGE',
  'ALERT', 'BACKSPACE', 'CHARACTER TABULATION', 'LINE FEED', 'LINE TABULATION', 'FORM FEED', 'CARRIAGE RETURN',
  'SHIFT OUT', 'SHIFT IN', 'DATA LINK ESCAPE', 'DEVICE CONTROL ONE', 'DEVICE CONTROL TWO', 'DEVICE CONTROL THREE',
  'DEVICE CONTROL FOUR', 'NEGATIVE ACKNOWLEDGE', 'SYNCHRONOUS IDLE', 'END OF TRANSMISSION BLOCK', 'CANCEL',
  'END OF MEDIUM', 'SUBSTITUTE', 'ESCAPE', 'INFORMATION SEPARATOR FOUR', 'INFORMATION SEPARATOR THREE',
  'INFORMATION SEPARATOR TWO', 'INFORMATION SEPARATOR ONE',
];

const C1_NAMES = [
  'PADDING CHARACTER', 'HIGH OCTET PRESET', 'BREAK PERMITTED HERE', 'NO BREAK HERE', 'INDEX', 'NEXT LINE',
  'START OF SELECTED AREA', 'END OF SELECTED AREA', 'CHARACTER TABULATION SET',
  'CHARACTER TABULATION WITH JUSTIFICATION', 'LINE TABULATION SET', 'PARTIAL LINE FORWARD',
  'PARTIAL LINE BACKWARD', 'REVERSE LINE FEED', 'SINGLE SHIFT TWO', 'SINGLE SHIFT THREE', 'DEVICE CONTROL STRING',
  'PRIVATE USE ONE', 'PRIVATE USE TWO', 'SET TRANSMIT STATE', 'CANCEL CHARACTER', 'MESSAGE WAITING',
  'START OF GUARDED AREA', 'END OF GUARDED AREA', 'START OF STRING', 'SINGLE GRAPHIC CHARACTER INTRODUCER',
  'SINGLE CHARACTER INTRODUCER', 'CONTROL SEQUENCE INTRODUCER', 'STRING TERMINATOR', 'OPERATING SYSTEM COMMAND',
  'PRIVACY MESSAGE', 'APPLICATION PROGRAM COMMAND',
];

/** Unicode's names for the rest of the class (the bidirectional controls and the zero-width characters among them). */
const NAMES: Readonly<Record<number, string>> = {
  0x00ad: 'SOFT HYPHEN',
  0x034f: 'COMBINING GRAPHEME JOINER',
  0x061c: 'ARABIC LETTER MARK',
  0x115f: 'HANGUL CHOSEONG FILLER',
  0x1160: 'HANGUL JUNGSEONG FILLER',
  0x17b4: 'KHMER VOWEL INHERENT AQ',
  0x17b5: 'KHMER VOWEL INHERENT AA',
  0x180b: 'MONGOLIAN FREE VARIATION SELECTOR ONE',
  0x180c: 'MONGOLIAN FREE VARIATION SELECTOR TWO',
  0x180d: 'MONGOLIAN FREE VARIATION SELECTOR THREE',
  0x180e: 'MONGOLIAN VOWEL SEPARATOR',
  0x180f: 'MONGOLIAN FREE VARIATION SELECTOR FOUR',
  0x200b: 'ZERO WIDTH SPACE',
  0x200c: 'ZERO WIDTH NON-JOINER',
  0x200d: 'ZERO WIDTH JOINER',
  0x200e: 'LEFT-TO-RIGHT MARK',
  0x200f: 'RIGHT-TO-LEFT MARK',
  0x202a: 'LEFT-TO-RIGHT EMBEDDING',
  0x202b: 'RIGHT-TO-LEFT EMBEDDING',
  0x202c: 'POP DIRECTIONAL FORMATTING',
  0x202d: 'LEFT-TO-RIGHT OVERRIDE',
  0x202e: 'RIGHT-TO-LEFT OVERRIDE',
  0x2060: 'WORD JOINER',
  0x2061: 'FUNCTION APPLICATION',
  0x2062: 'INVISIBLE TIMES',
  0x2063: 'INVISIBLE SEPARATOR',
  0x2064: 'INVISIBLE PLUS',
  0x2066: 'LEFT-TO-RIGHT ISOLATE',
  0x2067: 'RIGHT-TO-LEFT ISOLATE',
  0x2068: 'FIRST STRONG ISOLATE',
  0x2069: 'POP DIRECTIONAL ISOLATE',
  0x206a: 'INHIBIT SYMMETRIC SWAPPING',
  0x206b: 'ACTIVATE SYMMETRIC SWAPPING',
  0x206c: 'INHIBIT ARABIC FORM SHAPING',
  0x206d: 'ACTIVATE ARABIC FORM SHAPING',
  0x206e: 'NATIONAL DIGIT SHAPES',
  0x206f: 'NOMINAL DIGIT SHAPES',
  0x3164: 'HANGUL FILLER',
  0xfeff: 'ZERO WIDTH NO-BREAK SPACE (BYTE ORDER MARK)',
  0xffa0: 'HALFWIDTH HANGUL FILLER',
  0xfff9: 'INTERLINEAR ANNOTATION ANCHOR',
  0xfffa: 'INTERLINEAR ANNOTATION SEPARATOR',
  0xfffb: 'INTERLINEAR ANNOTATION TERMINATOR',
  0xe0001: 'LANGUAGE TAG',
  0xe007f: 'CANCEL TAG',
  0x1bca0: 'SHORTHAND FORMAT LETTER OVERLAP',
  0x1bca1: 'SHORTHAND FORMAT CONTINUING OVERLAP',
  0x1bca2: 'SHORTHAND FORMAT DOWN STEP',
  0x1bca3: 'SHORTHAND FORMAT UP STEP',
  0x1d173: 'MUSICAL SYMBOL BEGIN BEAM',
  0x1d174: 'MUSICAL SYMBOL END BEAM',
  0x1d175: 'MUSICAL SYMBOL BEGIN TIE',
  0x1d176: 'MUSICAL SYMBOL END TIE',
  0x1d177: 'MUSICAL SYMBOL BEGIN SLUR',
  0x1d178: 'MUSICAL SYMBOL END SLUR',
  0x1d179: 'MUSICAL SYMBOL BEGIN PHRASE',
  0x1d17a: 'MUSICAL SYMBOL END PHRASE',
};

/** U+E0000, where the tag characters begin. */
const TAGS_BASE = 0xe0000;

/**
 * What a hidden character is called: Unicode's name where there is a short one,
 * and a description of the group otherwise. A tag character says which ASCII
 * character it stands for, since a run of them can spell out a hidden message.
 */
export function hiddenCharName(codePoint: number): string {
  const named = NAMES[codePoint];
  if (named !== undefined) return named;
  if (codePoint <= 0x1f) return `CONTROL CHARACTER: ${C0_NAMES[codePoint]}`;
  if (codePoint === 0x7f) return 'CONTROL CHARACTER: DELETE';
  if (codePoint >= 0x80 && codePoint <= 0x9f) return `CONTROL CHARACTER: ${C1_NAMES[codePoint - 0x80]}`;
  if (codePoint >= 0xfe00 && codePoint <= 0xfe0f) return `VARIATION SELECTOR-${codePoint - 0xfe00 + 1}`;
  if (codePoint >= 0xe0100 && codePoint <= 0xe01ef) return `VARIATION SELECTOR-${codePoint - 0xe0100 + 17}`;
  if (codePoint >= 0xe0020 && codePoint <= 0xe007e) {
    return `TAG CHARACTER FOR ${JSON.stringify(String.fromCharCode(codePoint - TAGS_BASE))}`;
  }
  if (codePoint >= TAGS_BASE && codePoint <= 0xe007f) return 'TAG CHARACTER';
  // The members that have no character of their own yet: the property lists them so that a later
  // version of Unicode can give them one without changing what a reader sees of the text.
  if (isHiddenCodePoint(codePoint)) return 'UNASSIGNED DEFAULT-IGNORABLE CODE POINT';
  return 'HIDDEN CHARACTER';
}

/** The tooltip of a token: the name, then the code point. */
export function hiddenTitle(codePoint: number): string {
  return `${hiddenCharName(codePoint)} (${hiddenCodePointLabel(codePoint)})`;
}

/**
 * `text` with every hidden character written out as its token, as a plain string:
 * for places that can only hold text (an attribute, a label). The same limit as
 * `splitHidden` applies.
 */
export function hiddenAsText(text: string, mode: HiddenMode): string {
  const segments = splitHidden(text, mode);
  if (segments === null) return text;
  return segments
    .map((segment) => {
      if (segment.kind === 'text') return segment.text;
      if (segment.kind === 'hidden') return hiddenToken(segment.codePoint);
      return `⟨+${segment.count} more⟩`;
    })
    .join('');
}
