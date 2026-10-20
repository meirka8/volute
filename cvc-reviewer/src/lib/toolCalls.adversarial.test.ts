/**
 * Tool arguments are model-written text that can be malformed, oversized or
 * hostile. These tests feed the view-model builders the inputs that would hurt:
 * absurd nesting, prototype-looking keys, values of the wrong type, and volume.
 */
import { describe, expect, it } from 'vitest';
import { describeToolCall, parseToolArguments, toolOutcome, toolTitle, type ToolCallView } from './toolCalls';
import { rawToolCall, toolCall } from '../test/fixtures';

/** The fastest of a few runs, so a busy machine cannot fail a check meant to catch runaway cost. */
function bestOf(runs: number, work: () => void): number {
  let best = Infinity;
  for (let run = 0; run < runs; run++) {
    const started = performance.now();
    work();
    best = Math.min(best, performance.now() - started);
  }
  return best;
}

const nested = (depth: number): string => '['.repeat(depth) + ']'.repeat(depth);

/** How deep the first element chain of a parsed `[[[...]]]` goes, without recursion. */
function arrayDepth(value: unknown): number {
  let depth = 0;
  let current = value;
  while (Array.isArray(current)) {
    depth++;
    current = current[0];
  }
  return depth;
}

describe('parseToolArguments: unusual JSON', () => {
  it('parses nesting 2000 levels deep', () => {
    const parsed = parseToolArguments(`{"a":${nested(2000)}}`);
    expect(parsed.kind).toBe('object');
    if (parsed.kind === 'object') expect(arrayDepth(parsed.value.a)).toBe(2000);
  });

  it('never throws, whatever the nesting', () => {
    for (const depth of [1, 100, 5_000, 50_000, 500_000]) {
      for (const raw of [nested(depth), `{"a":${nested(depth)}}`, `{"a":${'{"a":'.repeat(depth)}1${'}'.repeat(depth)}}`]) {
        const parsed = parseToolArguments(raw);
        expect(['object', 'raw']).toContain(parsed.kind);
      }
    }
  });

  it('shows text that is nearly JSON verbatim', () => {
    for (const raw of [
      '﻿{"a":1}', // a byte-order mark
      '{"a":1} trailing',
      '{"a":NaN}',
      '{"a":Infinity}',
      "{'a':1}",
      '{"a":1,}',
      '{a:1}',
      '{"a":01}',
      '{"a":"\u0001"}', // a raw control character inside a string
      '{"a":1}{"b":2}',
    ]) {
      expect(parseToolArguments(raw), JSON.stringify(raw)).toEqual({ kind: 'raw', text: raw });
    }
  });

  it('lets the last of two equal keys win, as JSON.parse does', () => {
    expect(parseToolArguments('{"a":1,"a":2}')).toEqual({ kind: 'object', value: { a: 2 } });
    expect(parseToolArguments('{"bytes":1,"bytes":2,"cvc_truncated":true}')).toEqual({ kind: 'truncated', bytes: 2 });
  });

  it('accepts only a safe, non-negative integer as the size in the truncation sentinel', () => {
    expect(parseToolArguments(`{"bytes":${Number.MAX_SAFE_INTEGER},"cvc_truncated":true}`)).toEqual({
      kind: 'truncated',
      bytes: Number.MAX_SAFE_INTEGER,
    });
    for (const bytes of ['9007199254740992', '1e21', '1e999', '1.0e0e', '-1', '0.5', 'null', '[]', '"5"', 'true']) {
      const parsed = parseToolArguments(`{"bytes":${bytes},"cvc_truncated":true}`);
      // 1.0e0e is not JSON at all; the rest parse but are not a size.
      expect(['object', 'raw'], bytes).toContain(parsed.kind);
    }
  });

  it('does not treat a sentinel nested inside another value as one', () => {
    expect(parseToolArguments('{"input":{"bytes":5,"cvc_truncated":true}}').kind).toBe('object');
    expect(parseToolArguments('[{"bytes":5,"cvc_truncated":true}]').kind).toBe('raw');
  });

  it('copes with an enormous argument string', () => {
    const big = JSON.stringify({ command: 'x'.repeat(2 * 1024 * 1024) });
    const best = bestOf(3, () => {
      expect(parseToolArguments(big).kind).toBe('object');
    });
    expect(best).toBeLessThan(500);
  });
});

describe('describeToolCall: keys that look like Object.prototype members', () => {
  const polluted = () => ({}) as Record<string, unknown>;

  it('shows an own "__proto__" key as a row and pollutes nothing', () => {
    const before = Object.keys(Object.prototype);
    const view = describeToolCall(rawToolCall('mcp__srv__tool', '{"__proto__":{"polluted":"yes"},"a":1}'));
    expect(view.kind).toBe('fields');
    if (view.kind === 'fields') {
      expect(view.fields.map((f) => f.key)).toEqual(['__proto__', 'a']);
      expect(view.fields[0].value).toBe('{\n  "polluted": "yes"\n}');
    }
    expect(polluted().polluted).toBeUndefined();
    expect(Object.keys(Object.prototype)).toEqual(before);
    expect(Object.getPrototypeOf({})).toBe(Object.prototype);
  });

  it('keeps such a key as an extra on a tool that is understood, without reading its value as a field', () => {
    const view = describeToolCall(
      rawToolCall('Bash', '{"command":"ls","__proto__":{"command":"rm -rf /"},"constructor":"x","toString":"y"}'),
    );
    expect(view).toMatchObject({ kind: 'command', command: 'ls' });
    expect(view.extras.map((f) => f.key)).toEqual(['__proto__', 'constructor', 'toString']);
    expect(polluted().command).toBeUndefined();
  });

  it('does not read a field from the prototype chain when the input lacks it', () => {
    // `{}` inherits `constructor`, `toString`, `valueOf`...; none of them is input.
    for (const tool of ['Bash', 'Edit', 'MultiEdit', 'Write', 'NotebookEdit', 'Read', 'Glob', 'Grep', 'Agent', 'Task', 'WebFetch', 'WebSearch', 'TodoWrite']) {
      expect(describeToolCall(rawToolCall(tool, '{}')), tool).toMatchObject({ kind: 'fields', fields: [] });
    }
  });

  it.each([
    ['constructor', '{"constructor":{"prototype":{"polluted":1}}}'],
    ['hasOwnProperty', '{"hasOwnProperty":1,"propertyIsEnumerable":"x"}'],
    ['valueOf', '{"valueOf":"x","toString":"y","toLocaleString":"z","isPrototypeOf":1}'],
    ['__defineGetter__', '{"__defineGetter__":"x","__lookupGetter__":"y"}'],
  ])('shows %s as an ordinary key', (_label, raw) => {
    for (const tool of ['Bash', 'mcp__srv__tool', 'TodoWrite', 'Edit']) {
      const view = describeToolCall(rawToolCall(tool, raw));
      const keys = [
        ...(view.kind === 'fields' ? view.fields.map((f) => f.key) : []),
        ...view.extras.map((f) => f.key),
      ];
      for (const key of Object.keys(JSON.parse(raw) as object)) expect(keys, `${tool} ${key}`).toContain(key);
    }
    expect(polluted().polluted).toBeUndefined();
  });

  it('does not let a MultiEdit or TodoWrite item smuggle in a prototype', () => {
    const edit = describeToolCall(
      rawToolCall('MultiEdit', '{"file_path":"/repo/a","edits":[{"old_string":"a","new_string":"b","__proto__":{"polluted":1}}]}'),
    );
    expect(edit.kind).toBe('fields'); // an unexpected key sends the whole call to plain rows
    const todos = describeToolCall(
      rawToolCall('TodoWrite', '{"todos":[{"content":"x","status":"pending","__proto__":{"polluted":1}}]}'),
    );
    expect(todos.kind).toBe('fields');
    expect(polluted().polluted).toBeUndefined();
  });

  it('treats tool names that are prototype members as unknown tools', () => {
    for (const name of ['__proto__', 'constructor', 'toString', 'hasOwnProperty', 'valueOf', '__defineGetter__']) {
      expect(describeToolCall(rawToolCall(name, '{"command":"ls"}')), name).toMatchObject({ kind: 'fields', tool: name });
      expect(describeToolCall(rawToolCall(`server:${name}`, '{"query":"q"}')), name).toMatchObject({ kind: 'fields' });
      expect(toolTitle(name)).toBe(name);
      expect(toolOutcome(toolCall(name, {}, 'failure'))).toBe('failure');
    }
  });
});

describe('describeToolCall: fields of the wrong type', () => {
  const rowsOf = (view: ToolCallView) => (view.kind === 'fields' ? view.fields : []);

  it.each([
    ['a number', 123, '123'],
    ['null', null, 'null'],
    ['a boolean', true, 'true'],
    ['an object', { a: 1 }, '{\n  "a": 1\n}'],
    ['an array', ['ls', '-la'], '[\n  "ls",\n  "-la"\n]'],
  ])('shows a Bash command that is %s as a row, with nothing dropped', (_label, command, text) => {
    const view = describeToolCall(toolCall('Bash', { command, description: 'd' }));
    expect(view.kind).toBe('fields');
    expect(rowsOf(view)).toEqual([
      { key: 'command', value: text, block: text.includes('\n') },
      { key: 'description', value: 'd', block: false },
    ]);
    expect(view.extras).toEqual([]);
  });

  it('shows an empty command as a command', () => {
    expect(describeToolCall(toolCall('Bash', { command: '' }))).toMatchObject({ kind: 'command', command: '' });
  });

  it.each([
    ['a string', 'oops'],
    ['a number', 5],
    ['null', null],
    ['a boolean', true],
    ['an object', { old_string: 'a', new_string: 'b' }],
    ['an empty array', []],
    ['an array of arrays', [['a', 'b']]],
    ['an array holding null', [null]],
    ['an array holding a number', [1]],
  ])('shows MultiEdit edits that are %s as rows', (_label, edits) => {
    const view = describeToolCall(toolCall('MultiEdit', { file_path: '/repo/a.ts', edits }));
    expect(view.kind).toBe('fields');
    expect(rowsOf(view).map((f) => f.key)).toEqual(['file_path', 'edits']);
  });

  it('shows a MultiEdit with a sparse or oversized edit list without losing an edit', () => {
    const many = Array.from({ length: 5000 }, (_, i) => ({ old_string: `old${i}`, new_string: `new${i}` }));
    const view = describeToolCall(toolCall('MultiEdit', { file_path: '/repo/a.ts', edits: many }));
    expect(view.kind).toBe('edit');
    if (view.kind === 'edit') {
      expect(view.edits).toHaveLength(5000);
      expect(view.edits[4999]).toEqual({ oldText: 'old4999', newText: 'new4999', chips: [] });
    }
  });

  it.each([
    ['Edit', { file_path: '/repo/a', old_string: 1, new_string: 'b' }],
    ['Edit', { file_path: '/repo/a', old_string: 'a' }],
    ['Edit', { file_path: 7, old_string: 'a', new_string: 'b' }],
    ['Write', { file_path: '/repo/a', content: null }],
    ['Write', { file_path: ['/repo/a'], content: 'x' }],
    ['NotebookEdit', { notebook_path: '/repo/a.ipynb', new_source: 5 }],
    ['Read', { file_path: { path: '/repo/a' } }],
    ['Glob', { pattern: ['*.ts'] }],
    ['Grep', { pattern: 42 }],
    ['Agent', { prompt: 7, description: 'd' }],
    ['WebFetch', { url: { href: 'https://example.com' } }],
    ['WebSearch', { query: null }],
    ['TodoWrite', { todos: 'nope' }],
  ])('falls back to rows for %s with %j, showing every recorded field', (tool, input) => {
    const view = describeToolCall(toolCall(tool, input));
    expect(view.kind).toBe('fields');
    expect(rowsOf(view).map((f) => f.key)).toEqual(Object.keys(input));
  });

  it('keeps an optional field of the wrong type as an extra, whatever its type', () => {
    const view = describeToolCall(
      toolCall('Grep', { pattern: 'x', '-A': '3', '-B': null, '-C': [2], head_limit: '20', glob: 5, path: {}, type: false }),
    );
    expect(view.kind).toBe('lookup');
    expect(view.extras.map((f) => f.key)).toEqual(['-A', '-B', '-C', 'head_limit', 'glob', 'path', 'type']);
    if (view.kind === 'lookup') expect(view.chips).toEqual([]);
  });

  it('turns an odd Read range into the lines it names, without inventing any', () => {
    const chips = (input: Record<string, unknown>) => {
      const view = describeToolCall(toolCall('Read', { file_path: '/repo/a', ...input }));
      return view.kind === 'lookup' ? view.chips : null;
    };
    expect(chips({ offset: -5, limit: -5 })).toEqual([]);
    expect(chips({ offset: 2.9, limit: 3.9 })).toEqual(['lines 2–4']);
    expect(chips({ offset: '5', limit: '5' })).toEqual([]);
    expect(chips({ offset: null })).toEqual([]);
  });

  it('formats an odd Bash timeout as a number of milliseconds or seconds', () => {
    const chip = (timeout: unknown) => {
      const view = describeToolCall(toolCall('Bash', { command: 'ls', timeout }));
      return view.kind === 'command' ? view.chips : null;
    };
    expect(chip(0)).toEqual(['timeout 0ms']);
    expect(chip(999)).toEqual(['timeout 999ms']);
    expect(chip(1000)).toEqual(['timeout 1s']);
    expect(chip(1001)).toEqual(['timeout 1001ms']);
    expect(chip(-5)).toEqual(['timeout -5ms']);
    expect(chip('120000')).toEqual([]); // a string is not a timeout; it stays an extra
    expect(chip(null)).toEqual([]);
  });
});

describe('describeToolCall: volume', () => {
  it('builds rows for an input with thousands of keys quickly', () => {
    const input = Object.fromEntries(Array.from({ length: 5000 }, (_, i) => [`key${i}`, `value${i}`]));
    let view: ToolCallView | undefined;
    const best = bestOf(3, () => {
      view = describeToolCall(toolCall('mcp__srv__tool', input));
    });
    expect(view?.kind).toBe('fields');
    if (view?.kind === 'fields') expect(view.fields).toHaveLength(5000);
    expect(best).toBeLessThan(300);
  });

  it('keeps a megabyte command whole', () => {
    const command = `echo ${'x'.repeat(1024 * 1024)}`;
    const view = describeToolCall(toolCall('Bash', { command }));
    expect(view).toMatchObject({ kind: 'command', command });
  });

  it('describes a long todo list and a long Task prompt without trimming them', () => {
    const todos = Array.from({ length: 5000 }, (_, i) => ({ content: `item ${i}`, status: 'pending' }));
    const view = describeToolCall(toolCall('TodoWrite', { todos }));
    expect(view.kind === 'todos' && view.todos).toHaveLength(5000);
    const prompt = 'p'.repeat(1024 * 1024);
    expect(describeToolCall(toolCall('Task', { prompt }))).toMatchObject({ kind: 'delegation', prompt });
  });
});

describe('describeToolCall: nesting', () => {
  it('shows a value nested 2000 levels deep without overflowing the stack', () => {
    const view = describeToolCall(rawToolCall('mcp__srv__tool', `{"deep":${nested(2000)}}`));
    expect(view.kind).toBe('fields');
    if (view.kind === 'fields') {
      const [field] = view.fields;
      expect(field.key).toBe('deep');
      expect(field.block).toBe(true);
      // However it is printed (indented or compact), the value is there and recognisable.
      expect(field.value.startsWith('[')).toBe(true);
      expect(field.value).not.toBe('[unrenderable value]');
    }
  });

  it('degrades to a bounded display when a value is nested beyond what can be printed', () => {
    const view = describeToolCall(rawToolCall('mcp__srv__tool', `{"deep":${nested(200_000)}}`));
    // Either the parser or the printer gives up; the call is still described.
    expect(['fields', 'raw']).toContain(view.kind);
    if (view.kind === 'fields') {
      // Changed with the bounded pretty-printing: the old placeholder text `[unrenderable value]`
      // (a bracketed string that looks like recorded content) is gone. A value the engine cannot
      // print is flagged `unrenderable` and carries no text; one it can print is shown compactly,
      // clipped to 256 KiB with the number of characters left out reported alongside.
      const [field] = view.fields;
      if (field.unrenderable) {
        expect(field.value).toBe('');
      } else {
        expect(field.value.length).toBeLessThanOrEqual(256 * 1024);
        expect(field.omitted ?? 0).toBeGreaterThan(0);
      }
      expect(field.value).not.toContain('[unrenderable value]');
    }
  });

  /**
   * Was QA-3: `JSON.stringify(value, null, 2)` makes its output grow with the SQUARE of
   * the nesting depth (each level is indented two more spaces). A 2 KB argument nested
   * 1,000 deep became a 2 MB string; a node may carry 256 tool calls, and 256 such calls
   * cost +540 MB and 1.3 s to render. Fixed by printing compactly first and indenting
   * only a value that is shallow and small (see `showValue` in toolCalls.ts).
   */
  it('does not multiply the size of a nested argument into megabytes of text', () => {
    const depth = 1000;
    const raw = `{"deep":${'['.repeat(depth)}${']'.repeat(depth)}}`;
    const view = describeToolCall(rawToolCall('mcp__srv__tool', raw));
    const shown = view.kind === 'fields' ? view.fields.reduce((sum, field) => sum + field.value.length, 0) : 0;
    // Pretty-printing may add indentation, but ten times the input is already generous.
    expect(shown, `${raw.length} bytes of arguments became ${shown} characters`).toBeLessThanOrEqual(raw.length * 10 + 64 * 1024);
    // In fact nothing is added at all: past 32 levels the value is printed compactly.
    expect(shown).toBeLessThanOrEqual(raw.length);
  });

  it('keeps 256 deeply nested tool calls cheap, in time and in text', () => {
    const raw = `{"deep":${'['.repeat(2000)}${']'.repeat(2000)}}`;
    let total = 0;
    const best = bestOf(2, () => {
      total = 0;
      for (let i = 0; i < 256; i++) {
        const view = describeToolCall(rawToolCall('mcp__srv__tool', raw));
        if (view.kind === 'fields') total += view.fields.reduce((sum, field) => sum + field.value.length, 0);
      }
    });
    expect(total).toBeLessThanOrEqual(256 * raw.length);
    expect(best).toBeLessThan(2000); // takes tens of milliseconds
  });

  it('survives the same value as an unused extra on a tool that is understood', () => {
    const view = describeToolCall(rawToolCall('Bash', `{"command":"ls","extra":${nested(2000)}}`));
    expect(view).toMatchObject({ kind: 'command', command: 'ls' });
    expect(view.extras).toHaveLength(1);
    expect(view.extras[0].key).toBe('extra');
  });

  it('shows nested objects as it does arrays', () => {
    const view = describeToolCall(rawToolCall('mcp__srv__tool', `{"deep":${'{"k":'.repeat(1500)}1${'}'.repeat(1500)}}`));
    expect(view.kind).toBe('fields');
    if (view.kind === 'fields') expect(view.fields[0].value).toContain('"k"');
  });
});

describe('toolTitle: odd names', () => {
  it('leaves any name it cannot make sense of exactly as recorded', () => {
    for (const name of ['mcp__server__tool\n', ' mcp__a__b', 'MCP__a__b', 'mcp__a_b__c', '‮Bash', 'a'.repeat(10_000)]) {
      const title = toolTitle(name);
      expect(typeof title).toBe('string');
      expect(title.length).toBeGreaterThan(0);
    }
    expect(toolTitle('MCP__a__b')).toBe('MCP__a__b');
    expect(toolTitle(' mcp__a__b')).toBe(' mcp__a__b');
    expect(toolTitle('mcp__a_b__c')).toBe('a_b · c');
  });

  it('keeps a hostile name as text for the caller to render as text', () => {
    expect(toolTitle('<script>alert(1)</script>')).toBe('<script>alert(1)</script>');
    expect(toolTitle('mcp__<b>x</b>__<i>y</i>')).toBe('<b>x</b> · <i>y</i>');
  });
});
