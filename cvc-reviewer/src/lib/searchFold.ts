/**
 * The one fold of the search (issue #118): the text the index keeps, the terms a person types, and the
 * text a card draws, all go through it, so that what the search finds is what the page marks.
 *
 * Pure: no React, no DOM, and no regular expression that a text could make slow: the only one is the test for
 * a character that is not printable ASCII or a white space control (`SPECIAL`), which has nothing to backtrack
 * and finds the next such character at the speed of the engine. It reads the text once, a code point at a time,
 * and every code point folds on its own (the result for a code point is remembered, so a text of a million
 * characters that uses two hundred of them asks the engine two hundred questions). In order, a code point:
 *
 * 1. that is White_Space becomes a space (a no-break space, a tab, a line separator, a vertical tab: all
 *    of them), and a carriage return followed by a line feed is one space, as the cards draw it;
 * 2. that is in the hidden class (`lib/hiddenChars`, the strict one) is left out, so that a character
 *    which draws nothing cannot hide a match: `pass<ZWSP>word` is "password" here, and is drawn as two
 *    halves with a token between them;
 * 3. is decomposed in the compatibility form (NFKD): a full-width letter is the letter, a ligature is its
 *    letters, a precomposed accented letter is its letter and its accent, so that it is the same as
 *    the letter followed by the accent;
 * 4. is folded for case in full, one code point at a time: `ß` is "ss", `İ` is "i" and the dot above, and
 *    the final sigma `ς` is `σ`, as it is in the middle of a word, since a term can start or end anywhere
 *    in one. (The dotless `ı` is not `i`: Unicode does not fold them either.)
 *
 * The folded text is longer than the text it comes from, or shorter, so `foldWithMap` says where each unit
 * of it came from: that is how the match of a term in the folded text becomes a stretch of the text on the
 * page, to be marked, and how a mark comes to cover a whole character of the text and the hidden
 * characters inside the match.
 *
 * Things it does NOT fold: a letter of one script that looks like a letter of another (a Cyrillic `а` is
 * not a Latin `a`), and accents when only one of the two texts has them in a way NFKD does not unify
 * (a combining mark can follow a base letter in any order, and the fold does not reorder them).
 */
import { isHiddenCodePoint } from './hiddenChars';

// ---------------------------------------------------------------------------
// One code point
// ---------------------------------------------------------------------------

/** Unicode's White_Space property: the characters `\s` means, plus U+0085 and without U+FEFF. */
export function isWhiteSpaceCode(code: number): boolean {
  return (
    code === 0x20 ||
    (code >= 0x09 && code <= 0x0d) ||
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

/** One code point after the steps above, for the code points that are not plain ASCII. */
function foldString(text: string): string {
  const decomposed = text.normalize('NFKD');
  let out = '';
  for (const character of decomposed) {
    const code = character.codePointAt(0) as number;
    if (isWhiteSpaceCode(code)) out += ' ';
    else if (isHiddenCodePoint(code)) continue;
    // The dotless i is its own letter. Every other case variant folds through the upper case: that is
    // what makes ß "ss", ς σ and the long s an s, none of which the lower case alone does.
    else out += code === 0x131 ? character : character.toUpperCase().toLowerCase();
  }
  return out;
}

function computeFold(codePoint: number): string {
  if (isWhiteSpaceCode(codePoint)) return ' ';
  if (isHiddenCodePoint(codePoint)) return '';
  let current = String.fromCodePoint(codePoint);
  // A result can itself decompose or change case again (ẞ is ß is "ss"): fold until it is stable.
  for (let round = 0; round < 4; round++) {
    const next = foldString(current);
    if (next === current) return current;
    current = next;
  }
  return current;
}

const BMP_CACHE: Array<string | undefined> = new Array<string | undefined>(0x10000);
const ASTRAL_CACHE = new Map<number, string>();

/** What a code point folds to: '' (left out), ' ', or its letters. Remembered, so each is worked out once. */
export function foldCodePoint(codePoint: number): string {
  if (codePoint < 0x10000) {
    let folded = BMP_CACHE[codePoint];
    if (folded === undefined) {
      folded = computeFold(codePoint);
      BMP_CACHE[codePoint] = folded;
    }
    return folded;
  }
  let folded = ASTRAL_CACHE.get(codePoint);
  if (folded === undefined) {
    folded = computeFold(codePoint);
    ASTRAL_CACHE.set(codePoint, folded);
  }
  return folded;
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/**
 * Anything but printable ASCII and the white space controls (tab, line feed, vertical tab, form feed, carriage return):
 * the characters that need more than lower case. A class with nothing to backtrack, run by the engine; it is also what
 * makes the usual text fast, since the engine reads a string of any make (a slice, a concatenation) this way at the same
 * speed, where a loop over its characters in script depends on what the string happens to be.
 */
const SPECIAL = /[^\x20-\x7e\t\n\r\v\f]/g;
/** The same, less the vertical tab, form feed and carriage return: what a text can have and need no map. */
const NOT_PLAIN = /[^\x20-\x7e\t\n]/;

/** A carriage return and line feed together (one line break), or any one white space control. */
const CONTROLS = /\r\n|[\t\n\r\v\f]/g;
/** The white space a text with no map can have. */
const TAB_OR_NEWLINE = /[\t\n]/g;

/** The white space controls of `text`, each a space and a carriage return and line feed together one: one pass of the engine. */
function spaceOutControls(text: string): string {
  return text.replace(CONTROLS, ' ');
}

/** The code point that starts at `index`, and how many code units it takes. */
function codePointAt(text: string, index: number, code: number): number {
  if (code >= 0xd800 && code <= 0xdbff) {
    const low = text.charCodeAt(index + 1);
    if (low >= 0xdc00 && low <= 0xdfff) return 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
  }
  return code;
}

/** Printable ASCII, tabs and line breaks, folded: in lower case, with the white space as spaces. */
function plainRun(text: string): string {
  return spaceOutControls(text.toLowerCase());
}

/**
 * `text` folded (see the top of this file). Total: any string at all. Printable ASCII, tabs and line breaks, which
 * is nearly all of any text, are lower-cased and have their white space turned into spaces by the engine's own string
 * operations, a stretch at a time; each character that is anything else (every non-ASCII character, and the ASCII
 * controls that draw nothing) is folded on its own, and the stretches in between are not broken up by line breaks.
 */
export function fold(text: string): string {
  SPECIAL.lastIndex = 0;
  let found = SPECIAL.exec(text);
  if (found === null) return plainRun(text);
  const length = text.length;
  const pieces: string[] = [];
  let runStart = 0;
  while (found !== null) {
    const at = found.index;
    // The stretch before the character is folded as a whole, and the character on its own.
    if (at > runStart) pieces.push(plainRun(text.slice(runStart, at)));
    const code = text.charCodeAt(at);
    let next = at + 1;
    if (code >= 0x80) {
      const codePoint = codePointAt(text, at, code);
      const replacement = foldCodePoint(codePoint);
      if (replacement !== '') pieces.push(replacement);
      if (codePoint > 0xffff) next++;
    }
    // (An ASCII control that draws nothing, below 0x20 or DEL, is left out: nothing is added for it.)
    runStart = next;
    SPECIAL.lastIndex = next;
    found = SPECIAL.exec(text);
  }
  if (runStart < length) pieces.push(plainRun(text.slice(runStart)));
  return pieces.join('');
}

/** A text folded, and where each unit of it came from. */
export interface Folded {
  /** The folded text; equal to `fold(source)`. */
  readonly text: string;
  /**
   * Per unit of `text`: where in the source the character it came from starts, and where it ends (the source
   * as the cards draw it). Null for both when `text` has a unit for every unit of the source, in the same
   * place (the text had nothing but printable ASCII, tabs and line feeds).
   */
  readonly starts: Uint32Array | null;
  readonly ends: Uint32Array | null;
}

/** An array of unsigned numbers that grows, for the map of a long text. */
class Offsets {
  data: Uint32Array;
  size = 0;

  constructor(capacity: number) {
    this.data = new Uint32Array(Math.max(16, capacity));
  }

  push(value: number): void {
    if (this.size === this.data.length) {
      const grown = new Uint32Array(this.data.length * 2);
      grown.set(this.data);
      this.data = grown;
    }
    this.data[this.size++] = value;
  }

  done(): Uint32Array {
    return this.data.subarray(0, this.size);
  }
}

/**
 * `text` folded, with a map from each unit of the result to the character of `text` it came from. The text
 * of the result is `fold(text)` in every case (a test holds the two to that on random and hostile text).
 */
export function foldWithMap(text: string): Folded {
  const length = text.length;
  // Printable ASCII, tabs and line feeds fold one unit to one unit, in place: no map is needed.
  if (!NOT_PLAIN.test(text)) {
    const lower = text.toLowerCase();
    return { text: lower.replace(TAB_OR_NEWLINE, ' '), starts: null, ends: null };
  }

  const pieces: string[] = [];
  const starts = new Offsets(length + 16);
  const ends = new Offsets(length + 16);
  const note = (from: number, to: number, units: number) => {
    for (let k = 0; k < units; k++) {
      starts.push(from);
      ends.push(to);
    }
  };
  let runStart = 0;
  let at = 0;
  const flush = (upTo: number) => {
    if (upTo <= runStart) return;
    pieces.push(text.slice(runStart, upTo).toLowerCase());
    for (let place = runStart; place < upTo; place++) {
      starts.push(place);
      ends.push(place + 1);
    }
  };
  while (at < length) {
    const code = text.charCodeAt(at);
    if (code >= 0x20 && code <= 0x7e) {
      at++;
      continue;
    }
    flush(at);
    let width = 1;
    let replacement: string;
    if (code < 0x80) {
      if (code === 0x0d && text.charCodeAt(at + 1) === 0x0a) width = 2;
      replacement = code >= 0x09 && code <= 0x0d ? ' ' : '';
    } else {
      const codePoint = codePointAt(text, at, code);
      if (codePoint > 0xffff) width = 2;
      replacement = foldCodePoint(codePoint);
    }
    if (replacement !== '') {
      pieces.push(replacement);
      note(at, at + width, replacement.length);
    }
    at += width;
    runStart = at;
  }
  flush(length);
  return { text: pieces.join(''), starts: starts.done(), ends: ends.done() };
}

/**
 * The stretch of the source that the stretch `[from, to)` of a folded text came from. It runs from the
 * start of the first character to the end of the last one, so it takes in whole characters and the hidden
 * characters between them.
 */
export function sourceRange(folded: Folded, from: number, to: number): { start: number; end: number } {
  if (folded.starts === null || folded.ends === null) return { start: from, end: to };
  return { start: folded.starts[from], end: folded.ends[to - 1] };
}

/** `\` read as `/`, one unit for one: the separator of a path is one or the other. */
export function withSlashes(text: string): string {
  return text.includes('\\') ? text.split('\\').join('/') : text;
}
