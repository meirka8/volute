/**
 * What a card says about the agent a conversation came from, and about what kind of reasoning it holds (#187).
 *
 * `refs/cvc/main` stays on FORMAT 5: the agent is read from the conversation id's prefix, and a provider's
 * summary of reasoning is marked inside `model_cot`. Both are the publisher's words, and the card says so
 * (the title of the agent label) or shows the text literally (a marker that does not verify).
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TimelineNode } from './TimelineNode';
import { reasoningBoundary } from '../../lib/reasoningMarker';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { makeNode, mcpNode, promptStep } from '../../test/fixtures';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

const WORDS = "provider summary of the model's reasoning";
const marked = (id: string, body: string) => `[cvc: ${WORDS} ${reasoningBoundary(id, 'provider_summary', 0)}]\n\n${body}`;

function card(node: InteractionNode): HTMLElement {
  render(<TimelineNode interaction={node} />);
  return screen.getAllByRole('listitem')[0];
}

describe('the agent label', () => {
  it.each([
    ['codex:thr_1', 'Codex'],
    ['opencode:ses_1', 'OpenCode'],
    ['cursor:c1', 'Cursor'],
    ['antigravity:a1', 'Antigravity'],
    ['claude-code:4f2a9c1e-0000-4000-8000-000000000001', 'Claude Code'],
  ])('shows %s as %s, as recorded by the publisher', (conversation, label) => {
    const el = card({ ...promptStep, conversation_id: conversation });
    const badge = within(el).getByText(label);
    expect(badge).toHaveAttribute('title', `Agent: ${label}, as recorded by the publisher`);
  });

  it('says "as recorded by the publisher" to screen readers and keyboard users too, not only in the tooltip', () => {
    const el = card({ ...promptStep, conversation_id: 'codex:thr_1' });
    const badge = el.querySelector('[data-agent]')!;
    // Hidden from sight, not from assistive technology (sr-only, not display:none or aria-hidden).
    const note = within(badge as HTMLElement).getByText('(as recorded by the publisher)');
    expect(note).toHaveClass('sr-only');
    expect(note.closest('[aria-hidden="true"]')).toBeNull();
    // The visible text stays short, and the header button the keyboard reaches is named with the note.
    expect(badge.firstChild?.textContent).toBe('Codex');
    expect(badge.closest('button')).toHaveTextContent(/Codex \(as recorded by the publisher\)/);
  });

  it('shows no agent for an unprefixed id', () => {
    for (const conversation of ['4f2a9c1e-0000-4000-8000-000000000001', '4f2a9c1e-0000-4000-8000-000000000001:agent-a1b2', 'conv-example-1']) {
      const el = card({ ...promptStep, conversation_id: conversation });
      expect(el.querySelector('[data-agent]')).toBeNull();
      cleanup();
    }
  });

  it('is on the triad layout too', () => {
    const el = card({ ...mcpNode, conversation_id: 'cursor:c1' });
    expect(within(el).getByText('Cursor')).toHaveAttribute('title', 'Agent: Cursor, as recorded by the publisher');
  });
});

describe('the reasoning pane of a transcript step', () => {
  const step = (cot: string | null) => ({ ...promptStep, conversation_id: 'codex:thr_1', model_cot: cot });

  it('labels a verified provider summary and strips the marker', () => {
    const el = card(step(marked(promptStep.id, 'Looking at the retry loop.')));
    const pane = within(el).getByText("Provider summary of the model's reasoning").parentElement!;
    expect(within(pane).getByText('Looking at the retry loop.')).toBeInTheDocument();
    expect(el.textContent).not.toContain('~cvc-');
    expect(el.textContent).not.toContain('[cvc: provider summary');
    expect(within(el).queryByText('Reasoning')).toBeNull();
  });

  it('shows a marker that does not verify literally, under the label Reasoning', () => {
    const forged = marked('6b1f0c0e-1111-4aaa-8bbb-000000000002', 'Looking at the retry loop.');
    const el = card(step(forged));
    const pane = within(el).getByText('Reasoning').parentElement!;
    expect(pane.textContent).toContain('[cvc: provider summary of the model\'s reasoning ~cvc-');
    expect(pane.textContent).toContain('Looking at the retry loop.');
    expect(within(el).queryByText("Provider summary of the model's reasoning")).toBeNull();
  });

  it('keeps the label Reasoning for unmarked text', () => {
    const el = card(step('I should read the file first.'));
    expect(within(el).getByText('Reasoning')).toBeInTheDocument();
  });

  it('labels the withheld note "Withheld" and does not draw the capture\'s sentence', () => {
    const el = card(step('[cvc: withheld 7690 bytes that could not be scrubbed]'));
    const pane = within(el).getByText('Withheld').parentElement!;
    expect(within(pane).getByText('Withheld: 7,690 bytes could not be scrubbed.')).toBeInTheDocument();
    expect(el.textContent).not.toContain('[cvc: withheld');
  });

  it('draws the body of a summary as text even when it reads like the withheld note', () => {
    const note = '[cvc: withheld 12 bytes that could not be scrubbed]';
    const el = card(step(marked(promptStep.id, note)));
    const pane = within(el).getByText("Provider summary of the model's reasoning").parentElement!;
    expect(pane.textContent).toContain(note);
  });

  it('never calls the reasoning full or complete', () => {
    for (const cot of [marked(promptStep.id, 'x'), 'x', '[cvc: withheld 1 bytes that could not be scrubbed]']) {
      const el = card(step(cot));
      expect(el.textContent).not.toMatch(/full reasoning|complete reasoning|real thought/i);
      cleanup();
    }
  });
});

describe('the "Why?" panel of an agent step', () => {
  it('labels a verified provider summary and strips the marker', () => {
    const node = makeNode({ ...mcpNode, model_cot: marked(mcpNode.id, 'Weighed two options.') });
    const el = card(node);
    fireEvent.click(within(el).getByRole('button', { name: 'Why?' }));
    expect(within(el).getByText("Provider summary of the model's reasoning")).toBeInTheDocument();
    expect(within(el).getByText('Weighed two options.')).toBeInTheDocument();
    expect(el.textContent).not.toContain('~cvc-');
  });

  it('keeps Decision Factors for unmarked text', () => {
    const el = card(mcpNode);
    fireEvent.click(within(el).getByRole('button', { name: 'Why?' }));
    expect(within(el).getByText('Decision Factors')).toBeInTheDocument();
  });
});
