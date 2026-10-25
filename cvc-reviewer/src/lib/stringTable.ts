/**
 * A table of the distinct strings of a set (issue #118): each gets a number the first time it is seen, and
 * the same string always gets the same number.
 *
 * It exists for strings that come from the wire (a conversation id, a tool name, a commit hash), which can
 * be a mebibyte long. A `Map` keyed by such strings is quadratic: the engine hashes a string of more than
 * 16,383 characters by its length alone, so a table of strings of one length is one long chain, and every
 * lookup compares the key with each string in it, character by character. Here a string up to
 * `SHORT_STRING` characters goes in a `Map` (the engine hashes those in full); a longer one is filed under
 * a key made of its length and a sample of it (its first and last characters, and some spread over the
 * rest), which takes the same time whatever the length, and is compared in full only against the strings
 * filed under the same key, so ids that differ at the end, or anywhere the sample reaches, are never
 * compared at all. Two different strings always get two numbers: the comparison is in full.
 */

/** A string this short is hashed in full by the engine, and goes in a `Map` as it is. */
export const SHORT_STRING = 256;

const HEAD = 48;
const TAIL = 48;
const SPREAD = 32;

/** The key a long string is filed under: its length and a sample of its characters. */
function sampleKey(text: string): string {
  const length = text.length;
  let key = `${length}\u0000${text.slice(0, HEAD)}\u0000${text.slice(length - TAIL)}\u0000`;
  const step = (length - HEAD - TAIL) / (SPREAD + 1);
  for (let k = 1; k <= SPREAD; k++) key += text[HEAD + Math.floor(k * step)];
  return key;
}

export class StringTable {
  /** The distinct strings, in the order they were first seen: the number of a string is its place here. */
  readonly values: string[] = [];
  private readonly short = new Map<string, number>();
  private readonly long = new Map<string, number[]>();

  /** The number of `text`, or -1 when it is not in the table. */
  indexOf(text: string): number {
    if (text.length <= SHORT_STRING) return this.short.get(text) ?? -1;
    const filed = this.long.get(sampleKey(text));
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
    const key = sampleKey(text);
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
