/**
 * Selecting a step must not redraw the others. A card can be thousands of
 * elements, so a click that redrew every card was slow however light each card was
 * to build once.
 *
 * These tests use the REAL framer-motion, not the stub the other timeline tests
 * use: whether a memoised card still bails out in the tree that ships is what they
 * are about. (The cards no longer animate through framer-motion, only the panel a
 * card opens does, so this is about the memoisation, not about the library.)
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { injectionParentStep, injectionSystemStep, makeNode, promptStep } from '../../test/fixtures';
import type { InteractionNode } from '../../types/cvc';

/** The id of the step each draw of a card was for, in order. */
const drawn = vi.hoisted(() => ({ ids: [] as string[] }));

vi.mock('./TimelineNode', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./TimelineNode')>();
  return {
    ...actual,
    TimelineNode: (props: Parameters<typeof actual.TimelineNode>[0]) => {
      drawn.ids.push(props.interaction.id);
      return actual.TimelineNode(props);
    },
  };
});

beforeEach(() => {
  installDomStubs();
  drawn.ids.length = 0;
});
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

/** Six steps, newest last in the array (the timeline shows them newest first). */
const steps: InteractionNode[] = Array.from({ length: 6 }, (_, i) =>
  makeNode({
    ...promptStep,
    id: `1f0e5c3a-9d2b-8c4e-a1b7-00000000010${i}`,
    timestamp: promptStep.timestamp + i * 1000,
    user_prompt: `Step ${i}`,
  }),
);

/** Forgets the draws so far, so the next ones can be counted. */
const forget = () => {
  drawn.ids.length = 0;
};

describe('selecting a step', () => {
  it('draws once per step to begin with', () => {
    render(<ShadowTimeline interactions={steps} onSelectInteraction={() => {}} />);
    expect([...drawn.ids].sort()).toEqual(steps.map((step) => step.id).sort());
  });

  it('redraws the step that lost the selection and the one that gained it, and no other', () => {
    const select = () => {};
    const { rerender } = render(
      <ShadowTimeline interactions={steps} selectedInteractionId={steps[1].id} onSelectInteraction={select} />,
    );
    forget();
    rerender(<ShadowTimeline interactions={steps} selectedInteractionId={steps[4].id} onSelectInteraction={select} />);
    expect([...drawn.ids].sort()).toEqual([steps[1].id, steps[4].id].sort());
    // The header of a card is the button that says whether it is selected.
    expect(screen.getAllByRole('button', { pressed: true })).toHaveLength(1);
    expect(screen.getByRole('button', { pressed: true }).closest('li')).toHaveTextContent('Step 4');
  });

  it('redraws only the step that gained it when nothing had been selected, and only the one that lost it when cleared', () => {
    const select = () => {};
    const { rerender } = render(<ShadowTimeline interactions={steps} onSelectInteraction={select} />);
    forget();
    rerender(<ShadowTimeline interactions={steps} selectedInteractionId={steps[2].id} onSelectInteraction={select} />);
    expect(drawn.ids).toEqual([steps[2].id]);
    forget();
    rerender(<ShadowTimeline interactions={steps} selectedInteractionId={null} onSelectInteraction={select} />);
    expect(drawn.ids).toEqual([steps[2].id]);
  });

  it('redraws nothing when the parent passes a new function for the same job', () => {
    const { rerender } = render(
      <ShadowTimeline interactions={steps} selectedInteractionId={steps[0].id} onSelectInteraction={() => {}} />,
    );
    forget();
    for (let pass = 0; pass < 3; pass++) {
      rerender(<ShadowTimeline interactions={steps} selectedInteractionId={steps[0].id} onSelectInteraction={() => {}} />);
    }
    expect(drawn.ids).toEqual([]);
  });

  it('still calls the newest function, with the id of the step clicked', () => {
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = render(<ShadowTimeline interactions={steps} onSelectInteraction={first} />);
    rerender(<ShadowTimeline interactions={steps} onSelectInteraction={second} />);
    fireEvent.click(screen.getByText('Step 3'));
    expect(second).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledWith(steps[3].id);
    expect(first).not.toHaveBeenCalled();

    // ...and the keyboard reaches it the same way, through the header (a browser turns Enter into this click).
    fireEvent.click(within(screen.getByText('Step 5').closest('li')!).getByRole('button', { pressed: false }));
    expect(second).toHaveBeenLastCalledWith(steps[5].id);
    expect(second).toHaveBeenCalledTimes(2);
  });

  it('does not fail when the parent passes no function', () => {
    render(<ShadowTimeline interactions={steps} />);
    expect(() => fireEvent.click(screen.getByText('Step 2'))).not.toThrow();
  });
});

describe('what a step is handed', () => {
  it('keeps a step that has a parent still while the interactions are the same ones', () => {
    // injectionSystemStep's parent is loaded, so it is handed a list of that parent's calls.
    const nodes = [injectionParentStep, injectionSystemStep];
    const { rerender } = render(<ShadowTimeline interactions={nodes} selectedInteractionId={null} />);
    forget();
    rerender(<ShadowTimeline interactions={nodes} selectedInteractionId={injectionParentStep.id} />);
    // Only the step selected is drawn again, not its child that holds the list of calls.
    expect(drawn.ids).toEqual([injectionParentStep.id]);
  });

  it('adds a step by drawing only it (the steps before it are unchanged)', () => {
    const { rerender } = render(<ShadowTimeline interactions={steps.slice(0, 4)} />);
    forget();
    rerender(<ShadowTimeline interactions={steps.slice(0, 5)} />);
    // Steps without a parent are handed null for it: nothing about them changed.
    expect(drawn.ids).toEqual([steps[4].id]);
  });
});
