/**
 * A timeline of 10,000 steps, the most a FORMAT5 capture may hold (issue #135).
 *
 * Drawing every card of such a timeline took the browser far longer than a person
 * waits and put hundreds of thousands of elements on the page. The windowed list
 * draws the cards near the viewport, so what it costs, and how many elements it
 * puts on the page, depends on the size of the screen and not on the length of the
 * capture.
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { makeNode, toolCall } from '../../test/fixtures';
import { stepOf, stubPageLayout } from '../../test/layoutStubs';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

const COUNT = 10_000;
const BASE = Date.parse('2026-03-02T09:15:00.000Z');
const hex = (n: number) => n.toString(16).padStart(12, '0');

/**
 * Ten thousand steps of the four kinds a Claude Code capture is made of, newest
 * first: a prompt with a command, a result with a response, an MCP thought, and a
 * prompt with an edit. Each says "Step N".
 */
function bigTimeline(count: number): InteractionNode[] {
  return Array.from({ length: count }, (_, n) => {
    const timestamp = BASE - n * 1000;
    switch (n % 4) {
      case 0:
        return makeNode({
          id: `4a1c9e07-3b52-8d6f-9a0e-${hex(n)}`,
          author: 'human',
          timestamp,
          model_name: 'claude-opus-4-5-20251101',
          user_prompt: `Step ${n}`,
          model_response: 'Running the tests.',
          tool_executions: [toolCall('Bash', { command: 'npm test', description: 'Run the tests' })],
        });
      case 1:
        return makeNode({
          id: `4a1c9e07-3b52-8d6f-9a0e-${hex(n)}`,
          author: 'system',
          timestamp,
          model_name: 'claude-opus-4-5-20251101',
          user_prompt: '[tool_result Bash] 12 passed',
          model_response: `Step ${n}`,
        });
      case 2:
        // Not a version 8 id and an agent author: the Thought / Action / Observation layout.
        return makeNode({
          id: `4a1c9e07-3b52-4d6f-9a0e-${hex(n)}`,
          author: 'agent',
          timestamp,
          model_name: 'agent',
          user_prompt: `Step ${n}`,
          model_response: 'Replaced the fixed delay with a capped backoff.',
        });
      default:
        return makeNode({
          id: `4a1c9e07-3b52-8d6f-9a0e-${hex(n)}`,
          author: 'human',
          timestamp,
          model_name: 'claude-opus-4-5-20251101',
          user_prompt: `Step ${n}`,
          tool_executions: [
            toolCall('Edit', { file_path: '/repo/src/a.ts', old_string: 'const a = 1;\nconst b = 2;', new_string: 'const a = 1;\nconst b = 3;' }),
          ],
        });
    }
  });
}

const HEIGHTS = [260, 180, 300, 420];
const heightOf = (step: number) => HEIGHTS[step % 4];

/**
 * How long the first frame of the timeline takes: the fastest of a few draws, so that a machine busy with other
 * work (the test files run side by side) does not fail a check about how much work the timeline does.
 * Leaves the last draw on the page.
 */
function firstFrame(nodes: InteractionNode[]) {
  let best = Infinity;
  let view: ReturnType<typeof render> | null = null;
  for (let attempt = 0; attempt < 3; attempt++) {
    view?.unmount();
    const started = performance.now();
    view = render(<ShadowTimeline interactions={nodes} />);
    best = Math.min(best, performance.now() - started);
  }
  return { elapsed: best, container: view!.container };
}

/** How many elements the timeline put on the page. */
const elementsIn = (container: HTMLElement) => container.querySelectorAll('*').length;
const cardRoots = (container: HTMLElement) => Array.from(container.querySelectorAll<HTMLElement>('.flow-root > *'));

describe(`a timeline of ${COUNT.toLocaleString('en-US')} steps`, () => {
  const nodes = bigTimeline(COUNT);

  it('draws its first frame in under a second, in far fewer than 5,000 elements, even when the page cannot be measured', () => {
    // No layout at all: jsdom reports every card as zero tall, the worst case for a list that
    // decides how many cards to draw by how tall they are.
    const { elapsed, container } = firstFrame(nodes);

    expect(screen.getByText(`${COUNT} interactions linked to this PR`)).toBeInTheDocument();
    expect(elapsed).toBeLessThan(1000);
    expect(elementsIn(container)).toBeLessThan(5000);
    expect(cardRoots(container).length).toBeLessThan(100);
    expect(cardRoots(container).length).toBeGreaterThan(0);
  });

  it('draws its first frame in under a second when the page can be measured, in a few dozen cards', () => {
    stubPageLayout({ heightOf, viewport: 900 });
    const { elapsed, container } = firstFrame(nodes);

    expect(elapsed).toBeLessThan(1000);
    expect(elementsIn(container)).toBeLessThan(5000);
    // 900 of viewport and 900 of overscan below it, at 180 to 420 a card.
    expect(cardRoots(container).length).toBeLessThan(20);
    expect(cardRoots(container).length).toBeGreaterThan(3);
    expect(stepOf(cardRoots(container)[0])).toBe(0);
  });

  it('keeps the number of elements on the page bounded at every scroll position, from the top to the bottom', () => {
    const page = stubPageLayout({ heightOf, viewport: 900 });
    const { container } = render(<ShadowTimeline interactions={nodes} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    const total = () => {
      const list = scroller.firstElementChild as HTMLElement;
      return (parseFloat(list.style.paddingTop) || 0) + (parseFloat(list.style.paddingBottom) || 0);
    };

    let most = 0;
    const started = performance.now();
    // A walk down the list in pages, then a few jumps (a drag on the scrollbar).
    const positions = [0, 900, 1800, 2700, 40_000, 1_000_000, 1_500_000, 2_000_000, 100, 2_400_000];
    for (const position of positions) {
      scroller.scrollTop = position;
      fireEvent.scroll(scroller);
      most = Math.max(most, elementsIn(container));
      expect(cardRoots(container).length).toBeLessThan(40);
      expect(total()).toBeGreaterThan(0);
    }
    expect(most).toBeLessThan(5000);
    // ...and to the very end of the list, wherever the estimates put it.
    for (let round = 0; round < 40; round++) {
      scroller.scrollTop = scroller.scrollHeight || Number.MAX_SAFE_INTEGER;
      scroller.scrollTop = 1e9;
      fireEvent.scroll(scroller);
    }
    expect(elementsIn(container)).toBeLessThan(5000);
    expect(page.measured.size).toBeGreaterThan(10);
    // Ten scroll events and forty more, each deciding what to draw over ten thousand steps. About half a second on a
    // quiet machine; the bound is loose because the test files run side by side, and it is there to catch a list that
    // does work for every step on every scroll (which takes minutes), not to time one.
    expect(performance.now() - started).toBeLessThan(5000);
  });

  it('reaches the last step, and mounts it, when scrolled to the end', () => {
    stubPageLayout({ heightOf, viewport: 900 });
    const { container } = render(<ShadowTimeline interactions={nodes} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scroller.scrollTop = 1e9;
    fireEvent.scroll(scroller);
    expect(screen.getByText(`Step ${COUNT - 1}`)).toBeInTheDocument();
    const list = scroller.firstElementChild as HTMLElement;
    expect(parseFloat(list.style.paddingBottom) || 0).toBe(0);
  });

  it('selects the step clicked, a long way down the list', () => {
    stubPageLayout({ heightOf, viewport: 900 });
    const onSelect = vi.fn();
    const { container } = render(<ShadowTimeline interactions={nodes} onSelectInteraction={onSelect} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    act(() => {
      scroller.scrollTop = 1_000_000;
    });
    fireEvent.scroll(scroller);
    const card = cardRoots(container)[0];
    const step = stepOf(card)!;
    fireEvent.click(card);
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenCalledWith(nodes[step].id);
  });

  it('draws a step of either layout from anywhere in the list', () => {
    stubPageLayout({ heightOf, viewport: 900 });
    const { container } = render(<ShadowTimeline interactions={nodes} />);
    const scroller = container.querySelector<HTMLElement>('.overflow-y-auto')!;
    scroller.scrollTop = 600_000;
    fireEvent.scroll(scroller);
    const layouts = new Set(cardRoots(container).map((card) => (card.querySelector('[data-layout="transcript"]') ? 'transcript' : 'triad')));
    expect(layouts).toEqual(new Set(['transcript', 'triad']));
  });
});
