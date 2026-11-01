/**
 * What a card draws for a call, as text for the search to read (issue #118, revision round): `toolCallPieces` and `splitChip`.
 * Values are what the capture recorded, as the card shows them; the names of arguments and the Reviewer's own words for the
 * options are not among them.
 */
import { describe, expect, it } from 'vitest';
import { MAX_DRAWN_STRINGS, splitChip, toolCallPieces, type DrawnPiece } from './toolCalls';
import { rawToolCall, toolCall, wireToolCall } from '../test/fixtures';

const texts = (pieces: DrawnPiece[]) => pieces.map((piece) => piece.text);
const piecesOf = (name: string, input: Record<string, unknown>) => toolCallPieces(wireToolCall(name, input));

describe('splitChip', () => {
  it.each([
    ['timeout 120s', { label: 'timeout ', value: '120s' }],
    ['in /repo/src', { label: 'in ', value: '/repo/src', scope: 'path' }],
    ['glob *.ts', { label: 'glob ', value: '*.ts' }],
    ['type ts', { label: 'type ', value: 'ts' }],
    ['output content', { label: 'output ', value: 'content' }],
    ['-C 2', { label: '-C ', value: '2' }],
    ['head_limit 20', { label: 'head_limit ', value: '20' }],
    ['cell c3f2', { label: 'cell ', value: 'c3f2' }],
    ['mode insert', { label: 'mode ', value: 'insert' }],
    ['subagent Explore', { label: 'subagent ', value: 'Explore' }],
    ['pages 1-3', { label: 'pages ', value: '1-3' }],
    ['lines 10–59', { label: 'lines ', value: '10–59' }],
    ['line 10', { label: 'line ', value: '10' }],
    ['from line 120', { label: 'from line ', value: '120' }],
  ])('takes %j apart into the Reviewer\'s words and the value', (chip, expected) => {
    expect(splitChip(chip)).toEqual(expected);
  });

  it.each(['runs in background', 'replace all occurrences', 'ignore case', 'line numbers', 'multiline'])('knows %j for the Reviewer\'s words alone', (chip) => {
    expect(splitChip(chip)).toBeNull();
  });

  it('is not fooled by a value that looks like an option, and does not fail on anything', () => {
    expect(splitChip('type multiline')).toEqual({ label: 'type ', value: 'multiline' });
    expect(splitChip('')).toBeNull();
    expect(splitChip('something else')).toBeNull();
    expect(splitChip(7 as unknown as string)).toBeNull();
  });
});

describe('toolCallPieces', () => {
  it('gives the name a call is shown under first, as the one name it is searched as', () => {
    expect(toolCallPieces(wireToolCall('mcp__srv__do_thing', { a: 'x' }))[0]).toEqual({ text: 'srv · do_thing', scope: 'tool' });
    expect(toolCallPieces(wireToolCall('Bash', { command: 'ls' }))[0]).toEqual({ text: 'Bash', scope: 'tool' });
    expect(toolCallPieces(wireToolCall('server:web_search', { query: 'x' }))[0]).toEqual({ text: 'web_search (server-side)', scope: 'tool' });
  });

  it('gives a command, its description and the value of its options, and not the words for the options', () => {
    const pieces = piecesOf('Bash', { command: 'npm test', description: 'Run the tests', timeout: 120000, run_in_background: true });
    expect(texts(pieces)).toEqual(['Bash', 'Run the tests', 'npm test', '120s']);
  });

  it('gives the file of an edit as a path, and the lines of the old and new text as lines', () => {
    const pieces = piecesOf('Edit', { file_path: '/repo/a.ts', old_string: 'a\nb', new_string: 'a\nc', replace_all: true });
    expect(pieces).toEqual([
      { text: 'Edit', scope: 'tool' },
      { text: '/repo/a.ts', scope: 'path' },
      { text: 'a\nb', lines: true },
      { text: 'a\nc', lines: true },
    ]);
  });

  it('gives each edit of a MultiEdit', () => {
    const pieces = piecesOf('MultiEdit', { file_path: '/r/a.ts', edits: [{ old_string: 'x', new_string: 'y' }, { old_string: 'p', new_string: 'q', replace_all: true }] });
    expect(texts(pieces)).toEqual(['MultiEdit', '/r/a.ts', 'x', 'y', 'p', 'q']);
  });

  it('gives a file that is read as a path, and the range of lines as a value', () => {
    const pieces = piecesOf('Read', { file_path: '/repo/a.ts', offset: 10, limit: 50 });
    expect(pieces).toEqual([{ text: 'Read', scope: 'tool' }, { text: '/repo/a.ts', scope: 'path' }, { text: '10–59' }]);
  });

  it('gives a search, its place as a path and its options as values, and not the options that are the Reviewer\'s words', () => {
    const pieces = piecesOf('Grep', { pattern: 'TODO', path: '/repo/src', glob: '*.ts', '-i': true, '-n': true, '-C': 2, multiline: true, output_mode: 'content' });
    expect(pieces).toEqual([
      { text: 'Grep', scope: 'tool' },
      { text: 'TODO' },
      { text: '/repo/src', scope: 'path' },
      { text: '*.ts' },
      { text: 'content' },
      { text: '2' },
    ]);
  });

  it('gives the values of a call it knows nothing of, numbers and booleans as they are drawn, and never the names', () => {
    const pieces = piecesOf('mcp__srv__tool', { count: 12345, flag: true, plain: 'gamma', list: ['alpha'], nothing: null, file_path: '/x/y.ts', path: 7 });
    // (In the order the capture writes them: by name.)
    expect(texts(pieces)).toEqual(['srv · tool', '12345', '/x/y.ts', 'true', '[\n  "alpha"\n]', 'null', '7', 'gamma']);
    // The file is a path when it is text, and a number that is called `path` is just a number.
    expect(pieces.filter((piece) => piece.scope === 'path').map((piece) => piece.text)).toEqual(['/x/y.ts']);
  });

  it('gives the arguments the card does not use as values too, beside the ones it does', () => {
    const pieces = piecesOf('Bash', { command: 'ls', dangerouslyDisableSandbox: true, extra: 'more' });
    expect(texts(pieces)).toEqual(['Bash', 'ls', 'true', 'more']);
  });

  it('gives the items of a checklist and a sub-agent\'s prompt', () => {
    expect(texts(piecesOf('TodoWrite', { todos: [{ content: 'Write it', status: 'pending', activeForm: 'Writing it' }, { content: 'Ship', status: 'done' }] }))).toEqual(['TodoWrite', 'Write it', 'Writing it', 'Ship']);
    expect(texts(piecesOf('Task', { description: 'Look around', prompt: 'Find the bug', subagent_type: 'Explore' }))).toEqual(['Task', 'Look around', 'Explore', 'Find the bug']);
  });

  it('gives arguments that are not JSON as they were recorded, and nothing for what the card says in its own words', () => {
    expect(texts(toolCallPieces(rawToolCall('Odd', 'not json')))).toEqual(['Odd', 'not json']);
    expect(texts(toolCallPieces(rawToolCall('Odd', '[1,"two"]')))).toEqual(['Odd', '[1,"two"]']);
    // "No arguments recorded", and arguments too large to keep: nothing a person would look for.
    expect(texts(toolCallPieces(rawToolCall('Odd', 'null')))).toEqual(['Odd']);
    expect(texts(toolCallPieces(rawToolCall('Odd', '')))).toEqual(['Odd']);
    expect(texts(toolCallPieces(rawToolCall('Odd', '{"bytes":99999,"cvc_truncated":true}')))).toEqual(['Odd']);
  });

  it('gives arguments recorded as an object before blobs were validated as the JSON they would be', () => {
    const legacy = { tool_name: 'Bash', arguments: { command: 'ls' } as unknown as string };
    expect(texts(toolCallPieces(legacy))).toEqual(['Bash', '{"command":"ls"}']);
  });

  it('does not fail on a call that is not one', () => {
    for (const exec of [null, undefined, {}, { tool_name: 7 }, { tool_name: 'Bash', arguments: null }] as unknown as Array<Parameters<typeof toolCallPieces>[0]>) {
      expect(() => toolCallPieces(exec)).not.toThrow();
    }
    expect(toolCallPieces(null)[0].text).toBe('Unknown tool');
  });

  it('reads no more of a huge call than its budget, and shares one budget between calls when it is given one', () => {
    const many = Object.fromEntries(Array.from({ length: 50_000 }, (_, k) => [`k${k}`, `value ${k}`]));
    const started = performance.now();
    const pieces = toolCallPieces(wireToolCall('mcp__srv__tool', many));
    expect(pieces.length).toBeLessThanOrEqual(MAX_DRAWN_STRINGS + 1);
    expect(performance.now() - started).toBeLessThan(2_000);
    const budget = { strings: 3, chars: 1_000 };
    const first = toolCallPieces(toolCall('Bash', { command: 'a', description: 'b', extra: 'c', more: 'd' }), budget);
    expect(first).toHaveLength(1 + 3);
    expect(toolCallPieces(toolCall('Bash', { command: 'zzz' }), budget).map((piece) => piece.text)).toEqual(['Bash']);
  });
});
