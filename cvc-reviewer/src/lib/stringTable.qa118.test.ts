/**
 * Round 3 QA of issue #118: the table of the distinct strings of a set (src/lib/stringTable.ts), by behaviour only. Ids and
 * names come from the wire and can be a mebibyte long, and a person who can write to the capture chooses them: so the table
 * is checked for exactness (two different strings are two numbers, whatever they have in common and wherever they differ)
 * and for time (a set of long strings that agree everywhere but in one place takes time in proportion to what it holds).
 * Nothing in the first two groups depends on how a long string is told from another; the third group is about the key a long
 * string is filed under (what its documentation promises of it: every character is in it, and the seed is different on every page).
 */
import { describe, expect, it, vi } from 'vitest';
import { longKey, SHORT_STRING, StringTable } from './stringTable';

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

/** What a table must be: the numbers a plain `Map` hands out in the order strings are first seen. */
function reference(strings: readonly string[]) {
  const numbers = new Map<string, number>();
  const values: string[] = [];
  const answers = strings.map((text) => {
    let number = numbers.get(text);
    if (number === undefined) {
      number = values.length;
      values.push(text);
      numbers.set(text, number);
    }
    return number;
  });
  return { answers, values, numbers };
}

describe('the numbers the table gives', () => {
  it('are the numbers of a Map in the order of first sight, for strings of every length, repeated, in any order', () => {
    const rand = rng(31);
    const lengths = [0, 1, 2, 17, 255, 256, 257, 258, 1_000, 16_383, 16_384, 16_385, 20_000, 70_000];
    for (let round = 0; round < 12; round++) {
      // A small pool, so that strings come round again; some of them are copies of another with one character changed.
      const pool: string[] = [];
      for (let k = 0; k < 24; k++) {
        const length = lengths[Math.floor(rand() * lengths.length)];
        if (pool.length > 0 && rand() < 0.5) {
          const base = pool[Math.floor(rand() * pool.length)];
          if (base.length === 0) {
            pool.push('x');
          } else {
            const at = Math.floor(rand() * base.length);
            pool.push(base.slice(0, at) + (base[at] === 'a' ? 'b' : 'a') + base.slice(at + 1));
          }
        } else {
          pool.push('a'.repeat(length));
        }
      }
      const input = Array.from({ length: 200 }, () => pool[Math.floor(rand() * pool.length)]);
      const table = new StringTable();
      const expected = reference(input);
      expect(input.map((text) => table.add(text))).toEqual(expected.answers);
      expect(table.values).toEqual(expected.values);
      expect(table.size).toBe(expected.values.length);
      // A string already in the table is found; one that is not, is not.
      for (const text of input) expect(table.indexOf(text)).toBe(expected.numbers.get(text));
      expect(table.indexOf('not in the table at all')).toBe(-1);
      expect(table.indexOf('a'.repeat(30_000) + 'b')).toBe(-1);
    }
  });

  it('tell strings apart wherever they differ: first character, last, middle, near a place a sample might not reach', () => {
    for (const length of [300, 1_000, 16_384, 40_000]) {
      const base = 'k'.repeat(length);
      const table = new StringTable();
      const numbers = new Set<number>([table.add(base)]);
      // One different character at each of a spread of places, and a few places picked one by one.
      const places = new Set<number>([0, 1, 2, length - 3, length - 2, length - 1, Math.floor(length / 2), Math.floor(length / 3), Math.floor((2 * length) / 3)]);
      for (let place = 0; place < length; place += Math.max(1, Math.floor(length / 211))) places.add(place);
      for (const place of places) numbers.add(table.add(base.slice(0, place) + 'z' + base.slice(place + 1)));
      expect(numbers.size).toBe(places.size + 1);
      expect(table.size).toBe(places.size + 1);
      for (const place of places) expect(table.indexOf(base.slice(0, place) + 'z' + base.slice(place + 1))).toBeGreaterThan(0);
      expect(table.indexOf(base)).toBe(0);
    }
  });

  it('tell strings apart that are one character longer or shorter, and ones that differ in two places', () => {
    const table = new StringTable();
    const base = 'm'.repeat(5_000);
    const a = table.add(base);
    const b = table.add(base + 'm');
    const c = table.add(base.slice(1));
    const d = table.add(`z${base.slice(1, -1)}z`);
    const e = table.add(`${base.slice(0, 2_000)}yy${base.slice(2_002)}`);
    expect(new Set([a, b, c, d, e]).size).toBe(5);
  });

  it('are those of strings made in different ways: a slice, a concatenation, a repeat, a string that came from a split', () => {
    const table = new StringTable();
    const made = ['q'.repeat(50_000), 'q'.repeat(25_000) + 'q'.repeat(25_000), ['q'.repeat(10_000), 'q'.repeat(40_000)].join(''), 'xq'.repeat(25_000).slice(0).replaceAll('x', 'q'), `${'q'.repeat(49_999)}q`];
    const numbers = made.map((text) => table.add(text));
    expect(new Set(numbers).size).toBe(1);
    expect(table.size).toBe(1);
  });

  it('are those of strings with characters outside the BMP and of lone surrogates, short and long', () => {
    const table = new StringTable();
    const strings = ['😀', '\ud83d', '\ude00', '😀'.repeat(300), '😀'.repeat(299) + '\ud83d', '😀'.repeat(299) + '\ude00', '😀'.repeat(300).slice(1)];
    const expected = reference(strings);
    expect(strings.map((text) => table.add(text))).toEqual(expected.answers);
  });
});

describe('the time the table takes', () => {
  it('is in proportion to the strings when thousands of long ones differ in one place, wherever it is', { timeout: 30_000 }, () => {
    const started = performance.now();
    const length = 20_000;
    const table = new StringTable();
    const base = 'i'.repeat(length);
    const count = 3_000;
    for (let k = 0; k < count; k++) {
      // Each differs from the others in one place, and the places are spread over the string: the middle most of all.
      const place = k % 3 === 0 ? Math.floor(length / 2) : (k * 7_919) % length;
      const text = `${base.slice(0, place)}${k.toString(36)}${base.slice(place + 1)}`;
      expect(table.add(text)).toBe(k);
    }
    expect(table.size).toBe(count);
    // Looking them up again is as quick.
    for (let k = 0; k < count; k += 7) {
      const place = k % 3 === 0 ? Math.floor(length / 2) : (k * 7_919) % length;
      expect(table.indexOf(`${base.slice(0, place)}${k.toString(36)}${base.slice(place + 1)}`)).toBe(k);
    }
    expect(performance.now() - started).toBeLessThan(5_000);
  });

  it('is in proportion to the strings when they are all the same length and share a long start and a long end', { timeout: 30_000 }, () => {
    const started = performance.now();
    const table = new StringTable();
    const head = 'h'.repeat(8_000);
    const tail = 't'.repeat(8_000);
    for (let k = 0; k < 4_000; k++) table.add(`${head}${k.toString().padStart(8, '0')}${tail}`);
    expect(table.size).toBe(4_000);
    expect(performance.now() - started).toBeLessThan(5_000);
  });

  it('takes a million short strings in a moment', { timeout: 30_000 }, () => {
    const started = performance.now();
    const table = new StringTable();
    for (let k = 0; k < 300_000; k++) table.add(`conversation-${k % 5_000}`);
    expect(table.size).toBe(5_000);
    expect(performance.now() - started).toBeLessThan(3_000);
  });
});

describe('the table with a key that every string shares', () => {
  // The table is told how to file a long string; with a key that is the same for all of them it is one chain, and it must be exact.
  it('stays exact when the keys fall together: the numbers of a Map, for short and long strings, in any order', () => {
    const rand = rng(77);
    for (const keyOf of [() => 0, (text: string) => text.length % 3, (text: string) => text.charCodeAt(0)]) {
      const pool = Array.from({ length: 30 }, (_, k) => {
        const length = [SHORT_STRING - 1, SHORT_STRING, SHORT_STRING + 1, 300, 301, 1_000][k % 6];
        const changed = k % 10 === 0 ? 'a'.repeat(length - 1) + 'b' : 'a'.repeat(Math.floor(length / 2)) + String.fromCharCode(98 + (k % 24)) + 'a'.repeat(length - Math.floor(length / 2) - 1);
        return changed;
      });
      const input = Array.from({ length: 300 }, () => pool[Math.floor(rand() * pool.length)]);
      const table = new StringTable(keyOf);
      const expected = reference(input);
      expect(input.map((text) => table.add(text))).toEqual(expected.answers);
      expect(table.values).toEqual(expected.values);
      for (const text of input) expect(table.indexOf(text)).toBe(expected.numbers.get(text));
      expect(table.indexOf('a'.repeat(SHORT_STRING + 5) + 'z')).toBe(-1);
      expect(table.indexOf('')).toBe(-1);
    }
  });

  it('counts a string of SHORT_STRING characters and one of one more as strings like any other', () => {
    const table = new StringTable(() => 1);
    const at = 'q'.repeat(SHORT_STRING);
    const over = 'q'.repeat(SHORT_STRING + 1);
    expect([table.add(at), table.add(over), table.add(at), table.add(over), table.add(over + 'q')]).toEqual([0, 1, 0, 1, 2]);
    expect([table.indexOf(at), table.indexOf(over), table.indexOf(over + 'q'), table.indexOf(over + 'r')]).toEqual([0, 1, 2, -1]);
  });
});

describe('the key a long string is filed under', () => {
  const long = (length: number, seed = 5) => {
    const rand = rng(seed);
    return Array.from({ length }, () => String.fromCharCode(97 + Math.floor(rand() * 26))).join('');
  };
  const bitsDifferent = (a: number, b: number) => {
    let count = 0;
    for (const [x, y] of [[Math.floor(a / 0x100000000), Math.floor(b / 0x100000000)], [a >>> 0, b >>> 0]]) {
      let v = (x ^ y) >>> 0;
      while (v !== 0) {
        count += v & 1;
        v >>>= 1;
      }
    }
    return count;
  };

  it('is a whole number of at most 53 bits, the same every time and for a string made in any way', () => {
    for (const length of [SHORT_STRING + 1, 300, 1_000, 40_000]) {
      const text = long(length);
      const key = longKey(text);
      expect(Number.isSafeInteger(key), `${length}`).toBe(true);
      expect(key).toBeGreaterThanOrEqual(0);
      expect(key).toBeLessThan(2 ** 53);
      expect(longKey(text)).toBe(key);
      expect(longKey(text.slice(0, length / 2) + text.slice(length / 2))).toBe(key);
      expect(longKey(Array.from(text).join(''))).toBe(key);
      expect(longKey(`x${text}`.slice(1))).toBe(key);
    }
  });

  it('has every character of the string in it: changing any one of them, at any place, changes the key', () => {
    for (const [length, every] of [[SHORT_STRING + 1, 1], [SHORT_STRING + 2, 1], [1_001, 1], [1_002, 1], [4_096, 7], [20_001, 97]] as const) {
      const text = long(length, length);
      const key = longKey(text);
      const places = new Set<number>([0, 1, 2, 3, length - 4, length - 3, length - 2, length - 1]);
      for (let place = 0; place < length; place += every) places.add(place);
      for (const place of places) {
        const other = text[place] === 'z' ? 'y' : 'z';
        expect(longKey(text.slice(0, place) + other + text.slice(place + 1)), `${length}: place ${place}`).not.toBe(key);
      }
    }
  });

  it('has the order of the characters in it, and the length: swapped neighbours, halves and a repeated character are other keys', () => {
    const text = long(1_000, 9);
    const key = longKey(text);
    for (const place of [0, 1, 2, 499, 500, 997, 998]) {
      if (text[place] === text[place + 1]) continue;
      expect(longKey(text.slice(0, place) + text[place + 1] + text[place] + text.slice(place + 2)), `swapped at ${place}`).not.toBe(key);
    }
    expect(longKey(text.slice(500) + text.slice(0, 500))).not.toBe(key);
    expect(longKey(text.slice(0, 999))).not.toBe(key);
    expect(longKey(text.slice(1))).not.toBe(key);
    expect(longKey(`${text}${text[999]}`)).not.toBe(key);
    // The same character, so many times: each length is its own key.
    const keys = new Set<number>();
    for (let length = SHORT_STRING + 1; length < SHORT_STRING + 400; length++) keys.add(longKey('a'.repeat(length)));
    expect(keys.size).toBe(399);
    keys.clear();
    for (let length = SHORT_STRING + 1; length < SHORT_STRING + 400; length++) keys.add(longKey('\u0000'.repeat(length)));
    expect(keys.size).toBe(399);
  });

  it('is told apart on the two halves of every character: the high byte and the low byte change the key alike', () => {
    const text = 'k'.repeat(400);
    const key = longKey(text);
    for (const place of [0, 1, 200, 201, 398, 399]) {
      for (const code of [0x6b ^ 0x01, 0x6b ^ 0x80, 0x6b ^ 0x100, 0x6b ^ 0x8000, 0xd83d, 0xde00, 0xffff, 0]) {
        expect(longKey(text.slice(0, place) + String.fromCharCode(code) + text.slice(place + 1)), `${place}: ${code}`).not.toBe(key);
      }
    }
  });

  it('is spread over its bits: a change of one character changes about half of them, wherever it is', () => {
    const rand = rng(2024);
    for (const length of [SHORT_STRING + 1, 300, 1_001, 5_000]) {
      const text = long(length, length + 1);
      const key = longKey(text);
      let total = 0;
      let fewest = 53;
      const trials = 400;
      for (let k = 0; k < trials; k++) {
        const place = k < 20 ? length - 1 - (k % 10) : k < 40 ? k % 10 : Math.floor(rand() * length);
        const other = String.fromCharCode(text.charCodeAt(place) ^ (1 << Math.floor(rand() * 5)));
        const changed = bitsDifferent(key, longKey(text.slice(0, place) + other + text.slice(place + 1)));
        total += changed;
        fewest = Math.min(fewest, changed);
      }
      expect(total / trials, `${length}: bits that differ, on average`).toBeGreaterThan(20);
      expect(total / trials, `${length}: bits that differ, on average`).toBeLessThan(33);
      expect(fewest, `${length}: fewest bits that differ`).toBeGreaterThanOrEqual(5);
    }
  });

  it('is another number on another page: each time the module is loaded it draws a seed of its own', async () => {
    const text = long(400, 3);
    const here = longKey(text);
    const keys = new Set<number>([here]);
    for (let load = 0; load < 4; load++) {
      vi.resetModules();
      const loaded = (await import('./stringTable')) as typeof import('./stringTable');
      const key = loaded.longKey(text);
      expect(loaded.longKey(text), 'the same page, the same seed').toBe(key);
      keys.add(key);
    }
    expect(keys.size).toBe(5);
    // And a table of one page does not need the key of another: it is exact on its own.
    const table = new StringTable();
    expect([table.add(text), table.add(text), table.indexOf(text)]).toEqual([0, 0, 0]);
  });
});
