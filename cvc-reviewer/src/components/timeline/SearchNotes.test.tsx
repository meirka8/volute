/**
 * What a card says about a search beyond its marks (issue #118, revision round): how many marks a card draws, and the
 * note about a match in a field the card does not draw.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { highlightTerms, parseQuery } from '../../lib/timelineSearch';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { generatedLink, makeNode, toolCall } from '../../test/fixtures';
import { MARKS_PER_CARD } from './markBudget';
import { HighlightContext } from './searchHighlight';
import { TimelineNode } from './TimelineNode';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(() => installDomStubs());
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

const HASH = 'abcdef0123456789abcdef0123456789abcdef01';
/** A diff of `lines` lines, each of which changes and holds the letter "e" four times. */
function bigEdit(lines: number) {
  const make = (word: string) => Array.from({ length: lines }, (_, k) => `${word} line ${k} e e e e`).join('\n');
  return makeNode({
    id: '1f0e5c3a-9d2b-8c4e-a1b7-0000000000e1',
    author: 'human',
    user_prompt: 'p',
    tool_executions: [toolCall('Edit', { file_path: '/repo/a.ts', old_string: make('old'), new_string: make('new') })],
  });
}

function card(node: ReturnType<typeof makeNode>, query: string, strict = false) {
  const tree = (
    <HighlightContext.Provider value={highlightTerms(parseQuery(query))}>
      <TimelineNode interaction={node} />
    </HighlightContext.Provider>
  );
  return render(strict ? <StrictMode>{tree}</StrictMode> : tree);
}
const marksIn = (root: ParentNode) => root.querySelectorAll('mark').length;
const budgetNote = () => screen.queryByText(`Only the first ${MARKS_PER_CARD} matches on this card are marked.`);

describe('the marks of a card', () => {
  it('are at most the budget of the card, however many blocks of text the card draws and however much they match', () => {
    const { container } = card(bigEdit(700), 'e');
    // The diff is cut to its first rows until it is opened: open it (all 1,400 rows, which hold thousands of matches).
    fireEvent.click(screen.getByRole('button', { name: /^Show all/ }));
    expect(container.querySelectorAll('ins, del').length).toBeGreaterThan(1_000);
    expect(marksIn(container)).toBeLessThanOrEqual(MARKS_PER_CARD);
    expect(marksIn(container)).toBeGreaterThan(MARKS_PER_CARD - 20);
    // And the card says so.
    expect(budgetNote()).toBeInTheDocument();
  });

  it('are counted by the fold in full, whatever is drawn', () => {
    card(bigEdit(700), 'e');
    expect(screen.getByRole('button', { name: /^Show all 1,400 lines · 1000\+ matches$/ })).toBeInTheDocument();
  });

  it('are not limited, and the card says nothing, when the card holds fewer matches than the budget', () => {
    const { container } = card(bigEdit(20), 'e');
    fireEvent.click(screen.getByRole('button', { name: /^Show all/ }));
    expect(marksIn(container)).toBeGreaterThan(40);
    expect(marksIn(container)).toBeLessThan(MARKS_PER_CARD);
    expect(budgetNote()).toBeNull();
  });

  it('start again with the next search: a term with few matches is drawn whole, and the note goes', () => {
    const view = card(bigEdit(700), 'e');
    fireEvent.click(screen.getByRole('button', { name: /^Show all/ }));
    expect(budgetNote()).toBeInTheDocument();
    view.rerender(
      <HighlightContext.Provider value={highlightTerms(parseQuery('"line 69"'))}>
        <TimelineNode interaction={bigEdit(700)} />
      </HighlightContext.Provider>,
    );
    expect(budgetNote()).toBeNull();
    expect(marksIn(view.container)).toBeGreaterThan(0);
    expect(marksIn(view.container)).toBeLessThan(MARKS_PER_CARD);
  });

  it('give back what a block that goes away held: folding a long text away makes room for the rest of the card', () => {
    const { container } = card(bigEdit(700), 'e');
    const toggle = () => screen.getByRole('button', { name: /^(Show all|Show less)/ });
    fireEvent.click(toggle()); // open: the budget is spent on the rows
    expect(budgetNote()).toBeInTheDocument();
    fireEvent.click(toggle()); // closed: only the first rows are drawn again
    expect(budgetNote()).toBeNull();
    expect(marksIn(container)).toBeLessThan(MARKS_PER_CARD);
  });

  it('are the same under StrictMode, which draws everything twice', () => {
    const { container } = card(bigEdit(700), 'e', true);
    fireEvent.click(screen.getByRole('button', { name: /^Show all/ }));
    expect(marksIn(container)).toBeLessThanOrEqual(MARKS_PER_CARD);
    expect(marksIn(container)).toBeGreaterThan(MARKS_PER_CARD - 20);
    expect(budgetNote()).toBeInTheDocument();
  });

  it('are not limited for a text drawn on its own, with no card around it', () => {
    // (A card has a budget; a block with none is limited by the block's own hundred.)
    const node = makeNode({ id: '1f0e5c3a-9d2b-8c4e-a1b7-0000000000e2', author: 'human', user_prompt: 'p', model_response: 'e '.repeat(1_000) });
    const { container } = card(node, 'e');
    expect(marksIn(container)).toBe(100);
    expect(budgetNote()).toBeNull();
  });
});

describe('a match in a field the card does not draw', () => {
  const node = makeNode({
    id: '1f0e5c3a-9d2b-8c4e-a1b7-0000000000f1',
    author: 'human',
    user_prompt: 'Fix the import',
    model_response: 'Done.',
    context_items: [{ file_path: '/work/repo/src/secret_module.ts' }],
    artifact_links: [generatedLink('1f0e5c3a-9d2b-8c4e-a1b7-0000000000f1', HASH)],
  });

  it('is said in one line: the files of the context of a step are on no card', () => {
    card(node, 'path:secret_module');
    expect(screen.getByText('Matched in: context file paths, which this card does not show.')).toBeInTheDocument();
    expect(screen.getByRole('note')).toHaveTextContent(/^Matched in: context file paths/);
  });

  it('is said for a plain term too, and not for one that is not in those files', () => {
    const { unmount } = card(node, 'secret_module');
    expect(screen.getByText(/^Matched in: /)).toBeInTheDocument();
    unmount();
    card(node, 'import');
    expect(screen.queryByText(/^Matched in: /)).toBeNull();
  });

  it('is not said for a term with another prefix, which does not look in files', () => {
    card(node, 'tool:secret_module');
    expect(screen.queryByText(/^Matched in: /)).toBeNull();
  });

  it('is not said while nothing is searched, and a card with no context says nothing', () => {
    const { unmount } = render(<TimelineNode interaction={node} />);
    expect(screen.queryByText(/^Matched in: /)).toBeNull();
    unmount();
    card(makeNode({ id: '1f0e5c3a-9d2b-8c4e-a1b7-0000000000f2', author: 'human', user_prompt: 'secret_module' }), 'secret_module');
    expect(screen.queryByText(/^Matched in: /)).toBeNull();
  });
});
