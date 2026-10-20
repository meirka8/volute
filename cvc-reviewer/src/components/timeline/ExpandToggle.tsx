interface ExpandToggleProps {
  expanded: boolean;
  onToggle: () => void;
  /** Text while collapsed, e.g. "Show more" or "Show all 40 lines". */
  collapsedLabel: string;
  expandedLabel?: string;
  /** Id of the element this button expands or collapses. */
  controls?: string;
}

/**
 * The "Show more" / "Show less" control used across the transcript layout.
 *
 * It is a real button, and it stops the click from reaching the timeline card,
 * which selects the step on a click anywhere on it. (Keyboard activation needs no
 * such guard: the card has no key handling at all, and the header, a button of its
 * own, is what selects it from the keyboard.)
 */
export function ExpandToggle({
  expanded,
  onToggle,
  collapsedLabel,
  expandedLabel = 'Show less',
  controls,
}: ExpandToggleProps) {
  return (
    <button
      type="button"
      aria-expanded={expanded}
      aria-controls={controls}
      onClick={(event) => {
        event.stopPropagation();
        onToggle();
      }}
      className="mt-2 text-xs text-action transition-colors hover:opacity-80"
    >
      {expanded ? expandedLabel : collapsedLabel}
    </button>
  );
}
