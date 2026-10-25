/**
 * The bar of the timeline search (issue #118, revision round): "Copy link to this view" and the line that asks before a text
 * that looks like a secret goes into a link, the field the link is shown in where the clipboard is not the page's to write
 * to, the line that says a link brought the search, and the note about terms that are not used.
 */
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { makeNode, toolCall } from '../../test/fixtures';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

const BASE = Date.parse('2026-03-02T09:15:00.000Z');
const id = (n: number) => `1f0e5c3a-9d2b-8c4e-a1b7-${n.toString(16).padStart(12, '0')}`;
const step = (n: number, extra: Partial<InteractionNode> = {}) =>
  makeNode({ id: id(n), author: 'human', timestamp: BASE + n * 1000, model_name: `Step ${n}`, user_prompt: `Step ${n}`, ...extra });
const STEPS = [
  step(1),
  step(2, { model_response: 'The needle is here' }),
  step(3),
  step(4, { tool_executions: [toolCall('Bash', { command: 'grep needle file.txt' })] }),
  step(5),
];

beforeEach(() => {
  installDomStubs();
  vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
  window.history.replaceState(null, '', '/pr/acme/shop/7?tab=files');
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  restoreDomStubs();
  delete (window.navigator as unknown as Record<string, unknown>).clipboard;
  window.history.replaceState(null, '', '/');
});

const box = () => screen.getByRole('searchbox', { name: 'Search steps' }) as HTMLInputElement;
const typeQuery = (value: string) => fireEvent.change(box(), { target: { value } });
const count = () => screen.getByRole('status').textContent;
const copyButton = () => screen.getByRole('button', { name: 'Copy link to this view' });

/** Gives the page a clipboard that keeps what is written to it (or one that refuses). */
function clipboard(refuse = false): string[] {
  const written: string[] = [];
  Object.defineProperty(window.navigator, 'clipboard', {
    configurable: true,
    value: { writeText: (text: string) => (refuse ? Promise.reject(new Error('denied')) : (written.push(text), Promise.resolve())) },
  });
  return written;
}
/** Presses a control and lets the promise it waits on settle. */
const press = async (control: HTMLElement) => {
  await act(async () => {
    fireEvent.click(control);
  });
};

describe('Copy link to this view', () => {
  it('copies the address with the filters and the search text in its fragment, and says so where the count is', async () => {
    const written = clipboard();
    render(<ShadowTimeline interactions={STEPS} />);
    fireEvent.click(screen.getByRole('button', { name: 'Filters' }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    typeQuery('needle');
    const before = count();
    await press(copyButton());
    expect(written).toHaveLength(1);
    const link = new URL(written[0]);
    expect(link.pathname).toBe('/pr/acme/shop/7');
    expect(link.search).toBe('?tab=files');
    expect(new URLSearchParams(link.hash.slice(1)).get('q')).toBe('needle');
    expect(new URLSearchParams(link.hash.slice(1)).getAll('tool')).toEqual(['Bash']);
    // The address of the page itself still has the filters only.
    expect(window.location.hash).toBe('#tool=Bash');
    // "Link copied" is said in the live region, in the count's place, and the count comes back.
    expect(screen.getAllByRole('status')).toHaveLength(1);
    expect(count()).toBe('Link copied');
    await act(async () => void vi.advanceTimersByTime(3_100));
    expect(count()).toBe(before);
    // Focus stays on the button that was pressed.
    expect(document.activeElement).toBe(copyButton());
  });

  it('says "Link copied" only about the search it copied: typing on takes it away', async () => {
    clipboard();
    render(<ShadowTimeline interactions={STEPS} />);
    typeQuery('needle');
    await press(copyButton());
    expect(count()).toBe('Link copied');
    typeQuery('needle is');
    expect(count()).not.toBe('Link copied');
  });

  it('is the address of the page itself when nothing is searched or filtered', async () => {
    const written = clipboard();
    render(<ShadowTimeline interactions={STEPS} />);
    await press(copyButton());
    expect(written).toEqual([`${window.location.origin}/pr/acme/shop/7?tab=files`]);
  });

  it('keeps the parameters of the fragment that are not ours, and takes the old text out of the link', async () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7#section-2');
    const written = clipboard();
    render(<ShadowTimeline interactions={STEPS} />);
    typeQuery('needle');
    await press(copyButton());
    expect(new URL(written[0]).hash).toBe('#section-2&q=needle');
  });

  it('writes the file that is open for a filter that follows it', async () => {
    const written = clipboard();
    render(<ShadowTimeline interactions={[step(1, { context_items: [{ file_path: '/w/src/a.ts' }] })]} openFile="src/a.ts" />);
    fireEvent.click(screen.getByRole('button', { name: 'Filters' }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Touches the open file/ }));
    await press(copyButton());
    expect(new URLSearchParams(new URL(written[0]).hash.slice(1)).get('file')).toBe('src/a.ts');
  });
});

describe('a search text that looks like a secret', () => {
  const secret = 'password = hunter2hunter2';

  it('is not put into a link before the person has been asked, and the safe answer is where the keyboard is', async () => {
    const written = clipboard();
    render(<ShadowTimeline interactions={STEPS} />);
    typeQuery(secret);
    await press(copyButton());
    expect(written).toEqual([]);
    const asked = screen.getByRole('alert');
    expect(asked).toHaveTextContent('The search text looks like a secret; anyone with the link will see it');
    expect(screen.getByRole('group', { name: /looks like a secret/ })).toContainElement(asked);
    const safe = screen.getByRole('button', { name: 'Copy without the search text' });
    expect(document.activeElement).toBe(safe);
    expect(screen.getByRole('button', { name: 'Copy anyway' })).toBeInTheDocument();
  });

  it('is left out of the link by the default answer, filters and all', async () => {
    const written = clipboard();
    render(<ShadowTimeline interactions={STEPS} />);
    fireEvent.click(screen.getByRole('button', { name: 'Filters' }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Bash/ }));
    typeQuery(secret);
    await press(copyButton());
    await press(screen.getByRole('button', { name: 'Copy without the search text' }));
    expect(written).toHaveLength(1);
    expect(written[0]).not.toContain('hunter2');
    expect(written[0]).not.toContain('q=');
    expect(new URL(written[0]).hash).toBe('#tool=Bash');
    // The line goes away, "Link copied" is said, and the keyboard is back on the button.
    expect(screen.queryByRole('alert')).toBeNull();
    expect(count()).toBe('Link copied');
    expect(document.activeElement).toBe(copyButton());
  });

  it('is put into the link when the person says so', async () => {
    const written = clipboard();
    render(<ShadowTimeline interactions={STEPS} />);
    typeQuery(secret);
    await press(copyButton());
    await press(screen.getByRole('button', { name: 'Copy anyway' }));
    expect(new URLSearchParams(new URL(written[0]).hash.slice(1)).get('q')).toBe(secret);
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('can be taken back with Escape, which copies nothing and returns to the button', async () => {
    const written = clipboard();
    render(<ShadowTimeline interactions={STEPS} />);
    typeQuery(secret);
    await press(copyButton());
    fireEvent.keyDown(screen.getByRole('button', { name: 'Copy without the search text' }), { key: 'Escape' });
    expect(screen.queryByRole('alert')).toBeNull();
    expect(written).toEqual([]);
    expect(document.activeElement).toBe(copyButton());
    // And the box was not cleared by it.
    expect(box().value).toBe(secret);
  });

  it('is not asked about any more once the text or the filters have changed', async () => {
    const written = clipboard();
    render(<ShadowTimeline interactions={STEPS} />);
    typeQuery(secret);
    await press(copyButton());
    expect(screen.getByRole('alert')).toBeInTheDocument();
    typeQuery('retry limit');
    expect(screen.queryByRole('alert')).toBeNull();
    expect(written).toEqual([]);
  });

  it('is not asked about when the text does not look like one', async () => {
    const written = clipboard();
    render(<ShadowTimeline interactions={STEPS} />);
    typeQuery('retry limit');
    await press(copyButton());
    expect(screen.queryByRole('alert')).toBeNull();
    expect(written).toHaveLength(1);
  });
});

describe('where the clipboard is not the page\'s to write to', () => {
  it('shows the link in a field that is selected, and the field can be closed', async () => {
    // (No clipboard at all: jsdom has none.)
    render(<ShadowTimeline interactions={STEPS} />);
    typeQuery('needle');
    await press(copyButton());
    const field = screen.getByRole('textbox', { name: /did not let this page copy the link/ }) as HTMLInputElement;
    expect(field).toHaveAttribute('readonly');
    expect(new URL(field.value).pathname).toBe('/pr/acme/shop/7');
    expect(new URLSearchParams(new URL(field.value).hash.slice(1)).get('q')).toBe('needle');
    expect(document.activeElement).toBe(field);
    expect(field.selectionStart).toBe(0);
    expect(field.selectionEnd).toBe(field.value.length);
    // Nothing was said to have been copied.
    expect(count()).not.toBe('Link copied');
    await press(screen.getByRole('button', { name: 'Close' }));
    expect(screen.queryByRole('textbox', { name: /did not let this page copy the link/ })).toBeNull();
    expect(document.activeElement).toBe(copyButton());
  });

  it('takes the field away when the search changes, which the link in it was not made for', async () => {
    render(<ShadowTimeline interactions={STEPS} />);
    typeQuery('needle');
    await press(copyButton());
    expect(screen.getByRole('textbox', { name: /did not let this page copy the link/ })).toBeInTheDocument();
    typeQuery('needle is');
    expect(screen.queryByRole('textbox', { name: /did not let this page copy the link/ })).toBeNull();
  });

  it('does the same when the browser refuses', async () => {
    clipboard(true);
    render(<ShadowTimeline interactions={STEPS} />);
    await press(copyButton());
    expect(screen.getByRole('textbox', { name: /did not let this page copy the link/ })).toBeInTheDocument();
    expect(count()).not.toBe('Link copied');
  });

  it('shows the link without the text when the person chose that for a secret', async () => {
    render(<ShadowTimeline interactions={STEPS} />);
    typeQuery('password = hunter2hunter2');
    await press(copyButton());
    await press(screen.getByRole('button', { name: 'Copy without the search text' }));
    const field = screen.getByRole('textbox', { name: /did not let this page copy the link/ }) as HTMLInputElement;
    expect(field.value).not.toContain('hunter2');
  });
});

describe('a view that a link brought', () => {
  it('is said, with a way to clear it, until the person changes the search or the filters', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7#q=needle&tool=Bash');
    render(<ShadowTimeline interactions={STEPS} />);
    const note = screen.getByText('This view was opened from a link with a search and filters applied.');
    expect(note.closest('[role="note"]')).not.toBeNull();
    expect(count()).toBe('1 of 1 · 1 of 5 steps');
    typeQuery('needle ');
    expect(screen.queryByText(/opened from a link/)).toBeNull();
  });

  it('says only what the link brought', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7#q=needle');
    const { unmount } = render(<ShadowTimeline interactions={STEPS} />);
    expect(screen.getByText('This view was opened from a link with a search applied.')).toBeInTheDocument();
    unmount();
    window.history.replaceState(null, '', '/pr/acme/shop/7#tool=Bash');
    render(<ShadowTimeline interactions={STEPS} />);
    expect(screen.getByText('This view was opened from a link with filters applied.')).toBeInTheDocument();
    expect(count()).toBe('1 of 5 steps');
  });

  it('is cleared by its own button, which puts the keyboard in the box', () => {
    window.history.replaceState(null, '', '/pr/acme/shop/7#q=needle&tool=Bash');
    render(<ShadowTimeline interactions={STEPS} />);
    fireEvent.click(screen.getByRole('button', { name: 'Clear' }));
    expect(box().value).toBe('');
    expect(count()).toBe('');
    expect(screen.queryByText(/opened from a link/)).toBeNull();
    expect(screen.queryByRole('button', { name: /^Remove filter/ })).toBeNull();
    expect(document.activeElement).toBe(box());
    expect(window.location.hash).toBe('');
  });

  it('is not there for a page opened with nothing in its address, or after the person typed', () => {
    render(<ShadowTimeline interactions={STEPS} />);
    expect(screen.queryByText(/opened from a link/)).toBeNull();
    typeQuery('needle');
    expect(screen.queryByText(/opened from a link/)).toBeNull();
  });

  it('is said again for a link opened in this page, when the address changes', () => {
    render(<ShadowTimeline interactions={STEPS} />);
    typeQuery('needle');
    act(() => {
      window.history.pushState(null, '', '/pr/acme/shop/7?tab=files#tool=Bash&q=grep');
      window.dispatchEvent(new HashChangeEvent('hashchange'));
    });
    expect(box().value).toBe('grep');
    expect(screen.getByText('This view was opened from a link with a search and filters applied.')).toBeInTheDocument();
    expect(window.location.hash).toBe('#tool=Bash');
  });
});

describe('terms that are not used', () => {
  it('are said when there are more than sixteen different ones, and not before', () => {
    render(<ShadowTimeline interactions={STEPS} />);
    const terms = Array.from({ length: 16 }, (_, k) => `t${k}`);
    typeQuery(terms.join(' '));
    expect(screen.queryByText(/Only the first 16 terms are used/)).toBeNull();
    typeQuery(`${terms.join(' ')} t0 t1`); // the same terms again are not new ones
    expect(screen.queryByText(/Only the first 16 terms are used/)).toBeNull();
    typeQuery(`${terms.join(' ')} t16`);
    expect(screen.getByText('Only the first 16 terms are used')).toBeInTheDocument();
    typeQuery(terms.join(' '));
    expect(screen.queryByText(/Only the first 16 terms are used/)).toBeNull();
  });
});
