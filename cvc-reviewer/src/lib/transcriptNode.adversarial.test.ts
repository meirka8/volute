/**
 * Hostile and unusual input for the transcript-node logic: text that only looks
 * like a stimulus marker, empty and missing prompts, inputs built to make a scan
 * slow, and nodes from before blobs were validated.
 */
import { describe, expect, it } from 'vitest';
import {
  failedToolCount,
  groupStimulus,
  hasText,
  nodeLayout,
  parseStimulus,
  promptIsEmbedded,
  stimulusOrigin,
  toolResultsSummary,
  type StimulusSegment,
  type ToolResultSegment,
} from './transcriptNode';
import { makeNode, toolCall } from '../test/fixtures';
import type { InteractionNode } from '../types/cvc';

const V8 = '5e8f1a2b-3c4d-8e5f-a6b7-c8d9e0f1a2b3';
const V4 = '3b9f0f1e-6a4c-4d6e-9b1a-2f6c8e7d5a10';

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

/**
 * The fastest of a few runs. A linear scan takes milliseconds; taking the best
 * of three keeps a garbage-collection pause or a busy CI machine from failing a
 * check that is only meant to catch quadratic or backtracking behaviour.
 */
function bestOf(runs: number, work: () => void): number {
  let best = Infinity;
  for (let run = 0; run < runs; run++) {
    const started = performance.now();
    work();
    best = Math.min(best, performance.now() - started);
  }
  return best;
}

describe('parseStimulus: text that only resembles a marker', () => {
  const asPrompt = (text: string) => [{ kind: 'prompt', text }];

  it.each([
    ['a marker in the middle of a line', 'the log says [tool_result Bash] ok'],
    ['two markers in the middle of a line', 'a [meta] b [compaction summary]\nc [tool_result Read] d'],
    ['a marker after a single newline', 'first line\n[tool_result Bash] second line'],
    ['a marker after blank-line-then-space', 'first\n\n [tool_result Bash] indented'],
    ['a marker after blank-line-then-tab', 'first\n\n\t[meta] indented'],
    ['a marker after Windows blank lines', 'first\r\n\r\n[tool_result Bash] crlf-separated'],
    ['a marker with the wrong case', '[Tool_Result Bash] x'],
    ['a marker in upper case', 'ok\n\n[TOOL_RESULT Bash] x\n\n[META] y'],
    ['a tab where the space belongs', '[tool_result\tBash] x'],
    ['a tab after the name', '[tool_result Bash\t] x'],
    ['a newline inside the name', '[tool_result Ba\nsh] x'],
    ['no space after the closing bracket', 'first\n\n[tool_result Bash]output'],
    ['an error flag without the space', 'first\n\n[tool_result Bash error]output'],
    ['a misspelt error flag', 'first\n\n[tool_result Bash errors] x'],
    ['a plural marker', '[tool_results Bash] x'],
    ['a meta marker without its space', '[meta]text'],
    ['a compaction marker without its newline', '[compaction summary] text'],
    ['a compaction marker with a Windows newline', '[compaction summary]\r\ntext'],
  ])('keeps %s as one prompt', (_label, text) => {
    expect(parseStimulus(text)).toEqual(asPrompt(text));
  });

  it('does not close a tool name at a bracket that belongs to the output', () => {
    // The first "]" ends the name, so what follows must be "] " or " error] ".
    expect(parseStimulus('[tool_result a]b] c')).toEqual(asPrompt('[tool_result a]b] c'));
    expect(parseStimulus('[tool_result [x]] y')).toEqual(asPrompt('[tool_result [x]] y'));
  });
});

describe('parseStimulus: a marker that is not closed', () => {
  it.each([
    '[tool_result Bash',
    '[tool_result Bash error',
    '[tool_result Bash err]',
    '[tool_result Bash error]',
    '[tool_result Bash]',
    '[tool_result',
    '[tool_result ',
    '[tool_result ]',
    '[tool_result ] x',
    '[tool_result  ] x',
    '[tool_result  Bash] two spaces',
    '[tool_result ]]] x',
    '[',
    '[]',
    '[ ]',
    '[meta',
    '[meta]',
    '[compaction summary',
    '[compaction summary]',
    '[Request interrupted',
  ])('keeps %j as plain text, whole', (text) => {
    const segments = parseStimulus(text);
    expect(segments).toHaveLength(1);
    expect(serialize(segments)).toBe(text);
    // Only the interruption line is a prompt-by-itself boundary; everything else here is a prompt too.
    expect(segments[0].kind).toBe('prompt');
  });

  it('treats an unclosed marker after a blank line as the end of the previous segment, not as a result', () => {
    expect(parseStimulus('[tool_result Bash] out\n\n[tool_result Read')).toEqual([
      { kind: 'tool_result', tool: 'Bash', isError: false, text: 'out\n\n[tool_result Read' },
    ]);
  });

  it('recognises a result whose name is exactly the longest allowed, and rejects one a character longer', () => {
    const longest = 'n'.repeat(200);
    expect(parseStimulus(`[tool_result ${longest}] x`)).toEqual([
      { kind: 'tool_result', tool: longest, isError: false, text: 'x' },
    ]);
    const tooLong = 'n'.repeat(201);
    expect(parseStimulus(`[tool_result ${tooLong}] x`)).toEqual([{ kind: 'prompt', text: `[tool_result ${tooLong}] x` }]);
    // The same bound applies when the name is the end of the text.
    expect(parseStimulus(`[tool_result ${'n'.repeat(5000)}`)).toHaveLength(1);
  });
});

describe('parseStimulus: empty and missing input', () => {
  it('reports no stimulus for null, undefined, the empty string and whitespace', () => {
    for (const prompt of [null, undefined, '', ' ', '\n', '\n\n', ' \t\r\n ', ' ', ' ']) {
      expect(parseStimulus(prompt), JSON.stringify(prompt)).toEqual([{ kind: 'none' }]);
    }
  });

  it('does not take other types for a prompt', () => {
    for (const value of [0, 1, NaN, true, false, {}, [], () => 'x', Symbol('x')]) {
      expect(parseStimulus(value as unknown as string)).toEqual([{ kind: 'none' }]);
    }
  });

  it('reports no stimulus only for the exact sentinel', () => {
    expect(parseStimulus('[no stimulus recorded]')).toEqual([{ kind: 'none' }]);
    expect(parseStimulus('[no stimulus recorded] ')).toEqual([{ kind: 'prompt', text: '[no stimulus recorded] ' }]);
    expect(parseStimulus('[No stimulus recorded]')).toEqual([{ kind: 'prompt', text: '[No stimulus recorded]' }]);
    // As the first of several parts it is just text: the producer only writes it alone.
    expect(parseStimulus('[no stimulus recorded]\n\n[meta] x')).toEqual([
      { kind: 'prompt', text: '[no stimulus recorded]' },
      { kind: 'meta', text: 'x' },
    ]);
  });

  it('gives the other helpers nothing to trip over for a missing stimulus', () => {
    const none = parseStimulus(null);
    expect(groupStimulus(none)).toEqual([{ kind: 'none' }]);
    expect(stimulusOrigin({ author: 'human' }, none)).toBe('No stimulus recorded');
    expect(stimulusOrigin({ author: 'agent' }, none)).toBe('No stimulus recorded');
    expect(promptIsEmbedded({ author: 'human' }, none)).toBe(false);
    expect(stimulusOrigin({ author: 'system' }, [])).toBe('No stimulus recorded');
    expect(groupStimulus([])).toEqual([]);
  });
});

describe('parseStimulus: odd characters', () => {
  it('keeps every Unicode form of text byte for byte', () => {
    const texts = [
      'Zażółć gęślą jaźń — 你好 🙂',
      '‮evil‬',
      'a b c',
      'NUL\u0000inside',
      '\ud800 lone surrogate',
      'é combining',
      '﻿bom first',
    ];
    for (const text of texts) {
      expect(serialize(parseStimulus(text))).toBe(text);
      expect(serialize(parseStimulus(`[tool_result Bash] ${text}\n\n[meta] ${text}`))).toBe(`[tool_result Bash] ${text}\n\n[meta] ${text}`);
    }
  });

  it('is not fooled by a tool name made of look-alike characters', () => {
    // Full-width brackets, or a Cyrillic letter in the keyword, are not the marker.
    expect(parseStimulus('［tool_result Bash］ x')).toEqual([{ kind: 'prompt', text: '［tool_result Bash］ x' }]);
    const cyrillicA = '\u0430';
    const cyrillicO = '\u043e';
    expect(parseStimulus(`[tool_result B${cyrillicA}sh] x`)).toEqual([
      { kind: 'tool_result', tool: `B${cyrillicA}sh`, isError: false, text: 'x' },
    ]);
    expect(parseStimulus(`[t${cyrillicO}${cyrillicO}l_result Bash] x`)).toEqual([
      { kind: 'prompt', text: `[t${cyrillicO}${cyrillicO}l_result Bash] x` },
    ]);
  });

  it('accepts any non-space characters in a tool name, as text', () => {
    const name = '<img/src=x/onerror=alert(1)>';
    expect(parseStimulus(`[tool_result ${name}] x`)).toEqual([{ kind: 'tool_result', tool: name, isError: false, text: 'x' }]);
    expect(parseStimulus('[tool_result __proto__] x')).toEqual([
      { kind: 'tool_result', tool: '__proto__', isError: false, text: 'x' },
    ]);
  });
});

describe('parseStimulus: re-joining always reproduces the input', () => {
  it('holds for a larger and different set of random inputs', () => {
    // Deterministic xorshift (another seed than transcriptNode.test.ts), so a failure is reproducible.
    let state = 0x1badb002;
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
      '\n\n\n',
      '\r\n',
      '\r\n\r\n',
      ' ',
      '\t',
      '[meta] ',
      '[meta]',
      '[compaction summary]\n',
      '[compaction summary]',
      '[tool_result Bash] ',
      '[tool_result Read error] ',
      '[tool_result server:web_search] ',
      '[tool_result mcp__a__b error] ',
      '[tool_result ',
      '[tool_result  ',
      '[tool_result ]',
      '[no stimulus recorded]',
      '[Request interrupted by user]',
      '[Request interrupted by user for tool use]',
      '[Request interrupted',
      '[',
      ']',
      '] ',
      ' error] ',
      'error',
      'text',
      'Zażółć 🙂',
      '…[cvc: truncated 12 bytes]…',
      '<script>alert(1)</script>',
      '\u0000',
    ];
    let checked = 0;
    for (let run = 0; run < 6000; run++) {
      let input = '';
      const length = 1 + Math.floor(next() * 40);
      for (let i = 0; i < length; i++) input += pieces[Math.floor(next() * pieces.length)];
      if (input.trim() === '' || input === '[no stimulus recorded]') continue;
      const segments = parseStimulus(input);
      expect(serialize(segments)).toBe(input);
      checked++;
    }
    expect(checked).toBeGreaterThan(5500);
  });

  it('never yields a tool result with an empty name or an unterminated flag', () => {
    const results: ToolResultSegment[] = [];
    for (const input of [
      '[tool_result a] x\n\n[tool_result b error] y\n\n[tool_result  c] z\n\n[tool_result ] w',
      '[tool_result error] x\n\n[tool_result error error] y',
    ]) {
      for (const segment of parseStimulus(input)) if (segment.kind === 'tool_result') results.push(segment);
    }
    expect(results.map((r) => [r.tool, r.isError])).toEqual([
      ['a', false],
      ['b', true],
      ['error', false],
      ['error', true],
    ]);
    for (const result of results) expect(result.tool).not.toBe('');
  });
});

describe('parseStimulus: hostile input of the largest size the harness writes stays fast', () => {
  const SIXTY_FOUR_KIB = 64 * 1024;
  const fill = (unit: string, bytes: number) => unit.repeat(Math.ceil(bytes / unit.length)).slice(0, bytes);

  const shapes: Array<[string, string]> = [
    ['"[" repeated', fill('[', SIXTY_FOUR_KIB)],
    ['"\\n\\n[tool_result " repeated', fill('\n\n[tool_result ', SIXTY_FOUR_KIB)],
    ['"\\n\\n[tool_result a" repeated', fill('\n\n[tool_result a', SIXTY_FOUR_KIB)],
    ['"\\n\\n[tool_result a]" repeated', fill('\n\n[tool_result a]', SIXTY_FOUR_KIB)],
    ['"\\n\\n[tool_result a] x" repeated', fill('\n\n[tool_result a] x', SIXTY_FOUR_KIB)],
    ['"[tool_result a error] " repeated', fill('\n\n[tool_result a error] ', SIXTY_FOUR_KIB)],
    ['"\\n\\n[meta] " repeated', fill('\n\n[meta] ', SIXTY_FOUR_KIB)],
    ['"\\n\\n[Request interrupted by user]" repeated', fill('\n\n[Request interrupted by user]', SIXTY_FOUR_KIB)],
    ['newlines only', fill('\n', SIXTY_FOUR_KIB)],
    ['blank lines only, after a marker', `[tool_result a] ${fill('\n\n', SIXTY_FOUR_KIB)}`],
    ['a name of maximal length, repeated', fill(`\n\n[tool_result ${'n'.repeat(200)}`, SIXTY_FOUR_KIB)],
    ['an unterminated name, 64 KiB long', `[tool_result ${fill('n', SIXTY_FOUR_KIB)}`],
    ['one marker with a 64 KiB body', `[tool_result Bash] ${fill('x', SIXTY_FOUR_KIB)}`],
    ['alternating markers and brackets', fill('[]\n\n[tool_result b] ]\n\n[', SIXTY_FOUR_KIB)],
  ];

  it.each(shapes.map(([label, input]): [string, string] => [label, input.slice(0, SIXTY_FOUR_KIB)]))(
    'parses %s well under 100 ms and loses nothing',
    (_label, input) => {
    expect(input.length).toBe(SIXTY_FOUR_KIB);
    let segments: StimulusSegment[] = [];
    const best = bestOf(3, () => {
      segments = parseStimulus(input);
    });
    // Blank text is "no stimulus"; everything else must come back whole.
    expect(serialize(segments)).toBe(input.trim() === '' ? '[no stimulus recorded]' : input);
    // Expected: a few milliseconds. The bound is generous on purpose.
    expect(best).toBeLessThan(100);
    },
  );

  it('keeps the whole pipeline (group, summarise, classify) linear on the busiest shape', () => {
    const input = fill('\n\n[tool_result a] x', SIXTY_FOUR_KIB);
    const best = bestOf(3, () => {
      const segments = parseStimulus(input);
      const blocks = groupStimulus(segments);
      const first = blocks[0];
      if (first?.kind === 'tool_results') toolResultsSummary(first.results);
      stimulusOrigin({ author: 'system' }, segments);
      promptIsEmbedded({ author: 'human' }, segments);
    });
    expect(best).toBeLessThan(150);
  });

  it('scales to a megabyte without going quadratic', () => {
    // The validator lets a string be 1 MiB; a quadratic scan would take minutes here.
    for (const unit of ['\n\n[tool_result ', '\n\n[tool_result a] x', '\n\n', '[', '\n\n[meta] ']) {
      const input = fill(unit, 1024 * 1024);
      let segments: StimulusSegment[] = [];
      const best = bestOf(2, () => {
        segments = parseStimulus(input);
      });
      expect(serialize(segments), JSON.stringify(unit)).toBe(input.trim() === '' ? '[no stimulus recorded]' : input);
      expect(best, JSON.stringify(unit)).toBeLessThan(1500);
    }
  });
});

describe('stimulusOrigin: which stimulus names a mixed prompt', () => {
  const system = { author: 'system' } as const;
  const origin = (author: 'human' | 'agent' | 'system' | 'external', prompt: string) =>
    stimulusOrigin({ author }, parseStimulus(prompt));

  it('ranks tool results above a compaction summary above injected context above plain text', () => {
    expect(origin('system', '[compaction summary]\nsummary\n\n[tool_result Bash] ok')).toBe('Tool results');
    expect(origin('system', '[tool_result Bash] ok\n\n[compaction summary]\nsummary')).toBe('Tool results');
    expect(origin('system', '[meta] note\n\n[compaction summary]\nsummary')).toBe('Compaction summary');
    expect(origin('system', '[compaction summary]\nsummary\n\n[meta] note')).toBe('Compaction summary');
    expect(origin('system', 'plain\n\n[meta] note')).toBe('Context');
    // Plain text in a stimulus no human wrote is not called a prompt: its origin is not known.
    // (Changed: this used to be 'Prompt', which let text in tool output pass for a person's.)
    expect(origin('system', 'plain text only')).toBe('Unmarked text');
  });

  it('calls anything else than a system stimulus by the same rules, and a human one a prompt whatever it holds', () => {
    for (const prompt of ['[tool_result Bash] ok', '[compaction summary]\nx', '[meta] x', 'plain']) {
      expect(origin('human', prompt), prompt).toBe('Prompt');
    }
    expect(origin('external', '[tool_result Bash] ok')).toBe('Tool results');
    expect(origin('agent', '[meta] x')).toBe('Context');
  });

  it('looks past a blank prompt to the real stimulus', () => {
    expect(origin('system', '\n\n[tool_result Bash] ok')).toBe('Tool results');
    expect(stimulusOrigin(system, [{ kind: 'prompt', text: '   ' }])).toBe('No stimulus recorded');
  });
});

describe('toolResultsSummary and groupStimulus: awkward names and volumes', () => {
  const result = (tool: string, isError = false): ToolResultSegment => ({ kind: 'tool_result', tool, isError, text: '' });

  it('counts tools named like Object.prototype members as ordinary names', () => {
    expect(toolResultsSummary([result('__proto__'), result('constructor'), result('__proto__'), result('toString')])).toBe(
      'Tool results · __proto__ ×2, constructor, toString',
    );
    expect(toolResultsSummary([result('hasOwnProperty', true)])).toBe('Tool results · hasOwnProperty · 1 error');
  });

  it('abbreviates a very long list of distinct tools and counts every error', () => {
    const many = Array.from({ length: 2000 }, (_, i) => result(`tool${i}`, i % 2 === 0));
    expect(toolResultsSummary(many)).toBe('Tool results · tool0, tool1, tool2, tool3, +1996 more · 1000 errors');
  });

  const joinedResults = (count: number): string =>
    Array.from({ length: count }, (_, i) => `[tool_result T] out${i}`).join('\n\n');

  it('puts every result of a long run into one group, in order', () => {
    const segments = parseStimulus(joinedResults(3000));
    const blocks = groupStimulus(segments);
    expect(blocks).toHaveLength(1);
    const [only] = blocks;
    expect(only.kind).toBe('tool_results');
    if (only.kind === 'tool_results') expect(only.results.map((r) => r.text)).toEqual(Array.from({ length: 3000 }, (_, i) => `out${i}`));
  });

  // Changed with the marker hardening: a note between two results used to split them into three blocks,
  // the note drawn as labelled context. It could be a forgery inside the first result, so it is output now.
  it('keeps results that a note separates in one group, the note drawn as output between them', () => {
    const blocks = groupStimulus(parseStimulus('[tool_result A] a\n\n[meta] note\n\n[tool_result B] b'));
    expect(blocks.map((b) => b.kind)).toEqual(['tool_results']);
    const [group] = blocks;
    expect(group.kind === 'tool_results' && group.results.map((r) => r.kind)).toEqual(['tool_result', 'output', 'tool_result']);
    expect(group.kind === 'tool_results' && group.results[1]).toEqual({ kind: 'output', text: '[meta] note' });
  });

  it('still keeps a prompt that comes before the results, and context before them, as blocks of their own', () => {
    const blocks = groupStimulus(parseStimulus('[meta] note\n\n[tool_result A] a'));
    expect(blocks.map((b) => b.kind)).toEqual(['meta', 'tool_results']);
    expect(groupStimulus(parseStimulus('hello\n\n[tool_result A] a')).map((b) => b.kind)).toEqual(['prompt', 'tool_results']);
  });
});

describe('nodeLayout: nodes that were never validated', () => {
  const asNode = (over: Record<string, unknown>) =>
    ({ author: 'human', id: V4, user_prompt: 'plain', tool_executions: [], ...over }) as unknown as Parameters<typeof nodeLayout>[0];

  it('does not throw, and falls back to the triad, for fields of the wrong type', () => {
    for (const over of [
      { id: undefined },
      { id: null },
      { id: 42 },
      { id: {} },
      { id: ['1f0e5c3a-9d2b-8c4e-a1b7-000000000001'] },
      { user_prompt: undefined },
      { user_prompt: 42 },
      { user_prompt: {} },
      { user_prompt: ['[tool_result Bash] x'] },
      { tool_executions: undefined },
      { tool_executions: null },
      { tool_executions: 'nope' },
      { tool_executions: { length: 3 } },
      { author: undefined },
      { author: 'robot' },
    ]) {
      expect(nodeLayout(asNode(over)), JSON.stringify(over)).toBe('triad');
    }
  });

  it('is decided by the first matching rule even when later fields are garbage', () => {
    expect(nodeLayout(asNode({ id: V8, tool_executions: 'nope', user_prompt: 7 }))).toBe('transcript');
    expect(nodeLayout(asNode({ user_prompt: '[meta] x', tool_executions: null }))).toBe('transcript');
    expect(nodeLayout(asNode({ tool_executions: [null] }))).toBe('transcript');
    expect(nodeLayout(asNode({ author: 'agent', id: V8, user_prompt: '[meta] x', tool_executions: [null] }))).toBe('triad');
  });

  it.each([
    ['a trailing newline', `${V8}\n`],
    ['a leading newline', `\n${V8}`],
    ['surrounding spaces', ` ${V8} `],
    ['a urn prefix', `urn:uuid:${V8}`],
    ['braces', `{${V8}}`],
    ['no hyphens', V8.replaceAll('-', '')],
    ['a doubled hyphen', V8.replace('-', '--')],
    ['a version 7 id', '5e8f1a2b-3c4d-7e5f-a6b7-c8d9e0f1a2b3'],
    ['a version 4 id', V4],
    ['a version 1 id', '5e8f1a2b-3c4d-1e5f-a6b7-c8d9e0f1a2b3'],
    ['a nil id', '00000000-0000-0000-0000-000000000000'],
    ['an NCS-variant id', '5e8f1a2b-3c4d-8e5f-76b7-c8d9e0f1a2b3'],
    ['a Microsoft-variant id', '5e8f1a2b-3c4d-8e5f-c6b7-c8d9e0f1a2b3'],
    ['a future-variant id', '5e8f1a2b-3c4d-8e5f-f6b7-c8d9e0f1a2b3'],
    ['full-width digits', '５e8f1a2b-3c4d-8e5f-a6b7-c8d9e0f1a2b3'],
    ['a non-hex digit', '5e8f1a2g-3c4d-8e5f-a6b7-c8d9e0f1a2b3'],
    ['a short last group', '5e8f1a2b-3c4d-8e5f-a6b7-c8d9e0f1a2b'],
    ['a long last group', '5e8f1a2b-3c4d-8e5f-a6b7-c8d9e0f1a2b3c'],
    ['an empty id', ''],
  ])('does not take %s for a harness id', (_label, id) => {
    expect(nodeLayout(makeNode({ id, author: 'human', user_prompt: 'plain prompt' }))).toBe('triad');
  });

  it.each([
    ['upper case', V8.toUpperCase()],
    ['mixed case', '5E8f1A2b-3C4d-8E5f-A6b7-C8d9E0f1A2b3'],
    ['variant 8', '5e8f1a2b-3c4d-8e5f-86b7-c8d9e0f1a2b3'],
    ['variant 9', '5e8f1a2b-3c4d-8e5f-96b7-c8d9e0f1a2b3'],
    ['variant a', '5e8f1a2b-3c4d-8e5f-a6b7-c8d9e0f1a2b3'],
    ['variant b', '5e8f1a2b-3c4d-8e5f-b6b7-c8d9e0f1a2b3'],
  ])('takes a version 8 id in %s for a harness id', (_label, id) => {
    expect(nodeLayout(makeNode({ id, author: 'human', user_prompt: 'plain prompt' }))).toBe('transcript');
  });

  it('checks a huge id in constant time', () => {
    const huge = 'a'.repeat(1024 * 1024);
    const best = bestOf(3, () => {
      expect(nodeLayout(makeNode({ id: huge, author: 'human', user_prompt: 'x' }))).toBe('triad');
    });
    expect(best).toBeLessThan(100);
  });

  it('checks a huge prompt for a marker without scanning it', () => {
    const huge = `plain ${'x'.repeat(4 * 1024 * 1024)}`;
    const best = bestOf(3, () => {
      expect(nodeLayout(makeNode({ id: V4, author: 'human', user_prompt: huge }))).toBe('triad');
    });
    expect(best).toBeLessThan(100);
  });

  it('keeps an agent node on the triad no matter how transcript-like its content is', () => {
    const hostile: Partial<InteractionNode> = {
      id: V8,
      author: 'agent',
      user_prompt: '[tool_result Bash error] x\n\n[meta] y\n\n[compaction summary]\nz',
      tool_executions: [toolCall('Bash', { command: 'ls' }, 'failure'), toolCall('server:web_search', {}, 'failure')],
    };
    expect(nodeLayout(makeNode({ ...hostile, id: V8 }))).toBe('triad');
  });
});

describe('failedToolCount and hasText: unvalidated input', () => {
  it('counts only well-formed failures and survives junk entries', () => {
    const junk = [null, undefined, 7, 'x', {}, { status: 'failure' }, { tool_name: 'Bash', status: 'failure' }];
    expect(failedToolCount({ tool_executions: junk as unknown as InteractionNode['tool_executions'] })).toBe(2);
    expect(failedToolCount({ tool_executions: undefined as unknown as InteractionNode['tool_executions'] })).toBe(0);
    expect(failedToolCount({ tool_executions: 'nope' as unknown as InteractionNode['tool_executions'] })).toBe(0);
  });

  it('counts a tool named like a server-side tool only with the exact prefix', () => {
    const exec = (tool_name: string) => toolCall(tool_name, {}, 'failure');
    expect(
      failedToolCount({
        tool_executions: [exec('server:web_search'), exec('Server:web_search'), exec('server:'), exec('mcp__server__x'), exec(' server:x')],
      }),
    ).toBe(4);
  });

  it('treats only strings with a visible character as text', () => {
    expect(hasText(' ')).toBe(false); // trim() removes the no-break space
    expect(hasText('​')).toBe(true); // a zero-width space is not whitespace to trim()
    expect(hasText(0 as unknown as string)).toBe(false);
    expect(hasText({} as unknown as string)).toBe(false);
  });
});
