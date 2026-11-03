/**
 * The page opens the pull request on GitHub in a window of its own, from the command palette (issue #118, revision round 4):
 * that window gets no reference to this page (noopener) and is not told which page sent it (noreferrer), as a link with
 * `target="_blank"` is not, in the browsers that give one that.
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Route, Router } from 'wouter';
import { PRReviewPage } from './PRReview';
import { usePR } from '../hooks/usePR';
import { usePRInteractions } from '../hooks/usePRInteractions';
import { installDomStubs, restoreDomStubs } from '../test/domStubs';
import { makeNode } from '../test/fixtures';

vi.mock('../hooks/usePR', () => ({ usePR: vi.fn() }));
vi.mock('../hooks/usePRInteractions', () => ({ usePRInteractions: vi.fn() }));
vi.mock('../auth/AuthContext', () => ({ useAuth: () => ({ logout: () => {} }) }));
vi.mock('../components/diff/DiffViewer', () => ({
  DiffViewer: ({ filename }: { filename: string }) => <div data-testid="diff">{filename}</div>,
}));
// The layout of the page animates panes of several kinds: any tag is a plain element here, without the animation props.
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
  vi.mocked(usePR).mockImplementation((() => ({
    data: { pr: { title: 'A pull request' }, files: [{ filename: 'src/a.ts', status: 'modified', additions: 1, deletions: 0, patch: '@@ -1 +1 @@\n-a\n+b' }] },
    isLoading: false,
    error: null,
  })) as unknown as typeof usePR);
  vi.mocked(usePRInteractions).mockImplementation((() => ({
    data: { interactions: [makeNode({ id: '1f0e5c3a-9d2b-8c4e-a1b7-000000000001', author: 'human', user_prompt: 'A step' })], commits: [], truncated: false, hasHistory: true },
    isLoading: false,
    error: null,
  })) as unknown as typeof usePRInteractions);
  // jsdom has no matchMedia, and the layout asks it about the width of the window; cmdk scrolls the selected item into view.
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener: () => {}, removeEventListener: () => {}, addListener: () => {}, removeListener: () => {} }));
  Element.prototype.scrollIntoView = () => {};
  window.history.replaceState(null, '', '/pr/acme/shop/7');
});
afterEach(() => {
  cleanup();
  restoreDomStubs();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  window.history.replaceState(null, '', '/');
});

describe('"Open PR on GitHub" in the command palette', () => {
  it('opens the pull request in a new window that has no reference to this page and is not told where it came from', () => {
    const open = vi.spyOn(window, 'open').mockImplementation(() => null);
    render(
      <Router>
        <Route path="/pr/:owner/:repo/:pr">
          <PRReviewPage />
        </Route>
      </Router>,
    );
    act(() => void fireEvent.keyDown(document, { key: 'k', ctrlKey: true }));
    fireEvent.click(screen.getByText('Open PR on GitHub'));
    expect(open).toHaveBeenCalledTimes(1);
    expect(open).toHaveBeenCalledWith('https://github.com/acme/shop/pull/7', '_blank', 'noopener,noreferrer');
  });
});
