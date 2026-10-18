/**
 * Selecting a node, and selecting text. Dragging across text to copy it ends in a
 * click on the card; that click must not also select the node (which would move
 * the diff pane under the reader's hands). The rule applies to both layouts and
 * changes no markup.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TimelineNode } from './TimelineNode';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { failingStep, mcpNode, promptStep } from '../../test/fixtures';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  window.getSelection()?.removeAllRanges();
  cleanup();
  restoreDomStubs();
});

/** Selects everything inside `node`, as dragging across it would. */
function selectContentsOf(node: Node) {
  const range = document.createRange();
  range.selectNodeContents(node);
  const selection = window.getSelection()!;
  selection.removeAllRanges();
  selection.addRange(range);
}

/**
 * Selects from `anchor` to `focus`, as a drag does, in either direction (a Range
 * is always forward; a backward drag needs the Selection API).
 */
function selectBetween(anchor: Node, focus: Node) {
  const selection = window.getSelection()!;
  selection.removeAllRanges();
  selection.setBaseAndExtent(anchor, 0, focus, 1);
}

const LAYOUTS: Array<[string, InteractionNode, string]> = [
  ['triad', mcpNode, 'Thought'],
  ['transcript', promptStep, 'AI step'],
];

describe.each(LAYOUTS)('a %s card', (_layout, node, heading) => {
  const setup = (extra: { onSelect?: () => void } = {}) => {
    const onSelect = extra.onSelect ?? vi.fn();
    const view = render(
      <div>
        <TimelineNode interaction={node} onSelect={onSelect} />
        <p data-testid="outside">text outside the card</p>
      </div>,
    );
    const card = view.container.querySelector('[role="button"][aria-pressed]') as HTMLElement;
    return { ...view, card, onSelect: onSelect as ReturnType<typeof vi.fn> };
  };

  it('is selected by a click when no text is selected', () => {
    const { card, onSelect } = setup();
    fireEvent.click(within(card).getByText(heading));
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  it('is not selected by the click that ends a drag across its text', () => {
    const { card, onSelect } = setup();
    const text = within(card).getByText(heading);
    selectContentsOf(text);
    expect(window.getSelection()!.isCollapsed).toBe(false);
    fireEvent.click(text);
    fireEvent.click(card);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('is not selected when only one end of the selection is inside it', () => {
    const { card, onSelect } = setup();
    const outside = screen.getByTestId('outside');
    const inside = within(card).getByText(heading);

    selectBetween(inside.firstChild!, outside.firstChild!); // anchor inside, focus outside
    expect(window.getSelection()!.isCollapsed).toBe(false);
    fireEvent.click(card);
    selectBetween(outside.firstChild!, inside.firstChild!); // anchor outside, focus inside (a backward drag)
    expect(window.getSelection()!.isCollapsed).toBe(false);
    expect(window.getSelection()!.focusNode).toBe(inside.firstChild);
    fireEvent.click(card);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('is selected when the selection is elsewhere on the page', () => {
    const { card, onSelect } = setup();
    selectContentsOf(screen.getByTestId('outside'));
    expect(window.getSelection()!.isCollapsed).toBe(false);
    fireEvent.click(card);
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  it('is selected when the selection is only a caret, even inside it', () => {
    const { card, onSelect } = setup();
    const text = within(card).getByText(heading).firstChild!;
    const range = document.createRange();
    range.setStart(text, 1);
    range.collapse(true);
    const selection = window.getSelection()!;
    selection.removeAllRanges();
    selection.addRange(range);
    expect(selection.isCollapsed).toBe(true);
    fireEvent.click(card);
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  it('is selected again once the selection is cleared', () => {
    const { card, onSelect } = setup();
    selectContentsOf(within(card).getByText(heading));
    fireEvent.click(card);
    expect(onSelect).not.toHaveBeenCalled();

    window.getSelection()!.removeAllRanges();
    fireEvent.click(card);
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  it('is still selected from the keyboard while text is selected: only the mouse click is held back', () => {
    const { card, onSelect } = setup();
    selectContentsOf(within(card).getByText(heading));
    fireEvent.keyDown(card, { key: 'Enter' });
    fireEvent.keyDown(card, { key: ' ' });
    expect(onSelect).toHaveBeenCalledTimes(2);
  });

  it('keeps the selection it did not touch', () => {
    const { card } = setup();
    const text = within(card).getByText(heading);
    selectContentsOf(text);
    fireEvent.click(card);
    expect(window.getSelection()!.isCollapsed).toBe(false);
    expect(window.getSelection()!.toString()).toBe(text.textContent);
  });
});

describe('controls inside a card, with text selected', () => {
  it('still work, and still do not select the card', () => {
    const onSelect = vi.fn();
    const { container } = render(<TimelineNode interaction={failingStep} onSelect={onSelect} />);
    const card = container.firstElementChild as HTMLElement;
    selectContentsOf(within(card).getByText('Response'));

    const toggle = within(card).getByRole('button', { name: /^Tool results/ });
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expect(onSelect).not.toHaveBeenCalled();
  });
});

describe('the card, as markup', () => {
  it('is the same with or without a selection: the rule is behaviour only', () => {
    const html = (selected: boolean) => {
      const { container, unmount } = render(<TimelineNode interaction={mcpNode} />);
      if (selected) selectContentsOf(within(container).getByText('Thought'));
      const markup = container.innerHTML;
      unmount();
      window.getSelection()!.removeAllRanges();
      return markup;
    };
    expect(html(true)).toBe(html(false));
  });
});
