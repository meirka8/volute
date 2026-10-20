/**
 * The line diff must stay cheap and truthful on input built to be expensive:
 * long runs of lines with nothing in common, edits just under and just over the
 * give-up threshold, and line-ending differences.
 */
import { describe, expect, it } from 'vitest';
import { diffAllLines, diffRows, MAX_EDIT_LENGTH } from './lineDiff';
import { displayText } from './textLines';

/** The fastest of a few runs, so a busy machine cannot fail a check meant to catch runaway cost. */
function bestOf(runs: number, work: () => void): number {
  let best = Infinity;
  for (let run = 0; run < runs; run++) {
    const started = performance.now();
    work();
    best = Math.min(best, performance.now() - started);
  }
  return best;
}

/** Old and new line lists recovered from a full diff: the diff must contain both texts. */
function sides(oldText: string, newText: string) {
  const rows = diffAllLines(oldText, newText);
  const lines = (...keep: string[]) => rows.filter((row) => keep.includes(row.kind)).map((row) => row.text);
  return { rows, before: lines('context', 'remove'), after: lines('context', 'add') };
}

/** Deterministic xorshift, so a failure is reproducible. */
function prng(seed: number): () => number {
  let state = seed;
  return () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x100000000;
  };
}

const lowercase = 'abcdefghijklmnopqrstuvwxyz';
const uppercase = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ';

/** `count` lines of one character each, cycling through `alphabet`: count * 2 - 1 bytes. */
const oneCharLines = (count: number, alphabet: string): string =>
  Array.from({ length: count }, (_, i) => alphabet[i % alphabet.length]).join('\n');

describe('diffAllLines: input with nothing in common', () => {
  const SIZE = 8192; // 8192 one-character lines, about 16 KiB
  const oldText = oneCharLines(SIZE, lowercase);
  const newText = oneCharLines(SIZE, uppercase);

  it('is about 16 KiB a side', () => {
    expect(oldText.length).toBe(2 * SIZE - 1);
    expect(newText.length).toBe(2 * SIZE - 1);
  });

  it('gives up on the minimal diff and finishes quickly', () => {
    const best = bestOf(3, () => {
      diffAllLines(oldText, newText);
    });
    // Expected: tens of milliseconds at most. Minimal-diff effort of ~10^8 steps would not be.
    expect(best).toBeLessThan(500);
  });

  it('still shows both sides in full: everything removed, then everything added', () => {
    const { rows, before, after } = sides(oldText, newText);
    expect(rows.filter((row) => row.kind === 'remove')).toHaveLength(SIZE);
    expect(rows.filter((row) => row.kind === 'add')).toHaveLength(SIZE);
    expect(rows.filter((row) => row.kind === 'context')).toHaveLength(0);
    expect(before.join('\n')).toBe(oldText);
    expect(after.join('\n')).toBe(newText);
    // The fallback is "all removed, then all added", never interleaved.
    const kinds = rows.map((row) => row.kind);
    expect(kinds.slice(0, SIZE).every((kind) => kind === 'remove')).toBe(true);
    expect(kinds.slice(SIZE).every((kind) => kind === 'add')).toBe(true);
  });

  it('shows the whole diff as changes, with no gap to hide a line', () => {
    const rows = diffRows(oldText, newText);
    expect(rows).toHaveLength(2 * SIZE);
    expect(rows.some((row) => row.kind === 'gap')).toBe(false);
  });

  it('copes with a megabyte of such lines on each side', () => {
    const big = 512 * 1024; // lines per side: 1 MiB of text
    const oldBig = oneCharLines(big, lowercase);
    const newBig = oneCharLines(big, uppercase);
    let count = 0;
    const best = bestOf(2, () => {
      count = diffAllLines(oldBig, newBig).length;
    });
    expect(count).toBe(2 * big);
    expect(best).toBeLessThan(3000);
  });
});

describe('diffAllLines: input that overlaps but differs everywhere', () => {
  it('terminates quickly on random one-character lines and reconstructs both sides', () => {
    const rand = prng(0x2f6e2b1);
    const pick = (count: number, alphabet: string) =>
      Array.from({ length: count }, () => alphabet[Math.floor(rand() * alphabet.length)]).join('\n');
    // A tiny alphabet makes accidental matches (and so long snakes) common.
    for (const alphabet of ['ab', 'abcd', 'abcdefgh']) {
      const oldText = pick(8192, alphabet);
      const newText = pick(8192, alphabet);
      let result: ReturnType<typeof sides> | undefined;
      const best = bestOf(2, () => {
        result = sides(oldText, newText);
      });
      expect(result?.before.join('\n'), alphabet).toBe(oldText);
      expect(result?.after.join('\n'), alphabet).toBe(newText);
      expect(best, alphabet).toBeLessThan(1500);
    }
  });

  it('terminates on lines that are long and equal for a long prefix', () => {
    // Comparing two such lines costs a thousand character comparisons, and the
    // diff makes up to ~125,000 comparisons before it gives up.
    const prefix = 'x'.repeat(999);
    const oldText = Array.from({ length: 1000 }, (_, i) => `${prefix}${i}`).join('\n');
    const newText = Array.from({ length: 1000 }, (_, i) => `${prefix}${i + 1000}`).join('\n');
    let result: ReturnType<typeof sides> | undefined;
    const best = bestOf(2, () => {
      result = sides(oldText, newText);
    });
    expect(result?.before.join('\n')).toBe(oldText);
    expect(result?.after.join('\n')).toBe(newText);
    expect(best).toBeLessThan(3000);
  });

  it('handles single lines of a megabyte', () => {
    const oldText = `${'a'.repeat(1024 * 1024)}1`;
    const newText = `${'a'.repeat(1024 * 1024)}2`;
    const best = bestOf(3, () => {
      expect(diffAllLines(oldText, newText).map((row) => row.kind)).toEqual(['remove', 'add']);
    });
    expect(best).toBeLessThan(500);
  });

  it('trims a megabyte of common head and tail in linear time', () => {
    const common = Array.from({ length: 200_000 }, (_, i) => `line ${i}`);
    const oldText = [...common, 'old middle', ...common].join('\n');
    const newText = [...common, 'new middle', ...common].join('\n');
    let rows: ReturnType<typeof diffAllLines> = [];
    const best = bestOf(2, () => {
      rows = diffAllLines(oldText, newText);
    });
    expect(rows.filter((row) => row.kind !== 'context').map((row) => `${row.kind}:${row.text}`)).toEqual([
      'remove:old middle',
      'add:new middle',
    ]);
    expect(rows).toHaveLength(2 * common.length + 2);
    expect(best).toBeLessThan(1500);
  });
});

describe('diffAllLines: around the give-up threshold', () => {
  /** 700 numbered lines in which `changed` of the first 2 * changed lines (the even ones) are rewritten. */
  function scattered(changed: number) {
    const lines = Array.from({ length: 700 }, (_, i) => `line ${i}`);
    const edited = lines.map((line, i) => (i % 2 === 0 && i / 2 < changed ? `${line} (edited)` : line));
    return { oldText: lines.join('\n'), newText: edited.join('\n') };
  }

  it('finds the minimal diff while the edit is small, keeping the lines between changes as context', () => {
    // The search runs to MAX_EDIT_LENGTH (100) insertions plus deletions; this edit is 80 of them.
    // (It was 200 while the limit was 500. The limit came down to 100 because a node can hold
    // hundreds of diffs that each hit it: about 1 ms each instead of 24 ms.)
    const { oldText, newText } = scattered(40);
    const { rows, before, after } = sides(oldText, newText);
    expect(rows.filter((row) => row.kind === 'remove')).toHaveLength(40);
    expect(rows.filter((row) => row.kind === 'add')).toHaveLength(40);
    // 700 - 40 unchanged lines stay context.
    expect(rows.filter((row) => row.kind === 'context')).toHaveLength(660);
    expect(before.join('\n')).toBe(oldText);
    expect(after.join('\n')).toBe(newText);
  });

  it('is minimal up to exactly MAX_EDIT_LENGTH edits and falls back just past it', () => {
    expect(MAX_EDIT_LENGTH).toBe(100);
    // scattered(n) is n deletions and n insertions: 2n edits.
    const atLimit = sides(...Object.values(scattered(50)) as [string, string]);
    expect(atLimit.rows.filter((row) => row.kind === 'remove')).toHaveLength(50);
    expect(atLimit.rows.filter((row) => row.kind === 'context')).toHaveLength(650);

    const pastLimit = sides(...Object.values(scattered(51)) as [string, string]);
    // The unchanged lines inside the edited span are no longer recognised as unchanged.
    expect(pastLimit.rows.filter((row) => row.kind === 'remove').length).toBeGreaterThan(51);
    expect(pastLimit.rows.filter((row) => row.kind === 'context').length).toBeLessThan(650);
  });

  it('falls back to remove-then-add once the edit is large, and still reconstructs both texts', () => {
    const { oldText, newText } = scattered(340); // 680 insertions and deletions
    const { rows, before, after } = sides(oldText, newText);
    // The unchanged odd lines inside the edited span are no longer recognised as unchanged.
    expect(rows.filter((row) => row.kind === 'remove').length).toBeGreaterThan(340);
    expect(rows.filter((row) => row.kind === 'add').length).toBeGreaterThan(340);
    expect(before.join('\n')).toBe(oldText);
    expect(after.join('\n')).toBe(newText);
  });

  it('keeps head and tail context around a fallback middle', () => {
    const { oldText, newText } = scattered(340);
    const rows = diffAllLines(oldText, newText);
    expect(rows[rows.length - 1]).toEqual({ kind: 'context', text: 'line 699' });
    const firstChange = rows.findIndex((row) => row.kind !== 'context');
    expect(rows.slice(0, firstChange).every((row) => row.kind === 'context')).toBe(true);
  });

  it('reconstructs both sides for random edits of every size from nothing to far past the threshold', () => {
    const rand = prng(0x7a11ad);
    const alphabet = ['a', 'b', 'c', 'd', '', '  indented', '}', '{', '// note'];
    for (let run = 0; run < 80; run++) {
      const length = Math.floor(rand() * 900);
      const lines = Array.from({ length }, () => alphabet[Math.floor(rand() * alphabet.length)]);
      // Rewrite a random share of the lines, insert and delete some.
      const share = rand();
      const edited: string[] = [];
      for (const line of lines) {
        const roll = rand();
        if (roll < share * 0.4) edited.push(alphabet[Math.floor(rand() * alphabet.length)]);
        else if (roll < share * 0.6) continue;
        else if (roll < share * 0.8) edited.push(line, alphabet[Math.floor(rand() * alphabet.length)]);
        else edited.push(line);
      }
      const oldText = lines.join('\n');
      const newText = edited.join('\n');
      const { before, after } = sides(oldText, newText);
      expect(before.join('\n'), `run ${run}`).toBe(oldText);
      expect(after.join('\n'), `run ${run}`).toBe(newText);
    }
  });
});

describe('diff of text that only differs in line endings', () => {
  it('reports every line as changed when the raw texts are compared', () => {
    const { rows } = sides('a\r\nb\r\nc', 'a\nb\nc');
    expect(rows.filter((row) => row.kind === 'remove').map((row) => row.text)).toEqual(['a\r', 'b\r']);
  });

  it('reports no change once line endings are normalised, as the diff view does', () => {
    for (const [a, b] of [
      ['a\r\nb\r\nc', 'a\nb\nc'],
      ['a\rb\rc', 'a\nb\nc'],
      ['a\r\nb\rc\nd', 'a\nb\nc\nd'],
      ['a\r\n', 'a\n'],
    ]) {
      const rows = diffRows(displayText(a), displayText(b));
      expect(rows.every((row) => row.kind === 'context'), JSON.stringify([a, b])).toBe(true);
      expect(rows.length).toBeGreaterThan(0);
    }
  });

  it('still reports a real change that sits among Windows line endings', () => {
    const rows = diffRows(displayText('a\r\nb\r\nc'), displayText('a\r\nB\r\nc'));
    expect(rows.filter((row) => row.kind !== 'context').map((row) => `${row.kind}:${row.kind === 'gap' ? '' : row.text}`)).toEqual([
      'remove:b',
      'add:B',
    ]);
  });
});

describe('diffAllLines: degenerate text', () => {
  const kinds = (oldText: string, newText: string) => diffAllLines(oldText, newText).map((row) => `${row.kind}:${row.text}`);

  it('treats a trailing newline as an empty last line', () => {
    expect(kinds('a\n', 'a')).toEqual(['context:a', 'remove:']);
    expect(kinds('a', 'a\n')).toEqual(['context:a', 'add:']);
  });

  it('keeps whitespace-only differences visible', () => {
    expect(kinds('a ', 'a')).toEqual(['remove:a ', 'add:a']);
    expect(kinds('\ta', '    a')).toEqual(['remove:\ta', 'add:    a']);
  });

  it('shows text made only of newlines', () => {
    expect(kinds('\n', '')).toEqual(['remove:', 'remove:']);
    expect(kinds('', '\n\n')).toEqual(['add:', 'add:', 'add:']);
    expect(kinds('\n', '\n')).toEqual(['context:', 'context:']);
  });

  it('is exact for identical text of any size', () => {
    const text = oneCharLines(50_000, lowercase);
    const rows = diffAllLines(text, text);
    expect(rows).toHaveLength(50_000);
    expect(rows.every((row) => row.kind === 'context')).toBe(true);
  });

  it('keeps lines made of regex-special and markup-like characters as plain text', () => {
    const odd = ['<script>alert(1)</script>', '.*+?^${}()|[]\\', '‮evil', '__proto__', 'constructor', ''];
    const { before, after } = sides(odd.join('\n'), [...odd].reverse().join('\n'));
    expect(before).toEqual(odd);
    expect(after).toEqual([...odd].reverse());
  });
});

describe('diffRows: unusual context sizes', () => {
  const numbered = (count: number, change?: number) =>
    Array.from({ length: count }, (_, i) => (i === change ? `changed ${i}` : `line ${i}`)).join('\n');

  it('shows only the changes with no context at all', () => {
    const rows = diffRows(numbered(20), numbered(20, 10), 0);
    expect(rows.map((row) => (row.kind === 'gap' ? `gap${row.hidden}` : `${row.kind}:${row.text}`))).toEqual([
      'gap10',
      'remove:line 10',
      'add:changed 10',
      'gap9',
    ]);
  });

  it('shows everything for a context that is not a number of lines', () => {
    for (const context of [NaN, Infinity]) {
      const rows = diffRows(numbered(30), numbered(30, 15), context);
      expect(rows.some((row) => row.kind === 'gap')).toBe(false);
      expect(rows).toHaveLength(31);
    }
  });

  it('accounts for every line: shown rows plus hidden counts equal the full diff', () => {
    for (const context of [0, 1, 2, 3, 5, 8]) {
      const oldText = numbered(60);
      const newText = oldText.replace('line 7', 'changed 7').replace('line 40', 'changed 40').replace('line 41', 'changed 41');
      const full = diffAllLines(oldText, newText);
      const rows = diffRows(oldText, newText, context);
      const accounted = rows.reduce((sum, row) => sum + (row.kind === 'gap' ? row.hidden : 1), 0);
      expect(accounted, `context ${context}`).toBe(full.length);
      // Every changed line is visible whatever the context.
      expect(rows.filter((row) => row.kind === 'remove' || row.kind === 'add')).toHaveLength(
        full.filter((row) => row.kind === 'remove' || row.kind === 'add').length,
      );
    }
  });
});
