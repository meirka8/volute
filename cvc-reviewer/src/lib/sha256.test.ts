import { describe, expect, it } from 'vitest';
import { Sha256, asciiBytes, sha256, toHex } from './sha256';

/** The platform's own SHA-256, which the one under test is held against. */
async function reference(bytes: Uint8Array): Promise<string> {
  return toHex(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes as unknown as BufferSource)));
}

/** Deterministic bytes of any length, so that a failure names its input. */
function noise(length: number, seed: number): Uint8Array {
  const bytes = new Uint8Array(length);
  let state = seed >>> 0 || 1;
  for (let i = 0; i < length; i++) {
    state ^= state << 13;
    state >>>= 0;
    state ^= state >>> 17;
    state ^= state << 5;
    state >>>= 0;
    bytes[i] = state & 0xff;
  }
  return bytes;
}

describe('sha256: the NIST vectors (FIPS 180-4 examples)', () => {
  it.each([
    ['the empty message', '', 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'],
    ['abc', 'abc', 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'],
    [
      'the 448-bit message',
      'abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq',
      '248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1',
    ],
    [
      'the 896-bit message',
      'abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu',
      'cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1',
    ],
    [
      'a million letters a',
      'a'.repeat(1_000_000),
      'cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0',
    ],
  ])('%s', (_name, message, digest) => {
    expect(toHex(sha256(asciiBytes(message)))).toBe(digest);
  });
});

describe('sha256: agrees with the platform', () => {
  it('on every length from 0 to 200 bytes, around each padding boundary', async () => {
    for (let length = 0; length <= 200; length++) {
      const bytes = noise(length, length + 7);
      expect(toHex(sha256(bytes)), `${length} bytes`).toBe(await reference(bytes));
    }
  });

  it('on larger inputs, including lengths that are not a multiple of anything', async () => {
    for (const length of [1_000, 4_095, 4_096, 65_537, 1_048_576 + 13]) {
      const bytes = noise(length, length);
      expect(toHex(sha256(bytes)), `${length} bytes`).toBe(await reference(bytes));
    }
  });

  it('on 200 random messages, however they are cut into updates', async () => {
    for (let round = 0; round < 200; round++) {
      const bytes = noise((round * 37) % 300, round + 1);
      const hash = new Sha256();
      // Cut at pseudo-random places, including empty updates.
      const cuts = noise(6, round + 99);
      let at = 0;
      for (const cut of cuts) {
        const next = Math.min(bytes.length, at + (cut % 90));
        hash.update(bytes.subarray(at, next));
        at = next;
      }
      hash.update(bytes.subarray(at));
      expect(toHex(hash.digest()), `round ${round}`).toBe(await reference(bytes));
    }
  });

  it('when the message is given one byte at a time', async () => {
    const bytes = noise(260, 5);
    const hash = new Sha256();
    for (const byte of bytes) hash.update(Uint8Array.of(byte));
    expect(toHex(hash.digest())).toBe(await reference(bytes));
  });
});

describe('Sha256.clone', () => {
  it('continues from the prefix without disturbing it, whatever the prefix length', async () => {
    for (const prefixLength of [0, 1, 55, 56, 63, 64, 65, 119, 120, 128, 1000]) {
      const prefix = noise(prefixLength, prefixLength + 3);
      const base = new Sha256().update(prefix);
      for (const tail of [new Uint8Array(0), Uint8Array.of(1, 2, 3, 4), noise(70, 8)]) {
        const joined = new Uint8Array(prefix.length + tail.length);
        joined.set(prefix);
        joined.set(tail, prefix.length);
        expect(toHex(base.clone().update(tail).digest()), `${prefixLength}+${tail.length}`).toBe(await reference(joined));
      }
      // The prefix alone still hashes as itself after all of those.
      expect(toHex(base.digest()), `${prefixLength} alone`).toBe(await reference(prefix));
    }
  });

  it('makes two hashes that do not share state', () => {
    const base = new Sha256().update(asciiBytes('ab'));
    const copy = base.clone();
    copy.update(asciiBytes('c'));
    expect(toHex(copy.digest())).toBe('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
    base.update(asciiBytes('d'));
    expect(toHex(base.digest())).toBe(toHex(sha256(asciiBytes('abd'))));
  });
});

describe('asciiBytes and toHex', () => {
  it('encodes ASCII one byte per character and refuses anything else', () => {
    expect(Array.from(asciiBytes('A~\u0000\u007f'))).toEqual([65, 126, 0, 127]);
    expect(() => asciiBytes('\u0080')).toThrow(RangeError);
    expect(() => asciiBytes('é')).toThrow(RangeError);
    expect(() => asciiBytes('\u{1F642}')).toThrow(RangeError);
  });

  it('writes two lowercase hex digits per byte', () => {
    expect(toHex(Uint8Array.of(0, 1, 15, 16, 171, 255))).toBe('00010f10abff');
    expect(toHex(new Uint8Array(0))).toBe('');
  });
});
