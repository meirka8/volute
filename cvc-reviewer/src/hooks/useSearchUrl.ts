import { useEffect, useState } from 'react';
import type { Filters } from '../lib/timelineSearch';
import { encodeFilters, withFragmentParams } from '../lib/timelineSearchUrl';

/**
 * Keeps the fragment of the page's address saying which filters the timeline has on (issue #118), so that a reload,
 * and a step back to the page, find them again. See `lib/timelineSearchUrl` for what the address holds and what it
 * never does: the text of the search is not in it, and a `q` that a link brought is taken out of it here.
 *
 * - The filters change by a person choosing one, so the address is written at once and replaces the entry in the
 *   history (there is no typing to wait out, and no entry of its own to make).
 * - It is written only while the page is still the one the timeline was drawn on. A timeline goes away when the
 *   router has already moved on to the next page, and its last write must not land on that page's address.
 * - It leaves the path, the query string and every parameter of the fragment that is not ours alone, and does
 *   nothing when the address already says it.
 *
 * `openFile` is the file the file filter means when it follows the open one. `revision` is raised by whoever has
 * just taken a state from the address (a link opened in this page): the address is then written again even though
 * the filters did not change, so that a `q` it brought leaves it.
 */
export function useSearchUrlSync(filters: Filters, openFile: string | null, revision = 0): void {
  const encoded = encodeFilters(filters, openFile);
  // The page the timeline was drawn on.
  const [mountedOn] = useState(() => (typeof window === 'undefined' ? '' : window.location.pathname));

  useEffect(() => {
    if (typeof window === 'undefined' || window.location.pathname !== mountedOn) return;
    const { pathname, search, hash } = window.location;
    const next = withFragmentParams(hash, encoded);
    // A bare `#` says nothing, and is not worth rewriting the address for.
    if (next === hash || (next === '' && hash === '#')) return;
    try {
      window.history.replaceState(window.history.state, '', `${pathname}${search}${next}`);
    } catch {
      // An address a page may not rewrite (a sandboxed frame): the view just is not in the address.
    }
  }, [encoded, mountedOn, revision]);
}
