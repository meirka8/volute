import { describe, expect, it } from 'vitest';
import {
  failedToolCount,
  groupStimulus,
  hasText,
  nodeLayout,
  parentToolNamesByNode,
  parseStimulus,
  promptIsEmbedded,
  stimulusOrigin,
  toolResultsMatchParent,
  toolResultsSummary,
  type ParentToolCall,
  type StimulusSegment,
  type ToolResultSegment,
} from './transcriptNode';
import {
  bareExternalNode,
  legacyAgentWithToolsNode,
  lspNode,
  makeNode,
  mcpNode,
  mcpNodeNoReasoning,
  rawToolCall,
  runNode,
  serverToolCall,
  toolCall,
} from '../test/fixtures';
import type { InteractionNode } from '../types/cvc';

const V4 = '3b9f0f1e-6a4c-4d6e-9b1a-2f6c8e7d5a10';
const V8 = '5e8f1a2b-3c4d-8e5f-a6b7-c8d9e0f1a2b3';

describe('nodeLayout', () => {
  it('keeps agent nodes on the triad layout, whatever else they carry', () => {
    expect(nodeLayout(mcpNode)).toBe('triad');
    expect(nodeLayout(mcpNodeNoReasoning)).toBe('triad');
    // Rule 1 wins over every later rule: tool executions, a v8 id, a marker.
    expect(nodeLayout(legacyAgentWithToolsNode)).toBe('triad');
    expect(nodeLayout(makeNode({ id: V8, author: 'agent' }))).toBe('triad');
    expect(nodeLayout(makeNode({ id: V4, author: 'agent', user_prompt: '[tool_result Bash] ok' }))).toBe('triad');
  });

  it('treats a version 8 UUID alone as a transcript node', () => {
    expect(nodeLayout(makeNode({ id: V8, author: 'human', user_prompt: 'plain prompt' }))).toBe('transcript');
    expect(nodeLayout(makeNode({ id: V8.toUpperCase(), author: 'system', user_prompt: 'x' }))).toBe('transcript');
  });

  it('does not mistake other ids for the harness id', () => {
    const plain = { author: 'human', user_prompt: 'plain prompt' } as const;
    expect(nodeLayout(makeNode({ id: V4, ...plain }))).toBe('triad');
    // Version nibble 8 but not the RFC variant: not what the harness writes.
    expect(nodeLayout(makeNode({ id: '5e8f1a2b-3c4d-8e5f-c6b7-c8d9e0f1a2b3', ...plain }))).toBe('triad');
    // Wrong shape altogether.
    expect(nodeLayout(makeNode({ id: '5e8f1a2b-3c4d-8e5f-a6b7-c8d9e0f1a2b', ...plain }))).toBe('triad');
    expect(nodeLayout(makeNode({ id: ` ${V8}`, ...plain }))).toBe('triad');
    expect(nodeLayout(makeNode({ id: 'legacy-flat-id', ...plain }))).toBe('triad');
  });

  it.each([
    ['a tool result', '[tool_result Bash] ok'],
    ['an errored tool result', '[tool_result Read error] nope'],
    ['injected context', '[meta] caveat'],
    ['a compaction summary', '[compaction summary]\nsummary text'],
    ['the empty-stimulus sentinel', '[no stimulus recorded]'],
  ])('treats a prompt that starts with %s as a transcript node', (_label, prompt) => {
    expect(nodeLayout(makeNode({ id: V4, author: 'system', user_prompt: prompt }))).toBe('transcript');
  });

  it('ignores markers that are not at the very start, and near misses', () => {
    for (const prompt of [
      'see [tool_result Bash] ok',
      ' [meta] leading space',
      '[Meta] wrong case',
      '[meta]no space',
      '[tool_results Bash] plural',
      '[no stimulus recorded] and more',
    ]) {
      expect(nodeLayout(makeNode({ id: V4, author: 'human', user_prompt: prompt }))).toBe('triad');
    }
  });

  it('treats any tool execution on a non-agent node as a transcript node', () => {
    const node = makeNode({
      id: V4,
      author: 'human',
      user_prompt: 'plain prompt',
      tool_executions: [toolCall('Bash', { command: 'ls' })],
    });
    expect(nodeLayout(node)).toBe('transcript');
  });

  it('leaves VS Code/LSP, cvc run and bare legacy nodes unchanged', () => {
    expect(nodeLayout(lspNode)).toBe('triad');
    expect(nodeLayout(runNode)).toBe('triad');
    expect(nodeLayout(bareExternalNode)).toBe('triad');
    expect(nodeLayout(makeNode({ id: V4, author: 'human', user_prompt: null }))).toBe('triad');
  });
});

describe('hasText', () => {
  it('is false for null, undefined and whitespace', () => {
    expect(hasText(null)).toBe(false);
    expect(hasText(undefined)).toBe(false);
    expect(hasText('')).toBe(false);
    expect(hasText(' \n\t ')).toBe(false);
    expect(hasText(' a ')).toBe(true);
  });
});

/** The inverse of the split, written independently of the implementation. */
function serialize(segments: readonly StimulusSegment[]): string {
  return segments
    .map((segment) => {
      switch (segment.kind) {
        case 'prompt':
          return segment.text;
        case 'meta':
          return `[meta] ${segment.text}`;
        case 'compaction':
          return `[compaction summary]\n${segment.text}`;
        case 'tool_result':
          return `[tool_result ${segment.tool}${segment.isError ? ' error' : ''}] ${segment.text}`;
        case 'none':
          return '[no stimulus recorded]';
      }
    })
    .join('\n\n');
}

describe('parseStimulus', () => {
  it('reports no stimulus for a missing, blank or sentinel prompt', () => {
    for (const prompt of [null, undefined, '', '  \n\n ', '[no stimulus recorded]']) {
      expect(parseStimulus(prompt)).toEqual([{ kind: 'none' }]);
    }
  });

  it('keeps plain human text, blank lines included, as one prompt segment', () => {
    expect(parseStimulus('Fix the build.')).toEqual([{ kind: 'prompt', text: 'Fix the build.' }]);
    const long = 'First paragraph.\n\nSecond paragraph.\n\n\nThird, after two blank lines.';
    expect(parseStimulus(long)).toEqual([{ kind: 'prompt', text: long }]);
  });

  it('parses injected context', () => {
    expect(parseStimulus('[meta] Caveat: local command output follows.')).toEqual([
      { kind: 'meta', text: 'Caveat: local command output follows.' },
    ]);
  });

  it('parses a compaction summary and keeps its blank lines', () => {
    expect(parseStimulus('[compaction summary]\nThe user asked for X.\n\nWe changed Y.')).toEqual([
      { kind: 'compaction', text: 'The user asked for X.\n\nWe changed Y.' },
    ]);
  });

  it('parses tool results, with the error flag and the tool name', () => {
    expect(parseStimulus('[tool_result Bash] On branch main')).toEqual([
      { kind: 'tool_result', tool: 'Bash', isError: false, text: 'On branch main' },
    ]);
    expect(parseStimulus('[tool_result Bash error] exit code 1')).toEqual([
      { kind: 'tool_result', tool: 'Bash', isError: true, text: 'exit code 1' },
    ]);
  });

  it('accepts the unusual tool names the harness can write', () => {
    expect(parseStimulus('[tool_result unknown-tool] x')).toEqual([
      { kind: 'tool_result', tool: 'unknown-tool', isError: false, text: 'x' },
    ]);
    expect(parseStimulus('[tool_result mcp__example_server__create_item error] x')).toEqual([
      { kind: 'tool_result', tool: 'mcp__example_server__create_item', isError: true, text: 'x' },
    ]);
    expect(parseStimulus('[tool_result server:web_search] x')).toEqual([
      { kind: 'tool_result', tool: 'server:web_search', isError: false, text: 'x' },
    ]);
    // A tool that is literally called "error" is not an error flag.
    expect(parseStimulus('[tool_result error] x')).toEqual([
      { kind: 'tool_result', tool: 'error', isError: false, text: 'x' },
    ]);
  });

  it('keeps an empty tool output as an empty segment', () => {
    expect(parseStimulus('[tool_result Bash] ')).toEqual([
      { kind: 'tool_result', tool: 'Bash', isError: false, text: '' },
    ]);
    expect(parseStimulus('[tool_result Bash] \n\n[tool_result Read] body')).toEqual([
      { kind: 'tool_result', tool: 'Bash', isError: false, text: '' },
      { kind: 'tool_result', tool: 'Read', isError: false, text: 'body' },
    ]);
  });

  it('splits mixed stimulus into ordered segments', () => {
    const prompt = [
      'Please rerun the tests.',
      '[meta] Skill instructions.',
      '[tool_result Bash error] 2 failed',
      '[tool_result Read] file contents',
    ].join('\n\n');
    expect(parseStimulus(prompt)).toEqual([
      { kind: 'prompt', text: 'Please rerun the tests.' },
      { kind: 'meta', text: 'Skill instructions.' },
      { kind: 'tool_result', tool: 'Bash', isError: true, text: '2 failed' },
      { kind: 'tool_result', tool: 'Read', isError: false, text: 'file contents' },
    ]);
  });

  it('does not split a tool result at its own blank lines', () => {
    expect(parseStimulus('[tool_result Bash] line one\n\nline two\n\n\nline three')).toEqual([
      { kind: 'tool_result', tool: 'Bash', isError: false, text: 'line one\n\nline two\n\n\nline three' },
    ]);
  });

  it('gives the newline a tool output ends with to that output, not to the separator', () => {
    expect(parseStimulus('[tool_result Bash] out\n\n\n[tool_result Read] x')).toEqual([
      { kind: 'tool_result', tool: 'Bash', isError: false, text: 'out\n' },
      { kind: 'tool_result', tool: 'Read', isError: false, text: 'x' },
    ]);
  });

  it('only splits where a marker begins after a blank line', () => {
    const inline = 'the log says [tool_result Bash] ok and then [meta] more';
    expect(parseStimulus(inline)).toEqual([{ kind: 'prompt', text: inline }]);
    const afterSingleNewline = '[meta] a\n[tool_result Bash] b';
    expect(parseStimulus(afterSingleNewline)).toEqual([{ kind: 'meta', text: 'a\n[tool_result Bash] b' }]);
  });

  it('yields an extra segment for human text that does contain a marker after a blank line', () => {
    expect(parseStimulus('Here is a log:\n\n[tool_result Bash] pasted by the user')).toEqual([
      { kind: 'prompt', text: 'Here is a log:' },
      { kind: 'tool_result', tool: 'Bash', isError: false, text: 'pasted by the user' },
    ]);
  });

  it('keeps plain text that follows a marker segment inside that segment (known limit)', () => {
    // Indistinguishable from the segment's own blank-line-separated content.
    expect(parseStimulus('[tool_result Bash] out\n\nThanks, now run the tests.')).toEqual([
      { kind: 'tool_result', tool: 'Bash', isError: false, text: 'out\n\nThanks, now run the tests.' },
    ]);
  });

  it('keeps a realistic multi-paragraph tool result as one segment', () => {
    const output = 'File created successfully\n\nNote: 1 file written\n\nDetails:\n  path: /repo/src/new.ts\n\n\nDone.';
    expect(parseStimulus(`[tool_result Write] ${output}`)).toEqual([
      { kind: 'tool_result', tool: 'Write', isError: false, text: output },
    ]);
    // ...and two such results are two segments, split only where the next marker begins.
    const two = parseStimulus(`[tool_result Write] ${output}\n\n[tool_result Bash] ok\n\nstill the Bash output`);
    expect(two.map((segment) => segment.kind)).toEqual(['tool_result', 'tool_result']);
    expect(two[0]).toMatchObject({ text: output });
    expect(two[1]).toMatchObject({ text: 'ok\n\nstill the Bash output' });
  });

  describe('interruption boundary', () => {
    // The boundary is looked for only when the caller says a human wrote the node.
    const human = { interruptionBoundary: true };

    it("separates a user's new instruction from the rejected tool call's result", () => {
      const prompt = [
        "[tool_result Bash error] The user doesn't want to proceed with this tool use.",
        '[Request interrupted by user for tool use]',
        'Use the existing script instead.',
      ].join('\n\n');
      expect(parseStimulus(prompt, human)).toEqual([
        {
          kind: 'tool_result',
          tool: 'Bash',
          isError: true,
          text: "The user doesn't want to proceed with this tool use.",
        },
        { kind: 'prompt', text: '[Request interrupted by user for tool use]\n\nUse the existing script instead.' },
      ]);
    });

    it('also recognises the shorter interruption line', () => {
      expect(parseStimulus('[tool_result Bash error] stopped\n\n[Request interrupted by user]\n\nTry this.', human)).toEqual([
        { kind: 'tool_result', tool: 'Bash', isError: true, text: 'stopped' },
        { kind: 'prompt', text: '[Request interrupted by user]\n\nTry this.' },
      ]);
    });

    it('is a plain prompt when it is all there is', () => {
      expect(parseStimulus('[Request interrupted by user]', human)).toEqual([
        { kind: 'prompt', text: '[Request interrupted by user]' },
      ]);
    });

    it('only splits after a blank line', () => {
      const midLine = '[tool_result Bash] the log says [Request interrupted by user] here';
      expect(parseStimulus(midLine, human)).toHaveLength(1);
      const afterSingleNewline = '[tool_result Bash] a\n[Request interrupted by user]';
      expect(parseStimulus(afterSingleNewline, human)).toHaveLength(1);
    });

    it('gives a human step a separable prompt, so nothing is flagged as embedded', () => {
      const segments = parseStimulus('[tool_result Bash error] no\n\n[Request interrupted by user for tool use]\n\nDo X.', human);
      expect(promptIsEmbedded({ author: 'human' }, segments)).toBe(false);
      expect(stimulusOrigin({ author: 'human' }, segments)).toBe('Prompt');
    });

    it('lets later markers split, but a tool result after the line means it was not an interruption', () => {
      // Changed with the narrowed boundary: this used to be a tool result, a prompt, then a tool result.
      const segments = parseStimulus(
        '[tool_result Bash error] no\n\n[Request interrupted by user]\n\nInstead:\n\n[tool_result Read] contents',
        human,
      );
      expect(segments).toEqual([
        { kind: 'tool_result', tool: 'Bash', isError: true, text: 'no\n\n[Request interrupted by user]\n\nInstead:' },
        { kind: 'tool_result', tool: 'Read', isError: false, text: 'contents' },
      ]);
    });
  });

  it('treats near-miss markers as plain text', () => {
    for (const text of [
      '[tool_result Bash]no space after the bracket',
      '[tool_result  Bash] two spaces',
      '[tool_result Foo bar] name with a space',
      '[tool_result ] empty name',
      '[tool_result Bash',
      '[meta]',
      '[compaction summary]',
      `[tool_result ${'a'.repeat(300)}] name too long`,
    ]) {
      expect(parseStimulus(text)).toEqual([{ kind: 'prompt', text }]);
    }
  });

  it('preserves truncation markers exactly', () => {
    const middle = `[tool_result Bash] ${'a'.repeat(30)}\n…[cvc: truncated 5120 bytes]…\n${'z'.repeat(30)}`;
    const tail = `${middle}\n\n[tool_result Read] short\n…[cvc: truncated 777 bytes]`;
    const segments = parseStimulus(tail);
    expect(segments).toEqual([
      {
        kind: 'tool_result',
        tool: 'Bash',
        isError: false,
        text: `${'a'.repeat(30)}\n…[cvc: truncated 5120 bytes]…\n${'z'.repeat(30)}`,
      },
      { kind: 'tool_result', tool: 'Read', isError: false, text: 'short\n…[cvc: truncated 777 bytes]' },
    ]);
  });

  it('keeps redaction tokens as ordinary text', () => {
    const token = '[CVC_REDACTED:v1:secret_assignment:11111111-2222-4333-8444-555555555555]';
    const [segment] = parseStimulus(`[tool_result Bash] key=${token}`);
    expect(segment).toMatchObject({ kind: 'tool_result', text: `key=${token}` });
  });

  it('keeps a blank leading segment so nothing is lost', () => {
    const segments = parseStimulus('\n\n[tool_result Bash] x');
    expect(segments).toEqual([
      { kind: 'prompt', text: '' },
      { kind: 'tool_result', tool: 'Bash', isError: false, text: 'x' },
    ]);
  });

  describe('content preservation', () => {
    const samples = [
      'plain',
      '[meta] ',
      '[compaction summary]\n',
      '[compaction summary]\n\n[meta] x',
      '[compaction summary]\n\n\n[meta] x',
      '[tool_result Bash] ',
      '[tool_result Bash] a\n\n\n\n[tool_result Read error] b\n',
      '\n\n[meta] x',
      'a\n\n\n\n[meta] x',
      '[meta] a\n\n[meta] b\n\n[compaction summary]\nc\n\nplain tail',
      "[tool_result Bash error] no\n\n[Request interrupted by user for tool use]\n\nDo X.",
      '[Request interrupted by user]\n\n[Request interrupted by user]',
      '\n\n[Request interrupted by user',
      '[tool_result Bash error] ] ] [ [ \n\n[ \n\n[tool_result',
    ];

    it.each(samples)('re-joining the segments reproduces %j', (sample) => {
      expect(serialize(parseStimulus(sample))).toBe(sample);
    });

    it('holds for random combinations of markers, separators and text', () => {
      // Deterministic xorshift, so a failure is reproducible.
      let state = 0x2545f491;
      const next = () => {
        state ^= state << 13;
        state ^= state >>> 17;
        state ^= state << 5;
        return (state >>> 0) / 0x100000000;
      };
      const pieces = [
        '\n',
        '\n\n',
        '\n\n',
        '[meta] ',
        '[compaction summary]\n',
        '[tool_result Bash] ',
        '[tool_result Read error] ',
        '[tool_result unknown-tool] ',
        '[no stimulus recorded]',
        '[Request interrupted by user]',
        '[tool_result ',
        '[',
        ']',
        ' ',
        'error',
        'text',
        '…[cvc: truncated 12 bytes]',
      ];
      for (let run = 0; run < 3000; run++) {
        let input = '';
        const length = 1 + Math.floor(next() * 30);
        for (let i = 0; i < length; i++) input += pieces[Math.floor(next() * pieces.length)];
        if (input.trim() === '' || input === '[no stimulus recorded]') continue;
        expect(serialize(parseStimulus(input))).toBe(input);
      }
    });
  });

  it('stays fast on hostile input of the largest size the harness writes', () => {
    const shapes = [
      `[tool_result ${'a'.repeat(64 * 1024)}`,
      '[tool_result '.repeat(5000),
      `x${'\n'.repeat(64 * 1024)}`,
      '[meta] \n\n'.repeat(6000),
      `[tool_result Bash] ${'x\n\n'.repeat(20000)}`,
      `${'[tool_result Bash] a\n\n'.repeat(3000)}tail`,
    ];
    const started = performance.now();
    for (const shape of shapes) {
      expect(serialize(parseStimulus(shape))).toBe(shape);
    }
    // Linear work takes milliseconds; a backtracking or quadratic scan would not.
    expect(performance.now() - started).toBeLessThan(2000);
  });
});

describe('stimulusOrigin', () => {
  const human = { author: 'human' } as const;
  const system = { author: 'system' } as const;

  it('calls anything a human wrote a prompt', () => {
    expect(stimulusOrigin(human, parseStimulus('hello'))).toBe('Prompt');
    expect(stimulusOrigin(human, parseStimulus('hello\n\n[tool_result Bash] ok'))).toBe('Prompt');
  });

  it('names tool results, compaction summaries and context for system steps, in that order', () => {
    expect(stimulusOrigin(system, parseStimulus('[tool_result Bash] ok'))).toBe('Tool results');
    expect(stimulusOrigin(system, parseStimulus('[meta] a\n\n[tool_result Bash] ok'))).toBe('Tool results');
    expect(stimulusOrigin(system, parseStimulus('[compaction summary]\nx\n\n[meta] y'))).toBe('Compaction summary');
    expect(stimulusOrigin(system, parseStimulus('[meta] y'))).toBe('Context');
  });

  it('reports a missing stimulus whatever the author says', () => {
    expect(stimulusOrigin(system, parseStimulus('[no stimulus recorded]'))).toBe('No stimulus recorded');
    expect(stimulusOrigin(human, parseStimulus(null))).toBe('No stimulus recorded');
  });

  it('calls plain text on a step no human wrote "Unmarked text", never a prompt', () => {
    expect(stimulusOrigin(system, parseStimulus('some text'))).toBe('Unmarked text');
    expect(stimulusOrigin({ author: 'external' }, parseStimulus('some text'))).toBe('Unmarked text');
    expect(stimulusOrigin({ author: 'agent' }, parseStimulus('some text'))).toBe('Unmarked text');
    // ...but a human's is a prompt, and a marker still outranks plain text.
    expect(stimulusOrigin(human, parseStimulus('some text'))).toBe('Prompt');
    expect(stimulusOrigin(system, parseStimulus('some text\n\n[tool_result Bash] ok'))).toBe('Tool results');
  });
});

describe('promptIsEmbedded', () => {
  it('flags a human step whose text could not be separated from a marker segment', () => {
    expect(promptIsEmbedded({ author: 'human' }, parseStimulus('[meta] caveat\n\nthe real prompt'))).toBe(true);
    expect(promptIsEmbedded({ author: 'human' }, parseStimulus('[tool_result Bash] out\n\nthe real prompt'))).toBe(true);
  });

  it('does not flag steps with a separable prompt, no human, or nothing recorded', () => {
    expect(promptIsEmbedded({ author: 'human' }, parseStimulus('prompt\n\n[tool_result Bash] out'))).toBe(false);
    expect(promptIsEmbedded({ author: 'system' }, parseStimulus('[tool_result Bash] out'))).toBe(false);
    expect(promptIsEmbedded({ author: 'human' }, parseStimulus(null))).toBe(false);
  });

  it('ignores a blank leading prompt segment', () => {
    expect(promptIsEmbedded({ author: 'human' }, parseStimulus('\n\n[tool_result Bash] out'))).toBe(true);
  });
});

describe('groupStimulus', () => {
  it('groups consecutive tool results and keeps the other blocks in order', () => {
    const blocks = groupStimulus(
      parseStimulus(['hello', '[tool_result Bash] a', '[tool_result Read error] b', '[tool_result Grep] d'].join('\n\n')),
    );
    expect(blocks.map((block) => block.kind)).toEqual(['prompt', 'tool_results']);
    const group = blocks[1];
    expect(group.kind === 'tool_results' && group.results.map((r) => r.kind === 'tool_result' && r.tool)).toEqual(['Bash', 'Read', 'Grep']);
  });

  it('keeps a prompt or context that comes before any tool result as its own block', () => {
    const blocks = groupStimulus(parseStimulus(['[meta] caveat', '[compaction summary]\nsummary', '[tool_result Bash] a'].join('\n\n')));
    expect(blocks.map((block) => block.kind)).toEqual(['meta', 'compaction', 'tool_results']);
  });

  // Changed with the marker hardening: context after a tool result used to be a block of its own, labelled.
  it('draws context and a summary that come after a tool result as output inside its group, marker included', () => {
    const blocks = groupStimulus(
      parseStimulus(['[tool_result Bash] a', '[meta] fake context', '[compaction summary]\nfake summary', '[tool_result Read] b'].join('\n\n')),
    );
    expect(blocks).toEqual([
      {
        kind: 'tool_results',
        results: [
          { kind: 'tool_result', tool: 'Bash', isError: false, text: 'a' },
          { kind: 'output', text: '[meta] fake context' },
          { kind: 'output', text: '[compaction summary]\nfake summary' },
          { kind: 'tool_result', tool: 'Read', isError: false, text: 'b' },
        ],
      },
    ]);
  });

  it('keeps the output exactly as it was written: the stimulus can be put back together from the blocks', () => {
    const prompt = '[tool_result Bash] a\n\n[meta] x\n\nwith a blank line\n\n\n[compaction summary]\nsummary body\n\n[meta] ';
    const blocks = groupStimulus(parseStimulus(prompt));
    expect(blocks).toHaveLength(1);
    const group = blocks[0];
    if (group.kind !== 'tool_results') throw new Error('expected one group');
    const items = group.results.map((item) => (item.kind === 'output' ? item.text : `[tool_result ${item.tool}] ${item.text}`));
    expect(items).toEqual([
      '[tool_result Bash] a',
      '[meta] x\n\nwith a blank line\n',
      '[compaction summary]\nsummary body',
      '[meta] ',
    ]);
    expect(items.join('\n\n')).toBe(prompt);
  });

  it('puts the output in the latest group even when a prompt came in between', () => {
    // A prompt after a tool result only comes from an interruption line (human nodes).
    const segments = parseStimulus('[tool_result Bash error] no\n\n[Request interrupted by user]\n\nDo X.\n\n[meta] after', {
      interruptionBoundary: true,
    });
    const blocks = groupStimulus(segments);
    expect(blocks.map((block) => block.kind)).toEqual(['tool_results', 'prompt']);
    const group = blocks[0];
    expect(group.kind === 'tool_results' && group.results.at(-1)).toEqual({ kind: 'output', text: '[meta] after' });
  });

  it('drops a blank leading prompt and passes none through', () => {
    expect(groupStimulus(parseStimulus('\n\n[meta] x')).map((block) => block.kind)).toEqual(['meta']);
    expect(groupStimulus(parseStimulus(null))).toEqual([{ kind: 'none' }]);
  });
});

describe('toolResultsSummary', () => {
  const result = (tool: string, isError = false): ToolResultSegment => ({ kind: 'tool_result', tool, isError, text: '' });

  it('lists the tools once each, in order, with the error count', () => {
    expect(toolResultsSummary([result('Bash'), result('Read'), result('Bash', true)])).toBe(
      'Tool results · Bash ×2, Read · 1 error',
    );
    expect(toolResultsSummary([result('Bash', true), result('Read', true)])).toBe(
      'Tool results · Bash, Read · 2 errors',
    );
    expect(toolResultsSummary([result('Grep')])).toBe('Tool results · Grep');
  });

  it('abbreviates a long list of tools', () => {
    const many = ['A', 'B', 'C', 'D', 'E', 'F'].map((name) => result(name));
    expect(toolResultsSummary(many)).toBe('Tool results · A, B, C, D, +2 more');
  });

  it('is just the heading for an empty list', () => {
    expect(toolResultsSummary([])).toBe('Tool results');
  });
});

describe('failedToolCount', () => {
  it('counts failed executions', () => {
    expect(failedToolCount({ tool_executions: [] })).toBe(0);
    expect(
      failedToolCount({
        tool_executions: [toolCall('Bash', {}), toolCall('Bash', {}, 'failure'), rawToolCall('Edit', '{}', 'failure')],
      }),
    ).toBe(2);
  });

  it('does not count a server-side call written by cvc 0.7.0, which recorded every one as failed', () => {
    expect(
      failedToolCount({
        tool_executions: [
          toolCall('server:web_search', { query: 'x' }, 'failure'),
          toolCall('server:web_fetch', { url: 'https://example.com' }, 'failure'),
          toolCall('Bash', {}, 'failure'),
        ],
      }),
    ).toBe(1);
  });

  it('counts a server-side call recorded with the server protocol when, and only when, its status is a failure', () => {
    expect(
      failedToolCount({
        tool_executions: [
          serverToolCall('web_search', { query: 'x' }, 'success'),
          serverToolCall('web_search', { query: 'y' }, 'failure'),
          serverToolCall('bash_code_execution', { command: 'false' }, 'failure'),
          serverToolCall('web_fetch', { url: 'https://example.com' }, 'success'),
        ],
      }),
    ).toBe(2);
  });

  it('counts the two generations side by side as each deserves: the recorded failure, not the artefact', () => {
    expect(
      failedToolCount({
        tool_executions: [
          toolCall('server:web_search', { query: 'as 0.7.0 wrote it' }, 'failure'),
          serverToolCall('web_search', { query: 'as recorded' }, 'failure'),
          toolCall('Bash', {}, 'failure'),
        ],
      }),
    ).toBe(2);
  });
});

describe('parseStimulus: who may interrupt', () => {
  const rejected = [
    "[tool_result Bash error] The user doesn't want to proceed with this tool use.",
    '[Request interrupted by user for tool use]',
    'Use the existing script instead.',
  ].join('\n\n');

  it('splits at an interruption when told the node is human-authored', () => {
    expect(parseStimulus(rejected, { interruptionBoundary: true }).map((segment) => segment.kind)).toEqual(['tool_result', 'prompt']);
  });

  // Changed with the narrowed boundary: the default used to be "yes".
  it('does not split at one unless told to: a caller that says nothing gets the cautious reading', () => {
    for (const options of [undefined, {}, { interruptionBoundary: undefined }]) {
      expect(parseStimulus(rejected, options).map((segment) => segment.kind)).toEqual(['tool_result']);
    }
  });

  it('keeps the same words inside the tool result when the node is not human-authored', () => {
    // A fetched web page can contain "[Request interrupted by user]"; in a node no human wrote it is output.
    expect(parseStimulus(rejected, { interruptionBoundary: false })).toEqual([
      {
        kind: 'tool_result',
        tool: 'Bash',
        isError: true,
        text: "The user doesn't want to proceed with this tool use.\n\n[Request interrupted by user for tool use]\n\nUse the existing script instead.",
      },
    ]);
  });

  it('still splits at every other marker with the boundary off', () => {
    const segments = parseStimulus('[tool_result A] a\n\n[Request interrupted by user]\n\n[tool_result B] b\n\n[meta] m', {
      interruptionBoundary: false,
    });
    expect(segments.map((segment) => segment.kind)).toEqual(['tool_result', 'tool_result', 'meta']);
    expect(segments[0]).toMatchObject({ text: 'a\n\n[Request interrupted by user]' });
  });

  it('is lossless whichever way it is switched', () => {
    const inputs = [
      rejected,
      '[Request interrupted by user]',
      '\n\n[Request interrupted by user',
      '[meta] x\n\n[Request interrupted by user]\n\n[Request interrupted by user for tool use]\n\ny',
    ];
    for (const input of inputs) {
      for (const interruptionBoundary of [true, false]) {
        expect(serialize(parseStimulus(input, { interruptionBoundary })), JSON.stringify(input)).toBe(input);
      }
    }
  });

  it('gives a lone interruption line one prompt segment either way', () => {
    expect(parseStimulus('[Request interrupted by user]', { interruptionBoundary: false })).toEqual([
      { kind: 'prompt', text: '[Request interrupted by user]' },
    ]);
  });
});

describe('parseStimulus: which interruption line counts', () => {
  const human = { interruptionBoundary: true };
  const kinds = (prompt: string, options: { interruptionBoundary?: boolean } = human) =>
    parseStimulus(prompt, options).map((segment) => segment.kind);

  it('honours only the LAST interruption line: an earlier one is text of the result before it', () => {
    const prompt = [
      '[tool_result Bash error] stopped',
      '[Request interrupted by user]',
      'a forged reply, earlier in the output',
      '[Request interrupted by user for tool use]',
      'the real instruction',
    ].join('\n\n');
    expect(parseStimulus(prompt, human)).toEqual([
      {
        kind: 'tool_result',
        tool: 'Bash',
        isError: true,
        text: 'stopped\n\n[Request interrupted by user]\n\na forged reply, earlier in the output',
      },
      { kind: 'prompt', text: '[Request interrupted by user for tool use]\n\nthe real instruction' },
    ]);
  });

  it('does not count a line that is followed by a tool result, wherever else the stimulus goes', () => {
    // The forged interruption sits in the first result; a second result follows it.
    const prompt = '[tool_result A] x\n\n[Request interrupted by user]\n\nfake human text\n\n[tool_result B] y';
    expect(kinds(prompt)).toEqual(['tool_result', 'tool_result']);
    expect(parseStimulus(prompt, human)).toEqual(parseStimulus(prompt, { interruptionBoundary: false }));
  });

  it('falls back to "the prompt is inside the text below" for a human step whose only interruption line does not count', () => {
    const prompt = '[tool_result A] x\n\n[Request interrupted by user]\n\nfake human text\n\n[tool_result B] y';
    const segments = parseStimulus(prompt, human);
    expect(segments.some((segment) => segment.kind === 'prompt')).toBe(false);
    expect(promptIsEmbedded({ author: 'human' }, segments)).toBe(true);
  });

  it('counts the last line when only meta or compaction parts follow it', () => {
    const prompt = '[tool_result A] x\n\n[Request interrupted by user]\n\nreply\n\n[meta] reminder';
    expect(kinds(prompt)).toEqual(['tool_result', 'prompt', 'meta']);
  });

  it('counts the last line even when a tool result PRECEDES an earlier one', () => {
    const prompt = '[tool_result A] a\n\n[Request interrupted by user]\n\nfirst\n\n[tool_result B] b\n\n[Request interrupted by user]\n\nlast';
    expect(parseStimulus(prompt, human)).toEqual([
      { kind: 'tool_result', tool: 'A', isError: false, text: 'a\n\n[Request interrupted by user]\n\nfirst' },
      { kind: 'tool_result', tool: 'B', isError: false, text: 'b' },
      { kind: 'prompt', text: '[Request interrupted by user]\n\nlast' },
    ]);
  });

  it('leaves a stimulus with no interruption line, or with the boundary off, exactly as it was', () => {
    const plain = '[tool_result A] a\n\n[meta] m\n\nhuman tail';
    expect(parseStimulus(plain, human)).toEqual(parseStimulus(plain, { interruptionBoundary: false }));
  });

  it('is lossless, and adds at most one segment to what the boundary-off reading gives', () => {
    // Deterministic xorshift, so a failure is reproducible.
    let state = 0x1b873593;
    const next = () => {
      state ^= state << 13;
      state ^= state >>> 17;
      state ^= state << 5;
      return (state >>> 0) / 0x100000000;
    };
    const pieces = [
      '\n\n',
      '\n\n',
      '\n',
      '[Request interrupted by user]',
      '[Request interrupted by user]',
      '[Request interrupted by user for tool use]',
      '[tool_result Bash] ',
      '[tool_result Read error] ',
      '[meta] ',
      '[compaction summary]\n',
      'text',
      ' ',
      '[',
    ];
    let honoured = 0;
    for (let run = 0; run < 4000; run++) {
      let input = '';
      const length = 1 + Math.floor(next() * 24);
      for (let i = 0; i < length; i++) input += pieces[Math.floor(next() * pieces.length)];
      if (input.trim() === '') continue;
      const on = parseStimulus(input, human);
      const off = parseStimulus(input, { interruptionBoundary: false });
      expect(serialize(on), JSON.stringify(input)).toBe(input);
      expect(on.length - off.length, JSON.stringify(input)).toBeGreaterThanOrEqual(0);
      expect(on.length - off.length, JSON.stringify(input)).toBeLessThanOrEqual(1);
      if (on.length === off.length) continue;
      honoured++;
      // The segment it added is the last prompt, begins at the line, and nothing after it is a tool result.
      let last = -1;
      for (let i = 0; i < on.length; i++) if (on[i].kind === 'prompt') last = i;
      expect(last, JSON.stringify(input)).toBeGreaterThanOrEqual(0);
      const added = on[last];
      expect(added.kind === 'prompt' && added.text.startsWith('[Request interrupted by user'), JSON.stringify(input)).toBe(true);
      expect(on.slice(last + 1).some((segment) => segment.kind === 'tool_result'), JSON.stringify(input)).toBe(false);
    }
    expect(honoured).toBeGreaterThan(50); // the sweep did reach the honoured case
  });
});

describe('toolResultsMatchParent', () => {
  const segments = (prompt: string) => parseStimulus(prompt);
  /** Calls the previous step made and saw succeed. */
  const ok = (...names: string[]): ParentToolCall[] => names.map((name) => ({ name, failed: false }));
  /** Calls that errored, or never got a result. */
  const failed = (...names: string[]): ParentToolCall[] => names.map((name) => ({ name, failed: true }));

  it('accepts results that are a sub-multiset of the parent calls, in any order', () => {
    expect(toolResultsMatchParent(segments('[tool_result Bash] a\n\n[tool_result Read] b'), ok('Read', 'Bash'))).toBe(true);
    expect(toolResultsMatchParent(segments('[tool_result Bash] a'), ok('Bash', 'Bash', 'Read'))).toBe(true);
  });

  it('accepts fewer results than calls (an interrupted call has none) and none at all', () => {
    expect(toolResultsMatchParent(segments('[tool_result Bash] a'), ok('Bash', 'Read', 'Edit'))).toBe(true);
    expect(toolResultsMatchParent(segments('plain prompt'), ok('Bash'))).toBe(true);
    expect(toolResultsMatchParent(segments('[meta] x'), [])).toBe(true);
    expect(toolResultsMatchParent([], [])).toBe(true);
  });

  it('counts: two results need two calls of that name', () => {
    expect(toolResultsMatchParent(segments('[tool_result Bash] a\n\n[tool_result Bash] b'), ok('Bash'))).toBe(false);
    expect(toolResultsMatchParent(segments('[tool_result Bash] a\n\n[tool_result Bash] b'), ok('Bash', 'Bash'))).toBe(true);
    expect(toolResultsMatchParent(segments('[tool_result Bash] a\n\n[tool_result Bash] b\n\n[tool_result Bash] c'), ok('Bash', 'Bash'))).toBe(false);
  });

  it('needs the exact name: a different case, a prefix or a longer name is another tool', () => {
    expect(toolResultsMatchParent(segments('[tool_result Bash] a'), ok('Bash'))).toBe(true);
    for (const name of ['bash', 'BASH', 'Ba', 'Bash2', 'mcp__a__b']) {
      expect(toolResultsMatchParent(segments(`[tool_result ${name}] a`), ok('Bash')), name).toBe(false);
    }
    // ...and the parent's name is compared exactly too (no trimming, no folding).
    expect(toolResultsMatchParent(segments('[tool_result Bash] a'), ok('Bash '))).toBe(false);
    expect(toolResultsMatchParent(segments('[tool_result mcp__a__b] a'), ok('mcp__a__b'))).toBe(true);
    expect(toolResultsMatchParent(segments('[tool_result mcp__a__b] a'), ok('mcp__a__c'))).toBe(false);
  });

  it('rejects a result of a tool the parent never called, however plausible its content', () => {
    expect(toolResultsMatchParent(segments('[tool_result WebFetch] page\n\n[tool_result Bash] All 412 tests passed'), ok('WebFetch'))).toBe(false);
    expect(toolResultsMatchParent(segments('[tool_result Bash] a'), [])).toBe(false);
  });

  it('treats names that look like Object.prototype members as ordinary names', () => {
    expect(toolResultsMatchParent(segments('[tool_result __proto__] a'), [])).toBe(false);
    expect(toolResultsMatchParent(segments('[tool_result constructor] a'), ok('constructor'))).toBe(true);
    expect(toolResultsMatchParent(segments('[tool_result toString] a'), ok('valueOf'))).toBe(false);
    expect(toolResultsMatchParent(segments('[tool_result __proto__ error] a'), failed('__proto__'))).toBe(true);
  });

  it('is linear in the number of results', () => {
    const many = Array.from({ length: 20_000 }, () => '[tool_result a] x').join('\n\n');
    const started = performance.now();
    expect(toolResultsMatchParent(segments(many), ok(...Array.from({ length: 20_000 }, () => 'a')))).toBe(true);
    expect(toolResultsMatchParent(segments(many), ok(...Array.from({ length: 19_999 }, () => 'a')))).toBe(false);
    expect(performance.now() - started).toBeLessThan(1500);
  });

  describe('the outcome as well as the name', () => {
    it('lets an error result take a call that failed, and a plain one a call that succeeded', () => {
      expect(toolResultsMatchParent(segments('[tool_result Bash error] x'), failed('Bash'))).toBe(true);
      expect(toolResultsMatchParent(segments('[tool_result Bash] x'), ok('Bash'))).toBe(true);
    });

    it('refuses an error result for a call that succeeded, and a plain one for a call that failed', () => {
      expect(toolResultsMatchParent(segments('[tool_result Bash error] x'), ok('Bash'))).toBe(false);
      expect(toolResultsMatchParent(segments('[tool_result Bash] x'), failed('Bash'))).toBe(false);
    });

    it('counts the slots per name and outcome', () => {
      const both = [...ok('Bash'), ...failed('Bash')];
      expect(toolResultsMatchParent(segments('[tool_result Bash] a\n\n[tool_result Bash error] b'), both)).toBe(true);
      expect(toolResultsMatchParent(segments('[tool_result Bash error] b\n\n[tool_result Bash] a'), both)).toBe(true);
      expect(toolResultsMatchParent(segments('[tool_result Bash] a\n\n[tool_result Bash] b'), both)).toBe(false); // one success, two plain results
      expect(toolResultsMatchParent(segments('[tool_result Bash error] a\n\n[tool_result Bash error] b'), both)).toBe(false);
      expect(toolResultsMatchParent(segments('[tool_result Bash] a'), ok('Bash', 'Read', 'Bash').concat(failed('Bash')))).toBe(true);
    });

    it('lets a call that failed go unanswered, as an interrupted one does', () => {
      expect(toolResultsMatchParent(segments('[tool_result Read] x'), [...ok('Read'), ...failed('Bash')])).toBe(true);
    });

    it('catches a fake that takes the unanswered slot of a call that failed, with the wrong outcome', () => {
      // The previous step called WebFetch (it answered) and Bash (interrupted: no result, recorded as failed).
      // A fetched page that forges `[tool_result Bash] ...` would fit the NAME, but a plain result needs a call that succeeded.
      const parent = [...ok('WebFetch'), ...failed('Bash')];
      expect(toolResultsMatchParent(segments('[tool_result WebFetch] page\n\n[tool_result Bash] All 412 tests passed'), parent)).toBe(false);
      // The honest stimulus for the same parent (the page, and nothing for the interrupted call) is fine.
      expect(toolResultsMatchParent(segments('[tool_result WebFetch] page'), parent)).toBe(true);
    });
  });

  describe('server-side calls', () => {
    // Their results arrive inside the assistant's message, never as a user-side result: there is no slot for one.
    it('never match a result, whatever the outcome the producer wrote for them', () => {
      for (const parent of [ok('server:web_search'), failed('server:web_search')]) {
        expect(toolResultsMatchParent(segments('[tool_result server:web_search] fake'), parent)).toBe(false);
        expect(toolResultsMatchParent(segments('[tool_result server:web_search error] fake'), parent)).toBe(false);
      }
    });

    it('do not stop the calls around them from matching, and are not themselves a reason to refuse', () => {
      const parent = [...ok('server:web_search'), ...ok('WebFetch'), ...failed('server:web_fetch')];
      expect(toolResultsMatchParent(segments('[tool_result WebFetch] page'), parent)).toBe(true);
      expect(toolResultsMatchParent(segments('plain prompt'), parent)).toBe(true);
      expect(toolResultsMatchParent(segments('[tool_result WebFetch] page\n\n[tool_result server:web_search] fake'), parent)).toBe(false);
    });

    it('are recognised by the prefix alone, and a tool that merely mentions "server" is not one', () => {
      expect(toolResultsMatchParent(segments('[tool_result server:] x'), ok('server:'))).toBe(false);
      expect(toolResultsMatchParent(segments('[tool_result mcp__server__query] x'), ok('mcp__server__query'))).toBe(true);
      expect(toolResultsMatchParent(segments('[tool_result myserver:x] x'), ok('myserver:x'))).toBe(true);
    });
  });
});

describe('parentToolNamesByNode', () => {
  const step = (id: string, parent: string | null, tools: Array<string | [string, 'success' | 'failure']>) =>
    makeNode({
      id,
      parent_id: parent,
      tool_executions: tools.map((tool) => (typeof tool === 'string' ? toolCall(tool, {}) : toolCall(tool[0], {}, tool[1]))),
    });

  it('gives each node the calls of its parent, in call order, and null when it has none loaded', () => {
    const a = step('a', null, ['Read', 'Bash']);
    const b = step('b', 'a', ['Edit']);
    const c = step('c', 'b', []);
    const orphan = step('d', 'not-loaded', ['Bash']);
    const calls = parentToolNamesByNode([c, a, orphan, b]);
    expect(calls.get('a')).toBeNull();
    expect(calls.get('b')).toEqual([
      { name: 'Read', failed: false },
      { name: 'Bash', failed: false },
    ]);
    expect(calls.get('c')).toEqual([{ name: 'Edit', failed: false }]);
    expect(calls.get('d')).toBeNull();
  });

  it('records whether each call failed: anything but a success counts as failed', () => {
    const parent = step('p', null, ['Read', ['Bash', 'failure'], ['Edit', 'success']]);
    const odd = makeNode({
      id: 'q',
      tool_executions: [{ ...toolCall('Grep', {}), status: 'cancelled' }] as unknown as InteractionNode['tool_executions'],
    });
    const calls = parentToolNamesByNode([parent, odd, step('c1', 'p', []), step('c2', 'q', [])]);
    expect(calls.get('c1')).toEqual([
      { name: 'Read', failed: false },
      { name: 'Bash', failed: true },
      { name: 'Edit', failed: false },
    ]);
    expect(calls.get('c2')).toEqual([{ name: 'Grep', failed: true }]);
  });

  it('gives an empty list, not null, for a loaded parent that called nothing', () => {
    const calls = parentToolNamesByNode([step('a', null, []), step('b', 'a', [])]);
    expect(calls.get('b')).toEqual([]);
  });

  it('survives parents with malformed tool executions', () => {
    const parent = makeNode({ id: 'p', tool_executions: [null, { tool_name: 7 }, { tool_name: 'Bash' }] as unknown as InteractionNode['tool_executions'] });
    const broken = makeNode({ id: 'q', tool_executions: 'nope' as unknown as InteractionNode['tool_executions'] });
    const calls = parentToolNamesByNode([parent, broken, step('c1', 'p', []), step('c2', 'q', [])]);
    // An entry without a usable name matches no result; one with no recorded success counts as failed.
    expect(calls.get('c1')).toEqual([
      { name: '', failed: true },
      { name: '', failed: true },
      { name: 'Bash', failed: true },
    ]);
    expect(calls.get('c2')).toEqual([]);
  });

  it('is linear in the number of nodes', () => {
    const nodes = Array.from({ length: 20_000 }, (_, i) => step(`n${i}`, i === 0 ? null : `n${i - 1}`, ['Bash']));
    const started = performance.now();
    const calls = parentToolNamesByNode(nodes);
    expect(calls.size).toBe(20_000);
    expect(performance.now() - started).toBeLessThan(1500);
  });
});

describe('toolResultsSummary: names as the tool calls show them', () => {
  const result = (tool: string, isError = false): ToolResultSegment => ({ kind: 'tool_result', tool, isError, text: '' });

  it('shows MCP, server-side and unknown tools by their titles', () => {
    expect(toolResultsSummary([result('mcp__github__create_issue'), result('server:web_search'), result('unknown-tool'), result('Bash')])).toBe(
      'Tool results · github · create_issue, web_search (server-side), Unknown tool, Bash',
    );
  });

  it('counts results under one title together', () => {
    expect(toolResultsSummary([result('mcp__a__b'), result('mcp__a__b', true), result('mcp__a__c')])).toBe(
      'Tool results · a · b ×2, a · c · 1 error',
    );
  });
});
