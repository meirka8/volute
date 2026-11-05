/**
 * What a card marks and counts in a stimulus whose markers verified, or did not (issues #118 and #133).
 *
 * The card draws a verified stimulus part by part, and a stimulus whose tokens do not verify as one block of text.
 * The search marks the text of the parts, counts the matches a closed fold or an undrawn page holds, and leaves the
 * Reviewer's own words (the labels, the notes about what the capture cut or withheld) and the markers alone.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { highlightTerms, parseQuery } from '../../lib/timelineSearch';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { makeNode } from '../../test/fixtures';
import { writeStimulus, type PartSpec } from '../../test/stimulusWriter';
import { NodeUiProvider } from './NodeUiScope';
import { NodeUiStore } from './nodeUiState';
import { HighlightContext } from './searchHighlight';
import { TimelineNode } from './TimelineNode';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(() => installDomStubs());
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

const ID = '1f0e5c3a-9d2b-8c4e-a1b7-000000000001';

function step(parts: PartSpec[], author: 'human' | 'system' = 'human') {
  return makeNode({ id: ID, author, user_prompt: writeStimulus(ID, parts), model_response: 'Done.' });
}

function searched(node: ReturnType<typeof makeNode>, query: string) {
  return render(
    <HighlightContext.Provider value={highlightTerms(parseQuery(query))}>
      <TimelineNode interaction={node} />
    </HighlightContext.Provider>,
  );
}
/** The fold whose label matches (a card's header is a button too, but it is not a fold). */
const fold = (label: RegExp): HTMLElement => {
  const found = screen.getAllByRole('button').find((button) => button.hasAttribute('aria-expanded') && label.test(button.textContent ?? ''));
  if (found === undefined) throw new Error(`no fold labelled ${label}`);
  return found;
};
const marks = (root: ParentNode) => Array.from(root.querySelectorAll('mark')).map((mark) => mark.textContent);

describe("a person's words in a verified stimulus", () => {
  it('are marked, and the marker words and the boundary token are not there to mark', () => {
    const node = step([{ kind: 'prompt', text: 'Rotate the staging certificate' }]);
    const { container } = searched(node, 'staging');
    expect(marks(container)).toEqual(['staging']);
    cleanup();
    for (const words of ['prompt', '~cvc-']) {
      const none = searched(node, words);
      expect(marks(none.container), words).toEqual([]);
      cleanup();
    }
  });

  it('draw the note about a cut part in the Reviewer\'s words, which a search does not mark', () => {
    const node = step([{ kind: 'prompt', cut: { head: 'The beginning', bytes: 1234, tail: 'The end' } }]);
    const { container } = searched(node, 'bytes');
    expect(container).toHaveTextContent('1,234 bytes truncated');
    expect(marks(container)).toEqual([]);
    cleanup();
    const found = searched(node, 'beginning');
    expect(marks(found.container)).toEqual(['beginning']);
  });
});

describe('the tool results of a verified stimulus', () => {
  it('say how many matches they hold while they are folded, and show the marks once they are open', () => {
    // A step the system started: its tool results start folded.
    const node = step([{ kind: 'tool_result', name: 'Bash', text: 'migration 0042 applied' }], 'system');
    const { container } = searched(node, 'migration');
    const toggle = fold(/Tool results/);
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(toggle).toHaveTextContent('· 1 match');
    expect(marks(container)).toEqual([]);

    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expect(toggle).not.toHaveTextContent('match');
    expect(marks(container)).toEqual(['migration']);
  });

  it('count the matches in the name a result is shown under', () => {
    const node = step([{ kind: 'tool_result', name: 'Bash', text: 'ok' }], 'system');
    searched(node, 'bash');
    expect(fold(/Tool results/)).toHaveTextContent('· 1 match');
  });

  it('say how many matches the results not drawn yet hold', () => {
    const parts: PartSpec[] = Array.from({ length: 25 }, (_, n) => ({
      kind: 'tool_result' as const,
      name: 'Read',
      text: n === 24 ? 'the needle is here' : `plain output ${n}`,
    }));
    const node = step(parts, 'human');
    searched(node, 'needle');
    // A page holds 20 results; the five after it hold the match.
    expect(screen.getByRole('button', { name: /Show 5 more tool results/ })).toHaveTextContent('· 1 match');
  });
});

describe('a stimulus whose markers could not be verified', () => {
  const unverified = makeNode({
    id: ID,
    author: 'system',
    user_prompt: '[prompt ~cvc-0123456789abcdef] Please rotate the key',
    model_response: 'Done.',
  });

  it('folds like the tool results do, and says how many matches it holds while it is folded', () => {
    const { container } = searched(unverified, 'rotate');
    const toggle = fold(/Stimulus text/);
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(toggle).toHaveTextContent('· 1 match');
    fireEvent.click(toggle);
    expect(marks(container)).toEqual(['rotate']);
  });

  it('keeps its fold as the reader left it when the timeline draws the card again', () => {
    const store = new NodeUiStore();
    const draw = () => render(<NodeUiProvider store={store}><TimelineNode interaction={unverified} /></NodeUiProvider>);
    const first = draw();
    expect(fold(/Stimulus text/)).toHaveAttribute('aria-expanded', 'false');
    fireEvent.click(fold(/Stimulus text/));
    expect(fold(/Stimulus text/)).toHaveAttribute('aria-expanded', 'true');
    // The card leaves the page (scrolled out of the window, or filtered out) and comes back.
    first.unmount();
    draw();
    expect(fold(/Stimulus text/)).toHaveAttribute('aria-expanded', 'true');
  });

  it('keeps the fold of a group of verified results as the reader left it, too', () => {
    const store = new NodeUiStore();
    const node = step([{ kind: 'tool_result', name: 'Bash', text: 'all good' }], 'system');
    const draw = () => render(<NodeUiProvider store={store}><TimelineNode interaction={node} /></NodeUiProvider>);
    const first = draw();
    fireEvent.click(fold(/Tool results/));
    expect(fold(/Tool results/)).toHaveAttribute('aria-expanded', 'true');
    first.unmount();
    draw();
    expect(fold(/Tool results/)).toHaveAttribute('aria-expanded', 'true');
  });
});
