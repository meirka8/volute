/**
 * The acceptance criteria of issue #118, as written there, over a typical 300-step timeline
 * (src/test/searchFixtures.ts):
 *
 * - typing a file name narrows to the steps that touched it within one frame, and clearing restores the full
 *   list and the scroll position;
 * - a subagent-only filter shows just that conversation's steps, with the parent session's commit links intact;
 * - the address of a filtered view reproduces it on reload.
 *
 * "Within one frame" is measured on the search itself (timelineSearch.perf.test.ts holds the numbers); here the
 * whole interaction, which jsdom draws far more slowly than a browser, is held to a coarse bound that only a
 * search that does work for every step on every key would break.
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { FakeResizeObserver, stubPageLayout } from '../../test/layoutStubs';
import { FIXTURE_SUBAGENTS, searchFixture } from '../../test/searchFixtures';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(() => {
  installDomStubs();
  FakeResizeObserver.instances = [];
  vi.stubGlobal('ResizeObserver', FakeResizeObserver);
  window.history.replaceState(null, '', '/');
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  restoreDomStubs();
  window.history.replaceState(null, '', '/');
});

const fixture = searchFixture(300);
const byId = new Map<string, InteractionNode>(fixture.nodes.map((node) => [node.id, node]));

const box = () => screen.getByRole('searchbox', { name: 'Search steps' }) as HTMLInputElement;
const typeQuery = (value: string) => fireEvent.change(box(), { target: { value } });
const count = () => screen.getByRole('status').textContent;
const cardRoots = (container: HTMLElement) => Array.from(container.querySelectorAll<HTMLElement>('.flow-root > *'));
const scrollerOf = (container: HTMLElement) => container.querySelector<HTMLElement>('.overflow-y-auto')!;
const nextFrame = () => act(() => void vi.advanceTimersByTime(20));
/** The short hashes of the commit chips a card shows. */
const commitChips = (card: HTMLElement) => Array.from(card.querySelectorAll('[aria-label*=": commit "]')).map((chip) => /commit ([0-9a-f]{7})/.exec(chip.getAttribute('aria-label')!)![1]);

describe('typing a file name in a 300-step timeline', () => {
  /** How many steps the list shows, from what the count says (nothing narrowing it: all 300). */
  const shownBy = (text: string | null): number => {
    if (text === null || text === '') return 300;
    const steps = /(?:^| · )([\d,]+) of 300 steps$/.exec(text);
    return steps === null ? 0 : Number(steps[1].replace(',', ''));
  };

  it('narrows to the steps that touched it, a letter at a time, and nothing is searched again for each letter', () => {
    // The list is windowed, as a browser draws it: a page of a few cards. (Drawing the three hundred cards that the first
    // letters leave, at each of nineteen keys, is what jsdom does slowly and what this test is not about.)
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    expect(screen.getByText('300 interactions linked to this PR')).toBeInTheDocument();
    const word = 'path:refund_handler';
    let previous = Infinity;
    const started = performance.now();
    for (let at = 1; at <= word.length; at++) {
      typeQuery(word.slice(0, at));
      // "path:" on its own is not a query yet (the person is still typing the value): the narrowing starts again after it.
      if (count() === '') {
        previous = Infinity;
        continue;
      }
      // Each step lists no more than the one before it: the list only narrows as the name is typed.
      const shown = shownBy(count());
      expect(shown, word.slice(0, at)).toBeLessThanOrEqual(previous);
      expect(cardRoots(container).length).toBeLessThan(40);
      previous = shown;
    }
    expect(performance.now() - started).toBeLessThan(10_000);

    expect(count()).toBe('1 of 3 · 3 of 300 steps');
    expect(cardRoots(container)).toHaveLength(3);
    // They are the steps that edited the file, newest first, and the card of each names it.
    for (const card of cardRoots(container)) expect(card.textContent).toContain('src/payments/refund_handler.ts');
    expect(container.querySelectorAll('mark').length).toBeGreaterThanOrEqual(3);
    expect(Array.from(container.querySelectorAll('mark')).every((mark) => mark.textContent === 'refund_handler')).toBe(true);
  });

  it('finds the same steps whether it is asked for as a path or as a word, and finds more only where the word is said', () => {
    const { container } = render(<ShadowTimeline interactions={fixture.nodes} />);
    typeQuery('path:refund_handler');
    const touching = cardRoots(container).map((card) => card.textContent);
    expect(touching).toHaveLength(fixture.fileSteps.length);
    typeQuery('refund_handler');
    // The steps that react to the result of those edits say the name too.
    expect(cardRoots(container).length).toBeGreaterThan(touching.length);
    expect(cardRoots(container).length).toBeLessThan(15);
  });

  it('restores the full list and the scroll position when it is cleared', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame'] });
    const { container } = render(<ShadowTimeline interactions={fixture.nodes} />);
    const scroller = scrollerOf(container);
    scroller.scrollTop = 20_150;
    fireEvent.scroll(scroller);
    const place = () => {
      const top = scroller.scrollTop;
      return cardRoots(container)
        .map((card) => ({ position: Number(card.getAttribute('aria-posinset')), card }))
        .filter(({ position }) => (position - 1) * 300 + 300 > top && (position - 1) * 300 < top + page.viewport)
        .map(({ position }) => position);
    };
    const before = place();
    expect(before.length).toBeGreaterThan(1);

    typeQuery('refund_handler');
    expect(cardRoots(container).length).toBeLessThan(15);
    expect(scroller.scrollTop).toBe(0);
    typeQuery('');
    nextFrame();

    expect(count()).toBe('');
    expect(cardRoots(container)[0].getAttribute('aria-setsize')).toBe('300');
    expect(place()).toEqual(before);
    expect(scroller.scrollTop).toBe(20_150);
  });
});

describe('a subagent-only filter', () => {
  it.each(FIXTURE_SUBAGENTS)('shows just the steps of %s, each with the commit links it had', (conversation) => {
    const { container } = render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    const expected = fixture.subagentSteps[conversation];
    expect(expected.length).toBeGreaterThan(10);

    fireEvent.click(screen.getByRole('button', { name: 'Filters' }));
    const subagent = conversation.slice(conversation.indexOf(':agent-') + ':agent-'.length);
    fireEvent.click(screen.getByRole('checkbox', { name: new RegExp(`Subagent ${subagent}`) }));

    expect(count()).toBe(`${expected.length} of 300 steps`);
    const cards = cardRoots(container);
    expect(cards).toHaveLength(expected.length);
    // The steps are those of the subagent, in the order the timeline lists them...
    expected.forEach((id, k) => {
      const node = byId.get(id)!;
      expect(node.conversation_id).toBe(conversation);
      // ...and each card shows every link of its step: the commit, with how sure the link is, as before.
      const hashes = node.artifact_links.map((link) => link.git_commit_hash.slice(0, 7));
      expect(commitChips(cards[k]).sort()).toEqual([...hashes].sort());
    });
    // Those are commits of the pull request, which the subagent did not make itself.
    const known = new Set(fixture.commits.map((commit) => commit.sha.slice(0, 7)));
    for (const card of cards) for (const hash of commitChips(card)) expect(known.has(hash)).toBe(true);
  });

  it('says it with a chip, and the other conversations come back when it is removed', () => {
    const { container } = render(<ShadowTimeline interactions={fixture.nodes} />);
    fireEvent.click(screen.getByRole('button', { name: 'Filters' }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Subagent a0bd78e0/ }));
    expect(cardRoots(container).length).toBeLessThan(40);
    fireEvent.click(screen.getByRole('button', { name: 'Remove filter Conversation: Subagent a0bd78e0' }));
    expect(count()).toBe('');
    expect(screen.getByText('300 interactions linked to this PR')).toBeInTheDocument();
  });

  it('can be told apart from the steps of the session by the commit filter as well', () => {
    const { container } = render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    fireEvent.click(screen.getByRole('button', { name: 'Filters' }));
    const commit = within(screen.getByRole('group', { name: 'Commit' }));
    // Newest commit first, each with the first line of its message.
    const labels = commit.getAllByRole('checkbox').map((input) => input.closest('label')!.textContent);
    expect(labels).toHaveLength(5);
    expect(labels[0]).toContain('chore: tidy the shipping client');
    expect(labels[4]).toContain('feat: add the refund handler');
    fireEvent.click(commit.getAllByRole('checkbox')[4]); // the oldest
    const oldest = fixture.commits[0].sha;
    const linked = fixture.nodes.filter((node) => node.artifact_links.some((link) => link.git_commit_hash === oldest)).length;
    expect(count()).toBe(`${linked} of 300 steps`);
    expect(cardRoots(container).length).toBeGreaterThan(0);
  });
});

describe('the address of a filtered view', () => {
  // Round 3: the address keeps the filters, in its fragment, and never the search text; a link that someone copies holds both.
  const fragment = () => new URLSearchParams(window.location.hash.slice(1));

  it('reproduces it on reload: the filters from the address, and the whole view from the link the page copies', () => {
    vi.useFakeTimers();
    window.history.replaceState(null, '', '/pr/acme/shop/7?tab=files');
    const first = render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    fireEvent.click(screen.getByRole('button', { name: 'Filters' }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Prompt/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Edit/ }));
    typeQuery('refund');

    // At once, with no timer to wait out; the query string is as the page had it and the text is not in the address.
    expect(window.location.pathname).toBe('/pr/acme/shop/7');
    expect(new URLSearchParams(window.location.search).get('tab')).toBe('files');
    expect(window.location.href).not.toContain('refund');
    expect(fragment().has('q')).toBe(false);
    expect(fragment().getAll('origin')).toEqual(['prompt']);
    expect(fragment().getAll('tool')).toEqual(['Edit']);

    const view = (container: HTMLElement) => ({
      query: box().value,
      count: count(),
      cards: cardRoots(container).map((card) => card.textContent),
      chips: screen.getAllByRole('button', { name: /^Remove filter/ }).map((chip) => chip.getAttribute('aria-label')),
    });
    const shown = view(first.container);
    expect(shown.cards.length).toBeGreaterThan(0);

    // The address alone brings the filters back, and not the text.
    const address = window.location.href;
    first.unmount();
    window.history.replaceState(null, '', address);
    const reloaded = render(<ShadowTimeline interactions={[...fixture.nodes]} commits={[...fixture.commits]} />);
    expect(box().value).toBe('');
    expect(screen.getAllByRole('button', { name: /^Remove filter/ }).map((chip) => chip.getAttribute('aria-label'))).toEqual(shown.chips);
    expect(window.location.href).toBe(address);
    reloaded.unmount();

    // The link the page copies holds the text as well, and the whole view comes back from it.
    const link = `${address}&q=refund`;
    window.history.replaceState(null, '', link);
    const second = render(<ShadowTimeline interactions={[...fixture.nodes]} commits={[...fixture.commits]} />);
    expect(view(second.container)).toEqual(shown);
    expect(window.location.href).toBe(address);
  });

  it('is shared as it is: opened on a page with the steps still loading, the view appears with them', () => {
    window.history.replaceState(null, '', '/#q=path%3Arefund_handler&origin=prompt');
    const { container, rerender } = render(<ShadowTimeline interactions={[]} isLoading />);
    expect(screen.getByText('Loading cognitive history...')).toBeInTheDocument();
    rerender(<ShadowTimeline interactions={fixture.nodes} />);
    expect(box()).toHaveValue('path:refund_handler');
    expect(count()).toBe('1 of 3 · 3 of 300 steps');
    expect(cardRoots(container)).toHaveLength(3);
    expect(screen.getByRole('button', { name: 'Remove filter Origin: Prompt' })).toBeInTheDocument();
  });
});

describe('a timeline of ten thousand steps', () => {
  const big = searchFixture(10_000, 40);

  it('stays responsive: a query, its narrowing, and its clearing, with a few dozen cards on the page', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 900 });
    const started = performance.now();
    const { container } = render(<ShadowTimeline interactions={big.nodes} commits={big.commits} />);
    const elements = () => container.querySelectorAll('*').length;
    const steps = ['r', 're', 'ref', 'refu', 'refun', 'refund', 'refund_', 'refund_h', 'refund_handler'];
    for (const query of steps) {
      typeQuery(query);
      expect(cardRoots(container).length).toBeLessThan(40);
      expect(elements()).toBeLessThan(6000);
    }
    // Every 97th step touched the file; the steps that read the result of each say the name too.
    expect(count()).toMatch(/^1 of [\d,]+ · [\d,]+ of 10,000 steps$/);
    typeQuery('path:refund_handler');
    expect(count()).toBe(`1 of ${big.fileSteps.length} · ${big.fileSteps.length} of 10,000 steps`);
    typeQuery('');
    expect(count()).toBe('');
    // A coarse bound on the whole sequence in jsdom (the render of ten thousand steps included); a search that
    // did work for every step on every key would take minutes.
    expect(performance.now() - started).toBeLessThan(20_000);
  });
});
