/**
 * Line diff for the Edit / MultiEdit tool calls: old text → new text, shown as
 * a unified diff with a few lines of context around each change.
 *
 * The cost of one diff is bounded, because a node can carry hundreds of them and
 * can be hostile:
 * - the common head and tail are trimmed in linear time first;
 * - only the differing middle goes through the diff algorithm, which is told to
 *   give up past MAX_EDIT_LENGTH insertions plus deletions, and after
 *   DIFF_TIMEOUT_MS as a backstop;
 * - if it gives up, the middle is shown as "all removed, then all added", which
 *   is still correct, just less minimal;
 * - `planDiff` can refuse a middle that is simply too long to show (the caller
 *   then shows both texts separately);
 * - a plan knows its totals without building any rows; `rows(limit)` builds only
 *   as many as asked for, so a collapsed diff costs a dozen rows, not thousands.
 */
import unidiff from 'unidiff';

export type DiffRow =
  | { kind: 'context' | 'add' | 'remove'; text: string }
  /** A run of unchanged lines that is not shown. */
  | { kind: 'gap'; hidden: number };

type ChangeRow = Exclude<DiffRow, { kind: 'gap' }>;

/**
 * The most insertions plus deletions the minimal-diff search runs to. Measured at
 * about 1 ms for a diff that hits it (24 ms at 500).
 */
export const MAX_EDIT_LENGTH = 100;
/** A backstop far above the expected cost of a capped search, for input that makes comparing lines slow. */
const DIFF_TIMEOUT_MS = 200;
/** What the Reviewer's diff view computes: a changed middle with more lines than this (both sides) is not diffed. */
export const MAX_DIFF_LINES = 5000;
const DEFAULT_CONTEXT_LINES = 3;

function toLines(text: string): string[] {
  return text === '' ? [] : text.split('\n');
}

/** A stretch of lines of one kind: `lines[from..to)`. */
interface Run {
  kind: 'context' | 'add' | 'remove';
  lines: readonly string[];
  from: number;
  to: number;
}

/** What is shown, in order: a stretch of lines, or a count of hidden unchanged lines. */
type Piece = { run: Run; from: number; to: number } | { gap: number };

export interface DiffPlan {
  /** The changed middle is longer than `maxDiffLines`: nothing was computed and `rows()` is empty. */
  tooLarge: boolean;
  /** Rows the diff has, fold markers included. */
  total: number;
  /** Rows that are lines (not fold markers): the figure for "Show all N lines". */
  lineCount: number;
  /** Whether any line was added or removed. */
  changed: boolean;
  /** The first `limit` rows (all of them by default), built only as far as asked. */
  rows(limit?: number): DiffRow[];
}

export interface DiffOptions {
  /** Unchanged lines kept on each side of a change; longer runs are folded. Not finite: keep all. */
  context?: number;
  /** Refuse (see `tooLarge`) when both sides of the changed middle together have more lines than this. */
  maxDiffLines?: number;
}

const TOO_LARGE: DiffPlan = {
  tooLarge: true,
  total: 0,
  lineCount: 0,
  changed: true,
  rows: () => [],
};

function addRun(runs: Run[], kind: Run['kind'], lines: readonly string[], from: number, to: number) {
  if (to > from) runs.push({ kind, lines, from, to });
}

/** The edit script for the differing middle of the two texts. */
function middleRuns(a: string[], b: string[], head: number, aEnd: number, bEnd: number): Run[] {
  const runs: Run[] = [];
  const removeAllThenAddAll = () => {
    addRun(runs, 'remove', a, head, aEnd);
    addRun(runs, 'add', b, head, bEnd);
  };
  if (aEnd === head || bEnd === head) {
    removeAllThenAddAll();
    return runs;
  }
  const changes = unidiff.diffArrays(a.slice(head, aEnd), b.slice(head, bEnd), {
    maxEditLength: MAX_EDIT_LENGTH,
    timeout: DIFF_TIMEOUT_MS,
  });
  if (changes === undefined) {
    removeAllThenAddAll();
    return runs;
  }
  for (const change of changes) {
    addRun(runs, change.added ? 'add' : change.removed ? 'remove' : 'context', change.value, 0, change.value.length);
  }
  return runs;
}

/**
 * Decides what is shown of each unchanged run, as arithmetic on run lengths (no
 * per-line work): the `context` lines next to a change stay, a longer stretch is
 * folded into one gap.
 */
function foldedPieces(runs: Run[], context: number): Piece[] {
  const changeAt = (run: Run) => run.kind !== 'context';
  const first = runs.findIndex(changeAt);
  let last = runs.length - 1;
  while (last > first && !changeAt(runs[last])) last--;
  const pieces: Piece[] = [];
  const keep = (run: Run, from: number, to: number) => {
    if (to > from) pieces.push({ run, from, to });
  };

  runs.forEach((run, index) => {
    if (run.kind !== 'context') {
      keep(run, run.from, run.to);
      return;
    }
    const length = run.to - run.from;
    if (index < first) {
      // Before the first change: only the lines next to it.
      const from = Math.max(run.from, run.to - context);
      if (from > run.from) pieces.push({ gap: from - run.from });
      keep(run, from, run.to);
    } else if (index > last) {
      // After the last change.
      const to = Math.min(run.to, run.from + context);
      keep(run, run.from, to);
      if (to < run.to) pieces.push({ gap: run.to - to });
    } else if (length <= 2 * context) {
      keep(run, run.from, run.to);
    } else {
      keep(run, run.from, run.from + context);
      pieces.push({ gap: length - 2 * context });
      keep(run, run.to - context, run.to);
    }
  });
  return pieces;
}

/**
 * Plans the diff of two texts. Nothing but the totals is computed up front; see
 * `DiffPlan`. When nothing changed, every line is kept (there is nothing to fold
 * around), so the text is still visible.
 */
export function planDiff(oldText: string, newText: string, options: DiffOptions = {}): DiffPlan {
  const context = options.context ?? DEFAULT_CONTEXT_LINES;
  const maxDiffLines = options.maxDiffLines ?? Infinity;
  const a = toLines(oldText);
  const b = toLines(newText);

  let head = 0;
  const shortest = Math.min(a.length, b.length);
  while (head < shortest && a[head] === b[head]) head++;
  let tail = 0;
  while (tail < shortest - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++;
  const aEnd = a.length - tail;
  const bEnd = b.length - tail;

  if (aEnd - head + (bEnd - head) > maxDiffLines) return TOO_LARGE;

  const runs: Run[] = [];
  addRun(runs, 'context', a, 0, head);
  runs.push(...middleRuns(a, b, head, aEnd, bEnd));
  addRun(runs, 'context', a, aEnd, a.length);

  const changed = runs.some((run) => run.kind !== 'context');
  const pieces: Piece[] =
    changed && Number.isFinite(context)
      ? foldedPieces(runs, context)
      : runs.map((run) => ({ run, from: run.from, to: run.to }));

  let total = 0;
  let lineCount = 0;
  for (const piece of pieces) {
    if ('gap' in piece) {
      total += 1;
    } else {
      total += piece.to - piece.from;
      lineCount += piece.to - piece.from;
    }
  }

  return {
    tooLarge: false,
    total,
    lineCount,
    changed,
    rows(limit = Infinity) {
      const rows: DiffRow[] = [];
      for (const piece of pieces) {
        if (rows.length >= limit) break;
        if ('gap' in piece) {
          rows.push({ kind: 'gap', hidden: piece.gap });
          continue;
        }
        const { run } = piece;
        for (let i = piece.from; i < piece.to && rows.length < limit; i++) {
          rows.push({ kind: run.kind, text: run.lines[i] });
        }
      }
      return rows;
    },
  };
}

/**
 * Every line of the diff, unchanged lines included. Concatenating the
 * `context` and `remove` rows gives back `oldText`'s lines; `context` and `add`
 * give `newText`'s lines.
 */
export function diffAllLines(oldText: string, newText: string): ChangeRow[] {
  return planDiff(oldText, newText, { context: Infinity }).rows() as ChangeRow[];
}

/**
 * The diff to display: changed lines plus `context` unchanged lines on each
 * side, with longer unchanged runs collapsed into a single `gap` row. When
 * nothing changed, every line is kept so the text is still visible.
 */
export function diffRows(
  oldText: string,
  newText: string,
  context: number = DEFAULT_CONTEXT_LINES,
): DiffRow[] {
  return planDiff(oldText, newText, { context }).rows();
}
