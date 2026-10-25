/**
 * Search and filters over a timeline long enough to be windowed (issue #118): next and previous bring a
 * match into view, the place the reader had is found again when the search is cleared, and what the reader
 * opened in a card is still open when the card comes back into the window.
 *
 * jsdom has no layout, so the page is given one (see ../../test/layoutStubs), as in
 * ShadowTimeline.windowing.test.tsx: each card is as tall as the test says, and "Step N" is how a card is told.
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { makeNode } from '../../test/fixtures';
import { FakeResizeObserver, stepOf, stubPageLayout, type PageLayout } from '../../test/layoutStubs';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(() => {
  installDomStubs();
  FakeResizeObserver.instances = [];
  vi.stubGlobal('ResizeObserver', FakeResizeObserver);
  vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame'] });
  window.history.replaceState(null, '', '/');
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  restoreDomStubs();
  window.history.replaceState(null, '', '/');
});

const BASE = Date.parse('2026-03-02T09:15:00.000Z');
const stepId = (n: number) => `2b7d4e91-5c3a-8f60-b2d4-${n.toString(16).padStart(12, '0')}`;

/**
 * Steps 0..count-1 as they are listed (step 0 is the newest). A step says "Step N" in its response; a step whose
 * number `matches` holds the word "needle" too. Its stimulus is a tool result, folded behind a toggle.
 */
function makeSteps(count: number, matches: (n: number) => boolean = () => false): InteractionNode[] {
  return Array.from({ length: count }, (_, n) =>
    makeNode({
      id: stepId(n),
      author: 'system',
      timestamp: BASE - n * 1000,
      user_prompt: `[tool_result Bash] output ${n}${matches(n) ? ' needle' : ''}`,
      model_response: `Step ${n}`,
    }),
  );
}

const box = () => screen.getByRole('searchbox', { name: 'Search steps' }) as HTMLInputElement;
const typeQuery = (value: string) => fireEvent.change(box(), { target: { value } });
const press = (key: string, init: KeyboardEventInit = {}) => fireEvent.keyDown(box(), { key, ...init });
const count = () => screen.getByRole('status').textContent;
const cardRoots = (container: HTMLElement) => Array.from(container.querySelectorAll<HTMLElement>('.flow-root > *'));
const scrollerOf = (container: HTMLElement) => container.querySelector<HTMLElement>('.overflow-y-auto')!;
const nextFrame = () => act(() => void vi.advanceTimersByTime(20));

function scrollTo(scroller: HTMLElement, top: number) {
  scroller.scrollTop = top;
  fireEvent.scroll(scroller);
}

/** Where every mounted card is, in the page, from the padding and the heights the page reports. */
function geometry(container: HTMLElement, page: PageLayout) {
  const scroller = scrollerOf(container);
  const list = scroller.querySelector<HTMLElement>('.flow-root')!;
  let y = parseFloat(list.style.paddingTop) || 0;
  return Array.from(list.children).map((element) => {
    const step = stepOf(element)!;
    const top = y;
    y += page.heightOf(step, element);
    return { element, step, top, bottom: y };
  });
}

/** The cards that overlap the viewport, and where each is in it. */
function viewportOf(container: HTMLElement, page: PageLayout) {
  const top = scrollerOf(container).scrollTop;
  return geometry(container, page)
    .filter((card) => card.bottom > top && card.top < top + page.viewport)
    .map((card) => ({ step: card.step, y: card.top - top }));
}

const current = (container: HTMLElement) => cardRoots(container).filter((card) => card.getAttribute('aria-current') === 'true');
const currentStep = (container: HTMLElement) => current(container).map((card) => stepOf(card));

describe('moving between matches in a long list', () => {
  // Every odd step matches: 100 of 200, which is still a windowed list.
  const odd = (n: number) => n % 2 === 1;

  it('narrows a long list to the matches, which is itself windowed, and starts at the top', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, odd)} />);
    scrollTo(scrollerOf(container), 30_000);
    typeQuery('needle');
    expect(count()).toBe('1 of 100 · 100 of 200 steps');
    expect(scrollerOf(container).scrollTop).toBe(0);
    expect(geometry(container, page).map((card) => card.step)).toEqual([1, 3, 5, 7, 9, 11]);
    // The list says how long it is: 100, not 200.
    expect(cardRoots(container)[0].getAttribute('aria-setsize')).toBe('100');
    expect(currentStep(container)).toEqual([1]);
  });

  it('brings the next match into view when it is not, and leaves the page alone when it is', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, odd)} />);
    typeQuery('needle');
    const scroller = scrollerOf(container);
    press('Enter'); // the second match is on the page already
    expect(count()).toBe('2 of 100 · 100 of 200 steps');
    expect(currentStep(container)).toEqual([3]);
    expect(scroller.scrollTop).toBe(0);

    for (let k = 0; k < 8; k++) press('Enter'); // the tenth match: 9 cards down
    expect(count()).toBe('10 of 100 · 100 of 200 steps');
    // Its top is at the top of the viewport.
    expect(scroller.scrollTop).toBe(9 * 300);
    expect(currentStep(container)).toEqual([19]);
    expect(screen.getByText('Step 19')).toBeInTheDocument();
  });

  it('wraps from the last match to the first, and from the first to the last, however far that is', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, odd)} />);
    typeQuery('needle');
    press('Enter', { shiftKey: true });
    expect(count()).toBe('100 of 100 · 100 of 200 steps');
    expect(currentStep(container)).toEqual([199]);
    expect(scrollerOf(container).scrollTop).toBe(99 * 300);
    expect(screen.getByText('Step 199')).toBeInTheDocument();
    press('Enter');
    expect(count()).toBe('1 of 100 · 100 of 200 steps');
    expect(currentStep(container)).toEqual([1]);
    expect(scrollerOf(container).scrollTop).toBe(0);
  });

  it('puts the match at the top even when the cards on the way turn out to be taller than guessed', () => {
    const heightOf = (n: number) => (n < 20 ? 300 : 520);
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, odd)} />);
    typeQuery('needle');
    press('Enter', { shiftKey: true }); // the last match, 99 cards down, none of which was ever measured
    nextFrame();
    const [card] = current(container);
    expect(card).toBeDefined();
    expect(stepOf(card)).toBe(199);
    const found = geometry(container, page).find((candidate) => candidate.element === card)!;
    // The card the search is on is at the top of what the reader sees, whatever was guessed about the ones above it.
    expect(found.top).toBeCloseTo(scrollerOf(container).scrollTop, 6);
  });

  it('keeps the match in view on every step of the way, though the page is only moved when it has to be', () => {
    const heightOf = (n: number) => (n < 20 ? 300 : 520);
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, odd)} />);
    typeQuery('needle');
    for (let k = 0; k < 60; k++) {
      press('Enter');
      nextFrame();
      const [card] = current(container);
      const found = geometry(container, page).find((candidate) => candidate.element === card)!;
      const scrollTop = scrollerOf(container).scrollTop;
      expect(found.top, `after ${k + 1} presses`).toBeGreaterThanOrEqual(scrollTop - 1e-6);
      expect(found.top, `after ${k + 1} presses`).toBeLessThan(scrollTop + page.viewport);
    }
  });

  it('does not move the page for a step that is on it, and does not select anything', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const onSelect = vi.fn();
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, odd)} onSelectInteraction={onSelect} />);
    typeQuery('needle');
    for (let k = 0; k < 30; k++) press('Enter');
    press('Enter', { shiftKey: true });
    expect(onSelect).not.toHaveBeenCalled();
    expect(screen.queryByRole('button', { pressed: true })).not.toBeInTheDocument();
    expect(current(container)).toHaveLength(1);
  });

  it('keeps going when the list is short enough to be on the page whole', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    // Twenty matches: the page asks where the card is.
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, (n) => n % 10 === 3)} />);
    typeQuery('needle');
    expect(cardRoots(container)).toHaveLength(20);
    const scroller = scrollerOf(container);
    const place = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      // The scroller is at the top of the page; a card is where its place in the list puts it, less how far the page is scrolled.
      if (this === scroller) return { top: 0, bottom: 800, height: 800, left: 0, right: 0, width: 0, x: 0, y: 0, toJSON: () => ({}) } as DOMRect;
      const index = cardRoots(container).indexOf(this);
      const top = index * 300 - scroller.scrollTop;
      return { top, bottom: top + 300, height: 300, left: 0, right: 0, width: 0, x: 0, y: top, toJSON: () => ({}) } as DOMRect;
    });
    for (let k = 0; k < 5; k++) press('Enter'); // the sixth match: 5 cards down, but not out of sight yet
    expect(scroller.scrollTop).toBe(3 * 300);
    expect(currentStep(container)).toEqual([53]);
    press('Enter'); // the seventh: off the bottom of the page
    expect(scroller.scrollTop).toBe(6 * 300);
    expect(currentStep(container)).toEqual([63]);
    press('Enter', { shiftKey: true }); // back to the sixth: it is above the top of the page now
    expect(scroller.scrollTop).toBe(5 * 300);
    expect(currentStep(container)).toEqual([53]);
    place.mockRestore();
  });
});

describe('finding the place again', () => {
  // Steps 0 to 19 are 300 tall and the rest 500: what the first cards measure is not what the others turn out to be.
  const heightOf = (n: number) => (n < 20 ? 300 : 500);

  it('puts the reader back at the card they were at, the same distance into it, when the search is cleared', () => {
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, (n) => n % 3 === 0)} />);
    const scroller = scrollerOf(container);
    scrollTo(scroller, 30_150);
    const before = viewportOf(container, page);
    expect(before.length).toBeGreaterThan(1);
    expect(before[0].y).toBeLessThan(0); // partway into the first card on the page

    typeQuery('needle');
    expect(scroller.scrollTop).toBe(0);
    for (let k = 0; k < 7; k++) press('Enter'); // wandering about in the matches
    expect(scroller.scrollTop).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole('button', { name: 'Clear search' }));
    nextFrame();
    expect(count()).toBe('');
    const after = viewportOf(container, page);
    expect(after[0]).toEqual(before[0]);
    expect(after.slice(0, 2)).toEqual(before.slice(0, 2));
  });

  it('does the same when it is the filters that are cleared, or both at once', () => {
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const nodes = makeSteps(200, (n) => n % 3 === 0).map((node, n) => (n % 2 === 0 ? { ...node, tool_executions: [{ tool_protocol: 'native', tool_name: 'Bash', arguments: '{}', status: 'success' as const }] } : node));
    const { container } = render(<ShadowTimeline interactions={nodes} />);
    const scroller = scrollerOf(container);
    scrollTo(scroller, 12_345);
    const before = viewportOf(container, page);

    fireEvent.click(screen.getByRole('button', { name: 'Filters' }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    typeQuery('needle');
    expect(scroller.scrollTop).toBe(0);
    fireEvent.click(screen.getByRole('button', { name: 'Clear search' }));
    expect(scroller.scrollTop).toBe(0); // the filters still narrow it
    fireEvent.click(screen.getByRole('button', { name: 'Remove filter Tool: Bash' }));
    nextFrame();
    expect(viewportOf(container, page)[0]).toEqual(before[0]);
  });

  it('keeps the first place when the search is changed, not the places it passes through', () => {
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, (n) => n % 2 === 0)} />);
    const scroller = scrollerOf(container);
    scrollTo(scroller, 20_020);
    const before = viewportOf(container, page);
    typeQuery('need');
    scrollTo(scroller, 4_000);
    typeQuery('needle');
    typeQuery('needle ');
    expect(scroller.scrollTop).toBe(0);
    typeQuery('');
    nextFrame();
    expect(viewportOf(container, page)[0]).toEqual(before[0]);
  });

  it('opens at the top when the search began at the top', () => {
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, (n) => n % 3 === 0)} />);
    typeQuery('needle');
    for (let k = 0; k < 7; k++) press('Enter');
    typeQuery('');
    nextFrame();
    expect(scrollerOf(container).scrollTop).toBe(0);
    expect(geometry(container, page)[0].step).toBe(0);
  });

  it('comes back to a list that is not windowed at the scroll position it had', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(20, (n) => n % 4 === 0)} />);
    const scroller = scrollerOf(container);
    scroller.scrollTop = 1_700;
    typeQuery('needle');
    expect(scroller.scrollTop).toBe(0);
    typeQuery('');
    expect(scroller.scrollTop).toBe(1_700);
  });

  it('comes back to a long list from a search that left only a few steps', () => {
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, (n) => n === 120)} />);
    const scroller = scrollerOf(container);
    scrollTo(scroller, 41_000);
    const before = viewportOf(container, page);
    typeQuery('needle'); // one step: the list is not windowed any more
    expect(cardRoots(container)).toHaveLength(1);
    typeQuery('');
    nextFrame();
    expect(viewportOf(container, page)[0]).toEqual(before[0]);
  });

  it('does not hold on to a place when the search was on from the start', () => {
    window.history.replaceState(null, '', '/#q=needle');
    stubPageLayout({ heightOf, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, (n) => n % 3 === 0)} />);
    expect(count()).toBe('1 of 67 · 67 of 200 steps');
    typeQuery('');
    nextFrame();
    expect(scrollerOf(container).scrollTop).toBe(0);
    expect(count()).toBe('');
  });
});

describe('what the reader opened', () => {
  it('is still open when the card comes back into the window after the reader scrolled away', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = scrollerOf(container);
    const toggle = () => cardRoots(container).find((card) => stepOf(card) === 2)!.querySelector('button[aria-expanded]') as HTMLElement;
    expect(toggle()).toHaveAttribute('aria-expanded', 'false');
    fireEvent.click(toggle());
    expect(toggle()).toHaveAttribute('aria-expanded', 'true');

    scrollTo(scroller, 30_000);
    expect(screen.queryByText('Step 2')).not.toBeInTheDocument(); // out of the window: unmounted
    scrollTo(scroller, 0);
    expect(toggle()).toHaveAttribute('aria-expanded', 'true');
  });

  it('is still open when the card was off the page for a search as well as for a scroll', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, (n) => n === 150)} />);
    const toggle = () => cardRoots(container).find((card) => stepOf(card) === 2)!.querySelector('button[aria-expanded]') as HTMLElement;
    fireEvent.click(toggle());
    typeQuery('needle');
    expect(cardRoots(container).map((card) => stepOf(card))).toEqual([150]);
    typeQuery('');
    expect(toggle()).toHaveAttribute('aria-expanded', 'true');
  });

  it('keeps the other folds of a card too: long text, code in full, a list paged further', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const lines = (n: number, label: string) => Array.from({ length: n }, (_, k) => `${label} ${k + 1}`).join('\n');
    const rich = makeNode({
      id: stepId(0),
      author: 'human',
      timestamp: BASE + 1000,
      user_prompt: 'Step 0',
      model_response: lines(9, 'response'),
      tool_executions: [
        { tool_protocol: 'native', tool_name: 'Bash', arguments: JSON.stringify({ command: lines(30, 'echo') }), status: 'success' },
        ...Array.from({ length: 12 }, (_, k) => ({ tool_protocol: 'native', tool_name: 'Bash', arguments: JSON.stringify({ command: `echo ${k}` }), status: 'success' as const })),
      ],
    });
    const { container } = render(<ShadowTimeline interactions={[rich, ...makeSteps(200).slice(1)]} />);
    const scroller = scrollerOf(container);
    fireEvent.click(screen.getByRole('button', { name: 'Show more' }));
    fireEvent.click(screen.getByRole('button', { name: 'Show all 30 lines' }));
    fireEvent.click(screen.getByRole('button', { name: /^Show 3 more tool calls/ }));
    const open = () => [screen.queryAllByRole('button', { name: 'Show less' }).length, screen.queryAllByRole('button', { name: /^Show 3 more tool calls/ }).length];
    expect(open()).toEqual([2, 0]);

    scrollTo(scroller, 30_000);
    expect(screen.queryByText('Step 0')).not.toBeInTheDocument();
    scrollTo(scroller, 0);
    expect(open()).toEqual([2, 0]);
  });
});

describe('a long list under a search', () => {
  it('draws only the cards near the viewport of the narrowed list, and counts them over the whole', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(2000, (n) => n % 2 === 0)} />);
    typeQuery('needle');
    expect(count()).toBe('1 of 1,000 · 1,000 of 2,000 steps');
    expect(cardRoots(container).length).toBeLessThan(10);
    scrollTo(scrollerOf(container), 150_000);
    expect(geometry(container, page).some((card) => card.step === 1000)).toBe(true);
    expect(cardRoots(container)[0].getAttribute('aria-setsize')).toBe('1000');
  });

  it('has the cards it brings back fade in no more than the first time: a search is not an arrival', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200, (n) => n % 2 === 1)} />);
    nextFrame(); // the reader looks at the page for a moment
    const entering = () => cardRoots(container).filter((card) => card.classList.contains('rr-card-enter')).map((card) => stepOf(card));
    // (The cards of the first draw keep the class they were given, which does not play again.)
    expect(entering()).toEqual([0, 1, 2, 3, 4, 5]);
    typeQuery('needle');
    expect(cardRoots(container).map((card) => stepOf(card))).toEqual([1, 3, 5, 7, 9, 11]);
    expect(entering()).toEqual([1, 3, 5]);
    scrollTo(scrollerOf(container), 4_000);
    typeQuery('');
    scrollTo(scrollerOf(container), 8_000);
    expect(entering().filter((step) => step! > 5)).toEqual([]);
  });
});
