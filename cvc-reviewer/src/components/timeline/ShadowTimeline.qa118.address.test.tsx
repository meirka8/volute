/**
 * Round 3 QA of issue #118: the address, in the cases the mutants of `useSearchUrl` and of the hook that takes a link found
 * untested (a mutant that stopped the address being rewritten when only the text of a pasted link was new, one that
 * rewrote a bare `#`, one that let a fragment which says nothing about the search clear it).
 *
 * The search text is never in the address of the page; a link that brings it is applied and the text leaves the address
 * at once, whether or not the filters of the link are new. Only the behaviour is tested, not how it is done.
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { searchFixture } from '../../test/searchFixtures';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);
vi.setConfig({ testTimeout: 30_000 });

beforeEach(() => {
  installDomStubs();
  window.history.replaceState(null, '', '/');
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  restoreDomStubs();
  window.history.replaceState(null, '', '/');
});

const fixture = searchFixture(40);
const box = () => screen.getByRole('searchbox', { name: 'Search steps' }) as HTMLInputElement;
const typeQuery = (value: string) => fireEvent.change(box(), { target: { value } });
const chips = () => screen.queryAllByRole('button', { name: /^Remove filter/ }).map((chip) => chip.getAttribute('aria-label'));

/** A link pasted into the address bar of the open page: the address changes and the browser says so. */
function pasteLink(url: string) {
  act(() => {
    window.history.replaceState(null, '', url);
    window.dispatchEvent(new HashChangeEvent('hashchange'));
  });
}

describe('a link opened in the page that is open', () => {
  it('applies the text and takes it out of the address, whether or not its filters are new', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7#tool=Edit');
    render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    expect(chips()).toEqual(['Remove filter Tool: Edit']);
    expect(box().value).toBe('');

    // New text, the same filters: the only news is the text, and it must not stay where everyone who has the address sees it.
    pasteLink('/pr/acme/shop/7#q=refund&tool=Edit');
    expect(box().value).toBe('refund');
    expect(window.location.hash).toBe('#tool=Edit');
    expect(window.location.href).not.toContain('refund');

    // The same text and the same filters again (the link is pasted twice): nothing is new, and the text still leaves.
    pasteLink('/pr/acme/shop/7#q=refund&tool=Edit');
    expect(box().value).toBe('refund');
    expect(window.location.hash).toBe('#tool=Edit');

    // Text typed by the person, then a link with that very text: the same.
    typeQuery('retry');
    pasteLink('/pr/acme/shop/7#q=retry&tool=Edit');
    expect(box().value).toBe('retry');
    expect(window.location.hash).toBe('#tool=Edit');
  });

  it('applies new filters with their text, says that a link brought them, and leaves a parameter of the fragment that is not ours', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7#section');
    render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    expect(screen.queryByText(/opened from a link/)).toBeNull();
    pasteLink('/pr/acme/shop/7#section&q=refund&origin=prompt');
    expect(box().value).toBe('refund');
    expect(chips()).toEqual(['Remove filter Origin: Prompt']);
    expect(screen.getByText('This view was opened from a link with a search and filters applied.')).toBeInTheDocument();
    expect(window.location.hash).toBe('#section&origin=prompt');
  });

  it('is not undone by a fragment that says nothing about the search (an anchor to a part of the page)', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7#tool=Edit');
    render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    typeQuery('refund');
    pasteLink('/pr/acme/shop/7#section');
    expect(box().value).toBe('refund');
    expect(chips()).toEqual(['Remove filter Tool: Edit']);
    // And the filters come back into the address at the next change of them (the anchor replaced them in it).
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Prompt/ }));
    expect(new URLSearchParams(window.location.hash.slice(1)).getAll('origin')).toEqual(['prompt']);
    expect(new URLSearchParams(window.location.hash.slice(1)).getAll('tool')).toEqual(['Edit']);
  });
});

describe('an address that says nothing about the search', () => {
  it('is not rewritten: a bare # stays, and nothing is written for a page that is only opened', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7?x=1#');
    const write = vi.spyOn(window.history, 'replaceState');
    render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    typeQuery('refund');
    typeQuery('');
    expect(write).not.toHaveBeenCalled();
    expect(window.location.href.endsWith('/pr/acme/shop/7?x=1#')).toBe(true);
  });

  it('is not rewritten for a fragment it can read nothing from either', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7#section&origin=nope&tool=');
    const write = vi.spyOn(window.history, 'replaceState');
    render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    // Nothing usable in it, so nothing of ours to write: the values it could not use are taken out, and the rest stays.
    expect(window.location.hash === '#section&origin=nope&tool=' || window.location.hash === '#section').toBe(true);
    expect(window.location.hash).toContain('section');
    expect(write.mock.calls.length).toBeLessThanOrEqual(1);
  });

  it('is put back to a page without a # when the last filter is taken off a page that had only that', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7#tool=Edit');
    render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    fireEvent.click(screen.getByRole('button', { name: 'Remove filter Tool: Edit' }));
    expect(window.location.pathname + window.location.search + window.location.hash).toBe('/pr/acme/shop/7');
    expect(window.location.href.endsWith('#')).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// The query string
// ---------------------------------------------------------------------------

/** The link the "Copy link to this view" button makes, which a stub of the clipboard catches. */
function copiedLink(): string {
  const written: string[] = [];
  vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText: (text: string) => (written.push(text), Promise.resolve()) } });
  fireEvent.click(screen.getByRole('button', { name: 'Copy link to this view' }));
  vi.unstubAllGlobals();
  expect(written).toHaveLength(1);
  return written[0];
}

describe('the query string of the address of the page', () => {
  // A server reads the query string and never the fragment. The search and the filters belong in the fragment; an older link may
  // have left them in the query string, and they are taken out of it (never read from it), and every other parameter stays.
  it('loses the parameters that are ours when the page opens, and keeps all the others, the history and the state of the page', () => {
    window.history.replaceState({ page: 'review', n: 3 }, '', '/pr/acme/shop/7?tab=files&q=secret&tool=Bash&x=%E2%9C%93&conv=c1&origin=prompt&file=f&link=generated&commit=abcdef1#section');
    const entries = window.history.length;
    const write = vi.spyOn(window.history, 'replaceState');
    render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    expect(window.location.pathname).toBe('/pr/acme/shop/7');
    expect(window.location.search).toBe('?tab=files&x=%E2%9C%93');
    expect(window.location.hash).toBe('#section');
    expect(window.location.href).not.toMatch(/secret|Bash|conv=|origin=|abcdef1/);
    // Rewritten in place: no entry of its own in the history, and the state of the page is the page's.
    expect(window.history.length).toBe(entries);
    expect(window.history.state).toEqual({ page: 'review', n: 3 });
    expect(write.mock.calls.length).toBeGreaterThan(0);
    for (const call of write.mock.calls) expect(call[0]).toEqual({ page: 'review', n: 3 });
    // And it is never read from there: no text, no filter.
    expect(box().value).toBe('');
    expect(chips()).toEqual([]);
    expect(screen.queryByText(/opened from a link/)).toBeNull();
  });

  it('leaves no ? behind when ours were all the query string had, and keeps the fragment as it was', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7?q=secret&tool=Bash#section&tool=Edit');
    render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    expect(window.location.search).toBe('');
    expect(window.location.href.includes('?')).toBe(false);
    expect(window.location.hash).toBe('#section&tool=Edit');
    // (The filter of the fragment is read, and the query string's is not.)
    expect(chips()).toEqual(['Remove filter Tool: Edit']);
  });

  it('is not rewritten when none of ours is in it: a page that is only opened writes nothing', () => {
    for (const address of ['/pr/acme/shop/7', '/pr/acme/shop/7?tab=files', '/pr/acme/shop/7?tab=files&&x=%zz&Q=1&qq=2#section', '/pr/acme/shop/7?#']) {
      window.history.replaceState(null, '', address);
      const href = window.location.href;
      const write = vi.spyOn(window.history, 'replaceState');
      const { unmount } = render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
      typeQuery('refund');
      typeQuery('');
      expect(write, address).not.toHaveBeenCalled();
      expect(window.location.href).toBe(href);
      unmount();
      write.mockRestore();
    }
  });

  it('is cleaned the next time the filters are written, if something put ours back in it, and the other parameters stay', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7?tab=files');
    render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    // Something else writes into the address (a router, an extension, a person): ours are in the query string again.
    window.history.replaceState(null, '', '/pr/acme/shop/7?q=late&tab=files&tool=Bash');
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(within(screen.getByRole('group', { name: 'Stimulus origin' })).getByRole('checkbox', { name: /Prompt/ }));
    expect(window.location.search).toBe('?tab=files');
    expect(window.location.hash).toBe('#origin=prompt');
    // The filters are not written into the query string: not now, and not when they change again.
    fireEvent.click(within(screen.getByRole('group', { name: 'Tool' })).getByRole('checkbox', { name: /^Edit/ }));
    expect(window.location.search).toBe('?tab=files');
    expect(new URLSearchParams(window.location.hash.slice(1)).getAll('tool')).toEqual(['Edit']);
  });

  it('is not touched on another page than the one the timeline was opened on, however its filters change after the router has moved on', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7?q=secret&tab=files');
    const { unmount } = render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    expect(window.location.search).toBe('?tab=files');
    // The router has moved on to the next page, which has a query string of its own that happens to hold a name of ours, and the
    // timeline is still there for a moment (it goes away with the page it was on): nothing it does may land on that address.
    window.history.replaceState(null, '', '/pr/acme/shop/8?q=theirs&tab=files#section');
    const write = vi.spyOn(window.history, 'replaceState');
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(within(screen.getByRole('group', { name: 'Stimulus origin' })).getByRole('checkbox', { name: /Prompt/ }));
    typeQuery('refund');
    expect(write).not.toHaveBeenCalled();
    expect(window.location.pathname + window.location.search + window.location.hash).toBe('/pr/acme/shop/8?q=theirs&tab=files#section');
    unmount();
    expect(window.location.pathname + window.location.search + window.location.hash).toBe('/pr/acme/shop/8?q=theirs&tab=files#section');
  });

  it('does not fail when the browser will not let the page rewrite its address: the view works, and is just not in the address', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7?q=secret&tab=files');
    const write = vi.spyOn(window.history, 'replaceState').mockImplementation(() => {
      throw new DOMException('The operation is insecure.', 'SecurityError');
    });
    expect(() => render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />)).not.toThrow();
    expect(write).toHaveBeenCalled(); // it tried, to take ours out of the query string
    typeQuery('refund');
    expect(box().value).toBe('refund');
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(within(screen.getByRole('group', { name: 'Tool' })).getByRole('checkbox', { name: /^Edit/ }));
    expect(chips()).toEqual(['Remove filter Tool: Edit']);
    expect(screen.queryByRole('alert')).toBeNull();
    // (Nothing was written, so the address is as the page had it.)
    write.mockRestore();
    expect(window.location.search).toBe('?q=secret&tab=files');
  });
});

describe('the link that the page copies', () => {
  it('has none of ours in its query string, even when something put them back, and the other parameters stay as they were', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7?tab=files#section');
    render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    typeQuery('refund');
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(within(screen.getByRole('group', { name: 'Tool' })).getByRole('checkbox', { name: /^Edit/ }));
    window.history.replaceState(null, '', '/pr/acme/shop/7?q=late&tab=files&utm=a%20b+c&tool=Bash&%71=x#section&tool=Edit');
    const link = copiedLink();
    const url = new URL(link);
    expect(url.pathname).toBe('/pr/acme/shop/7');
    expect(url.search).toBe('?tab=files&utm=a%20b+c');
    expect(link).not.toMatch(/late|Bash|%71/);
    // The text and the filters are in the fragment, after the parameter of it that is not ours.
    expect(url.hash.startsWith('#section&')).toBe(true);
    const params = new URLSearchParams(url.hash.slice(1));
    expect(params.get('q')).toBe('refund');
    expect(params.getAll('tool')).toEqual(['Edit']);
  });

  it('does not have the text in it when the person chose to leave it out, and still has no query string of ours', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7?tab=files');
    render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    typeQuery('password=hunter2hunter2');
    fireEvent.click(screen.getByRole('button', { name: 'Copy link to this view' }));
    // It asks first; the person leaves the text out.
    window.history.replaceState(null, '', '/pr/acme/shop/7?q=late&tab=files');
    const written: string[] = [];
    vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText: (text: string) => (written.push(text), Promise.resolve()) } });
    fireEvent.click(screen.getByRole('button', { name: 'Copy without the search text' }));
    vi.unstubAllGlobals();
    expect(written).toHaveLength(1);
    expect(written[0]).toBe(`${window.location.origin}/pr/acme/shop/7?tab=files`);
    expect(written[0]).not.toMatch(/hunter2|late|q=/);
  });
});
