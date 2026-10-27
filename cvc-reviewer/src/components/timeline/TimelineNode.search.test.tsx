/**
 * What a card marks, and what it leaves alone (issue #118, revision round): the value of an option and not the words for it,
 * the value of an argument and not its name, a file in a field that is one, a name or hash that is only partly drawn marked
 * whole, and the Action pane of the other layout in the parts it is written in.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { highlightTerms, parseQuery } from '../../lib/timelineSearch';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { generatedLink, makeNode, wireToolCall } from '../../test/fixtures';
import { HighlightContext } from './searchHighlight';
import { TimelineNode } from './TimelineNode';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(() => installDomStubs());
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

const HASH = 'abcdef0123456789abcdef0123456789abcdef01';
const ID = '1f0e5c3a-9d2b-8c4e-a1b7-000000000001';
const TRIAD_ID = '3b9f0f1e-6a4c-4d6e-9b1a-000000000002';

function searched(node: ReturnType<typeof makeNode>, query: string) {
  return render(
    <HighlightContext.Provider value={highlightTerms(parseQuery(query))}>
      <TimelineNode interaction={node} />
    </HighlightContext.Provider>,
  );
}
const marks = (root: ParentNode) => Array.from(root.querySelectorAll('mark')).map((mark) => mark.textContent);

const calls = makeNode({
  id: ID,
  author: 'human',
  user_prompt: 'p',
  tool_executions: [
    wireToolCall('Grep', { pattern: 'TODO', path: '/repo/src', glob: '*.ts', '-i': true, '-n': true, '-C': 2 }),
    wireToolCall('Bash', { command: 'ls', timeout: 120000, run_in_background: true }),
    wireToolCall('mcp__srv__tool', { target_name: 'alpha', file_path: '/x/target_name.ts', count: 42 }),
  ],
});

describe('the options of a call', () => {
  it('mark their value and never the words the Reviewer writes for them', () => {
    const { container } = searched(calls, '*.ts');
    expect(marks(container)).toEqual(['*.ts']);
    expect(container.querySelector('mark')!.closest('li')).toHaveTextContent('glob *.ts');
    // The chips drawn without a search are what they were: one text, label and value.
    cleanup();
    const plain = render(<TimelineNode interaction={calls} />);
    expect(Array.from(plain.container.querySelectorAll('li')).map((li) => li.textContent)).toContain('glob *.ts');
  });

  it('are not found by the words for them, nor marked by them', () => {
    for (const words of ['glob', 'ignore', 'line numbers', 'runs in', 'background', 'timeout']) {
      const { container } = searched(calls, words);
      expect(marks(container), words).toEqual([]);
      cleanup();
    }
  });

  it('mark a duration in the form the card writes it', () => {
    const { container } = searched(calls, '120s');
    expect(marks(container)).toEqual(['120s']);
  });

  it('mark the place of a search as a path', () => {
    const { container } = searched(calls, 'path:/repo/src');
    expect(marks(container)).toEqual(['/repo/src']);
    expect(container.querySelector('mark')!.closest('li')).toHaveTextContent('in /repo/src');
  });
});

describe('the arguments of a call', () => {
  it('are marked by their value and never by their name', () => {
    // `target_name` is the name of an argument, a part of the file's name, and not the value of anything else.
    const { container } = searched(calls, 'target_name');
    // The path /x/target_name.ts holds the word: it is marked there, in the field that holds the file, and not in the label of any row.
    expect(marks(container)).toEqual(['target_name']);
    const [mark] = Array.from(container.querySelectorAll('mark'));
    expect(mark.closest('dd')).not.toBeNull();
    expect(mark.closest('dt')).toBeNull();
    cleanup();
    const names = searched(calls, 'file_path');
    expect(marks(names.container)).toEqual([]);
  });

  it('mark a file in a field that is one as a path, and only for a term that looks in paths', () => {
    const { container } = searched(calls, 'path:target_name');
    expect(marks(container)).toEqual(['target_name']);
    cleanup();
    const other = searched(calls, 'tool:target_name');
    expect(marks(other.container)).toEqual([]);
  });

  it('mark a number as it is drawn', () => {
    const { container } = searched(calls, '42');
    expect(marks(container)).toEqual(['42']);
  });
});

describe('a name or a hash that is only partly drawn', () => {
  it('marks the name a card gives a tool whole when the term is in the name that was recorded', () => {
    const { container } = searched(calls, 'tool:mcp__srv');
    expect(marks(container)).toEqual(['srv · tool']);
    cleanup();
    const shown = searched(calls, 'tool:srv');
    expect(marks(shown.container)).toEqual(['srv']);
  });

  it('marks the seven characters of a commit whole when the term is in the rest of the hash', () => {
    const node = makeNode({ id: ID, author: 'human', user_prompt: 'p', artifact_links: [generatedLink(ID, HASH)] });
    const { container } = searched(node, `commit:${HASH.slice(4, 20)}`);
    expect(marks(container)).toEqual(['abcdef0']);
    cleanup();
    const inChip = searched(node, 'commit:abc');
    expect(marks(inChip.container)).toEqual(['abc']);
  });
});

describe('the Action pane of the Thought / Action / Observation layout', () => {
  const triad = makeNode({
    id: TRIAD_ID,
    author: 'agent',
    user_prompt: 'Fix the failing import',
    model_response: 'ok',
    tool_executions: [wireToolCall('Bash', { command: 'npm run build' }), wireToolCall('Edit', { file_path: '/repo/src/app.ts', old_string: 'a', new_string: 'b' })],
  });

  it('is drawn as one text without a search, and in parts with one', () => {
    const plain = render(<TimelineNode interaction={triad} />);
    const lines = (root: ParentNode) => Array.from(root.querySelectorAll('.font-mono.text-xs')).map((element) => element.textContent);
    expect(lines(plain.container)).toContain('Bash {"command":"npm run build"}');
    // One text node for the line, as it always was.
    const line = Array.from(plain.container.querySelectorAll('.font-mono.text-xs')).find((element) => element.textContent?.startsWith('Bash '))!;
    expect(line.childNodes).toHaveLength(1);
    cleanup();
    const found = searched(triad, 'npm');
    expect(lines(found.container)).toContain('Bash {"command":"npm run build"}');
  });

  it('marks the name of the tool for tool: and the file for path:', () => {
    expect(marks(searched(triad, 'tool:bash').container)).toEqual(['Bash']);
    cleanup();
    expect(marks(searched(triad, 'path:src/app.ts').container)).toEqual(['src/app.ts']);
  });

  it('marks the hash of a link, and never the word for the kind of link or the placeholder', () => {
    const linked = makeNode({ id: TRIAD_ID, author: 'agent', user_prompt: 'the generated code', model_response: 'nothing recorded here', artifact_links: [generatedLink(TRIAD_ID, HASH)] });
    expect(marks(searched(linked, 'commit:abcdef0').container)).toEqual(['abcdef0', 'abcdef0']);
    cleanup();
    expect(marks(searched(linked, 'generated').container)).toEqual(['generated']);
    cleanup();
    const bare = makeNode({ id: TRIAD_ID, author: 'agent', user_prompt: 'nothing recorded here', model_response: 'ok' });
    expect(marks(searched(bare, 'recorded').container)).toEqual(['recorded']);
  });

  it('draws the reasoning of an AI agent behind a count, and holds no other', () => {
    const agent = makeNode({ id: TRIAD_ID, author: 'agent', user_prompt: 'q', model_cot: 'the answer is zebra', model_response: 'r' });
    searched(agent, 'zebra');
    expect(screen.getByText('· 1 match')).toBeInTheDocument();
  });
});
