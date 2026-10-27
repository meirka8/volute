/**
 * Where a search matches inside a piece of text drawn on the page (issue #118).
 *
 * The index (`timelineSearch`) answers "does this step match?" over text it has folded for the purpose.
 * This module answers "where, in the text as it is drawn?", so the matches can be marked in place. Pure
 * functions only: no React, no DOM, and no regular expression, since the text is wire data and the terms
 * are typed by a person.
 *
 * What lets the two agree is that they use the same fold (`lib/searchFold`): the text on the page is folded
 * the way the index folded it, the terms are looked for in it (see `TermScan`: the engine's `indexOf`, and a
 * scan that reads each character once where occurrences overlap), and a match in the folded
 * text becomes a stretch of the text on the page through the map the fold keeps of where each of its units
 * came from. So a term typed as the text is drawn (with the joiner of a Persian word, with the dot of a
 * Turkish capital I) is found and marked, a match is marked from the start of a character to the end of one,
 * and the hidden characters inside a match are inside its mark.
 */
import { foldWithMap, sourceRange, withSlashes, type Folded } from './searchFold';

/** A stretch of a text: `text.slice(start, end)`. */
export interface MarkRange {
  readonly start: number;
  readonly end: number;
}

/**
 * The most marks drawn in one block of text. A block is one text node of a card
 * (a command, a paragraph, one line of a diff), and a term such as `e` can match
 * thousands of times in a block of a megabyte; each mark is an element.
 */
export const MAX_MARKS_PER_BLOCK = 100;

/**
 * A text folded for marking. The fold of a text is worked out once, however many times the terms change;
 * a surface that shows a file path reads `\` as `/` (as the index does), which `forPath` does to it.
 */
export function foldForMarks(text: string, forPath = false): Folded {
  const folded = foldWithMap(text);
  return forPath ? { ...folded, text: withSlashes(folded.text) } : folded;
}

// ---------------------------------------------------------------------------
// Finding a term: every occurrence, in order, each character read once
// ---------------------------------------------------------------------------

/** For each place in `term`, how long the longest border of the term up to there is (the prefix function of the KMP search). */
function prefixFunction(term: string): Int32Array {
  const length = term.length;
  const border = new Int32Array(length);
  for (let at = 1, matched = 0; at < length; at++) {
    const code = term.charCodeAt(at);
    while (matched > 0 && code !== term.charCodeAt(matched)) matched = border[matched - 1];
    if (code === term.charCodeAt(matched)) matched++;
    border[at] = matched;
  }
  return border;
}

/** The prefix functions of the terms in use: a few, and each is worked out once (a term is typed, and then looked for in many blocks). */
const prefixFunctions = new Map<string, Int32Array>();
const MOST_PREFIX_FUNCTIONS = 64;
const LONGEST_CACHED_TERM = 1024;

function borders(term: string): Int32Array {
  let border = prefixFunctions.get(term);
  if (border === undefined) {
    border = prefixFunction(term);
    if (term.length <= LONGEST_CACHED_TERM) {
      if (prefixFunctions.size >= MOST_PREFIX_FUNCTIONS) prefixFunctions.clear();
      prefixFunctions.set(term, border);
    }
  }
  return border;
}

/** `TermScan.ahead` when nothing has been read ahead (-1 is taken: the text has no more occurrences). */
const NOTHING = -2;
const NO_BORDERS = new Int32Array(0);

/**
 * The places a term occurs in a text, in order, as runs: each is a stretch where occurrences overlap one another, which is
 * what the page marks as one (two that only touch are two). It reads the text once, left to right, in both of the ways a
 * search can:
 *
 * - where no occurrence is under way the engine's own `indexOf` finds the next one, as fast as it goes over text that has
 *   nothing like the term in it;
 * - where one has just been found, the part of it that can begin another occurrence is its longest border (a term that
 *   ends as it begins: `aa`, `abab`), and the scan carries on from there one character at a time (the KMP automaton),
 *   which reads each character of the text once however many occurrences overlap.
 *
 * So it takes time in proportion to the length of the text and of the term and to nothing else: a megabyte of `a` and a
 * term of five hundred of them is a million steps, where looking again from one character past each occurrence is five
 * hundred million.
 */
class TermScan {
  /** The run found last: where it starts and where it ends, in the folded text; `start` is -1 when there are no more. */
  start = -1;
  end = -1;
  private readonly text: string;
  private readonly term: string;
  /** How much of the text has been read, and the length of the partial occurrence that ends there (0 when none is under way). */
  private at = 0;
  private matched = 0;
  /** The prefix function of the term, once an occurrence has been found (see `borders`). */
  private border: Int32Array = NO_BORDERS;
  /** The start of an occurrence read ahead (the first of the next run), or `NOTHING`; -1 when the text has no more of them. */
  private ahead = NOTHING;

  constructor(text: string, term: string) {
    this.text = text;
    this.term = term;
  }

  /** The start of the next occurrence, overlapping ones included, or -1 when there are no more. */
  private occurrence(): number {
    const { text, term } = this;
    const length = text.length;
    const size = term.length;
    let at = this.at;
    let matched = this.matched;
    for (;;) {
      if (matched === 0) {
        // Nothing is under way: the engine finds the next occurrence.
        const found = at >= length ? -1 : text.indexOf(term, at);
        if (found < 0) {
          this.at = length;
          return -1;
        }
        // What of it can begin another occurrence is its longest border (none, for a term of one character).
        if (size > 1) {
          if (this.border === NO_BORDERS) this.border = borders(term);
          this.matched = this.border[size - 1];
        } else {
          this.matched = 0;
        }
        this.at = found + size;
        return found;
      }
      // Part of an occurrence is under way, as far as `matched` characters back from `at`: go on one character at a time,
      // falling back to a shorter border where the next character does not fit, until it is complete or has failed.
      const border = this.border;
      while (matched > 0) {
        if (at >= length) {
          this.at = length;
          this.matched = 0;
          return -1;
        }
        const code = text.charCodeAt(at);
        while (matched > 0 && code !== term.charCodeAt(matched)) matched = border[matched - 1];
        if (code === term.charCodeAt(matched)) matched++;
        at++;
        if (matched === size) {
          this.at = at;
          this.matched = border[size - 1];
          return at - size;
        }
      }
    }
  }

  /** Moves on to the next run: false (and `start` is -1) when there is none. */
  next(): boolean {
    const first = this.ahead !== NOTHING ? this.ahead : this.occurrence();
    this.ahead = NOTHING;
    if (first < 0) {
      this.start = -1;
      return false;
    }
    const size = this.term.length;
    let end = first + size;
    for (;;) {
      const at = this.occurrence();
      // Occurrences come in order of their starts, and are as long as each other, so each ends later than the one before.
      if (at >= 0 && at < end) end = at + size;
      else {
        this.ahead = at;
        break;
      }
    }
    this.start = first;
    this.end = end;
    return true;
  }
}

/**
 * The stretches of `block` (a text folded, or a text) that any of `terms` matches, in order, as many as
 * `max`: overlapping and touching matches are one stretch. `terms` are folded already (see `parseQuery`);
 * an empty term matches nothing. The stretches are in the text as it is drawn.
 *
 * The folded text is read once for each term, from the start, whatever the terms and the text are (see `TermScan`:
 * a text that is one letter over and over, and a term of five hundred of that letter, is a million steps), and a
 * stretch is mapped to the text as it is drawn once, not once for each occurrence in it. It stops reading when `max`
 * stretches are done, which a text with fewer separate matches than that does not do: a text that is one long run of
 * matches is read to its end (and only once).
 */
export function markRanges(block: Folded | string, terms: readonly string[], max: number = MAX_MARKS_PER_BLOCK): MarkRange[] {
  const folded = typeof block === 'string' ? foldForMarks(block) : block;
  const text = folded.text;
  if (text === '' || max <= 0) return [];
  const scans: TermScan[] = [];
  for (const term of terms) {
    if (term === '') continue;
    const scan = new TermScan(text, term);
    if (scan.next()) scans.push(scan);
  }
  if (scans.length === 0) return [];

  const ranges: MarkRange[] = [];
  // The stretch being built, in the text as it is drawn; -1 for none.
  let start = -1;
  let end = -1;
  for (;;) {
    // The run that starts first, and every run of another term (or the next of its own) that overlaps it, as one run.
    let first: TermScan | null = null;
    for (const scan of scans) if (scan.start >= 0 && (first === null || scan.start < first.start)) first = scan;
    if (first === null) break;
    const runStart = first.start;
    let runEnd = first.end;
    first.next();
    for (let changed = true; changed; ) {
      changed = false;
      for (const scan of scans) {
        if (scan.start >= 0 && scan.start < runEnd) {
          if (scan.end > runEnd) runEnd = scan.end;
          scan.next();
          changed = true;
        }
      }
    }

    const found = sourceRange(folded, runStart, runEnd);
    if (start >= 0 && found.start <= end) {
      if (found.end > end) end = found.end;
    } else {
      if (start >= 0) ranges.push({ start, end });
      if (ranges.length >= max) {
        start = -1;
        break;
      }
      start = found.start;
      end = found.end;
    }
  }
  if (start >= 0) ranges.push({ start, end });
  return ranges;
}

/**
 * How many separate matches `block` holds, counting up to `cap`: what a fold reports
 * ("3 matches"). Touching and overlapping matches count once, as they are drawn once;
 * the cap on marks drawn in a block (`MAX_MARKS_PER_BLOCK`) does not limit this.
 */
export function countMarks(block: Folded | string, terms: readonly string[], cap: number = 1000): number {
  return markRanges(block, terms, cap).length;
}
