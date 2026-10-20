interface ShowMoreProps {
  /** How many items are not drawn yet. */
  remaining: number;
  /** How many one click draws, at most. */
  page: number;
  /** What the items are called, singular and plural: "tool result", "tool results". */
  one: string;
  many: string;
  onShow: () => void;
}

const count = (value: number) => value.toLocaleString('en-US');

/**
 * "Show 20 more tool results (4,980 not shown)": the control that draws the next
 * page of a list too long to draw at once. The count in brackets is how many are
 * not drawn right now, this page included. It draws one page per click, so a
 * list of a million items cannot be turned into a million elements by a click.
 *
 * It is a real button, and it stops the click from reaching the timeline card,
 * which selects the step on a click anywhere on it. It reveals more; it is not a
 * toggle, so it has no expanded state.
 */
export function ShowMore({ remaining, page, one, many, onShow }: ShowMoreProps) {
  if (remaining <= 0) return null;
  const next = Math.min(page, remaining);
  return (
    <button
      type="button"
      onClick={(event) => {
        event.stopPropagation();
        onShow();
      }}
      className="mt-2 text-xs text-action transition-colors hover:opacity-80"
    >
      {`Show ${count(next)} more ${next === 1 ? one : many}${remaining > next ? ` (${count(remaining)} not shown)` : ''}`}
    </button>
  );
}
