/**
 * The keyboard, from the point of view of the page (issue #135): Tab reaches the
 * header of a card first, Enter or Space on it selects the step, and the page-wide
 * Space shortcut (which marks the open file viewed) leaves the header, and every
 * control inside a card, alone: cancelling Space on a button would stop it from
 * being pressed.
 *
 * jsdom does not press a button for Enter or Space, as a browser does, so the click
 * that follows the key is made by the test; what is checked is that the keys reach
 * a real button uncancelled and that its click selects.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { useKeyboardNavigation } from '../../hooks/useKeyboardNavigation';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { failingStep, mcpNode, promptStep } from '../../test/fixtures';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

/** The Reviewer's page in little: the timeline, and the shortcuts that listen to the whole document. */
function Page({
  interactions,
  onSelect,
  onMarkViewed,
}: {
  interactions: InteractionNode[];
  onSelect: (id: string) => void;
  onMarkViewed: () => void;
}) {
  useKeyboardNavigation({ onNext: () => {}, onPrevious: () => {}, onExpand: () => {}, onMarkViewed });
  return <ShadowTimeline interactions={interactions} onSelectInteraction={onSelect} />;
}

/** The steps in the order the timeline lists them (newest first), whatever order they are given in. */
const newestFirst = (nodes: InteractionNode[]) => [...nodes].sort((a, b) => b.timestamp - a.timestamp);

const NODES = newestFirst([mcpNode, promptStep, failingStep]);

function setup(interactions: InteractionNode[] = NODES) {
  const onSelect = vi.fn();
  const onMarkViewed = vi.fn();
  const view = render(<Page interactions={interactions} onSelect={onSelect} onMarkViewed={onMarkViewed} />);
  const cards = Array.from(view.container.querySelectorAll<HTMLElement>('ol > li'));
  return { ...view, cards, onSelect, onMarkViewed };
}

const headerOf = (card: HTMLElement) => card.querySelector('button[aria-pressed]') as HTMLButtonElement;

describe('Tab and the header of a card', () => {
  it('has one stop for each card before anything inside it: its header', () => {
    const { cards } = setup();
    expect(cards).toHaveLength(3);
    for (const card of cards) {
      const stops = Array.from(card.querySelectorAll<HTMLElement>('button, a[href], [tabindex="0"]'));
      expect(stops[0], card.textContent!.slice(0, 40)).toBe(headerOf(card));
      expect(card).not.toHaveAttribute('tabindex');
    }
  });

  it('lets the header take focus, and does nothing to the page when it does', () => {
    const { cards, onSelect, onMarkViewed } = setup();
    const header = headerOf(cards[1]);
    header.focus();
    expect(document.activeElement).toBe(header);
    expect(onSelect).not.toHaveBeenCalled();
    expect(onMarkViewed).not.toHaveBeenCalled();
  });
});

describe('Enter and Space on the header of a card', () => {
  it('are not cancelled, so the browser presses the button, and the press selects the step', () => {
    const { cards, onSelect, onMarkViewed } = setup();
    const header = headerOf(cards[1]);
    header.focus();

    // A cancelled keydown would stop a browser from turning Enter or Space into a click.
    expect(fireEvent.keyDown(header, { key: 'Enter' })).toBe(true);
    fireEvent.click(header); // Enter: the click comes with the key press
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenLastCalledWith(NODES[1].id);

    expect(fireEvent.keyDown(header, { key: ' ' })).toBe(true);
    expect(fireEvent.keyUp(header, { key: ' ' })).toBe(true);
    fireEvent.click(header); // Space: the click comes on key up
    expect(onSelect).toHaveBeenCalledTimes(2);

    // The page-wide shortcut for Space did not take them: the file was not marked viewed.
    expect(onMarkViewed).not.toHaveBeenCalled();
  });

  it('select the step of the card they are on, and no other', () => {
    const { cards, onSelect } = setup();
    for (const [index, node] of NODES.entries()) {
      fireEvent.click(headerOf(cards[index]));
      expect(onSelect).toHaveBeenLastCalledWith(node.id);
    }
    expect(onSelect).toHaveBeenCalledTimes(3);
  });

  it('show which step is selected, on the header, once the page says so', () => {
    const onSelect = vi.fn();
    const { rerender } = render(<ShadowTimeline interactions={[mcpNode, promptStep]} onSelectInteraction={onSelect} />);
    expect(screen.queryAllByRole('button', { pressed: true })).toHaveLength(0);
    rerender(<ShadowTimeline interactions={[mcpNode, promptStep]} selectedInteractionId={promptStep.id} onSelectInteraction={onSelect} />);
    const pressed = screen.getByRole('button', { pressed: true });
    expect(pressed.closest('li')).toHaveTextContent('Add a retry limit');
  });
});

describe('the page-wide Space shortcut', () => {
  it('leaves the header of a card alone: it is a button', () => {
    const { cards, onMarkViewed } = setup();
    const event = new KeyboardEvent('keydown', { key: ' ', bubbles: true, cancelable: true });
    headerOf(cards[0]).dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
    expect(onMarkViewed).not.toHaveBeenCalled();
  });

  it('leaves every control in a card alone, and every block of code that scrolls', () => {
    const { cards, onMarkViewed } = setup([failingStep]);
    const card = cards[0];
    const owners = Array.from(card.querySelectorAll<HTMLElement>('button, [tabindex="0"]'));
    expect(owners.length).toBeGreaterThan(2);
    for (const owner of owners) {
      const event = new KeyboardEvent('keydown', { key: ' ', bubbles: true, cancelable: true });
      owner.dispatchEvent(event);
      expect(event.defaultPrevented, owner.outerHTML.slice(0, 60)).toBe(false);
    }
    expect(onMarkViewed).not.toHaveBeenCalled();
  });

  it('still marks the file viewed from the page itself, and from text in a card that nothing owns', () => {
    const { cards, onMarkViewed } = setup();
    const onPage = new KeyboardEvent('keydown', { key: ' ', bubbles: true, cancelable: true });
    document.body.dispatchEvent(onPage);
    expect(onPage.defaultPrevented).toBe(true);
    expect(onMarkViewed).toHaveBeenCalledTimes(1);

    // The card is not a control, so Space from inside it is not a control's: it is the shortcut's.
    const inCard = new KeyboardEvent('keydown', { key: ' ', bubbles: true, cancelable: true });
    const triad = cards.find((card) => within(card).queryByText('Thought') !== null)!;
    within(triad).getByText('Thought').dispatchEvent(inCard);
    expect(inCard.defaultPrevented).toBe(true);
    expect(onMarkViewed).toHaveBeenCalledTimes(2);
  });
});
