/**
 * `/` comes to the search box from anywhere on the page that does not use the key (issue #118),
 * as Space marks the file viewed from anywhere that does not use Space (see useKeyboardNavigation).
 */
import { cleanup, renderHook } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { OWNS_SLASH, slashShortcutApplies, useSlashShortcut } from './useSearchShortcut';

function press(target: EventTarget, init: KeyboardEventInit & { key?: string } = {}): KeyboardEvent {
  const event = new KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true, ...init });
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
  cleanup();
  document.body.innerHTML = '';
});

describe('the shortcut', () => {
  it('is a plain slash on the page, and is handled', () => {
    const focus = vi.fn();
    renderHook(() => useSlashShortcut(focus));
    const event = press(document.body);
    expect(focus).toHaveBeenCalledTimes(1);
    expect(event.defaultPrevented).toBe(true);
  });

  it.each([
    ['a plain element', '<div id="t">text</div>'],
    ['a button, which uses Enter and Space but not a slash', '<button id="t">Show more</button>'],
    ['a link', '<a href="#x" id="t">link</a>'],
    ['a block of code that scrolls', '<pre tabindex="0" id="t">code</pre>'],
    ['the header of a card', '<button id="t" aria-pressed="false">AI step</button>'],
    ['a summary', '<details><summary id="t">more</summary>body</details>'],
    ['a checkbox', '<input type="checkbox" id="t">'],
    ['a radio button', '<input type="radio" id="t">'],
    ['an input that is a button', '<input type="submit" id="t" value="Go">'],
    ['a range', '<input type="range" id="t">'],
    ['a button that has an element inside it', '<button><span id="t">x</span></button>'],
  ])('works from %s', (_label, html) => {
    const focus = vi.fn();
    renderHook(() => useSlashShortcut(focus));
    const target = mount(html).querySelector('#t')!;
    expect(press(target).defaultPrevented).toBe(true);
    expect(focus).toHaveBeenCalledTimes(1);
  });

  it.each([
    ['a text box', '<input id="t">'],
    ['a search box', '<input type="search" id="t">'],
    ['a box for a number, a date, a password, an address', '<input type="number" id="t">'],
    ['an input with a type that is not one the browser knows', '<input type="nonsense" id="t">'],
    ['a text area', '<textarea id="t"></textarea>'],
    ['a select, which uses typed characters to find an option', '<select id="t"><option>a</option></select>'],
    ['an editable region', '<div contenteditable id="t">x</div>'],
    ['something inside an editable region', '<div contenteditable="true"><p><em id="t">x</em></p></div>'],
    ['a widget made of a text box', '<div role="textbox" tabindex="0" id="t"></div>'],
    ['a search widget', '<div role="searchbox" tabindex="0" id="t"></div>'],
    ['a combo box', '<div role="combobox" tabindex="0" id="t"></div>'],
    ['a spin button', '<div role="spinbutton" tabindex="0" id="t"></div>'],
  ])('is left to %s: not handled, not cancelled', (_label, html) => {
    const focus = vi.fn();
    renderHook(() => useSlashShortcut(focus));
    const target = mount(html).querySelector('#t')!;
    const event = press(target);
    expect(event.defaultPrevented).toBe(false);
    expect(focus).not.toHaveBeenCalled();
  });

  it('still works from an editable region that says it is not editable', () => {
    const focus = vi.fn();
    renderHook(() => useSlashShortcut(focus));
    const target = mount('<div contenteditable="false" id="t">x</div>').querySelector('#t')!;
    expect(press(target).defaultPrevented).toBe(true);
    expect(focus).toHaveBeenCalledTimes(1);
  });

  it('is not for a slash with a modifier, one that is held down, one in the middle of composing text, or one already taken', () => {
    const focus = vi.fn();
    renderHook(() => useSlashShortcut(focus));
    for (const init of [{ ctrlKey: true }, { metaKey: true }, { altKey: true }, { repeat: true }, { isComposing: true }]) {
      expect(press(document.body, init).defaultPrevented).toBe(false);
    }
    document.addEventListener('keydown', (event) => event.preventDefault(), { once: true, capture: true });
    press(document.body);
    expect(focus).not.toHaveBeenCalled();
  });

  it('is for the slash key alone: a question mark, a backslash and other keys are not', () => {
    const focus = vi.fn();
    renderHook(() => useSlashShortcut(focus));
    for (const key of ['?', '\\', 'Slash', 's', 'Enter', ' ']) expect(press(document.body, { key }).defaultPrevented).toBe(false);
    expect(focus).not.toHaveBeenCalled();
  });

  it('is removed on unmount and absent when disabled', () => {
    const focus = vi.fn();
    const { unmount } = renderHook(() => useSlashShortcut(focus));
    unmount();
    press(document.body);
    expect(focus).not.toHaveBeenCalled();

    const off = vi.fn();
    renderHook(() => useSlashShortcut(off, false));
    press(document.body);
    expect(off).not.toHaveBeenCalled();
  });

  it('can be asked about without an element to ask: the target may be the document', () => {
    expect(slashShortcutApplies(new KeyboardEvent('keydown', { key: '/' }))).toBe(true);
    expect(OWNS_SLASH).toContain('input');
  });
});
