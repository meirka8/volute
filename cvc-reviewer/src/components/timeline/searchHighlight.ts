import { createContext, useContext, useMemo } from 'react';
import type { Folded } from '../../lib/searchFold';
import { countMarks, foldForMarks } from '../../lib/searchMarks';
import type { CountedText, HighlightTerms, QueryScope } from '../../lib/timelineSearch';

/**
 * What the search is looking for, for the text on the page to mark (issue #118).
 *
 * The timeline provides the terms of the query while there is one; `HiddenText`, which every
 * piece of text a step shows goes through, marks them, and the controls that fold text away
 * count them. A card drawn with no search around it gets `null` here and draws exactly what
 * it always drew.
 */
export const HighlightContext = createContext<HighlightTerms | null>(null);

/**
 * The terms to mark in a text, or null for none. A text is of a kind (`scope`) when it can only
 * be one field: a tool's name, a file path, a commit; a term with a prefix marks only a text of
 * its kind, and a term without marks any text.
 */
export function useHighlightTerms(scope?: QueryScope): readonly string[] | null {
  const terms = useContext(HighlightContext);
  return terms === null ? null : terms[scope ?? 'plain'];
}

/** Whether the timeline is being searched for something: a card that draws its text in parts only needs to while it is. */
export function useIsSearching(): boolean {
  return useContext(HighlightContext) !== null;
}

/** Counting stops here; a fold says "1000+" past it. */
export const MATCH_COUNT_CAP = 1000;

/** The most text a control folds to count what it holds, whatever a step holds: past it the rest is not counted. */
const MAX_COUNTED_CHARS = 4 * 1024 * 1024;

/** A text to count in, folded once (not once per key), and the surface it is drawn on. */
interface FoldedText {
  readonly block: Folded;
  readonly scope: QueryScope | undefined;
}

function foldAll(source: string | readonly CountedText[], scope: QueryScope | undefined, into: FoldedText[], chars: { left: number }): void {
  for (const entry of typeof source === 'string' ? [source] : source) {
    const text = typeof entry === 'string' ? entry : entry.text;
    if (chars.left <= 0) return;
    chars.left -= text.length;
    const entryScope = typeof entry === 'string' ? scope : entry.scope;
    into.push({ block: foldForMarks(text, entryScope === 'path'), scope: entryScope });
  }
}

/** How many matches the folded texts hold, counting up to the cap. */
function countIn(folded: readonly FoldedText[], terms: HighlightTerms): number {
  let count = 0;
  for (const { block, scope } of folded) {
    const wanted = terms[scope ?? 'plain'];
    if (wanted !== null) count += countMarks(block, wanted, MATCH_COUNT_CAP - count);
    if (count >= MATCH_COUNT_CAP) break;
  }
  return count;
}

/**
 * How many matches the text of a fold holds, for the fold to say so. Zero when nothing is being
 * searched or when `enabled` is false (a fold that is open shows its marks instead). The text
 * must be the same string, array or function until it changes: it is folded once for it, and the
 * terms are looked for in the fold each time they change.
 */
export function useMatchCount(texts: string | readonly CountedText[] | (() => readonly CountedText[]), enabled = true, scope?: QueryScope): number {
  const terms = useContext(HighlightContext);
  const searching = enabled && terms !== null;
  const folded = useMemo(() => {
    if (!searching) return null;
    // A function is called only now that there is something to count, so a fold costs nothing while nothing is searched.
    const out: FoldedText[] = [];
    foldAll(typeof texts === 'function' ? texts() : texts, scope, out, { left: MAX_COUNTED_CHARS });
    return out;
  }, [searching, texts, scope]);
  return useMemo(() => (folded === null || terms === null ? 0 : countIn(folded, terms)), [folded, terms]);
}

/**
 * How many matches the items of a paged list that are not drawn yet hold, for the control that draws more of them to say
 * so. `items` and `textsOf` must be the same until they change. Zero when nothing is searched or everything is drawn.
 */
export function useHiddenMatches<T>(items: readonly T[], visible: number, textsOf: (item: T) => string | readonly CountedText[]): number {
  const terms = useContext(HighlightContext);
  const searching = terms !== null && visible < items.length;
  const folded = useMemo(() => {
    if (!searching) return null;
    const out: FoldedText[] = [];
    const chars = { left: MAX_COUNTED_CHARS };
    for (let at = visible; at < items.length && chars.left > 0; at++) foldAll(textsOf(items[at]), undefined, out, chars);
    return out;
  }, [searching, items, visible, textsOf]);
  return useMemo(() => (folded === null || terms === null ? 0 : countIn(folded, terms)), [folded, terms]);
}

/** The words a fold adds to its label: " · 3 matches", or nothing. */
export function matchesLabel(count: number): string {
  if (count <= 0) return '';
  if (count >= MATCH_COUNT_CAP) return ` · ${MATCH_COUNT_CAP}+ matches`;
  return ` · ${count} ${count === 1 ? 'match' : 'matches'}`;
}
