/**
 * What a capture leaves out of a step (issue #133), as the card says it.
 *
 * A response is captured with at most 255 tool calls and 256 context items. When it had more, the
 * capture ends the calls with one record that says how many it left out: not a call the model made, so
 * it is no card and no failure, and the card ends with a note. A response the capture could not store
 * at all is recorded as a stand-in: its stimulus, no model, no reasoning, no calls, and a note in place
 * of the response. Neither may be forgeable by a record that only resembles it.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TimelineNode } from './TimelineNode';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { makeNode, toolCall } from '../../test/fixtures';
import { writeStimulus, type PartSpec } from '../../test/stimulusWriter';
import { boundaryToken } from '../../lib/stimulusV2';
import { WITHHELD_RESPONSE } from '../../lib/transcriptNode';
import type { InteractionNode, ToolExecution } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

/** The id of the producer's own example of a stand-in. */
const ID = '3c2b1a09-8f7e-8d6c-9b5a-4f3e2d1c0b9a';

const record = (toolCalls: number, contextItems: number, over: Partial<ToolExecution> = {}): ToolExecution => ({
  tool_protocol: 'cvc',
  tool_name: 'cvc-omitted',
  arguments: `{"context_items":${contextItems},"cvc_omitted":true,"tool_calls":${toolCalls}}`,
  status: 'success',
  ...over,
});

const bashes = (count: number) => Array.from({ length: count }, (_, index) => toolCall('Bash', { command: `echo ${index}` }));

function step(over: Partial<InteractionNode> = {}): InteractionNode {
  return makeNode({
    id: ID,
    author: 'human',
    model_name: 'claude-test',
    user_prompt: writeStimulus(ID, [{ kind: 'prompt', text: 'Run all of them' }]),
    model_response: 'Running them.',
    ...over,
  });
}

function renderNode(node: InteractionNode) {
  const view = render(<TimelineNode interaction={node} />);
  return { ...view, card: view.container.firstElementChild as HTMLElement };
}

const toolCardsOf = (card: HTMLElement) => Array.from(card.querySelectorAll<HTMLElement>('[data-status]'));
const headerOf = (card: HTMLElement): HTMLElement => card.firstElementChild as HTMLElement;
/** The pane of the card that holds the tool calls: the last one, labelled "Tool calls". */
const callsPaneOf = (card: HTMLElement): HTMLElement => Array.from(card.querySelectorAll<HTMLElement>('.rr-code')).find((pane) => /^Tool calls/.test(pane.textContent ?? ''))!;

describe('the record of what a response left out', () => {
  it('is a note at the end of the tool calls, and not a card', () => {
    const { card } = renderNode(step({ tool_executions: [...bashes(3), record(45, 44)] }));
    expect(toolCardsOf(card)).toHaveLength(3);
    expect(within(card).queryByText('cvc-omitted')).not.toBeInTheDocument();
    expect(card.textContent).not.toContain('cvc_omitted');
    const pane = callsPaneOf(card);
    const note = within(pane).getByText('45 more tool calls and 44 context items were not recorded');
    expect(note).toHaveClass('italic', 'text-muted');
    // It comes after the last card.
    expect(pane.lastElementChild).toBe(note);
  });

  it('is not counted as a call: the heading counts the calls the model made', () => {
    const { card } = renderNode(step({ tool_executions: [...bashes(3), record(45, 44)] }));
    expect(within(callsPaneOf(card)).getByText('3')).toBeInTheDocument();
    cleanup();
    // One call and the record: no count, as for one call.
    const one = renderNode(step({ tool_executions: [...bashes(1), record(1, 0)] }));
    expect(callsPaneOf(one.card).firstElementChild!.textContent).toBe('Tool calls');
  });

  it('is not counted as a failure, whatever status it carries', () => {
    const { card } = renderNode(step({ tool_executions: [...bashes(2), record(2, 0, { status: 'failure' })] }));
    expect(within(headerOf(card)).queryByText(/tool calls? failed/)).not.toBeInTheDocument();
    expect(card.querySelector('[data-status="failure"]')).toBeNull();
    cleanup();
    const failing = renderNode(step({ tool_executions: [toolCall('Bash', { command: 'false' }, 'failure'), record(2, 0, { status: 'failure' })] }));
    expect(within(headerOf(failing.card)).getByText('1 tool call failed')).toBeInTheDocument();
  });

  it('waits for the last card: it follows the page of calls, not the first one', () => {
    const { card } = renderNode(step({ tool_executions: [...bashes(12), record(45, 44)] }));
    expect(toolCardsOf(card)).toHaveLength(10);
    expect(within(card).queryByText(/were not recorded/)).not.toBeInTheDocument();
    fireEvent.click(within(card).getByRole('button', { name: 'Show 2 more tool calls' }));
    expect(toolCardsOf(card)).toHaveLength(12);
    expect(within(card).getByText('45 more tool calls and 44 context items were not recorded')).toBeInTheDocument();
    expect(within(callsPaneOf(card)).getByText('12')).toBeInTheDocument();
  });

  it('is a note under the heading alone when no call was recorded but context items were left out', () => {
    const { card } = renderNode(step({ tool_executions: [record(0, 3)] }));
    expect(toolCardsOf(card)).toHaveLength(0);
    const pane = callsPaneOf(card);
    expect(pane.firstElementChild!.textContent).toBe('Tool calls');
    expect(within(pane).getByText('3 context items were not recorded')).toBeInTheDocument();
    expect(pane.querySelector('ul')).toBeNull();
  });

  it.each([
    [{ calls: 45, items: 44 }, '45 more tool calls and 44 context items were not recorded'],
    [{ calls: 1, items: 1 }, '1 more tool call and 1 context item were not recorded'],
    [{ calls: 2, items: 0 }, '2 more tool calls were not recorded'],
    [{ calls: 1, items: 0 }, '1 more tool call was not recorded'],
    [{ calls: 0, items: 1 }, '1 context item was not recorded'],
    [{ calls: 0, items: 256 }, '256 context items were not recorded'],
    [{ calls: 1234, items: 0 }, '1,234 more tool calls were not recorded'],
  ])('says %j as "%s"', ({ calls, items }, sentence) => {
    const { card } = renderNode(step({ tool_executions: [...bashes(2), record(calls, items)] }));
    expect(within(card).getByText(sentence)).toBeInTheDocument();
  });

  it('draws nothing for a record of nothing, which is not a record: it is a call', () => {
    const { card } = renderNode(step({ tool_executions: [...bashes(1), record(0, 0)] }));
    expect(toolCardsOf(card)).toHaveLength(2);
    expect(within(card).getByText('cvc-omitted')).toBeInTheDocument();
  });

  it.each([
    ['another protocol', record(2, 1, { tool_protocol: 'native' })],
    ['a key more', record(2, 1, { arguments: '{"context_items":1,"cvc_omitted":true,"extra":true,"tool_calls":2}' })],
    ['a count that is a string', record(2, 1, { arguments: '{"context_items":"1","cvc_omitted":true,"tool_calls":2}' })],
    ['another name', record(2, 1, { tool_name: 'cvc-omitted-too' })],
    ['the keys in another order', record(2, 1, { arguments: '{"tool_calls":2,"cvc_omitted":true,"context_items":1}' })],
  ])('draws a record with %s as the ordinary call it is, with its fields', (_label, lookalike) => {
    const { card } = renderNode(step({ tool_executions: [...bashes(2), lookalike] }));
    const cards = toolCardsOf(card);
    expect(cards).toHaveLength(3);
    const own = cards[2];
    expect(within(own).getByText(lookalike.tool_name)).toBeInTheDocument();
    expect(within(own).getByText('tool_calls')).toBeInTheDocument();
    expect(within(callsPaneOf(card)).getByText('3')).toBeInTheDocument();
    expect(within(card).queryByText(/were not recorded|was not recorded/)).not.toBeInTheDocument();
  });

  it('is an ordinary call anywhere but at the end, where the capture never writes it', () => {
    const { card } = renderNode(step({ tool_executions: [record(2, 1), ...bashes(1)] }));
    expect(toolCardsOf(card)).toHaveLength(2);
    expect(within(card).queryByText(/were not recorded/)).not.toBeInTheDocument();
  });

  it('takes only the last of two as the capture\'s note', () => {
    const { card } = renderNode(step({ tool_executions: [...bashes(1), record(9, 9), record(2, 1)] }));
    expect(toolCardsOf(card)).toHaveLength(2);
    expect(within(card).getByText('2 more tool calls and 1 context item were not recorded')).toBeInTheDocument();
  });

  it('stays a card with the unknown tool\'s fields in a step that is not a transcript step', () => {
    // An agent\'s node keeps the Thought / Action / Observation layout, which lists its calls as they are.
    const { container } = render(<TimelineNode interaction={makeNode({ id: '3b9f0f1e-6a4c-4d6e-9b1a-2f6c8e7d5a10', author: 'agent', tool_executions: [record(2, 1)] })} />);
    expect(container.textContent).toContain('cvc-omitted');
  });
});

describe('the stand-in for a response the capture could not store', () => {
  const standIn = (over: Partial<InteractionNode> = {}) =>
    step({ model_name: null, model_cot: null, model_response: WITHHELD_RESPONSE, tool_executions: [], context_items: [], ...over });

  it('shows the stimulus as usual, no model, and a note in place of the response', () => {
    const { card } = renderNode(standIn());
    expect(within(card).getByText('Run all of them')).toHaveClass('font-serif');
    expect(within(headerOf(card)).getByText('Prompt')).toBeInTheDocument();
    expect(headerOf(card).textContent).not.toMatch(/claude/i);
    const note = within(card).getByText('This response was withheld: it could not be scrubbed within the capture limits.');
    expect(note).toHaveClass('italic', 'text-muted');
    expect(within(note.closest('.rr-thought') as HTMLElement).getByText('Response')).toBeInTheDocument();
    // The capture's own sentence is not drawn, and there are no tool calls to draw.
    expect(card.textContent).not.toContain('[cvc:');
    expect(card.textContent).not.toContain('could not be scrubbed within the limits');
    expect(card.querySelector('.rr-code')).toBeNull();
  });

  it('shows the stimulus the capture withheld, when that is what it could not scrub', () => {
    const user_prompt = writeStimulus(ID, [{ kind: 'meta', withheld: true }]);
    // The producer's own example of this form, for this id.
    expect(user_prompt).toBe('[meta ~cvc-8da2e7153d4041f0] [cvc: withheld ~cvc-8da2e7153d4041f0]');
    expect(boundaryToken(ID, 'meta', 0)).toBe('~cvc-8da2e7153d4041f0');
    const { card } = renderNode(standIn({ user_prompt }));
    expect(within(card).getByText('Withheld: this part could not be scrubbed')).toBeInTheDocument();
    expect(within(card).getByText('This response was withheld: it could not be scrubbed within the capture limits.')).toBeInTheDocument();
    expect(card.textContent).not.toContain('~cvc-');
  });

  it('shows the stimulus of the producer\'s own example, as a person\'s words', () => {
    const user_prompt = '[prompt ~cvc-93c1f65ee32fc5b2] Run all of them';
    const { card } = renderNode(standIn({ user_prompt }));
    expect(within(card).getByText('Run all of them')).toHaveClass('font-serif');
    expect(card.textContent).not.toContain('~cvc-');
  });

  it('is a stand-in whatever the stimulus is, and for a step the system started', () => {
    const { card } = renderNode(standIn({ author: 'system', user_prompt: writeStimulus(ID, [{ kind: 'tool_result', name: 'Bash', text: 'ok' }]) }));
    expect(within(card).getByText('This response was withheld: it could not be scrubbed within the capture limits.')).toBeInTheDocument();
    expect(within(card).getByRole('button', { name: 'Tool results · Bash' })).toBeInTheDocument();
  });

  it.each([
    ['reasoning', { model_cot: 'I thought about it.' }],
    ['a tool call', { tool_executions: [toolCall('Bash', { command: 'ls' })] }],
    ['the capture\'s record of what it left out', { tool_executions: [record(1, 0)] }],
    ['a context item', { context_items: [{ file_path: 'src/a.rs', git_blob_sha: null, dirty_patch: null, start_line: null, end_line: null } as never] }],
    ['a response that says more', { model_response: `${WITHHELD_RESPONSE} And then it went on.` }],
    ['a response that says it in other words', { model_response: '[cvc: withheld this response]' }],
  ])('is text, shown as written, for a step with %s', (_label, over) => {
    const node = standIn(over);
    const { card } = renderNode(node);
    expect(within(card).queryByText('This response was withheld: it could not be scrubbed within the capture limits.')).not.toBeInTheDocument();
    expect(card.textContent).toContain(node.model_response!.replace(/\s+/g, ' ').trim().slice(0, 20));
  });

  it('is drawn as the model\'s text in a step that merely says the sentence', () => {
    const { card } = renderNode(step({ model_response: WITHHELD_RESPONSE, model_cot: 'a thought' }));
    expect(within(card).getByText(WITHHELD_RESPONSE)).toBeInTheDocument();
  });
});

describe('strings the capture writes more often now', () => {
  it('draws a partly redacted key in numbered output as written, and does not take the numbers for a sequence', () => {
    const output = '     1\t[CVC_REDACTED:v1:pem:48f7e23d-eeed-419e-8ebb-6f26a04fe2ff]\n     4\tfollowing prose\n    12\tmore';
    const parts: PartSpec[] = [{ kind: 'tool_result', name: 'Read', text: output }];
    const { card } = renderNode(step({ user_prompt: writeStimulus(ID, parts) }));
    expect(card.querySelector('pre')!.textContent).toBe(output);
  });

  it('draws the withheld and the truncated forms of a field, in words', () => {
    const { card } = renderNode(
      step({
        model_cot: '[cvc: withheld 7690 bytes that could not be scrubbed]',
        model_response: 'Done.\n…[cvc: truncated 524288 bytes]',
        tool_executions: [
          { tool_protocol: 'native', tool_name: 'Write', arguments: '{"bytes":8223,"cvc_withheld":true}', status: 'success' },
          { tool_protocol: 'native', tool_name: 'Bash', arguments: '{"bytes":100041,"cvc_truncated":true}', status: 'success' },
          { tool_protocol: 'native', tool_name: 'Edit', arguments: '{"bytes":1,"cvc_truncated":true,"cvc_withheld":true}', status: 'success' },
        ],
      }),
    );
    expect(within(card).getByText('Withheld: 7,690 bytes could not be scrubbed.')).toBeInTheDocument();
    expect(within(card).getByText('Arguments withheld: 8223 bytes could not be scrubbed.')).toBeInTheDocument();
    expect(within(card).getByText('Arguments not recorded: 100041 bytes exceeded the capture limit.')).toBeInTheDocument();
    // Text that merely contains a truncation note is text; both flags at once is not the capture's sentinel.
    expect(card.textContent).toContain('…[cvc: truncated 524288 bytes]');
    expect(card.textContent).not.toContain('cvc_withheld":true}');
    const edit = toolCardsOf(card)[2];
    expect(within(edit).getByText('cvc_truncated')).toBeInTheDocument();
    expect(within(edit).getByText('cvc_withheld')).toBeInTheDocument();
  });

  it('shows the arguments sentinel only for the exact JSON: more keys or another value is an ordinary call', () => {
    for (const arguments_ of ['{"bytes":5,"cvc_withheld":false}', '{"bytes":"5","cvc_withheld":true}', '{"bytes":5,"cvc_withheld":true,"x":1}', '{"bytes":-5,"cvc_withheld":true}', '{"cvc_withheld":true}']) {
      const { card, unmount } = renderNode(step({ tool_executions: [{ tool_protocol: 'native', tool_name: 'Write', arguments: arguments_, status: 'success' }] }));
      expect(within(card).queryByText(/Arguments withheld/), arguments_).not.toBeInTheDocument();
      expect(within(card).getByText('cvc_withheld'), arguments_).toBeInTheDocument();
      unmount();
    }
  });
});
