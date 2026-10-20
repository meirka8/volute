/**
 * Helpers for tests that compare markup.
 *
 * The ids React makes with `useId` (for the card's name, and for the controls that
 * say what they open) count up across the whole run of a test file, so the same
 * card drawn twice has different ids the second time. Markup compared across draws
 * is compared without them; markup recorded as a snapshot has them renumbered in
 * the order they appear, which keeps what points at what.
 */

/** The attributes that hold ids, or lists of them. */
const ID_ATTRIBUTES = ['id', 'aria-controls', 'aria-labelledby'] as const;

/** `html` without its ids and without the attributes that point at them. */
export const withoutIds = (html: string): string => html.replace(/ (id|aria-controls|aria-labelledby)="[^"]*"/g, '');

/**
 * Renumbers the ids in `root` (and the attributes that point at them) as `id-1`, `id-2`,
 * ... in the order they first appear, so a snapshot of it does not depend on how many
 * ids were made before. Changes `root`; give it a copy.
 */
export function renumberIds(root: Element): void {
  const names = new Map<string, string>();
  const rename = (id: string): string => {
    let name = names.get(id);
    if (name === undefined) {
      name = `id-${names.size + 1}`;
      names.set(id, name);
    }
    return name;
  };
  const elements = [root, ...Array.from(root.querySelectorAll('*'))];
  // The ids first, so that a reference made before the element it points at is renumbered in the order of the ids.
  for (const element of elements) {
    const id = element.getAttribute('id');
    if (id !== null) element.setAttribute('id', rename(id));
  }
  for (const element of elements) {
    for (const attribute of ID_ATTRIBUTES) {
      if (attribute === 'id') continue;
      const value = element.getAttribute(attribute);
      if (value !== null) element.setAttribute(attribute, value.split(/\s+/).filter(Boolean).map(rename).join(' '));
    }
  }
}
