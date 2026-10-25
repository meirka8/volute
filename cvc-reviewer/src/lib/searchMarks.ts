/**
 * Where a search matches inside a piece of text drawn on the page (issue #118).
 *
 * The index (`timelineSearch`) answers "does this step match?" over text it has folded for the purpose.
 * This module answers "where, in the text as it is drawn?", so the matches can be marked in place. Pure
 * functions only: no React, no DOM, and no regular expression, since the text is wire data and the terms
 * are typed by a person.
 *
 * What lets the two agree is that they use the same fold (`lib/searchFold`): the text on the page is folded
 * the way the index folded it, the terms are looked for in it with `indexOf`, and a match in the folded
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

/**
 * The stretches of `block` (a text folded, or a text) that any of `terms` matches, in order, as many as
 * `max`: overlapping and touching matches are one stretch. `terms` are folded already (see `parseQuery`);
 * an empty term matches nothing. The stretches are in the text as it is drawn.
 *
 * The cost is one pass over the folded text per term plus a step for every occurrence, so a term of one
 * letter in a megabyte of that letter is a million steps and not more (and it stops at `max`).
 */
export function markRanges(block: Folded | string, terms: readonly string[], max: number = MAX_MARKS_PER_BLOCK): MarkRange[] {
  const folded = typeof block === 'string' ? foldForMarks(block) : block;
  const text = folded.text;
  const wanted = terms.filter((term) => term !== '');
  if (wanted.length === 0 || text === '' || max <= 0) return [];

  // Where each term occurs next, or -1 when it does not occur any more.
  const next = wanted.map((term) => text.indexOf(term));
  const ranges: MarkRange[] = [];
  let start = -1;
  let end = -1;
  for (;;) {
    let best = -1;
    let bestAt = Infinity;
    for (let k = 0; k < next.length; k++) {
      const at = next[k];
      if (at >= 0 && at < bestAt) {
        best = k;
        bestAt = at;
      }
    }
    if (best < 0) break;

    const found = sourceRange(folded, bestAt, bestAt + wanted[best].length);
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
    // One step on, not one term on: occurrences that overlap are the same stretch.
    next[best] = text.indexOf(wanted[best], bestAt + 1);
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
