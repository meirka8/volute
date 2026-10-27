import type { TimelineSearch } from '../../hooks/useTimelineSearch';

/** The placeholder of the search box, which also says how to narrow a term to a field. */
export const SEARCH_PLACEHOLDER = 'Search steps — path: tool: commit:';

const count = (value: number) => value.toLocaleString('en-US');

/**
 * What the count beside the arrows says. While anything narrows the list it says how many of the steps the list
 * shows ("12 of 300 steps"), so that a search or a filter that came with a link can not hide steps without saying so;
 * and, while there is a query, where the reader is among the matches ("3 of 12") in front of it.
 */
export function countText({ hasQuery, active, position, total, all }: Pick<TimelineSearch, 'hasQuery' | 'active' | 'position' | 'total' | 'all'>): string {
  if (!active) return '';
  const steps = `${count(total)} of ${count(all)} ${all === 1 ? 'step' : 'steps'}`;
  if (!hasQuery) return steps;
  return total === 0 ? `No matches · ${steps}` : `${count(position)} of ${count(total)} · ${steps}`;
}
