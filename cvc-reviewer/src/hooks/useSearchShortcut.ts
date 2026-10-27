import { useEffect } from 'react';

/**
 * Things that own the `/` key when they have focus: anything a person types into (a text box, a
 * select, which uses typed characters to find an option, an editable region) and the widgets that
 * are made of one. (A check box, a radio button, a range and the buttons that are inputs take no text.) The page-wide shortcut leaves all of them alone, as the Space shortcut leaves
 * buttons and links alone (see `OWNS_SPACE` in useKeyboardNavigation): `/` typed into a box is a
 * character of what is being typed.
 */
const NOT_TYPED_INTO = ['checkbox', 'radio', 'button', 'submit', 'reset', 'image', 'file', 'range', 'color'].map((type) => `:not([type="${type}"])`).join('');
export const OWNS_SLASH = `input${NOT_TYPED_INTO}, textarea, select, [contenteditable]:not([contenteditable="false"]), [role="textbox"], [role="searchbox"], [role="combobox"], [role="spinbutton"]`;

/**
 * Whether a key press is the shortcut for the search box: a plain `/`, not part of a combination,
 * not repeated by holding the key down, not in the middle of composing text, not already taken by
 * something else, and not typed into something that owns it.
 */
export function slashShortcutApplies(event: KeyboardEvent): boolean {
  if (event.key !== '/') return false;
  if (event.ctrlKey || event.metaKey || event.altKey) return false;
  if (event.defaultPrevented || event.isComposing || event.repeat) return false;
  const target = event.target;
  if (target instanceof Element) {
    if (target.closest(OWNS_SLASH)) return false;
    if ((target as HTMLElement).isContentEditable) return false;
  }
  return true;
}

/** Calls `focus` when `/` is pressed anywhere on the page that does not use it (see `slashShortcutApplies`). */
export function useSlashShortcut(focus: () => void, enabled: boolean = true): void {
  useEffect(() => {
    if (!enabled) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (!slashShortcutApplies(event)) return;
      event.preventDefault();
      focus();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [focus, enabled]);
}
