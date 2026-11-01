/**
 * QA of issue #118: the defects found, each kept as a test of the behaviour that was missing. (They were written red, one
 * for each defect, and every one passes now that the defect is fixed; the file was `qa118-failing.test.tsx`.) Nothing in this
 * file is a wish: each test states a behaviour that the README, the code's own comments, or the way the rest of the feature
 * works already promise. What each asserts is what it asserted when it failed; where the revision round changed the design,
 * the test holds for a better reason, and says so:
 *
 *  - 1 and 1b hold by construction now: the text of a search is never written into the address (M3), and the filters are written
 *    at once and only while the page is the one the timeline was drawn on, so no write is waiting to land on the next page;
 *  - 7 holds because the Action pane writes a call as the name of its tool and its arguments in parts, each marked as what it is;
 *  - 10 and 11 hold because the index holds what a card draws and nothing else.
 *
 *  1. A search still waiting to be written to the address is written into the NEXT page's address when the page
 *     navigates (useSearchUrl.ts, the flush on unmount reads window.location after the router changed it).
 *  2. Focus falls to <body> after "Clear search and filters" in the empty state (ShadowTimeline.tsx, NoMatches).
 *  3. The Action pane of the Thought / Action / Observation layout marks words the Reviewer wrote itself
 *     (TimelineNode.tsx, the HiddenText of the action lines).
 *  4. Chips of a tool call that are the Reviewer's own wording are marked (ToolCallCard.tsx, Chips).
 *  5. A text typed exactly as it is drawn is found and not marked when it holds a zero-width joiner or non-joiner
 *     that prose keeps (Persian, emoji sequences): HiddenText.tsx / searchMarks.ts against timelineSearch.ts.
 *  6. The same for the Turkish capital dotted I.
 *  7. `tool:` and `path:` find a step of the other layout and mark nothing in it (TimelineNode.tsx, unscoped action lines).
 *  8. The "touches the file" filter reads a path that holds a line break as two paths (timelineSearch.ts, paths joined by \n).
 *  9. After navigating to another pull request the page still has the last one's file open (PRReview.tsx), which the
 *     "touches the open file" filter now offers.
 * 10. A call recorded with no input (the JSON text `null`) is found by the word "null" though the card says "No arguments
 *     recorded" (timelineSearch.ts, readArguments keeps any JSON that is not an object as text).
 * 11. The reasoning of a step the other layout draws without a "Why?" (a human's, such as VS Code's) is searched and never
 *     drawn: the step is shown and nothing says why (timelineSearch.ts indexes model_cot; TimelineNode.tsx, showWhyButton).
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Route, Router } from 'wouter';
import { navigate } from 'wouter/use-browser-location';
import { ShadowTimeline } from '../components/timeline/ShadowTimeline';
import { getTimelineIndex, NO_FILTERS, parseQuery, runSearch } from '../lib/timelineSearch';
import { PRReviewPage } from '../pages/PRReview';
import { usePR } from '../hooks/usePR';
import { usePRInteractions } from '../hooks/usePRInteractions';
import { installDomStubs, restoreDomStubs } from './domStubs';
import { generatedLink, makeNode, rawToolCall, toolCall, wireToolCall } from './fixtures';
import { searchFixture } from './searchFixtures';
import type { InteractionNode } from '../types/cvc';

vi.mock('../hooks/usePR', () => ({ usePR: vi.fn() }));
vi.mock('../hooks/usePRInteractions', () => ({ usePRInteractions: vi.fn() }));
vi.mock('../auth/AuthContext', () => ({ useAuth: () => ({ logout: () => {} }) }));
vi.mock('../components/diff/DiffViewer', () => ({
  DiffViewer: ({ filename }: { filename: string }) => <div data-testid="diff">{filename}</div>,
}));
vi.mock('framer-motion', async () => {
  const React = await import('react');
  const ANIMATION = ['layout', 'initial', 'animate', 'exit', 'transition', 'whileHover', 'whileTap'];
  const made = new Map<string, unknown>();
  const motion = new Proxy(
    {},
    {
      get: (_target, tag: string) => {
        if (!made.has(tag)) {
          made.set(
            tag,
            React.forwardRef((props: Record<string, unknown>, ref: React.Ref<unknown>) => {
              const dom: Record<string, unknown> = { ...props };
              for (const name of ANIMATION) delete dom[name];
              return React.createElement(tag, { ...dom, ref });
            }),
          );
        }
        return made.get(tag);
      },
    },
  );
  return { motion, AnimatePresence: ({ children }: { children?: React.ReactNode }) => React.createElement(React.Fragment, null, children) };
});

beforeEach(() => {
  installDomStubs();
  vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame', 'setTimeout', 'clearTimeout'] });
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener: () => {}, removeEventListener: () => {}, addListener: () => {}, removeListener: () => {} }));
  Element.prototype.scrollIntoView = () => {};
  window.history.replaceState(null, '', '/');
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  restoreDomStubs();
  window.history.replaceState(null, '', '/');
});

const HASH = 'abcdef0123456789abcdef0123456789abcdef01';
const box = () => screen.getByRole('searchbox', { name: 'Search steps' }) as HTMLInputElement;
const typeQuery = (value: string) => fireEvent.change(box(), { target: { value } });
const marked = (root: ParentNode) => Array.from(root.querySelectorAll('mark')).map((mark) => mark.textContent ?? '');
const triad = (id: string, extra: Partial<InteractionNode>) => makeNode({ author: 'agent', model_name: 'agent', model_response: 'ok', ...extra, id });

// ---------------------------------------------------------------------------
// 1. The address
// ---------------------------------------------------------------------------

describe('1. the address when the page navigates', () => {
  it('does not write a search that was still waiting into the address of the page that follows', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/1?x=1');
    const { unmount } = render(<ShadowTimeline interactions={searchFixture(20).nodes} />);
    typeQuery('refund'); // the address is written a quarter of a second later: this is the pending write
    // The router changes the address first, and the old page goes away after (as wouter and React do it).
    window.history.pushState(null, '', '/pr/acme/shop/2');
    unmount();
    expect(window.location.pathname + window.location.search).toBe('/pr/acme/shop/2');
  });

  it('(the same through the page) a search typed just before a link to another pull request is followed leaves no trace in its address', () => {
    const ONE = searchFixture(30);
    vi.mocked(usePR).mockImplementation(((_o: string, _r: string, pr: number) => ({
      data: { pr: { title: `PR ${pr}` }, files: [{ filename: 'a.ts', status: 'modified', additions: 1, deletions: 0, patch: '@@ -1 +1 @@\n-a\n+b' }] },
      isLoading: false,
      error: null,
    })) as unknown as typeof usePR);
    vi.mocked(usePRInteractions).mockImplementation((() => ({ data: { interactions: ONE.nodes, commits: ONE.commits, truncated: false, hasHistory: true }, isLoading: false, error: null })) as unknown as typeof usePRInteractions);
    window.history.replaceState(null, '', '/pr/acme/shop/1');
    render(
      <Router>
        <Route path="/pr/:owner/:repo/:pr">
          <PRReviewPage />
        </Route>
      </Router>,
    );
    typeQuery('refund');
    act(() => navigate('/pr/acme/shop/2'));
    expect(window.location.pathname).toBe('/pr/acme/shop/2');
    expect(window.location.search).toBe('');
  });
});

// ---------------------------------------------------------------------------
// 2. Focus
// ---------------------------------------------------------------------------

describe('2. focus after the button of the empty state', () => {
  it('stays in the timeline when "Clear search and filters" is pressed, as it does for the chips and for "Clear filters"', () => {
    const { container } = render(<ShadowTimeline interactions={searchFixture(20).nodes} />);
    typeQuery('no such thing anywhere');
    const button = screen.getByRole('button', { name: 'Clear search and filters' });
    button.focus();
    expect(document.activeElement).toBe(button);
    fireEvent.click(button);
    // The button is gone with the empty state: where is the keyboard now?
    expect(document.activeElement).not.toBe(document.body);
    expect(container.contains(document.activeElement)).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 3 and 4. Marks on what the Reviewer wrote itself
// ---------------------------------------------------------------------------

describe('3. the Action pane of the Thought / Action / Observation layout', () => {
  it('does not mark the word that names the kind of link in the line it writes for a commit', () => {
    // The Action pane has no call to show, so it says what the step is linked to: "generated abcdef0". The word is the Reviewer's.
    const node = triad('3b9f0f1e-6a4c-4d6e-9b1a-000000000001', { user_prompt: 'the generated code', artifact_links: [generatedLink('3b9f0f1e-6a4c-4d6e-9b1a-000000000001', HASH)] });
    const { container } = render(<ShadowTimeline interactions={[node]} />);
    typeQuery('generated');
    expect(marked(container)).toEqual(['generated']); // the prompt's, and no other
  });

  it('does not mark the placeholder it writes when a step has neither a call nor a link', () => {
    const node = triad('3b9f0f1e-6a4c-4d6e-9b1a-000000000002', { user_prompt: 'nothing recorded here' });
    const { container } = render(<ShadowTimeline interactions={[node]} />);
    typeQuery('recorded');
    expect(marked(container)).toEqual(['recorded']); // the prompt's, not "No recorded tool execution."
  });
});

describe('4. the chips of a tool call', () => {
  it('do not mark the Reviewer\'s words ("runs in background", "replace all occurrences", "ignore case")', () => {
    const id = '1f0e5c3a-9d2b-8c4e-a1b7-000000000004';
    const node = makeNode({
      id,
      author: 'human',
      model_name: 'claude-opus-4-5-20251101',
      user_prompt: 'run it in the background and ignore case and replace all occurrences',
      model_response: 'ok',
      tool_executions: [
        wireToolCall('Bash', { command: 'true', run_in_background: true }),
        wireToolCall('Grep', { pattern: 'x', '-i': true }),
        wireToolCall('Edit', { file_path: '/repo/a.ts', old_string: 'a', new_string: 'b', replace_all: true }),
      ],
    });
    const { container } = render(<ShadowTimeline interactions={[node]} />);
    // The three words are in the prompt, once each; nowhere else is wire text.
    for (const word of ['background', 'ignore case', 'replace all occurrences']) {
      typeQuery(`"${word}"`);
      expect(marked(container), word).toEqual([word]);
    }
  });
});

// ---------------------------------------------------------------------------
// 5 and 6. A text typed as it is drawn is marked
// ---------------------------------------------------------------------------

describe('5. a zero-width non-joiner or joiner that prose keeps', () => {
  it.each([
    ['Persian, which is written with the non-joiner', 'می\u200cخواهم', 'میخواهم'],
    ['an emoji sequence joined with the zero-width joiner', 'family \u{1F468}\u200d\u{1F469}\u200d\u{1F467} here', '\u{1F468}\u{1F469}\u{1F467}'],
  ])('%s: typed exactly as drawn, it is found and it is marked', (_name, response, withoutJoiners) => {
    const node = makeNode({ id: '1f0e5c3a-9d2b-8c4e-a1b7-000000000005', author: 'human', model_name: 'claude-opus-4-5-20251101', user_prompt: 'say it', model_response: response });
    const { container } = render(<ShadowTimeline interactions={[node]} />);
    // The person types the word as it is on the screen.
    const word = response.includes('family') ? '\u{1F468}\u200d\u{1F469}\u200d\u{1F467}' : response;
    typeQuery(word);
    expect(screen.getByRole('status')).toHaveTextContent('1 of 1');
    // Found, and nothing says where: no token is drawn for these characters in prose, and no mark.
    expect(marked(container).join('').replaceAll('\u200c', '').replaceAll('\u200d', '')).toBe(withoutJoiners);
  });
});

describe('6. the Turkish capital dotted I', () => {
  it('a word typed as it is drawn is found and marked', () => {
    const node = makeNode({ id: '1f0e5c3a-9d2b-8c4e-a1b7-000000000006', author: 'human', model_name: 'claude-opus-4-5-20251101', user_prompt: 'say it', model_response: 'Visit İstanbul in spring' });
    const { container } = render(<ShadowTimeline interactions={[node]} />);
    typeQuery('İstanbul');
    expect(screen.getByRole('status')).toHaveTextContent('1 of 1');
    expect(marked(container)).toEqual(['İstanbul']);
  });
});

// ---------------------------------------------------------------------------
// 7. Prefixes on the other layout
// ---------------------------------------------------------------------------

describe('7. tool: and path: on a step of the Thought / Action / Observation layout', () => {
  const node = triad('3b9f0f1e-6a4c-4d6e-9b1a-000000000007', {
    user_prompt: 'Fix the failing import',
    tool_executions: [toolCall('Bash', { command: 'npm run build' }), toolCall('Edit', { file_path: '/repo/src/app.ts', old_string: 'a', new_string: 'b' })],
  });

  it('tool: marks the name of the call in the line of the Action pane that begins with it', () => {
    const { container } = render(<ShadowTimeline interactions={[node]} />);
    typeQuery('tool:bash');
    expect(screen.getByRole('status')).toHaveTextContent('1 of 1');
    expect(marked(container).map((text) => text.toLowerCase())).toEqual(['bash']);
  });

  it('path: marks the file in the arguments the Action pane shows', () => {
    const { container } = render(<ShadowTimeline interactions={[node]} />);
    typeQuery('path:src/app.ts');
    expect(screen.getByRole('status')).toHaveTextContent('1 of 1');
    expect(marked(container)).toEqual(['src/app.ts']);
  });
});

// ---------------------------------------------------------------------------
// 8. A path with a line break
// ---------------------------------------------------------------------------

describe('8. the "touches the file" filter and a path that holds a line break', () => {
  it('does not take the text after the line break for a path of its own', () => {
    const decoy = makeNode({ id: 'decoy', tool_executions: [toolCall('Read', { file_path: 'decoy\nsrc/payments/refund_handler.ts' })] });
    const real = makeNode({ id: 'real', tool_executions: [toolCall('Read', { file_path: '/repo/src/payments/refund_handler.ts' })] });
    const index = getTimelineIndex([decoy, real]);
    const ids = runSearch(index, { query: parseQuery(''), filters: NO_FILTERS, file: 'src/payments/refund_handler.ts' }).ids;
    expect(ids).toEqual(['real']);
  });
});

// ---------------------------------------------------------------------------
// 9. The file open in the diff after navigating
// ---------------------------------------------------------------------------

describe('9. the file the page has open after it navigates to another pull request', () => {
  it('is one of the files of the new pull request', () => {
    const FILES: Record<number, string[]> = { 1: ['docs/README.md', 'src/a.ts'], 2: ['src/b.ts'] };
    vi.mocked(usePR).mockImplementation(((_o: string, _r: string, pr: number) => ({
      data: { pr: { title: `PR ${pr}` }, files: FILES[pr].map((filename) => ({ filename, status: 'modified', additions: 1, deletions: 0, patch: '@@ -1 +1 @@\n-a\n+b' })) },
      isLoading: false,
      error: null,
    })) as unknown as typeof usePR);
    const nodes = searchFixture(10).nodes;
    vi.mocked(usePRInteractions).mockImplementation((() => ({ data: { interactions: nodes, commits: [], truncated: false, hasHistory: true }, isLoading: false, error: null })) as unknown as typeof usePRInteractions);
    window.history.replaceState(null, '', '/pr/acme/shop/1');
    render(
      <Router>
        <Route path="/pr/:owner/:repo/:pr">
          <PRReviewPage />
        </Route>
      </Router>,
    );
    fireEvent.click(screen.getByTitle('docs/README.md'));
    expect(screen.getByTestId('diff')).toHaveTextContent('docs/README.md');
    act(() => navigate('/pr/acme/shop/2'));
    // The new pull request has no docs/README.md: its first file is open, as it is when a page is loaded.
    expect(screen.queryByText('Select a file to view')).toBeNull();
    expect(screen.getByTestId('diff')).toHaveTextContent('src/b.ts');
  });
});

// ---------------------------------------------------------------------------
// 10 and 11. Matches the card does not account for
// ---------------------------------------------------------------------------

describe('10. a call recorded with no input', () => {
  it('is not found by the word the wire uses for "no input"', () => {
    const node = makeNode({
      id: '1f0e5c3a-9d2b-8c4e-a1b7-00000000000a',
      author: 'system',
      model_name: 'claude-opus-4-5-20251101',
      user_prompt: '[tool_result Bash] ok',
      model_response: 'Listing the items.',
      tool_executions: [rawToolCall('mcp__example_server__list_items', 'null')],
    });
    const { container } = render(<ShadowTimeline interactions={[node]} />);
    // The card says "No arguments recorded." There is no "null" on it, and nobody who types it means that.
    expect(container.textContent).not.toMatch(/null/i);
    typeQuery('null');
    expect(screen.getByRole('status')).toHaveTextContent('No matches');
  });
});

describe('11. reasoning that no card draws', () => {
  it('does not show a step by words that are nowhere on it, or says where they are', () => {
    const node = makeNode({
      id: '7c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f',
      author: 'human',
      model_name: 'example-model-1',
      user_prompt: 'Explain what parse_header returns.',
      model_cot: 'The function trims one optional newline before splitting on the first colon.',
      model_response: 'It returns a pair.',
    });
    const { container } = render(<ShadowTimeline interactions={[node]} />);
    expect(container.textContent).not.toContain('trims'); // the card has no "Why?" for a human step: the reasoning is not on it
    typeQuery('trims');
    const shown = container.querySelectorAll('ol > li').length;
    const folds = Array.from(container.querySelectorAll('button')).filter((button) => / · \d+ match/.test(button.textContent ?? '')).length;
    // Either the step is not shown, or it shows where the match is.
    if (shown > 0) expect(container.querySelectorAll('mark').length + folds).toBeGreaterThan(0);
  });
});
