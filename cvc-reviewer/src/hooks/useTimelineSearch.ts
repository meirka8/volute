import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from 'react';
import {
  activeFilterCount,
  buildFacets,
  effectiveFile,
  filterChips,
  getTimelineIndex,
  highlightTerms,
  NO_FACETS,
  NO_FILTERS,
  parseQuery,
  runSearch,
  sameFilters,
  withoutChip,
  withToggled,
  type CommitInfo,
  type Facets,
  type FilterChip,
  type FilterGroup,
  type Filters,
  type HighlightTerms,
} from '../lib/timelineSearch';
import type { ParentToolCall } from '../lib/transcriptNode';
import { EMPTY_SEARCH, readSearchState, viewLink, type SearchState } from '../lib/timelineSearchUrl';
import type { InteractionNode } from '../types/cvc';
import { useSearchUrlSync } from './useSearchUrl';
import type { ListControls, ScrollMemory } from './useWindowedList';

export interface TimelineSearchOptions {
  /** Every step, in the order the timeline draws them in. */
  steps: readonly InteractionNode[];
  /** The pull request's commits, oldest first, for the commit filter. */
  commits?: readonly CommitInfo[];
  /** The file open in the diff pane, which the file filter can mean. */
  openFile?: string | null;
  /** The list of steps on the page, to move the reader in it. */
  listControls: RefObject<ListControls | null>;
  /** What each step's parent called (see `parentToolNamesByNode`), which the index reads the stimulus of a step against. */
  parentToolNames?: ReadonlyMap<string, readonly ParentToolCall[] | null>;
}

/** What the search shows and does; see `useTimelineSearch`. */
export interface TimelineSearch {
  /** The text in the box. */
  query: string;
  filters: Filters;
  /** The query has a term (the box holds more than white space and a prefix still being typed). */
  hasQuery: boolean;
  /** A query or a filter is narrowing the steps. */
  active: boolean;
  /** The file the "touches" filter means right now (the open file, or the one a link named), if it is on and there is one. */
  file: string | null;
  /** The file open in the diff pane. */
  openFile: string | null;
  /** The steps to draw: all of them while nothing narrows them, else the ones that match. */
  steps: readonly InteractionNode[];
  /** How many steps match (all of them while nothing narrows them). */
  total: number;
  /** How many steps the timeline has, whatever the search leaves of them. */
  all: number;
  /** The step the search is on, while there is a query that matches anything. */
  currentId: string | null;
  /** Where it is among the matches, counting from 1; 0 when there is none. */
  position: number;
  /** Terms to mark on the cards, while there is a query. */
  highlight: HighlightTerms | null;
  facets: Facets;
  chips: readonly FilterChip[];
  /**
   * The search or the filters were taken from the address of the page (a link was opened, or the page was reloaded)
   * and the reader has not changed them since: the bar says so, and offers to clear them.
   */
  fromAddress: boolean;
  /** More different terms were typed than are used. */
  termsDropped: boolean;
  setQuery(query: string): void;
  clearQuery(): void;
  toggleFilter(group: FilterGroup, value: string): void;
  removeChip(chip: FilterChip): void;
  /** Turns the "touches the open file" filter on or off. */
  setFileFilter(on: boolean): void;
  clearFilters(): void;
  clearAll(): void;
  /** Moves to the next match (1) or the previous (-1), wrapping round, and brings it into view. */
  step(direction: 1 | -1): void;
  /** Says the person is about to search, so the index is built now rather than at the first key. */
  engage(): void;
  /** The link to this view: the address of the page with the filters, and with the search text if `withText`. */
  linkToView(withText: boolean): string;
}

const NO_IDS: readonly string[] = [];

/**
 * The search of the timeline (issue #118): the text and filters, what they leave, and where the reader
 * is in it.
 *
 * - The filters start from the fragment of the address of the page and are written back to it as they change
 *   (see `useSearchUrlSync`); the text of the search is never written to it. A search or filters that the address
 *   brought are applied, and the bar says so until the reader changes them. A fragment that changes while the page
 *   is open (a link pasted into this tab) is taken the same way.
 * - The index is built when it is first wanted: when the box or the filters are first used, or at once
 *   when the address brings a search. A timeline nobody searches pays nothing for it.
 * - While a query or a filter narrows the steps, the list is a different list, which opens at the top;
 *   the place the reader was at in the whole list is kept and is where it opens again when the last
 *   query and the last filter are gone.
 * - The step the search is "on" is the first match, and next and previous move it. It is not the selected step.
 *   Every change to the text or the filters puts it back on the first match of the new list (which opens at
 *   the top).
 */
export function useTimelineSearch({ steps, commits, openFile = null, listControls, parentToolNames }: TimelineSearchOptions): TimelineSearch {
  const [initial] = useState<SearchState>(() => (typeof window === 'undefined' ? EMPTY_SEARCH : readSearchState(window.location.hash)));
  const [state, setState] = useState<SearchState>(initial);
  const [fromAddress, setFromAddress] = useState(initial !== EMPTY_SEARCH);
  const [revision, setRevision] = useState(0);
  const [currentId, setCurrentId] = useState<string | null>(null);
  const [engaged, setEngaged] = useState(false);
  /** Where the reader was in the whole list when the search began. */
  const before = useRef<ScrollMemory | null>(null);

  const parsed = useMemo(() => parseQuery(state.q), [state.q]);
  const file = effectiveFile(state.filters.file, openFile);
  const hasQuery = parsed.terms.length > 0;
  const active = hasQuery || activeFilterCount(state.filters, openFile) > 0;

  useSearchUrlSync(state.filters, openFile, revision);

  const index = useMemo(() => (engaged || active ? getTimelineIndex(steps, parentToolNames) : null), [engaged, active, steps, parentToolNames]);
  const facets = useMemo(() => (index === null ? NO_FACETS : buildFacets(index, commits)), [index, commits]);
  const result = useMemo(
    () => (index === null || !active ? null : runSearch(index, { query: parsed, filters: state.filters, file })),
    [index, active, parsed, state.filters, file],
  );
  const shown = result === null ? steps : result.nodes;
  const ids = result === null ? NO_IDS : result.ids;
  const highlight = useMemo(() => highlightTerms(parsed), [parsed]);
  const chips = useMemo(() => filterChips(state.filters, facets, openFile), [state.filters, facets, openFile]);

  // The step the search is on: the one it was moved to if that still matches, else the first.
  const at = hasQuery && ids.length > 0 ? Math.max(0, currentId === null ? 0 : ids.indexOf(currentId)) : -1;
  const current = at >= 0 ? ids[at] : null;

  /**
   * Every change to the query or the filters goes through here, so that the reader's place is looked after in one spot.
   * `fromLink` is for a state the address brought; any other change is the reader's own.
   */
  const apply = useCallback(
    (next: SearchState, fromLink = false) => {
      const list = listControls.current;
      const nextActive = parseQuery(next.q).terms.length > 0 || activeFilterCount(next.filters, openFile) > 0;
      if (!active && nextActive) {
        before.current = list?.capture() ?? null;
        list?.scrollToTop();
      } else if (active && !nextActive) {
        list?.restore(before.current);
        before.current = null;
      } else if (nextActive) {
        // A different set of steps: it opens at the top.
        list?.scrollToTop();
      }
      setCurrentId(null);
      setState(next);
      setFromAddress(fromLink);
    },
    [active, openFile, listControls],
  );

  // A fragment that changes while the page is open (a link pasted into this tab, a step through the history)
  // is a link opened: its filters and text are applied, and its text leaves the address as it does at load.
  useEffect(() => {
    const onChange = () => {
      const next = readSearchState(window.location.hash);
      if (next === EMPTY_SEARCH) return;
      if (next.q !== state.q || !sameFilters(next.filters, state.filters)) apply(next, true);
      setRevision((value) => value + 1);
    };
    window.addEventListener('hashchange', onChange);
    return () => window.removeEventListener('hashchange', onChange);
  }, [apply, state]);

  // When the file that "the open file" means changes, the steps change under the reader: back to the top.
  const lastFile = useRef(file);
  useEffect(() => {
    if (lastFile.current === file) return;
    lastFile.current = file;
    if (state.filters.file.mode === 'open') listControls.current?.scrollToTop();
  }, [file, state.filters.file.mode, listControls]);

  const step = useCallback(
    (direction: 1 | -1) => {
      if (at < 0) return;
      const next = (at + direction + ids.length) % ids.length;
      setCurrentId(ids[next]);
      listControls.current?.scrollToIndex(next);
    },
    [at, ids, listControls],
  );

  return {
    query: state.q,
    filters: state.filters,
    hasQuery,
    active,
    file,
    openFile,
    steps: shown,
    total: shown.length,
    all: steps.length,
    currentId: current,
    position: at + 1,
    highlight,
    facets,
    chips,
    fromAddress,
    termsDropped: parsed.dropped === true,
    setQuery: (query) => apply({ q: query, filters: state.filters }),
    clearQuery: () => apply({ q: '', filters: state.filters }),
    toggleFilter: (group, value) => apply({ q: state.q, filters: withToggled(state.filters, group, value) }),
    removeChip: (chip) => apply({ q: state.q, filters: withoutChip(state.filters, chip) }),
    setFileFilter: (on) => apply({ q: state.q, filters: { ...state.filters, file: on ? { mode: 'open' } : { mode: 'off' } } }),
    clearFilters: () => apply({ q: state.q, filters: NO_FILTERS }),
    clearAll: () => apply(EMPTY_SEARCH),
    step,
    engage: () => setEngaged(true),
    linkToView: (withText) =>
      viewLink(
        { origin: window.location.origin, pathname: window.location.pathname, search: window.location.search, hash: window.location.hash },
        state,
        openFile,
        withText,
      ),
  };
}
