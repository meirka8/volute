import { describe, expect, it } from 'vitest';
import { SHORT_STRING, StringTable } from './stringTable';

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

  it('does the same for strings too long for the engine to hash in full', () => {
    const table = new StringTable();
    const a = 'x'.repeat(SHORT_STRING + 1);
    const b = `${'x'.repeat(SHORT_STRING)}y`;
    expect(table.add(a)).toBe(0);
    expect(table.add(b)).toBe(1);
    expect(table.add(`${'x'.repeat(SHORT_STRING)}x`)).toBe(0); // another copy of the same string
    expect(table.indexOf(b)).toBe(1);
    expect(table.indexOf('x'.repeat(SHORT_STRING + 2))).toBe(-1);
  });

  it('tells strings apart that differ only where the sample does not look', () => {
    // Same length, same beginning and end; they differ in one character in the middle, at a place no sample reaches.
    const table = new StringTable();
    const base = 'p'.repeat(5_000);
    const variants = Array.from({ length: 12 }, (_, k) => `${base.slice(0, 2_501 + k)}q${base.slice(2_502 + k)}`);
    const numbers = variants.map((variant) => table.add(variant));
    expect(new Set(numbers).size).toBe(12);
    variants.forEach((variant, k) => expect(table.indexOf(variant)).toBe(numbers[k]));
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
    expect(time(() => ids.forEach((id) => table.add(id)))).toBeLessThan(500);
    expect(table.size).toBe(4_000);
    expect(time(() => ids.forEach((id, k) => expect(table.indexOf(id)).toBe(k)))).toBeLessThan(500);
  });
});
