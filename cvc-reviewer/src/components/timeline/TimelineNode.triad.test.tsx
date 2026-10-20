/**
 * Regression guard for the Thought / Action / Observation layout.
 *
 * MCP, VS Code/LSP, `cvc run` and legacy nodes must keep rendering exactly as
 * they did before the Claude Code transcript layout existed (issue #105). The
 * snapshots below were recorded against the pre-change `TimelineNode`; a diff
 * here means the triad markup changed and needs a deliberate decision.
 *
 * One such decision has been made, for issue #135: the card became a list item
 * named by its header, and the header became the button that selects it (so the
 * card no longer is a button that holds buttons). The snapshots were recorded
 * again for that, and for it only: the wrapper and the header changed, and the
 * Thought, Action and Observation panes, the "Why?" panel and the commit chips
 * inside the card are as they were.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TimelineNode } from './TimelineNode';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import {
  bareExternalNode,
  legacyAgentWithToolsNode,
  lspNode,
  mcpNode,
  mcpNodeNoReasoning,
  runNode,
} from '../../test/fixtures';
import { renumberIds } from '../../test/markup';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

/**
 * Serializable copy of the rendered card. Icon path data is dropped (the class
 * names still say which icon was used) so an icon-library bump does not break
 * a test that is about our own markup. The ids that point at each other (the
 * card's name) are renumbered in order, since React counts them up across the
 * whole run; what points at what is kept.
 */
function snapshotOf(container: HTMLElement): Element {
  const clone = container.firstElementChild!.cloneNode(true) as Element;
  clone.querySelectorAll('svg').forEach((svg) => svg.replaceChildren());
  renumberIds(clone);
  return clone;
}

function renderNode(node: InteractionNode, props: { isSelected?: boolean; onSelect?: () => void } = {}) {
  return render(<TimelineNode interaction={node} {...props} />);
}

describe('TimelineNode triad layout: DOM snapshots', () => {
  it('MCP node with reasoning', () => {
    const { container } = renderNode(mcpNode);
    expect(snapshotOf(container)).toMatchSnapshot();
  });

  it('MCP node without reasoning', () => {
    const { container } = renderNode(mcpNodeNoReasoning);
    expect(snapshotOf(container)).toMatchSnapshot();
  });

  it('VS Code / LSP node', () => {
    const { container } = renderNode(lspNode);
    expect(snapshotOf(container)).toMatchSnapshot();
  });

  it('cvc run node', () => {
    const { container } = renderNode(runNode);
    expect(snapshotOf(container)).toMatchSnapshot();
  });

  it('agent node that carries tool executions', () => {
    const { container } = renderNode(legacyAgentWithToolsNode);
    expect(snapshotOf(container)).toMatchSnapshot();
  });

  it('node with nothing recorded and an external author', () => {
    const { container } = renderNode(bareExternalNode);
    expect(snapshotOf(container)).toMatchSnapshot();
  });

  it('selected card', () => {
    const { container } = renderNode(mcpNode, { isSelected: true });
    expect(snapshotOf(container)).toMatchSnapshot();
  });

  it('MCP node with the "Why?" panel open', () => {
    const { container } = renderNode(mcpNode);
    fireEvent.click(screen.getByRole('button', { name: 'Why?' }));
    expect(snapshotOf(container)).toMatchSnapshot();
  });
});

describe('TimelineNode triad layout: labels and behaviour', () => {
  it('labels an MCP node as an AI Agent that needs review', () => {
    renderNode(mcpNode);
    expect(screen.getByText('AI Agent')).toBeInTheDocument();
    expect(screen.getByText('Review required')).toBeInTheDocument();
    expect(screen.getByText('Thought')).toBeInTheDocument();
    expect(screen.getByText('Action')).toBeInTheDocument();
    expect(screen.getByText('Observation')).toBeInTheDocument();
    expect(screen.getByText(mcpNode.user_prompt!)).toBeInTheDocument();
    expect(screen.getByText(mcpNode.model_response!)).toBeInTheDocument();
    expect(screen.getByText('generated a1b2c3d')).toBeInTheDocument();
  });

  it('labels LSP and run nodes by their author with the green signal', () => {
    const { unmount } = renderNode(lspNode);
    expect(screen.getByText('User')).toBeInTheDocument();
    expect(screen.getByText('Green check')).toBeInTheDocument();
    unmount();

    renderNode(runNode);
    expect(screen.getByText('System')).toBeInTheDocument();
    expect(screen.getByText('Green check')).toBeInTheDocument();
  });

  it('keeps tool executions as raw "name arguments" action lines on agent nodes', () => {
    renderNode(legacyAgentWithToolsNode);
    expect(screen.getByText('AI Agent')).toBeInTheDocument();
    expect(screen.getByText('Critical alert')).toBeInTheDocument();
    expect(screen.getByText('Bash {"command":"npm run build"}')).toBeInTheDocument();
  });

  it('falls back to the placeholder copy when nothing was recorded', () => {
    renderNode(bareExternalNode);
    expect(screen.getByText('External')).toBeInTheDocument();
    expect(screen.getByText('No explicit thought was recorded for this step.')).toBeInTheDocument();
    expect(screen.getByText('No recorded tool execution.')).toBeInTheDocument();
    expect(screen.getByText('No recorded observation.')).toBeInTheDocument();
  });

  it('opens and closes the reasoning panel without selecting the card', () => {
    const onSelect = vi.fn();
    renderNode(mcpNode, { onSelect });
    const why = screen.getByRole('button', { name: 'Why?' });

    fireEvent.click(why);
    expect(screen.getByText('Decision Factors')).toBeInTheDocument();
    expect(onSelect).not.toHaveBeenCalled();

    fireEvent.click(why);
    expect(screen.queryByText('Decision Factors')).not.toBeInTheDocument();
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('offers no "Why?" control when there is no reasoning', () => {
    renderNode(mcpNodeNoReasoning);
    expect(screen.queryByRole('button', { name: 'Why?' })).not.toBeInTheDocument();
  });

  it('selects on a click on the header (which Enter or Space on it make) and in the body, but not for keys in nested controls', () => {
    const onSelect = vi.fn();
    renderNode(mcpNode, { onSelect });
    const header = screen.getByRole('button', { pressed: false });

    fireEvent.click(header);
    expect(onSelect).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByText('Thought'));
    expect(onSelect).toHaveBeenCalledTimes(2);

    fireEvent.keyDown(screen.getByRole('button', { name: 'Why?' }), { key: 'Enter' });
    expect(onSelect).toHaveBeenCalledTimes(2);
  });

  it('reflects selection in aria-pressed, on the header', () => {
    renderNode(mcpNode, { isSelected: true });
    const header = screen.getByRole('button', { pressed: true });
    expect(header).toHaveTextContent('AI Agent');
    expect(screen.queryByRole('button', { pressed: false, name: /^AI Agent/ })).not.toBeInTheDocument();
  });

  it('offers "Show more" once the thought overflows its clamp, and toggles it', async () => {
    vi.spyOn(Element.prototype, 'scrollHeight', 'get').mockReturnValue(200);
    vi.spyOn(Element.prototype, 'clientHeight', 'get').mockReturnValue(40);
    const onSelect = vi.fn();
    renderNode(mcpNode, { onSelect });

    const [more] = await screen.findAllByRole('button', { name: 'Show more' });
    const thought = screen.getByText(mcpNode.user_prompt!);
    expect(thought).toHaveClass('line-clamp-4');

    fireEvent.click(more);
    await waitFor(() => expect(thought).not.toHaveClass('line-clamp-4'));
    expect(screen.getAllByRole('button', { name: 'Show less' }).length).toBeGreaterThan(0);
    expect(onSelect).not.toHaveBeenCalled();
  });
});
