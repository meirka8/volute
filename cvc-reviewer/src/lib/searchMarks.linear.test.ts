/**
 * Marking and counting take time in proportion to the text (issue #118, revision round 4).
 *
 * The first version looked for each next occurrence of a term one character past the last, so a term that overlaps itself in a
 * text that is made of it (a megabyte of `a` and a term of five hundred of them) was found a million times, and each time
 * the engine compared up to five hundred characters: 344 ms to mark one block, and a fold's count over 4 MiB of text, which is
 * what a card counts in, about 1.4 s. The marks are now found by a scan that reads each character once (see `TermScan`); what it
 * finds must be what the first version found, to the character, and this file keeps that version as the oracle.
 */
import { describe, expect, it, vi } from 'vitest';
import { fold, sourceRange, type Folded } from './searchFold';
import { countMarks, foldForMarks, markRanges, MAX_MARKS_PER_BLOCK, type MarkRange } from './searchMarks';

// The scans of a few mebibytes, nine rounds of each, take a second or two on a machine with nothing else to do.
vi.setConfig({ testTimeout: 30_000 });

const MIB = 1024 * 1024;
/** What a measured time may exceed its share by, in milliseconds: the timer and the scheduler are not ours. */
const SLACK = 25;
const time = (run: () => void) => {
  const started = performance.now();
  run();
  return performance.now() - started;
};
/** The fastest of a few runs: noise only ever makes a run slower. */
const fastest = (run: () => void, times = 7) => Math.min(...Array.from({ length: times }, () => time(run)));
/**
 * The fastest run of each of two jobs, the two measured in turn, round after round: a load that comes and goes (another test
 * run on the same machine) is then in both of them, and what is compared is what the jobs cost.
 */
function fastestOfTwo(small: () => void, large: () => void, rounds = 9): [number, number] {
  let fastestSmall = Infinity;
  let fastestLarge = Infinity;
  for (let round = 0; round < rounds; round++) {
    fastestSmall = Math.min(fastestSmall, time(small));
    fastestLarge = Math.min(fastestLarge, time(large));
  }
  return [fastestSmall, fastestLarge];
}

/** The first version, kept as it was: the next occurrence of a term is looked for one character past the last. */
function referenceMarkRanges(block: Folded | string, terms: readonly string[], max: number): MarkRange[] {
  const folded = typeof block === 'string' ? foldForMarks(block) : block;
  const text = folded.text;
  const wanted = terms.filter((term) => term !== '');
  if (wanted.length === 0 || text === '' || max <= 0) return [];
  const next = wanted.map((term) => text.indexOf(term));
  const ranges: MarkRange[] = [];
  let start = -1;
  let end = -1;
  for (;;) {
    let best = -1;
    let bestAt = Infinity;
    for (let k = 0; k < next.length; k++) {
      const at = next[k];
      if (at >= 0 && at < bestAt) {
        best = k;
        bestAt = at;
      }
    }
    if (best < 0) break;
    const found = sourceRange(folded, bestAt, bestAt + wanted[best].length);
    if (start >= 0 && found.start <= end) {
      if (found.end > end) end = found.end;
    } else {
      if (start >= 0) ranges.push({ start, end });
      if (ranges.length >= max) {
        start = -1;
        break;
      }
      start = found.start;
      end = found.end;
    }
    next[best] = text.indexOf(wanted[best], bestAt + 1);
  }
  if (start >= 0) ranges.push({ start, end });
  return ranges;
}

describe('the marks of a text that is made of its term', () => {
  const term = 'a'.repeat(500);

  it('are one stretch, found in a few milliseconds, for a megabyte of one letter and a term of five hundred of it', () => {
    const block = foldForMarks('a'.repeat(MIB));
    let ranges: MarkRange[] = [];
    expect(fastest(() => (ranges = markRanges(block, [term])))).toBeLessThan(50);
    expect(ranges).toEqual([{ start: 0, end: MIB }]);
  });

  it('are counted over 4 MiB, the most a fold counts, in a fraction of a second', () => {
    const block = foldForMarks('a'.repeat(4 * MIB));
    let count = 0;
    expect(fastest(() => (count = countMarks(block, [term])), 5)).toBeLessThan(200);
    expect(count).toBe(1);
  });

  it('are found in a text of a period of two, for a term of a hundred of them, in the same time', () => {
    const block = foldForMarks('ab'.repeat(2 * MIB));
    const long = 'ab'.repeat(250);
    let count = 0;
    expect(fastest(() => (count = countMarks(block, [long])), 5)).toBeLessThan(200);
    expect(count).toBe(1);
    // A term of the other phase of the period is found in the same text as well (it starts one character on).
    expect(markRanges(block, ['ba'.repeat(250)], 5)).toEqual([{ start: 1, end: 4 * MIB - 1 }]);
  });

  it('are found for several terms of one period together: the stretch is the whole of the text', () => {
    const block = foldForMarks('abc'.repeat(MIB));
    const terms = ['abc'.repeat(100), 'bca'.repeat(100), 'cab'.repeat(100), 'abca'.repeat(50)];
    let ranges: MarkRange[] = [];
    // (A second, here: four terms over three mebibytes, and what is tested is that the stretch is the whole of the text.)
    expect(fastest(() => (ranges = markRanges(block, terms)), 5)).toBeLessThan(1_000);
    expect(ranges).toEqual([{ start: 0, end: 3 * MIB }]);
  });

  it('cost no more for a term four times as long: the text is read once, whatever the term is', () => {
    const block = foldForMarks('a'.repeat(MIB));
    const [short, long] = fastestOfTwo(
      () => countMarks(block, ['a'.repeat(100)]),
      () => countMarks(block, ['a'.repeat(400)]),
    );
    // The first version took four times as long: it compared the whole of the term at every place.
    expect(long).toBeLessThan(2 * short + 5);
  });

  it('cost less than three times as much for twice as much text', () => {
    const once = foldForMarks('a'.repeat(MIB));
    const twice = foldForMarks('a'.repeat(2 * MIB));
    const [small, large] = fastestOfTwo(
      () => countMarks(once, [term]),
      () => countMarks(twice, [term]),
    );
    expect(large).toBeLessThan(3 * small + SLACK);
  });
});

describe('a term that is not in the text', () => {
  it('is found to be missing in a few milliseconds, when it is all but the same as the text', () => {
    const text = foldForMarks('a'.repeat(MIB));
    for (const missing of ['b' + 'a'.repeat(499), 'a'.repeat(499) + 'b', 'a'.repeat(250) + 'b' + 'a'.repeat(249)]) {
      expect(fastest(() => markRanges(text, [missing]), 5), missing.slice(0, 4)).toBeLessThan(100);
      expect(markRanges(text, [missing])).toEqual([]);
    }
  });

  it('is found to be missing in a text that nearly has it over and over', () => {
    const unit = `${'a'.repeat(300)}b`;
    const text = foldForMarks(unit.repeat(3_000));
    const missing = `${'a'.repeat(300)}c`;
    expect(fastest(() => markRanges(text, [missing]), 5)).toBeLessThan(100);
    expect(markRanges(text, [missing])).toEqual([]);
  });
});

describe('many separate matches', () => {
  it('stop at the most that was asked for, however long the text goes on', () => {
    const text = foldForMarks('x '.repeat(MIB));
    let ranges: MarkRange[] = [];
    expect(fastest(() => (ranges = markRanges(text, ['x'])), 3)).toBeLessThan(50);
    expect(ranges).toHaveLength(MAX_MARKS_PER_BLOCK);
    expect(ranges[0]).toEqual({ start: 0, end: 1 });
    expect(ranges[99]).toEqual({ start: 198, end: 199 });
    expect(countMarks(text, ['x'], 1_000)).toBe(1_000);
  });

  it('are all of them counted, when the cap is the number of them', () => {
    const text = foldForMarks('xy '.repeat(50_000));
    expect(countMarks(text, ['x'], 100_000)).toBe(50_000);
    expect(countMarks(text, ['x', 'y'], 100_000)).toBe(50_000); // x and y touch: one stretch for each pair
    expect(countMarks(text, ['x', 'y '], 100_000)).toBe(1); // 'y ' touches the next x: the whole text is one stretch
  });
});

describe('the stretches that are found', () => {
  const marked = (text: string, terms: string[], max = 100) => markRanges(text, terms, max).map(({ start, end }) => text.slice(start, end));

  it('are one for occurrences that overlap, as many as the term can overlap itself', () => {
    expect(marked('aaaaa', ['aa'])).toEqual(['aaaaa']);
    expect(marked('abababab', ['abab'])).toEqual(['abababab']);
    expect(marked('abababab', ['aba'])).toEqual(['abababa']);
    expect(marked('aabaabaab', ['aabaa'])).toEqual(['aabaabaa']);
    expect(marked('aabaaab aabaaab', ['aabaaab'])).toEqual(['aabaaab', 'aabaaab']);
    expect(marked('aabaaabaabaaab', ['aabaaab'])).toEqual(['aabaaabaabaaab']); // two that touch are one
    expect(marked('xaaaaay', ['aaa'])).toEqual(['aaaaa']);
  });

  it('are two for occurrences that only touch in the text as it is drawn', () => {
    // In the folded text "abab" is two touching occurrences of "ab"; the characters are next to each other, so it is one stretch.
    expect(marked('abab', ['ab'])).toEqual(['abab']);
    // A hidden character between them is in neither of them: two stretches, with the token between.
    expect(marked('ab​ab', ['ab'])).toEqual(['ab', 'ab']);
    expect(marked('ab​ab', ['abab'])).toEqual(['ab​ab']);
  });

  it('are found for terms that begin inside each other, in whichever order they are given', () => {
    expect(marked('xabcdy', ['abc', 'cd'])).toEqual(['abcd']);
    expect(marked('xabcdy', ['cd', 'abc'])).toEqual(['abcd']);
    expect(marked('xabcdy', ['bc', 'abcd', 'b'])).toEqual(['abcd']);
    expect(marked('ab cd', ['cd', 'ab'])).toEqual(['ab', 'cd']);
  });

  it('are none for no term, an empty one, a term longer than the text, or no text', () => {
    expect(marked('abc', [])).toEqual([]);
    expect(marked('abc', [''])).toEqual([]);
    expect(marked('abc', ['abcd'])).toEqual([]);
    expect(marked('', ['a'])).toEqual([]);
    expect(markRanges('abc', ['b'], 0)).toEqual([]);
  });

  it('are the whole text for a term that is the text', () => {
    expect(marked('abcab', ['abcab'])).toEqual(['abcab']);
    expect(marked('a', ['a'])).toEqual(['a']);
  });

  it('are as many as the cap, counted in whole stretches: a stretch that is still growing is one', () => {
    expect(marked('a b a b a b', ['a'], 2)).toEqual(['a', 'a']);
    expect(marked('aaaa b aaaa b aaaa', ['aa'], 2)).toEqual(['aaaa', 'aaaa']);
    expect(marked('aaaaaaaa', ['aa'], 1)).toEqual(['aaaaaaaa']);
  });
});

describe('what is found is what the first version found', () => {
  /** A small deterministic source of numbers, so that a failure is the same failure the next time. */
  const generator = (seed: number) => {
    let state = seed >>> 0;
    const random = () => {
      state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
      return state / 0x100000000;
    };
    const pick = <T>(choices: readonly T[]): T => choices[Math.floor(random() * choices.length)];
    const word = (alphabet: readonly string[], length: number): string => Array.from({ length }, () => pick(alphabet)).join('');
    return { random, pick, word };
  };

  const ALPHABETS: string[][] = [
    ['a', 'b'],
    ['a', 'b', 'c'],
    ['a'],
    ['ab', 'a', 'b'],
    ['a', 'b', ' '],
    ['x', 'y', 'z', 'x', 'y'],
    // Text the fold changes: case, ligatures, accents, hidden characters, a no-break space, a letter outside the plane.
    ['a', 'B', 'ß', 'İ', 'é', '​', 'ﬁ', ' ', 'é', 'a‍b', '\u{1F600}', 'Σ', 'ς'],
  ];

  it('for 30,000 random texts and terms, in every alphabet, with one to three terms and every cap', () => {
    const { random, pick, word } = generator(12345);
    let different = 0;
    for (let round = 0; round < 30_000; round++) {
      const alphabet = pick(ALPHABETS);
      const text = word(alphabet, Math.floor(random() * 60));
      const terms = Array.from({ length: 1 + Math.floor(random() * 3) }, () => fold(word(alphabet, 1 + Math.floor(random() * 6))));
      const max = pick([1, 2, 3, 5, 100]);
      const block = foldForMarks(text);
      const got = markRanges(block, terms, max);
      const want = referenceMarkRanges(block, terms, max);
      if (JSON.stringify(got) !== JSON.stringify(want)) {
        different++;
        expect({ text, terms, max, got }).toEqual({ text, terms, max, got: want });
      }
    }
    expect(different).toBe(0);
  });

  it('for 30,000 texts that are built of the term, with a few breaks in them, which is where terms overlap themselves', () => {
    const { random, pick, word } = generator(777);
    let different = 0;
    for (let round = 0; round < 30_000; round++) {
      const unit = word(['a', 'b', 'c'], 1 + Math.floor(random() * 4));
      const term = unit.repeat(1 + Math.floor(random() * 5)) + (random() < 0.5 ? unit.slice(0, Math.floor(random() * unit.length)) : '');
      let text = '';
      for (let part = 0, parts = 1 + Math.floor(random() * 8); part < parts; part++) {
        text += unit.repeat(Math.floor(random() * 12)) + (random() < 0.5 ? word(['a', 'b', 'c', 'x'], Math.floor(random() * 3)) : '');
      }
      const terms = [term, ...(random() < 0.4 ? [unit.repeat(1 + Math.floor(random() * 3))] : [])];
      const max = pick([1, 2, 4, 100]);
      const block = foldForMarks(text);
      const got = markRanges(block, terms, max);
      const want = referenceMarkRanges(block, terms, max);
      if (JSON.stringify(got) !== JSON.stringify(want)) {
        different++;
        expect({ text, terms, max, got }).toEqual({ text, terms, max, got: want });
      }
    }
    expect(different).toBe(0);
  });

  it('for text that has hidden characters and letters that fold to more than one in the middle of a run of matches', () => {
    for (const [text, terms] of [
      ['a​a​a​a', ['aa']],
      ['ssßssß', ['ss']],
      ['ßßßß', ['ssss']],
      ['ﬁﬁﬁ', ['fifi']],
      ['éééé', ['ee']],
      ['İİİ', ['i̇i']],
      ['\u{1F600}\u{1F600}\u{1F600}', ['\u{1F600}\u{1F600}']],
    ] as Array<[string, string[]]>) {
      const folded = terms.map(fold);
      const block = foldForMarks(text);
      for (const max of [1, 2, 100]) expect(markRanges(block, folded, max), `${JSON.stringify(text)} ${max}`).toEqual(referenceMarkRanges(block, folded, max));
    }
  });
});
