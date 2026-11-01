/**
 * Round 3 QA of issue #118: the two places where the windowed list moves the reader for the search that the random walks
 * could not tell from a list that did nothing.
 *
 *  - Next and previous leave the page where it is when the match is in view already, and "in view" means its top is on the
 *    page and at least 160 pixels of it (or all of it, if it is shorter). A card that shows a sliver at the foot of the page
 *    is not in view: the reader would have to scroll for it. The boundary is tested on both sides of the 160.
 *  - When the file the "touches the open file" filter means changes under the reader, the list of steps is a different
 *    one, and the reader is at its top with the cards of its top on the page: the window is told at once, and does not
 *    wait for the page to report a scroll a frame later.
 *
 * jsdom has no layout, so the page is given one (src/test/layoutStubs.ts): "Step N" is how a card is told.
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { makeNode, toolCall } from '../../test/fixtures';
import { FakeResizeObserver, stepOf, stubPageLayout } from '../../test/layoutStubs';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);
vi.setConfig({ testTimeout: 30_000 });

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

/** Steps 0..count-1 as they are listed; each says "Step n" on its own. `extra` says more. */
function makeSteps(count: number, extra: (n: number) => Partial<InteractionNode> = () => ({})): InteractionNode[] {
  return Array.from({ length: count }, (_, n) =>
    makeNode({ id: stepId(n), author: 'system', timestamp: BASE - n * 1000, user_prompt: `[tool_result Bash] output ${n} needle`, model_response: `Step ${n}`, ...extra(n) }),
  );
}

const box = () => screen.getByRole('searchbox', { name: 'Search steps' }) as HTMLInputElement;
const typeQuery = (value: string) => fireEvent.change(box(), { target: { value } });
const press = (key: string, init: KeyboardEventInit = {}) => fireEvent.keyDown(box(), { key, ...init });
const scrollerOf = (container: HTMLElement) => container.querySelector<HTMLElement>('.overflow-y-auto')!;
const cards = (container: HTMLElement) => Array.from(container.querySelectorAll<HTMLElement>('.flow-root > *'));
const mountedSteps = (container: HTMLElement) => cards(container).map((card) => stepOf(card));
const currentStep = (container: HTMLElement) => cards(container).filter((card) => card.getAttribute('aria-current') === 'true').map((card) => stepOf(card));
const nextFrame = () => act(() => void vi.advanceTimersByTime(20));

describe('next and previous: a card that shows only a sliver of itself is not in view', () => {
  // Every card is 300 tall and every step matches, so each Enter goes to the card below. The cards start at 0, 300, 600: the
  // third one, step 2, has its top at 600. The page is `viewport` tall, so it shows `viewport - 600` pixels of that card.
  function enterTwice(viewport: number) {
    stubPageLayout({ heightOf: () => 300, viewport });
    const { container } = render(<ShadowTimeline interactions={makeSteps(120)} />);
    typeQuery('needle');
    const scroller = scrollerOf(container);
    expect(scroller.scrollTop).toBe(0);
    press('Enter'); // step 1: top 300, all of it in view
    nextFrame();
    expect(scroller.scrollTop).toBe(0);
    press('Enter'); // step 2: top 600
    nextFrame();
    return { container, scroller };
  }

  it('is left where it is when 160 pixels of it are in view (the whole of the 160 counts)', () => {
    const { container, scroller } = enterTwice(760); // 600 + 160 = 760: exactly enough
    expect(currentStep(container)).toEqual([2]);
    expect(scroller.scrollTop).toBe(0);
  });

  it('is brought to the top when 159 pixels of it are in view', () => {
    const { container, scroller } = enterTwice(759);
    expect(currentStep(container)).toEqual([2]);
    expect(scroller.scrollTop).toBe(600);
  });

  it('is brought to the top when only a sliver of it is in view, and not when most of it is', () => {
    const sliver = enterTwice(620); // 20 pixels
    expect(sliver.scroller.scrollTop).toBe(600);
    cleanup();
    const most = enterTwice(880); // 280 pixels
    expect(most.scroller.scrollTop).toBe(0);
  });

  it('counts a card shorter than 160 pixels as in view when all of it is', () => {
    stubPageLayout({ heightOf: () => 100, viewport: 750 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(120)} />);
    typeQuery('needle');
    const scroller = scrollerOf(container);
    // Cards of 100 pixels: step 6 spans 600-700, all in a page of 750; step 7 spans 700-800, half of it.
    for (let k = 0; k < 6; k++) press('Enter');
    nextFrame();
    expect(currentStep(container)).toEqual([6]);
    expect(scroller.scrollTop).toBe(0);
    press('Enter');
    nextFrame();
    expect(currentStep(container)).toEqual([7]);
    // 700 + min(100, 160) = 800 > 750: the whole of the card is the least that shows, and it does not: the page moves.
    expect(scroller.scrollTop).toBe(700);
  });

  it('brings a card above the page to the top, and moves the page only as far as it has to', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container } = render(<ShadowTimeline interactions={makeSteps(120)} />);
    typeQuery('needle');
    const scroller = scrollerOf(container);
    for (let k = 0; k < 3; k++) press('Enter'); // step 3: top 900, off the foot of the page (0-800)
    nextFrame();
    expect(currentStep(container)).toEqual([3]);
    expect(scroller.scrollTop).toBe(900);
    press('Enter'); // step 4: top 1200, in the page (900-1700) with 300 of it showing
    nextFrame();
    expect(scroller.scrollTop).toBe(900);
    press('Enter', { shiftKey: true }); // back to step 3, which is at the top
    press('Enter', { shiftKey: true }); // step 2: top 600, above the page
    nextFrame();
    expect(currentStep(container)).toEqual([2]);
    expect(scroller.scrollTop).toBe(600);
    press('Enter', { shiftKey: true }); // step 1: top 300, above the page
    nextFrame();
    expect(currentStep(container)).toEqual([1]);
    expect(scroller.scrollTop).toBe(300);
  });
});

describe('next and previous in a list that is not windowed: the same rule, asked of the page', () => {
  // Ten steps are all on the page at once, so the page is asked where each card is (a list that is windowed keeps its own table
  // of heights). jsdom has no layout: a card is `height` tall, and its top is its place in the list less how far the page has scrolled.
  function onPage(height: number, viewport: number) {
    stubPageLayout({ heightOf: () => height, viewport });
    const original = HTMLElement.prototype.getBoundingClientRect;
    HTMLElement.prototype.getBoundingClientRect = function (this: HTMLElement) {
      const rect = (top: number, tall: number) => ({ x: 0, y: top, top, left: 0, right: 0, bottom: top + tall, width: 0, height: tall, toJSON: () => ({}) }) as DOMRect;
      if (this.classList.contains('overflow-y-auto')) return rect(0, viewport);
      const parent = this.parentElement;
      if (parent !== null && parent.classList.contains('flow-root')) {
        const scroller = parent.closest<HTMLElement>('.overflow-y-auto');
        return rect(Array.prototype.indexOf.call(parent.children, this) * height - (scroller?.scrollTop ?? 0), height);
      }
      return rect(0, 0);
    };
    return () => {
      HTMLElement.prototype.getBoundingClientRect = original;
    };
  }

  /** `keys` is a walk through the matches: n is Enter (the next one) and p is Shift+Enter (the one before). */
  function walk(height: number, viewport: number, keys: string) {
    const done = onPage(height, viewport);
    try {
      const { container } = render(<ShadowTimeline interactions={makeSteps(10)} />);
      typeQuery('needle');
      const scroller = scrollerOf(container);
      expect(cards(container)).toHaveLength(10); // all of them on the page: not windowed
      expect(scroller.scrollTop).toBe(0);
      for (const key of keys) press('Enter', key === 'p' ? { shiftKey: true } : {});
      nextFrame();
      return { current: currentStep(container), scrollTop: scroller.scrollTop };
    } finally {
      done();
    }
  }

  it('leaves the page where it is when 160 pixels of the card are in view, and moves it to the card when 159 are', () => {
    // Cards of 300: the third starts at 600, so a page of 760 shows 160 of it.
    expect(walk(300, 760, 'nn')).toEqual({ current: [2], scrollTop: 0 });
    cleanup();
    expect(walk(300, 759, 'nn')).toEqual({ current: [2], scrollTop: 600 });
    cleanup();
    expect(walk(300, 620, 'nn')).toEqual({ current: [2], scrollTop: 600 }); // a sliver
    cleanup();
    expect(walk(300, 880, 'nn')).toEqual({ current: [2], scrollTop: 0 }); // most of it
  });

  it('counts a card shorter than 160 pixels as in view when all of it is, and not when part of it is not', () => {
    // Cards of 100: step 6 spans 600-700, in a page of 700 (all of it); step 7 spans 700-800, out of it.
    expect(walk(100, 700, 'nnnnnn')).toEqual({ current: [6], scrollTop: 0 });
    cleanup();
    expect(walk(100, 699, 'nnnnnn')).toEqual({ current: [6], scrollTop: 600 });
    cleanup();
    expect(walk(100, 750, 'nnnnnnn')).toEqual({ current: [7], scrollTop: 700 });
  });

  it('moves the page only as far as the card, from where the page is, going down and going up', () => {
    // A page of 620 and cards of 300. Step 2 (600) shows 20 of itself: the page moves to 600. Step 3 (900) is then 300 down the page
    // with all of it in view: the page stays. Step 4 (1200) is 600 down with 20 in view: the page moves by 600, to 1200.
    expect(walk(300, 620, 'nnn')).toEqual({ current: [3], scrollTop: 600 });
    cleanup();
    expect(walk(300, 620, 'nnnn')).toEqual({ current: [4], scrollTop: 1200 });
    cleanup();
    // Going back: step 3 (900) is above the page (1200), and the page moves up to it; so does step 2 (600) from 900.
    expect(walk(300, 620, 'nnnnp')).toEqual({ current: [3], scrollTop: 900 });
    cleanup();
    expect(walk(300, 620, 'nnnnpp')).toEqual({ current: [2], scrollTop: 600 });
  });
});

describe('the file filter follows the open file in a windowed list', () => {
  // Steps with an even number touched src/a.ts and the others src/b.ts: seventy-five steps for each file, so the filtered list is
  // itself windowed. A step says its file only in its tool call.
  const steps = makeSteps(150, (n) => ({ tool_executions: [toolCall('Read', { file_path: n % 2 === 0 ? '/repo/src/a.ts' : '/repo/src/b.ts' })] }));

  it('opens the list of the new file at its top, with the cards of its top on the page', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container, rerender } = render(<ShadowTimeline interactions={steps} openFile="src/a.ts" />);
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Touches the open file/ }));
    const scroller = scrollerOf(container);
    expect(cards(container)[0].getAttribute('aria-setsize')).toBe('75');
    scroller.scrollTop = 12_000;
    fireEvent.scroll(scroller);
    nextFrame();
    // Far down the list of src/a.ts: its steps, 40 cards in.
    expect(mountedSteps(container).every((step) => step !== null && step % 2 === 0 && step > 50)).toBe(true);

    rerender(<ShadowTimeline interactions={steps} openFile="src/b.ts" />);
    // No frame needed: the page is at the top of the list of src/b.ts and the cards on it are the first ones.
    expect(scroller.scrollTop).toBe(0);
    expect(cards(container)[0].getAttribute('aria-setsize')).toBe('75');
    expect(cards(container)[0].getAttribute('aria-posinset')).toBe('1');
    expect(mountedSteps(container)).toEqual([1, 3, 5, 7, 9, 11]);
  });

  it('does the same when the file is switched back, and when the person had not scrolled', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container, rerender } = render(<ShadowTimeline interactions={steps} openFile="src/a.ts" />);
    fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Touches the open file/ }));
    const scroller = scrollerOf(container);
    expect(mountedSteps(container)).toEqual([0, 2, 4, 6, 8, 10]);
    rerender(<ShadowTimeline interactions={steps} openFile="src/b.ts" />);
    expect(mountedSteps(container)).toEqual([1, 3, 5, 7, 9, 11]);
    scroller.scrollTop = 3_000;
    fireEvent.scroll(scroller);
    nextFrame();
    expect(mountedSteps(container)[0]).toBeGreaterThan(1);
    rerender(<ShadowTimeline interactions={steps} openFile="src/a.ts" />);
    expect(scroller.scrollTop).toBe(0);
    expect(mountedSteps(container)).toEqual([0, 2, 4, 6, 8, 10]);
  });

  it('leaves the page alone when the open file changes and the filter is off', () => {
    stubPageLayout({ heightOf: () => 300, viewport: 800 });
    const { container, rerender } = render(<ShadowTimeline interactions={steps} openFile="src/a.ts" />);
    const scroller = scrollerOf(container);
    scroller.scrollTop = 6_000;
    fireEvent.scroll(scroller);
    nextFrame();
    const before = mountedSteps(container);
    rerender(<ShadowTimeline interactions={steps} openFile="src/b.ts" />);
    nextFrame();
    expect(scroller.scrollTop).toBe(6_000);
    expect(mountedSteps(container)).toEqual(before);
  });
});
