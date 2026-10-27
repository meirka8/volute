/**
 * The address the timeline keeps (issue #118): the filters in the fragment, written at once, and our names taken out of the
 * query string when the page opens, so that a search left there by an older link is not kept in the address and the history
 * and is not what a server is sent. The rest of the address is left as it was.
 */
import { renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { NO_FILTERS, type Filters } from '../lib/timelineSearch';
import { useSearchUrlSync } from './useSearchUrl';

const only = (partial: Partial<Filters>): Filters => ({ ...NO_FILTERS, ...partial });
const address = () => window.location.pathname + window.location.search + window.location.hash;

beforeEach(() => {
  window.history.replaceState(null, '', '/pr/o/r/1');
});
afterEach(() => {
  vi.restoreAllMocks();
  window.history.replaceState(null, '', '/');
});

describe('the query string of the address when the timeline opens', () => {
  it('loses a search a link left in it, at once, in the entry it was in', () => {
    window.history.replaceState({ marker: 1 }, '', '/pr/o/r/1?q=password%3Dhunter2hunter2');
    const length = window.history.length;
    const push = vi.spyOn(window.history, 'pushState');
    renderHook(() => useSearchUrlSync(NO_FILTERS, null));
    expect(address()).toBe('/pr/o/r/1');
    expect(window.history.length).toBe(length);
    expect(window.history.state).toEqual({ marker: 1 });
    expect(push).not.toHaveBeenCalled();
  });

  it('loses every one of our names, and keeps the other parameters as they were, and the fragment', () => {
    window.history.replaceState(null, '', '/pr/o/r/1?tab=files&q=x&conv=abc&origin=prompt&tool=Bash&link=temporal&commit=abcdef0&file=a.ts&utm=x%20y#top');
    renderHook(() => useSearchUrlSync(NO_FILTERS, null));
    expect(address()).toBe('/pr/o/r/1?tab=files&utm=x%20y#top');
  });

  it('does it in the same write as the filters', () => {
    window.history.replaceState(null, '', '/pr/o/r/1?tab=files&q=x&tool=Read#top');
    const replace = vi.spyOn(window.history, 'replaceState');
    renderHook(() => useSearchUrlSync(only({ tool: ['Bash'] }), null));
    expect(replace).toHaveBeenCalledTimes(1);
    expect(address()).toBe('/pr/o/r/1?tab=files#top&tool=Bash');
  });

  it('leaves an address with none of our names in its query string alone, and does not write it', () => {
    window.history.replaceState(null, '', '/pr/o/r/1?tab=files&&x=1&Q=upper#top');
    const replace = vi.spyOn(window.history, 'replaceState');
    renderHook(() => useSearchUrlSync(NO_FILTERS, null));
    expect(replace).not.toHaveBeenCalled();
    expect(address()).toBe('/pr/o/r/1?tab=files&&x=1&Q=upper#top');
  });

  it('does not read any of it: a name in the query string is not a filter', () => {
    window.history.replaceState(null, '', '/pr/o/r/1?tool=Bash&origin=prompt');
    renderHook(() => useSearchUrlSync(NO_FILTERS, null));
    expect(address()).toBe('/pr/o/r/1');
  });

  it('tidies the query string once, and not again for the filters that follow', () => {
    window.history.replaceState(null, '', '/pr/o/r/1?q=x');
    const replace = vi.spyOn(window.history, 'replaceState');
    const { rerender } = renderHook(({ filters }) => useSearchUrlSync(filters, null), { initialProps: { filters: NO_FILTERS } });
    expect(replace).toHaveBeenCalledTimes(1);
    rerender({ filters: only({ tool: ['Bash'] }) });
    expect(replace).toHaveBeenCalledTimes(2);
    expect(address()).toBe('/pr/o/r/1#tool=Bash');
    // The same filters again: the address already says it.
    rerender({ filters: only({ tool: ['Bash'] }) });
    expect(replace).toHaveBeenCalledTimes(2);
  });

  it('writes a query string a later link brought nothing to, again, when the revision says a link was opened', () => {
    const { rerender } = renderHook(({ revision }) => useSearchUrlSync(NO_FILTERS, null, revision), { initialProps: { revision: 0 } });
    // Another link, opened in this page, with a search in its query string (and one in its fragment).
    window.history.replaceState(null, '', '/pr/o/r/1?q=again&tab=files#q=again');
    rerender({ revision: 1 });
    expect(address()).toBe('/pr/o/r/1?tab=files');
  });

  it('does not touch the address of the page that follows, which the router has already moved to', () => {
    const { rerender } = renderHook(({ filters }) => useSearchUrlSync(filters, null), { initialProps: { filters: NO_FILTERS } });
    window.history.pushState(null, '', '/pr/o/r/2?q=other');
    rerender({ filters: only({ tool: ['Bash'] }) });
    expect(address()).toBe('/pr/o/r/2?q=other');
  });

  it('survives an address a page may not rewrite', () => {
    window.history.replaceState(null, '', '/pr/o/r/1?q=x');
    vi.spyOn(window.history, 'replaceState').mockImplementation(() => {
      throw new DOMException('not allowed', 'SecurityError');
    });
    expect(() => renderHook(() => useSearchUrlSync(only({ tool: ['Bash'] }), null))).not.toThrow();
  });
});
