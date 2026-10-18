/**
 * The page-wide keyboard shortcuts: j and k move between files, x expands, and
 * Space marks the open file viewed. Space is the one that was wrong: it was
 * taken from every focused control, which cancelled its activation and flipped
 * the open file's "Viewed" state as a side effect.
 */
import { renderHook } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { useKeyboardNavigation } from './useKeyboardNavigation';

function handlers() {
  return { onNext: vi.fn(), onPrevious: vi.fn(), onExpand: vi.fn(), onMarkViewed: vi.fn() };
}

function press(target: EventTarget, key: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

function mount(html: string): HTMLElement {
  const host = document.createElement('div');
  host.innerHTML = html;
  document.body.appendChild(host);
  return host;
}

afterEach(() => {
  document.body.innerHTML = '';
});

describe('Space', () => {
  it('marks the open file viewed when nothing in particular has focus, and is handled', () => {
    const on = handlers();
    renderHook(() => useKeyboardNavigation(on));
    const event = press(document.body, ' ');
    expect(on.onMarkViewed).toHaveBeenCalledTimes(1);
    expect(event.defaultPrevented).toBe(true);
  });

  it('also does so from a plain element, as the sidebar file rows are (divs with a click handler)', () => {
    const on = handlers();
    renderHook(() => useKeyboardNavigation(on));
    const host = mount('<div id="row">src/app.ts</div><span id="label">label</span>');
    expect(press(host.querySelector('#row')!, ' ').defaultPrevented).toBe(true);
    expect(press(host.querySelector('#label')!, ' ').defaultPrevented).toBe(true);
    expect(on.onMarkViewed).toHaveBeenCalledTimes(2);
  });

  it.each([
    ['a button', '<button id="t">Show more</button>'],
    ['a button\'s own child', '<button><span id="t">Show more</span></button>'],
    ['a card that is a role=button', '<div role="button" tabindex="0" id="t">card</div>'],
    ['something inside such a card', '<div role="button" tabindex="0"><p><em id="t">text</em></p></div>'],
    ['a block of code that scrolls', '<pre tabindex="0" id="t">code</pre>'],
    ['a diff region', '<div tabindex="0" role="region" aria-label="Diff" id="t"><div>line</div></div>'],
    ['a link', '<a href="#x" id="t">link</a>'],
    ['a summary', '<details><summary id="t">more</summary>body</details>'],
    ['a select', '<select id="t"><option>a</option></select>'],
    ['any element with a tabindex of 0', '<div tabindex="0" id="t">focusable</div>'],
    ['an element with a positive tabindex', '<div tabindex="3" id="t">focusable</div>'],
  ])('is left to %s: not handled, not cancelled, and the file is not marked viewed', (_label, html) => {
    const on = handlers();
    renderHook(() => useKeyboardNavigation(on));
    const target = mount(html).querySelector('#t')!;
    const event = press(target, ' ');
    expect(event.defaultPrevented).toBe(false);
    expect(on.onMarkViewed).not.toHaveBeenCalled();
  });

  it.each([
    ['an anchor with no href', '<a id="t">not a link</a>'],
    ['an element that is only focusable by script (tabindex -1)', '<div tabindex="-1" id="t">script focus</div>'],
    ['a plain paragraph', '<p id="t">text</p>'],
  ])('still marks the file viewed from %s', (_label, html) => {
    const on = handlers();
    renderHook(() => useKeyboardNavigation(on));
    const event = press(mount(html).querySelector('#t')!, ' ');
    expect(event.defaultPrevented).toBe(true);
    expect(on.onMarkViewed).toHaveBeenCalledTimes(1);
  });

  it('stays out of the way in text inputs, as before', () => {
    const on = handlers();
    renderHook(() => useKeyboardNavigation(on));
    const host = mount('<input id="a"><textarea id="b"></textarea>');
    for (const id of ['a', 'b']) {
      const event = press(host.querySelector(`#${id}`)!, ' ');
      expect(event.defaultPrevented).toBe(false);
    }
    expect(on.onMarkViewed).not.toHaveBeenCalled();
  });

  it('ignores Space with a modifier, as before', () => {
    const on = handlers();
    renderHook(() => useKeyboardNavigation(on));
    for (const modifier of [{ ctrlKey: true }, { metaKey: true }, { altKey: true }]) {
      expect(press(document.body, ' ', modifier).defaultPrevented).toBe(false);
    }
    expect(on.onMarkViewed).not.toHaveBeenCalled();
  });

  it('does not stop a focused button from activating: its keydown is not cancelled, so the browser clicks it', () => {
    const on = handlers();
    renderHook(() => useKeyboardNavigation(on));
    const host = mount('<button id="t">Show all 40 lines</button>');
    const button = host.querySelector('#t') as HTMLButtonElement;
    button.focus();
    // `dispatchEvent` returns false when a listener cancelled the event.
    expect(button.dispatchEvent(new KeyboardEvent('keydown', { key: ' ', bubbles: true, cancelable: true }))).toBe(true);
  });
});

describe('the other shortcuts', () => {
  it.each([
    ['j', 'onNext'],
    ['k', 'onPrevious'],
    ['x', 'onExpand'],
    ['J', 'onNext'],
  ] as const)('%s calls %s, from the page', (key, handler) => {
    const on = handlers();
    renderHook(() => useKeyboardNavigation(on));
    const event = press(document.body, key);
    expect(on[handler]).toHaveBeenCalledTimes(1);
    expect(event.defaultPrevented).toBe(true);
  });

  it.each(['j', 'k', 'x'] as const)('%s still works from a focused button, which does not use that key', (key) => {
    const on = handlers();
    renderHook(() => useKeyboardNavigation(on));
    const button = mount('<button id="t">Show more</button>').querySelector('#t')!;
    const event = press(button, key);
    const called = { j: on.onNext, k: on.onPrevious, x: on.onExpand }[key];
    expect(called).toHaveBeenCalledTimes(1);
    expect(event.defaultPrevented).toBe(true);
  });

  it('j still works from a card and from a scrolling block', () => {
    const on = handlers();
    renderHook(() => useKeyboardNavigation(on));
    const host = mount('<div role="button" tabindex="0" id="card">card</div><pre tabindex="0" id="code">code</pre>');
    press(host.querySelector('#card')!, 'j');
    press(host.querySelector('#code')!, 'j');
    expect(on.onNext).toHaveBeenCalledTimes(2);
  });

  it('are not handled in text inputs, and other keys are never handled', () => {
    const on = handlers();
    renderHook(() => useKeyboardNavigation(on));
    const input = mount('<input id="t">').querySelector('#t')!;
    for (const key of ['j', 'k', 'x']) expect(press(input, key).defaultPrevented).toBe(false);
    for (const key of ['Enter', 'a', 'Tab', 'ArrowDown']) expect(press(document.body, key).defaultPrevented).toBe(false);
    expect(on.onNext).not.toHaveBeenCalled();
    expect(on.onMarkViewed).not.toHaveBeenCalled();
  });
});

describe('the listener', () => {
  it('is removed on unmount and absent when disabled', () => {
    const on = handlers();
    const { unmount } = renderHook(() => useKeyboardNavigation(on));
    unmount();
    press(document.body, ' ');
    press(document.body, 'j');
    expect(on.onMarkViewed).not.toHaveBeenCalled();
    expect(on.onNext).not.toHaveBeenCalled();

    const off = handlers();
    renderHook(() => useKeyboardNavigation({ ...off, enabled: false }));
    press(document.body, ' ');
    expect(off.onMarkViewed).not.toHaveBeenCalled();
  });
});
