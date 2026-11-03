/**
 * Where a search matches in the text that is drawn (issue #118): the stretches to mark, and
 * the count a fold reports.
 */
import { describe, expect, it } from 'vitest';
import { fold } from './searchFold';
import { countMarks, foldForMarks, markRanges, MAX_MARKS_PER_BLOCK } from './searchMarks';

const marked = (text: string, terms: string[], max?: number) => markRanges(text, terms, max).map(({ start, end }) => text.slice(start, end));

describe('terms and text are folded the same way', () => {
  it('finds a term typed as the text is drawn, and marks the text as it is drawn', () => {
    // The term is folded as the index folds it (parseQuery does); the text on the page is folded with its map.
    expect(marked('Fetch the FETCH', [fold('fetch')])).toEqual(['Fetch', 'FETCH']);
    expect(marked('Visit İstanbul in spring', [fold('İstanbul')])).toEqual(['İstanbul']);
    expect(marked('Die Straße ist lang', [fold('STRASSE')])).toEqual(['Straße']);
    expect(marked('ΟΔΥΣΣΕΥΣ and οδυσσευς', [fold('οδυσσευσ')])).toEqual(['ΟΔΥΣΣΕΥΣ', 'οδυσσευς']);
    expect(marked('see ＡＢＣ and ﬁnd', [fold('abc'), fold('find')])).toEqual(['ＡＢＣ', 'ﬁnd']);
  });

  it('marks a word with a joiner or a non-joiner whole, though the term has none', () => {
    const persian = 'می\u200Cخواهم';
    expect(marked(`say ${persian} now`, [fold(persian)])).toEqual([persian]);
    const family = '👨\u200D👩\u200D👧';
    expect(marked(`see ${family}!`, [fold(family)])).toEqual([family]);
  });

  it('marks the hidden characters that are inside a match, and none at its edges', () => {
    expect(marked('x pass\u200Bword y', ['password'])).toEqual(['pass\u200Bword']);
    expect(marked('\u200Bab\u200B', ['ab'])).toEqual(['ab']);
  });

  it('marks a precomposed letter whole when the term is the letter without its accent, and finds either form', () => {
    expect(marked('un café noir', [fold('cafe')])).toEqual(['café']);
    expect(marked('un cafe\u0301 noir', [fold('café')])).toEqual(['cafe\u0301']);
  });

  it('marks a character whole when the term ends inside the folded form of it', () => {
    expect(marked('Straße', [fold('stras')])).toEqual(['Straß']);
    expect(marked('Straße', [fold('sse')])).toEqual(['ße']);
    expect(marked('x ﬃ y', [fold('ff')])).toEqual(['ﬃ']);
  });

  it('can be handed the fold of a text, which it does not fold again', () => {
    const block = foldForMarks('Hello World');
    expect(block).toEqual({ text: 'hello world', starts: null, ends: null });
    expect(markRanges(block, ['world'])).toEqual([{ start: 6, end: 11 }]);
    expect(countMarks(block, ['o'])).toBe(2);
  });

  it('reads \\ and / as one separator on a surface that shows a path', () => {
    const block = foldForMarks('C:\\Users\\dev\\src\\app.ts', true);
    expect(block.text).toBe('c:/users/dev/src/app.ts');
    expect(markRanges(block, ['src/app.ts'])).toEqual([{ start: 13, end: 23 }]);
    expect(markRanges(foldForMarks('C:\\Users\\dev\\src\\app.ts'), ['src/app.ts'])).toEqual([]);
  });
});

describe('the stretches to mark', () => {
  it('finds every occurrence of a term, whatever the case, as the text wrote it', () => {
    expect(marked('Fetch the fetch, FETCH!', ['fetch'])).toEqual(['Fetch', 'fetch', 'FETCH']);
    expect(markRanges('ab ab', ['ab'])).toEqual([{ start: 0, end: 2 }, { start: 3, end: 5 }]);
  });

  it('finds nothing for no term, an empty term, or text with no match', () => {
    expect(markRanges('text', [])).toEqual([]);
    expect(markRanges('text', [''])).toEqual([]);
    expect(markRanges('', ['a'])).toEqual([]);
    expect(markRanges('text', ['zzz'])).toEqual([]);
  });

  it('marks the match of any term, in order', () => {
    expect(marked('alpha beta gamma', ['gamma', 'alpha'])).toEqual(['alpha', 'gamma']);
  });

  it('makes one stretch of matches that overlap or touch', () => {
    expect(marked('foobar', ['foo', 'bar'])).toEqual(['foobar']);
    expect(marked('foobar', ['foob', 'obar'])).toEqual(['foobar']);
    expect(marked('aaaa', ['aa'])).toEqual(['aaaa']);
    expect(marked('aaa', ['aa'])).toEqual(['aaa']);
    expect(marked('abcabc', ['abc', 'bca'])).toEqual(['abcabc']);
    // Not touching: two stretches.
    expect(marked('foo bar', ['foo', 'bar'])).toEqual(['foo', 'bar']);
  });

  it('finds a phrase with white space and a term with punctuation as plain text', () => {
    expect(marked('the retry limit is (a+)+$ here', ['retry limit', '(a+)+$'])).toEqual(['retry limit', '(a+)+$']);
    expect(marked('a.b axb', ['a.b'])).toEqual(['a.b']);
  });

  it('gives positions in the text as it was written, even where lower case would lengthen it', () => {
    const text = 'İ hit İ hit';
    expect(marked(text, ['hit'])).toEqual(['hit', 'hit']);
    expect(markRanges(text, ['hit'])[0]).toEqual({ start: 2, end: 5 });
  });

  it('stops at the most marks it was asked for, and counts a stretch that is still growing only once it is whole', () => {
    expect(markRanges('a b a b a b', ['a'], 2)).toHaveLength(2);
    expect(markRanges('a b a b a b', ['a'], 0)).toEqual([]);
    expect(markRanges('a'.repeat(1000), ['a'], 1)).toEqual([{ start: 0, end: 1000 }]);
    const lots = markRanges('x '.repeat(5000), ['x']);
    expect(lots).toHaveLength(MAX_MARKS_PER_BLOCK);
    expect(lots[0]).toEqual({ start: 0, end: 1 });
  });

  it('does a megabyte of one letter, one letter asked for, without taking a second', () => {
    const text = 'a'.repeat(1024 * 1024);
    const started = performance.now();
    expect(markRanges(text, ['a'])).toEqual([{ start: 0, end: text.length }]);
    expect(performance.now() - started).toBeLessThan(1500);
  });

  it('does a megabyte of nothing like the term quickly', () => {
    const text = 'abcdefghij'.repeat(100_000);
    const started = performance.now();
    expect(markRanges(text, ['zzz', 'qqq', 'abcdefghijk x'])).toEqual([]);
    expect(performance.now() - started).toBeLessThan(500);
  });
});

describe('the count a fold reports', () => {
  it('counts the stretches, not the terms', () => {
    expect(countMarks('fetch fetch retry', ['fetch'])).toBe(2);
    expect(countMarks('fetch fetch retry', ['fetch', 'retry'])).toBe(3);
    expect(countMarks('foobar', ['foo', 'bar'])).toBe(1);
    expect(countMarks('nothing', ['zzz'])).toBe(0);
  });

  it('counts past the marks that are drawn, up to a cap', () => {
    const text = 'x '.repeat(500);
    expect(countMarks(text, ['x'])).toBe(500);
    expect(markRanges(text, ['x'])).toHaveLength(MAX_MARKS_PER_BLOCK);
    expect(countMarks(text, ['x'], 40)).toBe(40);
  });
});
