import { createContext, Fragment, useCallback, useContext, useId, useLayoutEffect, useMemo, useState, type ReactNode } from 'react';
import { AlertTriangle } from 'lucide-react';
import { hiddenCodePointLabel, hiddenTitle, hiddenToken, splitHidden, type HiddenMode } from '../../lib/hiddenChars';
import { fold, type Folded } from '../../lib/searchFold';
import { foldForMarks, MAX_MARKS_PER_BLOCK, markRanges, type MarkRange } from '../../lib/searchMarks';
import type { QueryScope } from '../../lib/timelineSearch';
import { MarkBudgetContext, type MarkBudget } from './markBudget';
import { useHighlightTerms } from './searchHighlight';

/**
 * Hidden and bidirectional characters, drawn (issue #135). See `lib/hiddenChars`
 * for the class and the two levels (`strict` for code-like text, `prose` for text
 * a person or the model wrote).
 *
 * A card wraps its contents in a `HiddenCharsScope`. Every `HiddenText` that has a
 * hidden character to draw tells the scope, and the card's `HiddenCharsNotice`
 * shows while any of them is on screen. So the notice's sentence ("shown as
 * ⟨U+…⟩") is true whenever it is there, and text that is not drawn (a folded block,
 * a list not yet paged in) does not raise it.
 */

/** How a `HiddenText` tells its card that it is drawing a hidden character, and that it stopped. */
const ReportContext = createContext<((delta: 1 | -1) => void) | null>(null);
/** How many `HiddenText`s of the card are drawing one. */
const CountContext = createContext(0);

/** A warning-tinted monospace chip in the text, in the page's ink colour so it reads on both themes. */
const TOKEN_CLASS =
  'mx-px whitespace-nowrap rounded border border-warning/60 bg-warning/25 px-1 font-mono text-[0.85em] font-medium text-ink';

/** What a mark looks like is in index.css: it is a `<mark>` with this class, holding nothing but the text it marks. */
const MARK_CLASS = 'rr-mark';

/** `text` with the stretches in `ranges` in `<mark>` elements. A text with none is returned as it is. */
function marked(text: string, ranges: readonly MarkRange[], keyBase: string): ReactNode {
  if (ranges.length === 0) return text;
  const pieces: ReactNode[] = [];
  let at = 0;
  ranges.forEach((range, k) => {
    if (range.start > at) pieces.push(text.slice(at, range.start));
    pieces.push(
      <mark key={`${keyBase}${k}`} className={MARK_CLASS}>
        {text.slice(range.start, range.end)}
      </mark>,
    );
    at = range.end;
  });
  if (at < text.length) pieces.push(text.slice(at));
  return pieces;
}

const NO_RANGES: readonly MarkRange[] = [];

/** The marks of a text, run by run, and how many of them the card allowed. */
interface Marks {
  readonly ranges: ReadonlyArray<readonly MarkRange[]>;
  readonly granted: number;
  readonly denied: boolean;
}

/**
 * A plain run of a text, between the hidden characters that are drawn as tokens: what it says, and where in the whole text
 * it starts (-1 where that is not known: the runs after the summary of the hidden characters past the limit are one text
 * made of pieces of the text, and have no one place).
 */
interface Run {
  readonly text: string;
  readonly at: number;
}

/** What is searched in a text that is drawn in runs: the fold of the whole of it, and the fold of each run that has no place in it. */
interface Folds {
  readonly whole: Folded;
  readonly alone: ReadonlyArray<Folded | null>;
}

/** The part of `ranges` that is inside `[from, to)`, as ranges of that part. */
function within(ranges: readonly MarkRange[], from: number, to: number): MarkRange[] {
  const inside: MarkRange[] = [];
  for (const range of ranges) {
    const start = Math.max(range.start, from);
    const end = Math.min(range.end, to);
    if (start < end) inside.push({ start: start - from, end: end - from });
  }
  return inside;
}

/**
 * Where the terms are in each plain run of a text: as many marks as the block allows, and as the card has left (the
 * budget is asked, and what it grants is kept for this block: see `MarkBudget`). A match is looked for in the whole text,
 * so that one that runs across a hidden character is found, and is marked on either side of the token that is drawn for
 * it. `whole` marks each run from end to end, for a text that is only part of what matched (see `HiddenText`).
 */
function findMarks(
  runs: ReadonlyArray<Run | null>,
  folds: Folds,
  terms: readonly string[],
  whole: boolean,
  budget: MarkBudget | null,
  owner: string,
): Marks {
  // One more than the card has room for, so that a match that is left out is known to be there.
  const cap = budget === null ? MAX_MARKS_PER_BLOCK : Math.min(MAX_MARKS_PER_BLOCK, budget.room(owner) + 1);
  const matches = whole || cap <= 0 ? NO_RANGES : markRanges(folds.whole, terms, cap);
  const found: Array<readonly MarkRange[]> = [];
  let wanted = 0;
  runs.forEach((run, k) => {
    if (run === null || run.text === '') {
      found.push(NO_RANGES);
      return;
    }
    let ranges: readonly MarkRange[];
    if (whole) ranges = [{ start: 0, end: run.text.length }];
    else if (run.at >= 0) ranges = within(matches, run.at, run.at + run.text.length);
    else {
      const alone = folds.alone[k];
      ranges = alone === null || cap - wanted <= 0 ? NO_RANGES : markRanges(alone, terms, cap - wanted);
    }
    found.push(ranges);
    wanted += ranges.length;
  });
  if (budget === null) return { ranges: found, granted: wanted, denied: false };

  const granted = budget.claim(owner, wanted);
  if (granted >= wanted) return { ranges: found, granted, denied: false };
  let allowed = granted;
  const kept = found.map((ranges) => {
    const take = Math.min(ranges.length, allowed);
    allowed -= take;
    return take === ranges.length ? ranges : ranges.slice(0, take);
  });
  return { ranges: kept, granted, denied: true };
}

/**
 * Text with its hidden characters drawn as visible tokens. Text without any is
 * returned as it is, so it draws exactly as a plain string would: no wrapper, no
 * extra elements.
 *
 * While the timeline is searched, what the search matches is drawn in `<mark>` elements: text
 * nodes inside them and nothing else, so a mark can not carry anything but the text it marks.
 * The text is folded the way the index folded it (`lib/searchFold`), so a match is marked where
 * it is in the text as it is drawn, whatever the fold made of it: a word typed with the
 * non-joiner it is written with is marked whole, and the hidden characters inside a match are
 * inside its mark. A match that runs across a hidden character that is drawn as a token (the text
 * is two runs, with the token between them) is found (the search reads the text without those
 * characters), and is marked on either side of the token: the token is not inside a mark, which
 * holds text and nothing else, and the card's note about hidden characters says what it is.
 * No more than `MAX_MARKS_PER_BLOCK` marks are drawn in one block of text, and the card has a
 * budget of its own (see `MarkBudget`).
 *
 * - `scope` says the text can only be one field (a tool's name, a file path, a commit), which is
 *   what a term with a prefix marks; the separators of a path are `/` or `\`, as in the index;
 * - `marks={false}` is for text the search does not look in: the Reviewer's own words, and the
 *   names of arguments;
 * - `full` is the whole of what this text is a part of or another spelling of (a commit hash
 *   of which the card shows seven characters, the name a tool was recorded under where the card
 *   shows the name it gives it): a term that matches it and not this text marks this text whole,
 *   so a match in the part that is not drawn is not left unmarked.
 */
export function HiddenText({
  text,
  mode,
  scope,
  marks = true,
  full,
}: {
  text: string;
  mode: HiddenMode;
  scope?: QueryScope;
  marks?: boolean;
  full?: string;
}) {
  // Worked out once per text. A value that is not text is left for React to draw
  // (or to refuse, as it always did).
  const segments = useMemo(() => (typeof text === 'string' ? splitHidden(text, mode) : null), [text, mode]);
  const report = useContext(ReportContext);
  const found = segments !== null;
  const searched = useHighlightTerms(scope);
  const terms = marks && typeof text === 'string' ? searched : null;
  const searching = terms !== null;
  const budget = useContext(MarkBudgetContext);
  const owner = useId();

  // The plain runs of the text, and where each starts in it, once per text and not once per key typed.
  const runs = useMemo<ReadonlyArray<Run | null>>(() => {
    if (segments === null) return [{ text: typeof text === 'string' ? text : '', at: 0 }];
    let at = 0;
    return segments.map((segment) => {
      if (segment.kind === 'text') {
        const run = { text: segment.text, at };
        if (at >= 0) at += segment.text.length;
        return run;
      }
      if (segment.kind === 'hidden') {
        if (at >= 0) at += segment.codePoint > 0xffff ? 2 : 1;
        return null;
      }
      at = -1;
      return null;
    });
  }, [text, segments]);
  const folds = useMemo<Folds | null>(() => {
    if (!searching || typeof text !== 'string') return null;
    const forPath = scope === 'path';
    return {
      whole: foldForMarks(text, forPath),
      alone: runs.map((run) => (run === null || run.at >= 0 ? null : foldForMarks(run.text, forPath))),
    };
  }, [searching, text, runs, scope]);
  // The whole of this text is marked when the term is in what it is a part of and not in it.
  const whole = useMemo(() => {
    if (terms === null || full === undefined || typeof text !== 'string') return false;
    const drawn = fold(text);
    const other = fold(full);
    return terms.some((term) => other.includes(term) && !drawn.includes(term));
  }, [terms, full, text]);
  const result = useMemo(
    () => (terms === null || folds === null ? null : findMarks(runs, folds, terms, whole, budget, owner)),
    [terms, folds, runs, whole, budget, owner],
  );

  useLayoutEffect(() => {
    if (!found || report === null) return;
    report(1);
    return () => report(-1);
  }, [found, report]);

  // What the card's budget granted this block stays granted for as long as the block is on the page.
  useLayoutEffect(() => {
    if (budget === null || result === null) return;
    budget.hold(owner, result.granted, result.denied);
    budget.settle();
    return () => {
      budget.release(owner);
      budget.settle();
    };
  }, [budget, owner, result]);

  const drawn = (run: string, index: number, keyBase: string): ReactNode => (result === null ? run : marked(run, result.ranges[index], keyBase));
  if (segments === null) return result === null ? text : <>{drawn(text, 0, 'm')}</>;
  return (
    <>
      {segments.map((segment, index) => {
        if (segment.kind === 'text') return result === null ? segment.text : <Fragment key={index}>{drawn(segment.text, index, `m${index}-`)}</Fragment>;
        if (segment.kind === 'hidden') {
          const title = hiddenTitle(segment.codePoint);
          return (
            <span
              key={index}
              role="img"
              aria-label={title}
              title={title}
              data-hidden-char={hiddenCodePointLabel(segment.codePoint)}
              className={TOKEN_CLASS}
            >
              {hiddenToken(segment.codePoint)}
            </span>
          );
        }
        const summary = `${segment.count.toLocaleString('en-US')} more hidden characters, left out of this text`;
        return (
          <span key={index} role="img" aria-label={summary} title={summary} data-hidden-char="more" className={TOKEN_CLASS}>
            {`⟨+${segment.count.toLocaleString('en-US')} more⟩`}
          </span>
        );
      })}
    </>
  );
}

/** Counts the hidden characters a card is drawing. It adds no element of its own. */
export function HiddenCharsScope({ children }: { children: ReactNode }) {
  const [count, setCount] = useState(0);
  const report = useCallback((delta: 1 | -1) => setCount((current) => current + delta), []);
  return (
    <ReportContext.Provider value={report}>
      <CountContext.Provider value={count}>{children}</CountContext.Provider>
    </ReportContext.Provider>
  );
}

/** One line for the card, shown only while it is drawing a hidden character. */
export function HiddenCharsNotice() {
  const count = useContext(CountContext);
  if (count === 0) return null;
  return (
    <p
      role="note"
      className="mb-3 flex items-start gap-2 rounded-xl border border-warning/50 bg-warning/10 px-3 py-2 text-xs text-ink"
    >
      <AlertTriangle size={14} className="mt-0.5 shrink-0 text-warning" aria-hidden="true" />
      <span>This step contains hidden or bidirectional Unicode characters, shown as ⟨U+…⟩.</span>
    </p>
  );
}
