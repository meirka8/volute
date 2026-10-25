/**
 * Searching and filtering the timeline (issue #118), from the point of view of the page: the box, the
 * count, next and previous, the keys, the marks, the counts on folds, the filters and their chips, and
 * the address. (The same, over a list long enough to be windowed, is in ShadowTimeline.searchWindowed.test.tsx.)
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { useKeyboardNavigation } from '../../hooks/useKeyboardNavigation';
import { TimelineNode } from './TimelineNode';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { withoutIds } from '../../test/markup';
import * as fixtures from '../../test/fixtures';
import {
  generatedLink,
  injectionParentStep,
  injectionSystemStep,
  makeNode,
  mcpNode,
  promptStep,
  toolCall,
  wireToolCall,
} from '../../test/fixtures';
import type { CommitInfo } from '../../lib/timelineSearch';
import { normalizeInteraction, type CVCBlobData, type InteractionNode } from '../../types/cvc';
import { undrawableNode } from '../../test/brokenFixtures';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

const BASE = Date.parse('2026-03-02T09:15:00.000Z');
const harnessId = (n: number) => `1f0e5c3a-9d2b-8c4e-a1b7-${n.toString(16).padStart(12, '0')}`;
const HASH = 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678';
const HASH_B = 'b2c3d4e5f60718293a4b5c6d7e8f901234567890';
const SESSION = '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70';
const AGENT = `${SESSION}:agent-ab12cd34`;

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

/**
 * A human step that says "Step n" (and later steps are newer, so the timeline lists them first). The model name
 * says it too, which is how a card is told from the others whatever else the step holds or folds away.
 */
const step = (n: number, extra: Partial<InteractionNode> = {}) =>
  makeNode({ id: harnessId(n), author: 'human', timestamp: BASE + n * 1000, model_name: `Step ${n}`, user_prompt: `Step ${n}`, ...extra });

const box = () => screen.getByRole('searchbox', { name: 'Search steps' }) as HTMLInputElement;
const typeQuery = (value: string) => fireEvent.change(box(), { target: { value } });
const press = (key: string, init: KeyboardEventInit = {}) => fireEvent.keyDown(box(), { key, ...init });
const count = () => screen.getByRole('status').textContent;

const cardsOf = (container: HTMLElement) =>
  Array.from(container.querySelectorAll<HTMLElement>('ol > li')).filter((item) => item.querySelector('button[aria-pressed]') !== null);
/** Which steps are drawn, in order, by the "Step n" in each card. */
const stepsOf = (container: HTMLElement) => cardsOf(container).map((card) => Number(/Step (\d+)/.exec(card.textContent ?? '')?.[1]));
const currentOf = (container: HTMLElement) => cardsOf(container).filter((card) => card.getAttribute('aria-current') === 'true');
const marksIn = (root: ParentNode) => Array.from(root.querySelectorAll('mark'));
const markedText = (root: ParentNode) => marksIn(root).map((mark) => mark.textContent);
const openFilters = () => fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));
const lines = (n: number, label: string) => Array.from({ length: n }, (_, k) => `${label} ${k + 1}`).join('\n');

/** Five steps; 2 and 4 hold the word "needle", in a response and in a command. */
const FIVE = [
  step(1),
  step(2, { model_response: 'The needle is here' }),
  step(3),
  step(4, { tool_executions: [toolCall('Bash', { command: 'grep needle file.txt' })] }),
  step(5),
];

describe('the search box', () => {
  it('is a labelled box with a placeholder that says how to narrow a term, and says nothing until used', () => {
    render(<ShadowTimeline interactions={FIVE} />);
    expect(box()).toHaveAttribute('placeholder', 'Search steps — path: tool: commit:');
    expect(box()).toHaveValue('');
    expect(count()).toBe('');
    expect(screen.getByRole('button', { name: 'Filters' })).toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByRole('button', { name: 'Previous match' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Next match' })).toBeDisabled();
    expect(screen.queryByRole('button', { name: 'Clear search' })).not.toBeInTheDocument();
  });

  it('hides the steps that do not match as you type, keeps the order of those that do, and counts them', () => {
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    expect(stepsOf(container)).toEqual([5, 4, 3, 2, 1]);
    typeQuery('needle');
    expect(stepsOf(container)).toEqual([4, 2]);
    expect(count()).toBe('1 of 2 · 2 of 5 steps');
    // The header still counts the steps of the pull request; the count of the search is its own.
    expect(screen.getByText('5 interactions linked to this PR')).toBeInTheDocument();
    typeQuery('needle is');
    expect(stepsOf(container)).toEqual([2]);
    expect(count()).toBe('1 of 1 · 1 of 5 steps');
  });

  it('puts every step back when the query is cleared', () => {
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    typeQuery('needle');
    fireEvent.click(screen.getByRole('button', { name: 'Clear search' }));
    expect(box()).toHaveValue('');
    expect(stepsOf(container)).toEqual([5, 4, 3, 2, 1]);
    expect(count()).toBe('');
    expect(box()).toHaveFocus();
  });

  it('says so when nothing matches, and offers to clear', () => {
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    typeQuery('zzzz');
    expect(cardsOf(container)).toHaveLength(0);
    expect(count()).toBe('No matches · 0 of 5 steps');
    expect(screen.queryByRole('list', { name: 'Interactions linked to this PR' })).not.toBeInTheDocument();
    expect(screen.getByText('No steps match')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Clear search and filters' }));
    expect(stepsOf(container)).toEqual([5, 4, 3, 2, 1]);
    expect(box()).toHaveValue('');
  });

  it('does not mistake a prefix still being typed for a query', () => {
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    typeQuery('path:');
    expect(stepsOf(container)).toEqual([5, 4, 3, 2, 1]);
    expect(count()).toBe('');
    typeQuery('   ');
    expect(stepsOf(container)).toEqual([5, 4, 3, 2, 1]);
  });

  it('works on a step that arrives while a search is on', () => {
    const { container, rerender } = render(<ShadowTimeline interactions={FIVE} />);
    typeQuery('needle');
    rerender(<ShadowTimeline interactions={[...FIVE, step(6, { model_response: 'another needle' })]} />);
    expect(stepsOf(container)).toEqual([6, 4, 2]);
    expect(count()).toBe('1 of 3 · 3 of 6 steps');
  });

  it('keeps what was typed when the list is replaced, and when it is loading and then loaded', () => {
    const { container, rerender } = render(<ShadowTimeline interactions={[]} isLoading />);
    expect(screen.getByText('Loading cognitive history...')).toBeInTheDocument();
    expect(screen.queryByRole('searchbox')).not.toBeInTheDocument();
    rerender(<ShadowTimeline interactions={FIVE} />);
    typeQuery('needle');
    rerender(<ShadowTimeline interactions={[]} />);
    expect(screen.getByText('No cognitive history')).toBeInTheDocument();
    rerender(<ShadowTimeline interactions={FIVE} />);
    expect(box()).toHaveValue('needle');
    expect(stepsOf(container)).toEqual([4, 2]);
  });
});

describe('next and previous', () => {
  it('start on the first match, move to the next on Enter and back on Shift+Enter, and wrap round', () => {
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    typeQuery('needle');
    expect(currentOf(container).map((card) => Number(/Step (\d+)/.exec(card.textContent!)![1]))).toEqual([4]);
    press('Enter');
    expect(count()).toBe('2 of 2 · 2 of 5 steps');
    expect(currentOf(container).map((card) => card.textContent!.includes('Step 2'))).toEqual([true]);
    press('Enter');
    expect(count()).toBe('1 of 2 · 2 of 5 steps');
    press('Enter', { shiftKey: true });
    expect(count()).toBe('2 of 2 · 2 of 5 steps');
    press('Enter', { shiftKey: true });
    expect(count()).toBe('1 of 2 · 2 of 5 steps');
  });

  it('are buttons too, and only there to use when there is somewhere to go', () => {
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    typeQuery('needle');
    fireEvent.click(screen.getByRole('button', { name: 'Next match' }));
    expect(count()).toBe('2 of 2 · 2 of 5 steps');
    fireEvent.click(screen.getByRole('button', { name: 'Previous match' }));
    expect(count()).toBe('1 of 2 · 2 of 5 steps');
    typeQuery('needle is');
    expect(screen.getByRole('button', { name: 'Next match' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Previous match' })).toBeDisabled();
    typeQuery('zzz');
    expect(currentOf(container)).toHaveLength(0);
  });

  it('outline the step with a class and aria-current, and nothing about it is selection', () => {
    const onSelect = vi.fn();
    const { container } = render(<ShadowTimeline interactions={FIVE} onSelectInteraction={onSelect} selectedInteractionId={harnessId(2)} />);
    typeQuery('needle');
    const [current] = currentOf(container);
    expect(current).toHaveClass('rr-current-match');
    expect(within(current).getByRole('button', { pressed: false })).toBeInTheDocument();
    press('Enter');
    press('Enter', { shiftKey: true });
    expect(onSelect).not.toHaveBeenCalled();
    // The selected step is the one it was; the search being on another does not change that.
    expect(screen.getAllByRole('button', { pressed: true })).toHaveLength(1);
    expect(screen.getByRole('button', { pressed: true }).closest('li')).toHaveTextContent('Step 2');
  });

  it('are not for a query that is only filters: there is no match to go to', () => {
    render(<ShadowTimeline interactions={FIVE} />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    expect(count()).toBe('1 of 5 steps');
    expect(screen.getByRole('button', { name: 'Next match' })).toBeDisabled();
  });

  it('start again from the first match when the query changes: a new query is a new search', () => {
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    typeQuery('needle');
    press('Enter'); // step 2
    expect(count()).toBe('2 of 2 · 2 of 5 steps');
    typeQuery('needle ');
    expect(count()).toBe('1 of 2 · 2 of 5 steps');
    expect(currentOf(container).map((card) => card.textContent!.includes('Step 4'))).toEqual([true]);
  });

  it('are not announced as a change of what is selected, but the count is in a polite live region', () => {
    render(<ShadowTimeline interactions={FIVE} />);
    const status = screen.getByRole('status');
    expect(status).toHaveAttribute('aria-live', 'polite');
    expect(status).toHaveAttribute('aria-atomic', 'true');
  });
});

describe('the keys', () => {
  it('Escape clears the box first, and then leaves it', () => {
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    box().focus();
    typeQuery('needle');
    press('Escape');
    expect(box()).toHaveValue('');
    expect(stepsOf(container)).toEqual([5, 4, 3, 2, 1]);
    expect(box()).toHaveFocus();
    press('Escape');
    expect(box()).not.toHaveFocus();
  });

  it('Escape does not clear the filters: they have chips for that', () => {
    render(<ShadowTimeline interactions={FIVE} />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    box().focus();
    press('Escape');
    expect(box()).not.toHaveFocus();
    expect(screen.getByRole('list', { name: 'Active filters' })).toBeInTheDocument();
  });

  it('/ comes to the box from the page, with what is in it selected', () => {
    render(<ShadowTimeline interactions={FIVE} />);
    typeQuery('needle');
    box().blur();
    const event = new KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true });
    document.body.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    expect(box()).toHaveFocus();
    expect(box().selectionStart).toBe(0);
    expect(box().selectionEnd).toBe('needle'.length);
  });

  it('/ works from a button, which does not use it, such as the header of a card', () => {
    render(<ShadowTimeline interactions={FIVE} />);
    const header = screen.getAllByRole('button', { pressed: false })[0];
    header.focus();
    fireEvent.keyDown(header, { key: '/' });
    expect(box()).toHaveFocus();
  });

  it('/ is a character when it is typed into a box, a text area, a select or an editable region', () => {
    render(
      <>
        <input aria-label="other box" />
        <textarea aria-label="notes" />
        <div role="textbox" tabIndex={0} aria-label="rich" />
        <div contentEditable suppressContentEditableWarning aria-label="editable" />
        <ShadowTimeline interactions={FIVE} />
      </>,
    );
    for (const target of [screen.getByLabelText('other box'), screen.getByLabelText('notes'), screen.getByRole('textbox', { name: 'rich' }), screen.getByLabelText('editable')]) {
      target.focus();
      const event = new KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true });
      target.dispatchEvent(event);
      expect(event.defaultPrevented).toBe(false);
      expect(box()).not.toHaveFocus();
    }
  });

  it('/ with a modifier, or not alone, is left alone', () => {
    render(<ShadowTimeline interactions={FIVE} />);
    for (const init of [{ ctrlKey: true }, { metaKey: true }, { altKey: true }, { repeat: true }]) {
      document.body.dispatchEvent(new KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true, ...init }));
      expect(box()).not.toHaveFocus();
    }
    document.body.dispatchEvent(new KeyboardEvent('keydown', { key: '?', shiftKey: true, bubbles: true, cancelable: true }));
    expect(box()).not.toHaveFocus();
  });

  it('typing j, k, x or a space in the box is typing, not the page shortcuts', () => {
    const on = { onNext: vi.fn(), onPrevious: vi.fn(), onExpand: vi.fn(), onMarkViewed: vi.fn() };
    function Page() {
      useKeyboardNavigation(on);
      return <ShadowTimeline interactions={FIVE} />;
    }
    render(<Page />);
    for (const key of ['j', 'k', 'x', ' ']) {
      const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true });
      box().dispatchEvent(event);
      expect(event.defaultPrevented).toBe(false);
    }
    expect(on.onNext).not.toHaveBeenCalled();
    expect(on.onPrevious).not.toHaveBeenCalled();
    expect(on.onExpand).not.toHaveBeenCalled();
    expect(on.onMarkViewed).not.toHaveBeenCalled();
    // ...and they are still the shortcuts anywhere else on the page.
    document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'j', bubbles: true, cancelable: true }));
    expect(on.onNext).toHaveBeenCalledTimes(1);
  });

  it('ignores Enter while text is being composed', () => {
    render(<ShadowTimeline interactions={FIVE} />);
    typeQuery('needle');
    fireEvent.keyDown(box(), { key: 'Enter', isComposing: true });
    expect(count()).toBe('1 of 2 · 2 of 5 steps');
  });
});

describe('what is marked', () => {
  it('draws what matches in <mark> elements that hold nothing but text, in the case it was written in', () => {
    const { container } = render(<ShadowTimeline interactions={[promptStep]} />);
    typeQuery('retry');
    const marks = marksIn(container);
    expect(marks.length).toBeGreaterThan(0);
    expect(new Set(marks.map((mark) => mark.textContent))).toEqual(new Set(['retry', 'Retry']));
    for (const mark of marks) {
      expect(mark.children).toHaveLength(0);
      expect(Array.from(mark.childNodes).every((node) => node.nodeType === Node.TEXT_NODE)).toBe(true);
      expect(mark).toHaveClass('rr-mark');
    }
    // The text is the text: marking took nothing out and put nothing in.
    expect(screen.getByText(/Add a/).textContent).toBe(promptStep.user_prompt);
  });

  it('marks every term, wherever it occurs: the prompt, the response, a command and the diff', () => {
    const { container } = render(<ShadowTimeline interactions={[promptStep]} />);
    typeQuery('check script');
    const text = markedText(container).join('|').toLowerCase();
    expect(text).toContain('check');
    expect(text).toContain('script');
    // In the response and in the command (a code block), and not only where it first occurs.
    expect(marksIn(screen.getByText(/I'll cap the retries/)).length).toBeGreaterThan(0);
    expect(marksIn(container.querySelector('pre')!).length).toBeGreaterThan(0);
  });

  it('marks nothing while there is no query, or only filters', () => {
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    expect(marksIn(container)).toHaveLength(0);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    expect(marksIn(container)).toHaveLength(0);
  });

  it('marks a term with a prefix only where its field is shown', () => {
    const node = makeNode({
      id: harnessId(10),
      author: 'human',
      user_prompt: 'run bash and read fetch.ts for me',
      model_response: 'I ran bash and read fetch.ts',
      tool_executions: [wireToolCall('Bash', { command: 'ls' }), wireToolCall('Read', { file_path: '/repo/src/fetch.ts' })],
      artifact_links: [generatedLink(harnessId(10), HASH)],
    });
    const { container } = render(<ShadowTimeline interactions={[node]} />);

    typeQuery('tool:bash');
    expect(markedText(container)).toEqual(['Bash']); // the name of the call, and not the prose
    expect(marksIn(screen.getByText(/I ran/))).toHaveLength(0);

    typeQuery('path:fetch.ts');
    expect(markedText(container)).toEqual(['fetch.ts']); // the path of the file read, not the words of the prompt and response

    typeQuery('commit:a1b2c');
    expect(markedText(container)).toEqual(['a1b2c']); // the short hash on the chip
  });

  it('does not mark the model name, the summary of a group of results, or the words the Reviewer wrote itself', () => {
    const node = makeNode({
      id: harnessId(11),
      author: 'system',
      model_name: 'claude-opus-4-5-20251101',
      user_prompt: '[tool_result Bash] fine',
      model_response: 'ok',
    });
    const { container } = render(<ShadowTimeline interactions={[node]} />);
    typeQuery('opus');
    // "opus" is nowhere in the step but the model name: no step matches, and nothing is marked.
    expect(cardsOf(container)).toHaveLength(0);
    typeQuery('bash');
    // The name of the group of results ("Tool results · Bash") is drawn by the Reviewer: the match is counted on it.
    expect(screen.getByRole('button', { name: /^Tool results · Bash · 1 match$/ })).toBeInTheDocument();
    expect(marksIn(screen.getByRole('button', { name: /^Tool results/ }))).toHaveLength(0);
  });

  it('draws no more than a hundred marks in one block of text', () => {
    const node = step(20, { model_response: 'x '.repeat(500) });
    const { container } = render(<ShadowTimeline interactions={[node]} />);
    typeQuery('x');
    const perBlock = Array.from(container.querySelectorAll('p')).map((paragraph) => marksIn(paragraph).length);
    expect(Math.max(...perBlock)).toBe(100);
    expect(container.querySelectorAll('mark')).toHaveLength(100);
  });

  it('can not be hidden from by a character that draws nothing, and is marked on either side of it', () => {
    const hidden = step(21, { model_response: 'reset the pass\u200Bword now' });
    const { container } = render(<ShadowTimeline interactions={[hidden]} />);
    typeQuery('password');
    // The step is found: the search reads the text without the hidden character...
    expect(stepsOf(container)).toEqual([21]);
    // ...the text is drawn with a token in the middle of the word, so the match is marked on either side of the token (a
    // mark holds text and nothing else, so the token is between two marks and in neither), and the card says why there is a token.
    expect(markedText(container)).toEqual(['pass', 'word']);
    expect(container.querySelector('[data-hidden-char="U+200B"]')).not.toBeNull();
    expect(screen.getByRole('note')).toHaveTextContent('hidden or bidirectional Unicode characters');
    // A match on one side of it is marked.
    typeQuery('reset');
    expect(markedText(container)).toEqual(['reset']);
  });

  it('marks in the cards of the other layout too', () => {
    const { container } = render(<ShadowTimeline interactions={[mcpNode]} />);
    typeQuery('backoff');
    expect(markedText(container).map((text) => text?.toLowerCase())).toEqual(['backoff']);
  });

  it('marks within lines of a diff', () => {
    const edit = makeNode({
      id: harnessId(12),
      author: 'human',
      user_prompt: 'edit',
      tool_executions: [wireToolCall('Edit', { file_path: '/repo/a.ts', old_string: 'const a = 1;', new_string: 'const answer = 42;' })],
    });
    const { container } = render(<ShadowTimeline interactions={[edit]} />);
    typeQuery('answer');
    const diff = container.querySelector('[aria-label="Diff"]')!;
    expect(markedText(diff)).toEqual(['answer']);
    expect(diff.querySelector('ins mark')).not.toBeNull();
  });
});

describe('folded text', () => {
  it('is not opened by a search, and says how many matches it holds', () => {
    const { container } = render(<ShadowTimeline interactions={[step(1, { author: 'system', user_prompt: `[tool_result Bash] alpha needle beta\n\n[tool_result Read] needle needle` })]} />);
    const toggle = screen.getByRole('button', { name: 'Tool results · Bash, Read' });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    typeQuery('needle');
    const counted = screen.getByRole('button', { name: 'Tool results · Bash, Read · 3 matches' });
    expect(counted).toHaveAttribute('aria-expanded', 'false');
    expect(marksIn(container)).toHaveLength(0);
    // Opening it shows the marks, and the count goes: they are the count.
    fireEvent.click(counted);
    expect(marksIn(container)).toHaveLength(3);
    expect(screen.getByRole('button', { name: 'Tool results · Bash, Read' })).toHaveAttribute('aria-expanded', 'true');
    // Folded again, the count is back.
    fireEvent.click(screen.getByRole('button', { name: 'Tool results · Bash, Read' }));
    expect(screen.getByRole('button', { name: 'Tool results · Bash, Read · 3 matches' })).toBeInTheDocument();
  });

  it('says "1 match" for one', () => {
    render(<ShadowTimeline interactions={[step(1, { author: 'system', user_prompt: '[tool_result Bash] a needle' })]} />);
    typeQuery('needle');
    expect(screen.getByRole('button', { name: 'Tool results · Bash · 1 match' })).toBeInTheDocument();
  });

  it('counts the matches of a group with an error, and keeps its marker', () => {
    render(<ShadowTimeline interactions={[step(1, { author: 'system', user_prompt: '[tool_result Bash error] boom needle' })]} />);
    typeQuery('needle');
    expect(screen.getByRole('button', { name: 'Tool results · Bash · 1 error · 1 match' })).toBeInTheDocument();
  });

  it('counts on the "Show all N lines" of a block of code that is cut, and on "Show more" of prose that is cut', () => {
    const long = step(2, {
      model_response: `${lines(9, 'response')}\nthe needle`,
      tool_executions: [toolCall('Bash', { command: `${lines(30, 'echo')}\necho needle\necho needle` })],
    });
    render(<ShadowTimeline interactions={[long]} />);
    expect(screen.getByRole('button', { name: 'Show all 32 lines' })).toBeInTheDocument();
    typeQuery('needle');
    expect(screen.getByRole('button', { name: 'Show all 32 lines · 2 matches' })).toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByRole('button', { name: 'Show more · 1 match' })).toHaveAttribute('aria-expanded', 'false');
    // Nothing was opened for the reader.
    expect(screen.queryByRole('button', { name: /Show less/ })).not.toBeInTheDocument();
  });

  it('counts on the subagent prompt behind its toggle, and on a diff that is cut', () => {
    const node = makeNode({
      id: harnessId(30),
      author: 'human',
      user_prompt: 'delegate',
      tool_executions: [
        wireToolCall('Agent', { description: 'Look', prompt: 'find the needle in the haystack', subagent_type: 'Explore' }),
        wireToolCall('Edit', { file_path: '/repo/a.ts', old_string: lines(40, 'old'), new_string: `${lines(39, 'new')}\nneedle` }),
      ],
    });
    render(<ShadowTimeline interactions={[node]} />);
    typeQuery('needle');
    expect(screen.getByRole('button', { name: 'Show subagent prompt · 1 match' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^Show all \d+ lines · 1 match$/ })).toBeInTheDocument();
  });

  describe('and the controls that draw more of a long list', () => {
    const showMore = (name: RegExp) => screen.getByRole('button', { name });

    it('say how many matches the tool calls not drawn yet hold', () => {
      const calls = Array.from({ length: 12 }, (_, k) => toolCall('Bash', { command: k === 11 ? 'echo needle' : `echo call ${k}` }));
      render(<ShadowTimeline interactions={[step(1, { tool_executions: calls })]} />);
      expect(showMore(/^Show 2 more tool calls$/)).toBeInTheDocument();
      typeQuery('needle');
      expect(showMore(/^Show 2 more tool calls · 1 match$/)).toBeInTheDocument();
      // Drawn, the match is marked, and the control goes.
      fireEvent.click(showMore(/^Show 2 more tool calls/));
      expect(screen.queryByRole('button', { name: /^Show \d+ more tool calls/ })).not.toBeInTheDocument();
      expect(markedText(document.body)).toContain('needle');
    });

    it('say it for the results of a group, and for the parts of a stimulus', () => {
      const results = Array.from({ length: 25 }, (_, k) => `[tool_result Bash] output ${k}${k === 23 ? ' needle' : ''}`).join('\n\n');
      const parts = Array.from({ length: 105 }, (_, k) => `[meta] note ${k}${k === 103 ? ' needle' : ''}`).join('\n\n');
      render(<ShadowTimeline interactions={[step(1, { user_prompt: results }), step(2, { author: 'system', user_prompt: parts })]} />);
      typeQuery('needle');
      expect(showMore(/^Show 5 more tool results · 1 match$/)).toBeInTheDocument();
      expect(showMore(/^Show 5 more parts · 1 match$/)).toBeInTheDocument();
    });

    it('say it for the fields, the items of a checklist and the edits of a call', () => {
      const fields = Object.fromEntries(Array.from({ length: 30 }, (_, k) => [`field_${String(k).padStart(2, '0')}`, k === 27 ? 'a needle' : `value ${k}`]));
      const todos = Array.from({ length: 105 }, (_, k) => ({ activeForm: `Doing ${k}`, content: k === 103 ? 'Find the needle' : `Item ${k}`, status: 'pending' }));
      const edits = Array.from({ length: 6 }, (_, k) => ({ new_string: k === 5 ? 'needle' : `new ${k}`, old_string: `old ${k}` }));
      const node = makeNode({
        id: harnessId(40),
        author: 'human',
        user_prompt: 'many things',
        tool_executions: [
          wireToolCall('mcp__srv__bulk', fields),
          wireToolCall('TodoWrite', { todos }),
          wireToolCall('MultiEdit', { edits, file_path: '/repo/a.ts' }),
        ],
      });
      render(<ShadowTimeline interactions={[node]} />);
      typeQuery('needle');
      expect(showMore(/^Show 5 more fields · 1 match$/)).toBeInTheDocument();
      expect(showMore(/^Show 5 more items · 1 match$/)).toBeInTheDocument();
      expect(showMore(/^Show 1 more edit · 1 match$/)).toBeInTheDocument();
    });

    it('say nothing when the items not drawn hold no match, or nothing is searched', () => {
      const calls = Array.from({ length: 12 }, (_, k) => toolCall('Bash', { command: k === 0 ? 'echo needle' : `echo call ${k}` }));
      render(<ShadowTimeline interactions={[step(1, { tool_executions: calls })]} />);
      typeQuery('needle');
      expect(showMore(/^Show 2 more tool calls$/)).toBeInTheDocument();
      typeQuery('');
      expect(showMore(/^Show 2 more tool calls$/)).toBeInTheDocument();
    });
  });

  it('counts on the triad layout: "Why?" and "Show more"', () => {
    render(<ShadowTimeline interactions={[mcpNode]} />);
    typeQuery('jitter');
    const why = screen.getByRole('button', { name: /^Why\?/ });
    expect(why).toHaveTextContent('Why? · 1 match');
    expect(why).toHaveAttribute('class'); // (the wording is the only change: the same control)
    // Asking why shows the reasoning, with the match marked in it, and the count goes.
    fireEvent.click(why);
    expect(screen.getByRole('button', { name: 'Why?' })).toBeInTheDocument();
    expect(markedText(screen.getByText('Decision Factors').parentElement!)).toEqual(['jitter']);
  });

  it('counts on the "Show more" of the triad layout once its text is cut off', async () => {
    vi.spyOn(Element.prototype, 'scrollHeight', 'get').mockReturnValue(200);
    vi.spyOn(Element.prototype, 'clientHeight', 'get').mockReturnValue(40);
    render(<ShadowTimeline interactions={[mcpNode]} />);
    typeQuery('retry');
    const more = await screen.findAllByRole('button', { name: /^Show more/ });
    expect(more.map((button) => button.textContent)).toEqual(['Show more · 2 matches', 'Show more']);
  });
});

describe('filters', () => {
  const NODES = [
    step(1, { conversation_id: SESSION, tool_executions: [toolCall('Bash', { command: 'ls' })], artifact_links: [generatedLink(harnessId(1), HASH)] }),
    step(2, { conversation_id: SESSION, author: 'system', user_prompt: '[tool_result Bash] Step 2 ok', tool_executions: [toolCall('Read', { file_path: '/work/repo/src/a.ts' })], context_items: [{ file_path: '/work/repo/src/a.ts' }], artifact_links: [{ interaction_id: harnessId(2), git_commit_hash: HASH_B, link_type: 'temporal' }] }),
    step(3, { conversation_id: AGENT, tool_executions: [toolCall('Grep', { pattern: 'x' })], artifact_links: [generatedLink(harnessId(3), HASH)] }),
    step(4, { conversation_id: AGENT, author: 'system', user_prompt: '[tool_result Grep] Step 4 ok', artifact_links: [generatedLink(harnessId(4), HASH_B)] }),
  ];
  const COMMITS: CommitInfo[] = [
    { sha: HASH, subject: 'feat: the first' },
    { sha: HASH_B, subject: 'fix: the second' },
  ];

  it('is a disclosure that opens the filters, which list what the steps have, with counts', () => {
    render(<ShadowTimeline interactions={NODES} commits={COMMITS} openFile="src/a.ts" />);
    const toggle = screen.getByRole('button', { name: 'Filters' });
    expect(screen.queryByRole('group', { name: 'Filters' })).not.toBeInTheDocument();
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    const panel = screen.getByRole('group', { name: 'Filters' });
    expect(toggle).toHaveAttribute('aria-controls', panel.id);
    for (const legend of ['Conversation', 'Stimulus origin', 'Tool', 'Link type', 'Commit']) {
      expect(within(panel).getByRole('group', { name: legend })).toBeInTheDocument();
    }
    const conversation = within(panel).getByRole('group', { name: 'Conversation' });
    // Each label: the name, the number of steps, for a subagent the session it belongs to, and the first words of what the conversation was asked.
    expect(within(conversation).getAllByRole('checkbox').map((box) => box.closest('label')!.textContent)).toEqual([
      'Session 5c1d8e2a 2Step 1',
      'Subagent ab12cd34 2of session 5c1d8e2aStep 3',
    ]);
    expect(within(panel).getByRole('checkbox', { name: /Prompt 2/ })).toBeInTheDocument();
    expect(within(panel).getByRole('checkbox', { name: /Tool results 2/ })).toBeInTheDocument();
    expect(within(panel).getByRole('checkbox', { name: /Bash 1/ })).toBeInTheDocument();
    expect(within(panel).getByRole('checkbox', { name: /Generated 3/ })).toBeInTheDocument();
    expect(within(panel).getByRole('checkbox', { name: /Temporal 1/ })).toBeInTheDocument();
    expect(within(panel).queryByRole('checkbox', { name: /Verified/ })).not.toBeInTheDocument(); // only the kinds present
    expect(within(panel).getByRole('checkbox', { name: /Touches the open file/ })).toBeInTheDocument();
  });

  it('offers the commits of the pull request, newest first, with the subject', () => {
    render(<ShadowTimeline interactions={NODES} commits={COMMITS} />);
    openFilters();
    const commit = within(screen.getByRole('group', { name: 'Commit' }));
    const labels = commit.getAllByRole('checkbox').map((box) => box.closest('label')!.textContent);
    expect(labels).toEqual([`${HASH_B.slice(0, 7)} 2fix: the second`, `${HASH.slice(0, 7)} 2feat: the first`]);
  });

  it('narrows the steps as boxes are ticked, with a chip for each, and the count says how many are left', () => {
    const { container } = render(<ShadowTimeline interactions={NODES} commits={COMMITS} />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Subagent ab12cd34/ }));
    expect(stepsOf(container)).toEqual([4, 3]);
    expect(count()).toBe('2 of 4 steps');
    expect(screen.getByRole('button', { name: 'Filters (1 active)' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Remove filter Conversation: Subagent ab12cd34' })).toBeInTheDocument();

    fireEvent.click(screen.getByRole('checkbox', { name: /Prompt/ }));
    expect(stepsOf(container)).toEqual([3]);
    expect(count()).toBe('1 of 4 steps');
    expect(screen.getByRole('button', { name: 'Filters (2 active)' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Remove filter Origin: Prompt' })).toBeInTheDocument();
  });

  it('shows the steps of a subagent with the commit links of the session intact', () => {
    const { container } = render(<ShadowTimeline interactions={NODES} commits={COMMITS} />);
    const before = cardsOf(container).map((card) => Array.from(card.querySelectorAll('[aria-label*="commit "]')).map((chip) => chip.getAttribute('aria-label')));
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Subagent ab12cd34/ }));
    const after = cardsOf(container);
    expect(after).toHaveLength(2);
    // Each card of the subagent still shows the link it had, with the short hash of the commit.
    expect(within(after[1]).getByLabelText(new RegExp(`commit ${HASH.slice(0, 7)}`))).toBeInTheDocument();
    expect(within(after[0]).getByLabelText(new RegExp(`commit ${HASH_B.slice(0, 7)}`))).toBeInTheDocument();
    // And they are the links the steps had without the filter: nothing was added or taken away.
    expect(before.flat().filter((label) => label?.includes(HASH.slice(0, 7)))).toHaveLength(2);
  });

  it('keeps the checks of the parent of a step when the parent itself is filtered out', () => {
    const note = /Shown undivided: the tool results named here don't match the previous step's tool calls/;
    const { container } = render(<ShadowTimeline interactions={[injectionParentStep, injectionSystemStep]} />);
    expect(screen.getAllByText(note)).toHaveLength(1);
    typeQuery('green test run'); // only the child's response has it
    expect(cardsOf(container)).toHaveLength(1);
    expect(screen.getAllByText(note)).toHaveLength(1);
  });

  it('combines a query with the filters', () => {
    const { container } = render(<ShadowTimeline interactions={NODES} />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Tool results/ }));
    expect(stepsOf(container)).toEqual([4, 2]);
    typeQuery('ok');
    expect(stepsOf(container)).toEqual([4, 2]);
    typeQuery('"step 4"');
    expect(stepsOf(container)).toEqual([4]);
    expect(count()).toBe('1 of 1 · 1 of 4 steps');
  });

  it('moves keyboard focus to the Filters control when the chip that had it is removed', () => {
    render(<ShadowTimeline interactions={NODES} />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Prompt/ }));
    const chip = screen.getByRole('button', { name: 'Remove filter Tool: Bash' });
    chip.focus();
    fireEvent.click(chip);
    expect(screen.getByRole('button', { name: /^Filters/ })).toHaveFocus();
    fireEvent.click(screen.getByRole('button', { name: 'Remove filter Origin: Prompt' }));
    expect(screen.getByRole('button', { name: 'Filters' })).toHaveFocus();
  });

  it('removes one filter with its chip, and all of them with "Clear filters"', () => {
    const { container } = render(<ShadowTimeline interactions={NODES} />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Subagent ab12cd34/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Prompt/ }));
    expect(stepsOf(container)).toEqual([]);
    expect(screen.getAllByRole('button', { name: /^Remove filter/ })).toHaveLength(3);
    fireEvent.click(screen.getByRole('button', { name: 'Remove filter Tool: Bash' }));
    expect(stepsOf(container)).toEqual([3]);
    expect(screen.getAllByRole('button', { name: /^Remove filter/ })).toHaveLength(2);
    fireEvent.click(screen.getByRole('button', { name: 'Clear filters' }));
    expect(stepsOf(container)).toEqual([4, 3, 2, 1]);
    expect(screen.queryByRole('list', { name: 'Active filters' })).not.toBeInTheDocument();
    expect(count()).toBe('');
  });

  it('needs any of the values ticked in one group, and one of each group', () => {
    const { container } = render(<ShadowTimeline interactions={NODES} />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Grep/ }));
    expect(stepsOf(container)).toEqual([3, 1]);
    fireEvent.click(screen.getByRole('checkbox', { name: /Session 5c1d8e2a/ }));
    expect(stepsOf(container)).toEqual([1]);
  });

  it('filters by link type and by commit', () => {
    const { container } = render(<ShadowTimeline interactions={NODES} commits={COMMITS} />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Temporal/ }));
    expect(stepsOf(container)).toEqual([2]);
    fireEvent.click(screen.getByRole('button', { name: 'Remove filter Link: Temporal' }));
    fireEvent.click(screen.getByRole('checkbox', { name: new RegExp(`${HASH.slice(0, 7)} 2feat`) }));
    expect(stepsOf(container)).toEqual([3, 1]);
  });

  it('touches the open file: follows the file as it changes, and is off when none is open', () => {
    const { container, rerender } = render(<ShadowTimeline interactions={NODES} openFile="src/a.ts" />);
    openFilters();
    const toggle = screen.getByRole('checkbox', { name: /Touches the open file/ });
    expect(toggle).not.toBeChecked();
    fireEvent.click(toggle);
    expect(stepsOf(container)).toEqual([2]);
    expect(screen.getByRole('button', { name: 'Remove filter File: src/a.ts' })).toBeInTheDocument();
    // The reader opens another file: the filter means that one now.
    rerender(<ShadowTimeline interactions={NODES} openFile="src/other.ts" />);
    expect(stepsOf(container)).toEqual([]);
    expect(screen.getByRole('button', { name: 'Remove filter File: src/other.ts' })).toBeInTheDocument();
    rerender(<ShadowTimeline interactions={NODES} openFile="src/a.ts" />);
    expect(stepsOf(container)).toEqual([2]);
    fireEvent.click(screen.getByRole('checkbox', { name: /Touches the open file/ }));
    expect(stepsOf(container)).toEqual([4, 3, 2, 1]);
  });

  it('can not be turned on with no file open', () => {
    render(<ShadowTimeline interactions={NODES} openFile={null} />);
    openFilters();
    expect(screen.getByRole('checkbox', { name: /Touches the open file/ })).toBeDisabled();
  });

  it('is told apart from the file of a link: a file named by the address stays that file', () => {
    window.history.replaceState(null, '', '/#file=src%2Fa.ts');
    const { container, rerender } = render(<ShadowTimeline interactions={NODES} openFile="src/other.ts" />);
    expect(stepsOf(container)).toEqual([2]);
    rerender(<ShadowTimeline interactions={NODES} openFile="src/third.ts" />);
    expect(stepsOf(container)).toEqual([2]);
    expect(screen.getByRole('button', { name: 'Remove filter File: src/a.ts' })).toBeInTheDocument();
  });

  it('says what a filter is when its value is not one the steps have, rather than hiding it', () => {
    window.history.replaceState(null, '', `/#conv=${SESSION}-gone&tool=Teleport`);
    const { container } = render(<ShadowTimeline interactions={NODES} />);
    expect(stepsOf(container)).toEqual([]);
    expect(screen.getByRole('button', { name: 'Remove filter Tool: Teleport' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: `Remove filter Conversation: Session ${SESSION.slice(0, 8)}` })).toBeInTheDocument();
  });

  it('draws the names that come from the steps as text, with their hidden characters shown', () => {
    const node = step(1, { tool_executions: [toolCall('Ba\u202Esh', { command: 'ls' })] });
    render(<ShadowTimeline interactions={[node]} />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Ba/ }));
    const chip = screen.getByRole('button', { name: /^Remove filter Tool: Ba⟨U\+202E⟩sh$/ });
    expect(chip.querySelector('[data-hidden-char="U+202E"]')).not.toBeNull();
  });
});

describe('what a card remembers', () => {
  it('keeps what the reader opened in a step while a search takes the step off the page', () => {
    const results = step(1, { author: 'system', user_prompt: '[tool_result Bash] needle in the output' });
    const other = step(2, { model_response: 'nothing here' });
    const { container } = render(<ShadowTimeline interactions={[results, other]} />);
    fireEvent.click(screen.getByRole('button', { name: 'Tool results · Bash' }));
    expect(screen.getByRole('button', { name: 'Tool results · Bash' })).toHaveAttribute('aria-expanded', 'true');
    typeQuery('nothing');
    expect(stepsOf(container)).toEqual([2]);
    expect(screen.queryByRole('button', { name: /Tool results/ })).not.toBeInTheDocument();
    typeQuery('');
    expect(stepsOf(container)).toEqual([2, 1]);
    expect(screen.getByRole('button', { name: 'Tool results · Bash' })).toHaveAttribute('aria-expanded', 'true');
  });

  it('keeps a fold that starts open and the reader shut, shut', () => {
    const human = step(1, { user_prompt: '[tool_result Bash] needle' });
    const { container } = render(<ShadowTimeline interactions={[human, step(2)]} />);
    expect(screen.getByRole('button', { name: 'Tool results · Bash' })).toHaveAttribute('aria-expanded', 'true');
    fireEvent.click(screen.getByRole('button', { name: 'Tool results · Bash' }));
    typeQuery('Step 2');
    expect(stepsOf(container)).toEqual([2]);
    typeQuery('');
    expect(screen.getByRole('button', { name: 'Tool results · Bash' })).toHaveAttribute('aria-expanded', 'false');
  });

  it('keeps long text expanded, code shown in full, a list paged further, and the triad "Why?" open', () => {
    const long = step(3, {
      model_response: `${lines(9, 'response')}`,
      tool_executions: [
        toolCall('Bash', { command: lines(30, 'echo') }),
        ...Array.from({ length: 12 }, (_, k) => toolCall('Bash', { command: `echo call-${k}` })),
      ],
    });
    const { container } = render(<ShadowTimeline interactions={[long, mcpNode]} />);
    fireEvent.click(screen.getByRole('button', { name: 'Show more' }));
    fireEvent.click(screen.getByRole('button', { name: 'Show all 30 lines' }));
    fireEvent.click(screen.getByRole('button', { name: /^Show 3 more tool calls/ }));
    fireEvent.click(screen.getByRole('button', { name: 'Why?' }));
    expect(screen.getByText('Decision Factors')).toBeInTheDocument();

    typeQuery('zzzz');
    expect(cardsOf(container)).toHaveLength(0);
    typeQuery('');
    expect(screen.getAllByRole('button', { name: 'Show less' }).length).toBeGreaterThanOrEqual(2);
    expect(screen.queryByRole('button', { name: /^Show 3 more tool calls/ })).not.toBeInTheDocument(); // all 13 are drawn
    expect(screen.getByText('Decision Factors')).toBeInTheDocument();
  });

  it('forgets nothing when a search narrows and widens again, step by step', () => {
    const a = step(1, { author: 'system', user_prompt: '[tool_result Bash] first' });
    const b = step(2, { author: 'system', user_prompt: '[tool_result Read] second' });
    const { container } = render(<ShadowTimeline interactions={[a, b]} />);
    fireEvent.click(screen.getByRole('button', { name: 'Tool results · Read' }));
    typeQuery('first');
    fireEvent.click(screen.getByRole('button', { name: 'Tool results · Bash · 1 match' }));
    typeQuery('');
    expect(stepsOf(container)).toEqual([2, 1]);
    expect(screen.getByRole('button', { name: 'Tool results · Read' })).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByRole('button', { name: 'Tool results · Bash' })).toHaveAttribute('aria-expanded', 'true');
  });
});

describe('the address', () => {
  // Round 3: the address keeps the filters, in its fragment, at once; it never keeps the text of the search (a link that is
  // copied does, see the tests of the bar). A fragment that brings a search is applied, and the search leaves the address.
  beforeEach(() => {
    vi.useFakeTimers();
  });
  const hash = () => window.location.hash;

  it('is written at once when a filter changes, with the path, the query string and the other parameters as they were', () => {
    window.history.replaceState(null, '', '/pr/o/r/1?tab=files#top');
    render(<ShadowTimeline interactions={FIVE} />);
    expect(hash()).toBe('#top');
    typeQuery('needle');
    typeQuery('needle is');
    // The text of a search is never in the address.
    expect(hash()).toBe('#top');
    expect(window.location.search).toBe('?tab=files');
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    // No timer to wait out.
    expect(hash()).toBe('#top&tool=Bash');
    expect(window.location.pathname).toBe('/pr/o/r/1');
    expect(window.location.search).toBe('?tab=files');
  });

  it('replaces the entry in the history, so choosing filters does not fill it, and typing writes nothing', () => {
    const replace = vi.spyOn(window.history, 'replaceState');
    const push = vi.spyOn(window.history, 'pushState');
    render(<ShadowTimeline interactions={FIVE} />);
    typeQuery('n');
    typeQuery('ne');
    typeQuery('need');
    expect(replace).not.toHaveBeenCalled();
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    expect(replace).toHaveBeenCalledTimes(1);
    expect(push).not.toHaveBeenCalled();
  });

  it('says the filters, one parameter for each value, and takes them out when they are cleared', () => {
    window.history.replaceState(null, '', '/?keep=1#keep=2');
    render(<ShadowTimeline interactions={FIVE} />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Prompt/ }));
    typeQuery('needle');
    expect(hash()).toBe('#keep=2&origin=prompt&tool=Bash');
    fireEvent.click(screen.getByRole('button', { name: 'Clear search' }));
    fireEvent.click(screen.getByRole('button', { name: 'Clear filters' }));
    expect(hash()).toBe('#keep=2');
    expect(window.location.search).toBe('?keep=1');
  });

  it('does not write anything for a timeline that goes away, and writes nothing to the address of the page that follows', () => {
    window.history.replaceState(null, '', '/pr/o/r/1');
    const { unmount } = render(<ShadowTimeline interactions={FIVE} />);
    openFilters();
    typeQuery('needle');
    // The router moves on before the old page goes away.
    window.history.pushState(null, '', '/pr/o/r/2');
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    unmount();
    expect(window.location.pathname + window.location.search + window.location.hash).toBe('/pr/o/r/2');
  });

  it('reproduces the filters when the timeline comes back, or the page is loaded again, and not the text', () => {
    const first = render(<ShadowTimeline interactions={FIVE} />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    typeQuery('grep');
    expect(stepsOf(first.container)).toEqual([4]);
    first.unmount();

    const second = render(<ShadowTimeline interactions={FIVE} />);
    expect(box()).toHaveValue('');
    expect(stepsOf(second.container)).toEqual([4]);
    expect(screen.getByRole('button', { name: 'Remove filter Tool: Bash' })).toBeInTheDocument();
    expect(count()).toBe('1 of 5 steps');
  });

  it('opens on a link to a filtered view of a timeline that is still loading, and filters it once it is loaded', () => {
    window.history.replaceState(null, '', '/#q=needle');
    const { container, rerender } = render(<ShadowTimeline interactions={[]} isLoading />);
    rerender(<ShadowTimeline interactions={FIVE} />);
    expect(box()).toHaveValue('needle');
    expect(stepsOf(container)).toEqual([4, 2]);
    // The text was taken out of the address as the timeline took it.
    expect(hash()).toBe('');
  });

  it('applies a link at once and takes the text out of the address at once, keeping the rest of the fragment', () => {
    window.history.replaceState(null, '', '/pr/o/r/1?tab=files#top&q=needle&tool=Bash');
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    expect(box()).toHaveValue('needle');
    expect(stepsOf(container)).toEqual([4]);
    expect(hash()).toBe('#top&tool=Bash');
    expect(window.location.search).toBe('?tab=files');
  });

  it('does not read a search from the query string, which it never wrote', () => {
    window.history.replaceState(null, '', '/?q=needle&tool=Bash');
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    expect(box()).toHaveValue('');
    expect(stepsOf(container)).toEqual([5, 4, 3, 2, 1]);
    expect(window.location.search).toBe('?q=needle&tool=Bash');
  });

  it('ignores a value it can not use, without a word, and uses the rest', () => {
    window.history.replaceState(null, '', '/#origin=everything&link=nope&conv=has%20space&commit=zz&q=needle&tool=');
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    expect(screen.queryByRole('list', { name: 'Active filters' })).not.toBeInTheDocument();
    expect(box()).toHaveValue('needle');
    expect(stepsOf(container)).toEqual([4, 2]);
    // The address is tidied to what was understood of it, at once: nothing is left.
    expect(hash()).toBe('');
  });

  it('reads the text of the address as text', () => {
    window.history.replaceState(null, '', `/#q=${encodeURIComponent('<img src=x onerror=alert(1)>')}&tool=${encodeURIComponent('<b>bold</b>')}`);
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    expect(box()).toHaveValue('<img src=x onerror=alert(1)>');
    expect(container.querySelector('img')).toBeNull();
    expect(screen.getByRole('button', { name: 'Remove filter Tool: <b>bold</b>' })).toBeInTheDocument();
    expect(container.querySelector('b')).toBeNull();
  });

  it('writes the file the filter follows, and the new one when it changes', () => {
    const nodes = [step(1, { context_items: [{ file_path: '/w/src/a.ts' }] })];
    const { rerender } = render(<ShadowTimeline interactions={nodes} openFile="src/a.ts" />);
    openFilters();
    fireEvent.click(screen.getByRole('checkbox', { name: /Touches the open file/ }));
    expect(hash()).toBe('#file=src%2Fa.ts');
    rerender(<ShadowTimeline interactions={nodes} openFile="src/b.ts" />);
    expect(hash()).toBe('#file=src%2Fb.ts');
  });

  it('takes a link opened in the page itself (the fragment changes, the page does not load again) as it takes one at load', () => {
    window.history.replaceState(null, '', '/pr/o/r/1');
    const { container } = render(<ShadowTimeline interactions={FIVE} />);
    expect(stepsOf(container)).toEqual([5, 4, 3, 2, 1]);
    act(() => {
      window.history.pushState(null, '', '/pr/o/r/1#q=needle&tool=Bash');
      window.dispatchEvent(new HashChangeEvent('hashchange'));
    });
    expect(box()).toHaveValue('needle');
    expect(stepsOf(container)).toEqual([4]);
    expect(hash()).toBe('#tool=Bash');
    expect(screen.getByText(/This view was opened from a link with a search and filters applied/)).toBeInTheDocument();
  });

  it('starts a different pull request without the search of the last one', () => {
    const keyed = (key: string, interactions: InteractionNode[]) => <ShadowTimeline key={key} interactions={interactions} />;
    const { rerender } = render(keyed('o/r#1', FIVE));
    typeQuery('needle');
    rerender(keyed('o/r#2', FIVE));
    expect(box()).toHaveValue('');
  });
});

describe('the markup of a card', () => {
  const NODES = Object.values(fixtures).filter(
    (value): value is InteractionNode => typeof value === 'object' && value !== null && !Array.isArray(value) && 'artifact_links' in value && 'tool_executions' in value,
  );

  it('is exactly what it was while nothing is searched for: the timeline adds no element, attribute or text to a card', () => {
    expect(NODES.length).toBeGreaterThan(20);
    for (const node of NODES) {
      const alone = render(<TimelineNode interaction={node} />);
      const standalone = withoutIds((alone.container.firstElementChild as HTMLElement).innerHTML);
      alone.unmount();
      const inTimeline = render(<ShadowTimeline interactions={[node]} />);
      const card = inTimeline.container.querySelector('ol > li') as HTMLElement;
      expect(withoutIds(card.innerHTML), node.id).toBe(standalone);
      inTimeline.unmount();
    }
  });

  it('is the same again once a search is cleared', () => {
    const node = NODES.find((candidate) => candidate.id === promptStep.id)!;
    const { container } = render(<ShadowTimeline interactions={[node, step(2)]} />);
    const cardOf = () => withoutIds((container.querySelector('ol > li') as HTMLElement).innerHTML);
    const before = cardOf();
    typeQuery('retry');
    expect(container.querySelectorAll('mark').length).toBeGreaterThan(0);
    typeQuery('');
    expect(cardOf()).toBe(before);
  });
});

describe('every kind of step', () => {
  const RAW = import.meta.glob<string>('../../test/wire/*/*.json', { eager: true, query: '?raw', import: 'default' });
  const wire = Object.values(RAW).map((raw) => normalizeInteraction(JSON.parse(raw) as CVCBlobData));
  // The wire nodes of the first fixture are in more than one directory under the same id: each is searched once.
  const everything = [...new Map([...NODES_FOR_ALL(), ...wire].map((node) => [node.id, node])).values()];

  function NODES_FOR_ALL(): InteractionNode[] {
    return Object.values(fixtures).filter(
      (value): value is InteractionNode => typeof value === 'object' && value !== null && !Array.isArray(value) && 'artifact_links' in value && 'tool_executions' in value,
    );
  }

  it.each(['e', 'a', 'the', 'tool:read', 'path:src', 'commit:a', '"  "', '.*', '[', '(a+)+$', 'x'.repeat(600)])('is drawn under the query %s without failing, whatever the steps hold', (query) => {
    vi.spyOn(console, 'error').mockImplementation(() => {}); // React logs the error it hands to the boundary of a step that can not be drawn
    const { container } = render(<ShadowTimeline interactions={[...everything, undrawableNode]} />);
    typeQuery(query);
    // Marks hold nothing but text, in every card.
    for (const mark of Array.from(container.querySelectorAll('mark'))) {
      expect(mark.children).toHaveLength(0);
      expect(mark.textContent).not.toBe('');
    }
    // Clearing brings back all of them (a list this long is windowed: the list says how long it is).
    typeQuery('');
    expect(count()).toBe('');
    expect(container.querySelector('ol > li')!.getAttribute('aria-setsize')).toBe(String(everything.length + 1));
  });

  it('finds the real steps by what they hold, and marks it', () => {
    const { container } = render(<ShadowTimeline interactions={wire} />);
    typeQuery('mcp__github__create_issue');
    expect(cardsOf(container).length).toBeGreaterThan(0);
    typeQuery('tool:NotebookEdit');
    expect(cardsOf(container).length).toBeGreaterThan(0);
    expect(markedText(container)).toContain('NotebookEdit');
  });
});
