import { useId, useMemo, useState } from 'react';
import { clsx } from 'clsx';
import { useIsClamped } from '../../hooks/useIsClamped';
import { displayText, exceedsLines } from '../../lib/textLines';
import { ExpandToggle } from './ExpandToggle';

// Literal class names, so Tailwind's scanner finds them.
const CLAMP_CLASS = {
  3: 'line-clamp-3',
  4: 'line-clamp-4',
  6: 'line-clamp-6',
} as const;

interface ClampedTextProps {
  text: string;
  /** How many lines to show before "Show more". */
  lines?: keyof typeof CLAMP_CLASS;
  className?: string;
}

/**
 * Prose limited to a few lines, with "Show more" / "Show less" once it is
 * actually cut off. Line breaks and spacing in `text` are kept.
 *
 * Whether the text is cut off is measured in the browser (wrapping depends on
 * the pane width), while it is closed: the answer from the closed text stands
 * while it is open, so "Show less" stays mounted (and focused) when pressed. Text
 * with more explicit line breaks than the clamp is known to be cut off without
 * measuring, which also keeps the control available where layout is not computed.
 */
export function ClampedText({ text, lines = 4, className }: ClampedTextProps) {
  const [expanded, setExpanded] = useState(false);
  const [element, setElement] = useState<HTMLElement | null>(null);
  const shown = useMemo(() => displayText(text), [text]);
  const measuredClamp = useIsClamped(element, expanded, shown);
  const id = useId();

  const canExpand = measuredClamp || exceedsLines(shown, lines);
  return (
    <div className="min-w-0">
      <p
        id={id}
        ref={setElement}
        className={clsx('whitespace-pre-wrap break-words', !expanded && CLAMP_CLASS[lines], className)}
      >
        {shown}
      </p>
      {(canExpand || expanded) && (
        <ExpandToggle
          expanded={expanded}
          onToggle={() => setExpanded((value) => !value)}
          collapsedLabel="Show more"
          controls={id}
        />
      )}
    </div>
  );
}
