/**
 * The windowed timeline (issue #135): a long list mounts only the cards near the
 * viewport and stands in for the rest with space, without the page jumping as
 * cards are measured, and without changing which step is selected, which steps
 * are checked against their parents, or how many are counted.
 *
 * jsdom has no layout, so these tests give the page one (see ../../test/layoutStubs):
 * each card is as tall as the test says, and "Step N" in a card's text is how the
 * test knows which card it is.
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { WINDOWING_THRESHOLD } from '../../hooks/useWindowedList';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { undrawableNode } from '../../test/brokenFixtures';
import { injectionParentStep, injectionSystemStep, makeNode } from '../../test/fixtures';
import { FakeResizeObserver, stepOf, stubPageLayout, type PageLayout } from '../../test/layoutStubs';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

/** The id of the step each draw of a card was for, in order. */
const drawn = vi.hoisted(() => ({ ids: [] as string[] }));

vi.mock('./TimelineNode', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./TimelineNode')>();
  return {
    ...actual,
    TimelineNode: (props: Parameters<typeof actual.TimelineNode>[0]) => {
      drawn.ids.push(props.interaction.id);
      return actual.TimelineNode(props);
    },
  };
});

beforeEach(() => {
  installDomStubs();
  drawn.ids.length = 0;
  FakeResizeObserver.instances = [];
  vi.stubGlobal('ResizeObserver', FakeResizeObserver);
  // Only the frame callbacks: the timeline defers some work to the next frame.
  vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame'] });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  restoreDomStubs();
});

const BASE = Date.parse('2026-03-02T09:15:00.000Z');
// (Not the ids of the fixtures: those are 1f0e5c3a-..., and a step of the same id would be the same step.)
const stepId = (n: number) => `2b7d4e91-5c3a-8f60-b2d4-${n.toString(16).padStart(12, '0')}`;

/** Steps 0..count-1 as they are listed: step 0 is the newest. A step shows "Step N". */
function makeSteps(count: number): InteractionNode[] {
  return Array.from({ length: count }, (_, n) =>
    makeNode({ id: stepId(n), author: 'human', timestamp: BASE - n * 1000, user_prompt: `Step ${n}` }),
  );
}

/** A step newer than all of those made by `makeSteps`. */
const newestStep = () => makeNode({ id: stepId(999), author: 'human', timestamp: BASE + 60_000, user_prompt: 'Step 999' });

/** `count` steps newer than all of those made by `makeSteps`, newest first. */
const newerSteps = (count: number) =>
  Array.from({ length: count }, (_, n) =>
    makeNode({ id: stepId(1000 + n), author: 'human', timestamp: BASE + (count - n + 1) * 1000, user_prompt: `Step ${1000 + n}` }),
  );

const NOTE_PATTERN = /Shown undivided: the tool results named here don't match the previous step's tool calls/;

/** Where the page puts everything, worked out the way a browser would from the padding and the card heights. */
function geometry(container: HTMLElement, page: PageLayout) {
  const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
  const list = scroller.firstElementChild as HTMLElement;
  const before = parseFloat(list.style.paddingTop) || 0;
  const after = parseFloat(list.style.paddingBottom) || 0;
  let y = before;
  const cards = Array.from(list.children).map((element) => {
    const step = stepOf(element)!;
    const top = y;
    y += page.heightOf(step, element);
    return { element, step, top, bottom: y };
  });
  return { scroller, list, before, after, cards, total: y + after };
}

/** What the reader sees: the cards that overlap the viewport, and where each is in it. */
function viewportOf(container: HTMLElement, page: PageLayout) {
  const { scroller, cards } = geometry(container, page);
  const top = scroller.scrollTop;
  return cards
    .filter((card) => card.bottom > top && card.top < top + page.viewport)
    .map((card) => ({ step: card.step, y: card.top - top }));
}

/** Where every mounted card is in the viewport (above it is negative), by step. */
function positionsOf(container: HTMLElement, page: PageLayout): Map<number, number> {
  const { scroller, cards } = geometry(container, page);
  return new Map(cards.map((card) => [card.step, card.top - scroller.scrollTop]));
}

function scrollTo(scroller: HTMLElement, top: number) {
  scroller.scrollTop = top;
  fireEvent.scroll(scroller);
}

/** The next frame: the timeline does some work then. */
const nextFrame = () => act(() => void vi.advanceTimersByTime(20));

const cardRoots = (container: HTMLElement) => Array.from(container.querySelectorAll<HTMLElement>('.flow-root > *'));

/** The header of a card: the button that selects it. */
const headerOf = (card: Element) => card.querySelector('button[aria-pressed]') as HTMLButtonElement;

/** The cards that say they are selected, found by what they contain. */
const selectedCards = (container: HTMLElement) =>
  Array.from(container.querySelectorAll('[aria-pressed="true"]')).map((element) => element.closest('.flow-root > *') as HTMLElement);

const resizeObserverOf = (scroller: HTMLElement) => FakeResizeObserver.watching(scroller)!;

// ---------------------------------------------------------------------------

describe('a short timeline', () => {
  it(`mounts every card when there are ${WINDOWING_THRESHOLD} or fewer, with no space and nothing measured`, () => {
    const page = stubPageLayout({ heightOf: () => 300 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(WINDOWING_THRESHOLD)} />);
    const { cards, list, scroller } = geometry(container, page);
    expect(cards).toHaveLength(WINDOWING_THRESHOLD);
    expect(list.getAttribute('style')).toBeNull();
    expect(scroller.style.overflowAnchor).toBe('');
    expect(page.measured.size).toBe(0);
  });

  it('starts windowing with one card more', () => {
    const page = stubPageLayout({ heightOf: () => 300 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(WINDOWING_THRESHOLD + 1)} />);
    const { cards } = geometry(container, page);
    expect(cards.length).toBeLessThan(WINDOWING_THRESHOLD + 1);
    expect(cards.length).toBeGreaterThan(0);
  });
});

describe('a long timeline: what is mounted', () => {
  it('mounts the cards near the viewport and gives the rest as space below, from the heights measured', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const { cards, before, after, total } = geometry(container, page);
    // The viewport is 800, so a viewport of overscan below it too: [0, 1600) is steps 0 to 5.
    expect(cards.map((card) => card.step)).toEqual([0, 1, 2, 3, 4, 5]);
    expect(before).toBe(0);
    // The other 194 steps are as tall as the ones measured.
    expect(after).toBe(194 * 300);
    expect(total).toBe(200 * 300);
  });

  it('follows the scroll position, with space above and below that adds up to the whole list', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;

    scrollTo(scroller, 30_000); // the top of step 100
    const { cards, before, after, total } = geometry(container, page);
    expect(cards.map((card) => card.step)).toEqual([97, 98, 99, 100, 101, 102, 103, 104, 105]);
    expect(before).toBe(97 * 300);
    expect(after).toBe(94 * 300);
    expect(total).toBe(200 * 300);
    expect(screen.queryByText('Step 96')).not.toBeInTheDocument();
    expect(screen.queryByText('Step 106')).not.toBeInTheDocument();
    expect(screen.getByText('Step 100')).toBeInTheDocument();
  });

  it('mounts the cards at the end of the list, with no space below', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 200 * 300 - 800);
    const { cards, after } = geometry(container, page);
    expect(cards[cards.length - 1].step).toBe(199);
    expect(after).toBe(0);
    expect(screen.getByText('Step 199')).toBeInTheDocument();
  });

  it('mounts more cards when the viewport is taller, and when the pane is made taller', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const { scroller } = geometry(container, page);
    expect(cardRoots(container)).toHaveLength(6);

    page.viewport = 1600;
    act(() => resizeObserverOf(scroller).trigger());
    nextFrame();
    // [0, 1600 + 1600) is steps 0 to 10.
    expect(cardRoots(container)).toHaveLength(11);
  });

  it('counts the margin between two cards once, as part of the card, since the margins of neighbours collapse into one gap', () => {
    // The cards have the margins of a card (my-3, 12px above and below); the page reports their boxes without them.
    const style = document.createElement('style');
    style.textContent = '.my-3 { margin-top: 12px; margin-bottom: 12px; }';
    document.head.appendChild(style);
    try {
      stubPageLayout({ heightOf: () => 300, viewport: 800 });
      const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
      const list = container.querySelector<HTMLElement>('.flow-root')!;
      // 312 a card: [0, 1600) is steps 0 to 5, and the other 194 are 194 x 312.
      expect(list.children).toHaveLength(6);
      expect(parseFloat(list.style.paddingBottom)).toBe(194 * 312);
    } finally {
      style.remove();
    }
  });

  it('keeps the count in the header over the whole list', () => {
    stubPageLayout({ heightOf: () => 300 });
    render(<ShadowTimeline interactions={makeSteps(200)} />);
    expect(screen.getByText('200 interactions linked to this PR')).toBeInTheDocument();
  });

  it('still mounts a bounded number of cards when the page reports every card as zero tall, as a hidden pane does', () => {
    stubPageLayout({ heightOf: () => 0, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    // No card is taken to be shorter than 40: [0, 1600) at 40 is 40 cards, not all 200.
    expect(cardRoots(container).length).toBeGreaterThan(0);
    expect(cardRoots(container).length).toBeLessThanOrEqual(41);
  });

  it('turns the browser’s own scroll anchoring off, since the list holds the reader’s place itself', () => {
    stubPageLayout({ heightOf: () => 300 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    expect(container.querySelector<HTMLElement>('.overflow-y-auto')!.style.overflowAnchor).toBe('none');
  });

  it('draws only the cards that come into the window as the reader scrolls, not those that were already there', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    expect([...drawn.ids].sort()).toEqual([0, 1, 2, 3, 4, 5].map(stepId).sort());

    drawn.ids.length = 0;
    scrollTo(scroller, 300); // one card's worth: [0, 1900) is steps 0 to 6
    expect(drawn.ids).toEqual([stepId(6)]);

    drawn.ids.length = 0;
    scrollTo(scroller, 1500); // [700, 3100) is steps 2 to 10
    expect([...drawn.ids].sort()).toEqual([7, 8, 9, 10].map(stepId).sort());
    expect(screen.queryByText('Step 1')).not.toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------

describe('a long timeline: selection and the rest of the list', () => {
  it('selects the card clicked, whichever part of the list is mounted', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const onSelect = vi.fn();
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} onSelectInteraction={onSelect} />);
    fireEvent.click(screen.getByText('Step 2'));
    expect(onSelect).toHaveBeenLastCalledWith(stepId(2));

    scrollTo(container.querySelector<HTMLElement>('.overflow-y-auto')!, 30_000);
    fireEvent.click(screen.getByText('Step 100'));
    expect(onSelect).toHaveBeenLastCalledWith(stepId(100));
    expect(onSelect).toHaveBeenCalledTimes(2);
  });

  it('shows a step as selected once it is scrolled into the window, though it was not mounted when it was selected', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} selectedInteractionId={stepId(150)} />);
    expect(selectedCards(container)).toHaveLength(0);

    scrollTo(container.querySelector<HTMLElement>('.overflow-y-auto')!, 45_000);
    const selected = selectedCards(container);
    expect(selected).toHaveLength(1);
    expect(selected[0].textContent).toContain('Step 150');
  });

  it('moves the selection between steps that are mounted, redrawing the two and no other', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const steps = makeSteps(200);
    const select = () => {};
    const { rerender, container } = render(
      <ShadowTimeline interactions={steps} selectedInteractionId={stepId(1)} onSelectInteraction={select} />,
    );
    drawn.ids.length = 0;
    rerender(<ShadowTimeline interactions={steps} selectedInteractionId={stepId(4)} onSelectInteraction={select} />);
    expect([...drawn.ids].sort()).toEqual([stepId(1), stepId(4)].sort());
    expect(selectedCards(container)).toHaveLength(1);
  });

  it('checks a step against its parent even though the parent is not mounted', () => {
    // [f0, f1, child, parent, f4, ...]: the child is mounted, its parent (the next one down) is not.
    const fillers = makeSteps(40).map((step, n) =>
      makeNode({ ...step, timestamp: n === 0 ? BASE + 100 * 60_000 : n === 1 ? BASE + 90 * 60_000 : step.timestamp }),
    );
    const steps = [fillers[0], fillers[1], injectionSystemStep, injectionParentStep, ...fillers.slice(4)];
    stubPageLayout({ heightOf: () => 600, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={steps} />);

    // Three cards are mounted: [0, 1600) at 600 each is steps 0, 1 and 2.
    expect(cardRoots(container)).toHaveLength(3);
    expect(screen.queryByText(/Summarise the release notes at/)).not.toBeInTheDocument();
    // The child still knows what its parent called, so its fake markers are caught.
    expect(screen.getAllByText(NOTE_PATTERN)).toHaveLength(1);
  });

  it('keeps the order newest first, over the whole list', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const shuffled = [...makeSteps(200)].reverse();
    const { container } = render(<ShadowTimeline interactions={shuffled} />);
    expect(cardRoots(container).map((card) => stepOf(card))).toEqual([0, 1, 2, 3, 4, 5]);
  });
});

// ---------------------------------------------------------------------------

describe('a long timeline: the page does not jump', () => {
  /** Steps 0 to 9 are 300 tall and the rest 500: what the first cards measure is not what the others turn out to be. */
  const heightOf = (step: number) => (step < 10 ? 300 : 500);

  it('scrolling to a place never measured keeps the card at the top of the viewport at the top', () => {
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;

    // The list thinks step 100 is at 30,000 (every card 300, as the first ones were). It is not.
    scrollTo(scroller, 30_000);
    const { cards } = geometry(container, page);
    const step100 = cards.find((card) => card.step === 100)!;
    expect(step100.top).toBe(scroller.scrollTop);
    expect(scroller.scrollTop).not.toBe(30_000);
  });

  it('scrolling up past cards measured for the first time moves everything in view by exactly what the reader scrolled', () => {
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_000);
    const start = scroller.scrollTop;

    let corrections = 0;
    for (let round = 0; round < 10; round++) {
      const before = positionsOf(container, page);
      const scrolledTo = scroller.scrollTop - 400;
      scrollTo(scroller, scrolledTo);
      const afterwards = positionsOf(container, page);
      // Every card that was mounted and still is stands 400 further down the viewport: what the reader
      // scrolled, and nothing the cards coming into the window above did when they were measured.
      let kept = 0;
      for (const [step, y] of before) {
        const now = afterwards.get(step);
        if (now === undefined) continue;
        kept++;
        expect(now, `step ${step} after scrolling up in round ${round}`).toBeCloseTo(y + 400, 6);
      }
      expect(kept).toBeGreaterThan(3);
      if (scroller.scrollTop !== scrolledTo) corrections++;
    }
    // The cards coming into the window above were taller than the guess, so the list did have to correct.
    expect(corrections).toBeGreaterThan(0);
    expect(scroller.scrollTop).toBeGreaterThan(start - 10 * 400);
    // ...and it reached steps never measured before.
    expect(Math.min(...Array.from(cardRoots(container), (card) => stepOf(card)!))).toBeLessThan(97);
  });

  it('scrolling down into cards never measured leaves what is in view where it is', () => {
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_000);
    for (let round = 0; round < 10; round++) {
      const before = positionsOf(container, page);
      scrollTo(scroller, scroller.scrollTop + 400);
      const afterwards = positionsOf(container, page);
      let kept = 0;
      for (const [step, y] of before) {
        const now = afterwards.get(step);
        if (now === undefined) continue;
        kept++;
        expect(now, `step ${step} after scrolling down in round ${round}`).toBeCloseTo(y - 400, 6);
      }
      expect(kept).toBeGreaterThan(3);
    }
  });

  it('holds the reader’s place when a card above the viewport turns out taller, and when it turns out shorter', () => {
    const bump = new Map<number, number>();
    const page = stubPageLayout({ heightOf: (step) => heightOf(step) + (bump.get(step) ?? 0), viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_000);

    const aboveTheViewport = () => geometry(container, page).cards.filter((card) => card.bottom <= scroller.scrollTop);
    const above = aboveTheViewport();
    expect(above.length).toBeGreaterThan(1);

    // A font arrives, an image loads: the card just above the viewport grows by 150.
    const before = viewportOf(container, page);
    const scrollTopBefore = scroller.scrollTop;
    bump.set(above[above.length - 1].step, 150);
    act(() => resizeObserverOf(scroller).trigger());
    nextFrame();
    expect(scroller.scrollTop).toBe(scrollTopBefore + 150);
    expect(viewportOf(container, page)).toEqual(before);

    // ...and an earlier one shrinks by 100.
    const shrunk = above[0].step;
    bump.set(shrunk, -100);
    act(() => resizeObserverOf(scroller).trigger());
    nextFrame();
    expect(scroller.scrollTop).toBe(scrollTopBefore + 150 - 100);
    expect(viewportOf(container, page)).toEqual(before);
  });

  it('does not scroll the reader back when the page was scrolled in the frame a card resized, before the scroll was announced', () => {
    // A script, or a smooth scroll, moves the page during a frame; its scroll event comes with the next one,
    // and a resize notification can come first. The place held from before must not be restored over it.
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_000);

    scroller.scrollTop = 30_900; // no scroll event
    act(() => resizeObserverOf(scroller).trigger());
    expect(scroller.scrollTop).toBe(30_900);

    // ...and the window catches up with where the reader is, a frame later.
    nextFrame();
    expect(scroller.scrollTop).toBe(30_900);
    const mounted = geometry(container, page).cards;
    expect(mounted.some((card) => card.top <= 30_900 && card.bottom > 30_900)).toBe(true);
  });

  it('keeps the card the reader scrolled to, when a newer step arrives in the frame they scrolled, before the scroll was announced', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const steps = makeSteps(200);
    const { container, rerender } = render(<ShadowTimeline interactions={steps} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_000);

    scroller.scrollTop = 30_900; // 900 into step 103, no scroll event yet
    rerender(<ShadowTimeline interactions={[newestStep(), ...steps]} />);
    // Step 103 is where the reader was in it (the new step adds 300 above everything).
    expect(scroller.scrollTop).toBe(30_900 + 300);
    const card = geometry(container, page).cards.find((candidate) => candidate.step === 103)!;
    expect(card.top - scroller.scrollTop).toBe(0); // its top was at the viewport top (30,900) before
  });

  it('takes the reader’s place afresh when the window is redrawn in a frame after they scrolled, before the scroll was announced', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_000);

    // The pane is made taller: the list will draw more cards on the next frame...
    page.viewport = 1600;
    act(() => resizeObserverOf(scroller).trigger());
    // ...and before that frame the page is scrolled by something that does not announce it.
    scroller.scrollTop = 30_600;
    nextFrame();
    expect(scroller.scrollTop).toBe(30_600);
    expect(cardRoots(container).length).toBeGreaterThan(9);
  });

  it('writes the scroll position in whole pixels, whatever fractions the cards measure', () => {
    stubPageLayout({ heightOf: (step) => (step < 10 ? 300.4 : 500.7), viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_000);
    expect(scroller.scrollTop).not.toBe(30_000); // it did have to correct
    expect(Number.isInteger(scroller.scrollTop)).toBe(true);
  });

  it('does not move when a card at or below the top of the viewport changes height', () => {
    const bump = new Map<number, number>();
    const page = stubPageLayout({ heightOf: (step) => heightOf(step) + (bump.get(step) ?? 0), viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_000);
    const scrollTopBefore = scroller.scrollTop;
    const inView = viewportOf(container, page);

    // The card at the top of the viewport (the reader is partway into it) and the one below it both grow.
    bump.set(inView[0].step, 200);
    bump.set(inView[1].step, 300);
    act(() => resizeObserverOf(scroller).trigger());
    nextFrame();
    expect(scroller.scrollTop).toBe(scrollTopBefore);
  });

  it('makes no correction at all, at the top of the list, for the cards measured below', () => {
    stubPageLayout({ heightOf: () => 500, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    expect(container.querySelector<HTMLElement>('.overflow-y-auto')!.scrollTop).toBe(0);
  });

  it('remembers the height of every card it measured, by id, once it is out of the window', () => {
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;

    scrollTo(scroller, 30_000);
    scrollTo(scroller, 0);
    const { total, cards } = geometry(container, page);
    expect(cards[0].step).toBe(0);

    // Every card the page was asked to measure keeps its real height; the others are as tall as the first ones were.
    const estimate = 300;
    let expected = 0;
    for (let step = 0; step < 200; step++) expected += page.measured.has(step) ? heightOf(step) : estimate;
    expect(page.measured.size).toBeGreaterThan(6);
    expect(total).toBe(expected);
    expect(total).toBeGreaterThan(200 * 300);
  });

  it('keeps the heights it measured when the list changes, for the cards that are still in it', () => {
    const page = stubPageLayout({ heightOf, viewport: 800 });
    const steps = makeSteps(200);
    const { container, rerender } = render(<ShadowTimeline interactions={steps} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_000);
    scrollTo(scroller, 0);
    const before = geometry(container, page).total;
    expect(before).toBeGreaterThan(200 * 300);

    // One more step at the top: the reader is at the top, so it is mounted and measured (500). Every card
    // measured and then left out of the window is as tall as it was measured, not as tall as a guess.
    rerender(<ShadowTimeline interactions={[newestStep(), ...steps]} />);
    expect(geometry(container, page).total).toBe(before + heightOf(999));
  });

  it('measures again, and forgets what it knew, when the pane changes width', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_000);

    // Narrower, so every card wraps to 700.
    page.width = 300;
    page.heightOf = () => 700;
    act(() => resizeObserverOf(scroller).trigger());
    nextFrame();
    scrollTo(scroller, 0);

    const { total, cards } = geometry(container, page);
    // The cards not mounted are taken to be as tall as the ones that were measured again.
    expect(total).toBe(200 * 700);
    expect(cards[0].step).toBe(0);
  });
});

// ---------------------------------------------------------------------------

describe('a long timeline: the list changes', () => {
  it('keeps the card at the top of the viewport where it is when a newer step arrives above', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const steps = makeSteps(200);
    const { container, rerender } = render(<ShadowTimeline interactions={steps} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_150); // 150 into step 100
    const at = (step: number) => geometry(container, page).cards.find((card) => card.step === step)!.top - scroller.scrollTop;
    expect(at(100)).toBe(-150);

    rerender(<ShadowTimeline interactions={[newestStep(), ...steps]} />);
    expect(scroller.scrollTop).toBe(30_150 + 300);
    expect(at(100)).toBe(-150);
    expect(screen.getByText('201 interactions linked to this PR')).toBeInTheDocument();
  });

  it('draws only the cards on the page, once each, when a hundred newer steps arrive above a reader far down the list', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const steps = makeSteps(200);
    const { container, rerender } = render(<ShadowTimeline interactions={steps} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scrollTo(scroller, 30_000);
    const mountedBefore = cardRoots(container).map((card) => stepOf(card));
    drawn.ids.length = 0;

    rerender(<ShadowTimeline interactions={[...newerSteps(100), ...steps]} />);
    // The same cards are on the page, 100 places further down a longer list. Each is drawn once more, to say
    // so (its place in the list and the size of the list are in its markup), and no card that was not on
    // the page is drawn: the list worked out the right window for the new list at once rather than first
    // drawing the one for the old scroll position.
    expect(cardRoots(container).map((card) => stepOf(card))).toEqual(mountedBefore);
    expect([...drawn.ids].sort()).toEqual(mountedBefore.map((step) => stepId(step!)).sort());
    expect(scroller.scrollTop).toBe(30_000 + 100 * 300);
    expect(geometry(container, page).cards.find((card) => card.step === 100)!.top).toBe(scroller.scrollTop);
  });

  it('shows the newer step when the reader is at the top, rather than holding them below it', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const steps = makeSteps(200);
    const { container, rerender } = render(<ShadowTimeline interactions={steps} />);
    rerender(<ShadowTimeline interactions={[newestStep(), ...steps]} />);
    expect(container.querySelector<HTMLElement>('.overflow-y-auto')!.scrollTop).toBe(0);
    expect(stepOf(cardRoots(container)[0])).toBe(999);
    expect(screen.getByText('201 interactions linked to this PR')).toBeInTheDocument();
  });

  it('draws the new card and the cards on the page when a step is added at the top, and no other', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const steps = makeSteps(200);
    const { rerender, container } = render(<ShadowTimeline interactions={steps} />);
    drawn.ids.length = 0;
    rerender(<ShadowTimeline interactions={[newestStep(), ...steps]} />);
    // Every card on the page is one place further down a list one longer, which its markup says (aria-posinset
    // and aria-setsize), so each is drawn again, once; the new card is drawn; nothing else is.
    const onPage = cardRoots(container).map((card) => stepOf(card)!);
    expect(onPage[0]).toBe(999);
    expect([...drawn.ids].sort()).toEqual(onPage.map((step) => stepId(step)).sort());
  });

  it('goes from whole to windowed, and back, as the list grows past the threshold and shrinks again', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container, rerender } = render(<ShadowTimeline interactions={makeSteps(WINDOWING_THRESHOLD)} />);
    expect(cardRoots(container)).toHaveLength(WINDOWING_THRESHOLD);

    rerender(<ShadowTimeline interactions={makeSteps(200)} />);
    expect(cardRoots(container)).toHaveLength(6);
    expect(container.querySelector<HTMLElement>('.overflow-y-auto')!.style.overflowAnchor).toBe('none');

    rerender(<ShadowTimeline interactions={makeSteps(WINDOWING_THRESHOLD)} />);
    expect(cardRoots(container)).toHaveLength(WINDOWING_THRESHOLD);
    expect(container.querySelector<HTMLElement>('.overflow-y-auto')!.style.overflowAnchor).toBe('');
  });

  it('forgets the height of a step that is no longer in the list, and copes with the list emptying', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { rerender } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    rerender(<ShadowTimeline interactions={makeSteps(50)} />);
    expect(screen.getByText('50 interactions linked to this PR')).toBeInTheDocument();
    rerender(<ShadowTimeline interactions={[]} />);
    expect(screen.getByText('No cognitive history')).toBeInTheDocument();
    rerender(<ShadowTimeline interactions={makeSteps(200)} />);
    expect(screen.getByText('Step 0')).toBeInTheDocument();
  });

  it('works when the list first appears after loading', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const steps = makeSteps(200);
    const { container, rerender } = render(<ShadowTimeline interactions={[]} isLoading />);
    expect(screen.getByText('Loading cognitive history...')).toBeInTheDocument();
    rerender(<ShadowTimeline interactions={steps} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    expect(cardRoots(container)).toHaveLength(6);
    // ...and it is listening to the scroll position: the first card scrolled to is mounted.
    scrollTo(scroller, 30_000);
    expect(screen.getByText('Step 100')).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------

describe('a long timeline: entrance animation', () => {
  const entering = (container: HTMLElement) => cardRoots(container).filter((card) => card.classList.contains('rr-card-enter'));

  it('fades in the cards of the first draw, but not a card that scrolling brings back into the window', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    // All six, including the one the window grew by once the heights were measured.
    expect(entering(container)).toHaveLength(6);

    nextFrame(); // the reader looks at the page for a moment before scrolling
    scrollTo(scroller, 30_000);
    expect(cardRoots(container).length).toBeGreaterThan(0);
    expect(entering(container)).toHaveLength(0);

    scrollTo(scroller, 0); // steps 0 to 5 are mounted again
    expect(cardRoots(container).map((card) => stepOf(card))).toEqual([0, 1, 2, 3, 4, 5]);
    expect(entering(container)).toHaveLength(0);
  });

  it('fades in a step that arrives while it is in view', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const steps = makeSteps(200);
    const { container, rerender } = render(<ShadowTimeline interactions={steps} />);
    nextFrame();
    rerender(<ShadowTimeline interactions={[newestStep(), ...steps]} />);
    // (The cards of the first draw keep the class they were given, which does not play again.)
    expect(entering(container).map((card) => stepOf(card))).toContain(999);
    expect(cardRoots(container).find((card) => stepOf(card) === 999)!.classList.contains('rr-card-enter')).toBe(true);
  });

  it('fades in every card of a short list, as before', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(5)} />);
    expect(entering(container)).toHaveLength(5);
  });
});

// ---------------------------------------------------------------------------

describe('a long timeline: housekeeping', () => {
  it('stops listening when it is taken off the page', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container, unmount } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    const observer = resizeObserverOf(scroller);
    const removeListener = vi.spyOn(scroller, 'removeEventListener');
    unmount();
    expect(removeListener).toHaveBeenCalledWith('scroll', expect.any(Function));
    expect(observer.observed.size).toBe(0);
    // A notification that was already on its way finds nothing to do.
    expect(() => observer.trigger()).not.toThrow();
  });

  it('observes the cards that are mounted, and only those', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    const observer = resizeObserverOf(scroller);
    expect(observer.observed.has(scroller)).toBe(true);
    const cardsObserved = () => cardRoots(container).every((card) => observer.observed.has(card));
    expect(cardsObserved()).toBe(true);
    expect(observer.observed.size).toBe(1 + 6);

    scrollTo(scroller, 30_000);
    expect(cardsObserved()).toBe(true);
    expect(observer.observed.size).toBe(1 + cardRoots(container).length);
  });

  it('does not measure at all while the list is short', () => {
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(10)} />);
    scrollTo(container.querySelector<HTMLElement>('.overflow-y-auto')!, 500);
    expect(page.measured.size).toBe(0);
  });
});

// ---------------------------------------------------------------------------

describe('a long timeline: what a screen reader and a keyboard are given', () => {
  const listOf = (container: HTMLElement) => container.querySelector<HTMLElement>('ol')!;
  const headers = (container: HTMLElement) => Array.from(container.querySelectorAll<HTMLButtonElement>('button[aria-pressed]'));

  it('is a list with a name, and the cards that are on the page are its only children', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const list = listOf(container);
    expect(screen.getByRole('list', { name: 'Interactions linked to this PR' })).toBe(list);
    expect(list.children.length).toBeGreaterThan(0);
    for (const child of Array.from(list.children)) expect(child.tagName).toBe('LI');
    expect(screen.getAllByRole('listitem').length).toBe(list.children.length);
  });

  it('says where each card is in the whole list and how long the list is, as most of it is not on the page', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const position = () =>
      cardRoots(container).map((card) => [stepOf(card), card.getAttribute('aria-posinset'), card.getAttribute('aria-setsize')]);
    expect(position()).toEqual([0, 1, 2, 3, 4, 5].map((step) => [step, String(step + 1), '200']));

    scrollTo(container.querySelector<HTMLElement>('.overflow-y-auto')!, 30_000);
    expect(position()).toEqual([97, 98, 99, 100, 101, 102, 103, 104, 105].map((step) => [step, String(step + 1), '200']));
  });

  it('says it again, with the new length and the new places, when the list changes', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const steps = makeSteps(200);
    const { container, rerender } = render(<ShadowTimeline interactions={steps} />);
    rerender(<ShadowTimeline interactions={[newestStep(), ...steps]} />);
    const first = cardRoots(container)[0];
    expect(stepOf(first)).toBe(999);
    expect([first.getAttribute('aria-posinset'), first.getAttribute('aria-setsize')]).toEqual(['1', '201']);
    const old = cardRoots(container)[1];
    expect([stepOf(old), old.getAttribute('aria-posinset'), old.getAttribute('aria-setsize')]).toEqual([0, '2', '201']);
  });

  it('does not say it for a list that is on the page whole: the list itself counts', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(10)} />);
    for (const card of cardRoots(container)) {
      expect(card).not.toHaveAttribute('aria-posinset');
      expect(card).not.toHaveAttribute('aria-setsize');
    }
    expect(screen.getAllByRole('listitem')).toHaveLength(10);
  });

  it('gives the stand-in for a step that could not be drawn its place in the list too', () => {
    vi.spyOn(console, 'error').mockImplementation(() => {}); // React logs the error it handed to the boundary
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    // It sorts between steps 2 and 3, so it is the fourth entry.
    const broken = makeNode({ ...undrawableNode, timestamp: BASE - 2500 });
    const { container } = render(<ShadowTimeline interactions={[...makeSteps(200), broken]} />);
    const entry = screen.getByText('This step could not be displayed.').closest('li')!;
    expect(listOf(container).contains(entry)).toBe(true);
    expect([entry.getAttribute('aria-posinset'), entry.getAttribute('aria-setsize')]).toEqual(['4', '201']);
    expect(entry.querySelector('button')).toBeNull();
  });

  it('puts only the cards that are on the page in the tab order: one header button each, not one for every step', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    expect(headers(container)).toHaveLength(cardRoots(container).length);
    expect(headers(container).length).toBeLessThan(20);
    for (const card of cardRoots(container)) expect(card).not.toHaveAttribute('tabindex');

    // Scrolled somewhere else, it is the cards there that can be reached.
    scrollTo(container.querySelector<HTMLElement>('.overflow-y-auto')!, 30_000);
    expect(headers(container)).toHaveLength(cardRoots(container).length);
    expect(headers(container).map((header) => header.closest('li')!.textContent!.includes('Step 100')).filter(Boolean)).toHaveLength(1);
  });

  it('keeps going from card to card: the next card after the last one that is on the page is not far below it', () => {
    // A keyboard user tabs to the last card on the page, the browser scrolls it into view, and the window
    // then holds the cards after it: there is always a viewport of cards below the one in view.
    const page = stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(200)} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    let focused = 0;
    for (let tabs = 0; tabs < 60; tabs++) {
      const cards = geometry(container, page).cards;
      const next = cards.find((card) => card.step === focused);
      expect(next, `step ${focused} is on the page after ${tabs} tabs`).toBeDefined();
      headerOf(next!.element).focus();
      // The browser scrolls a focused element into view: the least it takes to show it, here at the bottom.
      if (next!.bottom > scroller.scrollTop + page.viewport) scrollTo(scroller, next!.bottom - page.viewport);
      focused++;
    }
    expect(focused).toBe(60);
  });
});

