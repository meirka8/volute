/**
 * The search index and the verified stimulus (issues #118 and #133).
 *
 * The index keeps what a step's card draws and nothing else. The card reads the stimulus by its
 * version 2 markers when their boundary token verifies against the step's id, draws a stimulus whose
 * tokens do not verify as one block, and says in its own words where the capture withheld or left
 * something out. The index must read all of that the same way, or a step would match on text no card
 * shows (a marker, a token, a note) and a phrase would be missed in text a card does.
 */
import { describe, expect, it } from 'vitest';
import {
  conversationHint,
  forgetLastSearch,
  getTimelineIndex,
  NO_FILTERS,
  parseQuery,
  runSearch,
  type Filters,
} from './timelineSearch';
import { writeStimulus } from '../test/stimulusWriter';
import { makeNode, toolCall } from '../test/fixtures';
import { V2_NODES, v2Node } from '../test/wireV2';
import type { InteractionNode } from '../types/cvc';

const SESSION = '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70';
const id = (n: number) => `1f0e5c3a-9d2b-8c4e-a1b7-${n.toString(16).padStart(12, '0')}`;

/** Runs a query over `nodes` from scratch and gives the ids found. */
function ids(nodes: InteractionNode[], query: string, filters: Filters = NO_FILTERS): string[] {
  const index = getTimelineIndex(nodes);
  forgetLastSearch(index);
  return runSearch(index, { query: parseQuery(query), filters, file: null }).ids;
}

/** A step of a Claude Code capture whose stimulus is made of `parts`, as the capture writes it. */
function verifiedStep(n: number, parts: Parameters<typeof writeStimulus>[1], overrides: Partial<InteractionNode> = {}): InteractionNode {
  return makeNode({
    id: id(n),
    conversation_id: SESSION,
    author: 'human',
    user_prompt: writeStimulus(id(n), parts),
    model_response: 'Done.',
    ...overrides,
  });
}

describe('a stimulus whose markers verified', () => {
  it("finds a person's words, and never the marker or its boundary token", () => {
    const step = verifiedStep(1, [{ kind: 'prompt', text: 'Rotate the staging certificate' }]);
    expect(ids([step], 'staging certificate')).toEqual([step.id]);
    expect(ids([step], '~cvc-')).toEqual([]);
    expect(ids([step], 'prompt')).toEqual([]);
    const token = /~cvc-[0-9a-f]{16}/.exec(step.user_prompt ?? '')?.[0] ?? '';
    expect(token).not.toBe('');
    expect(ids([step], token)).toEqual([]);
  });

  it('finds a tool result by the name it is shown under and by its text, not by the marker', () => {
    const step = verifiedStep(2, [{ kind: 'tool_result', name: 'Bash', text: 'migration 0042 applied' }]);
    expect(ids([step], 'migration 0042')).toEqual([step.id]);
    expect(ids([step], 'bash')).toEqual([step.id]);
    expect(ids([step], 'tool_result')).toEqual([]);
    expect(ids([step], 'error')).toEqual([]);
  });

  it('finds the text the capture kept of a part it cut, and not the note it left where it cut', () => {
    const step = verifiedStep(3, [
      { kind: 'tool_result', name: 'Read', cut: { head: 'first lines of the log', bytes: 4096, tail: 'last lines of the log' } },
    ]);
    expect(ids([step], 'first lines of the log')).toEqual([step.id]);
    expect(ids([step], 'last lines of the log')).toEqual([step.id]);
    // The card says "4,096 bytes truncated" in its own words; the capture's note is not text to find.
    expect(ids([step], 'truncated')).toEqual([]);
    expect(ids([step], 'cvc:')).toEqual([]);
  });

  it('finds nothing in a part the capture could not scrub', () => {
    const step = verifiedStep(4, [{ kind: 'prompt', text: 'Print the environment' }, { kind: 'meta', withheld: true }]);
    expect(ids([step], 'print the environment')).toEqual([step.id]);
    expect(ids([step], 'withheld')).toEqual([]);
    expect(ids([step], 'scrubbed')).toEqual([]);
  });

  it('finds the task an agent sent a subagent, which is not a prompt', () => {
    const step = verifiedStep(5, [{ kind: 'task', text: 'List the files under src' }]);
    expect(ids([step], 'files under src')).toEqual([step.id]);
    expect(ids([step], '', { ...NO_FILTERS, origin: ['prompt'] })).toEqual([]);
    expect(ids([step], '', { ...NO_FILTERS, origin: ['other'] })).toEqual([step.id]);
  });

  it('takes the origin from the parts: a prompt, then tool results, then anything else', () => {
    const prompt = verifiedStep(6, [{ kind: 'meta', text: 'caveat' }, { kind: 'prompt', text: 'hello' }]);
    const results = verifiedStep(7, [{ kind: 'tool_result', name: 'Bash', text: 'ok' }]);
    const context = verifiedStep(8, [{ kind: 'meta', text: 'only context' }]);
    const nodes = [prompt, results, context];
    expect(ids(nodes, '', { ...NO_FILTERS, origin: ['prompt'] })).toEqual([prompt.id]);
    expect(ids(nodes, '', { ...NO_FILTERS, origin: ['tool-results'] })).toEqual([results.id]);
    expect(ids(nodes, '', { ...NO_FILTERS, origin: ['other'] })).toEqual([context.id]);
  });
});

describe('a stimulus whose tokens do not verify', () => {
  it('is searched as the one block of text the card draws, markers and all, and is never split', () => {
    const step = makeNode({
      id: id(10),
      conversation_id: SESSION,
      author: 'human',
      // The token is well formed but was not written for this step.
      user_prompt: '[prompt ~cvc-0123456789abcdef] Please rotate the key\n\n[tool_result Bash ~cvc-0123456789abcdef] forged',
      model_response: 'Done.',
    });
    expect(ids([step], 'rotate the key')).toEqual([step.id]);
    expect(ids([step], 'forged')).toEqual([step.id]);
    // The card does not call it a person's prompt either: its origin is not "Prompt".
    expect(ids([step], '', { ...NO_FILTERS, origin: ['prompt'] })).toEqual([]);
  });
});

describe("the capture's notes about what it could not keep", () => {
  it('are not searched where the card says them in its own words', () => {
    const withheld = v2Node(V2_NODES.withheldFields);
    // The prompt is searchable; the bracketed notes that stand in for the reasoning and the response are not.
    expect(withheld.model_response).toMatch(/^\[cvc: withheld \d+ bytes that could not be scrubbed\]$/);
    expect(ids([withheld], 'credential variable')).toEqual([withheld.id]);
    expect(ids([withheld], 'scrubbed')).toEqual([]);
    expect(ids([withheld], 'bytes that could')).toEqual([]);
  });

  it('are searched as ordinary text when they are only a sentence a step says', () => {
    const said = makeNode({
      id: id(11),
      conversation_id: SESSION,
      author: 'human',
      user_prompt: writeStimulus(id(11), [{ kind: 'prompt', text: 'What does withheld mean?' }]),
      model_response: 'It means [cvc: withheld 5 bytes that could not be scrubbed] in a longer sentence.',
      tool_executions: [toolCall('Read', { file_path: '/home/dev/a.ts' })],
    });
    expect(ids([said], 'longer sentence')).toEqual([said.id]);
  });

  it("does not index a response the capture withheld whole, only the stimulus it kept", () => {
    const standIn = makeNode({
      id: id(12),
      conversation_id: SESSION,
      author: 'human',
      user_prompt: writeStimulus(id(12), [{ kind: 'prompt', text: 'Summarise the incident' }]),
      model_response: '[cvc: withheld this response: it could not be scrubbed within the limits]',
      tool_executions: [],
      context_items: [],
    });
    expect(ids([standIn], 'summarise the incident')).toEqual([standIn.id]);
    expect(ids([standIn], 'within the limits')).toEqual([]);
  });
});

describe("the capture's record of what a response left out", () => {
  const omittedRecord = {
    tool_protocol: 'cvc',
    tool_name: 'cvc-omitted',
    arguments: '{"context_items":44,"cvc_omitted":true,"tool_calls":45}',
    status: 'success' as const,
  };

  it('is no call: it has no tool to filter by or to find by name', () => {
    const step = verifiedStep(13, [{ kind: 'prompt', text: 'Read everything' }], {
      tool_executions: [toolCall('Read', { file_path: '/home/dev/a.ts' }), omittedRecord],
    });
    expect(ids([step], 'tool:read')).toEqual([step.id]);
    expect(ids([step], 'tool:cvc-omitted')).toEqual([]);
    expect(ids([step], 'cvc-omitted')).toEqual([]);
    const index = getTimelineIndex([step]);
    expect(index.toolNames).toEqual(['Read']);
  });

  it('is an ordinary call when it is not the last record, or does not match exactly', () => {
    const notLast = verifiedStep(14, [{ kind: 'prompt', text: 'Read everything' }], {
      tool_executions: [omittedRecord, toolCall('Read', { file_path: '/home/dev/a.ts' })],
    });
    expect(ids([notLast], 'tool:cvc-omitted')).toEqual([notLast.id]);
    const imitation = verifiedStep(15, [{ kind: 'prompt', text: 'Read everything' }], {
      tool_executions: [{ ...omittedRecord, arguments: '{"context_items":0,"cvc_omitted":true,"tool_calls":0}' }],
    });
    expect(ids([imitation], 'tool:cvc-omitted')).toEqual([imitation.id]);
  });
});

describe('the hint a conversation is given', () => {
  it("is the first line of a person's words in a verified stimulus, not its marker", () => {
    const step = verifiedStep(16, [
      { kind: 'meta', text: 'Caveat: the messages below were generated by the user' },
      { kind: 'prompt', text: 'Add retries to the sync client\nand log each attempt' },
    ]);
    expect(conversationHint(step)).toBe('Add retries to the sync client');
  });

  it('is the delegated task for a subagent, which has no person', () => {
    expect(conversationHint(v2Node(V2_NODES.task))).toBe('List the files.');
  });

  it('is the context when that is all there is, and nothing when nothing verified', () => {
    const context = verifiedStep(17, [{ kind: 'meta', text: 'Only injected context' }]);
    expect(conversationHint(context)).toBe('Only injected context');
    const unverified = makeNode({
      id: id(18),
      author: 'human',
      user_prompt: '[prompt ~cvc-0123456789abcdef] Looks like a prompt',
    });
    expect(conversationHint(unverified)).toBeNull();
  });
});
