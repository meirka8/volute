/**
 * A stimulus whose markers verify can be as long as a crafted node is allowed to be: tens of thousands of
 * parts, or one part with thousands of notes. Each list-shaped part of it draws one page and offers "Show N
 * more ..." for the rest, as the lists of a version 1 stimulus do (`TimelineNode.limits.test.tsx`), and
 * reading it costs time in proportion to its length and no more.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { RENDER_LIMITS } from './limits';
import { TimelineNode } from './TimelineNode';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { makeNode } from '../../test/fixtures';
import { boundaryToken } from '../../lib/stimulusV2';
import { writeStimulus, type PartSpec } from '../../test/stimulusWriter';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

const ID = '1f0e5c3a-9d2b-8c4e-a1b7-0000000000bc';

const step = (parts: readonly PartSpec[], author: InteractionNode['author'] = 'system', options = {}): InteractionNode =>
  makeNode({ id: ID, author, model_name: 'claude-test', user_prompt: writeStimulus(ID, parts, options) });

function renderNode(node: InteractionNode) {
  const view = render(<TimelineNode interaction={node} />);
  return { ...view, card: view.container.firstElementChild as HTMLElement };
}

const paneOf = (card: HTMLElement) => card.querySelector('[data-layout="transcript"] > .rr-thought') as HTMLElement;
const moreButtons = (root: HTMLElement) =>
  within(root)
    .queryAllByRole('button', { name: /^Show [\d,]+ more / })
    .map((button) => button.textContent);

describe('the limit on the pieces of one part', () => {
  it('is what a capture writes for a part: its head, a note and its tail', () => {
    expect(RENDER_LIMITS.contentPieces).toBe(3);
  });

  it('draws a cut part whole: three pieces are a page', () => {
    const { card } = renderNode(step([{ kind: 'tool_result', name: 'Read', cut: { head: 'head', bytes: 12, tail: 'tail' } }], 'human'));
    expect(moreButtons(paneOf(card))).toEqual([]);
    expect(within(paneOf(card)).getByText('… 12 bytes truncated')).toBeInTheDocument();
  });
});

describe('a part with thousands of notes', () => {
  const boundary = boundaryToken(ID, 'tool_result', 0);
  const crafted = (notes: number) => {
    // A part the grammar allows and no capture writes: a note after every word.
    const content = Array.from({ length: notes }, (_, index) => `word ${index}\n…[cvc: truncated ${index + 1} bytes ${boundary}]`).join('\n');
    return makeNode({ id: ID, author: 'human', model_name: 'claude-test', user_prompt: `[tool_result Bash ${boundary}] ${content}` });
  };

  it('is a verified stimulus that draws one page of its pieces and offers the rest', () => {
    const started = performance.now();
    const { card } = renderNode(crafted(5000));
    const pane = paneOf(card);
    // The pieces are a word, a note, a word, a note...: the first page is the first word, its note and the next word.
    expect(within(pane).getByText('… 1 bytes truncated')).toBeInTheDocument();
    expect(within(pane).queryByText('… 2 bytes truncated')).not.toBeInTheDocument();
    expect(pane.querySelectorAll('pre')).toHaveLength(2);
    // 5,000 words and notes make 10,000 pieces, three of them drawn.
    expect(moreButtons(pane)).toEqual(['Show 3 more pieces (9,997 not shown)']);
    expect(pane.querySelectorAll('*').length).toBeLessThan(150);
    expect(performance.now() - started).toBeLessThan(5000);
  });

  it('draws a page more per click, in order', () => {
    const { card } = renderNode(crafted(40));
    const pane = paneOf(card);
    fireEvent.click(within(pane).getByRole('button', { name: /^Show 3 more pieces/ }));
    // The next page: the second note, the next word, the third note.
    expect(within(pane).getByText('… 2 bytes truncated')).toBeInTheDocument();
    expect(within(pane).getByText('… 3 bytes truncated')).toBeInTheDocument();
    expect(within(pane).queryByText('… 4 bytes truncated')).not.toBeInTheDocument();
    expect(pane.querySelectorAll('pre')).toHaveLength(3);
    expect(Array.from(pane.querySelectorAll('pre')).map((pre) => pre.textContent)).toEqual(['word 0', 'word 1', 'word 2']);
  });
});

describe('a stimulus of thousands of parts', () => {
  it('draws one page of tool results, however many there are, behind one line', () => {
    const parts = Array.from({ length: 4000 }, (_, index): PartSpec => ({ kind: 'tool_result', name: 'T', text: `out${index}` }));
    const started = performance.now();
    const { card } = renderNode(step(parts, 'system'));
    expect(paneOf(card).querySelectorAll('pre')).toHaveLength(0);
    fireEvent.click(within(card).getByRole('button', { name: 'Tool results · T ×4000' }));
    expect(paneOf(card).querySelectorAll('pre')).toHaveLength(20);
    expect(moreButtons(paneOf(card))).toEqual(['Show 20 more tool results (3,980 not shown)']);
    expect(performance.now() - started).toBeLessThan(5000);
  });

  it('draws a page of blocks of the other kinds, and the closing note after the last of them', () => {
    const parts = Array.from({ length: 2000 }, (_, index): PartSpec => ({ kind: 'prompt', text: `paragraph ${index}` }));
    const { card } = renderNode(step(parts, 'human', { omitted: 5 }));
    const pane = paneOf(card);
    expect(pane.querySelectorAll('p.font-serif')).toHaveLength(RENDER_LIMITS.stimulusBlocks);
    expect(moreButtons(pane)).toEqual(['Show 100 more parts (1,900 not shown)']);
    expect(within(pane).queryByText('5 more parts not recorded')).not.toBeInTheDocument();
  });

  it('draws a bounded number of elements on first draw, whatever the parts are made of', () => {
    // Blocks of every kind, with a cut result after each: the most a stimulus of this size can ask a card to draw at once.
    const parts: PartSpec[] = [];
    for (let index = 0; index < 600; index++) {
      parts.push({ kind: index % 2 === 0 ? 'meta' : 'prompt', text: `block ${index}` });
      for (let result = 0; result < 3; result++) parts.push({ kind: 'tool_result', name: 'Read', cut: { head: 'h', bytes: 9, tail: 't' } });
    }
    const started = performance.now();
    const { card } = renderNode(step(parts, 'human'));
    const drawn = card.querySelectorAll('*').length;
    // About 2,200 elements: a page of 100 blocks, and in each group a page of results, each with at most three pieces.
    expect(drawn).toBeLessThan(4_000);
    expect(performance.now() - started).toBeLessThan(10_000);
  });
});
