import { useId, useMemo, useState } from 'react';
import { clsx } from 'clsx';
import {
  cutAtCodePoint,
  exceedsCharBudget,
  shouldCollapse,
  splitDisplayLines,
  WRAPPED_CHAR_BUDGET,
} from '../../lib/textLines';
import { ExpandToggle } from './ExpandToggle';

/** A count as the controls say it: "8,192 lines". */
const count = (value: number) => value.toLocaleString('en-US');

interface CollapsibleCodeProps {
  text: string;
  /** Lines shown before "Show all N lines". */
  maxLines?: number;
  /**
   * Wrap long lines (command output, prose) instead of scrolling sideways
   * (commands, file content).
   */
  wrap?: boolean;
  /**
   * What the block is ("Command", "File content", "Tool output"). A block that
   * scrolls sideways is focusable, so it is a named region; a wrapping one is not.
   */
  label?: string;
  className?: string;
}

/**
 * A monospace block with real line breaks, collapsed to `maxLines` lines when
 * that hides at least a few lines (see `shouldCollapse`). A block that WRAPS is
 * also cut to a length budget (see `WRAPPED_CHAR_BUDGET`), because it grows with
 * its text however few lines it has. Text is rendered as a plain text node;
 * whitespace is preserved and nothing is interpreted.
 *
 * Scrolling blocks are focusable so keyboard users can scroll them, and are
 * named regions so a screen reader says what they hold; the card around them
 * ignores key events that originate here.
 */
export function CollapsibleCode({ text, maxLines = 12, wrap = false, label = 'Text', className }: CollapsibleCodeProps) {
  const [expanded, setExpanded] = useState(false);
  const id = useId();
  const lines = useMemo(() => splitDisplayLines(text), [text]);
  const full = useMemo(() => lines.join('\n'), [lines]);

  // A block that scrolls sideways stays one row high however long its line is,
  // so only a wrapping one is cut by length as well as by lines.
  const cutByLines = shouldCollapse(lines.length, maxLines);
  const cutByLength = wrap && exceedsCharBudget(full.length);
  const collapsible = cutByLines || cutByLength;
  const shown = useMemo(() => {
    if (!collapsible || expanded) return full;
    const head = cutByLines ? lines.slice(0, maxLines).join('\n') : full;
    return wrap ? cutAtCodePoint(head, WRAPPED_CHAR_BUDGET) : head;
  }, [collapsible, expanded, full, lines, cutByLines, maxLines, wrap]);

  return (
    <div className={clsx('min-w-0', className)}>
      <pre
        id={id}
        tabIndex={wrap ? undefined : 0}
        role={wrap ? undefined : 'region'}
        aria-label={wrap ? undefined : label}
        className={clsx(
          'rounded-xl border border-line bg-canvas/80 px-3 py-2 font-mono text-xs leading-relaxed text-ink [tab-size:4]',
          wrap ? 'whitespace-pre-wrap break-words' : 'overflow-x-auto whitespace-pre',
        )}
      >
        {shown}
      </pre>
      {collapsible && (
        <ExpandToggle
          expanded={expanded}
          onToggle={() => setExpanded((value) => !value)}
          collapsedLabel={cutByLines ? `Show all ${count(lines.length)} lines` : `Show all ${count(full.length)} characters`}
          controls={id}
        />
      )}
    </div>
  );
}
