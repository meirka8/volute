/**
 * A table of the distinct strings of a set (issue #118): each gets a number the first time it is seen, and
 * the same string always gets the same number.
 *
 * It exists for strings that come from the wire (a conversation id, a tool name, a commit hash), which can
 * be a mebibyte long, and which whoever wrote the capture chose. A `Map` keyed by such strings is quadratic:
 * the engine hashes a string of more than 16,383 characters by its length alone, so a table of strings of one
 * length is one long chain, and every lookup compares the key with each string in it, character by character.
 *
 * So a string up to `SHORT_STRING` characters goes in a `Map` (the engine hashes those in full, with a seed of
 * its own that nobody can read), and a longer one is filed under a hash of ALL of it, and of its length, that
 * this module makes:
 *
 * - it reads every character once, so it takes time in proportion to the length and to nothing else;
 * - it is made of two lanes of 32 bits that are worked out together and kept as one number of 53 bits;
 * - it is seeded: the starting state, the odd factor of each lane and the constants of the last scramble
 *   are drawn once per page from `crypto.getRandomValues`, so that no one who writes a string can know which
 *   other strings it shares a key with (a hash that is only started from the seed, as a polynomial one is,
 *   gives two strings of one length the same key whatever the seed, and is easy to make collide; each step
 *   here is not polynomial, and the factors are the seed's too).
 *
 * The strings filed under one key are compared in full, so the table stays exact however the hash falls: two
 * different strings always get two numbers. A key that two strings do share costs a comparison; it can not be
 * made to happen on purpose, and by chance it happens about once in 10^16 pairs.
 */

/** A string this short is hashed in full by the engine, and goes in a `Map` as it is. */
export const SHORT_STRING = 256;

// ---------------------------------------------------------------------------
// The seed
// ---------------------------------------------------------------------------

/** What the hash of a long string is made from, drawn once per page. */
interface Seed {
  /** The starting state of each lane. */
  readonly a: number;
  readonly b: number;
  /** The odd factor of each lane. */
  readonly ma: number;
  readonly mb: number;
  /** What each lane is mixed with before its last scramble. */
  readonly ka: number;
  readonly kb: number;
}

/** `count` random 32-bit words: from the browser's own source of them, where there is one. */
function randomWords(count: number): Uint32Array {
  const words = new Uint32Array(count);
  try {
    const source = (globalThis as { crypto?: { getRandomValues?: (array: Uint32Array) => unknown } }).crypto;
    if (source !== undefined && typeof source.getRandomValues === 'function') {
      source.getRandomValues(words);
      return words;
    }
  } catch {
    // A source that refuses (a page that may not use one): the numbers below, which are not a secret but are not a constant either.
  }
  // No source of the kind (every browser the Reviewer runs in has one).
  for (let k = 0; k < count; k++) words[k] = Math.floor(Math.random() * 0x100000000) ^ (Date.now() + k);
  return words;
}

/** How many bits of a 32-bit number are set. */
function bitsSet(value: number): number {
  let v = value - ((value >>> 1) & 0x55555555);
  v = (v & 0x33333333) + ((v >>> 2) & 0x33333333);
  return Math.imul((v + (v >>> 4)) & 0x0f0f0f0f, 0x01010101) >>> 24;
}

function drawSeed(): Seed {
  const words = randomWords(12);
  // A factor with few or many bits set mixes badly: take the first odd candidate that has between 12 and 20.
  const factor = (from: number, fallback: number): number => {
    for (let k = from; k < from + 4; k++) {
      const candidate = words[k] | 1;
      const ones = bitsSet(candidate);
      if (ones >= 12 && ones <= 20) return candidate;
    }
    return fallback;
  };
  return {
    a: words[0] | 0,
    b: words[1] | 0,
    ka: words[2] | 0,
    kb: words[3] | 0,
    ma: factor(4, 0x9e3779b1 | 0),
    mb: factor(8, 0x85ebca6b | 0),
  };
}

/** The seed of this page: drawn the first time a long string needs it, and never again. */
let seed: Seed | null = null;

// ---------------------------------------------------------------------------
// The hash of a long string
// ---------------------------------------------------------------------------

/**
 * The key `text` is filed under when it is longer than `SHORT_STRING`: a seeded hash of every character of it and of
 * its length, as a whole number of 53 bits (32 from one lane, 21 from the other). The same text has the same key for as
 * long as the page lives, and a page that is opened again has another seed. Exported for the tests.
 */
export function longKey(text: string): number {
  const s = seed ?? (seed = drawSeed());
  const length = text.length;
  const ma = s.ma;
  const mb = s.mb;
  let a = s.a;
  let b = s.b;
  let at = 0;
  // Two characters at a time, as one word: half the steps, and every character is in a word.
  for (; at + 1 < length; at += 2) {
    const word = text.charCodeAt(at) | (text.charCodeAt(at + 1) << 16);
    a = Math.imul(a ^ word, ma);
    a ^= a >>> 15;
    b = Math.imul(b + word, mb);
    b = (b << 13) | (b >>> 19);
  }
  if (at < length) {
    const word = text.charCodeAt(at);
    a = Math.imul(a ^ word, ma);
    a ^= a >>> 15;
    b = Math.imul(b + word, mb);
    b = (b << 13) | (b >>> 19);
  }
  // The last scramble: the length and the seed's constants, then every bit of the lane into every other.
  let x = a ^ s.ka ^ length;
  x ^= x >>> 16;
  x = Math.imul(x, 0x85ebca6b);
  x ^= x >>> 13;
  x = Math.imul(x, 0xc2b2ae35);
  x ^= x >>> 16;
  let y = b ^ s.kb ^ Math.imul(length, 0x9e3779b1);
  y ^= y >>> 15;
  y = Math.imul(y, 0x2c1b3c6d);
  y ^= y >>> 12;
  y = Math.imul(y, 0x297a2d39);
  y ^= y >>> 15;
  return (x >>> 0) * 0x200000 + (y >>> 11);
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

export class StringTable {
  /** The distinct strings, in the order they were first seen: the number of a string is its place here. */
  readonly values: string[] = [];
  private readonly short = new Map<string, number>();
  /** The numbers of the long strings, by the key of each (see `longKey`). */
  private readonly long = new Map<number, number[]>();
  private readonly keyOf: (text: string) => number;

  /**
   * `keyOf` is what files a long string; it is `longKey` for everything but the tests, which give the table a key that
   * every string shares to see that it stays exact when the keys fall together.
   */
  constructor(keyOf: (text: string) => number = longKey) {
    this.keyOf = keyOf;
  }

  /** The number of `text`, or -1 when it is not in the table. */
  indexOf(text: string): number {
    if (text.length <= SHORT_STRING) return this.short.get(text) ?? -1;
    const filed = this.long.get(this.keyOf(text));
    if (filed !== undefined) for (const number of filed) if (this.values[number] === text) return number;
    return -1;
  }

  /** The number of `text`; it is added if it is new. */
  add(text: string): number {
    if (text.length <= SHORT_STRING) {
      const known = this.short.get(text);
      if (known !== undefined) return known;
      const number = this.values.length;
      this.values.push(text);
      this.short.set(text, number);
      return number;
    }
    const key = this.keyOf(text);
    let filed = this.long.get(key);
    if (filed === undefined) {
      filed = [];
      this.long.set(key, filed);
    } else {
      for (const number of filed) if (this.values[number] === text) return number;
    }
    const number = this.values.length;
    this.values.push(text);
    filed.push(number);
    return number;
  }

  get size(): number {
    return this.values.length;
  }
}
