/**
 * Round 3 QA of issue #118: where a search marks (src/lib/searchMarks.ts), by behaviour only. `markRanges` and `countMarks`
 * are checked against a naive reference that is written here from the definition and shares nothing with the module but the
 * fold (`foldWithMap`, which has tests of its own): every occurrence of every term in the folded text, taken back to the
 * text on the page; the occurrences that overlap or touch made one; the first `max` of them. Whatever the module does inside
 * (an indexOf per term, a linear scan of many) its output is the reference's, on random text with hidden characters,
 * joiners, ligatures, accents, astral characters and line breaks, and on terms that overlap one another and themselves.
 */
import { describe, expect, it } from 'vitest';
import { countMarks, markRanges, MAX_MARKS_PER_BLOCK, type MarkRange } from './searchMarks';
import { fold, foldWithMap, sourceRange } from './searchFold';

function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** The ranges of `terms` in `text`, from the definition and with no cleverness. */
function reference(text: string, terms: readonly string[], max: number): MarkRange[] {
  const folded = foldWithMap(text);
  const found: MarkRange[] = [];
  for (const term of terms) {
    if (term === '') continue;
    for (let at = folded.text.indexOf(term); at >= 0; at = folded.text.indexOf(term, at + 1)) found.push(sourceRange(folded, at, at + term.length));
  }
  found.sort((a, b) => a.start - b.start || a.end - b.end);
  const merged: { start: number; end: number }[] = [];
  for (const range of found) {
    const last = merged[merged.length - 1];
    if (last !== undefined && range.start <= last.end) last.end = Math.max(last.end, range.end);
    else merged.push({ start: range.start, end: range.end });
  }
  return max <= 0 ? [] : merged.slice(0, max);
}

const PIECES = [
  'a', 'b', 'A', 'B', 'ab', 'aab', 'abab', ' ', ' ', '\n', '\r\n', '\t', 'retry', 'Retry', 'limit', 'needle',
  'é', 'e\u0301', 'ß', 'ss', 'İ', 'ı', 'ﬁ', 'fi', 'Σ', 'ς', 'σ', '😀', '𝒜', '日本',
  '\u200b', '\u200d', '\u00ad', '\ufeff', '\u202e', '\u00a0', '\u3000', '\u0000', '\u007f',
];

function randomText(rand: () => number, length: number): string {
  let out = '';
  for (let k = 0; k < length; k++) out += PIECES[Math.floor(rand() * PIECES.length)];
  return out;
}

/** Terms as `parseQuery` makes them: a typed word, folded. Some are cut from the text, so that they occur. */
function randomTerms(rand: () => number, text: string): string[] {
  const terms: string[] = [];
  const folded = fold(text);
  for (let n = 1 + Math.floor(rand() * 5); n > 0; n--) {
    if (folded.length > 0 && rand() < 0.7) {
      const start = Math.floor(rand() * folded.length);
      const length = 1 + Math.floor(rand() * 4);
      terms.push(folded.slice(start, start + length).trim());
    } else {
      terms.push(fold(PIECES[Math.floor(rand() * PIECES.length)] + PIECES[Math.floor(rand() * PIECES.length)]));
    }
  }
  return terms.filter((term) => term !== '');
}

describe('markRanges against the definition', () => {
  it('gives the ranges of the reference, on random text and random terms, for any limit', () => {
    const rand = rng(118);
    let checked = 0;
    let withRanges = 0;
    let merged = 0;
    for (let k = 0; k < 2_500; k++) {
      const text = randomText(rand, 1 + Math.floor(rand() * 40));
      const terms = randomTerms(rand, text);
      const max = [MAX_MARKS_PER_BLOCK, 1, 2, 3, 5, 1_000][Math.floor(rand() * 6)];
      const expected = reference(text, terms, max);
      const got = markRanges(text, terms, max);
      expect(got, `${JSON.stringify(text)} ${JSON.stringify(terms)} max ${max}`).toEqual(expected);
      expect(countMarks(text, terms, max), `count: ${JSON.stringify(text)} ${JSON.stringify(terms)}`).toBe(expected.length);
      checked++;
      if (expected.length > 0) withRanges++;
      if (terms.length > 1 && expected.length > 0) merged++;
    }
    expect(checked).toBe(2_500);
    expect(withRanges).toBeGreaterThan(1_200);
    expect(merged).toBeGreaterThan(600);
  });

  it('gives the same ranges for a text folded beforehand and for the text itself', () => {
    const rand = rng(7);
    for (let k = 0; k < 300; k++) {
      const text = randomText(rand, 1 + Math.floor(rand() * 30));
      const terms = randomTerms(rand, text);
      expect(markRanges(foldWithMap(text), terms)).toEqual(markRanges(text, terms));
    }
  });

  it('gives ranges that are in the text, in order, apart, and that never cut a character in two', () => {
    const rand = rng(99);
    for (let k = 0; k < 800; k++) {
      const text = randomText(rand, 1 + Math.floor(rand() * 40));
      const ranges = markRanges(text, randomTerms(rand, text), 1_000);
      let previousEnd = -1;
      for (const range of ranges) {
        expect(range.start).toBeGreaterThanOrEqual(0);
        expect(range.end).toBeGreaterThan(range.start);
        expect(range.end).toBeLessThanOrEqual(text.length);
        expect(range.start, 'ranges are apart, in order').toBeGreaterThan(previousEnd);
        previousEnd = range.end;
        // A range does not begin or end between the two halves of a surrogate pair.
        const first = text.charCodeAt(range.start);
        const last = text.charCodeAt(range.end - 1);
        expect(first >= 0xdc00 && first <= 0xdfff, `starts inside a pair in ${JSON.stringify(text)}`).toBe(false);
        expect(last >= 0xd800 && last <= 0xdbff, `ends inside a pair in ${JSON.stringify(text)}`).toBe(false);
      }
    }
  });
});

describe('markRanges on text that repeats itself, where terms overlap themselves and one another', () => {
  // The cases a scan that remembers part of a match gets wrong: a term that begins as it ends (aa, abab, abaaba), a text that is
  // nearly the term over and over, terms that are parts of one another. A small alphabet makes every one of them common.
  it('gives the ranges of the reference on 12,000 texts of two or three letters, with terms of up to twelve of them', { timeout: 30_000 }, () => {
    const rand = rng(5150);
    let withRanges = 0;
    let longRuns = 0;
    for (let k = 0; k < 12_000; k++) {
      const alphabet = k % 3 === 0 ? 'ab' : k % 3 === 1 ? 'abc' : 'aab';
      const word = (length: number) => Array.from({ length }, () => alphabet[Math.floor(rand() * alphabet.length)]).join('');
      // Sometimes the text is made of a term, so that there are many occurrences, overlapping.
      const base = word(1 + Math.floor(rand() * 12));
      const text = rand() < 0.5 ? base.repeat(1 + Math.floor(rand() * 20)).slice(Math.floor(rand() * 3)) + word(Math.floor(rand() * 8)) : word(1 + Math.floor(rand() * 120));
      const terms = Array.from({ length: 1 + Math.floor(rand() * 3) }, () => (rand() < 0.4 ? base.slice(0, 1 + Math.floor(rand() * base.length)) : word(1 + Math.floor(rand() * 12))));
      const max = [MAX_MARKS_PER_BLOCK, 1, 2, 3, 7][Math.floor(rand() * 5)];
      const expected = reference(text, terms, max);
      expect(markRanges(text, terms, max), `${JSON.stringify(text)} ${JSON.stringify(terms)} max ${max}`).toEqual(expected);
      expect(countMarks(text, terms, max)).toBe(expected.length);
      if (expected.length > 0) withRanges++;
      if (expected.some((range) => range.end - range.start >= 2 * Math.max(...terms.map((term) => term.length)))) longRuns++;
    }
    expect(withRanges).toBeGreaterThan(7_500);
    expect(longRuns).toBeGreaterThan(1_500); // runs of several overlapping matches, which are the point
  });

  it('gives the ranges of the reference when hidden characters, joiners and astral letters are between the letters of the repeats', { timeout: 30_000 }, () => {
    const rand = rng(8);
    const GAPS = ['', '', '', '\u200b', '\u00ad', '\u200d', '\u{1f600}', ' ', '\u00e9'];
    for (let k = 0; k < 3_000; k++) {
      const letters = Array.from({ length: 2 + Math.floor(rand() * 30) }, () => 'ab'[Math.floor(rand() * 2)]);
      const text = letters.map((letter) => letter + GAPS[Math.floor(rand() * GAPS.length)]).join('');
      const terms = Array.from({ length: 1 + Math.floor(rand() * 3) }, () => Array.from({ length: 1 + Math.floor(rand() * 5) }, () => 'ab'[Math.floor(rand() * 2)]).join(''));
      expect(markRanges(text, terms, 1_000), `${JSON.stringify(text)} ${JSON.stringify(terms)}`).toEqual(reference(text, terms, 1_000));
    }
  });
});

describe('markRanges on the cases that are easy to get wrong', () => {
  const marked = (text: string, terms: string[], max?: number) => markRanges(text, terms, max).map((range) => text.slice(range.start, range.end));

  it('makes matches that overlap one stretch, and matches that touch one stretch', () => {
    expect(marked('aaaaaa', ['aa'])).toEqual(['aaaaaa']);
    expect(marked('abab', ['ab'])).toEqual(['abab']);
    expect(marked('xababy', ['aba', 'bab'])).toEqual(['abab']);
    expect(marked('abXab', ['ab'])).toEqual(['ab', 'ab']);
    expect(marked('ababa', ['aba'])).toEqual(['ababa']);
  });

  it('finds a term inside another term, and the same text under two terms, once', () => {
    expect(marked('needle in a haystack', ['needle', 'eed', 'needle'])).toEqual(['needle']);
    expect(marked('needle in a haystack', ['hay', 'stack', 'haystack'])).toEqual(['haystack']);
  });

  it('keeps to the limit, counting stretches and not occurrences', () => {
    const text = 'x '.repeat(500);
    expect(markRanges(text, ['x'], 100)).toHaveLength(100);
    expect(markRanges(text, ['x'], 7)).toHaveLength(7);
    expect(markRanges(text, ['x'], 0)).toEqual([]);
    expect(markRanges(text, ['x'], -3)).toEqual([]);
    expect(markRanges('xxxx', ['x'], 1)).toEqual([{ start: 0, end: 4 }]);
    expect(countMarks(text, ['x'], 10)).toBe(10);
    expect(countMarks(text, ['x'])).toBe(500);
    expect(countMarks(text, ['x'], 1_000)).toBe(500);
  });

  it('marks whole characters where the fold changes how many there are', () => {
    expect(marked('Straße here', ['strasse'])).toEqual(['Straße']);
    expect(marked('the ﬁle', ['file'])).toEqual(['ﬁle']);
    expect(marked('visit İstanbul', [fold('İstanbul')])).toEqual(['İstanbul']);
    expect(marked('visit İstanbul', ['istanbul'])).toEqual([]); // typed without the dot: not the same word
    expect(marked('Ｒｅｔｒｙ', ['retry'])).toEqual(['Ｒｅｔｒｙ']);
    expect(marked('x 😀 y', ['😀'])).toEqual(['😀']);
    // A precomposed letter is one character, and the mark takes all of it. A letter with a separate combining accent is two, and a term
    // without the accent ends before it: the mark ends there too, on the letter (a cosmetic edge, written down so that a change to it is chosen).
    expect(marked('caf\u00e9 and cafe\u0301', ['cafe'])).toEqual(['caf\u00e9', 'cafe']);
    expect(marked('cafe\u0301', [fold('cafe\u0301')])).toEqual(['cafe\u0301']);
  });

  it('takes in the hidden characters inside a match and none at its edges', () => {
    expect(marked('pass\u200bword', ['password'])).toEqual(['pass\u200bword']);
    expect(marked('\u200bpassword\u200b', ['password'])).toEqual(['password']);
    expect(marked('pa\u200b\u200bss', ['pass'])).toEqual(['pa\u200b\u200bss']);
  });

  it('treats every kind of white space as one space, in the text and in a term', () => {
    expect(marked('retry\u00a0limit and retry\nlimit and retry\t\tlimit', [fold('retry limit')])).toEqual(['retry\u00a0limit', 'retry\nlimit']);
    expect(marked('a\r\nb', ['a b'])).toEqual(['a\r\nb']);
  });

  it('is total: text with lone surrogates, controls and nothing at all, and terms that are not in it', () => {
    for (const text of ['', '\ud800', '\udc00x', 'x\ud800', '\u0000\u0001', '\ud83d', 'a\ud83dz']) {
      expect(() => markRanges(text, ['x', 'a', '\ud83d'])).not.toThrow();
    }
    expect(markRanges('', ['a'])).toEqual([]);
    expect(markRanges('abc', [])).toEqual([]);
    expect(markRanges('abc', [''])).toEqual([]);
    expect(markRanges('abc', ['zzz'])).toEqual([]);
  });
});

describe('markRanges on text that is very long or made to be slow', () => {
  it('takes time in proportion to the text, not to the terms times the text', { timeout: 30_000 }, () => {
    const started = performance.now();
    const text = 'a'.repeat(1_000_000);
    markRanges(text, ['a'], 100);
    markRanges(text, ['aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab'], 100); // never found: a long partial match at every place
    markRanges(text, Array.from({ length: 16 }, (_, k) => 'a'.repeat(k + 1)), 100);
    markRanges(`${'ab'.repeat(500_000)}`, ['abababababababababababababababababababc', 'ba'], 100);
    countMarks(text, ['a'], 1_000);
    expect(performance.now() - started).toBeLessThan(8_000);
  });

  it('takes time in proportion to the text when a term of five hundred letters overlaps itself at every place of a million of them', { timeout: 30_000 }, () => {
    const started = performance.now();
    const text = 'a'.repeat(1_000_000);
    expect(markRanges(text, ['a'.repeat(500)], 100)).toEqual([{ start: 0, end: 1_000_000 }]);
    expect(markRanges(text, ['a'.repeat(500), 'a'.repeat(499), 'a'.repeat(250)], 100)).toEqual([{ start: 0, end: 1_000_000 }]);
    expect(countMarks(text, ['a'.repeat(500)])).toBe(1);
    const periodic = 'ab'.repeat(500_000);
    expect(markRanges(periodic, ['ab'.repeat(200)], 100)).toEqual([{ start: 0, end: 1_000_000 }]);
    expect(markRanges(periodic, ['ab'.repeat(200), 'ba'.repeat(200)], 100)).toEqual([{ start: 0, end: 1_000_000 }]);
    // A text that is nearly the term at every place and never is.
    expect(markRanges(periodic, ['ab'.repeat(300) + 'c'], 100)).toEqual([]);
    expect(performance.now() - started).toBeLessThan(8_000);
  });

  it('gives the same ranges for a million characters as for the first of them when the limit is reached early', () => {
    const text = `${'needle '.repeat(10)}${'filler '.repeat(150_000)}`;
    expect(markRanges(text, ['needle'], 5)).toEqual(markRanges(text.slice(0, 200), ['needle'], 5));
  });
});
