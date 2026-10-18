/**
 * The timeline as a whole: it hands each step the tool names of the step before
 * it (so a stimulus that fakes capture markers can be recognised), and it keeps
 * one step that cannot be drawn from taking the others with it.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { displayText } from '../../lib/textLines';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import {
  failingStep,
  injectionHumanStep,
  injectionParentStep,
  injectionSystemStep,
  makeNode,
  mcpNode,
  promptStep,
} from '../../test/fixtures';
import { undrawableNode } from '../../test/brokenFixtures';
import { normalizeInteraction, type CVCBlobData, type InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

const NOTE_PATTERN = /Shown undivided: the tool results named here don't match the previous step's tool calls/;
const FAILED_CARD = 'This step could not be displayed.';

const cardsOf = (container: HTMLElement) => Array.from(container.querySelectorAll<HTMLElement>('[role="button"][aria-pressed]'));

// ---------------------------------------------------------------------------
// Real nodes, written by the shipped producer (see ../../test/wire/README.md)
// ---------------------------------------------------------------------------

const RAW = import.meta.glob<string>('../../test/wire/*/*.json', { eager: true, query: '?raw', import: 'default' });

function wireNodes(directory: string): InteractionNode[] {
  return Object.entries(RAW)
    .filter(([path]) => path.includes(`/wire/${directory}/`))
    .map(([, raw]) => normalizeInteraction(JSON.parse(raw) as CVCBlobData));
}

describe('the parent of each step, from parent_id', () => {
  it('flags the injection in a system step whose parent is loaded, and only there', () => {
    const { container } = render(<ShadowTimeline interactions={[injectionParentStep, injectionSystemStep]} />);
    expect(screen.getAllByText(NOTE_PATTERN)).toHaveLength(1);
    const [child, parent] = cardsOf(container); // newest first
    expect(within(child).getByText(NOTE_PATTERN)).toBeInTheDocument();
    expect(within(parent).queryByText(NOTE_PATTERN)).not.toBeInTheDocument();
  });

  it('does not flag it when the parent is not among the steps shown', () => {
    render(<ShadowTimeline interactions={[injectionSystemStep]} />);
    expect(screen.queryByText(NOTE_PATTERN)).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Tool results · WebFetch, Bash' })).toBeInTheDocument();
  });

  it('flags it in a human step too, whatever order the steps are given in', () => {
    for (const nodes of [
      [injectionParentStep, injectionHumanStep],
      [injectionHumanStep, injectionParentStep],
    ]) {
      const { container, unmount } = render(<ShadowTimeline interactions={nodes} />);
      expect(screen.getAllByText(NOTE_PATTERN)).toHaveLength(1);
      // Open by default, since a human took part; the note and the raw text are on the page.
      const human = cardsOf(container).find((card) => card.textContent!.includes('Running the tests.'))!;
      expect(within(human).getByRole('button', { name: 'Tool results' })).toHaveAttribute('aria-expanded', 'true');
      expect(human.querySelector('.rr-thought pre')!.textContent).toBe(injectionHumanStep.user_prompt);
      unmount();
    }
  });

  it('checks each step against ITS parent: two children of one parent are both checked', () => {
    const other = makeNode({ ...injectionSystemStep, id: '1f0e5c3a-9d2b-8c4e-a1b7-0000000000dd', user_prompt: '[tool_result WebFetch] a plain page' });
    render(<ShadowTimeline interactions={[injectionParentStep, injectionSystemStep, other]} />);
    expect(screen.getAllByText(NOTE_PATTERN)).toHaveLength(1); // only the injected one
    expect(screen.getByRole('button', { name: 'Tool results · WebFetch' })).toBeInTheDocument(); // the honest sibling stays divided
  });

  it('uses the parent as it is now when the steps are replaced', () => {
    const { rerender } = render(<ShadowTimeline interactions={[injectionSystemStep]} />);
    expect(screen.queryByText(NOTE_PATTERN)).not.toBeInTheDocument();
    rerender(<ShadowTimeline interactions={[injectionParentStep, injectionSystemStep]} />);
    expect(screen.getAllByText(NOTE_PATTERN)).toHaveLength(1);
    // The parent now says it called Bash too: the results are plausible, so the note goes.
    const bashed = makeNode({ ...injectionParentStep, tool_executions: [...injectionParentStep.tool_executions, { ...injectionParentStep.tool_executions[0], tool_name: 'Bash' }] });
    rerender(<ShadowTimeline interactions={[bashed, injectionSystemStep]} />);
    expect(screen.queryByText(NOTE_PATTERN)).not.toBeInTheDocument();
  });

  it('leaves the triad cards exactly as they were, parent or no parent', () => {
    const normalise = (html: string) => html.replace(/ (id|aria-controls)="[^"]*"/g, '');
    const triadCards = (container: HTMLElement) =>
      cardsOf(container)
        .filter((card) => card.textContent!.includes('Thought'))
        .map((card) => normalise(card.innerHTML));
    const child = makeNode({ ...mcpNode, id: '3b9f0f1e-6a4c-4d6e-9b1a-2f6c8e7d5a11', parent_id: promptStep.id, timestamp: mcpNode.timestamp + 1 });

    const alone = render(<ShadowTimeline interactions={[mcpNode, child]} />);
    const without = triadCards(alone.container);
    alone.unmount();
    const withParent = render(<ShadowTimeline interactions={[promptStep, mcpNode, child]} />);
    expect(without).toHaveLength(2);
    expect(triadCards(withParent.container)).toEqual(without);
  });
});

describe('real conversations stay divided', () => {
  const summaries = (container: HTMLElement) =>
    Array.from(container.querySelectorAll('button'))
      .map((button) => button.textContent ?? '')
      .filter((text) => text.startsWith('Tool results'))
      .sort();

  it('keeps the v2.1.260 conversation as the producer divided it: every result is a result of the step before', () => {
    const nodes = wireNodes('v2.1.260-basic');
    expect(nodes).toHaveLength(7);
    const { container } = render(<ShadowTimeline interactions={nodes} />);
    expect(screen.queryByText(NOTE_PATTERN)).not.toBeInTheDocument();
    expect(container.querySelectorAll('[data-layout="transcript"]')).toHaveLength(7);
    expect(summaries(container)).toEqual([
      'Tool results · Bash, Read',
      'Tool results · Edit · 1 error',
      'Tool results · Read',
      'Tool results · Read',
      'Tool results · Write',
    ]);
  });

  it('keeps the edge-case conversation divided, including the step that answers only four of six calls', () => {
    const nodes = wireNodes('edge-cases');
    expect(nodes.length).toBeGreaterThanOrEqual(6);
    const { container } = render(<ShadowTimeline interactions={nodes} />);
    expect(screen.queryByText(NOTE_PATTERN)).not.toBeInTheDocument();
    const names = summaries(container);
    expect(names).toContain('Tool results · Write, fixture · bulk_import, NotebookEdit, Task · 1 error');
    expect(names).toContain('Tool results · github · create_issue');
    // The orphan result's parent is not loaded, so it is divided as before.
    expect(names).toContain('Tool results · Unknown tool');
  });

  it('would flag an orphan result whose parent IS loaded and did not call it (the check is exact)', () => {
    // The producer writes `unknown-tool` when it cannot tell which call a result belongs to; a loaded
    // parent has no call of that name, so the stimulus is shown undivided (with every character in it).
    const [orphan] = wireNodes('edge-cases').filter((node) => node.user_prompt?.startsWith('Try again'));
    const parent = makeNode({ id: orphan.parent_id!, author: 'human', tool_executions: [] });
    const { container } = render(<ShadowTimeline interactions={[parent, orphan]} />);
    expect(screen.getAllByText(NOTE_PATTERN)).toHaveLength(1);
    const card = cardsOf(container).find((c) => c.textContent!.includes('Recovered.'))!;
    expect(card.querySelector('.rr-thought pre')!.textContent).toBe(displayText(orphan.user_prompt!));
  });
});

describe('a step that cannot be drawn', () => {
  beforeEach(() => {
    // React logs the error it handed to the boundary; that log is the expected one.
    vi.spyOn(console, 'error').mockImplementation(() => {});
  });

  it('shows a small card with its id, and every other step is drawn as usual', () => {
    const { container } = render(<ShadowTimeline interactions={[promptStep, undrawableNode, failingStep]} />);
    expect(screen.getByText(FAILED_CARD)).toBeInTheDocument();
    expect(screen.getByText(undrawableNode.id)).toBeInTheDocument();
    // The other two steps are whole.
    expect(screen.getAllByText('AI step')).toHaveLength(2);
    expect(screen.getByText(promptStep.user_prompt!)).toBeInTheDocument();
    expect(screen.getByText('1 tool call failed')).toBeInTheDocument();
    // Three entries in the list, one of them the fallback; the count in the header is of the nodes it was given.
    expect(screen.getByText('3 interactions linked to this PR')).toBeInTheDocument();
    expect(cardsOf(container)).toHaveLength(2);
  });

  it('takes nothing else down when several steps fail, and keeps the order', () => {
    const second = makeNode({ ...undrawableNode, id: 'aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa', timestamp: undrawableNode.timestamp - 1000 });
    render(<ShadowTimeline interactions={[second, promptStep, undrawableNode]} />);
    expect(screen.getAllByText(FAILED_CARD)).toHaveLength(2);
    expect(screen.getAllByText('AI step')).toHaveLength(1);
  });

  it('draws the step once its data is replaced by data that can be drawn', () => {
    const { rerender } = render(<ShadowTimeline interactions={[undrawableNode]} />);
    expect(screen.getByText(FAILED_CARD)).toBeInTheDocument();

    rerender(<ShadowTimeline interactions={[makeNode({ ...undrawableNode, model_name: 'repaired-model' })]} />);
    expect(screen.queryByText(FAILED_CARD)).not.toBeInTheDocument();
    expect(screen.getByText('repaired-model')).toBeInTheDocument();
  });

  it('is not selectable as a card, since there is nothing to select', () => {
    const onSelect = vi.fn();
    render(<ShadowTimeline interactions={[undrawableNode]} onSelectInteraction={onSelect} />);
    fireEvent.click(screen.getByText(FAILED_CARD));
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('draws a transcript step whose model name is not text, instead of failing', () => {
    // The transcript layout reads the model name only if it is text.
    const odd = makeNode({
      ...promptStep,
      id: '1f0e5c3a-9d2b-8c4e-a1b7-0000000000ee',
      model_name: { family: 'broken' } as unknown as string,
    });
    render(<ShadowTimeline interactions={[odd]} />);
    expect(screen.queryByText(FAILED_CARD)).not.toBeInTheDocument();
    expect(screen.getByText('AI step')).toBeInTheDocument();
  });

  it('names a node with no usable id without throwing', () => {
    const noId = { ...undrawableNode, id: undefined as unknown as string };
    expect(() => render(<ShadowTimeline interactions={[noId]} />)).not.toThrow();
    expect(screen.getByText(FAILED_CARD)).toBeInTheDocument();
  });
});

describe('the steps keep their own state as others come and go', () => {
  it('leaves a step open when another step is removed or added', () => {
    const a = failingStep;
    const b = promptStep;
    const { rerender } = render(<ShadowTimeline interactions={[a, b]} />);
    const toggle = screen.getByRole('button', { name: /^Tool results/ });
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');

    rerender(<ShadowTimeline interactions={[a]} />);
    expect(screen.getByRole('button', { name: /^Tool results/ })).toHaveAttribute('aria-expanded', 'true');
    rerender(<ShadowTimeline interactions={[a, b, makeNode({ ...promptStep, id: '1f0e5c3a-9d2b-8c4e-a1b7-0000000000ff', timestamp: promptStep.timestamp + 1 })]} />);
    expect(screen.getByRole('button', { name: /^Tool results/ })).toHaveAttribute('aria-expanded', 'true');
  });

  it('still reports selection by the id of the step clicked', () => {
    const onSelect = vi.fn();
    render(<ShadowTimeline interactions={[promptStep, failingStep]} selectedInteractionId={failingStep.id} onSelectInteraction={onSelect} />);
    const cards = screen.getAllByRole('button', { pressed: true });
    expect(cards).toHaveLength(1);
    fireEvent.click(screen.getAllByText('AI step')[1]);
    expect(onSelect).toHaveBeenCalledWith(promptStep.id); // newest first: the failing step is on top
  });
});
