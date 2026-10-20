/**
 * The transcript layout under hostile and unusual input: arguments built to
 * break a renderer, text that only looks like a marker, sheer volume, nodes
 * from before blobs were validated, and several hundred random nodes.
 *
 * Every test also fails on a React warning or error logged to the console.
 */
import '@testing-library/jest-dom/vitest';
import { StrictMode } from 'react';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TimelineNode } from './TimelineNode';
import { displayText } from '../../lib/textLines';
import { diffRows } from '../../lib/lineDiff';
import { failedToolCount, nodeLayout, parseStimulus } from '../../lib/transcriptNode';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import * as fixtures from '../../test/fixtures';
import { withoutIds } from '../../test/markup';
import { makeNode, rawToolCall, toolCall } from '../../test/fixtures';
import type { InteractionNode, ToolExecution } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

let consoleProblems: string[] = [];
beforeEach(() => {
  installDomStubs();
  consoleProblems = [];
  const record = (...args: unknown[]) => {
    consoleProblems.push(args.map(String).join(' ').slice(0, 300));
  };
  vi.spyOn(console, 'error').mockImplementation(record);
  vi.spyOn(console, 'warn').mockImplementation(record);
});
afterEach(() => {
  cleanup();
  const problems = consoleProblems;
  restoreDomStubs();
  expect(problems, 'React or the components logged a warning or an error').toEqual([]);
});

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** A version 8 id with the RFC variant bits, as the harness derives them. */
const HARNESS_ID = '1f0e5c3a-9d2b-8c4e-a1b7-0000000000aa';

function step(over: Partial<InteractionNode> = {}): InteractionNode {
  return makeNode({
    id: HARNESS_ID,
    author: 'system',
    model_name: 'claude-test-1',
    user_prompt: '[tool_result Bash] ok',
    model_response: 'Done.',
    ...over,
  });
}

function renderStep(over: Partial<InteractionNode> = {}, onSelect: () => void = () => {}) {
  return render(<TimelineNode interaction={step(over)} onSelect={onSelect} />);
}

const cardOf = (container: HTMLElement): HTMLElement => container.firstElementChild as HTMLElement;
const toolCardsOf = (card: HTMLElement): HTMLElement[] => Array.from(card.querySelectorAll<HTMLElement>('[data-status]'));

const nested = (depth: number): string => '['.repeat(depth) + ']'.repeat(depth);

/** Clicks every closed toggle until none is left. */
function expandAll(card: HTMLElement) {
  for (let pass = 0; pass < 12; pass++) {
    const closed = Array.from(card.querySelectorAll('button[aria-expanded="false"]'));
    if (closed.length === 0) return;
    for (const button of closed) fireEvent.click(button);
  }
}

/**
 * Clicks "Show N more ..." until the list has drawn everything. Each click draws
 * one page, so this takes a few clicks for a few hundred items.
 */
function showEverything(root: HTMLElement, noun: RegExp = /./) {
  for (let click = 0; click < 200; click++) {
    const more = within(root)
      .queryAllByRole('button', { name: /^Show [\d,]+ more / })
      .find((button) => noun.test(button.textContent ?? ''));
    if (!more) return;
    fireEvent.click(more);
  }
  throw new Error('"Show more" kept offering more');
}

/** Deterministic xorshift, so a failure is reproducible. */
function prng(seed: number): () => number {
  let state = seed;
  return () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x100000000;
  };
}

// ---------------------------------------------------------------------------
// Hostile arguments
// ---------------------------------------------------------------------------

describe('arguments nested absurdly deep', () => {
  it('shows a value nested 2000 levels deep, without overflowing the stack', () => {
    const { container } = renderStep({ tool_executions: [rawToolCall('mcp__srv__tool', `{"deep":${nested(2000)}}`)] });
    const [card] = toolCardsOf(cardOf(container));
    expect(within(card).getByText('deep')).toBeInTheDocument();
    const shown = card.querySelector('pre')!.textContent!;
    expect(shown.startsWith('[')).toBe(true);
    // Whatever the printing, only a screenful of it is on the page; the rest waits behind a toggle or is cut.
    expect(shown.length).toBeLessThan(100_000);
  });

  it('shows an unusable value as a note and still draws the card, however deep it goes', () => {
    const { container } = renderStep({ tool_executions: [rawToolCall('mcp__srv__tool', `{"deep":${nested(200_000)}}`)] });
    const cards = toolCardsOf(cardOf(container));
    expect(cards).toHaveLength(1);
    expect(within(cards[0]).getByText('srv · tool')).toBeInTheDocument();
    // Changed with the bounded pretty-printing: the bracketed placeholder `[unrenderable value]` is
    // gone (brackets look like recorded content). An engine that cannot print 200,000 levels gets
    // the Reviewer's note; one that can prints it compactly, clipped, and says how much was left out.
    const text = cards[0].textContent!;
    expect(/\(too deeply nested to display\)|more characters not shown/.test(text)).toBe(true);
    expect(text).not.toContain('[unrenderable value]');
  });

  it('survives nesting in a field a tool is understood by', () => {
    const { container } = renderStep({
      tool_executions: [
        rawToolCall('Bash', `{"command":"ls","note":${nested(3000)}}`),
        rawToolCall('TodoWrite', `{"todos":${nested(3000)}}`),
        rawToolCall('MultiEdit', `{"file_path":"/repo/a","edits":${nested(3000)}}`),
        rawToolCall('Edit', `{"file_path":"/repo/a","old_string":${nested(3000)},"new_string":"x"}`),
      ],
    });
    expect(toolCardsOf(cardOf(container))).toHaveLength(4);
  });
});

describe('keys that look like Object.prototype members', () => {
  it('shows them as ordinary keys and pollutes nothing', () => {
    const before = Object.getOwnPropertyNames(Object.prototype).sort();
    const { container } = renderStep({
      user_prompt: '[tool_result __proto__] {"__proto__":{"polluted":"stimulus"}}',
      model_response: '__proto__ constructor',
      tool_executions: [
        rawToolCall('Bash', '{"command":"ls","__proto__":{"polluted":"bash"}}'),
        rawToolCall(
          'mcp__srv__tool',
          '{"__proto__":{"polluted":"mcp"},"constructor":{"prototype":{"polluted":"constructor"}},"toString":"s","hasOwnProperty":1,"valueOf":null}',
        ),
        rawToolCall('MultiEdit', '{"file_path":"/r/a","edits":[{"old_string":"a","new_string":"b","__proto__":{"polluted":"edit"}}]}'),
        rawToolCall('TodoWrite', '{"todos":[{"content":"c","status":"s","__proto__":{"polluted":"todo"}}]}'),
        rawToolCall('__proto__', '{"constructor":"x"}'),
        rawToolCall('constructor', '{"__proto__":"x"}'),
      ],
    });
    const card = cardOf(container);

    expect(({} as Record<string, unknown>).polluted).toBeUndefined();
    expect(Object.getOwnPropertyNames(Object.prototype).sort()).toEqual(before);
    expect(Object.getPrototypeOf({})).toBe(Object.prototype);

    // Nothing was dropped: the keys are on the page as rows.
    const mcp = toolCardsOf(card)[1];
    for (const key of ['__proto__', 'constructor', 'toString', 'hasOwnProperty', 'valueOf']) {
      expect(within(mcp).getByText(key), key).toBeInTheDocument();
    }
    expect(within(toolCardsOf(card)[0]).getByText('__proto__')).toBeInTheDocument(); // an extra on a known tool
    expect(screen.getByRole('button', { name: 'Tool results · __proto__' })).toBeInTheDocument();
  });
});

describe('volume', () => {
  it('draws 256 tool calls in one node (the most the reviewer accepts), 10 at a time and in order', () => {
    const started = performance.now();
    const tools: ToolExecution[] = Array.from({ length: 256 }, (_, i) =>
      toolCall(i % 2 === 0 ? 'Bash' : 'mcp__srv__tool', i % 2 === 0 ? { command: `echo ${i}` } : { index: i }, i % 7 === 0 ? 'failure' : 'success'),
    );
    const { container } = renderStep({ tool_executions: tools });
    const card = cardOf(container);
    // Changed with the render caps: one page of 10 cards, the rest behind "Show N more tool calls".
    expect(toolCardsOf(card)).toHaveLength(10);
    expect(within(card).getByRole('button', { name: 'Show 10 more tool calls (246 not shown)' })).toBeInTheDocument();
    showEverything(card, /tool calls?/);
    expect(toolCardsOf(card)).toHaveLength(256);
    expect(within(card).getByText('Tool calls')).toHaveTextContent('Tool calls256');
    expect(within(card).getByText(`${failedToolCount(step({ tool_executions: tools }))} tool calls failed`)).toBeInTheDocument();
    expect(toolCardsOf(card)[255].textContent).toContain('srv');
    expect(performance.now() - started).toBeLessThan(15_000); // generous: this takes about a second
  });

  it('draws 200 Edit calls that each carry a diff', () => {
    const tools = Array.from({ length: 200 }, (_, i) => toolCall('Edit', { file_path: `/repo/f${i}.ts`, old_string: `a${i}`, new_string: `b${i}` }));
    const { container } = renderStep({ tool_executions: tools });
    const card = cardOf(container);
    expect(toolCardsOf(card)).toHaveLength(10); // one page; see the render caps
    showEverything(card, /tool calls?/);
    expect(toolCardsOf(card)).toHaveLength(200);
    expect(card.querySelectorAll('del')).toHaveLength(200);
    expect(card.querySelectorAll('ins')).toHaveLength(200);
  });

  it('summarises a 64 KiB run of tool results behind one toggle on a system step', () => {
    const prompt = Array.from({ length: 4000 }, () => '[tool_result a] x').join('\n\n').slice(0, 64 * 1024);
    const results = parseStimulus(prompt).length;
    expect(results).toBeGreaterThan(3000);
    const { container } = renderStep({ user_prompt: prompt, tool_executions: [] });
    const toggle = within(cardOf(container)).getByRole('button', { name: `Tool results · a ×${results}` });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(container.querySelectorAll('pre')).toHaveLength(0);
  });

  it('draws every result of a modest run when a human step opens them, 20 at a time', () => {
    const prompt = Array.from({ length: 60 }, (_, i) => `[tool_result T${i % 5}] out${i}`).join('\n\n');
    const { container } = renderStep({ author: 'human', user_prompt: prompt, tool_executions: [] });
    expect(within(cardOf(container)).getByRole('button', { name: /^Tool results · T0 ×12, T1 ×12, T2 ×12, T3 ×12, \+1 more$/ })).toHaveAttribute(
      'aria-expanded',
      'true',
    );
    // Changed with the render caps: one page of 20 results, the other 40 behind "Show N more tool results".
    expect(container.querySelectorAll('pre')).toHaveLength(20);
    showEverything(cardOf(container), /tool results?/);
    expect(container.querySelectorAll('pre')).toHaveLength(60);
  });

  it('draws a run of 20 results, the size of one page, all at once', () => {
    const prompt = Array.from({ length: 20 }, (_, i) => `[tool_result T${i}] out${i}`).join('\n\n');
    const { container } = renderStep({ author: 'human', user_prompt: prompt, tool_executions: [] });
    expect(container.querySelectorAll('pre')).toHaveLength(20);
    expect(within(cardOf(container)).queryByRole('button', { name: /^Show [\d,]+ more / })).not.toBeInTheDocument();
  });

  /**
   * Was QA-4: tool results start OPEN on a human-authored step (so that human text that
   * follows them is never hidden), and every result was drawn at once. The harness caps a
   * prompt at 64 KiB, but the reviewer accepts strings up to 1 MiB; a 1 MiB prompt of
   * `[tool_result a] x` parts is about 58,000 results, which took 7 s and 1.4 GB to render.
   * Now a group draws 20 results and offers the rest a page at a time.
   */
  it('draws a bounded number of results at once, however many the prompt holds', () => {
    const prompt = Array.from({ length: 5000 }, (_, i) => `[tool_result T] out${i}`).join('\n\n');
    const { container } = renderStep({ author: 'human', user_prompt: prompt, tool_executions: [] });
    const drawn = container.querySelectorAll('pre').length;
    expect(drawn, `${drawn} of 5000 results were drawn at once`).toBeLessThanOrEqual(1000);
    expect(drawn).toBe(20);
    expect(within(cardOf(container)).getByRole('button', { name: 'Show 20 more tool results (4,980 not shown)' })).toBeInTheDocument();
  });

  it('keeps a megabyte of results cheap to draw', () => {
    const prompt = '[tool_result a] x\n\n'.repeat(60_000).slice(0, 1024 * 1024);
    const started = performance.now();
    const { container } = renderStep({ author: 'human', user_prompt: prompt, tool_executions: [] });
    expect(container.querySelectorAll('pre').length).toBeLessThanOrEqual(20);
    expect(container.querySelectorAll('*').length).toBeLessThan(1000);
    expect(performance.now() - started).toBeLessThan(3000);
  });
});

describe('arguments of the wrong type', () => {
  it.each([
    ['a number', 123, '123'],
    ['null', null, 'null'],
    ['a boolean', true, 'true'],
    ['an object', { a: 1 }, null],
    ['an array', ['ls', '-la'], null],
  ])('shows a Bash command that is %s as a row', (_label, command, text) => {
    const { container } = renderStep({ tool_executions: [toolCall('Bash', { command })] });
    const [card] = toolCardsOf(cardOf(container));
    expect(within(card).getByText('command')).toBeInTheDocument();
    expect(card.textContent).toContain(text ?? JSON.stringify(command, null, 2).split('\n')[0]);
    expect(within(card).queryByText('Empty command.')).not.toBeInTheDocument();
  });

  it('shows an empty Bash command as such', () => {
    const { container } = renderStep({ tool_executions: [toolCall('Bash', { command: '' })] });
    expect(within(toolCardsOf(cardOf(container))[0]).getByText('Empty command.')).toBeInTheDocument();
  });

  it.each([
    ['a string', 'oops'],
    ['a number', 5],
    ['null', null],
    ['an object', { old_string: 'a', new_string: 'b' }],
    ['an empty array', []],
    ['an array of strings', ['a', 'b']],
  ])('shows MultiEdit edits that are %s as rows, not as a diff', (_label, edits) => {
    const { container } = renderStep({ tool_executions: [toolCall('MultiEdit', { file_path: '/repo/a.ts', edits })] });
    const [card] = toolCardsOf(cardOf(container));
    expect(within(card).getByText('edits')).toBeInTheDocument();
    expect(within(card).getByText('file_path')).toBeInTheDocument();
    expect(card.querySelector('del, ins')).toBeNull();
  });

  it('shows arguments that are an array, a number or a string as they were recorded', () => {
    const { container } = renderStep({
      tool_executions: [rawToolCall('Bash', '["ls","-la"]'), rawToolCall('Bash', '42'), rawToolCall('Bash', '"just text"'), rawToolCall('Bash', 'true')],
    });
    const cards = toolCardsOf(cardOf(container));
    expect(cards.map((card) => card.querySelector('pre')!.textContent)).toEqual(['["ls","-la"]', '42', '"just text"', 'true']);
  });
});

// ---------------------------------------------------------------------------
// Edit diffs
// ---------------------------------------------------------------------------

describe('Edit diffs of unusual text', () => {
  const edit = (input: Record<string, unknown>) => renderStep({ tool_executions: [toolCall('Edit', { file_path: '/repo/a.ts', ...input })] });

  it('says so when the old and new text are identical, and still shows the text once', () => {
    const { container } = edit({ old_string: 'same\ntext', new_string: 'same\ntext' });
    const [card] = toolCardsOf(cardOf(container));
    // Changed with the diff notes: judged on the RAW text, and worded as a fact about the two texts.
    expect(within(card).getByText('Old and new text are identical.')).toBeInTheDocument();
    expect(card.querySelectorAll('ins, del')).toHaveLength(0);
    // Changed with the unchanged-text path: the text is one block of text, not a diff row per line.
    const blocks = card.querySelectorAll('pre');
    expect(blocks).toHaveLength(1);
    expect(blocks[0].textContent).toBe('same\ntext');
    expect(within(card).getByRole('region', { name: 'Text' })).toBe(blocks[0]);
  });

  it('shows an empty old_string as pure insertion and an empty new_string as pure deletion', () => {
    const { container } = edit({ old_string: '', new_string: 'brand\nnew' });
    const [card] = toolCardsOf(cardOf(container));
    expect(Array.from(card.querySelectorAll('ins')).map((el) => el.textContent)).toEqual(['+brand', '+new']);
    expect(card.querySelectorAll('del')).toHaveLength(0);
    cleanup();

    const removal = edit({ old_string: 'gone\ntoo', new_string: '' });
    const [removalCard] = toolCardsOf(cardOf(removal.container));
    expect(Array.from(removalCard.querySelectorAll('del')).map((el) => el.textContent)).toEqual(['-gone', '-too']);
    expect(removalCard.querySelectorAll('ins')).toHaveLength(0);
  });

  it('says that nothing was recorded when both sides are empty', () => {
    const { container } = edit({ old_string: '', new_string: '' });
    expect(within(toolCardsOf(cardOf(container))[0]).getByText('Old and new text are both empty.')).toBeInTheDocument();
  });

  it('draws a diff whose only content is whitespace', () => {
    const { container } = edit({ old_string: '  ', new_string: '\t' });
    const [card] = toolCardsOf(cardOf(container));
    expect(card.querySelectorAll('del')).toHaveLength(1);
    expect(card.querySelectorAll('ins')).toHaveLength(1);
  });

  it('draws a diff between texts that differ only in line endings, without a carriage return', () => {
    const { container } = edit({ old_string: 'a\r\nb\r\nc', new_string: 'a\nb\nc' });
    expect(cardOf(container).textContent).not.toContain('\r');
  });

  /**
   * Was QA-2: `TextDiff` folds CRLF/CR to LF before diffing, so an Edit whose old_string and
   * new_string differ only in line endings (a conversion from Windows to Unix endings) was
   * shown as "No difference between the old and new text." That is false: the call changed
   * the file. The note is now decided on the RAW texts.
   */
  it('does not claim that there is no difference when only the line endings differ', () => {
    const { container } = edit({ old_string: 'a\r\nb\r\nc', new_string: 'a\nb\nc' });
    const [card] = toolCardsOf(cardOf(container));
    expect('a\r\nb\r\nc').not.toBe('a\nb\nc');
    expect(within(card).queryByText('No difference between the old and new text.')).not.toBeInTheDocument();
    expect(within(card).queryByText('Old and new text are identical.')).not.toBeInTheDocument();
    // Changed with the line-ending note: "(CRLF vs LF)" was also shown for a pair like CR and LF.
    expect(within(card).getByText('Only the line endings differ.')).toBeInTheDocument();
    // The text is still shown, once, as one block, with no change marked.
    expect(card.querySelectorAll('ins, del')).toHaveLength(0);
    expect(Array.from(card.querySelectorAll('pre')).map((pre) => pre.textContent)).toEqual(['a\nb\nc']);
  });

  // Changed with the line-ending note: it used to say "(CRLF vs LF)" whatever the pair was, which is
  // false for CR against LF or CR against CRLF. It now says only what is true of every pair.
  it('says that only the line endings differ, truthfully, for every pair of endings in either direction', () => {
    for (const [a, b] of [
      ['a\nb', 'a\r\nb'],
      ['a\rb', 'a\nb'],
      ['a\r\nb', 'a\rb'],
      ['a\r\n', 'a\n'],
    ]) {
      const { container, unmount } = edit({ old_string: a, new_string: b });
      const card = toolCardsOf(cardOf(container))[0];
      expect(within(card).getByText('Only the line endings differ.'), JSON.stringify([a, b])).toBeInTheDocument();
      expect(card.textContent).not.toMatch(/CRLF|\bLF\b|\bCR\b/);
      unmount();
    }
  });

  it('says nothing about line endings when there is a real change as well', () => {
    const { container } = edit({ old_string: 'a\r\nb', new_string: 'a\nB' });
    expect(within(toolCardsOf(cardOf(container))[0]).queryByText(/line endings/)).not.toBeInTheDocument();
  });

  it('draws a Windows-ended diff that has a real change as that change only', () => {
    const { container } = edit({ old_string: 'a\r\nb\r\nc', new_string: 'a\r\nB\r\nc' });
    const [card] = toolCardsOf(cardOf(container));
    expect(Array.from(card.querySelectorAll('del')).map((el) => el.textContent)).toEqual(['-b']);
    expect(Array.from(card.querySelectorAll('ins')).map((el) => el.textContent)).toEqual(['+B']);
  });

  // Changed with the hidden-character tokens (#135): the right-to-left override and the NUL in this text are
  // drawn as tokens (⟨U+202E⟩, ⟨U+0000⟩) instead of being put on the page raw; the markup is still only text.
  it('keeps markup-like text in the diff as text, and draws the control characters in it as tokens', () => {
    const old = '<img src=x onerror=alert(1)>\u202E\u0000';
    const { container } = edit({ old_string: old, new_string: '&lt;script&gt;' });
    const [card] = toolCardsOf(cardOf(container));
    expect(card.querySelector('img, script')).toBeNull();
    expect(card.querySelector('del')!.textContent).toBe('-<img src=x onerror=alert(1)>⟨U+202E⟩⟨U+0000⟩');
    expect(card.querySelector('ins')!.textContent).toBe('+&lt;script&gt;');
    // Neither character is in the text of the page any more.
    expect(card.textContent).not.toContain(String.fromCharCode(0x0000));
    expect(card.textContent).not.toContain(String.fromCharCode(0x202e));
  });
});

describe('Edit diffs with folded runs of unchanged lines', () => {
  it('counts the lines it would show in "Show all N lines", not the fold markers', () => {
    const numbered = (changed: number[]) =>
      Array.from({ length: 90 }, (_, i) => (changed.includes(i) ? `changed ${i}` : `line ${i}`)).join('\n');
    // Three clusters of changes with long unchanged runs between them.
    const edits = [3, 4, 5, 6, 7, 8, 40, 41, 42, 43, 44, 45, 80, 81, 82, 83, 84, 85];
    const oldText = numbered([]);
    const newText = numbered(edits);
    const rows = diffRows(oldText, newText);
    const folds = rows.filter((row) => row.kind === 'gap').length;
    const shown = rows.length - folds;
    expect(folds).toBeGreaterThanOrEqual(2);
    expect(shown).toBeGreaterThan(14);

    const { container } = renderStep({ tool_executions: [toolCall('Edit', { file_path: '/repo/a.ts', old_string: oldText, new_string: newText })] });
    const [card] = toolCardsOf(cardOf(container));
    expect(within(card).getByRole('button', { name: `Show all ${shown} lines` })).toBeInTheDocument();
    fireEvent.click(within(card).getByRole('button', { name: `Show all ${shown} lines` }));
    // The "⋯" glyph is decoration (aria-hidden); the words a screen reader gets are "N unchanged lines".
    expect(within(card).getAllByText(/^\d+ unchanged lines?$/)).toHaveLength(folds);
    for (const marker of card.querySelectorAll('[aria-hidden="true"]')) {
      if (marker.textContent === '⋯') expect(marker.parentElement!.textContent).toMatch(/^⋯ \d+ unchanged lines?$/);
    }
    expect(card.querySelectorAll('del')).toHaveLength(edits.length);
    expect(card.querySelectorAll('ins')).toHaveLength(edits.length);
  });
});

describe('Edit diffs between texts with nothing in common', () => {
  const oneCharLines = (count: number, alphabet: string) =>
    Array.from({ length: count }, (_, i) => alphabet[i % alphabet.length]).join('\n');

  it('draws a ~16 KiB edit promptly: too large to diff, so both texts are shown, each collapsed', () => {
    const oldText = oneCharLines(8192, 'abcdefghijklmnopqrstuvwxyz');
    const newText = oneCharLines(8192, 'ABCDEFGHIJKLMNOPQRSTUVWXYZ');
    const started = performance.now();
    const { container } = renderStep({ tool_executions: [toolCall('Edit', { file_path: '/repo/a.ts', old_string: oldText, new_string: newText })] });
    const elapsed = performance.now() - started;
    const [card] = toolCardsOf(cardOf(container));

    expect(elapsed).toBeLessThan(5_000); // a few milliseconds; the bound only catches runaway work
    // Changed with the diff budget: a changed middle of 16,384 lines is past the 5,000-line limit,
    // so nothing is diffed. (It used to list everything removed, then everything added.)
    expect(card.querySelectorAll('del, ins')).toHaveLength(0);
    expect(within(card).getByText(/^Diff too large to compute\./)).toBeInTheDocument();
    const toggles = within(card).getAllByRole('button', { name: 'Show all 8,192 lines' });
    expect(toggles).toHaveLength(2); // the old text and the new text
    for (const toggle of toggles) expect(toggle).toHaveAttribute('aria-expanded', 'false');
    const [oldBlock, newBlock] = Array.from(card.querySelectorAll('pre'));
    expect(oldBlock.textContent).toBe(oldText.split('\n').slice(0, 12).join('\n'));
    expect(newBlock.textContent).toBe(newText.split('\n').slice(0, 12).join('\n'));
  });

  it('shows both texts in full, side by side, when a diff is too large and both are opened', () => {
    const oldText = oneCharLines(2501, 'abcdefghijklmnopqrstuvwxyz');
    const newText = oneCharLines(2500, 'ABCDEFGHIJKLMNOPQRSTUVWXYZ');
    const { container } = renderStep({ tool_executions: [toolCall('Edit', { file_path: '/repo/a.ts', old_string: oldText, new_string: newText })] });
    const [card] = toolCardsOf(cardOf(container));
    expect(within(card).getByText(/^Diff too large to compute\./)).toBeInTheDocument(); // 5,001 lines in all
    for (const toggle of within(card).getAllByRole('button', { name: /^Show all [\d,]+ lines$/ })) fireEvent.click(toggle);
    const [oldBlock, newBlock] = Array.from(card.querySelectorAll('pre'));
    expect(oldBlock.textContent).toBe(oldText);
    expect(newBlock.textContent).toBe(newText);
  });

  it('still diffs an edit of 5,000 changed lines, at the limit', () => {
    const oldText = oneCharLines(2500, 'abcdefghijklmnopqrstuvwxyz');
    const newText = oneCharLines(2500, 'ABCDEFGHIJKLMNOPQRSTUVWXYZ');
    const { container } = renderStep({ tool_executions: [toolCall('Edit', { file_path: '/repo/a.ts', old_string: oldText, new_string: newText })] });
    const [card] = toolCardsOf(cardOf(container));
    expect(within(card).queryByText(/Diff too large/)).not.toBeInTheDocument();
    expect(card.querySelectorAll('del')).toHaveLength(12);
    expect(within(card).getByRole('button', { name: 'Show all 5,000 lines' })).toBeInTheDocument();
  });

  it('shows both sides in full once expanded', () => {
    // Past the 500-edit give-up threshold, but small enough to expand quickly in jsdom.
    const oldText = oneCharLines(1500, 'abcdefghijklmnopqrstuvwxyz');
    const newText = oneCharLines(1500, 'ABCDEFGHIJKLMNOPQRSTUVWXYZ');
    const { container } = renderStep({ tool_executions: [toolCall('Edit', { file_path: '/repo/a.ts', old_string: oldText, new_string: newText })] });
    const [card] = toolCardsOf(cardOf(container));
    fireEvent.click(within(card).getByRole('button', { name: 'Show all 3,000 lines' }));
    const removed = Array.from(card.querySelectorAll('del')).map((el) => el.textContent!.slice(1));
    const added = Array.from(card.querySelectorAll('ins')).map((el) => el.textContent!.slice(1));
    expect(removed.join('\n')).toBe(oldText);
    expect(added.join('\n')).toBe(newText);
    expect(card.textContent).not.toContain('unchanged'); // nothing is folded away
  });
});

// ---------------------------------------------------------------------------
// Stimulus that only looks like markers
// ---------------------------------------------------------------------------

describe('stimulus text that only resembles a marker', () => {
  it.each([
    ['a marker in the middle of a sentence', 'the log says [tool_result Bash] ok and [meta] more'],
    ['a marker after a single newline', 'first line\n[tool_result Bash] second line'],
    ['an unclosed marker', '[tool_result Bash'],
    ['a marker with no tool name', '[tool_result ] x'],
    ['an error flag with no space after it', '[tool_result Bash error]output'],
    ['a compaction marker without its newline', '[compaction summary] text'],
  ])('shows %s as a prompt and offers no tool-result toggle', (_label, prompt) => {
    const { container } = renderStep({ author: 'human', user_prompt: prompt, tool_executions: [] });
    const card = cardOf(container);
    expect(within(card).queryByRole('button', { name: /^Tool results/ })).not.toBeInTheDocument();
    expect(within(card).getByText(prompt, { normalizer: (text) => text })).toHaveClass('font-serif');
  });

  it('splits at a marker that follows a blank line in a human prompt, as the producer does not mark human text', () => {
    const { container } = renderStep({ author: 'human', user_prompt: 'Here is a log:\n\n[tool_result Bash] pasted by the user', tool_executions: [] });
    const card = cardOf(container);
    expect(within(card).getByText('Here is a log:')).toBeInTheDocument();
    expect(within(card).getByRole('button', { name: 'Tool results · Bash' })).toHaveAttribute('aria-expanded', 'true');
    expect(within(card).getByText('pasted by the user')).toBeInTheDocument();
  });

  it('shows a missing, empty or blank stimulus as none recorded', () => {
    for (const user_prompt of [null, '', '   ', '\n\n', '[no stimulus recorded]']) {
      const { container, unmount } = renderStep({ user_prompt, tool_executions: [] });
      // The badge names it; the pane (reworded) says what that means.
      expect(within(cardOf(container)).getAllByText('No stimulus recorded'), JSON.stringify(user_prompt)).toHaveLength(1);
      expect(within(cardOf(container)).getByText('Nothing was recorded for what this step reacted to.')).toBeInTheDocument();
      unmount();
    }
  });

  it('keeps a lone interruption line as a prompt', () => {
    const { container } = renderStep({ author: 'human', user_prompt: '[Request interrupted by user]', tool_executions: [] });
    expect(within(cardOf(container)).getByText('[Request interrupted by user]')).toHaveClass('font-serif');
  });
});

// ---------------------------------------------------------------------------
// Props that change, strict mode, frozen data
// ---------------------------------------------------------------------------

describe('re-rendering and strict mode', () => {
  const resultsStep = step({ user_prompt: '[tool_result Bash] first\n\n[tool_result Read error] second' });

  it('switches between the layouts when the node changes under the same card', () => {
    const { container, rerender } = render(<TimelineNode interaction={fixtures.mcpNode} />);
    expect(screen.getByText('Thought')).toBeInTheDocument();

    rerender(<TimelineNode interaction={fixtures.promptStep} />);
    expect(screen.getByText('AI step')).toBeInTheDocument();
    expect(screen.queryByText('Thought')).not.toBeInTheDocument();
    expect(container.querySelector('[data-layout="transcript"]')).not.toBeNull();

    rerender(<TimelineNode interaction={fixtures.mcpNode} />);
    expect(screen.getByText('Thought')).toBeInTheDocument();
    expect(screen.queryByText('AI step')).not.toBeInTheDocument();
    expect(container.querySelector('[data-layout]')).toBeNull();
  });

  it('keeps a toggle open across a re-render with an updated node', () => {
    const { rerender } = render(<TimelineNode interaction={resultsStep} />);
    const toggle = screen.getByRole('button', { name: /^Tool results/ });
    fireEvent.click(toggle);
    expect(screen.getByText('first')).toBeInTheDocument();

    rerender(<TimelineNode interaction={{ ...resultsStep, artifact_links: [fixtures.generatedLink(HARNESS_ID)] }} isSelected />);
    expect(screen.getByRole('button', { name: /^Tool results/ })).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('first')).toBeInTheDocument();
    expect(screen.getByText('a1b2c3d')).toBeInTheDocument();
  });

  it('works under StrictMode, which runs effects and initialisers twice', () => {
    const { container } = render(
      <StrictMode>
        <TimelineNode interaction={fixtures.failingStep} />
      </StrictMode>,
    );
    const toggle = screen.getByRole('button', { name: /^Tool results/ });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expandAll(cardOf(container));
    expect(screen.getByText(/test line 30/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Show less' }));
    expect(screen.queryByText(/test line 30/)).not.toBeInTheDocument();
  });

  it('renders the same card every time: nothing depends on randomness or the clock', () => {
    const normalise = withoutIds;
    for (const node of [fixtures.promptStep, fixtures.failingStep, fixtures.agentStep, fixtures.serverToolStep]) {
      const first = render(<TimelineNode interaction={node} />);
      const a = normalise(first.container.innerHTML);
      first.unmount();
      const second = render(<TimelineNode interaction={node} />);
      expect(normalise(second.container.innerHTML)).toBe(a);
      second.unmount();
    }
  });

  it('does not write to the node it is given', () => {
    const freeze = <T,>(value: T): T => {
      if (value !== null && typeof value === 'object' && !Object.isFrozen(value)) {
        Object.freeze(value);
        for (const item of Object.values(value)) freeze(item);
      }
      return value;
    };
    const nodes = Object.values(fixtures).filter(
      (value): value is InteractionNode =>
        typeof value === 'object' && value !== null && 'conversation_id' in value && 'tool_executions' in value,
    );
    expect(nodes.length).toBeGreaterThanOrEqual(20);
    for (const node of nodes) {
      const frozen = freeze(structuredClone(node));
      const { container, unmount } = render(<TimelineNode interaction={frozen} />);
      expandAll(cardOf(container));
      unmount();
    }
  });
});

describe('nodes that were never validated', () => {
  it('draws a node whose arrays are missing or the wrong type, as a transcript node', () => {
    for (const tool_executions of [undefined, null, 'nope', 7, {}] as unknown[]) {
      const { container, unmount } = renderStep({ tool_executions: tool_executions as InteractionNode['tool_executions'] });
      expect(within(cardOf(container)).getByText('AI step')).toBeInTheDocument();
      expect(toolCardsOf(cardOf(container))).toHaveLength(0);
      unmount();
    }
  });

  it('draws a node whose text fields are the wrong type without throwing', () => {
    const odd = step({
      user_prompt: 42 as unknown as string,
      model_cot: { a: 1 } as unknown as string,
      model_response: ['x'] as unknown as string,
      model_name: 7 as unknown as string,
    });
    const { container } = render(<TimelineNode interaction={odd} />);
    expect(within(cardOf(container)).getByText('AI step')).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// Random nodes
// ---------------------------------------------------------------------------

describe('random nodes', () => {
  const PIECES = [
    '',
    ' ',
    'x',
    'hello world',
    'line one\nline two',
    '\r\n',
    '\t',
    '\n\n',
    '\n',
    '[tool_result Bash] out',
    '[tool_result Read error] bad',
    '[tool_result mcp__a__b] r',
    '[meta] note',
    '[compaction summary]\nsummary',
    '[Request interrupted by user]',
    '[no stimulus recorded]',
    '[image]',
    '<b>bold</b>',
    '&amp;',
    '"quote"',
    "'",
    '`tick`',
    'Zażółć 🙂',
    '[CVC_REDACTED:v1:secret_assignment:11111111-2222-4333-8444-555555555555]',
    '…[cvc: truncated 5 bytes]…',
    'a'.repeat(300),
    '0123456789abcdef'.repeat(40),
    '[',
    ']',
    '[tool_result ',
    ' error] ',
    '__proto__',
    'constructor',
  ];
  const IDS = [HARNESS_ID, '3b9f0f1e-6a4c-4d6e-9b1a-2f6c8e7d5a10', 'legacy-id'];
  const AUTHORS: InteractionNode['author'][] = ['human', 'system', 'external', 'agent'];
  const TOOL_NAMES = [
    'Bash',
    'Edit',
    'MultiEdit',
    'Write',
    'NotebookEdit',
    'Read',
    'Glob',
    'Grep',
    'Agent',
    'Task',
    'WebFetch',
    'WebSearch',
    'TodoWrite',
    'mcp__a__b',
    'mcp__a',
    'server:web_search',
    'server:x',
    '',
    'weird name',
    '__proto__',
  ];

  it('renders 250 random nodes without a warning, losing no text and miscounting no failure', () => {
    const rand = prng(0xdecafbad);
    const pick = <T,>(items: readonly T[]): T => items[Math.floor(rand() * items.length)];
    const text = (max = 8) =>
      Array.from({ length: Math.floor(rand() * max) }, () => pick(PIECES)).join(pick(['', '', '\n', '\n\n', ' ']));
    const json = (depth = 0): unknown => {
      const roll = rand();
      if (depth > 3 || roll < 0.35) return rand() < 0.5 ? text(3) : pick([1, 0, -1, 1.5, true, false, null]);
      if (roll < 0.65) return Array.from({ length: Math.floor(rand() * 4) }, () => json(depth + 1));
      return Object.fromEntries(
        Array.from({ length: Math.floor(rand() * 4) }, () => [
          pick(['a', 'b', 'command', 'file_path', 'old_string', 'new_string', 'content', '__proto__', 'constructor', text(1)]),
          json(depth + 1),
        ]),
      );
    };
    const argumentsFor = (name: string): string => {
      const roll = rand();
      if (roll < 0.06) return '';
      if (roll < 0.1) return 'null';
      if (roll < 0.14) return text(3);
      if (roll < 0.18) return `{"bytes":${Math.floor(rand() * 1e6)},"cvc_truncated":true}`;
      if (roll < 0.22) return '{"a":';
      if (roll < 0.5) return JSON.stringify(json());
      switch (name) {
        case 'Bash':
          return JSON.stringify({ command: text(), description: rand() < 0.5 ? text(2) : undefined, timeout: rand() < 0.5 ? Math.floor(rand() * 600000) : undefined });
        case 'Edit':
          return JSON.stringify({ file_path: text(1), old_string: text(), new_string: text(), replace_all: rand() < 0.3 });
        case 'MultiEdit':
          return JSON.stringify({ file_path: text(1), edits: Array.from({ length: Math.floor(rand() * 4) }, () => ({ old_string: text(), new_string: text() })) });
        case 'Write':
          return JSON.stringify({ file_path: text(1), content: text() });
        case 'Read':
          return JSON.stringify({ file_path: text(1), offset: rand() < 0.5 ? Math.floor(rand() * 100) : undefined, limit: rand() < 0.5 ? Math.floor(rand() * 100) : undefined });
        case 'TodoWrite':
          return JSON.stringify({ todos: Array.from({ length: Math.floor(rand() * 4) }, () => ({ content: text(2), status: pick(['pending', 'completed', 'in_progress', text(1)]) })) });
        case 'Task':
        case 'Agent':
          return JSON.stringify({ prompt: text(), description: text(2), subagent_type: text(1) });
        default:
          return JSON.stringify(json());
      }
    };

    let transcript = 0;
    let triad = 0;
    for (let run = 0; run < 250; run++) {
      const tools: ToolExecution[] = Array.from({ length: rand() < 0.4 ? 0 : Math.floor(rand() * 7) }, () => {
        const tool_name = pick(TOOL_NAMES);
        return { tool_protocol: 'native', tool_name, arguments: argumentsFor(tool_name), status: rand() < 0.3 ? 'failure' : 'success' };
      });
      const node = makeNode({
        id: pick(IDS),
        author: pick(AUTHORS),
        model_name: rand() < 0.2 ? null : text(1) || 'm',
        user_prompt: rand() < 0.1 ? null : text(10),
        model_cot: rand() < 0.5 ? null : text(),
        model_response: rand() < 0.3 ? null : text(),
        tool_executions: tools,
      });
      const { container, unmount } = render(
        <StrictMode>
          <TimelineNode interaction={node} />
        </StrictMode>,
      );
      const card = cardOf(container);
      const label = `run ${run}`;

      if (nodeLayout(node) === 'transcript') {
        transcript++;
        // "AI step" only for a human or system author; any other author is named as the author field says.
        const header = node.author === 'human' || node.author === 'system' ? 'AI step' : 'External';
        expect(within(card.firstElementChild as HTMLElement).getByText(header), label).toBeInTheDocument();
        expect(toolCardsOf(card), label).toHaveLength(tools.length);
        const failed = failedToolCount(node);
        const chip = /(\d+) tool calls? failed|1 tool call failed/.exec(card.firstElementChild!.textContent ?? '');
        expect(chip ? Number.parseInt(chip[0], 10) : 0, label).toBe(failed);
        for (const word of ['Thought', 'Observation', 'Green check', 'Review required', 'Critical alert', 'AI Agent']) {
          expect(card.textContent, `${label} ${word}`).not.toContain(word);
        }

        expandAll(card);
        const body = card.textContent ?? '';
        const shown = (value: string) => displayText(value).replace(/\n$/, '');
        for (const segment of parseStimulus(node.user_prompt)) {
          if (segment.kind === 'none' || segment.text.trim() === '') continue;
          expect(body.includes(shown(segment.text)), `${label} lost ${segment.kind}: ${JSON.stringify(segment.text).slice(0, 100)}`).toBe(true);
        }
        for (const field of [node.model_cot, node.model_response]) {
          if (field && field.trim() !== '') expect(body.includes(shown(field)), `${label} lost ${JSON.stringify(field).slice(0, 100)}`).toBe(true);
        }
        const crInCode = Array.from(card.querySelectorAll('pre, [class*="whitespace-pre"]')).filter((el) => (el.textContent ?? '').includes('\r'));
        expect(crInCode, `${label} carriage return in text`).toHaveLength(0);
      } else {
        triad++;
        expect(within(card).getByText('Thought'), label).toBeInTheDocument();
        expect(within(card).queryByText('AI step'), label).not.toBeInTheDocument();
      }
      unmount();
    }
    // Both layouts are exercised.
    expect(transcript).toBeGreaterThan(60);
    expect(triad).toBeGreaterThan(60);
  });
});
