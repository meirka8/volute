/**
 * Round 3 QA of issue #118: the memory of the fold (src/lib/searchFold.ts). What a code point folds to is remembered; for the
 * code points above U+FFFF there are a million, and a text can use as many as it has room for, so the memory is cut off at
 * `ASTRAL_CACHE_LIMIT`. Checked by what can be seen of it: the number remembered never passes the limit, a code point is
 * remembered while there is room, and what a code point folds to is the same whether it was remembered, forgotten, or never met.
 */
import { describe, expect, it } from 'vitest';
import { ASTRAL_CACHE_LIMIT, astralCacheSize, fold, foldCodePoint, foldWithMap } from './searchFold';

/** `count` different code points above U+FFFF, starting at `from`, none of them a surrogate. */
function astral(from: number, count: number): number[] {
  return Array.from({ length: count }, (_, k) => from + k);
}

describe('what the fold remembers of the characters above U+FFFF', () => {
  // (These come first: they read the size of the memory, which the test after them fills.)
  it('remembers a code point it has met, while there is room: the size grows by one for each new one and not for one met again', () => {
    const before = astralCacheSize();
    expect(before + 40).toBeLessThanOrEqual(ASTRAL_CACHE_LIMIT);
    const codePoints = astral(0x1d400, 40); // mathematical bold letters
    for (const codePoint of codePoints) foldCodePoint(codePoint);
    expect(astralCacheSize()).toBe(before + 40);
    for (const codePoint of codePoints) foldCodePoint(codePoint);
    expect(astralCacheSize()).toBe(before + 40);
    // A text of them is folded without asking again.
    fold(String.fromCodePoint(...codePoints));
    expect(astralCacheSize()).toBe(before + 40);
  });

  it('is never more than the limit, however many different code points are met', () => {
    expect(ASTRAL_CACHE_LIMIT).toBeGreaterThanOrEqual(1_000); // room for the emoji, the mathematical letters and the rare Chinese characters
    expect(ASTRAL_CACHE_LIMIT).toBeLessThanOrEqual(100_000); // and not for a million
    let most = 0;
    for (let codePoint = 0x20000; codePoint < 0x20000 + ASTRAL_CACHE_LIMIT * 3 + 17; codePoint++) {
      foldCodePoint(codePoint);
      most = Math.max(most, astralCacheSize());
      expect(astralCacheSize()).toBeLessThanOrEqual(ASTRAL_CACHE_LIMIT);
    }
    // It was filled (the limit is where it stops), and it still holds what it has just been asked.
    expect(most).toBeGreaterThan(ASTRAL_CACHE_LIMIT / 2);
    expect(astralCacheSize()).toBeGreaterThan(0);
  });

  it('is never more than the limit when the code points come from a text, the way they do', () => {
    const text = String.fromCodePoint(...astral(0x30000, ASTRAL_CACHE_LIMIT * 2 + 5));
    fold(text);
    expect(astralCacheSize()).toBeLessThanOrEqual(ASTRAL_CACHE_LIMIT);
    foldWithMap(text);
    expect(astralCacheSize()).toBeLessThanOrEqual(ASTRAL_CACHE_LIMIT);
  });
});

describe('what the fold gives for the characters above U+FFFF', () => {
  it('is the same whether the code point was remembered, forgotten or never met', () => {
    // The mathematical letters, the emoji, a few rare Chinese characters, an ancient script, a private use plane.
    const chosen = [...astral(0x1d400, 60), ...astral(0x1f600, 40), 0x20000, 0x2a6d6, 0x10000, 0x10400, 0x10ffff, 0x1d7ce, 0x1f1e6, 0xe0001, 0xe0100];
    const asked = chosen.map((codePoint) => foldCodePoint(codePoint));
    // Flood the memory with other code points, so that the ones above are forgotten...
    for (let codePoint = 0x40000; codePoint < 0x40000 + ASTRAL_CACHE_LIMIT * 2 + 3; codePoint++) foldCodePoint(codePoint);
    // ... then ask again: the same answers, one at a time and as a text.
    expect(chosen.map((codePoint) => foldCodePoint(codePoint))).toEqual(asked);
    const text = String.fromCodePoint(...chosen);
    expect(fold(text)).toBe(asked.join(''));
    expect(foldWithMap(text).text).toBe(asked.join(''));
    // The answers themselves, for the ones where there is something to know: a bold letter is a letter, a tag character is nothing.
    expect(foldCodePoint(0x1d400)).toBe('a'); // mathematical bold capital A
    expect(foldCodePoint(0x1d41a)).toBe('a'); // mathematical bold small a
    expect(foldCodePoint(0x1d7ce)).toBe('0'); // mathematical bold digit zero
    expect(foldCodePoint(0x1f600)).toBe(String.fromCodePoint(0x1f600)); // an emoji stays what it is
    expect(foldCodePoint(0xe0001)).toBe(''); // a tag character draws nothing
  });

  it('is the same for a text whose code points push each other out of the memory as it is folded, and its map says the same', () => {
    const codePoints = astral(0x50000, ASTRAL_CACHE_LIMIT * 2 + 11);
    const text = `ab${String.fromCodePoint(...codePoints)}cd${String.fromCodePoint(...codePoints.slice(0, 50))}`;
    const first = fold(text);
    for (let codePoint = 0x60000; codePoint < 0x60000 + ASTRAL_CACHE_LIMIT + 5; codePoint++) foldCodePoint(codePoint);
    expect(fold(text)).toBe(first);
    const mapped = foldWithMap(text);
    expect(mapped.text).toBe(first);
    // Every unit of the folded text is from a place in the text, in order.
    if (mapped.starts !== null && mapped.ends !== null) {
      expect(mapped.starts.length).toBe(first.length);
      for (let k = 1; k < mapped.starts.length; k++) expect(mapped.starts[k]).toBeGreaterThanOrEqual(mapped.starts[k - 1]);
      expect(mapped.ends[mapped.ends.length - 1]).toBeLessThanOrEqual(text.length);
    }
  });
});
