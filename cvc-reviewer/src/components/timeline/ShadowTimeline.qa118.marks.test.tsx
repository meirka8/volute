/**
 * (Revision round: a match across a hidden character that is drawn as a token is marked on both sides of the token and
 * counted; a term that is in the part of a commit's hash the chip does not draw marks the chip whole; the Action pane's
 * line for a link marks its hash. The tests that say so are marked "round 3".)
 *
 * Independent QA of issue #118: what the search marks on a card. Every surface that draws text from the wire marks it,
 * and every step the search shows leaves a trace of why (a mark, or the count on the fold that hides the match); a term
 * with a prefix marks only its field; what the Reviewer writes itself is left alone; a block holds at most a hundred
 * marks; a hidden character inside a match is found and not marked across.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { MAX_MARKS_PER_BLOCK } from '../../lib/searchMarks';
import { getTimelineIndex, NO_FILTERS, parseQuery, runSearch } from '../../lib/timelineSearch';
import * as fixtures from '../../test/fixtures';
import { searchFixture } from '../../test/searchFixtures';
import { normalizeInteraction, type CVCBlobData, type InteractionNode } from '../../types/cvc';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { generatedLink, makeNode, rawToolCall, wireToolCall } from '../../test/fixtures';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

// Drawing cards in jsdom is slow on a slow core: none of these should fail by the clock.
vi.setConfig({ testTimeout: 30_000 });

const consoleProblems: string[] = [];

beforeEach(() => {
  consoleProblems.length = 0;
  // A warning from React (a key, an update during render, a state update outside act) is a defect of the feature.
  vi.spyOn(console, 'error').mockImplementation((...args: unknown[]) => void consoleProblems.push(args.map(String).join(' ').slice(0, 300)));
  vi.spyOn(console, 'warn').mockImplementation((...args: unknown[]) => void consoleProblems.push(args.map(String).join(' ').slice(0, 300)));
  installDomStubs();
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

const HASH = 'abcdef0123456789abcdef0123456789abcdef01';
const MODEL = 'claude-opus-4-5-20251101';
const ID_SINK = '1f0e5c3a-9d2b-8c4e-a1b7-000000000a01';
const ID_RESULTS = '1f0e5c3a-9d2b-8c4e-a1b7-000000000a02';
const ID_TRIAD = '3b9f0f1e-6a4c-4d6e-9b1a-000000000a03';

/** A step with a token on every surface a tool call or a pane can draw, each token different. */
const SINK = makeNode({
  id: ID_SINK,
  author: 'human',
  timestamp: 3000,
  model_name: MODEL,
  user_prompt: 'TOK_PROMPT first line.',
  model_cot: 'TOK_COT reasoning words',
  model_response: 'TOK_RESP response words',
  tool_executions: [
    wireToolCall('Bash', { command: 'echo TOK_CMD', description: 'TOK_DESC the command', timeout: 120000, run_in_background: true }),
    wireToolCall('Edit', { file_path: '/repo/src/TOK_EDITPATH.ts', old_string: 'old TOK_OLD', new_string: 'new TOK_NEW', replace_all: true }),
    wireToolCall('Write', { file_path: '/repo/docs/TOK_WRITEPATH.md', content: 'TOK_CONTENT line' }),
    wireToolCall('Read', { file_path: '/repo/lib/TOK_READPATH.ts', offset: 3, limit: 10 }),
    wireToolCall('Grep', { pattern: 'TOK_PATTERN', path: '/repo/src', output_mode: 'content', '-i': true, '-n': true, glob: '*.TOK_GLOB' }),
    wireToolCall('TodoWrite', { todos: [{ content: 'TOK_TODO item', status: 'pending', activeForm: 'TOK_ACTIVE doing' }] }),
    wireToolCall('mcp__srv__do_thing', { title: 'TOK_FIELDVAL', note: 'x' }),
    wireToolCall('Task', { description: 'TOK_TASKDESC', prompt: 'TOK_SUBPROMPT long prompt', subagent_type: 'general' }),
    wireToolCall('WebFetch', { url: 'https://example.com/TOK_URL', prompt: 'TOK_FETCHPROMPT' }),
    rawToolCall('Odd', 'TOK_RAWARGS not json'),
  ],
  artifact_links: [generatedLink(ID_SINK, HASH)],
});

/** Reacts to tool results; the group is folded. After the results come a note, and a compaction summary, drawn as output. */
const RESULTS = makeNode({
  id: ID_RESULTS,
  author: 'system',
  timestamp: 2000,
  model_name: MODEL,
  user_prompt: '[tool_result Bash] TOK_RESULT output\n\n[tool_result Read error] TOK_ERRRESULT\n\n[meta] TOK_META note\n\n[compaction summary]\nTOK_COMPACT summary',
  model_response: 'ok',
  artifact_links: [generatedLink(ID_RESULTS, HASH)],
});

/** The Thought / Action / Observation layout. */
const TRIAD = makeNode({
  id: ID_TRIAD,
  author: 'agent',
  timestamp: 1000,
  model_name: 'agent',
  user_prompt: 'TOK_THOUGHT prompt',
  model_cot: 'TOK_WHY reasoning',
  model_response: 'TOK_OBS observation',
  artifact_links: [generatedLink(ID_TRIAD, HASH)],
});

const box = () => screen.getByRole('searchbox', { name: 'Search steps' }) as HTMLInputElement;
const typeQuery = (value: string) => fireEvent.change(box(), { target: { value } });
const marksIn = (root: ParentNode) => Array.from(root.querySelectorAll('mark'));
const markedText = (root: ParentNode) => marksIn(root).map((mark) => mark.textContent);
const FOLD_COUNT = / · \d+\+? match(?:es)?/;
const countLabels = (root: ParentNode) => Array.from(root.querySelectorAll('button')).map((button) => button.textContent ?? '').filter((text) => FOLD_COUNT.test(text));

function search(query: string, nodes = [SINK, RESULTS, TRIAD]) {
  const view = render(<ShadowTimeline interactions={nodes} />);
  typeQuery(query);
  return view;
}

// ---------------------------------------------------------------------------
// Every surface
// ---------------------------------------------------------------------------

describe('a match is marked on every surface that draws wire text', () => {
  it.each([
    ['the prompt', 'TOK_PROMPT'],
    ['the reasoning', 'TOK_COT'],
    ['the response', 'TOK_RESP'],
    ['a command', 'TOK_CMD'],
    ['the description of a command', 'TOK_DESC'],
    ['the file an edit changes', 'TOK_EDITPATH'],
    ['the old text of an edit, in the diff', 'TOK_OLD'],
    ['the new text of an edit, in the diff', 'TOK_NEW'],
    ['the file a write creates', 'TOK_WRITEPATH'],
    ['the content of a write', 'TOK_CONTENT'],
    ['the file a read opens', 'TOK_READPATH'],
    ['the pattern of a search', 'TOK_PATTERN'],
    ['a chip made from a value (the glob)', 'TOK_GLOB'],
    ['an item of a checklist', 'TOK_TODO'],
    ['the doing form of an item of a checklist', 'TOK_ACTIVE'],
    ['the value of a field of a call with no special view', 'TOK_FIELDVAL'],
    ['the description of a task for a subagent', 'TOK_TASKDESC'],
    ['the url a fetch asks for', 'TOK_URL'],
    ['the prompt of a fetch', 'TOK_FETCHPROMPT'],
    ['arguments that are not JSON', 'TOK_RAWARGS'],
    ['the thought, in the other layout', 'TOK_THOUGHT'],
    ['the observation, in the other layout', 'TOK_OBS'],
  ])('%s', (_where, token) => {
    const { container } = search(token);
    expect(markedText(container)).toEqual([token]);
    // The step was found for a reason that is drawn: it holds no fold with a count that is not accounted for.
    expect(countLabels(container)).toEqual([]);
  });

  it('shows a short hash by marking it on the commit chip of each step, and no more of it than the chip draws', () => {
    const { container } = search('abcdef0');
    // One chip on each card; and the Action pane of the other layout names the commit it is linked to.
    expect(markedText(container).filter((text) => text === 'abcdef0').length).toBeGreaterThanOrEqual(3);
    for (const chip of Array.from(container.querySelectorAll('[aria-label*=": commit "]'))) expect(within(chip as HTMLElement).getAllByText('abcdef0', { selector: 'mark' })).toHaveLength(1);
  });

  it('keeps a mark from ever holding anything but its text, whatever surface it is on', () => {
    for (const token of ['TOK_', 'o', 'e', 'a', 'abcdef0', 'tool:o', 'path:/', 'commit:a']) {
      const { container, unmount } = search(token);
      for (const mark of marksIn(container)) {
        expect(mark.children).toHaveLength(0);
        expect(mark.textContent).not.toBe('');
        expect(mark.closest('mark')).toBe(mark);
      }
      unmount();
    }
  });
});

describe('a match in text that is folded away is counted, not opened', () => {
  const FOLDS: Array<[string, string, string]> = [
    ['the prompt a subagent is sent', 'TOK_SUBPROMPT', 'Show subagent prompt'],
    ['a tool result', 'TOK_RESULT', 'Tool results'],
    ['the output of a tool that failed', 'TOK_ERRRESULT', 'Tool results'],
    ['a note that follows tool results', 'TOK_META', 'Tool results'],
    ['a compaction summary that follows tool results', 'TOK_COMPACT', 'Tool results'],
    ['the reasoning behind "Why?"', 'TOK_WHY', 'Why?'],
  ];

  it.each(FOLDS)('%s: the control says "1 match", nothing is marked, and nothing opens by itself', (_where, token, control) => {
    const { container } = search(token);
    expect(marksIn(container)).toHaveLength(0);
    const label = countLabels(container);
    expect(label, 'one control says it').toHaveLength(1);
    expect(label[0]).toContain(control);
    expect(label[0]).toMatch(/ · 1 match$/);
    // The fold is shut, and says so to assistive technology.
    const toggle = screen.getAllByRole('button').find((button) => button.textContent === label[0])!;
    if (control !== 'Why?') expect(toggle).toHaveAttribute('aria-expanded', 'false');
  });

  it.each(FOLDS)('%s: opening the fold marks the match and takes the count away', (_where, token, control) => {
    const { container } = search(token);
    const toggle = screen.getAllByRole('button').find((button) => button.textContent?.startsWith(control) && FOLD_COUNT.test(button.textContent))!;
    fireEvent.click(toggle);
    expect(markedText(container)).toEqual([token]);
    expect(countLabels(container)).toEqual([]);
  });

  it('says "N matches" for several, counts the whole of the text and not only what is in view, and caps the count', () => {
    const lines = Array.from({ length: 40 }, (_, k) => `line ${k + 1} needle`).join('\n');
    const node = makeNode({
      id: ID_SINK,
      author: 'human',
      model_name: MODEL,
      user_prompt: 'run it',
      tool_executions: [wireToolCall('Bash', { command: lines })],
    });
    const { container } = search('needle', [node]);
    // The first twelve lines are in view, with twelve marks; the control counts the whole of the text.
    expect(marksIn(container)).toHaveLength(12);
    expect(countLabels(container)).toEqual(['Show all 40 lines · 40 matches']);
    const huge = makeNode({ id: ID_RESULTS, author: 'human', model_name: MODEL, user_prompt: 'run it', tool_executions: [wireToolCall('Bash', { command: Array.from({ length: 3_000 }, () => 'needle').join('\n') })] });
    cleanup();
    const second = search('needle', [huge]);
    expect(countLabels(second.container)).toEqual(['Show all 3,000 lines · 1000+ matches']);
    fireEvent.click(screen.getByRole('button', { name: /^Show all 3,000 lines/ }));
    // Opened, the block is one block of text and draws no more marks than a block may.
    expect(marksIn(second.container)).toHaveLength(MAX_MARKS_PER_BLOCK);
  });

  it('counts, on the control that draws more, the matches of what is not drawn yet, and a search that begins after the reader opened it counts again', () => {
    const calls = Array.from({ length: 25 }, (_, k) => wireToolCall('Bash', { command: k === 22 ? 'echo needle' : `echo ${k}` }));
    const node = makeNode({ id: ID_SINK, author: 'human', model_name: MODEL, user_prompt: 'run them', tool_executions: calls });
    const { container } = search('needle', [node]);
    expect(marksIn(container)).toHaveLength(0);
    expect(countLabels(container)).toEqual(['Show 10 more tool calls (15 not shown) · 1 match']);
    fireEvent.click(screen.getByRole('button', { name: /^Show 10 more tool calls/ }));
    expect(countLabels(container)).toEqual(['Show 5 more tool calls · 1 match']);
    fireEvent.click(screen.getByRole('button', { name: /^Show 5 more tool calls/ }));
    expect(markedText(container)).toEqual(['needle']);
    expect(countLabels(container)).toEqual([]);
    // The reader paged to the end; a search that goes and comes back finds it as they left it.
    typeQuery('zzz');
    typeQuery('needle');
    expect(markedText(container)).toEqual(['needle']);
    typeQuery('');
    expect(container.querySelectorAll('mark')).toHaveLength(0);
    expect(screen.queryByRole('button', { name: /^Show \d+ more tool calls/ })).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// A term with a prefix marks only its field
// ---------------------------------------------------------------------------

describe('a term with a prefix', () => {
  it('tool: marks the name of a call, and not the same word where it is only text', () => {
    const { container } = search('tool:read');
    expect(markedText(container)).toEqual(['Read']);
    const [mark] = marksIn(container);
    expect(mark.parentElement).toHaveTextContent(/^Read$/);
    // "read" is a plain word in the path TOK_READPATH of that call: the plain term marks it, the tool: term does not.
    cleanup();
    const plain = search('read');
    expect(markedText(plain.container).map((text) => text.toLowerCase())).toContain('read');
    expect(markedText(plain.container).length).toBeGreaterThan(1);
  });

  it('tool: finds a call by the name that was recorded, and marks the name the card shows when that has the term', () => {
    const { container } = search('tool:odd');
    expect(markedText(container)).toEqual(['Odd']);
    cleanup();
    const mcp = search('tool:do_thing');
    expect(markedText(mcp.container)).toEqual(['do_thing']);
  });

  it('path: marks the file of a call and nothing else', () => {
    const { container } = search('path:TOK_EDITPATH');
    expect(markedText(container)).toEqual(['TOK_EDITPATH']);
    const [mark] = marksIn(container);
    expect(mark.parentElement?.className).toContain('font-mono');
    cleanup();
    // The word is in the Edit's path and nowhere else the term may mark: another word of the prompt is not a path.
    const prompt = search('path:TOK_PROMPT');
    expect(screen.getByRole('status')).toHaveTextContent('No matches');
    expect(markedText(prompt.container)).toEqual([]);
  });

  it('commit: marks the hash on the chip of each step linked to it, and nothing else', () => {
    const { container } = search('commit:abcdef0');
    // Round 3: the three chips, and the hash in the line the Action pane writes for the link of the triad step.
    expect(markedText(container)).toEqual(['abcdef0', 'abcdef0', 'abcdef0', 'abcdef0']);
    for (const mark of marksIn(container)) expect(mark.closest('[aria-label*=": commit "], .font-mono')).not.toBeNull();
    expect(marksIn(container).filter((mark) => mark.closest('[aria-label*=": commit "]') !== null)).toHaveLength(3);
  });

  it('a prefix and a plain term together mark each its own surface', () => {
    const { container } = search('tool:read TOK_READPATH');
    expect(markedText(container).sort()).toEqual(['Read', 'TOK_READPATH']);
  });

  it('a term of a prefix with a longer value than the chip draws finds the step and marks the chip whole: the chip draws seven characters', () => {
    // Round 3: a field that is only partly drawn is marked as a whole, so that no match is left without a trace.
    const { container } = search('commit:abcdef0123456789');
    expect(screen.getByRole('status')).toHaveTextContent('1 of 3');
    expect(markedText(container)).toEqual(['abcdef0', 'abcdef0', 'abcdef0', 'abcdef0']);
  });
});

// ---------------------------------------------------------------------------
// What the Reviewer writes is left alone
// ---------------------------------------------------------------------------

describe('what the Reviewer writes itself', () => {
  it('is not marked in the labels of a card, its badges or its controls, though the wire text has the same words', () => {
    // A prompt that says the words the card uses for its own labels and controls.
    const words = ['Prompt', 'Response', 'Reasoning', 'Tool calls', 'success', 'failed', 'error', 'Show more', 'Show all', 'Why?', 'Thought', 'Action', 'Observation', 'AI step', 'Tool results'];
    const node = makeNode({
      id: ID_SINK,
      author: 'human',
      model_name: MODEL,
      user_prompt: words.join(' | '),
      model_cot: words.join(' | '),
      model_response: words.join(' | '),
      tool_executions: [wireToolCall('Bash', { command: 'true' }), wireToolCall('Edit', { file_path: '/x.ts', old_string: 'a', new_string: 'b' }, 'failure')],
    });
    for (const word of words) {
      const { container, unmount } = search(word, [node]);
      for (const mark of marksIn(container)) {
        // A mark is in the text of the prompt, the reasoning or the response, nowhere else.
        expect(mark.closest('p'), `${word}: ${mark.parentElement?.outerHTML.slice(0, 100)}`).not.toBeNull();
        expect(mark.closest('button, [data-status] > div:first-child, .text-action')?.tagName ?? 'none').not.toBe('BUTTON');
      }
      unmount();
    }
  });

  it('is not marked in the pass / fail lines the Observation pane writes for the calls of a step that has no response', () => {
    const node = makeNode({
      id: ID_TRIAD,
      author: 'agent',
      model_name: 'agent',
      user_prompt: 'pass fail Bash agent',
      model_cot: null,
      model_response: null,
      tool_executions: [wireToolCall('Bash', { command: 'true' })],
    });
    const { container } = search('pass', [node]);
    // The prompt says "pass"; the Observation pane writes "pass Bash" for the call, in the Reviewer's words.
    expect(markedText(container)).toEqual(['pass']);
    expect(marksIn(container)[0].closest('p')).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// The cap on marks
// ---------------------------------------------------------------------------

describe('the cap on marks', () => {
  it('a block of text holds at most a hundred marks, and the rest of the text is still drawn', () => {
    const text = Array.from({ length: 500 }, (_, k) => `x${k}`).join(' ');
    const node = makeNode({ id: ID_SINK, author: 'human', model_name: MODEL, user_prompt: 'p', model_response: text });
    const { container } = search('x', [node]);
    expect(marksIn(container)).toHaveLength(MAX_MARKS_PER_BLOCK);
    // Nothing is lost: with the marks taken away the text is the text.
    const response = Array.from(container.querySelectorAll('p')).find((p) => p.textContent?.startsWith('x0 x1'))!;
    expect(response.textContent).toBe(text);
  });

  it('a hundred a block, not a hundred a card: the prompt and the response each have their own', () => {
    const text = Array.from({ length: 300 }, () => 'x').join(' ');
    const node = makeNode({ id: ID_SINK, author: 'human', model_name: MODEL, user_prompt: text, model_response: text });
    const { container } = search('x', [node]);
    expect(marksIn(container)).toHaveLength(2 * MAX_MARKS_PER_BLOCK);
  });

  it('matches that touch or overlap are one stretch, and one mark', () => {
    const node = makeNode({ id: ID_SINK, author: 'human', model_name: MODEL, user_prompt: 'p', model_response: 'aaaaaa and abab and abXab' });
    const { container } = search('aa', [node]);
    expect(markedText(container)).toEqual(['aaaaaa']);
    // Overlapping: "ab" and "ba" in "abab".
    typeQuery('ab ba');
    expect(markedText(container)).toEqual(['abab', 'ab', 'ab']);
    // Touching, with nothing between: "ab" twice in "abab" is one stretch; with a letter between, two.
    typeQuery('ab');
    expect(markedText(container)).toEqual(['abab', 'ab', 'ab']);
  });

  it('draws the marks of a term of one letter over a very long block without stalling', () => {
    const node = makeNode({ id: ID_SINK, author: 'human', model_name: MODEL, user_prompt: 'p', model_response: 'e '.repeat(500_000) });
    const started = performance.now();
    const { container } = search('e', [node]);
    expect(performance.now() - started).toBeLessThan(5_000);
    expect(marksIn(container).length).toBeLessThanOrEqual(2 * MAX_MARKS_PER_BLOCK);
  });
});

// ---------------------------------------------------------------------------
// Hidden characters
// ---------------------------------------------------------------------------

describe('a hidden character inside a match', () => {
  const ZWSP = '\u200b';

  it('is found, and is marked on both sides of the token: each half is text of its own on either side of it', () => {
    const node = makeNode({
      id: ID_SINK,
      author: 'human',
      model_name: MODEL,
      user_prompt: 'set the password',
      tool_executions: [wireToolCall('Bash', { command: `echo pass${ZWSP}word` })],
    });
    // Found by what a person would type...
    const { container } = search('password', [node]);
    expect(screen.getByRole('status')).toHaveTextContent('1 of 1');
    // ...marked in the prompt, where it is whole, and on either side of the token in the command (round 3: it was not marked across).
    expect(markedText(container)).toEqual(['password', 'pass', 'word']);
    // The token is drawn between the halves, with the note that says what it is.
    expect(container.querySelector('[data-hidden-char="U+200B"]')).not.toBeNull();
    expect(screen.getByRole('note')).toHaveTextContent(/hidden or bidirectional/);
    for (const mark of marksIn(container)) expect(mark.children).toHaveLength(0);
    // Each half is a match of its own term.
    typeQuery('pass');
    expect(markedText(container)).toContain('pass');
    typeQuery('word');
    expect(markedText(container)).toContain('word');
  });

  it('is found when it is the person who typed it, and when it is in a command only', () => {
    const node = makeNode({ id: ID_SINK, author: 'human', model_name: MODEL, user_prompt: 'run it', tool_executions: [wireToolCall('Bash', { command: `rm -rf${'\u202e'} ./build` })] });
    const { container } = search('"rm -rf ./build"', [node]);
    expect(screen.getByRole('status')).toHaveTextContent('1 of 1');
    // Round 3: the phrase is marked on both sides of the override that is drawn between its halves (it was not marked across).
    expect(markedText(container)).toEqual(['rm -rf', ' ./build']);
    // As three words they are three terms, each of them whole in the text, and each is marked.
    typeQuery('rm -rf ./build');
    expect(markedText(container)).toEqual(['rm', '-rf', './build']);
    expect(screen.getByRole('note')).toBeInTheDocument();
  });

  it('is counted on the fold, when the match is hidden behind one and the text holds the character', () => {
    const lines = Array.from({ length: 30 }, (_, k) => (k === 20 ? `pass${ZWSP}word` : `line ${k}`)).join('\n');
    const node = makeNode({ id: ID_SINK, author: 'human', model_name: MODEL, user_prompt: 'run it', tool_executions: [wireToolCall('Bash', { command: lines })] });
    const { container } = search('password', [node]);
    expect(screen.getByRole('status')).toHaveTextContent('1 of 1');
    // Round 3: the count is of the matches that can be drawn, and the match across the token is one of them (it is drawn as two marks).
    expect(countLabels(container)).toEqual(['Show all 30 lines · 1 match']);
    fireEvent.click(screen.getByRole('button', { name: /^Show all 30 lines/ }));
    expect(markedText(container)).toEqual(['pass', 'word']);
  });

  it('a query made of nothing but hidden characters is not a query, and marks nothing', () => {
    const { container } = search(`${ZWSP}${ZWSP}\u202e`);
    expect(screen.getByRole('status')).toHaveTextContent('');
    expect(marksIn(container)).toHaveLength(0);
    expect(container.querySelectorAll('ol > li')).toHaveLength(3);
  });
});

// ---------------------------------------------------------------------------
// The other layout
// ---------------------------------------------------------------------------

describe('the Thought / Action / Observation layout', () => {
  it('marks the tool name and the string values of the arguments in the Action pane, and counts "Show more" on a clamped thought', () => {
    const node = makeNode({
      id: ID_TRIAD,
      author: 'agent',
      model_name: 'agent',
      user_prompt: 'Refactor the retry helper',
      model_response: 'Done',
      tool_executions: [wireToolCall('Edit', { file_path: '/repo/src/retry.ts', old_string: 'a', new_string: 'b' })],
    });
    const { container } = search('retry', [node]);
    expect(markedText(container).filter((text) => text === 'retry').length).toBe(2); // the thought, and the argument text of the Action pane
    // A plain term for the name of the call marks it in the line of the Action pane that begins with it.
    typeQuery('edit');
    expect(markedText(container).map((text) => text.toLowerCase())).toContain('edit');
  });
});

// ---------------------------------------------------------------------------
// Every step that is shown says why
// ---------------------------------------------------------------------------

describe('a step the search shows says why', () => {
  const RAW = import.meta.glob<string>('../../test/wire/*/*.json', { eager: true, query: '?raw', import: 'default' });
  const wire = Object.values(RAW).map((raw) => normalizeInteraction(JSON.parse(raw) as CVCBlobData));
  const named = Object.values(fixtures).filter((value): value is InteractionNode => typeof value === 'object' && value !== null && !Array.isArray(value) && 'artifact_links' in value && 'tool_executions' in value);
  const nodes = [...new Map([...named, ...wire, ...searchFixture(6).nodes].map((node) => [node.id, node])).values()];

  /**
   * Round 3: the index holds what a card draws and nothing else, so there is nothing to allow for. (Before it, three kinds of
   * word showed a step and marked nothing: those only in the markers of the wire, `null`, the arguments of a call made with no
   * input, and the reasoning of a step the other layout draws without a "Why?". Each was allowed here; none is any more, and a
   * word that shows a step and marks nothing is a surface that is not marked, or text that is searched and never drawn.)
   */

  it('by a mark, or by the count on the fold that holds the match, for every word of every step in the fixtures', () => {
    const wordsOf = (node: InteractionNode): string[] => {
      const text = [node.user_prompt, node.model_cot, node.model_response, ...node.tool_executions.map((call) => call.arguments), ...node.tool_executions.map((call) => call.tool_name)].filter(Boolean).join(' ');
      const words = new Set<string>();
      for (const word of text.split(/[^A-Za-z0-9_./-]+/)) if (word.length >= 3 && word.length <= 24) words.add(word.toLowerCase());
      return [...words].slice(0, 24);
    };
    let checked = 0;
    const unexplained: string[] = [];
    for (const node of nodes) {
      const { container, unmount } = render(<ShadowTimeline interactions={[node]} />);
      const index = getTimelineIndex([node]);
      for (const word of wordsOf(node)) {
        if (runSearch(index, { query: parseQuery(word), filters: NO_FILTERS, file: null }).ids.length === 0) continue;
        typeQuery(word);
        checked++;
        // (A card that says where it matched outside what it draws, "Matched in: ...", has said why.)
        if (marksIn(container).length > 0 || countLabels(container).length > 0 || container.querySelector('[role="note"]')?.textContent?.startsWith('Matched in:')) continue;
        unexplained.push(`${node.id.slice(-8)}: ${word}`);
      }
      unmount();
    }
    expect(checked).toBeGreaterThan(400);
    expect(unexplained.join('\n')).toBe('');
  });
});

// ---------------------------------------------------------------------------
// The same word, typed another way
// ---------------------------------------------------------------------------

describe('a word typed another way finds the same steps and marks them', () => {
  // Round 3: one fold serves the index, the terms and the marks, so a word in capitals, with a hidden character in the
  // middle, or in full-width letters is the word. Checked on the words of the steps of the fixtures: what the plain word
  // finds, each variant finds, and a step it shows says why (a mark or the count on a fold) just as it does for the plain word.
  const RAW = import.meta.glob<string>('../../test/wire/*/*.json', { eager: true, query: '?raw', import: 'default' });
  const wire = Object.values(RAW).map((raw) => normalizeInteraction(JSON.parse(raw) as CVCBlobData));
  const named = Object.values(fixtures).filter((value): value is InteractionNode => typeof value === 'object' && value !== null && !Array.isArray(value) && 'artifact_links' in value && 'tool_executions' in value);
  const nodes = [...new Map([...named, ...wire].map((node) => [node.id, node])).values()].slice(0, 70);

  const fullWidth = (word: string) => Array.from(word, (character) => (character >= '!' && character <= '~' ? String.fromCharCode(character.charCodeAt(0) + 0xfee0) : character)).join('');
  const withHidden = (word: string) => `${word.slice(0, Math.ceil(word.length / 2))}\u200b${word.slice(Math.ceil(word.length / 2))}`;
  const VARIANTS: Array<[string, (word: string) => string]> = [
    ['in capitals', (word) => word.toUpperCase()],
    ['with a zero-width space in the middle', withHidden],
    ['in full-width letters', fullWidth],
  ];

  it('for every variant of the words of every step', () => {
    const wordsOf = (node: InteractionNode): string[] => {
      const text = [node.user_prompt, node.model_response, ...node.tool_executions.map((call) => call.tool_name)].filter(Boolean).join(' ');
      const words = new Set<string>();
      for (const word of text.split(/[^A-Za-z0-9_./-]+/)) if (word.length >= 4 && word.length <= 20 && /[a-z]/.test(word)) words.add(word.toLowerCase());
      return [...words].slice(0, 5);
    };
    let checked = 0;
    const problems: string[] = [];
    for (const node of nodes) {
      const { container, unmount } = render(<ShadowTimeline interactions={[node]} />);
      const index = getTimelineIndex([node]);
      for (const word of wordsOf(node)) {
        const found = (query: string) => runSearch(index, { query: parseQuery(query), filters: NO_FILTERS, file: null }).ids.length > 0;
        if (!found(word)) continue;
        for (const [label, variant] of VARIANTS) {
          const typed = variant(word);
          checked++;
          if (!found(typed)) {
            problems.push(`${node.id.slice(-8)}: ${label}: ${JSON.stringify(typed)} is not found`);
            continue;
          }
          typeQuery(typed);
          const explained = marksIn(container).length > 0 || countLabels(container).length > 0 || container.querySelector('[role="note"]')?.textContent?.startsWith('Matched in:') === true;
          if (!explained) problems.push(`${node.id.slice(-8)}: ${label}: ${JSON.stringify(typed)} shows the step and says nothing`);
        }
      }
      unmount();
    }
    expect(checked).toBeGreaterThan(150);
    expect(problems.join('\n')).toBe('');
  });
});
