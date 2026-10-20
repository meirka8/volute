/**
 * Lists far longer than a screen. A node can be megabytes of crafted text, and a
 * card that drew every item of it would be hundreds of thousands of DOM elements,
 * so each list-shaped part draws one page and offers "Show N more ..." for the
 * rest. The data behind each list is untouched: the caps apply when it is drawn.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { RENDER_LIMITS } from './limits';
import { TimelineNode } from './TimelineNode';
import { describeToolCall } from '../../lib/toolCalls';
import { WRAPPED_CHAR_BUDGET } from '../../lib/textLines';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { bigListStep, bigStimulusStep, makeNode, rawToolCall, toolCall } from '../../test/fixtures';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

const HARNESS_ID = '1f0e5c3a-9d2b-8c4e-a1b7-0000000000bb';

function renderNode(node: InteractionNode, onSelect: () => void = () => {}) {
  const view = render(<TimelineNode interaction={node} onSelect={onSelect} />);
  return { ...view, card: view.container.firstElementChild as HTMLElement };
}

const stimulusOf = (card: HTMLElement) => card.querySelector('.rr-thought') as HTMLElement;
const toolCardsOf = (card: HTMLElement) => Array.from(card.querySelectorAll<HTMLElement>('[data-status]'));
const toolCard = (card: HTMLElement, title: string) => {
  const match = toolCardsOf(card).find((el) => el.firstElementChild!.firstElementChild!.textContent === title);
  if (!match) throw new Error(`no tool card ${title}`);
  return match;
};
/** The "Show N more ..." buttons inside `root`, by their text. */
const moreButtons = (root: HTMLElement) =>
  within(root)
    .queryAllByRole('button', { name: /^Show [\d,]+ more / })
    .map((button) => button.textContent);
const clickMore = (root: HTMLElement, name: RegExp | string) => fireEvent.click(within(root).getByRole('button', { name }));
/** Clicks "Show N more tool calls" until the step has drawn every card (the lists inside the cards keep their own pages). */
function showAllToolCalls(card: HTMLElement) {
  for (let click = 0; click < 60; click++) {
    const [label] = moreButtons(card).filter((text) => /more tool calls?\b/.test(text ?? ''));
    if (!label) return;
    clickMore(card, label);
  }
  throw new Error('"Show more tool calls" kept offering more');
}

describe('the limits', () => {
  it('are the ones in the specification', () => {
    expect(RENDER_LIMITS).toEqual({
      stimulusBlocks: 100,
      toolResultsPerGroup: 20,
      todos: 100,
      fieldRows: 25,
      toolCalls: 10,
      edits: 5,
    });
  });
});

describe('tool results in a group', () => {
  it('draws 20 of 70 on a step that opens them, and a page more per click', () => {
    const { card } = renderNode(bigListStep);
    const stimulus = stimulusOf(card);
    expect(stimulus.querySelectorAll('pre')).toHaveLength(20);
    expect(within(stimulus).getByText('result 20 of 70')).toBeInTheDocument();
    expect(within(stimulus).queryByText('result 21 of 70')).not.toBeInTheDocument();
    // The summary still counts all 70.
    expect(within(stimulus).getByRole('button', { name: 'Tool results · Bash ×70' })).toBeInTheDocument();

    expect(moreButtons(stimulus)).toEqual(['Show 20 more tool results (50 not shown)']);
    clickMore(stimulus, 'Show 20 more tool results (50 not shown)');
    expect(stimulus.querySelectorAll('pre')).toHaveLength(40);
    clickMore(stimulus, 'Show 20 more tool results (30 not shown)');
    expect(stimulus.querySelectorAll('pre')).toHaveLength(60);
    expect(moreButtons(stimulus)).toEqual(['Show 10 more tool results']); // the last page says no more than that
    clickMore(stimulus, 'Show 10 more tool results');
    expect(stimulus.querySelectorAll('pre')).toHaveLength(70);
    expect(moreButtons(stimulus)).toEqual([]);
    expect(within(stimulus).getByText('result 70 of 70')).toBeInTheDocument();
  });

  it('keeps the results in order across pages', () => {
    const { card } = renderNode(bigListStep);
    const stimulus = stimulusOf(card);
    for (let i = 0; i < 3; i++) fireEvent.click(within(stimulus).getByRole('button', { name: /^Show \d+ more tool results/ }));
    const shown = Array.from(stimulus.querySelectorAll('pre')).map((pre) => pre.textContent);
    expect(shown).toEqual(Array.from({ length: 70 }, (_, i) => `result ${i + 1} of 70`));
  });

  it('draws a closed group as one line, however many results it holds', () => {
    const node = makeNode({
      id: HARNESS_ID,
      author: 'system',
      user_prompt: Array.from({ length: 4000 }, (_, i) => `[tool_result T] out${i}`).join('\n\n'),
    });
    const { card } = renderNode(node);
    expect(stimulusOf(card).querySelectorAll('pre')).toHaveLength(0);
    expect(moreButtons(stimulusOf(card))).toEqual([]); // nothing to page until it is opened
    fireEvent.click(within(card).getByRole('button', { name: 'Tool results · T ×4000' }));
    expect(stimulusOf(card).querySelectorAll('pre')).toHaveLength(20);
    expect(moreButtons(stimulusOf(card))).toEqual(['Show 20 more tool results (3,980 not shown)']);
  });

  it('remembers how many are drawn when the group is folded and opened again', () => {
    const { card } = renderNode(bigListStep);
    const stimulus = stimulusOf(card);
    clickMore(stimulus, /^Show 20 more tool results/);
    const toggle = within(stimulus).getByRole('button', { name: 'Tool results · Bash ×70' });
    fireEvent.click(toggle);
    expect(stimulus.querySelectorAll('pre')).toHaveLength(0);
    fireEvent.click(toggle);
    expect(stimulus.querySelectorAll('pre')).toHaveLength(40);
  });
});

describe('blocks of a stimulus', () => {
  it('draws 100 of 260 parts and a page more per click', () => {
    const { card } = renderNode(bigStimulusStep);
    const stimulus = stimulusOf(card);
    const parts = () => stimulus.querySelectorAll('p.font-mono').length;
    expect(parts()).toBe(100);
    expect(moreButtons(stimulus)).toEqual(['Show 100 more parts (160 not shown)']);
    clickMore(stimulus, 'Show 100 more parts (160 not shown)');
    expect(parts()).toBe(200);
    clickMore(stimulus, 'Show 60 more parts');
    expect(parts()).toBe(260);
    expect(moreButtons(stimulus)).toEqual([]);
  });
});

describe('tool-call cards of a step', () => {
  it('draws 10 of 63, says how many there are, and offers the rest', () => {
    const { card } = renderNode(bigListStep);
    expect(toolCardsOf(card)).toHaveLength(10);
    expect(within(card).getByText('Tool calls')).toHaveTextContent('Tool calls63');
    const toolPane = card.querySelector('.rr-code') as HTMLElement;
    expect(moreButtons(toolPane)).toEqual(['Show 10 more tool calls (53 not shown)']);
    // The cards drawn are the first ones, in order.
    expect(toolCardsOf(card)[0].textContent).toContain('echo call-1');
    expect(toolCardsOf(card)[9].textContent).toContain('echo call-10');

    for (const left of [53, 43, 33, 23, 13]) clickMore(toolPane, `Show 10 more tool calls (${left} not shown)`);
    expect(toolCardsOf(card)).toHaveLength(60);
    expect(moreButtons(toolPane)).toEqual(['Show 3 more tool calls']); // the last page says no more than that
    clickMore(toolPane, 'Show 3 more tool calls');
    expect(toolCardsOf(card)).toHaveLength(63);
    expect(toolCardsOf(card)[62].firstElementChild!.firstElementChild!.textContent).toBe('example_server · bulk_update');
    // Nothing more to ask for among the cards (the lists inside them still have their own pages).
    expect(moreButtons(toolPane).filter((text) => /tool calls?/.test(text ?? ''))).toEqual([]);
  });

  it('draws a node of exactly 10 calls without a control, and the eleventh behind one', () => {
    const step = (count: number) =>
      makeNode({
        id: HARNESS_ID,
        author: 'system',
        user_prompt: '[tool_result Bash] ok',
        tool_executions: Array.from({ length: count }, (_, i) => toolCall('Bash', { command: `echo ${i}` })),
      });
    const ten = renderNode(step(10));
    expect(toolCardsOf(ten.card)).toHaveLength(10);
    expect(moreButtons(ten.card)).toEqual([]);
    cleanup();
    const eleven = renderNode(step(11));
    expect(toolCardsOf(eleven.card)).toHaveLength(10);
    expect(moreButtons(eleven.card)).toEqual(['Show 1 more tool call']);
  });

  it('draws 256 calls, the most the reviewer accepts, ten at a time', () => {
    const tools = Array.from({ length: 256 }, (_, i) => toolCall('Bash', { command: `echo ${i}` }));
    const { card } = renderNode(makeNode({ id: HARNESS_ID, author: 'system', user_prompt: '[tool_result Bash] ok', tool_executions: tools }));
    const labels: string[] = [];
    for (let click = 0; click < 40; click++) {
      const [label] = moreButtons(card);
      if (!label) break;
      labels.push(label!);
      fireEvent.click(within(card).getByRole('button', { name: label! }));
    }
    expect(labels).toHaveLength(25);
    expect(labels.slice(0, 2)).toEqual(['Show 10 more tool calls (246 not shown)', 'Show 10 more tool calls (236 not shown)']);
    expect(labels.slice(-2)).toEqual(['Show 10 more tool calls (16 not shown)', 'Show 6 more tool calls']);
    expect(toolCardsOf(card)).toHaveLength(256);
  });
});

describe('a long checklist', () => {
  it('draws 100 of 130 items and offers the other 30', () => {
    const { card } = renderNode(bigListStep);
    showAllToolCalls(card);
    const todos = toolCard(card, 'TodoWrite');
    expect(todos.querySelectorAll('li')).toHaveLength(100);
    expect(moreButtons(todos)).toEqual(['Show 30 more items']);
    clickMore(todos, 'Show 30 more items');
    expect(todos.querySelectorAll('li')).toHaveLength(130);
    expect(within(todos).getByText('Item 130')).toBeInTheDocument();
  });

  it('keeps each item\'s status and wording', () => {
    const { card } = renderNode(bigListStep);
    showAllToolCalls(card);
    const items = toolCard(card, 'TodoWrite').querySelectorAll('li');
    expect(items[0].textContent).toContain('completed: Item 1');
    expect(items[40].textContent).toContain('in progress: Item 41');
    expect(items[41].textContent).toContain('pending: Item 42');
    expect(items[0].textContent).toContain('Doing item 1');
  });
});

describe('the rows of a tool call', () => {
  it('draws 25 of 230 fields and offers the other 205, a page at a time', () => {
    const { card } = renderNode(bigListStep);
    showAllToolCalls(card);
    const mcp = toolCard(card, 'example_server · bulk_update');
    expect(mcp.querySelectorAll('dt')).toHaveLength(25);
    expect(moreButtons(mcp)).toEqual(['Show 25 more fields (205 not shown)']);
    clickMore(mcp, 'Show 25 more fields (205 not shown)');
    expect(mcp.querySelectorAll('dt')).toHaveLength(50);

    const labels: string[] = [];
    for (let click = 0; click < 20; click++) {
      const [label] = moreButtons(mcp);
      if (!label) break;
      labels.push(label!);
      clickMore(mcp, label!);
    }
    expect(labels).toHaveLength(8); // 50 + 7 pages of 25 + one of 5 = 230
    expect(labels.at(-1)).toBe('Show 5 more fields');
    expect(mcp.querySelectorAll('dt')).toHaveLength(230);
    expect(within(mcp).getByText('field_230')).toBeInTheDocument();
  });

  it('pages the extras of a tool that is understood on their own', () => {
    const extras = Object.fromEntries(Array.from({ length: 150 }, (_, i) => [`extra_${String(i).padStart(3, '0')}`, `v${i}`]));
    const { card } = renderNode(
      makeNode({ id: HARNESS_ID, author: 'system', user_prompt: '[tool_result Bash] ok', tool_executions: [toolCall('Bash', { command: 'ls', ...extras })] }),
    );
    const bash = toolCardsOf(card)[0];
    expect(bash.querySelector('pre')!.textContent).toBe('ls');
    expect(bash.querySelectorAll('dt')).toHaveLength(25);
    expect(moreButtons(bash)).toEqual(['Show 25 more fields (125 not shown)']);
  });
});

describe('the edits of a MultiEdit', () => {
  it('draws 5 of 45 diffs and offers the rest 5 at a time', () => {
    const { card } = renderNode(bigListStep);
    showAllToolCalls(card);
    const multi = toolCard(card, 'MultiEdit');
    expect(within(multi).getByText('Edit 5 of 45')).toBeInTheDocument();
    expect(within(multi).queryByText('Edit 6 of 45')).not.toBeInTheDocument();
    expect(multi.querySelectorAll('del')).toHaveLength(5);
    expect(moreButtons(multi)).toEqual(['Show 5 more edits (40 not shown)']);

    clickMore(multi, 'Show 5 more edits (40 not shown)');
    expect(multi.querySelectorAll('del')).toHaveLength(10);
    for (let click = 0; click < 7; click++) clickMore(multi, /^Show 5 more edits/);
    expect(multi.querySelectorAll('del')).toHaveLength(45);
    expect(within(multi).getByText('Edit 45 of 45')).toBeInTheDocument();
    expect(moreButtons(multi)).toEqual([]);
  });

  it('draws a thousand edits that each give up the minimal diff in a moment, 5 diffs at a time', () => {
    // About 4 MiB of edits: each side 250 lines with nothing in common, so every diff hits the search cap.
    const side = (prefix: string) => Array.from({ length: 250 }, (_, i) => `${prefix} line ${i}`).join('\n');
    const edits = Array.from({ length: 1000 }, (_, i) => ({ new_string: side(`new${i}`), old_string: side(`old${i}`) }));
    const node = makeNode({
      id: HARNESS_ID,
      author: 'system',
      user_prompt: '[tool_result Bash] ok',
      tool_executions: [toolCall('MultiEdit', { edits, file_path: '/repo/a.ts' })],
    });
    expect(JSON.stringify(node).length).toBeGreaterThan(3 * 1024 * 1024);
    const started = performance.now();
    const { card } = renderNode(node);
    const elapsed = performance.now() - started;
    expect(elapsed).toBeLessThan(5000); // about 100 ms; this took 16 s when every edit was diffed up front
    expect(card.querySelectorAll('del')).toHaveLength(5 * 12);
    expect(moreButtons(card)).toEqual(['Show 5 more edits (995 not shown)']);
    expect(card.querySelectorAll('*').length).toBeLessThan(800);
  });
});

describe('an edit that shows no change', () => {
  const edit = (oldText: string, newText: string) =>
    makeNode({
      id: HARNESS_ID,
      author: 'system',
      user_prompt: '[tool_result Bash] ok',
      tool_executions: [toolCall('Edit', { file_path: '/repo/big.txt', old_string: oldText, new_string: newText })],
    });
  const lines = (count: number, ending = '\n') => Array.from({ length: count }, () => 'x').join(ending);

  it('draws a long unchanged text as one block of text, so "Show all" builds no rows', () => {
    const text = lines(20_000);
    const { card } = renderNode(edit(text, text));
    const editCard = toolCardsOf(card)[0];
    expect(within(editCard).getByText('Old and new text are identical.')).toBeInTheDocument();
    expect(editCard.querySelectorAll('ins, del')).toHaveLength(0);
    const toggle = within(editCard).getByRole('button', { name: 'Show all 20,000 lines' });
    const pre = editCard.querySelector('pre') as HTMLElement;
    expect(pre.textContent).toBe(lines(12));

    const before = card.querySelectorAll('*').length;
    fireEvent.click(toggle);
    expect(card.querySelectorAll('*').length).toBe(before); // the same elements: only the text of the block grew
    expect(pre.textContent).toBe(text);
    expect(editCard.querySelectorAll('div').length).toBeLessThan(50);
  });

  it('does the same when only the line endings differ, whichever way round', () => {
    for (const [oldText, newText] of [
      [lines(20_000, '\r\n'), lines(20_000)],
      [lines(20_000), lines(20_000, '\r\n')],
      [lines(20_000, '\r'), lines(20_000)],
    ]) {
      const { card, unmount } = renderNode(edit(oldText, newText));
      const editCard = toolCardsOf(card)[0];
      expect(within(editCard).getByText('Only the line endings differ.')).toBeInTheDocument();
      const before = card.querySelectorAll('*').length;
      fireEvent.click(within(editCard).getByRole('button', { name: 'Show all 20,000 lines' }));
      expect(card.querySelectorAll('*').length).toBe(before);
      expect(editCard.querySelector('pre')!.textContent).toBe(lines(20_000)); // as the new text reads, line endings folded for display
      unmount();
    }
  });

  it('is as cheap for half a million lines as for a dozen', () => {
    // The largest string the reviewer accepts is 1 MiB: half a million one-character lines.
    const text = lines(500_000);
    expect(text.length).toBeLessThan(1024 * 1024);
    const started = performance.now();
    const { card } = renderNode(edit(text, text));
    const editCard = toolCardsOf(card)[0];
    fireEvent.click(within(editCard).getByRole('button', { name: 'Show all 500,000 lines' }));
    expect(editCard.querySelector('pre')!.textContent).toHaveLength(text.length);
    expect(card.querySelectorAll('*').length).toBeLessThan(200);
    expect(performance.now() - started).toBeLessThan(5000);
  });

  it('shows only the note when both sides are empty, and an unchanged blank text as a block', () => {
    const empty = renderNode(edit('', ''));
    expect(within(toolCardsOf(empty.card)[0]).getByText('Old and new text are both empty.')).toBeInTheDocument();
    expect(toolCardsOf(empty.card)[0].querySelector('pre')).toBeNull();
    empty.unmount();
    const blank = renderNode(edit('\n', '\n'));
    expect(within(toolCardsOf(blank.card)[0]).getByText('Old and new text are identical.')).toBeInTheDocument();
    expect(toolCardsOf(blank.card)[0].querySelectorAll('pre')).toHaveLength(1);
  });

  it('still diffs a text that changes, with the line endings of the two sides folded', () => {
    const { card } = renderNode(edit('a\r\nb\r\nc', 'a\nB\nc'));
    const editCard = toolCardsOf(card)[0];
    expect(editCard.querySelectorAll('del')).toHaveLength(1);
    expect(editCard.querySelectorAll('ins')).toHaveLength(1);
    expect(within(editCard).queryByText(/Only the line endings differ|identical/)).not.toBeInTheDocument();
    expect(within(editCard).getByRole('region', { name: 'Diff' })).toBeInTheDocument();
  });

  it('applies to each edit of a MultiEdit on its own', () => {
    const node = makeNode({
      id: HARNESS_ID,
      author: 'system',
      user_prompt: '[tool_result Bash] ok',
      tool_executions: [
        toolCall('MultiEdit', {
          edits: [
            { new_string: 'same\ntext', old_string: 'same\ntext' },
            { new_string: 'b', old_string: 'a' },
            { new_string: 'x\ny', old_string: 'x\r\ny' },
          ],
          file_path: '/repo/a.ts',
        }),
      ],
    });
    const editCard = toolCardsOf(renderNode(node).card)[0];
    expect(within(editCard).getByText('Old and new text are identical.')).toBeInTheDocument();
    expect(within(editCard).getByText('Only the line endings differ.')).toBeInTheDocument();
    expect(editCard.querySelectorAll('del')).toHaveLength(1);
    expect(editCard.querySelectorAll('ins')).toHaveLength(1);
  });
});

describe('the caps multiply: a crafted step draws a small first screen', () => {
  /**
   * The lists nest, so what a step draws first is the product of the pages along one
   * path: cards x edits x diff rows, or cards x field rows. A step built to hit every
   * cap at once used to draw about 58,000 elements from 230 KB.
   */
  function craftedStep(): InteractionNode {
    const changed = (prefix: string) => Array.from({ length: 14 }, (_, line) => `${prefix} line ${line} of the change`).join('\n');
    const multiEdit = (n: number) =>
      toolCall('MultiEdit', {
        edits: Array.from({ length: 20 }, (_, i) => ({ new_string: changed(`new ${n}.${i}`), old_string: changed(`old ${n}.${i}`) })),
        file_path: `/repo/file-${n}.ts`,
      });
    const manyFields = (n: number) =>
      toolCall('mcp__srv__tool', Object.fromEntries(Array.from({ length: 100 }, (_, i) => [`field_${i}`, `value ${n}.${i}\nwith a second line`])));
    const tools = Array.from({ length: 60 }, (_, n) => (n % 2 === 0 ? multiEdit(n) : manyFields(n)));
    return makeNode({ id: HARNESS_ID, author: 'system', user_prompt: '[tool_result Bash] ok', tool_executions: tools });
  }

  it('draws a few thousand elements, not tens of thousands', () => {
    const node = craftedStep();
    expect(JSON.stringify(node).length).toBeGreaterThan(200 * 1024);
    const { card } = renderNode(node);
    const drawn = card.querySelectorAll('*').length;
    expect(drawn, `${drawn} elements on the first screen`).toBeLessThan(2500);
    expect(toolCardsOf(card)).toHaveLength(10);
    // Each list inside a card shows its first page only.
    for (const cardEl of toolCardsOf(card)) {
      expect(cardEl.querySelectorAll('dt').length).toBeLessThanOrEqual(25);
      expect(cardEl.querySelectorAll('del').length).toBeLessThanOrEqual(5 * 14);
    }
  });

  it('adds one page of one list per click, not the product of the pages', () => {
    const { card } = renderNode(craftedStep());
    const before = card.querySelectorAll('*').length;
    const toolPane = card.querySelector('.rr-code') as HTMLElement;
    clickMore(toolPane, /^Show 10 more tool calls/);
    const after = card.querySelectorAll('*').length;
    expect(toolCardsOf(card)).toHaveLength(20);
    // Ten more cards, each of them one first page.
    expect(after - before).toBeLessThan(2500);
  });
});

describe('the controls that show more', () => {
  it('are real buttons that never select the card, and are not toggles', () => {
    const onSelect = vi.fn();
    const { card } = renderNode(bigListStep, onSelect);
    const buttons = within(card)
      .getAllByRole('button')
      .filter((button) => /^Show [\d,]+ more /.test(button.textContent ?? ''));
    expect(buttons.length).toBeGreaterThanOrEqual(2);
    for (const button of buttons) {
      expect(button.tagName).toBe('BUTTON');
      expect(button).toHaveAttribute('type', 'button');
      expect(button).not.toHaveAttribute('aria-expanded'); // they reveal more; there is nothing to fold back
      expect(button).toHaveAccessibleName();
      fireEvent.click(button);
      fireEvent.keyDown(button, { key: 'Enter' });
      fireEvent.keyDown(button, { key: ' ' });
    }
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('are not offered when everything is already drawn', () => {
    const node = makeNode({ id: HARNESS_ID, author: 'human', user_prompt: '[tool_result T] a\n\n[tool_result T] b', model_response: 'x' });
    expect(moreButtons(renderNode(node).card)).toEqual([]);
  });

  it('format long counts with thousands separators', () => {
    const node = makeNode({ id: HARNESS_ID, author: 'human', user_prompt: Array.from({ length: 12_345 }, () => '[tool_result T] x').join('\n\n') });
    expect(moreButtons(renderNode(node).card)).toEqual(['Show 20 more tool results (12,325 not shown)']);
  });
});

describe('what the caps leave alone', () => {
  it('keeps every list whole in the view models', () => {
    expect(bigListStep.tool_executions).toHaveLength(63);
    const todos = describeToolCall(bigListStep.tool_executions.find((t) => t.tool_name === 'TodoWrite')!);
    expect(todos.kind === 'todos' && todos.todos).toHaveLength(130);
    const edits = describeToolCall(bigListStep.tool_executions.find((t) => t.tool_name === 'MultiEdit')!);
    expect(edits.kind === 'edit' && edits.edits).toHaveLength(45);
    const fields = describeToolCall(bigListStep.tool_executions.find((t) => t.tool_name.startsWith('mcp__'))!);
    expect(fields.kind === 'fields' && fields.fields).toHaveLength(230);
  });

  it('does not touch the node it is given', () => {
    const frozen = structuredClone(bigListStep);
    const freeze = (value: unknown) => {
      if (value !== null && typeof value === 'object' && !Object.isFrozen(value)) {
        Object.freeze(value);
        for (const item of Object.values(value)) freeze(item);
      }
    };
    freeze(frozen);
    const { card } = renderNode(frozen);
    for (let pass = 0; pass < 40 && moreButtons(card).length > 0; pass++) {
      for (const button of moreButtons(card)) fireEvent.click(within(card).getByRole('button', { name: button! }));
    }
    expect(moreButtons(card)).toEqual([]);
    expect(toolCardsOf(card)).toHaveLength(63);
  });
});

describe('values that are cut or cannot be shown', () => {
  afterEach(() => vi.restoreAllMocks());
  const rowsStep = (...tools: ReturnType<typeof toolCall>[]) =>
    makeNode({ id: HARNESS_ID, author: 'system', user_prompt: '[tool_result Bash] ok', tool_executions: tools });

  it('says how much of an over-long value was left out, in a note outside the text', () => {
    const { card } = renderNode(rowsStep(toolCall('mcp__srv__tool', { body: 'x'.repeat(300 * 1024) })));
    const note = within(card).getByText('… 45,056 more characters not shown');
    expect(note.tagName).toBe('P');
    expect(note.closest('pre')).toBeNull(); // not part of the recorded text
    expect(card.querySelector('pre')!.textContent).not.toContain('not shown');
    expect(card.querySelector('pre')!.textContent).not.toContain('…');
    // Not in brackets: bracketed text in this layout means "recorded, but not kept".
    expect(note.textContent).not.toMatch(/[[\]]/);
  });

  it('says so for a value too deeply nested to print, in place of its text', () => {
    const original = JSON.stringify;
    vi.spyOn(JSON, 'stringify').mockImplementation(((value: unknown, ...rest: [never?, never?]) => {
      if (Array.isArray(value) && Array.isArray(value[0])) throw new RangeError('Maximum call stack size exceeded');
      return original(value, ...rest);
    }) as typeof JSON.stringify);
    const node = rowsStep(rawToolCall('mcp__srv__tool', '{"deep":[[[[1]]]],"after":"still here"}'));
    const { card } = renderNode(node);
    const row = within(card).getByText('deep').parentElement!;
    expect(within(row).getByText('(too deeply nested to display)')).toBeInTheDocument();
    expect(row.querySelector('pre')).toBeNull();
    expect(within(card).getByText('still here')).toBeInTheDocument();
    expect(card.textContent).not.toContain('[unrenderable');
  });

  it('says how much of over-long arguments that are not JSON was left out', () => {
    const { card } = renderNode(rowsStep(rawToolCall('Bash', `not json ${'q'.repeat(300 * 1024)}`)));
    const total = `not json ${'q'.repeat(300 * 1024)}`.length;
    expect(within(card).getByText(`… ${(total - 256 * 1024).toLocaleString('en-US')} more characters not shown`)).toBeInTheDocument();
    expect(card.querySelector('pre')!.textContent!.length).toBe(256 * 1024);
  });

  it('shows legacy arguments that are not text as compact JSON, not as "No arguments recorded"', () => {
    const legacy = { tool_protocol: 'native', tool_name: 'Bash', arguments: { command: 'ls' }, status: 'success' } as unknown as ReturnType<typeof toolCall>;
    const { card } = renderNode(rowsStep(legacy));
    expect(within(card).queryByText('No arguments recorded.')).not.toBeInTheDocument();
    expect(card.querySelector('pre')!.textContent).toBe('{"command":"ls"}');
    expect(within(card).getByRole('region', { name: 'Arguments' })).toBe(card.querySelector('pre'));
  });
});

describe('long lines in a block that wraps', () => {
  const BUDGET = WRAPPED_CHAR_BUDGET;
  const characters = (count: number) => `Show all ${count.toLocaleString('en-US')} characters`;
  const stray = /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/;

  /** A step whose only stimulus is one tool result of this text, its group opened. */
  function resultCard(text: string) {
    const { card } = renderNode(makeNode({ id: HARNESS_ID, author: 'system', user_prompt: `[tool_result Bash] ${text}` }));
    fireEvent.click(within(card).getByRole('button', { name: /^Tool results/ }));
    return { card, pre: stimulusOf(card).querySelector('pre') as HTMLElement };
  }

  it('cuts one enormous line of tool output to the budget behind "Show all N characters"', () => {
    const line = `HEAD ${'x'.repeat(100_000)} TAIL`;
    const { card, pre } = resultCard(line);
    expect(pre.textContent).toBe(line.slice(0, BUDGET));
    const toggle = within(card).getByRole('button', { name: characters(line.length) });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(toggle).toHaveAttribute('aria-controls', pre.id);

    fireEvent.click(toggle);
    expect(pre.textContent).toBe(line);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expect(toggle).toHaveTextContent('Show less');

    fireEvent.click(toggle);
    expect(pre.textContent).toBe(line.slice(0, BUDGET));
  });

  it.each([
    [1500, false],
    [1999, false],
    [2000, true],
  ])('a line of %i characters is cut: %s (hiding fewer than 500 is not worth a click)', (length, cut) => {
    const { card, pre } = resultCard('q'.repeat(length));
    expect(pre.textContent).toBe(cut ? 'q'.repeat(BUDGET) : 'q'.repeat(length));
    expect(within(card).queryAllByRole('button', { name: /^Show all / })).toHaveLength(cut ? 1 : 0);
  });

  it('never cuts a character in half', () => {
    const emoji = String.fromCodePoint(0x1f600);
    const { pre } = resultCard(`${'a'.repeat(BUDGET - 1)}${emoji}${'b'.repeat(3000)}`);
    expect(pre.textContent).toBe('a'.repeat(BUDGET - 1));
    expect(stray.test(pre.textContent!)).toBe(false);
  });

  it('counts a few long lines in characters, and cuts the first of them mid-line', () => {
    const lines = ['a', 'b', 'c'].map((letter) => letter.repeat(1000));
    const text = lines.join('\n');
    const { card, pre } = resultCard(text);
    expect(pre.textContent).toBe(text.slice(0, BUDGET));
    expect(within(card).getByRole('button', { name: characters(text.length) })).toBeInTheDocument();
  });

  it('keeps counting in lines when many lines are hidden, and shows ten of them whole when they fit the budget', () => {
    const lines = Array.from({ length: 40 }, (_, index) => `line ${index} ${'y'.repeat(90)}`);
    const { card, pre } = resultCard(lines.join('\n'));
    expect(pre.textContent).toBe(lines.slice(0, 10).join('\n'));
    expect(within(card).getByRole('button', { name: 'Show all 40 lines' })).toBeInTheDocument();
    expect(within(card).queryByRole('button', { name: /characters$/ })).not.toBeInTheDocument();
  });

  it('cuts the ten lines it does show to the budget when they do not fit, still counting in lines', () => {
    const lines = Array.from({ length: 40 }, (_, index) => `${index} ${'z'.repeat(1000)}`);
    const { card, pre } = resultCard(lines.join('\n'));
    expect(pre.textContent).toBe(lines.slice(0, 10).join('\n').slice(0, BUDGET));
    expect(within(card).getByRole('button', { name: 'Show all 40 lines' })).toBeInTheDocument();
    expect(within(card).queryByRole('button', { name: /characters$/ })).not.toBeInTheDocument();
    fireEvent.click(within(card).getByRole('button', { name: 'Show all 40 lines' }));
    expect(pre.textContent).toBe(lines.join('\n'));
  });

  it('cuts an over-long field value too, and still says how much was clipped before it got here', () => {
    const node = makeNode({
      id: HARNESS_ID,
      author: 'system',
      user_prompt: '[tool_result Bash] ok',
      tool_executions: [toolCall('mcp__srv__tool', { body: 'x'.repeat(300 * 1024) })],
    });
    const { card } = renderNode(node);
    const card0 = toolCard(card, 'srv · tool');
    const pre = card0.querySelector('pre') as HTMLElement;
    expect(pre.textContent).toBe('x'.repeat(BUDGET));
    expect(within(card0).getByText('… 45,056 more characters not shown')).toBeInTheDocument();
    fireEvent.click(within(card0).getByRole('button', { name: characters(256 * 1024) }));
    expect(pre.textContent).toHaveLength(256 * 1024);
  });

  it('cuts a sub-agent prompt, which is also text a machine wrote', () => {
    const prompt = `Please ${'w'.repeat(50_000)} done`;
    const node = makeNode({
      id: HARNESS_ID,
      author: 'system',
      user_prompt: '[tool_result Bash] ok',
      tool_executions: [toolCall('Agent', { description: 'Look around', prompt, subagent_type: 'Explore' })],
    });
    const { card } = renderNode(node);
    const agent = toolCard(card, 'Agent');
    fireEvent.click(within(agent).getByRole('button', { name: 'Show subagent prompt' }));
    expect(agent.querySelector('pre')!.textContent).toBe(prompt.slice(0, BUDGET));
    expect(within(agent).getByRole('button', { name: characters(prompt.length) })).toBeInTheDocument();
  });

  it('leaves a block that scrolls sideways alone: one line is one row however long it is', () => {
    const command = `echo ${'a'.repeat(100_000)}`;
    const node = makeNode({
      id: HARNESS_ID,
      author: 'system',
      user_prompt: '[tool_result Bash] ok',
      tool_executions: [toolCall('Bash', { command })],
    });
    const { card } = renderNode(node);
    const bash = toolCard(card, 'Bash');
    const pre = within(bash).getByRole('region', { name: 'Command' });
    expect(pre.textContent).toBe(command);
    expect(within(bash).queryByRole('button', { name: /^Show all / })).not.toBeInTheDocument();
  });

  it('does not select the node when its toggle is clicked, like every other control inside the card', () => {
    const onSelect = vi.fn();
    const node = makeNode({ id: HARNESS_ID, author: 'system', user_prompt: `[tool_result Bash] ${'k'.repeat(5000)}` });
    const { card } = renderNode(node, onSelect);
    fireEvent.click(within(card).getByRole('button', { name: /^Tool results/ }));
    fireEvent.click(within(card).getByRole('button', { name: characters(5000) }));
    expect(stimulusOf(card).querySelector('pre')!.textContent).toHaveLength(5000);
    expect(onSelect).not.toHaveBeenCalled();
  });
});
