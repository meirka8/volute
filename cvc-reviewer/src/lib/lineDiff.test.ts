import unidiff from 'unidiff';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { diffAllLines, diffRows, MAX_DIFF_LINES, MAX_EDIT_LENGTH, planDiff, type DiffRow } from './lineDiff';

function kinds(rows: DiffRow[]): string[] {
  return rows.map((row) => (row.kind === 'gap' ? `gap${row.hidden}` : `${row.kind}:${row.text}`));
}

/** Old and new line lists recovered from a full (gap-free) diff. */
function sides(oldText: string, newText: string) {
  const rows = diffAllLines(oldText, newText);
  const lines = (...keep: string[]) => rows.filter((row) => keep.includes(row.kind)).map((row) => row.text);
  return { before: lines('context', 'remove'), after: lines('context', 'add') };
}

describe('diffAllLines', () => {
  it('marks a replaced line between unchanged lines', () => {
    expect(kinds(diffAllLines('a\nb\nc', 'a\nB\nc'))).toEqual(['context:a', 'remove:b', 'add:B', 'context:c']);
  });

  it('shows an insertion and a deletion', () => {
    expect(kinds(diffAllLines('a\nc', 'a\nb\nc'))).toEqual(['context:a', 'add:b', 'context:c']);
    expect(kinds(diffAllLines('a\nb\nc', 'a\nc'))).toEqual(['context:a', 'remove:b', 'context:c']);
  });

  it('treats an empty side as nothing, not as one blank line', () => {
    expect(kinds(diffAllLines('', 'x\ny'))).toEqual(['add:x', 'add:y']);
    expect(kinds(diffAllLines('x\ny', ''))).toEqual(['remove:x', 'remove:y']);
    expect(diffAllLines('', '')).toEqual([]);
  });

  it('reports identical text as all context', () => {
    expect(kinds(diffAllLines('a\nb', 'a\nb'))).toEqual(['context:a', 'context:b']);
  });

  it('keeps blank lines and indentation', () => {
    expect(kinds(diffAllLines('if (x) {\n\n  a();\n}', 'if (x) {\n\n  b();\n}'))).toEqual([
      'context:if (x) {',
      'context:',
      'remove:  a();',
      'add:  b();',
      'context:}',
    ]);
  });

  it('does not report a trailing-newline difference as a rewrite of the last line', () => {
    expect(kinds(diffAllLines('a\nb', 'a\nb\nc'))).toEqual(['context:a', 'context:b', 'add:c']);
  });

  it('gives a usable diff when the edit is too large to diff minimally', () => {
    const oldText = Array.from({ length: 700 }, (_, i) => `old line ${i}`).join('\n');
    const newText = Array.from({ length: 700 }, (_, i) => `new line ${i}`).join('\n');
    const rows = diffAllLines(oldText, newText);
    expect(rows.filter((row) => row.kind === 'remove')).toHaveLength(700);
    expect(rows.filter((row) => row.kind === 'add')).toHaveLength(700);
    expect(sides(oldText, newText)).toEqual({ before: oldText.split('\n'), after: newText.split('\n') });
  });

  it('keeps the common head and tail even when the middle is too large to diff', () => {
    const middle = (prefix: string) => Array.from({ length: 700 }, (_, i) => `${prefix} ${i}`);
    const oldText = ['head', ...middle('old'), 'tail'].join('\n');
    const newText = ['head', ...middle('new'), 'tail'].join('\n');
    const rows = diffAllLines(oldText, newText);
    expect(rows[0]).toEqual({ kind: 'context', text: 'head' });
    expect(rows[rows.length - 1]).toEqual({ kind: 'context', text: 'tail' });
  });

  it('reconstructs both sides exactly for random edits', () => {
    // Deterministic xorshift, so a failure is reproducible.
    let state = 0x9e3779b9;
    const next = () => {
      state ^= state << 13;
      state ^= state >>> 17;
      state ^= state << 5;
      return (state >>> 0) / 0x100000000;
    };
    const alphabet = ['a', 'b', 'c', '', '  indented', '}', '{'];
    const randomText = () =>
      Array.from({ length: Math.floor(next() * 14) }, () => alphabet[Math.floor(next() * alphabet.length)]).join('\n');
    for (let run = 0; run < 1500; run++) {
      const oldText = randomText();
      const newText = randomText();
      const { before, after } = sides(oldText, newText);
      expect(before.join('\n')).toBe(oldText);
      expect(after.join('\n')).toBe(newText);
    }
  });
});

describe('diffRows', () => {
  const numbered = (count: number, change?: number) =>
    Array.from({ length: count }, (_, i) => (i === change ? `changed ${i}` : `line ${i}`)).join('\n');

  it('collapses long unchanged runs into gap rows, keeping three lines of context', () => {
    const rows = diffRows(numbered(30), numbered(30, 15));
    expect(kinds(rows)).toEqual([
      'gap12',
      'context:line 12',
      'context:line 13',
      'context:line 14',
      'remove:line 15',
      'add:changed 15',
      'context:line 16',
      'context:line 17',
      'context:line 18',
      'gap11',
    ]);
  });

  it('merges nearby changes without a gap between them', () => {
    const oldText = numbered(10);
    const newText = oldText.replace('line 3', 'changed 3').replace('line 6', 'changed 6');
    // Two unchanged lines separate the changes, and every other line is within
    // three lines of one of them: a single hunk with nothing hidden.
    expect(diffRows(oldText, newText).some((row) => row.kind === 'gap')).toBe(false);
  });

  it('shows a gap only where more than the context window separates changes', () => {
    const oldText = numbered(40);
    const newText = oldText.replace('line 5', 'changed 5').replace('line 30', 'changed 30');
    // 42 rows in all. Rows 0-1 lead the first hunk, rows 10-27 lie between the
    // hunks, and rows 36-41 trail the last one.
    expect(diffRows(oldText, newText).filter((row) => row.kind === 'gap')).toEqual([
      { kind: 'gap', hidden: 2 },
      { kind: 'gap', hidden: 18 },
      { kind: 'gap', hidden: 6 },
    ]);
  });

  it('honours a custom context size', () => {
    const rows = diffRows(numbered(10), numbered(10, 5), 1);
    expect(kinds(rows)).toEqual(['gap4', 'context:line 4', 'remove:line 5', 'add:changed 5', 'context:line 6', 'gap3']);
  });

  it('keeps every line when nothing changed, so the text stays visible', () => {
    expect(kinds(diffRows('a\nb', 'a\nb'))).toEqual(['context:a', 'context:b']);
  });

  it('keeps every line with unlimited context', () => {
    const rows = diffRows(numbered(30), numbered(30, 15), Infinity);
    expect(rows.some((row) => row.kind === 'gap')).toBe(false);
    expect(rows).toHaveLength(31);
  });

  it('is empty for two empty texts', () => {
    expect(diffRows('', '')).toEqual([]);
  });
});

/**
 * The folding rule written the long way, one row at a time, independently of the
 * run arithmetic `planDiff` uses: a row stays if it is within `context` rows of a
 * changed row.
 */
function foldReference(oldText: string, newText: string, context: number): DiffRow[] {
  const all = diffAllLines(oldText, newText);
  if (!Number.isFinite(context) || !all.some((row) => row.kind !== 'context')) return all;
  const visible = new Array<boolean>(all.length).fill(false);
  for (let i = 0; i < all.length; i++) {
    if (all[i].kind === 'context') continue;
    for (let j = Math.max(0, i - context); j <= Math.min(all.length - 1, i + context); j++) visible[j] = true;
  }
  const rows: DiffRow[] = [];
  let hidden = 0;
  all.forEach((row, i) => {
    if (visible[i]) {
      if (hidden > 0) rows.push({ kind: 'gap', hidden });
      hidden = 0;
      rows.push(row);
    } else {
      hidden++;
    }
  });
  if (hidden > 0) rows.push({ kind: 'gap', hidden });
  return rows;
}

describe('planDiff', () => {
  afterEach(() => vi.restoreAllMocks());

  const numbered = (count: number, changes: number[] = []) =>
    Array.from({ length: count }, (_, i) => (changes.includes(i) ? `changed ${i}` : `line ${i}`)).join('\n');

  it('folds exactly as the row-by-row rule does, for random edits and every context size', () => {
    let state = 0x51ed270b;
    const rand = () => {
      state ^= state << 13;
      state ^= state >>> 17;
      state ^= state << 5;
      return (state >>> 0) / 0x100000000;
    };
    for (let run = 0; run < 400; run++) {
      const length = Math.floor(rand() * 60);
      const lines = Array.from({ length }, (_, i) => `l${i}`);
      const edited = lines.flatMap((line) => {
        const roll = rand();
        return roll < 0.1 ? [`${line}!`] : roll < 0.15 ? [] : roll < 0.2 ? [line, `${line}+`] : [line];
      });
      const oldText = lines.join('\n');
      const newText = edited.join('\n');
      for (const context of [0, 1, 2, 3, 7, Infinity]) {
        expect(diffRows(oldText, newText, context), `run ${run} context ${context}`).toEqual(foldReference(oldText, newText, context));
      }
    }
  });

  it('reports totals that match the rows it builds', () => {
    for (const [oldText, newText] of [
      [numbered(80), numbered(80, [5, 40, 41])],
      [numbered(10), numbered(10)],
      ['', ''],
      ['', 'a\nb'],
      ['a\nb', ''],
      [numbered(300), numbered(300).replaceAll('line', 'LINE')],
    ]) {
      const plan = planDiff(oldText, newText);
      const rows = plan.rows();
      expect(plan.total).toBe(rows.length);
      expect(plan.lineCount).toBe(rows.filter((row) => row.kind !== 'gap').length);
      expect(plan.changed).toBe(rows.some((row) => row.kind === 'add' || row.kind === 'remove'));
      expect(plan.tooLarge).toBe(false);
    }
  });

  it('builds only as many rows as asked for, and they are the first ones', () => {
    const plan = planDiff(numbered(200), numbered(200, [100, 101, 150]));
    const all = plan.rows();
    expect(all.length).toBeGreaterThan(12);
    for (const limit of [0, 1, 5, 12, all.length - 1, all.length, all.length + 50]) {
      expect(plan.rows(limit), `limit ${limit}`).toEqual(all.slice(0, limit));
    }
  });

  it('needs no row to know its totals, even when everything is removed and then added', () => {
    // The give-up fallback: 2,400 lines each way is 4,800 rows. The plan holds two runs.
    const oldText = Array.from({ length: 2400 }, (_, i) => `old ${i}`).join('\n');
    const newText = Array.from({ length: 2400 }, (_, i) => `new ${i}`).join('\n');
    const plan = planDiff(oldText, newText);
    expect(plan.total).toBe(4800);
    expect(plan.lineCount).toBe(4800);
    const first = plan.rows(12);
    expect(first).toHaveLength(12);
    expect(first.every((row) => row.kind === 'remove')).toBe(true);
    expect(plan.rows(2401)[2400]).toEqual({ kind: 'add', text: 'new 0' });
  });

  it('refuses a changed middle that is longer than the limit, both sides counted, and computes nothing', () => {
    const search = vi.spyOn(unidiff, 'diffArrays');
    const oldText = Array.from({ length: 3000 }, (_, i) => `old ${i}`).join('\n');
    const newText = Array.from({ length: 2001 }, (_, i) => `new ${i}`).join('\n');
    const plan = planDiff(oldText, newText, { maxDiffLines: MAX_DIFF_LINES });
    expect(plan.tooLarge).toBe(true);
    expect(plan.rows()).toEqual([]);
    expect(plan.total).toBe(0);
    expect(search).not.toHaveBeenCalled();

    // Exactly at the limit it is still diffed.
    const atLimit = planDiff(oldText, Array.from({ length: 2000 }, (_, i) => `new ${i}`).join('\n'), { maxDiffLines: MAX_DIFF_LINES });
    expect(MAX_DIFF_LINES).toBe(5000);
    expect(atLimit.tooLarge).toBe(false);
    expect(atLimit.total).toBe(5000);
  });

  it('measures the limit on the changed middle, after the common head and tail are trimmed', () => {
    const common = Array.from({ length: 50_000 }, (_, i) => `same ${i}`);
    const oldText = [...common, 'old', ...common].join('\n');
    const newText = [...common, 'new', ...common].join('\n');
    expect(planDiff(oldText, newText, { maxDiffLines: MAX_DIFF_LINES }).tooLarge).toBe(false);
  });

  it('is never "too large" without a limit, and never for identical text', () => {
    const big = Array.from({ length: 20_000 }, (_, i) => `x${i}`).join('\n');
    expect(planDiff(big, `${big}\nmore`).tooLarge).toBe(false);
    expect(planDiff(big, big, { maxDiffLines: 1 }).tooLarge).toBe(false);
  });

  it('gives the search the edit cap and a timeout as a backstop', () => {
    const search = vi.spyOn(unidiff, 'diffArrays');
    planDiff('a\nb\nc', 'a\nB\nc');
    expect(search).toHaveBeenCalledTimes(1);
    const options = search.mock.calls[0][2];
    expect(options?.maxEditLength).toBe(MAX_EDIT_LENGTH);
    expect(MAX_EDIT_LENGTH).toBe(100);
    expect(options?.timeout).toBeGreaterThan(0);
    expect(options?.timeout).toBeLessThanOrEqual(1000);
  });

  it('falls back to "removed, then added" when the search gives up, however it gives up', () => {
    vi.spyOn(unidiff, 'diffArrays').mockReturnValue(undefined);
    const rows = planDiff('a\nb\nc', 'a\nB\nc').rows();
    expect(rows.map((row) => (row.kind === 'gap' ? 'gap' : `${row.kind}:${row.text}`))).toEqual([
      'context:a',
      'remove:b',
      'add:B',
      'context:c',
    ]);
  });

  it('does not search at all when one side of the middle is empty', () => {
    const search = vi.spyOn(unidiff, 'diffArrays');
    planDiff('a\nc', 'a\nb\nc');
    planDiff('a\nb\nc', 'a\nc');
    expect(search).not.toHaveBeenCalled();
  });
});
