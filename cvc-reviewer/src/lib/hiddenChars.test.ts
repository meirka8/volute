import { describe, expect, it } from 'vitest';
import {
  hasHiddenChars,
  hiddenAsText,
  hiddenCharName,
  hiddenCodePointLabel,
  hiddenTitle,
  hiddenToken,
  isHiddenCodePoint,
  MAX_HIDDEN_TOKENS,
  splitHidden,
  stripHidden,
  type HiddenMode,
  type HiddenSegment,
} from './hiddenChars';

// ---------------------------------------------------------------------------
// The class, written out independently of the module: Unicode's Default_Ignorable_Code_Point
// property, the C0 and C1 controls, and the interlinear annotation characters.
// ---------------------------------------------------------------------------

type Range = readonly [number, number];

/** Hidden in both modes. */
const BASELINE: readonly Range[] = [
  [0x00, 0x08], // C0 controls except TAB (09), LF (0A) and CR (0D)
  [0x0b, 0x0c],
  [0x0e, 0x1f],
  [0x7f, 0x7f], // DEL
  [0x80, 0x9f], // C1
  [0xad, 0xad],
  [0x34f, 0x34f],
  [0x61c, 0x61c],
  [0x115f, 0x1160],
  [0x17b4, 0x17b5],
  [0x180b, 0x180f],
  [0x200b, 0x200f],
  [0x202a, 0x202e],
  [0x2060, 0x206f], // including the unassigned U+2065
  [0x3164, 0x3164],
  [0xfeff, 0xfeff],
  [0xffa0, 0xffa0],
  [0xfff0, 0xfff8], // unassigned code points of the Specials block that the property lists
  [0xfff9, 0xfffb], // interlinear annotation
  [0x1bca0, 0x1bca3], // shorthand format controls
  [0x1d173, 0x1d17a], // musical symbols: beams, ties, slurs and phrases
  [0xe0000, 0xe0fff], // tag characters, variation selectors supplement, and the code points around them
];
/** Hidden only where text is code-like. */
const STRICT_ONLY: readonly Range[] = [
  [0x200c, 0x200d], // ZWNJ, ZWJ
  [0xfe00, 0xfe0f], // variation selectors 1-16
];

const inRanges = (codePoint: number, ranges: readonly Range[]) => ranges.some(([low, high]) => codePoint >= low && codePoint <= high);

/** What the issue says about one code point. */
function expectedHidden(codePoint: number, mode: HiddenMode): boolean {
  if (mode === 'strict') return inRanges(codePoint, BASELINE) || inRanges(codePoint, STRICT_ONLY);
  return inRanges(codePoint, BASELINE) && !inRanges(codePoint, STRICT_ONLY);
}

const chr = (codePoint: number) => String.fromCodePoint(codePoint);
const MODES: HiddenMode[] = ['strict', 'prose'];

/** Every code point in the class at this level, each once, in order. */
function members(mode: HiddenMode): number[] {
  const found = new Set<number>();
  for (const [low, high] of [...BASELINE, ...STRICT_ONLY]) {
    for (let codePoint = low; codePoint <= high; codePoint++) if (expectedHidden(codePoint, mode)) found.add(codePoint);
  }
  return [...found].sort((a, b) => a - b);
}

/** Plain text and hidden characters put back together, the way the text was. */
function rebuild(segments: HiddenSegment[]): string {
  return segments.map((segment) => (segment.kind === 'text' ? segment.text : segment.kind === 'hidden' ? chr(segment.codePoint) : '')).join('');
}

describe('the class of hidden characters', () => {
  it.each(MODES)('classifies every code point of the Basic Multilingual Plane as the issue says (%s)', (mode) => {
    const wrong: string[] = [];
    for (let codePoint = 0; codePoint <= 0xffff; codePoint++) {
      const text = String.fromCharCode(codePoint);
      if (hasHiddenChars(text, mode) !== expectedHidden(codePoint, mode)) wrong.push(hiddenCodePointLabel(codePoint));
      // The same answer from the splitter: no hidden character means no segments at all.
      if ((splitHidden(text, mode) !== null) !== expectedHidden(codePoint, mode)) wrong.push(`split ${hiddenCodePointLabel(codePoint)}`);
    }
    expect(wrong).toEqual([]);
  });

  it('covers every Default_Ignorable_Code_Point, and has no other member than the documented extras', () => {
    // The oracle is the engine's own copy of Unicode's property: nothing of this module is used to build it.
    const defaultIgnorable = /\p{Default_Ignorable_Code_Point}/u;
    const missing: string[] = [];
    const extra: number[] = [];
    for (let codePoint = 0; codePoint <= 0x10ffff; codePoint++) {
      if (codePoint >= 0xd800 && codePoint <= 0xdfff) continue; // a lone surrogate is not a code point of any text
      const character = chr(codePoint);
      const ignorable = defaultIgnorable.test(character);
      const hidden = hasHiddenChars(character, 'strict');
      if (ignorable && !hidden) missing.push(hiddenCodePointLabel(codePoint));
      if (hidden && !ignorable) extra.push(codePoint);
    }
    expect(missing).toEqual([]);
    // What the class holds besides the property: the C0 controls (but not tab, line feed and carriage return),
    // delete, the C1 controls, and the interlinear annotation characters.
    const documented = (codePoint: number) =>
      (codePoint <= 0x1f && ![0x09, 0x0a, 0x0d].includes(codePoint)) || (codePoint >= 0x7f && codePoint <= 0x9f) || (codePoint >= 0xfff9 && codePoint <= 0xfffb);
    expect(extra.filter((codePoint) => !documented(codePoint)).map(hiddenCodePointLabel)).toEqual([]);
    expect(extra).toHaveLength(29 + 1 + 32 + 3);
  });

  it('keeps for prose the distinctions it always made: the joiners and the variation selectors are code, not text', () => {
    const defaultIgnorable = /\p{Default_Ignorable_Code_Point}/u;
    const keptByProse: number[] = [];
    for (let codePoint = 0; codePoint <= 0xffff; codePoint++) {
      if (defaultIgnorable.test(chr(codePoint)) && !hasHiddenChars(chr(codePoint), 'prose')) keptByProse.push(codePoint);
    }
    expect(keptByProse).toEqual([0x200c, 0x200d, ...Array.from({ length: 16 }, (_, k) => 0xfe00 + k)]);
  });

  it('answers the same for a code point as for the text that holds it (isHiddenCodePoint)', () => {
    const wrong: string[] = [];
    for (const mode of MODES) {
      for (let codePoint = 0; codePoint <= 0x10ffff; codePoint++) {
        if (codePoint >= 0xd800 && codePoint <= 0xdfff) continue;
        if (isHiddenCodePoint(codePoint, mode) !== hasHiddenChars(chr(codePoint), mode)) wrong.push(`U+${codePoint.toString(16)} in ${mode}`);
      }
    }
    expect(wrong).toEqual([]);
  });

  it.each(MODES)('classifies the supplementary planes as the issue says (%s)', (mode) => {
    const wrong: string[] = [];
    // The whole block around the tag characters and the variation selectors, and the emoji planes.
    const check = (low: number, high: number) => {
      for (let codePoint = low; codePoint <= high; codePoint++) {
        if (hasHiddenChars(chr(codePoint), mode) !== expectedHidden(codePoint, mode)) wrong.push(hiddenCodePointLabel(codePoint));
      }
    };
    check(0xdff00, 0xe1100);
    check(0x1bc00, 0x1bd00);
    check(0x1d100, 0x1d200);
    check(0x1f000, 0x1faff);
    for (const codePoint of [0x10000, 0x2ffff, 0x10ffff]) check(codePoint, codePoint);
    expect(wrong).toEqual([]);
  });

  it('has the members listed above, counted', () => {
    // C0 29 (9 + 2 + 18), DEL 1, C1 32, three single characters, three short blocks (2 + 2 + 5),
    // the zero-width block 5, bidi embeddings 5, the block from the word joiner to the deprecated format controls 16,
    // three single characters, the reserved and annotation blocks of the Specials 12, shorthand format controls 4,
    // musical symbols 8, and the plane-14 block of tags and selectors 4096.
    const baseline = 29 + 1 + 32 + 3 + (2 + 2 + 5) + 5 + 5 + 16 + 3 + 12 + 4 + 8 + 4096;
    expect(baseline).toBe(4223);
    // Strict adds U+FE00-U+FE0F; prose leaves out the joiner and non-joiner.
    expect(members('strict')).toHaveLength(baseline + 16);
    expect(members('prose')).toHaveLength(baseline - 2);
    const prose = new Set(members('prose'));
    expect(members('strict').filter((codePoint) => !prose.has(codePoint))).toHaveLength(18);
  });

  it.each([
    ['TAB', 0x09],
    ['LF', 0x0a],
    ['CR', 0x0d],
    ['space', 0x20],
    ['tilde', 0x7e],
    ['no-break space', 0xa0],
    ['U+00AC (not sign)', 0xac],
    ['U+034E', 0x34e],
    ['U+0350', 0x350],
    ['U+061B', 0x61b],
    ['U+061D', 0x61d],
    ['en quad', 0x2000],
    ['hair space', 0x200a],
    ['line separator', 0x2028],
    ['paragraph separator', 0x2029],
    ['U+2070', 0x2070],
    ['U+3163', 0x3163],
    ['U+3165', 0x3165],
    ['U+FEFE', 0xfefe],
    ['U+FFA1', 0xffa1],
    ['U+FFFC (object replacement)', 0xfffc],
    ['U+FFEF', 0xffef],
    ['U+FFFD (replacement character)', 0xfffd],
    ['U+1BC9F', 0x1bc9f],
    ['U+1BCA4', 0x1bca4],
    ['U+1D172', 0x1d172],
    ['U+1D17B', 0x1d17b],
    ['U+E1000', 0xe1000],
    ['U+DFFFF', 0xdffff],
    ['a letter', 0x61],
    ['a CJK ideograph', 0x4e2d],
  ])('does not hide %s, in either mode', (_label, codePoint) => {
    for (const mode of MODES) expect(hasHiddenChars(chr(codePoint), mode), `${hiddenCodePointLabel(codePoint)} ${mode}`).toBe(false);
  });

  it.each([
    ['NUL', 0x00],
    ['VT', 0x0b],
    ['FF', 0x0c],
    ['ESC', 0x1b],
    ['DEL', 0x7f],
    ['C1 NEL', 0x85],
    ['soft hyphen', 0xad],
    ['combining grapheme joiner', 0x34f],
    ['Arabic letter mark', 0x61c],
    ['Hangul choseong filler', 0x115f],
    ['Khmer vowel inherent', 0x17b4],
    ['Mongolian vowel separator', 0x180e],
    ['ZWSP', 0x200b],
    ['LRM', 0x200e],
    ['RLM', 0x200f],
    ['LRE', 0x202a],
    ['RLO', 0x202e],
    ['word joiner', 0x2060],
    ['invisible plus', 0x2064],
    ['LRI', 0x2066],
    ['PDI', 0x2069],
    ['nominal digit shapes', 0x206f],
    ['Hangul filler', 0x3164],
    ['BOM', 0xfeff],
    ['halfwidth Hangul filler', 0xffa0],
    ['interlinear annotation', 0xfffa],
    ['tag A', 0xe0041],
    ['cancel tag', 0xe007f],
    ['variation selector 17', 0xe0100],
    ['variation selector 256', 0xe01ef],
    ['U+2065 (unassigned)', 0x2065],
    ['U+FFF0 (unassigned)', 0xfff0],
    ['U+FFF8 (unassigned)', 0xfff8],
    ['U+1BCA0', 0x1bca0],
    ['U+1BCA3', 0x1bca3],
    ['musical symbol begin beam', 0x1d173],
    ['musical symbol end phrase', 0x1d17a],
    ['U+E0002 (unassigned)', 0xe0002],
    ['U+E0080 (unassigned)', 0xe0080],
    ['U+E00FF (unassigned)', 0xe00ff],
    ['U+E01F0 (unassigned)', 0xe01f0],
    ['U+E0FFF (unassigned)', 0xe0fff],
  ])('hides %s, in both modes', (_label, codePoint) => {
    for (const mode of MODES) expect(hasHiddenChars(chr(codePoint), mode), `${hiddenCodePointLabel(codePoint)} ${mode}`).toBe(true);
  });

  it.each([
    ['zero-width joiner', 0x200d],
    ['zero-width non-joiner', 0x200c],
    ['variation selector-1', 0xfe00],
    ['variation selector-15 (text presentation)', 0xfe0e],
    ['variation selector-16 (emoji presentation)', 0xfe0f],
  ])('hides %s in strict text only', (_label, codePoint) => {
    expect(hasHiddenChars(chr(codePoint), 'strict')).toBe(true);
    expect(hasHiddenChars(chr(codePoint), 'prose')).toBe(false);
  });
});

describe('emoji and scripts that use joiners', () => {
  const FAMILY = '👨\u200D👩\u200D👧'; // man, ZWJ, woman, ZWJ, girl
  const TECHNOLOGIST = '👩🏽\u200D💻'; // woman, skin tone, ZWJ, laptop
  const HEART = '❤\uFE0F'; // heavy black heart + VS16
  const RAINBOW_FLAG = '🏳\uFE0F\u200D🌈'; // white flag + VS16 + ZWJ + rainbow
  const PERSIAN = 'می\u200Cخواهم'; // contains a ZWNJ
  const INDIC = 'क्\u200Dष'; // Devanagari with a ZWJ

  it.each([FAMILY, TECHNOLOGIST, HEART, RAINBOW_FLAG, PERSIAN, INDIC])('leaves %s untouched in prose', (text) => {
    expect(splitHidden(text, 'prose')).toBeNull();
    expect(hasHiddenChars(text, 'prose')).toBe(false);
  });

  it('reveals the joiners and selectors of the same text in strict mode, in order', () => {
    const family = splitHidden(FAMILY, 'strict')!;
    expect(family.filter((segment) => segment.kind === 'hidden')).toEqual([
      { kind: 'hidden', codePoint: 0x200d },
      { kind: 'hidden', codePoint: 0x200d },
    ]);
    expect(rebuild(family)).toBe(FAMILY);
    expect(splitHidden(HEART, 'strict')).toEqual([
      { kind: 'text', text: '❤' },
      { kind: 'hidden', codePoint: 0xfe0f },
    ]);
    expect(splitHidden(PERSIAN, 'strict')!.some((segment) => segment.kind === 'hidden' && segment.codePoint === 0x200c)).toBe(true);
    expect(rebuild(splitHidden(RAINBOW_FLAG, 'strict')!)).toBe(RAINBOW_FLAG);
  });

  it('still reveals everything else in prose, next to emoji that are left alone', () => {
    const text = `${FAMILY} ok\u202E ${HEART}\u200B`;
    const segments = splitHidden(text, 'prose')!;
    expect(segments.filter((segment) => segment.kind === 'hidden').map((segment) => (segment as { codePoint: number }).codePoint)).toEqual([0x202e, 0x200b]);
    expect(rebuild(segments)).toBe(text);
  });
});

describe('splitHidden', () => {
  it('returns null, and allocates nothing, for text with no hidden character', () => {
    for (const text of ['', 'plain text', 'tabs\tand\nnewlines\r\n', 'unicode: é ü 中文 😀', 'a'.repeat(10_000)]) {
      for (const mode of MODES) expect(splitHidden(text, mode), text.slice(0, 20)).toBeNull();
    }
  });

  it('splits around one hidden character, in order', () => {
    expect(splitHidden('rm -rf ./a\u202Eb', 'strict')).toEqual([
      { kind: 'text', text: 'rm -rf ./a' },
      { kind: 'hidden', codePoint: 0x202e },
      { kind: 'text', text: 'b' },
    ]);
  });

  it('keeps hidden characters at the start, at the end and next to each other', () => {
    expect(splitHidden('\u200Babc', 'strict')).toEqual([
      { kind: 'hidden', codePoint: 0x200b },
      { kind: 'text', text: 'abc' },
    ]);
    expect(splitHidden('abc\u200B', 'strict')).toEqual([
      { kind: 'text', text: 'abc' },
      { kind: 'hidden', codePoint: 0x200b },
    ]);
    expect(splitHidden('a\u202E\u2066\u0007b', 'strict')).toEqual([
      { kind: 'text', text: 'a' },
      { kind: 'hidden', codePoint: 0x202e },
      { kind: 'hidden', codePoint: 0x2066 },
      { kind: 'hidden', codePoint: 0x07 },
      { kind: 'text', text: 'b' },
    ]);
    expect(splitHidden('\u200B', 'strict')).toEqual([{ kind: 'hidden', codePoint: 0x200b }]);
  });

  it('reads tag characters, written as surrogate pairs, as the code points they are', () => {
    // "Hi" spelled in tag characters, between two letters.
    const text = `a${chr(0xe0048)}${chr(0xe0069)}b${chr(0xe007f)}`;
    expect(text.length).toBe(1 + 2 + 2 + 1 + 2);
    expect(splitHidden(text, 'prose')).toEqual([
      { kind: 'text', text: 'a' },
      { kind: 'hidden', codePoint: 0xe0048 },
      { kind: 'hidden', codePoint: 0xe0069 },
      { kind: 'text', text: 'b' },
      { kind: 'hidden', codePoint: 0xe007f },
    ]);
  });

  it('reads the variation selectors supplement the same way, and the rest of plane 14 up to U+E0FFF', () => {
    expect(splitHidden(`x${chr(0xe0100)}${chr(0xe01ef)}`, 'prose')).toEqual([
      { kind: 'text', text: 'x' },
      { kind: 'hidden', codePoint: 0xe0100 },
      { kind: 'hidden', codePoint: 0xe01ef },
    ]);
    // Any low surrogate after one of the four high surrogates is a member (U+E0000-U+E0FFF) ...
    for (const [high, low, codePoint] of [
      [0xdb40, 0xdc80, 0xe0080],
      [0xdb40, 0xddf0, 0xe01f0],
      [0xdb41, 0xdc00, 0xe0400],
      [0xdb42, 0xdfff, 0xe0bff],
      [0xdb43, 0xdfff, 0xe0fff],
    ] as const) {
      const text = `a${String.fromCharCode(high)}${String.fromCharCode(low)}b`;
      expect(splitHidden(text, 'strict'), `U+${codePoint.toString(16)}`).toEqual([
        { kind: 'text', text: 'a' },
        { kind: 'hidden', codePoint },
        { kind: 'text', text: 'b' },
      ]);
    }
    // ... and nothing next to them is: a high surrogate that is not followed by a low one, and the block after.
    const high = String.fromCharCode(0xdb40);
    for (const unit of [0xdbff, 0xd800, 0x0041]) {
      expect(splitHidden(`a${high}${String.fromCharCode(unit)}b`, 'strict'), `unit ${unit.toString(16)}`).toBeNull();
    }
    expect(splitHidden(`a${String.fromCharCode(0xdb44)}${String.fromCharCode(0xdc00)}b`, 'strict')).toBeNull(); // U+E1000
  });

  it('reads the musical symbols and the shorthand format controls, written as surrogate pairs, as the code points they are', () => {
    expect(splitHidden(`a${chr(0x1d173)}${chr(0x1d17a)}b`, 'prose')).toEqual([
      { kind: 'text', text: 'a' },
      { kind: 'hidden', codePoint: 0x1d173 },
      { kind: 'hidden', codePoint: 0x1d17a },
      { kind: 'text', text: 'b' },
    ]);
    expect(splitHidden(`a${chr(0x1bca0)}${chr(0x1bca3)}b`, 'strict')).toEqual([
      { kind: 'text', text: 'a' },
      { kind: 'hidden', codePoint: 0x1bca0 },
      { kind: 'hidden', codePoint: 0x1bca3 },
      { kind: 'text', text: 'b' },
    ]);
    // The pairs on either side of them are ordinary characters.
    for (const codePoint of [0x1d172, 0x1d17b, 0x1bc9f, 0x1bca4]) expect(splitHidden(`a${chr(codePoint)}b`, 'strict'), hiddenCodePointLabel(codePoint)).toBeNull();
  });

  it('does not misread lone and neighbouring surrogates', () => {
    const high = String.fromCharCode(0xdb40);
    expect(splitHidden(high, 'strict')).toBeNull(); // at the end of the text
    expect(splitHidden(`a${high}`, 'strict')).toBeNull();
    expect(splitHidden(`${high}a`, 'strict')).toBeNull();
    expect(splitHidden(`${high}${high}`, 'strict')).toBeNull();
    expect(splitHidden(String.fromCharCode(0xdc00), 'strict')).toBeNull(); // a low surrogate alone
    // A high surrogate that is not the tag block's, followed by a tag-range low one.
    expect(splitHidden(`${String.fromCharCode(0xd83d)}${String.fromCharCode(0xdc00)}`, 'strict')).toBeNull();
    // The right pair after a stray high surrogate still counts.
    expect(splitHidden(`${high}${high}${String.fromCharCode(0xdc41)}`, 'strict')).toEqual([
      { kind: 'text', text: high },
      { kind: 'hidden', codePoint: 0xe0041 },
    ]);
  });

  it('puts the text back together exactly when nothing is over the limit', () => {
    // Deterministic xorshift, so a failure is reproducible.
    let state = 0x2545f491;
    const next = () => {
      state ^= state << 13;
      state ^= state >>> 17;
      state ^= state << 5;
      return (state >>> 0) / 0x100000000;
    };
    const pool = ['a', 'b', ' ', '\n', '\t', '\u200B', '\u200D', '\u202E', '\u0007', '\u{E0041}', '\u{E0100}', '\uFE0F', '👨', 'é', String.fromCharCode(0xdb40), '\u00AD'];
    for (let run = 0; run < 3000; run++) {
      let text = '';
      const length = 1 + Math.floor(next() * 40);
      for (let i = 0; i < length; i++) text += pool[Math.floor(next() * pool.length)];
      for (const mode of MODES) {
        const segments = splitHidden(text, mode);
        expect(segments !== null, JSON.stringify(text)).toBe(hasHiddenChars(text, mode));
        if (segments === null) continue;
        expect(rebuild(segments), JSON.stringify(text)).toBe(text);
        // No plain run holds a hidden character, and no two plain runs are next to each other.
        segments.forEach((segment, index) => {
          if (segment.kind !== 'text') return;
          expect(hasHiddenChars(segment.text, mode)).toBe(false);
          expect(segments[index - 1]?.kind).not.toBe('text');
        });
      }
    }
  });

  describe('the limit on tokens', () => {
    it('is 200 by default', () => {
      expect(MAX_HIDDEN_TOKENS).toBe(200);
    });

    it('draws exactly the limit, then counts the rest in one summary where the first of them was', () => {
      const segments = splitHidden(`start${'\u200B'.repeat(1000)}end`, 'strict')!;
      expect(segments.filter((segment) => segment.kind === 'hidden')).toHaveLength(MAX_HIDDEN_TOKENS);
      expect(segments.filter((segment) => segment.kind === 'more')).toEqual([{ kind: 'more', count: 800 }]);
      // The text before the first hidden character, the tokens, the summary, then the text after.
      expect(segments[0]).toEqual({ kind: 'text', text: 'start' });
      expect(segments[1 + MAX_HIDDEN_TOKENS]).toEqual({ kind: 'more', count: 800 });
      expect(segments[segments.length - 1]).toEqual({ kind: 'text', text: 'end' });
    });

    it('draws a text with exactly the limit of them with no summary', () => {
      const segments = splitHidden('\u200B'.repeat(MAX_HIDDEN_TOKENS), 'strict')!;
      expect(segments).toHaveLength(MAX_HIDDEN_TOKENS);
      expect(segments.every((segment) => segment.kind === 'hidden')).toBe(true);
      expect(splitHidden('\u200B'.repeat(MAX_HIDDEN_TOKENS + 1), 'strict')!.at(-1)).toEqual({ kind: 'more', count: 1 });
    });

    it('joins the text after the limit into one run, with every hidden character left out of it', () => {
      const text = 'ab\u200B'.repeat(500); // 500 hidden characters between 500 pairs of letters
      const segments = splitHidden(text, 'strict')!;
      const after = segments.slice(segments.findIndex((segment) => segment.kind === 'more') + 1);
      expect(after).toHaveLength(1);
      expect(after[0].kind).toBe('text');
      expect((after[0] as { text: string }).text).toBe('ab'.repeat(299)); // 200 tokens, the 201st starts the summary
      expect(segments.find((segment) => segment.kind === 'more')).toEqual({ kind: 'more', count: 300 });
      for (const segment of segments) if (segment.kind === 'text') expect(hasHiddenChars(segment.text, 'strict')).toBe(false);
    });

    it('takes the limit as an argument', () => {
      const segments = splitHidden('a\u200Bb\u200Bc\u200Bd', 'strict', 2)!;
      expect(segments).toEqual([
        { kind: 'text', text: 'a' },
        { kind: 'hidden', codePoint: 0x200b },
        { kind: 'text', text: 'b' },
        { kind: 'hidden', codePoint: 0x200b },
        { kind: 'text', text: 'c' },
        { kind: 'more', count: 1 },
        { kind: 'text', text: 'd' },
      ]);
      expect(splitHidden('\u200B\u200B', 'strict', 0)).toEqual([{ kind: 'more', count: 2 }]);
    });
  });
});

describe('what a hidden character is called', () => {
  it('names the bidirectional controls', () => {
    expect(hiddenCharName(0x202a)).toBe('LEFT-TO-RIGHT EMBEDDING');
    expect(hiddenCharName(0x202b)).toBe('RIGHT-TO-LEFT EMBEDDING');
    expect(hiddenCharName(0x202c)).toBe('POP DIRECTIONAL FORMATTING');
    expect(hiddenCharName(0x202d)).toBe('LEFT-TO-RIGHT OVERRIDE');
    expect(hiddenCharName(0x202e)).toBe('RIGHT-TO-LEFT OVERRIDE');
    expect(hiddenCharName(0x2066)).toBe('LEFT-TO-RIGHT ISOLATE');
    expect(hiddenCharName(0x2067)).toBe('RIGHT-TO-LEFT ISOLATE');
    expect(hiddenCharName(0x2068)).toBe('FIRST STRONG ISOLATE');
    expect(hiddenCharName(0x2069)).toBe('POP DIRECTIONAL ISOLATE');
    expect(hiddenCharName(0x200e)).toBe('LEFT-TO-RIGHT MARK');
    expect(hiddenCharName(0x200f)).toBe('RIGHT-TO-LEFT MARK');
    expect(hiddenCharName(0x061c)).toBe('ARABIC LETTER MARK');
  });

  it('names the zero-width and invisible characters', () => {
    expect(hiddenCharName(0x200b)).toBe('ZERO WIDTH SPACE');
    expect(hiddenCharName(0x200c)).toBe('ZERO WIDTH NON-JOINER');
    expect(hiddenCharName(0x200d)).toBe('ZERO WIDTH JOINER');
    expect(hiddenCharName(0x2060)).toBe('WORD JOINER');
    expect(hiddenCharName(0xfeff)).toBe('ZERO WIDTH NO-BREAK SPACE (BYTE ORDER MARK)');
    expect(hiddenCharName(0x00ad)).toBe('SOFT HYPHEN');
  });

  it('gives every member of the class a name of its own kind, never the fallback', () => {
    for (const mode of MODES) {
      for (const codePoint of members(mode)) {
        const name = hiddenCharName(codePoint);
        expect(name, hiddenCodePointLabel(codePoint)).not.toBe('HIDDEN CHARACTER');
        expect(name.length).toBeGreaterThan(3);
      }
    }
    // Controls, selectors and tags are named one by one.
    expect(hiddenCharName(0x07)).toBe('CONTROL CHARACTER: ALERT');
    expect(hiddenCharName(0x1b)).toBe('CONTROL CHARACTER: ESCAPE');
    expect(hiddenCharName(0x7f)).toBe('CONTROL CHARACTER: DELETE');
    expect(hiddenCharName(0x85)).toBe('CONTROL CHARACTER: NEXT LINE');
    expect(hiddenCharName(0xfe0f)).toBe('VARIATION SELECTOR-16');
    expect(hiddenCharName(0xe0100)).toBe('VARIATION SELECTOR-17');
    expect(hiddenCharName(0xe01ef)).toBe('VARIATION SELECTOR-256');
    expect(hiddenCharName(0xe0041)).toBe('TAG CHARACTER FOR "A"');
    expect(hiddenCharName(0xe0020)).toBe('TAG CHARACTER FOR " "');
    expect(hiddenCharName(0xe007f)).toBe('CANCEL TAG');
    expect(hiddenCharName(0xe0001)).toBe('LANGUAGE TAG');
    expect(hiddenCharName(0xe0000)).toBe('TAG CHARACTER');
    expect(hiddenCharName(0x4e2d)).toBe('HIDDEN CHARACTER'); // not in the class: only the fallback
  });

  it('writes the token and the tooltip', () => {
    expect(hiddenToken(0x202e)).toBe('⟨U+202E⟩');
    expect(hiddenToken(0x07)).toBe('⟨U+0007⟩');
    expect(hiddenToken(0xe0041)).toBe('⟨U+E0041⟩');
    expect(hiddenToken(0xfeff)).toBe('⟨U+FEFF⟩');
    expect(hiddenCodePointLabel(0x1b)).toBe('U+001B');
    expect(hiddenTitle(0x202e)).toBe('RIGHT-TO-LEFT OVERRIDE (U+202E)');
    expect(hiddenTitle(0xe0041)).toBe('TAG CHARACTER FOR "A" (U+E0041)');
  });

  it('writes out text with its hidden characters as tokens, for places that can only hold text', () => {
    expect(hiddenAsText('plain', 'strict')).toBe('plain');
    expect(hiddenAsText('a\u202Eb\u200Bc', 'strict')).toBe('a⟨U+202E⟩b⟨U+200B⟩c');
    expect(hiddenAsText(`x${chr(0xe0041)}`, 'prose')).toBe('x⟨U+E0041⟩');
    expect(hiddenAsText('👨\u200D👩\u200D👧', 'prose')).toBe('👨\u200D👩\u200D👧');
    expect(hiddenAsText('👨\u200D👩\u200D👧', 'strict')).toBe('👨⟨U+200D⟩👩⟨U+200D⟩👧');
    expect(hiddenAsText('\u200B'.repeat(300), 'strict')).toBe('⟨U+200B⟩'.repeat(200) + '⟨+100 more⟩');
  });
});

describe('cost', () => {
  const MIB = 1024 * 1024;
  const time = (run: () => void) => {
    const started = performance.now();
    run();
    return performance.now() - started;
  };

  it('is linear on a mebibyte of plain text, and finds nothing', () => {
    const text = 'The quick brown fox jumps over the lazy dog.\n'.repeat(Math.ceil(MIB / 45)).slice(0, MIB);
    expect(text.length).toBe(MIB);
    for (const mode of MODES) {
      expect(time(() => expect(splitHidden(text, mode)).toBeNull())).toBeLessThan(250);
      expect(time(() => expect(hasHiddenChars(text, mode)).toBe(false))).toBeLessThan(250);
    }
  });

  it('is linear on a mebibyte of non-ASCII text', () => {
    const text = 'こんにちは世界 é ü ß 😀 '.repeat(Math.ceil(MIB / 14)).slice(0, MIB);
    expect(time(() => expect(splitHidden(text, 'strict')).toBeNull())).toBeLessThan(250);
  });

  it('is bounded on a mebibyte of nothing but hidden characters', () => {
    const text = '\u200B'.repeat(MIB);
    let segments: HiddenSegment[] | null = null;
    expect(time(() => (segments = splitHidden(text, 'strict')))).toBeLessThan(500);
    expect(segments!.length).toBe(MAX_HIDDEN_TOKENS + 1); // the tokens, then one summary
    expect(segments!.at(-1)).toEqual({ kind: 'more', count: MIB - MAX_HIDDEN_TOKENS });
  });

  it('is bounded on a mebibyte that alternates a letter and a hidden character', () => {
    const text = 'a\u200B'.repeat(MIB / 2);
    let segments: HiddenSegment[] | null = null;
    expect(time(() => (segments = splitHidden(text, 'strict')))).toBeLessThan(750);
    // 200 tokens, each after its letter; the 201st hidden character's letter and the summary; then ONE run
    // for every letter after that (the hidden characters among them are left out of it).
    expect(segments!.length).toBe(2 * MAX_HIDDEN_TOKENS + 3);
    const last = segments!.at(-1) as { kind: string; text: string };
    expect(last.kind).toBe('text');
    expect(last.text.length).toBe(MIB / 2 - MAX_HIDDEN_TOKENS - 1);
  });

  it('is bounded on a mebibyte of tag characters, which take two units each', () => {
    const text = chr(0xe0041).repeat(MIB / 2);
    let segments: HiddenSegment[] | null = null;
    expect(time(() => (segments = splitHidden(text, 'prose')))).toBeLessThan(500);
    expect(segments!.at(-1)).toEqual({ kind: 'more', count: MIB / 2 - MAX_HIDDEN_TOKENS });
  });

  it('is linear on the worst case for a scan that stops early: a hidden character at the very end', () => {
    const text = `${'x'.repeat(MIB - 1)}\u200B`;
    expect(time(() => expect(hasHiddenChars(text, 'strict')).toBe(true))).toBeLessThan(250);
  });
});

describe('stripHidden', () => {
  it('returns the very same string when there is nothing to leave out', () => {
    const text = 'plain text, with a tab\t and a newline\n and \u00e9 and 😀';
    for (const mode of MODES) expect(stripHidden(text, mode)).toBe(text);
  });

  it('leaves out every hidden character, so an invisible one cannot split a word', () => {
    expect(stripHidden('pass\u200Bword', 'strict')).toBe('password');
    expect(stripHidden('\u202Eab\u2066c\u2069d\ufeff', 'strict')).toBe('abcd');
    expect(stripHidden(`x${chr(0xe0041)}${chr(0xe0100)}y`, 'strict')).toBe('xy');
  });

  it('keeps tab, line feed and carriage return, which are not in the class', () => {
    expect(stripHidden('a\tb\nc\rd\u0000e', 'strict')).toBe('a\tb\nc\rde');
  });

  it('follows the two levels: prose keeps the joiners and the variation selectors', () => {
    const family = '👨\u200D👩\u200D👧';
    expect(stripHidden(family, 'prose')).toBe(family);
    expect(stripHidden(family, 'strict')).toBe('👨👩👧');
    expect(stripHidden('\u2764\ufe0f', 'prose')).toBe('\u2764\ufe0f');
    expect(stripHidden('\u2764\ufe0f', 'strict')).toBe('\u2764');
  });

  it('agrees with the class for every code point, in both modes', () => {
    // One walk over the whole code space with one assertion at the end: a test that makes an assertion
    // for each of a million code points spends its time on the assertions.
    const wrong: string[] = [];
    for (const mode of MODES) {
      for (let codePoint = 0; codePoint <= 0x10ffff; codePoint++) {
        if (codePoint >= 0xd800 && codePoint <= 0xdfff) continue;
        const text = `a${chr(codePoint)}b`;
        if (stripHidden(text, mode) !== (expectedHidden(codePoint, mode) ? 'ab' : text)) wrong.push(`U+${codePoint.toString(16)} in ${mode}`);
      }
    }
    expect(wrong).toEqual([]);
  });

  it('does not misread lone and neighbouring surrogates', () => {
    expect(stripHidden('\ud800x', 'strict')).toBe('\ud800x');
    expect(stripHidden('\udb40', 'strict')).toBe('\udb40');
    expect(stripHidden('\udb40\udc41ok', 'strict')).toBe('ok');
    expect(stripHidden('\udb43\udfffok', 'strict')).toBe('ok'); // U+E0FFF
    expect(stripHidden('\udb44\udc00ok', 'strict')).toBe('\udb44\udc00ok'); // U+E1000
  });

  it('is linear on a mebibyte, whether it holds hidden characters or not', () => {
    const MIB = 1024 * 1024;
    const time = (run: () => void) => {
      const started = performance.now();
      run();
      return performance.now() - started;
    };
    expect(time(() => expect(stripHidden('\u200B'.repeat(MIB), 'strict')).toBe(''))).toBeLessThan(750);
    expect(time(() => expect(stripHidden('a\u200B'.repeat(MIB / 2), 'strict')).toHaveLength(MIB / 2))).toBeLessThan(750);
    expect(time(() => expect(stripHidden('x'.repeat(MIB), 'strict')).toHaveLength(MIB))).toBeLessThan(250);
  });
});
