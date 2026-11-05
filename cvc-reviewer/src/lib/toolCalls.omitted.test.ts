/**
 * What a capture leaves out of a response (issue #133): the record at the end of a step's tool calls
 * that says how many calls and context items were not recorded, and the response a capture withheld.
 *
 * The record is not a call the model made, so it must be exactly the capture's own: a record that
 * only resembles it is an ordinary call, drawn as one, so nothing can hide behind a look-alike.
 */
import { describe, expect, it } from 'vitest';
import {
  OMITTED_PROTOCOL,
  OMITTED_TOOL_NAME,
  describeOmitted,
  describeToolCall,
  omittedCounts,
  parseToolArguments,
  splitOmitted,
} from './toolCalls';
import { failedToolCount, isWithheldResponse, parentToolNamesByNode, toolResultsMatchParent, WITHHELD_RESPONSE, type ParentToolCall } from './transcriptNode';
import { makeNode, toolCall } from '../test/fixtures';
import type { ToolExecution } from '../types/cvc';

const record = (toolCalls: number, contextItems: number, over: Partial<ToolExecution> = {}): ToolExecution => ({
  tool_protocol: 'cvc',
  tool_name: 'cvc-omitted',
  arguments: `{"context_items":${contextItems},"cvc_omitted":true,"tool_calls":${toolCalls}}`,
  status: 'success',
  ...over,
});

const execution = (arguments_: string, over: Partial<ToolExecution> = {}): ToolExecution => ({
  tool_protocol: 'cvc',
  tool_name: 'cvc-omitted',
  arguments: arguments_,
  status: 'success',
  ...over,
});

describe('omittedCounts: the capture\'s own record', () => {
  it('is the protocol, the name and the exact arguments the capture writes', () => {
    expect(OMITTED_PROTOCOL).toBe('cvc');
    expect(OMITTED_TOOL_NAME).toBe('cvc-omitted');
    expect(omittedCounts(record(45, 44))).toEqual({ toolCalls: 45, contextItems: 44 });
    expect(omittedCounts(execution('{"context_items":44,"cvc_omitted":true,"tool_calls":45}'))).toEqual({ toolCalls: 45, contextItems: 44 });
  });

  it('allows either count to be nothing, and takes whole numbers a double holds exactly', () => {
    expect(omittedCounts(record(2, 0))).toEqual({ toolCalls: 2, contextItems: 0 });
    expect(omittedCounts(record(0, 3))).toEqual({ toolCalls: 0, contextItems: 3 });
    expect(omittedCounts(record(1, 1))).toEqual({ toolCalls: 1, contextItems: 1 });
    expect(omittedCounts(execution('{"context_items":999999999999999,"cvc_omitted":true,"tool_calls":999999999999999}'))).toEqual({
      toolCalls: 999999999999999,
      contextItems: 999999999999999,
    });
  });

  it('does not depend on the status the record was written with: it is not a call, so it has no outcome', () => {
    expect(omittedCounts(record(1, 0, { status: 'failure' }))).toEqual({ toolCalls: 1, contextItems: 0 });
  });

  it('needs a count above nothing: a record of nothing left out is no record', () => {
    expect(omittedCounts(record(0, 0))).toBeNull();
  });

  it.each([
    ['another protocol', record(1, 1, { tool_protocol: 'native' })],
    ['the protocol of MCP', record(1, 1, { tool_protocol: 'mcp' })],
    ['the protocol of the provider', record(1, 1, { tool_protocol: 'server' })],
    ['a protocol in capitals', record(1, 1, { tool_protocol: 'CVC' })],
    ['no protocol', record(1, 1, { tool_protocol: '' })],
    ['another name', record(1, 1, { tool_name: 'cvc-omitted2' })],
    ['a name in capitals', record(1, 1, { tool_name: 'Cvc-Omitted' })],
    ['a name with a space', record(1, 1, { tool_name: ' cvc-omitted' })],
    ['the name of a server-side call', record(1, 1, { tool_name: 'server:cvc-omitted' })],
    ['a key more', execution('{"context_items":1,"cvc_omitted":true,"extra":1,"tool_calls":1}')],
    ['a key fewer', execution('{"cvc_omitted":true,"tool_calls":1}')],
    ['another key for a count', execution('{"context_items":1,"cvc_omitted":true,"tool_calls":1,"tool_call":1}')],
    ['the keys in another order', execution('{"tool_calls":45,"cvc_omitted":true,"context_items":44}')],
    ['spaces in the JSON', execution('{"context_items": 1, "cvc_omitted": true, "tool_calls": 1}')],
    ['the JSON written over several lines', execution('{\n  "context_items": 1,\n  "cvc_omitted": true,\n  "tool_calls": 1\n}')],
    ['a count as a string', execution('{"context_items":"1","cvc_omitted":true,"tool_calls":1}')],
    ['a count that is not a number', execution('{"context_items":null,"cvc_omitted":true,"tool_calls":1}')],
    ['a negative count', execution('{"context_items":-1,"cvc_omitted":true,"tool_calls":1}')],
    ['a count that is not whole', execution('{"context_items":1.5,"cvc_omitted":true,"tool_calls":1}')],
    ['a count in exponent form', execution('{"context_items":1e2,"cvc_omitted":true,"tool_calls":1}')],
    ['a count with a leading zero', execution('{"context_items":01,"cvc_omitted":true,"tool_calls":1}')],
    ['a count with a plus sign', execution('{"context_items":+1,"cvc_omitted":true,"tool_calls":1}')],
    ['a count too large to be exact', execution('{"context_items":1000000000000000,"cvc_omitted":true,"tool_calls":1}')],
    ['the flag false', execution('{"context_items":1,"cvc_omitted":false,"tool_calls":1}')],
    ['the flag a string', execution('{"context_items":1,"cvc_omitted":"true","tool_calls":1}')],
    ['the flag a number', execution('{"context_items":1,"cvc_omitted":1,"tool_calls":1}')],
    ['the flag in another case', execution('{"context_items":1,"CVC_OMITTED":true,"tool_calls":1}')],
    ['a trailing newline', execution('{"context_items":1,"cvc_omitted":true,"tool_calls":1}\n')],
    ['a leading space', execution(' {"context_items":1,"cvc_omitted":true,"tool_calls":1}')],
    ['arguments that are an array', execution('[{"context_items":1,"cvc_omitted":true,"tool_calls":1}]')],
    ['arguments nested in an object', execution('{"x":{"context_items":1,"cvc_omitted":true,"tool_calls":1}}')],
    ['arguments that are another sentinel', execution('{"bytes":1,"cvc_truncated":true}')],
    ['no arguments', execution('')],
    ['arguments that are not JSON', execution('context_items=1')],
  ])('is not the record for %s', (_label, exec) => {
    expect(omittedCounts(exec)).toBeNull();
  });

  it('is not the record for a value that is not a tool execution, or arguments that are not text', () => {
    expect(omittedCounts(null)).toBeNull();
    expect(omittedCounts(undefined)).toBeNull();
    expect(omittedCounts({ tool_protocol: 'cvc', tool_name: 'cvc-omitted', arguments: { tool_calls: 1 } as unknown as string })).toBeNull();
    expect(omittedCounts({ tool_protocol: 'cvc', tool_name: 'cvc-omitted', arguments: null as unknown as string })).toBeNull();
  });

  it('is read in constant time, however long the arguments are', () => {
    const started = performance.now();
    for (const bytes of [20, 1_000_000, 5_000_000]) {
      expect(omittedCounts(execution(`{"context_items":${'1'.repeat(bytes)}`))).toBeNull();
      expect(omittedCounts(execution(`{"context_items":1,"cvc_omitted":true,"tool_calls":${'9'.repeat(bytes)}}`))).toBeNull();
    }
    expect(performance.now() - started).toBeLessThan(1000);
  });
});

describe('splitOmitted', () => {
  const bash = toolCall('Bash', { command: 'ls' });
  const read = toolCall('Read', { file_path: 'a' });

  it('takes the record off the end of the calls, and says what it counted', () => {
    const { calls, omitted } = splitOmitted([bash, read, record(45, 44)]);
    expect(calls).toEqual([bash, read]);
    expect(omitted).toEqual({ toolCalls: 45, contextItems: 44 });
  });

  it('leaves the calls alone, as the same list, when there is no record', () => {
    const list = [bash, read];
    const { calls, omitted } = splitOmitted(list);
    expect(calls).toBe(list);
    expect(omitted).toBeNull();
    expect(splitOmitted([]).omitted).toBeNull();
  });

  it('knows a record by its place: it is the LAST entry the capture writes, and one anywhere else is a call', () => {
    const early = record(2, 0);
    expect(splitOmitted([early, bash]).calls).toEqual([early, bash]);
    expect(splitOmitted([early, bash]).omitted).toBeNull();
    // Two of them: only the last is the capture's; the other is drawn as the unknown tool it would be.
    const { calls, omitted } = splitOmitted([bash, early, record(7, 0)]);
    expect(calls).toEqual([bash, early]);
    expect(omitted).toEqual({ toolCalls: 7, contextItems: 0 });
  });

  it('handles a response that holds nothing but the record', () => {
    const { calls, omitted } = splitOmitted([record(0, 3)]);
    expect(calls).toEqual([]);
    expect(omitted).toEqual({ toolCalls: 0, contextItems: 3 });
  });

  it('leaves a look-alike at the end as the call it is', () => {
    const lookalike = execution('{"context_items":1,"cvc_omitted":true,"extra":true,"tool_calls":1}');
    const { calls, omitted } = splitOmitted([bash, lookalike]);
    expect(calls).toEqual([bash, lookalike]);
    expect(omitted).toBeNull();
  });

  it('is a lookalike\'s own card: an unknown tool shown as fields, with nothing hidden', () => {
    const view = describeToolCall(execution('{"context_items":1,"cvc_omitted":true,"extra":true,"tool_calls":1}'));
    expect(view.kind).toBe('fields');
    expect(view.title).toBe('cvc-omitted');
    if (view.kind === 'fields') expect(view.fields.map((field) => field.key)).toEqual(['context_items', 'cvc_omitted', 'extra', 'tool_calls']);
    // ...which is what the record itself would be if it were read as a call.
    expect(parseToolArguments(record(1, 1).arguments).kind).toBe('object');
  });
});

describe('describeOmitted', () => {
  it.each([
    [{ toolCalls: 45, contextItems: 44 }, '45 more tool calls and 44 context items were not recorded'],
    [{ toolCalls: 1, contextItems: 1 }, '1 more tool call and 1 context item were not recorded'],
    [{ toolCalls: 1, contextItems: 44 }, '1 more tool call and 44 context items were not recorded'],
    [{ toolCalls: 45, contextItems: 1 }, '45 more tool calls and 1 context item were not recorded'],
    [{ toolCalls: 45, contextItems: 0 }, '45 more tool calls were not recorded'],
    [{ toolCalls: 2, contextItems: 0 }, '2 more tool calls were not recorded'],
    [{ toolCalls: 1, contextItems: 0 }, '1 more tool call was not recorded'],
    [{ toolCalls: 0, contextItems: 3 }, '3 context items were not recorded'],
    [{ toolCalls: 0, contextItems: 1 }, '1 context item was not recorded'],
    [{ toolCalls: 1234, contextItems: 5678 }, '1,234 more tool calls and 5,678 context items were not recorded'],
  ])('%j reads "%s"', (counts, sentence) => {
    expect(describeOmitted(counts)).toBe(sentence);
  });
});

describe('the record is not a call', () => {
  const bash = toolCall('Bash', { command: 'ls' });
  const failed = toolCall('Bash', { command: 'false' }, 'failure');

  it('is not counted as a failure, whatever status it carries', () => {
    expect(failedToolCount(makeNode({ id: 'a', tool_executions: [bash, failed, record(3, 0, { status: 'failure' })] }))).toBe(1);
    expect(failedToolCount(makeNode({ id: 'a', tool_executions: [record(3, 0, { status: 'failure' })] }))).toBe(0);
  });

  it('is counted when it is a look-alike: that is a call recorded as failed', () => {
    const lookalike = execution('{"context_items":1,"cvc_omitted":true,"extra":true,"tool_calls":1}', { status: 'failure' });
    expect(failedToolCount(makeNode({ id: 'a', tool_executions: [bash, lookalike] }))).toBe(1);
  });

  it('is not one of the previous step\'s calls, and says how many calls it stands for', () => {
    const parent = makeNode({ id: 'p', tool_executions: [bash, failed, record(45, 44)] });
    const child = makeNode({ id: 'c', parent_id: 'p' });
    expect(parentToolNamesByNode([parent, child]).get('c')).toEqual<ParentToolCall[]>([
      { name: 'Bash', failed: false },
      { name: 'Bash', failed: true },
      { name: '', failed: false, unrecorded: 45 },
    ]);
    // Only context items were left out: no call is unaccounted for.
    const items = makeNode({ id: 'p', tool_executions: [bash, record(0, 3)] });
    expect(parentToolNamesByNode([items, child]).get('c')).toEqual([{ name: 'Bash', failed: false }]);
  });

  it('lets a result no recorded call accounts for answer one of the calls it stands for, once each', () => {
    const calls: ParentToolCall[] = [{ name: 'Bash', failed: false }, { name: '', failed: false, unrecorded: 2 }];
    const results = (names: string[]) => names.map((tool) => ({ kind: 'tool_result' as const, tool, isError: false, text: 'x' }));
    expect(toolResultsMatchParent(results(['Bash', 'Read', 'Edit']), calls)).toBe(true);
    expect(toolResultsMatchParent(results(['Bash', 'Read', 'Edit', 'Write']), calls)).toBe(false);
    expect(toolResultsMatchParent(results(['Read', 'Edit']), calls)).toBe(true);
    // A server-side call has no user-side result, whatever was left out.
    expect(toolResultsMatchParent(results(['Bash', 'server:web_search']), calls)).toBe(false);
    // Without the record, the same results do not match.
    expect(toolResultsMatchParent(results(['Bash', 'Read']), [{ name: 'Bash', failed: false }])).toBe(false);
  });
});

describe('isWithheldResponse: the stand-in for a response the capture could not store', () => {
  const standIn = () => makeNode({ id: 'a', author: 'human', model_name: null, model_cot: null, model_response: WITHHELD_RESPONSE, tool_executions: [], context_items: [] });

  it('is the sentence, no reasoning, no calls and no context items', () => {
    expect(WITHHELD_RESPONSE).toBe('[cvc: withheld this response: it could not be scrubbed within the limits]');
    expect(isWithheldResponse(standIn())).toBe(true);
    // The model's name is none, as the capture writes it, but is not what makes it a stand-in.
    expect(isWithheldResponse({ ...standIn(), model_name: 'claude' } as never)).toBe(true);
  });

  it.each([
    ['a response that says something else', { model_response: '[cvc: withheld this response]' }],
    ['the sentence with more after it', { model_response: `${WITHHELD_RESPONSE}\n` }],
    ['the sentence with text before it', { model_response: `I said: ${WITHHELD_RESPONSE}` }],
    ['the sentence in other case', { model_response: WITHHELD_RESPONSE.toUpperCase() }],
    ['the note for a field that could not be scrubbed', { model_response: '[cvc: withheld 5 bytes that could not be scrubbed]' }],
    ['no response', { model_response: null }],
    ['reasoning', { model_cot: 'thinking' }],
    ['empty reasoning', { model_cot: '' }],
    ['a tool call', { tool_executions: [toolCall('Bash', { command: 'ls' })] }],
    ['the capture\'s record of what it left out', { tool_executions: [record(1, 0)] }],
    ['a context item', { context_items: [{ file_path: 'src/a.rs' } as never] }],
  ])('is not one for %s', (_label, over) => {
    expect(isWithheldResponse({ ...standIn(), ...over })).toBe(false);
  });

  it('does not read a node whose lists are not lists', () => {
    expect(isWithheldResponse({ ...standIn(), tool_executions: undefined as never })).toBe(false);
    expect(isWithheldResponse({ ...standIn(), context_items: null as never })).toBe(false);
  });
});
