/**
 * Independent QA of issue #118 at the level of the page (src/pages/PRReview.tsx): the timeline is wired to the open file,
 * to the commits of the pull request and to the page's own keys; a search is about one pull request and the next one
 * starts without it; the address and the history behave across navigation. The hooks that fetch are replaced; the router,
 * the layout, the sidebar, the timeline and the keys are the real ones.
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Route, Router } from 'wouter';
import { navigate } from 'wouter/use-browser-location';
import { PRReviewPage } from './PRReview';
import { usePR } from '../hooks/usePR';
import { usePRInteractions } from '../hooks/usePRInteractions';
import { installDomStubs, restoreDomStubs } from '../test/domStubs';
import { generatedLink, makeNode } from '../test/fixtures';
import { FIXTURE_PATH, searchFixture } from '../test/searchFixtures';
import type { InteractionNode } from '../types/cvc';

vi.mock('../hooks/usePR', () => ({ usePR: vi.fn() }));
vi.mock('../hooks/usePRInteractions', () => ({ usePRInteractions: vi.fn() }));
vi.mock('../auth/AuthContext', () => ({ useAuth: () => ({ logout: () => {} }) }));
vi.mock('../components/diff/DiffViewer', () => ({
  DiffViewer: ({ filename }: { filename: string }) => <div data-testid="diff">{filename}</div>,
}));
vi.mock('framer-motion', async () => {
  const React = await import('react');
  const ANIMATION = ['layout', 'initial', 'animate', 'exit', 'transition', 'whileHover', 'whileTap'];
  const made = new Map<string, unknown>();
  const motion = new Proxy(
    {},
    {
      get: (_target, tag: string) => {
        if (!made.has(tag)) {
          made.set(
            tag,
            React.forwardRef((props: Record<string, unknown>, ref: React.Ref<unknown>) => {
              const dom: Record<string, unknown> = { ...props };
              for (const name of ANIMATION) delete dom[name];
              return React.createElement(tag, { ...dom, ref });
            }),
          );
        }
        return made.get(tag);
      },
    },
  );
  return { motion, AnimatePresence: ({ children }: { children?: React.ReactNode }) => React.createElement(React.Fragment, null, children) };
});

// ---------------------------------------------------------------------------
// Two pull requests
// ---------------------------------------------------------------------------

const ONE = searchFixture(40);
/** A second pull request's steps: different ids, and its own words. */
const TWO_NODES: InteractionNode[] = Array.from({ length: 12 }, (_, k) =>
  makeNode({
    id: `2f0e5c3a-9d2b-8c4e-a1b7-${(k + 1).toString(16).padStart(12, '0')}`,
    conversation_id: 'second-session',
    author: 'human',
    timestamp: 1_800_000_000_000 + k * 1000,
    model_name: 'claude-opus-4-5-20251101',
    user_prompt: `Second pull request step ${k} about invoices${k === 5 ? ' and refund_handler' : ''}`,
    model_response: 'Done.',
    artifact_links: [generatedLink(`2f0e5c3a-9d2b-8c4e-a1b7-${(k + 1).toString(16).padStart(12, '0')}`, 'feedfacefeedfacefeedfacefeedfacefeedface')],
  }),
);

const FILES: Record<number, string[]> = {
  1: [FIXTURE_PATH, 'src/cart/index.ts', 'docs/README.md'],
  2: ['docs/INVOICES.md', 'src/cart/index.ts'],
};
const DATA: Record<number, { interactions: InteractionNode[]; commits: { sha: string; subject?: string }[] }> = {
  1: { interactions: ONE.nodes, commits: ONE.commits },
  2: { interactions: TWO_NODES, commits: [{ sha: 'feedfacefeedfacefeedfacefeedfacefeedface', subject: 'feat: invoices' }] },
};

const interactionCalls: Array<[string, string, number]> = [];

function installHooks() {
  interactionCalls.length = 0;
  vi.mocked(usePR).mockImplementation(((owner: string, repo: string, pr: number) => ({
    data: { pr: { title: `Pull request ${pr} of ${owner}/${repo}` }, files: (FILES[pr] ?? []).map((filename) => ({ filename, status: 'modified', additions: 1, deletions: 0, patch: '@@ -1 +1 @@\n-a\n+b' })) },
    isLoading: false,
    error: null,
  })) as unknown as typeof usePR);
  vi.mocked(usePRInteractions).mockImplementation(((owner: string, repo: string, pr: number) => {
    interactionCalls.push([owner, repo, pr]);
    return { data: { ...DATA[pr], truncated: false, hasHistory: true }, isLoading: false, error: null };
  }) as unknown as typeof usePRInteractions);
}

// Drawing cards in jsdom is slow on a slow core: none of these should fail by the clock.
vi.setConfig({ testTimeout: 30_000 });

const consoleProblems: string[] = [];

beforeEach(() => {
  consoleProblems.length = 0;
  // A warning from React (a key, an update during render, a state update outside act) is a defect of the feature.
  vi.spyOn(console, 'error').mockImplementation((...args: unknown[]) => void consoleProblems.push(args.map(String).join(' ').slice(0, 300)));
  vi.spyOn(console, 'warn').mockImplementation((...args: unknown[]) => void consoleProblems.push(args.map(String).join(' ').slice(0, 300)));
  installDomStubs();
  installHooks();
  // jsdom has no matchMedia, and the layout asks it about the width of the window.
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener: () => {}, removeEventListener: () => {}, addListener: () => {}, removeListener: () => {} }));
  // cmdk scrolls the selected item of its list into view.
  Element.prototype.scrollIntoView = () => {};
  window.history.replaceState(null, '', '/');
});
afterEach(() => {
  const problems = [...consoleProblems];
  cleanup();
  vi.useRealTimers();
  restoreDomStubs();
  window.history.replaceState(null, '', '/');
  expect(problems).toEqual([]);
});

function page() {
  return render(
    <Router>
      <Route path="/pr/:owner/:repo/:pr">
        <PRReviewPage />
      </Route>
    </Router>,
  );
}

const box = () => screen.getByRole('searchbox', { name: 'Search steps' }) as HTMLInputElement;
const typeQuery = (value: string) => fireEvent.change(box(), { target: { value } });
const count = () => screen.getByRole('status').textContent;
const diffOf = () => screen.getByTestId('diff').textContent;
const timelineCards = () => Array.from(document.querySelectorAll<HTMLElement>('ol[aria-label="Interactions linked to this PR"] > li'));
const go = (to: string) => act(() => navigate(to));
/** Round 3: the filters are in the fragment of the address (and the search text is not). */
const fragment = () => new URLSearchParams(window.location.hash.slice(1));
const settle = () => act(() => void vi.advanceTimersByTime(300));
/** Whether the sidebar says the file is viewed: the button of its row, by the title of the row's name. */
const viewedLabel = (path: string) => screen.getByTitle(path).closest('div')!.querySelector('button')!.getAttribute('aria-label');
const markViewedButtons = () => screen.getAllByRole('button', { name: /^Mark as (un)?viewed$/ }).map((button) => button.getAttribute('aria-label'));

// ---------------------------------------------------------------------------
// The page is wired to the timeline
// ---------------------------------------------------------------------------

describe('the page and the timeline', () => {
  it('hands the timeline the pull request\'s own steps and commits, and asks for them once however much is searched', () => {
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    window.history.replaceState(null, '', '/pr/acme/shop/1');
    const fetchSpy = vi.fn(() => Promise.reject(new Error('no network')));
    vi.stubGlobal('fetch', fetchSpy);
    page();
    expect(screen.getByText('40 interactions linked to this PR')).toBeInTheDocument();
    expect(screen.getByText('Pull request 1 of acme/shop')).toBeInTheDocument();
    const before = interactionCalls.length;
    typeQuery('refund');
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    // The commits of the pull request, newest first, with their subjects.
    const commits = within(screen.getByRole('group', { name: 'Commit' })).getAllByRole('checkbox');
    expect(commits).toHaveLength(5);
    expect(commits[0].closest('label')).toHaveTextContent('chore: tidy the shipping client');
    fireEvent.click(commits[4]);
    settle();
    // Every render of the page asks its hook with the same arguments; the search is not a reason to ask for anything else.
    expect(before).toBeGreaterThanOrEqual(1);
    expect(new Set(interactionCalls.map((call) => call.join('|')))).toEqual(new Set(['acme|shop|1']));
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it('means, by "touches the open file", the file open in the diff, and follows it as j and k move through the files', () => {
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    window.history.replaceState(null, '', '/pr/acme/shop/1');
    page();
    expect(diffOf()).toBe(FIXTURE_PATH);
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Touches the open file/ }));
    const onFirst = count();
    expect(onFirst).toMatch(/^\d+ of 40 steps$/);
    expect(screen.getByRole('button', { name: `Remove filter File: ${FIXTURE_PATH}` })).toBeInTheDocument();
    settle();
    expect(fragment().get('file')).toBe(FIXTURE_PATH);

    fireEvent.keyDown(document.body, { key: 'j' });
    expect(diffOf()).toBe('src/cart/index.ts');
    expect(screen.getByRole('button', { name: 'Remove filter File: src/cart/index.ts' })).toBeInTheDocument();
    settle();
    expect(fragment().get('file')).toBe('src/cart/index.ts');
    fireEvent.keyDown(document.body, { key: 'k' });
    expect(diffOf()).toBe(FIXTURE_PATH);
    expect(count()).toBe(onFirst);
  });

  it('keeps its own keys: Space marks the open file viewed, j and k move, "/" comes to the box, and typing in the box is typing', () => {
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    window.history.replaceState(null, '', '/pr/acme/shop/1');
    page();
    expect(markViewedButtons()).toEqual(['Mark as viewed', 'Mark as viewed', 'Mark as viewed']);
    fireEvent.keyDown(document.body, { key: ' ' });
    expect(viewedLabel(FIXTURE_PATH)).toBe('Mark as unviewed');
    expect(markViewedButtons().filter((label) => label === 'Mark as unviewed')).toHaveLength(1);
    fireEvent.keyDown(document.body, { key: ' ' });
    expect(markViewedButtons()).toEqual(['Mark as viewed', 'Mark as viewed', 'Mark as viewed']);

    // "/" comes to the box of the timeline; the keys typed there are the box's.
    expect(fireEvent.keyDown(document.body, { key: '/' })).toBe(false);
    expect(document.activeElement).toBe(box());
    for (const key of ['j', 'k', ' ', 'x']) fireEvent.keyDown(box(), { key });
    expect(diffOf()).toBe(FIXTURE_PATH);
    expect(markViewedButtons()).toEqual(['Mark as viewed', 'Mark as viewed', 'Mark as viewed']);
    // Escape leaves the box; the page's keys work again.
    fireEvent.keyDown(box(), { key: 'Escape' });
    expect(document.activeElement).not.toBe(box());
    fireEvent.keyDown(document.body, { key: 'j' });
    expect(diffOf()).toBe('src/cart/index.ts');
  });

  it('does not let Space on a card, or on a control of the search, mark a file viewed', () => {
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    window.history.replaceState(null, '', '/pr/acme/shop/1');
    page();
    typeQuery('refund');
    const header = timelineCards()[0].querySelector<HTMLElement>('button[aria-pressed]')!;
    for (const target of [header, screen.getByRole('button', { name: 'Next match' }), screen.getByRole('button', { name: /^Filters/ })]) {
      target.focus();
      expect(fireEvent.keyDown(target, { key: ' ' })).toBe(true);
    }
    expect(markViewedButtons()).toEqual(['Mark as viewed', 'Mark as viewed', 'Mark as viewed']);
  });

  it('selects a card while it is found, and the selection is the page\'s, not the search\'s', () => {
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    window.history.replaceState(null, '', '/pr/acme/shop/1');
    page();
    typeQuery('src');
    const [first, second] = timelineCards();
    fireEvent.click(first.querySelector('button[aria-pressed]')!);
    expect(first.querySelector('button[aria-pressed]')).toHaveAttribute('aria-pressed', 'true');
    // Moving the outline does not move the selection.
    fireEvent.keyDown(box(), { key: 'Enter' });
    expect(timelineCards()[0].querySelector('button[aria-pressed]')).toHaveAttribute('aria-pressed', 'true');
    expect(second.querySelector('button[aria-pressed]')).toHaveAttribute('aria-pressed', 'false');
    typeQuery('');
    expect(timelineCards().some((card) => card.querySelector('button[aria-pressed="true"]') !== null)).toBe(true);
  });

  it('the command palette keeps its own box: "/" typed into it is a character, and Ctrl+K still opens it', () => {
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    window.history.replaceState(null, '', '/pr/acme/shop/1');
    page();
    fireEvent.keyDown(document.body, { key: 'k', ctrlKey: true });
    const palette = screen.getByRole('combobox');
    palette.focus();
    expect(fireEvent.keyDown(palette, { key: '/' })).toBe(true);
    expect(document.activeElement).toBe(palette);
    // The page's file keys are off while the palette is open.
    fireEvent.keyDown(document.body, { key: 'j' });
    expect(diffOf()).toBe(FIXTURE_PATH);
  });
});

describe('the commands of the page that open another page', () => {
  it('opens the pull request on GitHub in a new tab that cannot reach back to this one', () => {
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    window.history.replaceState(null, '', '/pr/acme/shop/1');
    const open = vi.fn(() => null);
    vi.stubGlobal('open', open);
    page();
    fireEvent.keyDown(document.body, { key: 'k', ctrlKey: true });
    fireEvent.click(screen.getByText('Open PR on GitHub'));
    expect(open).toHaveBeenCalledTimes(1);
    const [url, target, features] = open.mock.calls[0] as unknown as [string, string, string | undefined];
    expect(url).toBe('https://github.com/acme/shop/pull/1');
    expect(target).toBe('_blank');
    // The new page gets no handle on this one (window.opener), and no referrer that carries this page's address.
    const words = String(features ?? '').split(/[,\s]+/);
    expect(words).toContain('noopener');
    expect(words).toContain('noreferrer');
  });
});

// ---------------------------------------------------------------------------
// One pull request, then another
// ---------------------------------------------------------------------------

describe('navigating between pull requests', () => {
  it('starts the next one without the search, the filters or the folds of the last, and without them in its address', () => {
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    window.history.replaceState(null, '', '/pr/acme/shop/1?tab=files');
    page();
    typeQuery('refund_handler');
    const open = timelineCards()[0].querySelector<HTMLElement>('button[aria-expanded="false"]');
    expect(open).not.toBeNull();
    fireEvent.click(open!);
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(within(screen.getByRole('group', { name: 'Stimulus origin' })).getAllByRole('checkbox')[0]);
    settle();
    // Round 3: the filters are in the fragment, the search text is nowhere in the address, and the query string is not touched.
    expect(window.location.hash).toContain('origin=');
    expect(window.location.href).not.toContain('refund_handler');
    expect(window.location.search).toBe('?tab=files');

    go('/pr/acme/shop/2');
    expect(window.location.pathname).toBe('/pr/acme/shop/2');
    // Another pull request, another timeline: no query, no filters, its own steps.
    expect(box().value).toBe('');
    expect(count()).toBe('');
    expect(screen.getByText('12 interactions linked to this PR')).toBeInTheDocument();
    expect(screen.queryAllByRole('button', { name: /^Remove filter/ })).toHaveLength(0);
    expect(screen.getByRole('button', { name: 'Filters' })).toBeInTheDocument();
    expect(timelineCards()).toHaveLength(12);
    settle();
    expect(window.location.search + window.location.hash).toBe('');
    // And searching in it is about its steps.
    typeQuery('refund_handler');
    expect(count()).toBe('1 of 1 · 1 of 12 steps');
    typeQuery('invoices');
    expect(count()).toBe('1 of 12 · 12 of 12 steps');
  });

  it('is not given the file filter of the last pull request, nor its commits to choose from', () => {
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    window.history.replaceState(null, '', '/pr/acme/shop/1');
    page();
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Touches the open file/ }));
    settle();
    expect(window.location.hash).toContain('file=');
    go('/pr/acme/shop/2');
    settle();
    expect(window.location.search + window.location.hash).toBe('');
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    const commits = within(screen.getByRole('group', { name: 'Commit' })).getAllByRole('checkbox');
    expect(commits).toHaveLength(1);
    expect(commits[0].closest('label')).toHaveTextContent('feat: invoices');
    expect(screen.getByRole('checkbox', { name: /Touches the open file/ })).not.toBeChecked();
  });

  it('shows the pull request a link names with the view the link names, for each of two links in turn', () => {
    // Round 3: a link names its view in its fragment, and the search text it brought leaves the address at once.
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    window.history.replaceState(null, '', '/pr/acme/shop/1#q=refund_handler');
    page();
    expect(box().value).toBe('refund_handler');
    const first = count();
    expect(first).toMatch(/^1 of \d+ · \d+ of 40 steps$/);
    expect(window.location.href).not.toContain('refund_handler');
    go('/pr/acme/shop/2#q=invoices');
    expect(box().value).toBe('invoices');
    expect(count()).toBe('1 of 12 · 12 of 12 steps');
    expect(window.location.href).not.toContain('invoices');
    go('/pr/acme/shop/1#q=refund_handler&tool=Edit');
    expect(box().value).toBe('refund_handler');
    expect(screen.getByRole('button', { name: 'Remove filter Tool: Edit' })).toBeInTheDocument();
    settle();
    expect(window.location.pathname).toBe('/pr/acme/shop/1');
    expect(fragment().getAll('tool')).toEqual(['Edit']);
    expect(fragment().has('q')).toBe(false);
  });

  it('goes back to the filters that were left, and not to the text of the search, when the browser goes back', async () => {
    // Round 3: the address keeps a page's filters and never its search text; Back restores what the address kept.
    window.history.replaceState(null, '', '/pr/acme/shop/1?x=1');
    page();
    typeQuery('refund_handler');
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(within(screen.getByRole('group', { name: 'Stimulus origin' })).getAllByRole('checkbox')[0]);
    await waitFor(() => expect(window.location.hash).toContain('origin='), { timeout: 2_000 });
    expect(window.location.href).not.toContain('refund_handler');
    const left = window.location.pathname + window.location.search + window.location.hash;
    const chip = screen.getByRole('button', { name: /^Remove filter Origin/ }).getAttribute('aria-label');
    go('/pr/acme/shop/2');
    expect(box().value).toBe('');
    await waitFor(() => expect(window.location.search + window.location.hash).toBe(''), { timeout: 2_000 });
    act(() => window.history.back());
    await waitFor(() => expect(window.location.pathname).toBe('/pr/acme/shop/1'), { timeout: 2_000 });
    expect(window.location.pathname + window.location.search + window.location.hash).toBe(left);
    await waitFor(() => expect(screen.getByRole('button', { name: /^Remove filter Origin/ }).getAttribute('aria-label')).toBe(chip));
    expect(box().value).toBe('');
    expect(count()).toMatch(/^\d+ of 40 steps$/);
    expect(screen.getByText('40 interactions linked to this PR')).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// The panel the timeline is in can be closed, and on a narrow screen it starts closed
// ---------------------------------------------------------------------------

describe('the panel of the timeline', () => {
  it('comes back with the filters it was closed with: the address is where they are kept, and the text of the search is not', () => {
    // Round 3: the text of a search is never in the address, so closing the panel ends it; the filters stay.
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    window.history.replaceState(null, '', '/pr/acme/shop/1');
    page();
    typeQuery('refund_handler');
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(within(screen.getByRole('group', { name: 'Tool' })).getAllByRole('checkbox')[0]);
    const chips = screen.getAllByRole('button', { name: /^Remove filter/ }).map((chip) => chip.getAttribute('aria-label'));
    fireEvent.click(screen.getByRole('button', { name: 'Close timeline' }));
    expect(screen.queryByRole('searchbox')).toBeNull();
    expect(fragment().getAll('tool')).toHaveLength(1);
    expect(window.location.href).not.toContain('refund_handler');
    fireEvent.click(screen.getByRole('button', { name: 'Open timeline' }));
    expect(box().value).toBe('');
    expect(count()).toMatch(/^\d+ of 40 steps$/);
    expect(screen.getAllByRole('button', { name: /^Remove filter/ }).map((chip) => chip.getAttribute('aria-label'))).toEqual(chips);
  });

  it('is not there on a narrow screen until it is opened, and then it shows the view the address names', () => {
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
    const width = window.innerWidth;
    Object.defineProperty(window, 'innerWidth', { value: 800, configurable: true });
    try {
      window.history.replaceState(null, '', '/pr/acme/shop/1#q=refund_handler&tool=Edit');
      page();
      expect(screen.queryByRole('searchbox')).toBeNull();
      // "/" has nowhere to go, and does nothing.
      expect(() => fireEvent.keyDown(document.body, { key: '/' })).not.toThrow();
      act(() => void vi.advanceTimersByTime(500));
      // The address still says the view: a page that is not showing the timeline does not wipe it.
      expect(fragment().get('q')).toBe('refund_handler');
      fireEvent.click(screen.getByRole('button', { name: 'Open timeline' }));
      expect(box().value).toBe('refund_handler');
      expect(screen.getByRole('button', { name: 'Remove filter Tool: Edit' })).toBeInTheDocument();
      // The timeline took the text from the address and took it out of it, leaving the filter.
      expect(fragment().has('q')).toBe(false);
      expect(fragment().getAll('tool')).toEqual(['Edit']);
    } finally {
      Object.defineProperty(window, 'innerWidth', { value: width, configurable: true });
    }
  });
});
