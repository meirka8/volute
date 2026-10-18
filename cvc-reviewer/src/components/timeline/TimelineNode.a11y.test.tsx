/**
 * Keyboard and assistive-technology behaviour of the transcript layout: every
 * control is a real button that says what it does and whether it is open,
 * keyboard activation of a control never selects the card, and the card itself
 * still selects on a click, Enter or Space.
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TimelineNode } from './TimelineNode';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { generatedLink, makeNode, toolCall } from '../../test/fixtures';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

const HARNESS_ID = '1f0e5c3a-9d2b-8c4e-a1b7-0000000000ab';
const lines = (count: number, label: string) => Array.from({ length: count }, (_, i) => `${label} ${i + 1}`).join('\n');

/** A step with every kind of control: prose to expand, code to expand, a diff, a sub-agent prompt, tool results. */
const kitchenSink: InteractionNode = makeNode({
  id: HARNESS_ID,
  author: 'system',
  model_name: 'claude-test-1',
  user_prompt: `[meta] ${lines(8, 'context')}\n\n[tool_result Bash] ${lines(30, 'output')}\n\n[tool_result Read error] short`,
  model_cot: lines(9, 'reasoning'),
  model_response: lines(9, 'response'),
  tool_executions: [
    toolCall('Bash', { command: lines(30, 'echo'), description: 'A long command' }),
    toolCall('Edit', { file_path: '/repo/a.ts', old_string: lines(40, 'old'), new_string: lines(40, 'new') }),
    toolCall('Agent', { description: 'Look around', prompt: lines(20, 'step'), subagent_type: 'Explore' }),
    toolCall('WebFetch', { url: 'https://example.com', prompt: lines(8, 'question') }),
    toolCall('mcp__srv__tool', { body: lines(30, 'body'), small: 'x' }, 'failure'),
    toolCall('server:web_search', { query: 'q' }, 'failure'),
    toolCall('Read', { file_path: '/repo/a.ts' }),
    toolCall('Write', { file_path: '/repo/b.md', content: lines(30, 'doc') }),
  ],
  artifact_links: [generatedLink(HARNESS_ID)],
});

function renderSink(onSelect: () => void = () => {}) {
  const view = render(<TimelineNode interaction={kitchenSink} onSelect={onSelect} />);
  return { ...view, card: view.container.firstElementChild as HTMLElement };
}

const buttonsIn = (card: HTMLElement): HTMLButtonElement[] => Array.from(card.querySelectorAll('button'));

/** Opens every toggle, newly revealed ones included. */
function expandAll(card: HTMLElement) {
  for (let pass = 0; pass < 12; pass++) {
    const closed = Array.from(card.querySelectorAll('button[aria-expanded="false"]'));
    if (closed.length === 0) return;
    for (const button of closed) fireEvent.click(button);
  }
}

describe('the controls of a transcript card', () => {
  it('has the kinds of control this suite is about', () => {
    const { card } = renderSink();
    expandAll(card);
    const names = buttonsIn(card).map((button) => button.textContent!);
    expect(names.some((name) => name.startsWith('Tool results'))).toBe(true);
    expect(names.filter((name) => name === 'Show less').length).toBeGreaterThan(8);
    expect(names).toContain('Hide subagent prompt');
    // Opened: no control is still waiting to be opened.
    expect(card.querySelectorAll('button[aria-expanded="false"]')).toHaveLength(0);
  });

  it('are all real buttons that never submit, report their state and have a name', () => {
    const { card } = renderSink();
    for (const state of ['collapsed', 'expanded']) {
      if (state === 'expanded') expandAll(card);
      const buttons = buttonsIn(card);
      expect(buttons.length, state).toBeGreaterThanOrEqual(9);
      for (const button of buttons) {
        expect(button.tagName).toBe('BUTTON');
        expect(button, state).toHaveAttribute('type', 'button');
        expect(['true', 'false'], state).toContain(button.getAttribute('aria-expanded'));
        expect(button, `${state}: ${button.textContent}`).toHaveAccessibleName();
        expect(button.textContent!.trim(), state).not.toBe('');
      }
    }
  });

  it('has no other element posing as a control inside the card', () => {
    const { card } = renderSink();
    expandAll(card);
    expect(card.querySelectorAll('[role="button"], [role="link"], [role="switch"], [role="checkbox"], a[href]')).toHaveLength(0);
    // Everything focusable is a button or a scrollable block of text.
    const focusable = Array.from(card.querySelectorAll('[tabindex]')).map((el) => el.tagName);
    for (const tag of focusable) expect(['PRE', 'DIV'], 'only scrolling blocks are focusable besides buttons').toContain(tag);
    for (const el of card.querySelectorAll('[tabindex]')) expect(el.getAttribute('tabindex')).toBe('0');
  });

  it('leave every block that scrolls sideways reachable from the keyboard, and no other block', () => {
    const { card } = renderSink();
    expandAll(card);
    const scrollers = Array.from(card.querySelectorAll('.overflow-x-auto'));
    // The command, the written file and the diff scroll; wrapped output does not.
    expect(scrollers).toHaveLength(3);
    for (const el of scrollers) expect(el, el.className).toHaveAttribute('tabindex', '0');
    const wrapped = Array.from(card.querySelectorAll('pre.whitespace-pre-wrap'));
    expect(wrapped.length).toBeGreaterThan(0);
    for (const el of wrapped) expect(el).not.toHaveAttribute('tabindex');
    expect(card.querySelectorAll('[tabindex]').length).toBe(scrollers.length);
  });

  it('name what they do: how much more, which lines, or which prompt', () => {
    const { card } = renderSink();
    const labels = buttonsIn(card).map((button) => button.textContent!);
    expect(labels.some((label) => label.startsWith('Tool results · Bash, Read · 1 error'))).toBe(true);
    expect(labels.filter((label) => label === 'Show more').length).toBeGreaterThanOrEqual(4);
    expect(labels.filter((label) => /^Show all [\d,]+ lines$/.test(label)).length).toBeGreaterThanOrEqual(4);
    expect(labels).toContain('Show subagent prompt');
  });

  it('point only at elements that exist, and at none that are absent', () => {
    const { card } = renderSink();
    for (const state of ['collapsed', 'expanded']) {
      if (state === 'expanded') expandAll(card);
      const targets = buttonsIn(card)
        .map((button) => button.getAttribute('aria-controls'))
        .filter((id): id is string => id !== null);
      expect(targets.length, state).toBeGreaterThan(0);
      expect(new Set(targets).size, `${state}: ids are unique`).toBe(targets.length);
      for (const id of targets) {
        const target = document.getElementById(id);
        expect(target, `${state}: #${id}`).not.toBeNull();
        expect(card.contains(target), `${state}: #${id} is inside the card`).toBe(true);
      }
    }
  });

  it('flip aria-expanded and their label when used, and flip back', () => {
    const { card } = renderSink();
    const opened: Array<[string, string]> = [];
    for (const button of buttonsIn(card)) {
      const before = button.textContent!;
      expect(button).toHaveAttribute('aria-expanded', 'false');
      fireEvent.click(button);
      expect(button).toHaveAttribute('aria-expanded', 'true');
      const after = button.textContent!;
      // The tool-results summary keeps its text; every other control swaps to its "less" form.
      if (!before.startsWith('Tool results')) expect(['Show less', 'Hide subagent prompt']).toContain(after);
      opened.push([before, after]);
      fireEvent.click(button);
      expect(button).toHaveAttribute('aria-expanded', 'false');
      expect(button.textContent).toBe(before);
    }
    expect(opened.length).toBeGreaterThanOrEqual(8);
  });

  it('keep an accurate state when several are used in a row', () => {
    const { card } = renderSink();
    const buttons = buttonsIn(card);
    for (const button of buttons) fireEvent.click(button);
    for (const button of buttons) expect(button).toHaveAttribute('aria-expanded', 'true');
    for (const button of buttons.reverse()) fireEvent.click(button);
    for (const button of buttons) expect(button).toHaveAttribute('aria-expanded', 'false');
  });
});

describe('keyboard use of a control', () => {
  it('never selects the card for Enter or Space, and does not cancel the key (so the browser can activate the button)', () => {
    const onSelect = vi.fn();
    const { card } = renderSink(onSelect);
    expandAll(card);
    const buttons = buttonsIn(card);
    expect(buttons.length).toBeGreaterThan(8);
    for (const button of buttons) {
      for (const key of ['Enter', ' ']) {
        // fireEvent returns false when the event was cancelled; a cancelled keydown
        // would stop a real browser from turning Enter or Space into a click.
        expect(fireEvent.keyDown(button, { key }), `keydown ${JSON.stringify(key)}`).toBe(true);
        expect(fireEvent.keyUp(button, { key }), `keyup ${JSON.stringify(key)}`).toBe(true);
      }
    }
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('completes as a browser would: the click that follows toggles the control and still does not select', () => {
    const onSelect = vi.fn();
    const { card } = renderSink(onSelect);
    for (const button of buttonsIn(card)) {
      const wasOpen = button.getAttribute('aria-expanded') === 'true';
      fireEvent.keyDown(button, { key: 'Enter' });
      fireEvent.click(button); // what the browser does for Enter on a button
      expect(button).toHaveAttribute('aria-expanded', String(!wasOpen));
      fireEvent.keyDown(button, { key: ' ' });
      fireEvent.keyUp(button, { key: ' ' });
      fireEvent.click(button); // what the browser does for Space on a button, on key up
      expect(button).toHaveAttribute('aria-expanded', String(wasOpen));
    }
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('is not turned into a selection by keys pressed inside code, diffs or other focusable blocks either', () => {
    const onSelect = vi.fn();
    const { card } = renderSink(onSelect);
    expandAll(card);
    const blocks = Array.from(card.querySelectorAll<HTMLElement>('pre, [tabindex="0"]'));
    expect(blocks.length).toBeGreaterThan(5);
    for (const block of blocks) {
      for (const key of ['Enter', ' ', 'ArrowDown', 'PageDown']) {
        // Not cancelled either: Space and the arrow keys must still scroll the block.
        expect(fireEvent.keyDown(block, { key })).toBe(true);
      }
    }
    expect(onSelect).not.toHaveBeenCalled();
  });
});

describe('selecting the card', () => {
  it('happens on Enter or Space with the card focused, and the page does not scroll for Space', () => {
    const onSelect = vi.fn();
    const { card } = renderSink(onSelect);
    expect(card).toHaveAttribute('role', 'button');
    expect(card).toHaveAttribute('tabindex', '0');

    expect(fireEvent.keyDown(card, { key: 'Enter' })).toBe(false); // handled, so cancelled
    expect(fireEvent.keyDown(card, { key: ' ' })).toBe(false);
    expect(onSelect).toHaveBeenCalledTimes(2);

    // Anything else is left alone.
    for (const key of ['a', 'Tab', 'Escape', 'ArrowDown', 'Shift']) {
      expect(fireEvent.keyDown(card, { key })).toBe(true);
    }
    expect(onSelect).toHaveBeenCalledTimes(2);
  });

  it('happens on a click anywhere in the body of the card', () => {
    const onSelect = vi.fn();
    const { card } = renderSink(onSelect);
    expandAll(card);
    const targets: Array<[string, Element | null]> = [
      ['header title', within(card).getByText('AI step')],
      ['header model', within(card).getByText('claude-test-1')],
      ['stimulus note', within(card).getByText(/context 1/)],
      ['tool result code', card.querySelector('.rr-thought pre')],
      ['reasoning', within(card).getByText(/reasoning 1/)],
      ['response', within(card).getByText(/response 1/)],
      ['tool call heading', card.querySelector('[data-status] span.font-mono')],
      ['tool call code', card.querySelector('[data-status] pre')],
      ['diff line', card.querySelector('del')],
      ['outcome badge', within(card).getAllByText('failed')[0]],
      ['tool section label', within(card).getByText('Tool calls', { exact: false })],
      ['commit chip', within(card).getByText('a1b2c3d')],
      ['the card itself', card],
    ];
    let expected = 0;
    for (const [label, element] of targets) {
      expect(element, label).not.toBeNull();
      fireEvent.click(element!);
      expected++;
      expect(onSelect, label).toHaveBeenCalledTimes(expected);
    }
  });

  it('is not triggered by a click on a control, in either state', () => {
    const onSelect = vi.fn();
    const { card } = renderSink(onSelect);
    for (let round = 0; round < 2; round++) {
      for (const button of buttonsIn(card)) fireEvent.click(button);
    }
    expandAll(card);
    for (const button of buttonsIn(card)) fireEvent.click(button);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('reflects whether the node is selected in aria-pressed', () => {
    const { rerender, card } = renderSink();
    expect(card).toHaveAttribute('aria-pressed', 'false');
    rerender(<TimelineNode interaction={kitchenSink} isSelected />);
    expect(card).toHaveAttribute('aria-pressed', 'true');
  });
});

describe('what a screen reader is told', () => {
  it('hides decorative icons', () => {
    const { card } = renderSink();
    expandAll(card);
    const icons = Array.from(card.querySelectorAll('svg'));
    expect(icons.length).toBeGreaterThanOrEqual(3); // the bot, the chevrons, the error mark
    for (const icon of icons) expect(icon).toHaveAttribute('aria-hidden', 'true');
  });

  it('gets every tool outcome as words, never as colour alone', () => {
    const { card } = renderSink();
    const outcomes = Array.from(card.querySelectorAll('[data-status]')).map((el) => [
      el.getAttribute('data-status'),
      el.firstElementChild!.textContent,
    ]);
    expect(outcomes.map(([status]) => status)).toEqual([
      'success',
      'success',
      'success',
      'success',
      'failure',
      'unrecorded',
      'success',
      'success',
    ]);
    for (const [status, heading] of outcomes) {
      const word = { success: 'success', failure: 'failed', unrecorded: 'server-side · result not recorded' }[status!];
      expect(heading, String(status)).toContain(word);
    }
  });

  it('states a failure in the header in words too', () => {
    const { card } = renderSink();
    expect(within(card).getByText('1 tool call failed')).toBeInTheDocument();
  });

  it('marks added and removed lines with elements and with a sign, not with colour alone', () => {
    const { card } = renderSink();
    expandAll(card);
    const removed = card.querySelectorAll('del');
    const added = card.querySelectorAll('ins');
    expect(removed.length).toBe(40);
    expect(added.length).toBe(40);
    for (const row of removed) expect(row.textContent!.startsWith('-')).toBe(true);
    for (const row of added) expect(row.textContent!.startsWith('+')).toBe(true);
    // The sign is decoration for sighted users; the element carries the meaning.
    expect(removed[0].querySelector('[aria-hidden="true"]')!.textContent).toBe('-');
    expect(added[0].querySelector('[aria-hidden="true"]')!.textContent).toBe('+');
  });

  it('lists the tool calls as a list', () => {
    const { card } = renderSink();
    expect(card.querySelectorAll('ul > li > [data-status]')).toHaveLength(8);
  });

  it('keeps the long labels readable: the summary toggle names its tools and errors', () => {
    renderSink();
    expect(screen.getByRole('button', { name: 'Tool results · Bash, Read · 1 error' })).toBeInTheDocument();
  });
});

describe('focus when a control is pressed', () => {
  /**
   * Was QA-1: pressing "Show less" on prose that is long only because it WRAPS (no hard
   * line breaks, which is what most reasoning and responses look like) dropped keyboard
   * focus to <body>. `ClampedText` showed the toggle while `measuredClamp ||
   * exceedsLines(...) || expanded`; on collapse `expanded` turned false in the same render
   * while `measuredClamp` held the measurement taken while the text was open (not clipped,
   * so false), the control unmounted for a frame, and the next frame's measurement
   * re-created it. Fixed by not measuring while the text is open: the answer from the
   * closed text stands (`useIsClamped`). The triad layout's own Show more/less is frozen.
   */
  it('keeps the "Show less" control mounted and focused after it is pressed', async () => {
    // jsdom has no layout; this stand-in clips an element only while it carries a line-clamp class.
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(() => 300);
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return /\bline-clamp-\d\b/.test(this.className) ? 80 : 300;
    });
    const prose = 'word '.repeat(400); // wraps over many visual lines, with no line break at all
    render(
      <TimelineNode
        interaction={makeNode({
          id: HARNESS_ID,
          author: 'human',
          model_name: 'claude-test-1',
          user_prompt: prose,
          model_response: null,
          tool_executions: [],
        })}
      />,
    );

    fireEvent.click(await screen.findByRole('button', { name: 'Show more' }));
    const less = screen.getByRole('button', { name: 'Show less' });
    // Let any re-measure of the opened text run, as it would a frame after opening.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 60));
    });
    less.focus();
    expect(document.activeElement).toBe(less);

    fireEvent.click(less);

    expect(less.isConnected, 'the pressed control was removed from the page').toBe(true);
    expect(document.activeElement, 'focus fell back to <body>').toBe(less);
    expect(less).toHaveTextContent('Show more'); // the same button, now offering to open again
  });

  it('keeps focus through a whole open, close, open, close cycle', async () => {
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(() => 300);
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return /\bline-clamp-\d\b/.test(this.className) ? 80 : 300;
    });
    render(
      <TimelineNode
        interaction={makeNode({ id: HARNESS_ID, author: 'system', user_prompt: '[tool_result Bash] ok', model_response: 'word '.repeat(500) })}
      />,
    );
    const toggle = await screen.findByRole('button', { name: 'Show more' });
    toggle.focus();
    for (let round = 0; round < 3; round++) {
      fireEvent.click(toggle);
      expect(toggle).toHaveAttribute('aria-expanded', 'true');
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 30));
      });
      fireEvent.click(toggle);
      expect(toggle.isConnected, `round ${round}`).toBe(true);
      expect(toggle).toHaveAttribute('aria-expanded', 'false');
      expect(document.activeElement, `round ${round}`).toBe(toggle);
    }
  });
});
