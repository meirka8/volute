import { usePersistentCount } from '../components/timeline/nodeUiState';

/**
 * How many of `total` items to draw: the first `page`, and `page` more each time
 * `showMore` is called. A list that is drawn in full cannot be bounded; a list
 * that is drawn a page at a time costs a page, however long the data is.
 *
 * How far the reader has paged is kept with the step (see `nodeUiState`), under `name`,
 * which tells this list from the others in the same part of a card.
 */
export function usePaged(total: number, page: number, name: string) {
  const [shown, setShown] = usePersistentCount(name, page);
  const visible = Math.min(shown, total);
  return {
    /** How many of the items to draw now. */
    visible,
    /** How many are not drawn. */
    remaining: total - visible,
    showMore: () => setShown((current) => current + page),
  };
}
