import { afterEach, describe, expect, it, vi } from 'vitest';
import { longKey, SHORT_STRING, StringTable } from './stringTable';

const MIB = 1024 * 1024;
const time = (run: () => void) => {
  const started = performance.now();
  run();
  return performance.now() - started;
};

describe('StringTable', () => {
  it('gives each distinct string a number of its own, in the order it was first seen, and the same string the same number', () => {
    const table = new StringTable();
    expect(table.add('a')).toBe(0);
    expect(table.add('b')).toBe(1);
    expect(table.add('a')).toBe(0);
    expect(table.add('')).toBe(2);
    expect(table.add('')).toBe(2);
    expect(table.values).toEqual(['a', 'b', '']);
    expect(table.size).toBe(3);
    expect(table.indexOf('b')).toBe(1);
    expect(table.indexOf('nope')).toBe(-1);
  });

  it('does the same for strings longer than the engine hashes for us', () => {
    const table = new StringTable();
    const a = 'x'.repeat(SHORT_STRING + 1);
    const b = `${'x'.repeat(SHORT_STRING)}y`;
    expect(table.add(a)).toBe(0);
    expect(table.add(b)).toBe(1);
    expect(table.add(`${'x'.repeat(SHORT_STRING)}x`)).toBe(0); // another copy of the same string
    expect(table.indexOf(b)).toBe(1);
    expect(table.indexOf('x'.repeat(SHORT_STRING + 2))).toBe(-1);
  });

  it('tells strings apart that differ in one character, wherever it is, and finds each again', () => {
    // The same length, and the same but for one place: the first, the last, and a dozen in the middle.
    const table = new StringTable();
    const base = 'p'.repeat(5_000);
    const at = (place: number) => `${base.slice(0, place)}q${base.slice(place + 1)}`;
    const variants = [at(0), at(4_999), at(1), at(4_998), ...Array.from({ length: 12 }, (_, k) => at(2_501 + 7 * k)), base];
    const numbers = variants.map((variant) => table.add(variant));
    expect(new Set(numbers).size).toBe(variants.length);
    variants.forEach((variant, k) => expect(table.indexOf(variant)).toBe(numbers[k]));
    expect(table.indexOf(`${base}p`)).toBe(-1);
  });

  it('stays exact when every long string has the same key: two strings are two numbers, whatever the keys do', () => {
    const table = new StringTable(() => 7);
    const base = 'm'.repeat(600);
    const strings = Array.from({ length: 300 }, (_, k) => `${base.slice(0, 299)}${k.toString(36).padStart(3, '0')}${base.slice(302)}`);
    const numbers = strings.map((text) => table.add(text));
    expect(numbers).toEqual(strings.map((_, k) => k));
    expect(strings.map((text) => table.add(text))).toEqual(numbers);
    expect(strings.map((text) => table.indexOf(text))).toEqual(numbers);
    expect(table.indexOf(`${base.slice(0, 299)}zzz${base.slice(302)}`)).toBe(-1);
    // The short ones never needed a key.
    expect(table.add('short')).toBe(300);
  });

  it('is fast for ids that are a mebibyte long and equal in length, however many there are', () => {
    const table = new StringTable();
    const make = (k: number) => `${'i'.repeat(MIB - 8)}${String(k).padStart(8, '0')}`;
    const elapsed = time(() => {
      for (let round = 0; round < 3; round++) for (let k = 0; k < 20; k++) expect(table.add(make(k))).toBe(k);
    });
    expect(table.size).toBe(20);
    // 60 insertions of mebibyte strings (and the strings themselves, which this test makes): well under what 20x20 full comparisons cost.
    expect(elapsed).toBeLessThan(2_000);
  });

  it('is fast for a few thousand ids of a few thousand characters, all of one length', () => {
    const table = new StringTable();
    const ids = Array.from({ length: 4_000 }, (_, k) => `${'z'.repeat(6_000)}${k.toString(16).padStart(6, '0')}`);
    // Every character of every id is read once (24 MB, twice): about a tenth of a second; a table that put them all in one bucket would compare each with the rest in full.
    expect(time(() => ids.forEach((id) => table.add(id)))).toBeLessThan(2_000);
    expect(table.size).toBe(4_000);
    expect(time(() => ids.forEach((id, k) => expect(table.indexOf(id)).toBe(k)))).toBeLessThan(2_000);
  });
});

describe('longKey', () => {
  it('is a whole number of at most 53 bits, the same for the same string for as long as the page lives', () => {
    for (const text of ['x'.repeat(257), 'y'.repeat(5_000), `${'z'.repeat(999)}é\u{1F600}`, '\u0000'.repeat(300)]) {
      const key = longKey(text);
      expect(Number.isSafeInteger(key)).toBe(true);
      expect(key).toBeGreaterThanOrEqual(0);
      expect(longKey(`${text}`)).toBe(key);
      expect(longKey(text.slice(0, 100) + text.slice(100))).toBe(key);
    }
  });

  it('has a different key for every length of a string of one character, and for a change in every place of a string', () => {
    const lengths = new Set<number>();
    for (let length = SHORT_STRING + 1; length < 3_000; length++) lengths.add(longKey('a'.repeat(length)));
    expect(lengths.size).toBe(3_000 - SHORT_STRING - 1);

    const base = 'k'.repeat(2_001);
    const places = new Set<number>([longKey(base)]);
    for (let place = 0; place < base.length; place++) places.add(longKey(`${base.slice(0, place)}l${base.slice(place + 1)}`));
    expect(places.size).toBe(base.length + 1);
  });

  it('has a different key for strings that differ in a lone surrogate, which UTF-8 would make the same', () => {
    const base = 'u'.repeat(400);
    const keys = new Set<number>();
    for (let unit = 0xd800; unit <= 0xdfff; unit += 17) keys.add(longKey(`${base}${String.fromCharCode(unit)}${base}`));
    expect(keys.size).toBe(Math.ceil((0xe000 - 0xd800) / 17));
  });

  it('gives no two of 100,000 strings that differ only in a few places the same key', () => {
    // The places of the first version of the table did not look at, as in the flood tests, in base 36.
    const base = 'q'.repeat(300);
    const places = [60, 130, 200, 270];
    const keys = new Set<number>();
    for (let k = 0; k < 100_000; k++) {
      let text = '';
      let from = 0;
      places.forEach((place, d) => {
        text += base.slice(from, place) + (Math.floor(k / 36 ** (3 - d)) % 36).toString(36);
        from = place + 1;
      });
      keys.add(longKey(text + base.slice(from)));
    }
    expect(keys.size).toBe(100_000);
  });

  it('does not give the same key to the strings that break a hash which is a polynomial in the characters, whatever its seed', () => {
    // Thue-Morse strings: S and T of every length 2^k collide for any base and any start, under a modulus of 2^32 or 2^64.
    let s = 'a';
    let t = 'b';
    for (let k = 1; k <= 14; k++) {
      [s, t] = [s + t, t + s];
      if (s.length > SHORT_STRING) expect(longKey(s), `Thue-Morse, ${s.length} characters`).not.toBe(longKey(t));
    }
    // Two characters swapped, near and far (a hash that adds up a term for each character cannot tell them apart).
    const text = Array.from({ length: 1_000 }, (_, k) => String.fromCharCode(97 + ((k * 7) % 26)));
    const key = longKey(text.join(''));
    let swaps = 0;
    for (const distance of [1, 2, 3, 4, 8, 16, 32, 64, 128, 256, 512]) {
      for (let place = 0; place + distance < text.length; place += 13) {
        if (text[place] === text[place + distance]) continue;
        const swapped = text.slice();
        [swapped[place], swapped[place + distance]] = [swapped[place + distance], swapped[place]];
        expect(longKey(swapped.join(''))).not.toBe(key);
        swaps++;
      }
    }
    expect(swaps).toBeGreaterThan(500);
  });

  it('changes about half of its bits when one character of a string changes', () => {
    const bits = (a: number, b: number) => {
      const count = (value: number) => {
        let ones = 0;
        for (let rest = value >>> 0; rest !== 0; rest &= rest - 1) ones++;
        return ones;
      };
      return count(Math.floor(a / 0x200000) ^ Math.floor(b / 0x200000)) + count((a % 0x200000) ^ (b % 0x200000));
    };
    let state = 12345;
    const random = () => {
      state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
      return state;
    };
    let total = 0;
    const trials = 3_000;
    for (let trial = 0; trial < trials; trial++) {
      const length = SHORT_STRING + 1 + (random() % 600);
      const units = Array.from({ length }, () => random() & 0xffff);
      const place = random() % length;
      const changed = units.slice();
      changed[place] = (changed[place] + 1 + (random() % 3)) & 0xffff;
      total += bits(longKey(String.fromCharCode(...units)), longKey(String.fromCharCode(...changed)));
    }
    // 53 bits: half is 26.5; a hash that mixed badly would sit far from it.
    expect(total / trials).toBeGreaterThan(24);
    expect(total / trials).toBeLessThan(29);
  });
});

describe('the seed of the keys', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  /** A source of random numbers that fills its array from `pattern`, and says how often it was asked. */
  const stubRandom = (pattern: (index: number) => number) => {
    const asked: number[] = [];
    vi.stubGlobal('crypto', {
      getRandomValues: (array: Uint32Array) => {
        asked.push(array.length);
        for (let k = 0; k < array.length; k++) array[k] = pattern(k) >>> 0;
        return array;
      },
    });
    return asked;
  };
  const fresh = async () => {
    vi.resetModules();
    return import('./stringTable');
  };

  it('is drawn once per page, from the browser, and only when a long string first needs it', async () => {
    const asked = stubRandom((k) => Math.imul(k + 1, 0x9e3779b1) ^ 0x1234abcd);
    const page = await fresh();
    expect(asked).toHaveLength(0);
    const table = new page.StringTable();
    table.add('short');
    table.indexOf('another short one');
    expect(asked).toHaveLength(0);
    table.add('x'.repeat(300));
    expect(asked).toHaveLength(1);
    page.longKey('y'.repeat(5_000));
    table.add('z'.repeat(400));
    new page.StringTable().add('w'.repeat(500));
    expect(asked).toHaveLength(1);
  });

  it('is another for another page: the same string has another key', async () => {
    const text = 'x'.repeat(300);
    stubRandom((k) => Math.imul(k + 1, 0x9e3779b1) ^ 0x1234abcd);
    const first = (await fresh()).longKey(text);
    stubRandom((k) => Math.imul(k + 1, 0x9e3779b1) ^ 0x1234abce);
    const second = (await fresh()).longKey(text);
    stubRandom((k) => Math.imul(k + 1, 0x9e3779b1) ^ 0x1234abcd);
    const again = (await fresh()).longKey(text);
    expect(second).not.toBe(first);
    expect(again).toBe(first); // the same seed, the same key: the seed is all there is to it
  });

  it('is not a seed a polynomial hash would be helped by: no seed makes the strings that collide under every seed of one collide here', async () => {
    let s = 'a';
    let t = 'b';
    const pairs: Array<[string, string]> = [];
    for (let k = 1; k <= 12; k++) {
      [s, t] = [s + t, t + s];
      if (s.length > SHORT_STRING) pairs.push([s, t]);
    }
    for (const salt of [0, 1, 0xdeadbeef, 0x80000000, 0xffffffff, 0x00010001]) {
      stubRandom((k) => Math.imul(k + 7, 0x85ebca6b) ^ salt);
      const page = await fresh();
      for (const [a, b] of pairs) expect(page.longKey(a), `seed ${salt}, ${a.length} characters`).not.toBe(page.longKey(b));
    }
  });

  it('is made when the source of random numbers refuses, as it does without one', async () => {
    vi.stubGlobal('crypto', {
      getRandomValues: () => {
        throw new DOMException('not allowed', 'NotAllowedError');
      },
    });
    const page = await fresh();
    const key = page.longKey('v'.repeat(300));
    expect(Number.isSafeInteger(key)).toBe(true);
    expect(page.longKey('v'.repeat(300))).toBe(key);
  });

  it('is made without a source of random numbers too, and is the same for a string all the same', async () => {
    vi.stubGlobal('crypto', undefined);
    const page = await fresh();
    const key = page.longKey('v'.repeat(300));
    expect(Number.isSafeInteger(key)).toBe(true);
    expect(page.longKey('v'.repeat(300))).toBe(key);
    const table = new page.StringTable();
    expect(table.add('v'.repeat(300))).toBe(0);
    expect(table.add(`${'v'.repeat(299)}w`)).toBe(1);
    expect(table.indexOf('v'.repeat(300))).toBe(0);
  });
});
