import { describe, expect, it } from 'vitest';
import {
  cutAtCodePoint,
  displayText,
  exceedsCharBudget,
  exceedsLines,
  shouldCollapse,
  splitDisplayLines,
  WRAPPED_CHAR_BUDGET,
} from './textLines';

describe('splitDisplayLines', () => {
  it('splits on newlines', () => {
    expect(splitDisplayLines('a\nb\nc')).toEqual(['a', 'b', 'c']);
  });

  it('ignores one trailing newline but keeps interior and further blank lines', () => {
    expect(splitDisplayLines('a\nb\n')).toEqual(['a', 'b']);
    expect(splitDisplayLines('a\n\nb\n\n')).toEqual(['a', '', 'b', '']);
  });

  it('treats empty text as one empty line', () => {
    expect(splitDisplayLines('')).toEqual(['']);
  });
});

describe('exceedsLines', () => {
  it('compares the line count with the limit', () => {
    expect(exceedsLines('a\nb\nc\nd', 4)).toBe(false);
    expect(exceedsLines('a\nb\nc\nd\ne', 4)).toBe(true);
    expect(exceedsLines('single line', 1)).toBe(false);
    expect(exceedsLines('', 1)).toBe(false);
  });

  it('is fast on very long text', () => {
    expect(exceedsLines('x\n'.repeat(500_000), 4)).toBe(true);
  });
});

describe('displayText', () => {
  it('turns Windows and old Mac line endings into newlines and touches nothing else', () => {
    expect(displayText('a\r\nb\rc\nd')).toBe('a\nb\nc\nd');
    expect(displayText('tab\there  and  spaces')).toBe('tab\there  and  spaces');
    expect(displayText('trailing\r')).toBe('trailing\n');
    expect(displayText('plain')).toBe('plain');
  });

  it('makes lines come out the same whatever the line endings', () => {
    expect(splitDisplayLines('one\r\ntwo\r\n')).toEqual(['one', 'two']);
    expect(splitDisplayLines('one\r\ntwo\r\nthree')).toEqual(splitDisplayLines('one\ntwo\nthree'));
  });
});

describe('shouldCollapse', () => {
  it('collapses only when at least three lines would be hidden', () => {
    expect(shouldCollapse(12, 12)).toBe(false);
    expect(shouldCollapse(13, 12)).toBe(false);
    expect(shouldCollapse(14, 12)).toBe(false);
    expect(shouldCollapse(15, 12)).toBe(true);
    expect(shouldCollapse(500, 12)).toBe(true);
  });
});

describe('exceedsCharBudget', () => {
  it('cuts only when at least 500 characters beyond the budget would be hidden', () => {
    expect(WRAPPED_CHAR_BUDGET).toBe(1500);
    expect(exceedsCharBudget(0)).toBe(false);
    expect(exceedsCharBudget(1500)).toBe(false);
    expect(exceedsCharBudget(1999)).toBe(false);
    expect(exceedsCharBudget(2000)).toBe(true);
    expect(exceedsCharBudget(1_000_000)).toBe(true);
  });
});

describe('cutAtCodePoint', () => {
  it('returns text that fits unchanged, and otherwise its first `limit` characters', () => {
    expect(cutAtCodePoint('abc', 3)).toBe('abc');
    expect(cutAtCodePoint('abc', 10)).toBe('abc');
    expect(cutAtCodePoint('abcdef', 4)).toBe('abcd');
    expect(cutAtCodePoint('', 4)).toBe('');
  });

  it('never ends halfway through a surrogate pair', () => {
    const text = `ab${String.fromCodePoint(0x1f600)}cd`; // a b <2 units> c d
    expect(cutAtCodePoint(text, 3)).toBe('ab'); // the cut would land between the two units
    expect(cutAtCodePoint(text, 4)).toBe(`ab${String.fromCodePoint(0x1f600)}`);
    expect(cutAtCodePoint(text, 2)).toBe('ab');
  });

  it('keeps a lone high surrogate that is not at the cut', () => {
    expect(cutAtCodePoint('\ud83dxyz', 3)).toBe('\ud83dxy');
  });

  it('keeps nothing for a limit of zero or less, and never reads a negative one as "from the end"', () => {
    expect(cutAtCodePoint('abcdef', 0)).toBe('');
    expect(cutAtCodePoint('abcdef', -1)).toBe('');
    expect(cutAtCodePoint('abcdef', -100)).toBe('');
    expect(cutAtCodePoint('abcdef', Number.NEGATIVE_INFINITY)).toBe('');
    expect(cutAtCodePoint('', 0)).toBe('');
    expect(cutAtCodePoint('abcdef', 1)).toBe('a');
  });
});
