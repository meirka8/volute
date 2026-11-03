import { useContext, useMemo, useSyncExternalStore, type ReactNode } from 'react';
import { undrawnMatches } from '../../lib/timelineSearch';
import type { InteractionNode } from '../../types/cvc';
import { MARKS_PER_CARD, MarkBudget, MarkBudgetContext } from './markBudget';
import { HighlightContext } from './searchHighlight';

/**
 * What a card says about a search, beyond the marks (issue #118). Each note is one line, shown only while
 * the timeline is searched and only when it is true, and none of them draws an element otherwise.
 */

const NOTE_CLASS = 'mb-3 rounded-xl border border-line bg-canvas/60 px-3 py-2 text-xs text-muted';

/** Gives the blocks of text of one card a budget of marks to share (see `MarkBudget`); a new search starts a new budget. */
export function MarkBudgetScope({ children }: { children: ReactNode }) {
  const search = useContext(HighlightContext);
  // A new budget for each set of terms: the marks drawn for one search are not the marks of the next.
  // eslint-disable-next-line react-hooks/exhaustive-deps -- `search` is not read, it is what makes the budget new
  const budget = useMemo(() => new MarkBudget(), [search]);
  return <MarkBudgetContext.Provider value={budget}>{children}</MarkBudgetContext.Provider>;
}

const NEVER = () => false;
const NO_UNSUBSCRIBE = () => () => {};

/** Says that the marks of the card stopped, once they have. */
function MarkBudgetNotice() {
  const budget = useContext(MarkBudgetContext);
  const ranOut = useSyncExternalStore(budget === null ? NO_UNSUBSCRIBE : budget.subscribe, budget === null ? NEVER : budget.getSnapshot, NEVER);
  if (!ranOut) return null;
  return (
    <p role="note" className={NOTE_CLASS}>
      {`Only the first ${MARKS_PER_CARD.toLocaleString('en-US')} matches on this card are marked.`}
    </p>
  );
}

/** Says where the step matched that the card does not show (the files in its context). */
function MatchedElsewhereNote({ interaction }: { interaction: InteractionNode }) {
  const search = useContext(HighlightContext);
  const where = useMemo(() => undrawnMatches(interaction, search), [interaction, search]);
  if (where.length === 0) return null;
  return (
    <p role="note" className={NOTE_CLASS}>
      {`Matched in: ${where.join(', ')}, which this card does not show.`}
    </p>
  );
}

/** The notes of a card, to put under its header. */
export function SearchNotes({ interaction }: { interaction: InteractionNode }) {
  return (
    <>
      <MatchedElsewhereNote interaction={interaction} />
      <MarkBudgetNotice />
    </>
  );
}
