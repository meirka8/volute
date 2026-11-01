/**
 * The memory the fold keeps (issue #118, revision round 4): what each code point folds to is remembered, and for the ones
 * above U+FFFF, of which a text can use a million different ones, the most remembered at a time is `ASTRAL_CACHE_LIMIT`. (The
 * cache of the code points below U+10000 is an array of 65,536 places, and takes no more than that.)
 */
import { describe, expect, it } from 'vitest';
import { ASTRAL_CACHE_LIMIT, astralCacheSize, fold, foldCodePoint, foldWithMap } from './searchFold';

/** The text of the code points from `first` up to, not including, `end`, one after another. */
function codePoints(first: number, end: number): string {
  let text = '';
  for (let codePoint = first; codePoint < end; codePoint++) text += String.fromCodePoint(codePoint);
  return text;
}

/** Folded as the engine folds, one code point at a time (what `fold` is for a code point that is not special in another way). */
const SAMPLES: Array<[string, string]> = [
  ['\u{1D400}', 'a'], // mathematical bold capital A
  ['\u{1D5D4}', 'a'], // mathematical sans-serif bold capital A
  ['\u{10400}', '\u{10428}'], // Deseret capital long I, to its small letter
  ['\u{1E900}', '\u{1E922}'], // Adlam capital alif
  ['\u{1F600}', '\u{1F600}'], // an emoji, as it is
  ['\u{2F800}', '丽'], // a CJK compatibility ideograph, to the ideograph it stands for
  ['\u{E0001}', ''], // a tag character: nothing is drawn
  ['\u{1D173}', ''], // a musical symbol that draws nothing
];

describe('the cache of the code points above U+FFFF', () => {
  it('is never larger than its limit, however many different ones a text uses, in `fold`', () => {
    const total = 3 * ASTRAL_CACHE_LIMIT + 1_234;
    let largest = 0;
    for (let first = 0x20000; first < 0x20000 + total; first += 97) {
      fold(codePoints(first, Math.min(first + 97, 0x20000 + total)));
      largest = Math.max(largest, astralCacheSize());
      expect(astralCacheSize()).toBeLessThanOrEqual(ASTRAL_CACHE_LIMIT);
    }
    // It was used (a cache that remembers nothing has nothing to bound), and it did fill up and empty out.
    expect(largest).toBeGreaterThan(ASTRAL_CACHE_LIMIT / 2);
  });

  it('is never larger than its limit in `foldWithMap`, which the marks use', () => {
    const total = 2 * ASTRAL_CACHE_LIMIT + 77;
    for (let first = 0x30000; first < 0x30000 + total; first += 211) {
      foldWithMap(codePoints(first, Math.min(first + 211, 0x30000 + total)));
      expect(astralCacheSize()).toBeLessThanOrEqual(ASTRAL_CACHE_LIMIT);
    }
  });

  it('is never larger than its limit in `foldCodePoint`, one code point at a time, at any moment', () => {
    for (let codePoint = 0x10000; codePoint < 0x10000 + 3 * ASTRAL_CACHE_LIMIT; codePoint++) {
      foldCodePoint(codePoint);
      if (astralCacheSize() > ASTRAL_CACHE_LIMIT) throw new Error(`the cache held ${astralCacheSize()} after U+${codePoint.toString(16)}`);
    }
    expect(astralCacheSize()).toBeGreaterThan(0);
  });

  it('folds each code point to what it folded to before, whether it is remembered or has been forgotten', () => {
    const before = SAMPLES.map(([text]) => fold(text));
    expect(before).toEqual(SAMPLES.map(([, folded]) => folded));
    // Many more different code points than the cache holds: what was remembered is gone (the cache emptied at least twice).
    fold(codePoints(0x40000, 0x40000 + 2 * ASTRAL_CACHE_LIMIT + 10));
    expect(astralCacheSize()).toBeLessThanOrEqual(ASTRAL_CACHE_LIMIT);
    expect(SAMPLES.map(([text]) => fold(text))).toEqual(before);
    expect(SAMPLES.map(([text]) => foldWithMap(text).text)).toEqual(before);
    // And what the BMP code points fold to is not touched by any of it.
    expect(fold('ßİςﬁ')).toBe('ssi̇σfi');
  });

  it('is not filled by text with no code point above U+FFFF in it', () => {
    const size = astralCacheSize();
    fold('plain text, with accents éè and a ligature ﬁ and ß');
    foldWithMap('more of the same ÅÖ');
    expect(astralCacheSize()).toBe(size);
  });
});
