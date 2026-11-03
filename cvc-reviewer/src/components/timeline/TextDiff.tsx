import { useCallback, useId, useMemo } from 'react';
import { clsx } from 'clsx';
import { MAX_DIFF_LINES, planDiff, type DiffRow } from '../../lib/lineDiff';
import { displayText, shouldCollapse } from '../../lib/textLines';
import { CollapsibleCode } from './CollapsibleCode';
import { ExpandToggle } from './ExpandToggle';
import { HiddenText } from './HiddenText';
import { usePersistentToggle } from './nodeUiState';
import { Section } from './NodeUiScope';
import { useMatchCount } from './searchHighlight';

interface TextDiffProps {
  oldText: string;
  newText: string;
  /** Rows shown before "Show all N lines". */
  maxRows?: number;
}

function DiffLine({ row }: { row: DiffRow }) {
  if (row.kind === 'gap') {
    return (
      <div className="select-none px-3 py-0.5 text-[11px] italic text-muted">
        <span aria-hidden="true">⋯</span>
        {` ${row.hidden} unchanged ${row.hidden === 1 ? 'line' : 'lines'}`}
      </div>
    );
  }

  const sign = row.kind === 'add' ? '+' : row.kind === 'remove' ? '-' : ' ';
  const rowClass = clsx(
    'flex whitespace-pre px-3 no-underline',
    row.kind === 'add' && 'bg-success/15',
    row.kind === 'remove' && 'bg-danger/15',
  );
  const content = (
    <>
      <span
        aria-hidden="true"
        className={clsx(
          'w-4 shrink-0 select-none',
          row.kind === 'add' && 'text-success',
          row.kind === 'remove' && 'text-danger',
        )}
      >
        {sign}
      </span>
      <span>
        <HiddenText text={row.text} mode="strict" />
      </span>
    </>
  );

  // <ins> and <del> carry the meaning to assistive technology as well as colour.
  if (row.kind === 'add') return <ins className={rowClass}>{content}</ins>;
  if (row.kind === 'remove') return <del className={rowClass}>{content}</del>;
  return <div className={rowClass}>{content}</div>;
}

function Note({ children }: { children: string }) {
  return <p className="mb-1 text-xs italic text-muted">{children}</p>;
}

/**
 * What to say when the two texts show no change. Judged on the RAW text: line
 * endings are folded before diffing, so "the diff shows no change" is not the
 * same as "the call changed nothing".
 */
function unchangedNote(oldText: string, newText: string): string {
  if (oldText === newText) return oldText === '' ? 'Old and new text are both empty.' : 'Old and new text are identical.';
  return 'Only the line endings differ.';
}

/**
 * The texts show no change (they are the same, or differ only in line endings),
 * so there is no diff to draw. The text is shown once, as one block of text: a
 * diff would draw one row for every line of it, and "Show all" on a text of
 * hundreds of thousands of lines would build all of them for nothing.
 */
function UnchangedText({ oldText, newText, maxRows }: { oldText: string; newText: string; maxRows: number }) {
  return (
    <div className="min-w-0">
      <Note>{unchangedNote(oldText, newText)}</Note>
      {newText !== '' && <CollapsibleCode text={newText} label="Text" maxLines={maxRows} />}
    </div>
  );
}

function ChangedDiff({ oldText, newText, maxRows }: { oldText: string; newText: string; maxRows: number }) {
  const [expanded, setExpanded] = usePersistentToggle('expanded', false);
  const id = useId();
  // Line endings are folded first, so a Windows-style file does not show every
  // line as changed (nor a stray carriage return).
  const plan = useMemo(
    () => planDiff(displayText(oldText), displayText(newText), { maxDiffLines: MAX_DIFF_LINES }),
    [oldText, newText],
  );
  const collapsible = shouldCollapse(plan.total, maxRows);
  const rows = useMemo(
    () => plan.rows(collapsible && !expanded ? maxRows : undefined),
    [plan, collapsible, expanded, maxRows],
  );
  // While the diff is searched and folded: the matches in every line it has, which the toggle says. (The rows
  // are all built only then, and only to count them; a diff is at most MAX_DIFF_LINES lines.)
  const lineTexts = useCallback(() => plan.rows().flatMap((row) => (row.kind === 'gap' ? [] : [row.text])), [plan]);
  const matches = useMatchCount(lineTexts, collapsible && !expanded);

  if (plan.tooLarge) {
    return (
      <div className="min-w-0 space-y-2">
        <Note>Diff too large to compute. The old and new text are shown separately.</Note>
        <div>
          <div className="mb-1 text-[11px] uppercase tracking-[0.14em] text-muted">Old text</div>
          <Section name="old">
            <CollapsibleCode text={oldText} label="Old text" />
          </Section>
        </div>
        <div>
          <div className="mb-1 text-[11px] uppercase tracking-[0.14em] text-muted">New text</div>
          <Section name="new">
            <CollapsibleCode text={newText} label="New text" />
          </Section>
        </div>
      </div>
    );
  }

  return (
    <div className="min-w-0">
      <div
        id={id}
        tabIndex={0}
        role="region"
        aria-label="Diff"
        className="overflow-x-auto rounded-xl border border-line bg-canvas/80 py-1 font-mono text-xs leading-relaxed text-ink [tab-size:4]"
      >
        <div className="min-w-max">
          {rows.map((row, index) => (
            <DiffLine key={index} row={row} />
          ))}
        </div>
      </div>
      {collapsible && (
        <ExpandToggle
          expanded={expanded}
          onToggle={() => setExpanded((value) => !value)}
          collapsedLabel={`Show all ${plan.lineCount.toLocaleString('en-US')} lines`}
          controls={id}
          matches={matches}
        />
      )}
    </div>
  );
}

/**
 * Old text → new text as a unified line diff: removed lines tinted red with `-`,
 * added lines tinted green with `+`, a few lines of context around each change,
 * and longer unchanged runs folded into a single "N unchanged lines" row. Long
 * diffs collapse like CollapsibleCode. Scrolls sideways rather than wrapping so
 * indentation stays readable.
 *
 * Only the rows on screen are built (the totals come from the plan), and a diff
 * too long to compute is shown as the two texts side by side instead. Texts that
 * show no change are not diffed at all (see `UnchangedText`).
 */
export function TextDiff({ oldText, newText, maxRows = 12 }: TextDiffProps) {
  // Same test the diff itself would give (no line differs once the line endings
  // are folded), without splitting a text of any size into lines to find out.
  const unchanged = useMemo(() => displayText(oldText) === displayText(newText), [oldText, newText]);
  return unchanged ? (
    <UnchangedText oldText={oldText} newText={newText} maxRows={maxRows} />
  ) : (
    <ChangedDiff oldText={oldText} newText={newText} maxRows={maxRows} />
  );
}
