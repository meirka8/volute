import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  describeToolCall,
  isServerSideTool,
  parseToolArguments,
  ranProviderSide,
  toolOutcome,
  toolTitle,
  type ToolOutcome,
} from './toolCalls';
import { rawToolCall, serverToolCall, toolCall } from '../test/fixtures';

describe('parseToolArguments', () => {
  it('parses a JSON object, with real newlines in its strings', () => {
    const parsed = parseToolArguments('{"command":"cat <<EOF\\nhello\\nEOF","timeout":5000}');
    expect(parsed).toEqual({ kind: 'object', value: { command: 'cat <<EOF\nhello\nEOF', timeout: 5000 } });
  });

  it('recognises the exact truncation sentinel, in the key order the producer writes', () => {
    // serde_json writes object keys alphabetically.
    expect(parseToolArguments('{"bytes":100041,"cvc_truncated":true}')).toEqual({ kind: 'truncated', bytes: 100041 });
    // Detection is by parsing, so spacing and key order do not matter.
    expect(parseToolArguments('{"cvc_truncated":true,"bytes":131072}')).toEqual({ kind: 'truncated', bytes: 131072 });
    expect(parseToolArguments(' { "bytes" : 0 , "cvc_truncated" : true } ')).toEqual({ kind: 'truncated', bytes: 0 });
  });

  it.each([
    ['an extra key', '{"cvc_truncated":true,"bytes":10,"command":"ls"}'],
    ['a false flag', '{"cvc_truncated":false,"bytes":10}'],
    ['a string flag', '{"cvc_truncated":"true","bytes":10}'],
    ['a non-numeric size', '{"cvc_truncated":true,"bytes":"10"}'],
    ['a fractional size', '{"cvc_truncated":true,"bytes":1.5}'],
    ['a negative size', '{"cvc_truncated":true,"bytes":-1}'],
    ['a missing size', '{"cvc_truncated":true}'],
  ])('does not take %s for the sentinel', (_label, raw) => {
    expect(parseToolArguments(raw).kind).toBe('object');
  });

  it('returns invalid JSON verbatim', () => {
    expect(parseToolArguments('{"command": "ls')).toEqual({ kind: 'raw', text: '{"command": "ls' });
    expect(parseToolArguments('ls -la')).toEqual({ kind: 'raw', text: 'ls -la' });
  });

  it('returns arrays and primitives verbatim', () => {
    for (const raw of ['["a","b"]', '42', '"text"', 'true', 'null']) {
      expect(parseToolArguments(raw)).toEqual({ kind: 'raw', text: raw });
    }
  });

  it('returns empty or missing arguments as empty raw text', () => {
    expect(parseToolArguments('')).toEqual({ kind: 'raw', text: '' });
    expect(parseToolArguments('  \n')).toEqual({ kind: 'raw', text: '  \n' });
    expect(parseToolArguments(undefined)).toEqual({ kind: 'raw', text: '' });
    expect(parseToolArguments(null)).toEqual({ kind: 'raw', text: '' });
  });

  it('does not throw on absurdly deep nesting', () => {
    const deep = '['.repeat(200_000);
    expect(parseToolArguments(deep)).toEqual({ kind: 'raw', text: deep });
    const deepObject = `{"a":${'['.repeat(100_000)}${']'.repeat(100_000)}}`;
    expect(() => describeToolCall({ tool_name: 'Whatever', arguments: deepObject })).not.toThrow();
  });
});

describe('toolTitle', () => {
  it('shows MCP tools as server · tool', () => {
    expect(toolTitle('mcp__example_server__create_item')).toBe('example_server · create_item');
    expect(toolTitle('mcp__claude_ai_Linear__save_issue')).toBe('claude_ai_Linear · save_issue');
    expect(toolTitle('mcp__srv__tool__with__underscores')).toBe('srv · tool__with__underscores');
  });

  it('shows server-side tools as name (server-side)', () => {
    expect(toolTitle('server:web_search')).toBe('web_search (server-side)');
    expect(toolTitle('server:')).toBe('server:');
  });

  it('leaves every other name, and malformed MCP names, as recorded', () => {
    expect(toolTitle('Bash')).toBe('Bash');
    expect(toolTitle('mcp__')).toBe('mcp__');
    expect(toolTitle('mcp__only')).toBe('mcp__only');
    expect(toolTitle('mcp____tool')).toBe('mcp____tool');
    expect(toolTitle('mcp__server__')).toBe('mcp__server__');
    expect(toolTitle('')).toBe('Unknown tool');
  });
});

describe('describeToolCall', () => {
  describe('Bash', () => {
    it('keeps the command with real newlines, the description as the caption and options as chips', () => {
      const view = describeToolCall(
        toolCall('Bash', {
          command: "cat > /repo/run.sh <<'EOF'\n#!/bin/sh\necho hi\nEOF",
          description: 'Create the script',
          timeout: 120000,
          run_in_background: true,
        }),
      );
      expect(view).toEqual({
        kind: 'command',
        tool: 'Bash',
        title: 'Bash',
        caption: 'Create the script',
        command: "cat > /repo/run.sh <<'EOF'\n#!/bin/sh\necho hi\nEOF",
        chips: ['timeout 120s', 'runs in background'],
        extras: [],
      });
    });

    it('works with only a command', () => {
      expect(describeToolCall(toolCall('Bash', { command: 'ls' }))).toMatchObject({
        kind: 'command',
        caption: null,
        command: 'ls',
        chips: [],
        extras: [],
      });
    });

    it('formats sub-second and uneven timeouts in milliseconds', () => {
      const view = describeToolCall(toolCall('Bash', { command: 'ls', timeout: 1500 }));
      expect(view).toMatchObject({ chips: ['timeout 1500ms'] });
    });

    it('keeps fields it does not know in extras instead of dropping them', () => {
      const view = describeToolCall(toolCall('Bash', { command: 'ls', dangerouslyDisableSandbox: true }));
      expect(view.extras).toEqual([{ key: 'dangerouslyDisableSandbox', value: 'true', block: false }]);
    });

    it('keeps an optional field of the wrong type in extras', () => {
      const view = describeToolCall(toolCall('Bash', { command: 'ls', description: 5 }));
      expect(view).toMatchObject({ kind: 'command', caption: null });
      expect(view.extras).toEqual([{ key: 'description', value: '5', block: false }]);
    });

    it('falls back to rows when the command is missing or not a string', () => {
      expect(describeToolCall(toolCall('Bash', { description: 'no command' })).kind).toBe('fields');
      const view = describeToolCall(toolCall('Bash', { command: ['ls'] }));
      expect(view).toMatchObject({ kind: 'fields', fields: [{ key: 'command', value: '[\n  "ls"\n]', block: true }] });
    });
  });

  describe('Edit and MultiEdit', () => {
    it('describes an Edit as one edit with a replace-all chip', () => {
      const view = describeToolCall(
        toolCall('Edit', { file_path: '/repo/src/app.ts', old_string: 'a\nb', new_string: 'a\nc', replace_all: true }),
      );
      expect(view).toEqual({
        kind: 'edit',
        tool: 'Edit',
        title: 'Edit',
        filePath: '/repo/src/app.ts',
        edits: [{ oldText: 'a\nb', newText: 'a\nc', chips: [] }],
        chips: ['replace all occurrences'],
        extras: [],
      });
    });

    it('has no chip unless replace_all is true', () => {
      const view = describeToolCall(
        toolCall('Edit', { file_path: '/repo/a.ts', old_string: 'x', new_string: 'y', replace_all: false }),
      );
      expect(view).toMatchObject({ chips: [], extras: [] });
    });

    it('allows an empty old_string or new_string (insertion, deletion)', () => {
      const view = describeToolCall(toolCall('Edit', { file_path: '/repo/a.ts', old_string: '', new_string: 'new' }));
      expect(view).toMatchObject({ kind: 'edit', edits: [{ oldText: '', newText: 'new' }] });
    });

    it('describes a MultiEdit with per-edit options', () => {
      const view = describeToolCall(
        toolCall('MultiEdit', {
          file_path: '/repo/src/app.ts',
          edits: [
            { old_string: 'one', new_string: '1' },
            { old_string: 'two', new_string: '2', replace_all: true },
          ],
        }),
      );
      expect(view).toEqual({
        kind: 'edit',
        tool: 'MultiEdit',
        title: 'MultiEdit',
        filePath: '/repo/src/app.ts',
        edits: [
          { oldText: 'one', newText: '1', chips: [] },
          { oldText: 'two', newText: '2', chips: ['replace all occurrences'] },
        ],
        chips: [],
        extras: [],
      });
    });

    it.each([
      ['an edit that is not an object', { file_path: '/repo/a.ts', edits: ['x'] }],
      ['an edit with a missing string', { file_path: '/repo/a.ts', edits: [{ old_string: 'x' }] }],
      ['an edit with an unknown key', { file_path: '/repo/a.ts', edits: [{ old_string: 'x', new_string: 'y', note: 'z' }] }],
      ['an edit with a mistyped flag', { file_path: '/repo/a.ts', edits: [{ old_string: 'x', new_string: 'y', replace_all: 'yes' }] }],
      ['no edits', { file_path: '/repo/a.ts', edits: [] }],
      ['no file path', { edits: [{ old_string: 'x', new_string: 'y' }] }],
    ])('falls back to rows for %s, showing everything recorded', (_label, input) => {
      const view = describeToolCall(toolCall('MultiEdit', input));
      expect(view.kind).toBe('fields');
      if (view.kind === 'fields') expect(view.fields.map((f) => f.key)).toEqual(Object.keys(input));
    });
  });

  describe('Write and NotebookEdit', () => {
    it('describes Write as new file content', () => {
      expect(describeToolCall(toolCall('Write', { file_path: '/repo/README.md', content: '# Title\n\nBody\n' }))).toEqual({
        kind: 'file_content',
        tool: 'Write',
        title: 'Write',
        filePath: '/repo/README.md',
        content: '# Title\n\nBody\n',
        chips: [],
        extras: [],
      });
    });

    it('describes NotebookEdit with cell chips', () => {
      const view = describeToolCall(
        toolCall('NotebookEdit', {
          notebook_path: '/repo/analysis.ipynb',
          new_source: 'print(1)',
          cell_id: 'c3f2',
          cell_type: 'code',
          edit_mode: 'insert',
        }),
      );
      expect(view).toMatchObject({
        kind: 'file_content',
        filePath: '/repo/analysis.ipynb',
        content: 'print(1)',
        chips: ['cell c3f2', 'type code', 'mode insert'],
        extras: [],
      });
    });

    it('falls back to rows when required fields are mistyped', () => {
      expect(describeToolCall(toolCall('Write', { file_path: '/repo/a', content: 7 })).kind).toBe('fields');
      expect(describeToolCall(toolCall('NotebookEdit', { notebook_path: '/repo/a.ipynb' })).kind).toBe('fields');
    });
  });

  describe('Read, Glob, Grep', () => {
    it('describes Read with the lines selected by offset and limit', () => {
      expect(describeToolCall(toolCall('Read', { file_path: '/repo/a.ts', offset: 10, limit: 50 }))).toEqual({
        kind: 'lookup',
        tool: 'Read',
        title: 'Read',
        label: 'file',
        value: '/repo/a.ts',
        detail: null,
        chips: ['lines 10–59'],
        extras: [],
      });
    });

    it('handles partial and absent ranges', () => {
      const chipsOf = (input: Record<string, unknown>) => {
        const view = describeToolCall(toolCall('Read', { file_path: '/repo/a.ts', ...input }));
        return view.kind === 'lookup' ? view.chips : null;
      };
      expect(chipsOf({})).toEqual([]);
      expect(chipsOf({ offset: 120 })).toEqual(['from line 120']);
      expect(chipsOf({ limit: 40 })).toEqual(['lines 1–40']);
      expect(chipsOf({ offset: 0, limit: 40 })).toEqual(['lines 1–40']);
      expect(chipsOf({ pages: '1-3' })).toEqual(['pages 1-3']);
    });

    it('describes Glob with its directory', () => {
      expect(describeToolCall(toolCall('Glob', { pattern: '**/*.ts', path: '/repo/src' }))).toMatchObject({
        kind: 'lookup',
        label: 'pattern',
        value: '**/*.ts',
        chips: ['in /repo/src'],
      });
      expect(describeToolCall(toolCall('Glob', { pattern: '*.md' }))).toMatchObject({ chips: [] });
    });

    it('describes Grep with its options as chips', () => {
      const view = describeToolCall(
        toolCall('Grep', {
          pattern: 'fetchWithRetry',
          path: '/repo/src',
          glob: '*.ts',
          type: 'ts',
          output_mode: 'content',
          '-i': true,
          '-n': true,
          '-C': 2,
          head_limit: 20,
          multiline: false,
        }),
      );
      expect(view).toMatchObject({
        kind: 'lookup',
        label: 'pattern',
        value: 'fetchWithRetry',
        chips: ['in /repo/src', 'glob *.ts', 'type ts', 'output content', 'ignore case', 'line numbers', '-C 2', 'head_limit 20'],
        extras: [],
      });
    });
  });

  describe('Agent, Task, web and todo tools', () => {
    it('describes Agent and the legacy Task name alike', () => {
      for (const name of ['Agent', 'Task']) {
        const view = describeToolCall(
          toolCall(name, { description: 'Find usages', subagent_type: 'Explore', prompt: 'Search the repo for X.\nReport paths.' }),
        );
        expect(view).toEqual({
          kind: 'delegation',
          tool: name,
          title: name,
          caption: 'Find usages',
          prompt: 'Search the repo for X.\nReport paths.',
          chips: ['subagent Explore'],
          extras: [],
        });
      }
    });

    it('keeps the URL of a WebFetch as plain text, with the prompt as detail', () => {
      const view = describeToolCall(toolCall('WebFetch', { url: 'https://example.com/docs?q=1', prompt: 'Summarise the page' }));
      expect(view).toEqual({
        kind: 'lookup',
        tool: 'WebFetch',
        title: 'WebFetch',
        label: 'URL',
        value: 'https://example.com/docs?q=1',
        detail: 'Summarise the page',
        chips: [],
        extras: [],
      });
    });

    it('describes WebSearch and keeps its domain lists', () => {
      const view = describeToolCall(toolCall('WebSearch', { query: 'rust uuid v8', allowed_domains: ['example.com'] }));
      expect(view).toMatchObject({ kind: 'lookup', label: 'query', value: 'rust uuid v8' });
      expect(view.extras).toEqual([{ key: 'allowed_domains', value: '[\n  "example.com"\n]', block: true }]);
    });

    it('describes TodoWrite as a checklist', () => {
      const view = describeToolCall(
        toolCall('TodoWrite', {
          todos: [
            { content: 'Write tests', status: 'completed', activeForm: 'Writing tests' },
            { content: 'Run lint', status: 'in_progress', activeForm: 'Running lint' },
            { content: 'Ship', status: 'pending' },
          ],
        }),
      );
      expect(view).toEqual({
        kind: 'todos',
        tool: 'TodoWrite',
        title: 'TodoWrite',
        todos: [
          { content: 'Write tests', status: 'completed', activeForm: 'Writing tests' },
          { content: 'Run lint', status: 'in_progress', activeForm: 'Running lint' },
          { content: 'Ship', status: 'pending' },
        ],
        extras: [],
      });
    });

    it('drops an activeForm that only repeats the content, and keeps one that adds wording', () => {
      const view = describeToolCall(
        toolCall('TodoWrite', {
          todos: [
            { content: 'Run lint', status: 'pending', activeForm: 'Run lint' },
            { content: 'Run lint', status: 'pending', activeForm: 'Running lint' },
          ],
        }),
      );
      expect(view.kind === 'todos' && view.todos).toEqual([
        { content: 'Run lint', status: 'pending' },
        { content: 'Run lint', status: 'pending', activeForm: 'Running lint' },
      ]);
    });

    it('falls back to rows for a malformed TodoWrite', () => {
      expect(describeToolCall(toolCall('TodoWrite', { todos: [{ content: 'x' }] })).kind).toBe('fields');
      expect(describeToolCall(toolCall('TodoWrite', { todos: 'nope' })).kind).toBe('fields');
      expect(describeToolCall(toolCall('TodoWrite', { todos: [{ content: 'x', status: 'pending', owner: 'me' }] })).kind).toBe('fields');
    });
  });

  describe('everything else', () => {
    it('shows MCP tools as rows, with nested values as 2-space JSON', () => {
      const view = describeToolCall(
        toolCall('mcp__example_server__create_item', {
          title: 'Example',
          labels: ['a', 'b'],
          meta: { nested: { ok: true } },
          count: 3,
          flag: false,
          nothing: null,
          body: 'line one\nline two',
        }),
      );
      expect(view).toEqual({
        kind: 'fields',
        tool: 'mcp__example_server__create_item',
        title: 'example_server · create_item',
        fields: [
          { key: 'title', value: 'Example', block: false },
          { key: 'labels', value: '[\n  "a",\n  "b"\n]', block: true },
          { key: 'meta', value: '{\n  "nested": {\n    "ok": true\n  }\n}', block: true },
          { key: 'count', value: '3', block: false },
          { key: 'flag', value: 'false', block: false },
          { key: 'nothing', value: 'null', block: false },
          { key: 'body', value: 'line one\nline two', block: true },
        ],
        extras: [],
      });
    });

    it('treats long single-line values as blocks', () => {
      const view = describeToolCall(toolCall('Skill', { skill: 'x'.repeat(200) }));
      expect(view).toMatchObject({ kind: 'fields', fields: [{ key: 'skill', block: true }] });
    });

    it('shows an unknown tool with no input as no rows', () => {
      expect(describeToolCall(toolCall('KillShell', {}))).toMatchObject({ kind: 'fields', fields: [] });
    });

    it('does not confuse tool names with object prototype members', () => {
      for (const name of ['constructor', 'toString', '__proto__', 'hasOwnProperty']) {
        expect(describeToolCall(toolCall(name, { a: 1 }))).toMatchObject({ kind: 'fields', tool: name });
      }
    });

    it('handles an own __proto__ key in the input', () => {
      const view = describeToolCall(rawToolCall('Thing', '{"__proto__":{"polluted":true},"a":1}'));
      expect(view).toMatchObject({ kind: 'fields' });
      expect(({} as Record<string, unknown>).polluted).toBeUndefined();
    });
  });

  describe('server-side tools', () => {
    it('reads a server-side web search like WebSearch, named as server-side', () => {
      expect(describeToolCall(rawToolCall('server:web_search', '{"query":"cvc wire format"}'))).toEqual({
        kind: 'lookup',
        tool: 'server:web_search',
        title: 'web_search (server-side)',
        label: 'query',
        value: 'cvc wire format',
        detail: null,
        chips: [],
        extras: [],
      });
    });

    it('reads a server-side web fetch like WebFetch', () => {
      expect(
        describeToolCall(rawToolCall('server:web_fetch', '{"url":"https://example.com/a","prompt":"Summarise"}')),
      ).toMatchObject({ kind: 'lookup', label: 'URL', value: 'https://example.com/a', detail: 'Summarise' });
    });

    it('shows any other server-side tool as rows', () => {
      expect(describeToolCall(rawToolCall('server:code_execution', '{"code":"print(1)"}'))).toMatchObject({
        kind: 'fields',
        title: 'code_execution (server-side)',
        fields: [{ key: 'code', value: 'print(1)', block: false }],
      });
    });
  });

  describe('capture limits and bad data', () => {
    it('reports the truncation sentinel with its size', () => {
      expect(describeToolCall(rawToolCall('Write', '{"cvc_truncated":true,"bytes":98304}'))).toEqual({
        kind: 'truncated',
        tool: 'Write',
        title: 'Write',
        bytes: 98304,
        extras: [],
      });
    });

    it('passes non-JSON arguments through as raw text, whatever the tool', () => {
      expect(describeToolCall(rawToolCall('Bash', 'ls -la'))).toMatchObject({ kind: 'raw', text: 'ls -la' });
      expect(describeToolCall(rawToolCall('Bash', '["ls"]'))).toMatchObject({ kind: 'raw', text: '["ls"]' });
    });

    it('reports a tool call without an input (JSON null) or with empty arguments as having none', () => {
      for (const args of ['null', ' null\n', '', '  ']) {
        expect(describeToolCall(rawToolCall('mcp__example_server__list_items', args))).toEqual({
          kind: 'none',
          tool: 'mcp__example_server__list_items',
          title: 'example_server · list_items',
          extras: [],
        });
      }
    });

    it('survives an execution that is not an object at all (unvalidated legacy data)', () => {
      for (const junk of [null, undefined, 'bash', 42]) {
        const exec = junk as unknown as Parameters<typeof describeToolCall>[0];
        expect(describeToolCall(exec)).toMatchObject({ kind: 'none', title: 'Unknown tool' });
        // No recorded status at all: how the call ended is unknown, not a success.
        expect(toolOutcome(exec as unknown as Parameters<typeof toolOutcome>[0])).toBe('unknown');
      }
    });

    it('survives a missing tool name or arguments', () => {
      const view = describeToolCall({ tool_name: undefined as unknown as string, arguments: undefined as unknown as string });
      expect(view).toMatchObject({ kind: 'none', title: 'Unknown tool' });
    });

    it('leaves redaction tokens and truncation suffixes in text untouched', () => {
      const token = '[CVC_REDACTED:v1:secret_assignment:11111111-2222-4333-8444-555555555555]';
      const command = `curl -H "Authorization: ${token}" https://example.com\n…[cvc: truncated 4096 bytes]`;
      expect(describeToolCall(toolCall('Bash', { command }))).toMatchObject({ kind: 'command', command });
    });
  });
});

describe('toolOutcome', () => {
  it('reports success and failure as recorded', () => {
    expect(toolOutcome(toolCall('Bash', {}))).toBe('success');
    expect(toolOutcome(toolCall('Bash', {}, 'failure'))).toBe('failure');
    expect(toolOutcome(toolCall('mcp__example_server__tool', {}, 'failure'))).toBe('failure');
  });

  it('does not take a recorded failure of a server-side tool for an outcome', () => {
    // The 0.7.0 harness records every server-side call as failed (its result
    // never arrives as a user-side tool_result).
    expect(toolOutcome(toolCall('server:web_search', {}, 'failure'))).toBe('unrecorded');
    expect(toolOutcome(toolCall('server:web_fetch', {}, 'failure'))).toBe('unrecorded');
  });

  it('believes a server-side tool recorded as a success', () => {
    expect(toolOutcome(toolCall('server:web_search', {}, 'success'))).toBe('success');
  });

  it('never takes an unexpected status for a success or a failure', () => {
    // Legacy, unvalidated data: shown as "status not recorded", counted as neither.
    for (const status of ['weird', '', 'SUCCESS', 'Failure', undefined, null, 0, true]) {
      expect(toolOutcome({ tool_name: 'Bash', status: status as unknown as 'success' }), String(status)).toBe('unknown');
    }
  });
});

describe('isServerSideTool', () => {
  it('matches the server: prefix followed by a name', () => {
    expect(isServerSideTool('server:web_search')).toBe(true);
    expect(isServerSideTool('server:')).toBe(false);
    expect(isServerSideTool('Server:web_search')).toBe(false);
    expect(isServerSideTool('mcp__server__tool')).toBe(false);
    expect(isServerSideTool('Bash')).toBe(false);
  });
});

describe('toolOutcome: the exact shape cvc 0.7.0 writes for a server-side call', () => {
  const serverCall = (over: Partial<ReturnType<typeof toolCall>>) => ({ ...toolCall('server:web_search', {}, 'failure'), ...over });

  it('needs the server: name AND the native protocol to call a recorded failure unrecorded', () => {
    expect(toolOutcome(serverCall({}))).toBe('unrecorded');
    // Same name under another protocol is not what the harness writes: it is believed.
    expect(toolOutcome(serverCall({ tool_protocol: 'mcp' }))).toBe('failure');
    expect(toolOutcome(serverCall({ tool_protocol: 'other' }))).toBe('failure');
    expect(toolOutcome(serverCall({ tool_protocol: '' }))).toBe('failure');
    expect(toolOutcome({ tool_name: 'server:web_search', status: 'failure' })).toBe('failure'); // protocol absent
  });

  it('does not extend to a name that merely contains or resembles the prefix', () => {
    for (const name of ['Server:web_search', ' server:web_search', 'server:', 'serverless:x', 'mcp__server__x']) {
      expect(toolOutcome({ ...toolCall(name, {}, 'failure') }), name).toBe('failure');
    }
  });

  it('believes a success whatever the name and protocol', () => {
    expect(toolOutcome(toolCall('server:web_search', {}, 'success'))).toBe('success');
    expect(toolOutcome(serverCall({ status: 'success', tool_protocol: 'mcp' }))).toBe('success');
  });
});

describe('toolOutcome: protocol x status x name', () => {
  // Every combination that matters, written out. A server-side call is recorded in one of two shapes: cvc 0.7.0 wrote
  // protocol "native" and status "failure" whatever happened, which is no outcome; a harness that keeps the outcome
  // writes protocol "server" and the status its result block reported, which is believed.
  const rows: Array<[name: string, protocol: string | undefined, status: string, expected: ToolOutcome]> = [
    // The exact shape 0.7.0 wrote, for every kind of server-side tool: not an outcome.
    ['server:web_search', 'native', 'failure', 'unrecorded'],
    ['server:web_fetch', 'native', 'failure', 'unrecorded'],
    ['server:code_execution', 'native', 'failure', 'unrecorded'],
    ['server:bash_code_execution', 'native', 'failure', 'unrecorded'],
    ['server:text_editor_code_execution', 'native', 'failure', 'unrecorded'],
    // The outcome a harness that keeps it writes: believed, a failure included.
    ['server:web_search', 'server', 'success', 'success'],
    ['server:web_search', 'server', 'failure', 'failure'],
    ['server:web_fetch', 'server', 'success', 'success'],
    ['server:web_fetch', 'server', 'failure', 'failure'],
    ['server:code_execution', 'server', 'success', 'success'],
    ['server:code_execution', 'server', 'failure', 'failure'],
    ['server:bash_code_execution', 'server', 'success', 'success'],
    ['server:bash_code_execution', 'server', 'failure', 'failure'],
    ['server:text_editor_code_execution', 'server', 'success', 'success'],
    ['server:text_editor_code_execution', 'server', 'failure', 'failure'],
    // A success is a success under any protocol, even one 0.7.0 never wrote.
    ['server:web_search', 'native', 'success', 'success'],
    ['server:web_search', 'mcp', 'success', 'success'],
    // The protocol decides, not the name: "server" is believed on any name, and a bare "server:" is no server-side name.
    ['Bash', 'server', 'success', 'success'],
    ['Bash', 'server', 'failure', 'failure'],
    ['server:', 'server', 'failure', 'failure'],
    ['server:', 'native', 'failure', 'failure'],
    // Tools the harness and MCP servers run: as recorded.
    ['Bash', 'native', 'success', 'success'],
    ['Bash', 'native', 'failure', 'failure'],
    ['mcp__srv__tool', 'mcp', 'success', 'success'],
    ['mcp__srv__tool', 'mcp', 'failure', 'failure'],
    // A server-side name that is not the 0.7.0 shape: a failure is a failure (no protocol, an empty or other one, another case).
    ['server:web_search', 'mcp', 'failure', 'failure'],
    ['server:web_search', 'other', 'failure', 'failure'],
    ['server:web_search', '', 'failure', 'failure'],
    ['server:web_search', undefined, 'failure', 'failure'],
    ['server:web_search', 'Native', 'failure', 'failure'],
    ['server:web_search', 'SERVER', 'failure', 'failure'],
    ['Server:web_search', 'native', 'failure', 'failure'],
    [' server:web_search', 'native', 'failure', 'failure'],
    ['mcp__server__web_search', 'native', 'failure', 'failure'],
  ];

  it.each(rows)('%s recorded as protocol %s with status %s is %s', (name, protocol, status, expected) => {
    expect(toolOutcome({ tool_name: name, tool_protocol: protocol, status: status as 'success' | 'failure' })).toBe(expected);
  });

  it('is "unrecorded" for exactly one row of the table: a server-side name, native, failure', () => {
    expect(rows.filter(([, , , expected]) => expected === 'unrecorded').map(([name, protocol, status]) => [protocol, status, name.startsWith('server:')])).toEqual(
      Array.from({ length: 5 }, () => ['native', 'failure', true]),
    );
  });

  it.each(['native', 'server', 'mcp', 'other', '', undefined])(
    'keeps any status that is neither "success" nor "failure" unknown, for protocol %s and for a server-side name or not',
    (protocol) => {
      for (const name of ['server:web_search', 'Bash']) {
        for (const status of ['pending', '', 'SUCCESS', 'Failure', 'unknown', undefined, null, 0, true]) {
          expect(
            toolOutcome({ tool_name: name, tool_protocol: protocol, status: status as unknown as 'success' }),
            `${name} / ${String(protocol)} / ${String(status)}`,
          ).toBe('unknown');
        }
      }
    },
  );

  it('reads the same from the fixtures that stand for the two recordings', () => {
    expect(toolOutcome(toolCall('server:web_search', {}, 'failure'))).toBe('unrecorded'); // as 0.7.0 wrote it
    expect(toolOutcome(serverToolCall('web_search', {}, 'failure'))).toBe('failure');
    expect(toolOutcome(serverToolCall('web_search', {}, 'success'))).toBe('success');
    expect(toolOutcome(serverToolCall('web_search', {}))).toBe('success');
  });
});

describe('ranProviderSide', () => {
  it('is true only for the protocol "server"', () => {
    expect(ranProviderSide(serverToolCall('web_search', {}))).toBe(true);
    expect(ranProviderSide({ tool_protocol: 'server' })).toBe(true);
    for (const protocol of ['native', 'mcp', 'Server', 'SERVER', ' server', 'server ', 'servers', '', undefined]) {
      expect(ranProviderSide({ tool_protocol: protocol }), String(protocol)).toBe(false);
    }
    // A server-side NAME is not enough: that is what 0.7.0 wrote under the protocol "native".
    expect(ranProviderSide(toolCall('server:web_search', {}))).toBe(false);
  });

  it('is false for something that is not a tool execution at all', () => {
    for (const junk of [null, undefined]) expect(ranProviderSide(junk)).toBe(false);
    expect(ranProviderSide({} as { tool_protocol?: string })).toBe(false);
  });
});

describe('describeToolCall: bounded values', () => {
  const field = (value: unknown, tool = 'mcp__srv__tool') => {
    const view = describeToolCall(rawToolCall(tool, JSON.stringify({ v: value })));
    if (view.kind !== 'fields') throw new Error(`expected rows, got ${view.kind}`);
    return view.fields[0];
  };
  const nest = (depth: number, inner = '1') => `${'['.repeat(depth)}${inner}${']'.repeat(depth)}`;

  it('indents a small, shallow value by two spaces', () => {
    expect(field({ a: [1, { b: 2 }] }).value).toBe('{\n  "a": [\n    1,\n    {\n      "b": 2\n    }\n  ]\n}');
  });

  it('indents a value nested exactly 32 deep and prints one nested 33 deep compactly', () => {
    const at32 = field(JSON.parse(nest(32)) as unknown).value;
    expect(at32.split('\n').length).toBeGreaterThan(30); // indented: one level per line
    const at33 = field(JSON.parse(nest(33)) as unknown).value;
    expect(at33).toBe(nest(33)); // compact
    expect(at33).not.toContain('\n');
  });

  it('counts a container as one level whatever it holds, and measures objects as it measures arrays', () => {
    const objects = '{"k":'.repeat(33) + '1' + '}'.repeat(33);
    expect(field(JSON.parse(objects) as unknown).value).toBe(objects);
    const mixed = '[{"k":'.repeat(16) + '1' + '}]'.repeat(16); // 32 levels
    expect(field(JSON.parse(mixed) as unknown).value).toContain('\n');
  });

  // Changed with the indent budget: it used to be "compact form over 16 KiB"; it is now
  // "compact length x (depth + 2) over 64 KiB", which depends on how deep the value is.
  it('indents a shallow value only while its indented size is within 64 KiB, and prints it compactly past that', () => {
    const flat = (count: number) => Array.from({ length: count }, () => 1); // "[1,1,...]": 2n + 1 characters, one level deep
    expect(JSON.stringify(flat(10_922)).length).toBe(21_845);
    expect(field(flat(10_922)).value).toContain('\n'); // 21,845 x (1 + 2) = 65,535
    expect(JSON.stringify(flat(10_923)).length).toBe(21_847);
    expect(field(flat(10_923)).value).toBe(JSON.stringify(flat(10_923))); // 21,847 x 3 = 65,541
  });

  it('lets the depth decide: the same compact length is indented when shallow and compact when deep', () => {
    const wrapped = (levels: number, count: number) => {
      let value: unknown = Array.from({ length: count }, () => 1);
      for (let level = 1; level < levels; level++) value = [value];
      return value;
    };
    // About 4 KB compact. Eight levels: 4,000 x 10 = 40 KB, indented. Thirty-two: 4,000 x 34 = 136 KB, compact.
    expect(JSON.stringify(wrapped(8, 1000)).length).toBeLessThan(2100 + 16);
    expect(field(wrapped(8, 1000)).value).toContain('\n');
    const deep = wrapped(32, 1000);
    expect(JSON.stringify(deep, null, 2).length).toBeGreaterThan(64 * 1024);
    expect(field(deep).value).toBe(JSON.stringify(deep));
  });

  it('never indents a value past 64 KiB, whatever its shape', () => {
    // Deterministic xorshift, so a failure is reproducible.
    let state = 0x9e3779b9;
    const next = () => {
      state ^= state << 13;
      state ^= state >>> 17;
      state ^= state << 5;
      return (state >>> 0) / 0x100000000;
    };
    const depthOf = (value: unknown): number =>
      value === null || typeof value !== 'object'
        ? 0
        : 1 + Math.max(0, ...(Array.isArray(value) ? value : Object.values(value)).map(depthOf));
    // At most `budget` nodes per value, so a deep, wide draw cannot grow without end.
    const make = (levels: number, wide: number, budget: { left: number }): unknown => {
      if (levels === 0 || budget.left <= 0 || next() < 0.1) return next() < 0.5 ? 1 : 'x';
      const count = Math.min(budget.left, Math.floor(next() * wide));
      budget.left -= count;
      return next() < 0.5
        ? Array.from({ length: count }, () => make(levels - 1, wide, budget))
        : Object.fromEntries(Array.from({ length: count }, (_, i) => [`k${i}`, make(levels - 1, wide, budget)]));
    };
    let indented = 0;
    for (let run = 0; run < 3000; run++) {
      const value = make(1 + Math.floor(next() * 14), 2 + Math.floor(next() * 7), { left: 400 });
      if (value === null || typeof value !== 'object') continue;
      const shown = field(value);
      if (!shown.value.includes('\n')) continue; // printed compactly
      indented++;
      expect(shown.value.length, `depth ${depthOf(value)}, compact ${JSON.stringify(value).length}`).toBeLessThanOrEqual(64 * 1024);
      expect(shown.omitted).toBeUndefined();
    }
    expect(indented).toBeGreaterThan(100); // the sweep did exercise the indented case
  });

  it('does not multiply a deeply nested value into megabytes', () => {
    const raw = `{"deep":${nest(1000, '')}}`;
    const view = describeToolCall(rawToolCall('mcp__srv__tool', raw));
    const shown = view.kind === 'fields' ? view.fields[0].value.length : Infinity;
    expect(shown).toBeLessThanOrEqual(raw.length);
  });

  // Changed with the indent budget: this value (32 levels, 15 KB compact) used to be indented to
  // about 500 KB and then clipped at 256 KiB; it is now printed compactly and nothing is clipped.
  it('prints a deep, wide value compactly instead of indenting it to half a megabyte', () => {
    const wide = JSON.parse(`${'['.repeat(31)}${Array.from({ length: 7000 }, () => 1).join(',')}${']'.repeat(31)}`) as unknown;
    expect(JSON.stringify(wide, null, 2).length).toBeGreaterThan(256 * 1024); // what indenting it would have cost
    const shown = field(wide);
    expect(shown.value).toBe(JSON.stringify(wide));
    expect(shown.omitted).toBeUndefined();
  });

  it('still clips a compact value that is over the limit, with the count left out', () => {
    // A value too wide to indent, and itself longer than the clip: shown compactly, cut, and counted.
    const wide = Array.from({ length: 140_000 }, () => 1);
    const compact = JSON.stringify(wide);
    expect(compact.length).toBeGreaterThan(256 * 1024);
    const shown = field(wide);
    expect(shown.value).toBe(compact.slice(0, 256 * 1024));
    expect(shown.omitted).toBe(compact.length - 256 * 1024);
  });

  it('clips a string at 256 KiB and reports how much was left out, as a number not as text', () => {
    const big = 'x'.repeat(300 * 1024);
    const shown = field(big);
    expect(shown.value).toHaveLength(256 * 1024);
    expect(shown.omitted).toBe(44 * 1024);
    expect(shown.value).not.toContain('[');
    expect(shown.value).not.toContain('not shown'); // the note is the caller's, drawn outside the text
  });

  it('adds nothing to a value that fits', () => {
    expect(field('x'.repeat(256 * 1024))).toEqual({ key: 'v', value: 'x'.repeat(256 * 1024), block: true });
    expect(field('short')).toEqual({ key: 'v', value: 'short', block: false });
  });

  it('does not cut a surrogate pair in half', () => {
    const text = `${'a'.repeat(256 * 1024 - 1)}😀tail`; // the emoji straddles the limit
    const shown = field(text);
    const last = shown.value.charCodeAt(shown.value.length - 1);
    expect(last >= 0xd800 && last <= 0xdbff).toBe(false);
    expect(shown.value.length + (shown.omitted ?? 0)).toBe(text.length);
  });

  describe('a value nested too deeply for the engine to print', () => {
    afterEach(() => vi.restoreAllMocks());

    /** What JSON.stringify does on an engine whose stack is too small for the nesting. */
    function overflowOnDeepValues() {
      const original = JSON.stringify;
      vi.spyOn(JSON, 'stringify').mockImplementation(((value: unknown, ...rest: [never?, never?]) => {
        let depth = 0;
        for (let current = value; Array.isArray(current); current = current[0]) depth++;
        if (depth > 50) throw new RangeError('Maximum call stack size exceeded');
        return original(value, ...rest);
      }) as typeof JSON.stringify);
    }

    it('is unrenderable: no text, no count, and the card is still described', () => {
      const raw = `{"deep":${nest(2000, '')},"after":"still here"}`;
      const view = describeToolCall(rawToolCall('mcp__srv__tool', raw));
      overflowOnDeepValues();
      const again = describeToolCall(rawToolCall('mcp__srv__tool', raw));
      expect(view.kind).toBe('fields');
      expect(again.kind === 'fields' && again.fields).toEqual([
        { key: 'deep', value: '', block: false, unrenderable: true },
        { key: 'after', value: 'still here', block: false },
      ]);
    });

    it('is unrenderable among the extras of a tool that is understood', () => {
      overflowOnDeepValues();
      const view = describeToolCall(rawToolCall('Bash', `{"command":"ls","note":${nest(2000, '')}}`));
      expect(view).toMatchObject({ kind: 'command', command: 'ls' });
      expect(view.extras).toEqual([{ key: 'note', value: '', block: false, unrenderable: true }]);
    });

    it('is unrenderable as the arguments of a call too', () => {
      overflowOnDeepValues();
      let deep: unknown = 1;
      for (let i = 0; i < 100; i++) deep = [deep];
      expect(describeToolCall({ tool_name: 'Bash', arguments: deep as unknown as string })).toMatchObject({
        kind: 'raw',
        text: '',
        unrenderable: true,
      });
    });
  });

  it('keeps whatever the engine does with absurd nesting bounded: unrenderable, or compact and clipped', () => {
    // 200,000 levels. Engines differ: some overflow the stack (unrenderable), some print it.
    const view = describeToolCall(rawToolCall('mcp__srv__tool', `{"deep":${nest(200_000, '')}}`));
    expect(view.kind).toBe('fields');
    if (view.kind !== 'fields') return;
    const [deep] = view.fields;
    if (deep.unrenderable) {
      expect(deep).toEqual({ key: 'deep', value: '', block: false, unrenderable: true });
    } else {
      expect(deep.value.length).toBeLessThanOrEqual(256 * 1024);
      expect(deep.value.startsWith('[')).toBe(true);
      expect(deep.value).not.toContain('\n'); // compact
      expect(deep.omitted).toBe(400_000 - deep.value.length);
    }
  });

  it('clips the extras of a tool that is understood like any other value', () => {
    const view = describeToolCall(rawToolCall('Bash', `{"command":"ls","big":"${'y'.repeat(300 * 1024)}"}`));
    expect(view).toMatchObject({ kind: 'command', command: 'ls' });
    expect(view.extras.find((f) => f.key === 'big')).toMatchObject({ omitted: 44 * 1024 });
  });
});

describe('describeToolCall: what a field shows is worked out when it is read', () => {
  afterEach(() => vi.restoreAllMocks());

  /** A call with many fields whose values need printing (they are objects, not text). */
  const manyFields = (count: number) =>
    rawToolCall('mcp__srv__tool', JSON.stringify(Object.fromEntries(Array.from({ length: count }, (_, i) => [`k${i}`, { nested: [i, { more: i }] }]))));

  it('prints no value before one is read, and only the values that are read', () => {
    const call = manyFields(5000);
    const stringify = vi.spyOn(JSON, 'stringify');
    const view = describeToolCall(call);
    expect(view.kind === 'fields' && view.fields).toHaveLength(5000);
    expect(stringify).not.toHaveBeenCalled();

    if (view.kind !== 'fields') return;
    for (const field of view.fields.slice(0, 25)) void field.value;
    const reads = stringify.mock.calls.length;
    expect(reads).toBeGreaterThanOrEqual(25);
    expect(reads).toBeLessThanOrEqual(50); // compact, then indented, for each of the 25
  });

  it('works a field out once, however often and in whatever order its parts are read', () => {
    const view = describeToolCall(manyFields(3));
    if (view.kind !== 'fields') throw new Error('expected rows');
    const [first] = view.fields;
    const stringify = vi.spyOn(JSON, 'stringify');
    expect(first.block).toBe(true); // reading a part other than the value first is fine
    const text = first.value;
    const calls = stringify.mock.calls.length;
    expect(calls).toBeGreaterThan(0);
    for (let read = 0; read < 5; read++) {
      expect(first.value).toBe(text);
      expect(first.block).toBe(true);
      expect(first.omitted).toBeUndefined();
      expect(first.unrenderable).toBeUndefined();
    }
    expect(stringify.mock.calls.length).toBe(calls);
  });

  it('reads like a plain record: serialising a field gives its key, value and block, and what was cut or unprintable only when it was', () => {
    const view = describeToolCall(rawToolCall('mcp__srv__tool', JSON.stringify({ a: { b: 1 }, big: 'x'.repeat(300 * 1024) })));
    if (view.kind !== 'fields') throw new Error('expected rows');
    expect(JSON.parse(JSON.stringify(view.fields[0]))).toEqual({ key: 'a', value: '{\n  "b": 1\n}', block: true });
    const big = JSON.parse(JSON.stringify(view.fields[1])) as Record<string, unknown>;
    expect(Object.keys(big).sort()).toEqual(['block', 'key', 'omitted', 'value']);
    expect(big.omitted).toBe(44 * 1024);
  });

  it('does the same work for the extras of a tool that is understood', () => {
    const stringify = vi.spyOn(JSON, 'stringify');
    const view = describeToolCall(rawToolCall('Bash', JSON.stringify({ command: 'ls', ...Object.fromEntries(Array.from({ length: 500 }, (_, i) => [`x${i}`, [i]])) })));
    stringify.mockClear();
    expect(view.extras).toHaveLength(500);
    expect(stringify).not.toHaveBeenCalled();
    expect(view.extras[0].value).toBe('[\n  0\n]');
  });
});

describe('describeToolCall: arguments that are not text (unvalidated legacy data)', () => {
  const withArguments = (value: unknown) =>
    describeToolCall({ tool_name: 'Bash', arguments: value as unknown as string });

  it('shows an object, an array or a number as compact JSON, in the raw style', () => {
    expect(withArguments({ command: 'ls', n: 1 })).toMatchObject({ kind: 'raw', text: '{"command":"ls","n":1}' });
    expect(withArguments(['a', 'b'])).toMatchObject({ kind: 'raw', text: '["a","b"]' });
    expect(withArguments(42)).toMatchObject({ kind: 'raw', text: '42' });
    expect(withArguments(true)).toMatchObject({ kind: 'raw', text: 'true' });
  });

  it('still reports no arguments for null and undefined', () => {
    expect(withArguments(null).kind).toBe('none');
    expect(withArguments(undefined).kind).toBe('none');
  });

  it('bounds what it shows', () => {
    const big = withArguments({ command: 'z'.repeat(300 * 1024) });
    expect(big.kind === 'raw' && big.text.length).toBeLessThanOrEqual(256 * 1024);
    expect(big.kind === 'raw' && big.omitted).toBeGreaterThan(0);

    // Absurd nesting: unrenderable on an engine that overflows its stack, compact and clipped on one that does not.
    let deep: unknown = 1;
    for (let i = 0; i < 150_000; i++) deep = [deep];
    const view = withArguments(deep);
    expect(view.kind).toBe('raw');
    if (view.kind === 'raw') {
      expect(view.unrenderable === true || (view.omitted ?? 0) > 0).toBe(true);
      expect(view.text.length).toBeLessThanOrEqual(256 * 1024);
    }
  });

  it('survives values JSON cannot print', () => {
    expect(() => withArguments(10n)).not.toThrow();
    expect(withArguments(10n)).toMatchObject({ kind: 'raw', unrenderable: true });
  });

  it('also clips arguments that are text but not JSON', () => {
    const view = describeToolCall(rawToolCall('Bash', `not json ${'q'.repeat(300 * 1024)}`));
    expect(view.kind === 'raw' && view.text.length).toBe(256 * 1024);
    expect(view.kind === 'raw' && view.omitted).toBe(`not json ${'q'.repeat(300 * 1024)}`.length - 256 * 1024);
  });
});

describe('names and ranges', () => {
  it('shows a result whose call is not known as "Unknown tool"', () => {
    expect(toolTitle('unknown-tool')).toBe('Unknown tool');
    expect(toolTitle('unknown-tools')).toBe('unknown-tools');
  });

  it('names a Read of a single line "line N", not "lines N–N"', () => {
    const chips = (input: Record<string, unknown>) => {
      const view = describeToolCall(toolCall('Read', { file_path: '/repo/a.ts', ...input }));
      return view.kind === 'lookup' ? view.chips : null;
    };
    expect(chips({ offset: 10, limit: 1 })).toEqual(['line 10']);
    expect(chips({ limit: 1 })).toEqual(['line 1']);
    expect(chips({ offset: 10, limit: 2 })).toEqual(['lines 10–11']);
    expect(chips({ offset: 10 })).toEqual(['from line 10']);
  });
});
