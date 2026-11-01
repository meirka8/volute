/**
 * The one fold of the search (issue #118): what it does to a code point, to a text, and how `foldWithMap` and
 * `fold` agree. The engine's own copies of Unicode's properties (`White_Space`, case mapping) are the oracles:
 * nothing of the module is used to build an expectation.
 */
import { describe, expect, it } from 'vitest';
import { hasHiddenChars, isHiddenCodePoint } from './hiddenChars';
import { fold, foldCodePoint, foldWithMap, isWhiteSpaceCode, sourceRange } from './searchFold';

const chr = (codePoint: number) => String.fromCodePoint(codePoint);
const label = (codePoint: number) => `U+${codePoint.toString(16).toUpperCase().padStart(4, '0')}`;
/** Every code point of the code space, less the surrogates (no text has one on its own). */
function* codePoints(): Generator<number> {
  for (let codePoint = 0; codePoint <= 0x10ffff; codePoint++) {
    if (codePoint >= 0xd800 && codePoint <= 0xdfff) continue;
    yield codePoint;
  }
}

/**
 * The code points that are assigned, with the members of the hidden class (which the property lists even where nothing is
 * assigned yet): for the walks that fold every one of them, where an unassigned code point has no case and no decomposition
 * and folds to itself, and a walk of a million of them costs more than it can show.
 */
function* interestingCodePoints(): Generator<number> {
  const assigned = /\P{Cn}/u;
  for (const codePoint of codePoints()) if (isHiddenCodePoint(codePoint) || assigned.test(chr(codePoint))) yield codePoint;
}

/**
 * The code points for which `holds` is false, found by checking the code space a chunk of text at a time and
 * each code point of a chunk that fails on its own: a property of a fold that holds of a text if it holds of
 * every code point in it can be walked a thousand times faster than one call per code point.
 */
function failing(holds: (text: string) => boolean, size = 4096): string[] {
  const wrong: string[] = [];
  let chunk: number[] = [];
  const settle = () => {
    if (chunk.length === 0) return;
    if (!holds(chunk.map(chr).join(''))) for (const codePoint of chunk) if (!holds(chr(codePoint))) wrong.push(label(codePoint));
    chunk = [];
  };
  for (const codePoint of interestingCodePoints()) {
    chunk.push(codePoint);
    if (chunk.length === size) settle();
  }
  settle();
  return wrong;
}

/** Deterministic random numbers, so a failure is reproducible. */
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

/** Pieces of text that make the fold work hard: every kind of thing it treats specially, and lone surrogates. */
const ADVERSARIAL = [
  'a', 'B', 'z', 'Z', '0', ' ', '~', '\t', '\n', '\r', '\r\n', '\v', '\f', '\u0085', ' ', ' ', '　', ' ',
  '\u0000', '\u0007', '\u007f', '​', '‌', '‍', '‮', '⁦', '﻿', '­', '️',
  'é', 'é', 'É', 'ñ', 'ǭ', 'ß', 'ẞ', 'ſ', 'ς', 'Σ', 'σ', 'İ', 'ı', 'I', 'i', 'K', 'Ω', 'Å', 'ǅ', 'ǆ', 'Ǆ', 'ŉ', 'ǰ', 'ΐ', 'և',
  'ﬁ', 'ﬃ', 'ﬆ', 'Ａ', 'ａ', '１', '①', '㎏', '™', '½', 'Ⅻ', 'ﷺ', '𝐀', '𝔸',
  '한', '글', 'ᄀ', 'ᅡ', 'こ', 'ん', '中', '文', 'а', 'я', 'א', 'ب',
  '😀', '👨‍👩‍👧', '🏳️‍🌈', '\u{e0041}', '\u{e0100}', '\u{1d173}', '\u{1bca0}',
  '\ud800', '\udc00', '\udbff', '\udfff', '\ud83d', '\ude00',
];

function randomText(random: () => number, maxPieces: number): string {
  let text = '';
  for (let n = Math.floor(random() * maxPieces); n > 0; n--) text += ADVERSARIAL[Math.floor(random() * ADVERSARIAL.length)];
  return text;
}

describe('white space', () => {
  it('is Unicode White_Space, exactly', () => {
    const oracle = /\p{White_Space}/u;
    const wrong: string[] = [];
    for (const codePoint of codePoints()) if (isWhiteSpaceCode(codePoint) !== oracle.test(chr(codePoint))) wrong.push(label(codePoint));
    expect(wrong).toEqual([]);
  });

  it('turns every White_Space character into a space, wherever it is in a word or a phrase', () => {
    const oracle = /\p{White_Space}/u;
    const wrong: string[] = [];
    for (const codePoint of codePoints()) {
      if (!oracle.test(chr(codePoint))) continue;
      if (fold(`retry${chr(codePoint)}limit`) !== 'retry limit') wrong.push(label(codePoint));
      if (foldWithMap(`retry${chr(codePoint)}limit`).text !== 'retry limit') wrong.push(`map ${label(codePoint)}`);
    }
    expect(wrong).toEqual([]);
  });

  it('reads a carriage return followed by a line feed as the one line break the cards draw', () => {
    expect(fold('a\r\nb')).toBe('a b');
    expect(fold('a\rb')).toBe('a b');
    expect(fold('a\nb')).toBe('a b');
    expect(fold('a\n\nb')).toBe('a  b'); // two line breaks are two
    expect(fold('a\r\n\r\nb')).toBe('a  b');
    expect(fold('a\n\rb')).toBe('a  b'); // in this order they are two
    expect(foldWithMap('a\r\nb').text).toBe('a b');
    expect(foldWithMap('a\r\nb')).toMatchObject({ starts: expect.any(Uint32Array) });
    const folded = foldWithMap('a\r\nb');
    expect(Array.from(folded.starts!)).toEqual([0, 1, 3]);
    expect(Array.from(folded.ends!)).toEqual([1, 3, 4]);
  });

  it('treats a no-break space inside a phrase as a space (so the phrase typed with spaces finds it)', () => {
    expect(fold('retry limit')).toBe(fold('retry limit'));
    expect(fold('retry limit')).toBe(fold('retry limit'));
    expect(fold('retry　limit')).toBe(fold('retry limit'));
  });
});

describe('the hidden class', () => {
  it('leaves out every member of it, except the three that are also white space', () => {
    const wrong: string[] = [];
    const bothHiddenAndSpace: number[] = [];
    for (const codePoint of codePoints()) {
      if (!isHiddenCodePoint(codePoint)) continue;
      if (isWhiteSpaceCode(codePoint)) bothHiddenAndSpace.push(codePoint);
      if (fold(`a${chr(codePoint)}b`) !== (isWhiteSpaceCode(codePoint) ? 'a b' : 'ab')) wrong.push(label(codePoint));
    }
    expect(wrong).toEqual([]);
    // Which are those three: vertical tab, form feed and next line, controls that separate words.
    expect(bothHiddenAndSpace.map(label)).toEqual(['U+000B', 'U+000C', 'U+0085']);
  });

  it('keeps pass<ZWSP>word and password the same word, and any one invisible character from splitting a word', () => {
    expect(fold('pass​word')).toBe('password');
    expect(fold('pass‮word⁦')).toBe('password');
    expect(fold(`pass${chr(0xe0041)}${chr(0xe0100)}word`)).toBe('password');
    expect(fold('pass\u{1d173}word')).toBe('password');
    expect(fold('pass⁥word')).toBe('password'); // unassigned, and ignorable by property
  });

  it('agrees with the class on which code points it leaves out', () => {
    const wrong: string[] = [];
    for (const codePoint of codePoints()) {
      if (isWhiteSpaceCode(codePoint) || !hasHiddenChars(chr(codePoint), 'strict')) continue;
      if (foldCodePoint(codePoint) !== '') wrong.push(`${label(codePoint)} hidden but kept`);
    }
    expect(wrong).toEqual([]);
  });
});

describe('case', () => {
  it('folds the case variants the engine knows to the same text, for every code point', () => {
    // The dotless i is the one letter whose upper case is another letter's (the capital I), and Unicode's own
    // folding keeps it apart from the i: it is an exception here as it is there.
    const wrong: string[] = [];
    let tested = 0;
    for (const codePoint of codePoints()) {
      if (codePoint === 0x131) continue;
      const character = chr(codePoint);
      const lower = character.toLowerCase();
      const upper = character.toUpperCase();
      if (lower === character && upper === character) continue;
      tested++;
      const folded = fold(character);
      if (fold(lower) !== folded) wrong.push(`${label(codePoint)} -> ${JSON.stringify(lower)}`);
      if (fold(upper) !== folded) wrong.push(`${label(codePoint)} -> ${JSON.stringify(upper)}`);
    }
    expect(wrong).toEqual([]);
    expect(tested).toBeGreaterThan(2_800); // the engine knows a case variant of a few thousand code points
  });

  it('is full case folding: ß is ss, İ is i and the dot above, ς is σ', () => {
    expect(fold('Straße')).toBe('strasse');
    expect(fold('STRASSE')).toBe('strasse');
    expect(fold('STRAẞE')).toBe('strasse');
    expect(fold('ﬀ')).toBe('ff');
    expect(fold('İstanbul')).toBe('i̇stanbul');
    expect(fold('i̇stanbul')).toBe(fold('İSTANBUL'));
    expect(fold('ŉ')).toBe('ʼn');
    expect(fold('ǰ')).toBe('ǰ');
    expect(fold('և')).toBe('եւ');
  });

  it('gives the final sigma the same fold in the middle of a word, at its end and alone, so a term can start or end anywhere', () => {
    expect(fold('ΟΔΥΣΣΕΥΣ')).toBe('οδυσσευσ');
    expect(fold('οδυσσευς')).toBe('οδυσσευσ');
    expect(fold('οδυσσευσ')).toBe('οδυσσευσ');
    expect(fold('ς')).toBe('σ');
    expect(fold('Σ')).toBe('σ');
    // A term cut out of a word finds it, whichever sigma it ends in.
    expect(fold('ΟΔΥΣΣΕΥΣΑ').includes(fold('οδυσσευς'))).toBe(true);
  });

  it('keeps the dotless i apart from the i, as Unicode does, and folds the long s and the Kelvin sign', () => {
    expect(fold('ı')).toBe('ı');
    expect(fold('ı')).not.toBe(fold('i'));
    expect(fold('ſ')).toBe('s');
    expect(fold('K')).toBe('k');
    expect(fold('Ω')).toBe('ω');
    expect(fold('Å')).toBe(fold('å'));
  });
});

describe('compatibility forms', () => {
  it('folds full-width letters, digits and ligatures to the letters and digits they are', () => {
    expect(fold('ＡＢＣ１２３')).toBe('abc123');
    expect(fold('ａｂｃ')).toBe('abc');
    expect(fold('ﬁnd')).toBe('find');
    expect(fold('ﬃ')).toBe('ffi');
    expect(fold('ﬆ')).toBe('st');
    expect(fold('①')).toBe('1');
    expect(fold('㎏')).toBe('kg');
    expect(fold('Ⅻ')).toBe('xii');
    expect(fold('𝐀𝔸')).toBe('aa');
  });

  it('makes a precomposed accented letter and the letter with its accent after it the same text', () => {
    expect(fold('é')).toBe(fold('é'));
    expect(fold('É')).toBe(fold('é'));
    expect(fold('café')).toBe(fold('café'));
    expect(fold('ñ')).toBe(fold('ñ'));
    expect(fold('ǭ')).toBe(fold('ǭ'.normalize('NFD')));
    // And a term without the accent is found in the text with it (but not the other way round).
    expect(fold('café').includes(fold('cafe'))).toBe(true);
    expect(fold('cafe').includes(fold('café'))).toBe(false);
  });

  it('does not make letters of one script the letters of another that look like them', () => {
    expect(fold('а')).toBe('а'); // Cyrillic a
    expect(fold('а')).not.toBe(fold('a'));
    expect(fold('ο')).not.toBe(fold('o')); // Greek omicron
  });

  it('expands a character into all its letters, and a Hangul syllable into its jamo, without losing any', () => {
    expect(fold('ﷺ')).toHaveLength(18);
    expect(fold('한')).toBe('한'.normalize('NFD'));
  });
});

describe('what it is', () => {
  it('is idempotent on every code point', () => {
    expect(failing((text) => fold(fold(text)) === fold(text))).toEqual([]);
  });

  it('is idempotent on random and hostile text', () => {
    const random = rng(118);
    for (let run = 0; run < 2000; run++) {
      const text = randomText(random, 30);
      const once = fold(text);
      expect(fold(once), JSON.stringify(text)).toBe(once);
    }
  });

  it('leaves nothing in the folded text that is white space other than the space, or hidden', () => {
    const clean = (text: string) => {
      for (const character of fold(text)) {
        const code = character.codePointAt(0)!;
        if ((isWhiteSpaceCode(code) && code !== 0x20) || isHiddenCodePoint(code)) return false;
      }
      return true;
    };
    expect(failing(clean)).toEqual([]);
  });

  it('gives the plain text of printable ASCII the lower case of it, and the same answer whether it takes the fast way or not', () => {
    const random = rng(7);
    const reference = (text: string) => {
      // One code point at a time, as the slow way does, with a carriage return and line feed as one.
      let out = '';
      for (let at = 0; at < text.length; at++) {
        const code = text.charCodeAt(at);
        if (code === 0x0d && text.charCodeAt(at + 1) === 0x0a) at++;
        out += foldCodePoint(code);
      }
      return out;
    };
    const ascii = 'abcXYZ019 ~!@#$%^&*()_+-=[]{};:\'",.<>/?\\|`\t\n\r\v\f\u0000\u0001\u007f';
    for (let run = 0; run < 3000; run++) {
      let text = '';
      for (let n = Math.floor(random() * 60); n > 0; n--) text += ascii[Math.floor(random() * ascii.length)];
      expect(fold(text), JSON.stringify(text)).toBe(reference(text));
      expect(foldWithMap(text).text, JSON.stringify(text)).toBe(reference(text));
    }
    expect(fold('')).toBe('');
    expect(fold('Hello, World')).toBe('hello, world');
  });

  it('turns text that is not text (a lone surrogate) into itself, and never fails', () => {
    for (const text of ['\ud800', '\udc00', 'a\ud800b', '\udbff\udbff', '\ud83d', '\ude00x', 'x\ud83d']) {
      expect(() => fold(text)).not.toThrow();
      expect(fold(text)).toBe(text.toLowerCase());
      expect(foldWithMap(text).text).toBe(fold(text));
    }
  });
});

describe('foldWithMap', () => {
  it('gives, for every code point, the text fold gives', () => {
    expect(failing((text) => foldWithMap(text).text === fold(text))).toEqual([]);
    // And for each code point of the part of the code space where the scripts are, on its own.
    const wrong: string[] = [];
    for (let codePoint = 0; codePoint < 0x3000; codePoint++) {
      if (codePoint >= 0xd800 && codePoint <= 0xdfff) continue;
      if (foldWithMap(chr(codePoint)).text !== fold(chr(codePoint))) wrong.push(label(codePoint));
    }
    expect(wrong).toEqual([]);
  });

  it('gives, for random and hostile text, the text fold gives, and a map that is valid', () => {
    const random = rng(2026);
    // Mismatches are collected and asserted once: a million assertions would cost more than the walk.
    const wrong: string[] = [];
    for (let run = 0; run < 4000; run++) {
      const text = randomText(random, 40);
      const folded = foldWithMap(text);
      const shown = JSON.stringify(text);
      if (folded.text !== fold(text)) wrong.push(`${shown}: text`);
      if (folded.starts === null || folded.ends === null) {
        // No map: the folded text has a unit for every unit of the text, in its place.
        if (folded.text.length !== text.length) wrong.push(`${shown}: no map but ${folded.text.length} units for ${text.length}`);
        continue;
      }
      if (folded.starts.length !== folded.text.length || folded.ends.length !== folded.text.length) wrong.push(`${shown}: map length`);
      let previous = 0;
      for (let k = 0; k < folded.text.length; k++) {
        const start = folded.starts[k];
        const end = folded.ends[k];
        if (start < previous) wrong.push(`${shown}: out of order at ${k}`);
        if (!(end > start && end <= text.length)) wrong.push(`${shown}: bad end at ${k}`);
        // Whole characters: a boundary is never in the middle of a surrogate pair.
        for (const boundary of [start, end]) {
          if (boundary > 0 && boundary < text.length) {
            const before = text.charCodeAt(boundary - 1);
            const after = text.charCodeAt(boundary);
            if (before >= 0xd800 && before <= 0xdbff && after >= 0xdc00 && after <= 0xdfff) wrong.push(`${shown}: splits a pair at ${boundary}`);
          }
        }
        previous = start;
      }
    }
    expect(wrong).toEqual([]);
  });

  it('agrees with fold on long text of every kind', () => {
    const random = rng(99);
    for (let run = 0; run < 20; run++) {
      let text = '';
      while (text.length < 20_000) text += ADVERSARIAL[Math.floor(random() * ADVERSARIAL.length)] + (random() < 0.7 ? 'lorem ipsum dolor ' : '');
      expect(foldWithMap(text).text).toBe(fold(text));
    }
  });

  it('needs no map for printable ASCII, tabs and line feeds, and is the text itself in lower case', () => {
    const folded = foldWithMap('Hello\tWorld\nNext Line');
    expect(folded).toEqual({ text: 'hello world next line', starts: null, ends: null });
    expect(sourceRange(folded, 2, 7)).toEqual({ start: 2, end: 7 });
  });

  it('says where each unit came from: a mark covers whole characters and the hidden ones inside the match', () => {
    // pass<ZWSP>word: the match "password" covers the hidden character between the halves.
    const hidden = foldWithMap('x pass​word y');
    expect(hidden.text).toBe('x password y');
    expect(sourceRange(hidden, 2, 10)).toEqual({ start: 2, end: 11 });
    // A match that ends before a hidden character does not take it in, nor one that starts after it.
    const edges = foldWithMap('ab​cd');
    expect(edges.text).toBe('abcd');
    expect(sourceRange(edges, 0, 2)).toEqual({ start: 0, end: 2 });
    expect(sourceRange(edges, 2, 4)).toEqual({ start: 3, end: 5 });
    expect(sourceRange(edges, 1, 3)).toEqual({ start: 1, end: 4 });
    // A precomposed letter is two folded units and one source character: either one marks the whole letter.
    const accent = foldWithMap('café!');
    expect(accent.text).toBe('café!');
    expect(sourceRange(accent, 0, 4)).toEqual({ start: 0, end: 4 }); // "cafe" is the whole of "café"
    expect(sourceRange(accent, 3, 5)).toEqual({ start: 3, end: 4 });
    // ß is two units, İ is two units, a ligature is as many as it has letters.
    expect(sourceRange(foldWithMap('Straße'), 4, 6)).toEqual({ start: 4, end: 5 });
    expect(sourceRange(foldWithMap('ﬃ x'), 1, 2)).toEqual({ start: 0, end: 1 });
    // An astral character is one source character of two units.
    const emoji = foldWithMap('a😀b');
    expect(emoji.text).toBe('a😀b');
    expect(sourceRange(emoji, 1, 3)).toEqual({ start: 1, end: 3 });
  });

  it('marks an emoji sequence and a word with a non-joiner whole, as a person who types them as drawn expects', () => {
    const family = '👨‍👩‍👧';
    const sequence = foldWithMap(`see ${family}!`);
    expect(sequence.text).toBe('see 👨👩👧!');
    const at = sequence.text.indexOf('👨👩👧');
    expect(sourceRange(sequence, at, at + '👨👩👧'.length)).toEqual({ start: 4, end: 4 + family.length });
    const persian = 'می‌خواهم';
    const word = foldWithMap(persian);
    expect(sourceRange(word, 0, word.text.length)).toEqual({ start: 0, end: persian.length });
  });
});

describe('cost', () => {
  const time = (run: () => void) => {
    const started = performance.now();
    run();
    return performance.now() - started;
  };
  const MIB = 1024 * 1024;

  it('is the engine\'s own speed on printable ASCII with line breaks (the usual text)', () => {
    const text = 'The Quick brown fox jumps over the lazy dog.\n'.repeat(Math.ceil((8 * MIB) / 45));
    expect(time(() => fold(text))).toBeLessThan(500);
    expect(time(() => foldWithMap(text))).toBeLessThan(500);
  });

  it('is linear on a mebibyte of text in many scripts, with a map', () => {
    const text = 'こんにちは世界 é ü ß 😀 한글 Ａ ﬁ ​   İ ς '.repeat(Math.ceil(MIB / 40)).slice(0, MIB);
    expect(time(() => fold(text))).toBeLessThan(1_500);
    expect(time(() => foldWithMap(text))).toBeLessThan(2_500);
  });

  it('is linear on a mebibyte of one character that expands into eighteen', () => {
    const text = 'ﷺ'.repeat(MIB / 2);
    let folded = '';
    expect(time(() => (folded = fold(text)))).toBeLessThan(2_000);
    expect(folded).toHaveLength((MIB / 2) * 18);
  });

  it('is linear on a mebibyte of nothing but hidden characters, and of nothing but line breaks', () => {
    expect(time(() => expect(fold('​'.repeat(MIB))).toBe(''))).toBeLessThan(1_000);
    expect(time(() => expect(fold('\r\n'.repeat(MIB / 2))).toHaveLength(MIB / 2))).toBeLessThan(1_000);
    expect(time(() => expect(foldWithMap('​'.repeat(MIB)).text).toBe(''))).toBeLessThan(1_500);
  });

  it('asks the engine about each distinct code point once, however long the text', () => {
    // Every BMP code point, many times over: the cache holds the answers.
    let text = '';
    for (let codePoint = 0x4e00; codePoint < 0x9fff; codePoint++) text += chr(codePoint);
    const big = text.repeat(40);
    expect(time(() => fold(big))).toBeLessThan(1_500);
  });
});
