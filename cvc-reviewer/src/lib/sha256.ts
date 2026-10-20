/**
 * SHA-256 (FIPS 180-4) in plain TypeScript: synchronous, no dependency, and the same in a
 * browser, in a worker and under jsdom.
 *
 * Why this exists. The Reviewer verifies the boundary token of a version 2 stimulus (see
 * `stimulusV2.ts`) while it draws a step: a step's parts are worked out in a `useMemo`, and a long
 * timeline mounts only the cards near the viewport. `crypto.subtle.digest` is asynchronous, so using
 * it there would mean a state machine per card (a flash of the unverified reading, cancellation,
 * double effects), or checking every node of a pull request before the first one is drawn, and it
 * exists only in a secure context. The token is a consistency check over public values, not a
 * signature, so nothing here defends against a timing attack and nothing secret is hashed; what
 * matters is that the digest is right, and the tests pin that against the NIST vectors, the
 * producer's own vectors and the platform's implementation on random input.
 *
 * `Sha256` is incremental and can be copied (`clone`), which is what keeps the verification linear:
 * the common prefix of the 64 candidate inputs (the domain, the id and the shape, which can be
 * hundreds of kilobytes in a crafted stimulus) is hashed once, and each candidate costs the final
 * one or two blocks.
 */

/** The round constants: the first 32 bits of the fractional parts of the cube roots of the first 64 primes. */
const K = new Int32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

/** The initial hash value: the first 32 bits of the fractional parts of the square roots of the first 8 primes. */
const INITIAL = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

const BLOCK_BYTES = 64;

/**
 * The message schedule. One buffer for every hash: `compress` never yields, so two calls can never
 * interleave and it is rewritten in full on each one.
 */
const SCHEDULE = new Int32Array(64);

/** Mixes one 64-byte block, starting at `offset` in `data`, into the eight-word `state`. */
function compress(state: Int32Array, data: Uint8Array, offset: number): void {
  const w = SCHEDULE;
  for (let i = 0; i < 16; i++) {
    const at = offset + i * 4;
    w[i] = (data[at] << 24) | (data[at + 1] << 16) | (data[at + 2] << 8) | data[at + 3];
  }
  for (let i = 16; i < 64; i++) {
    const x = w[i - 15];
    const y = w[i - 2];
    const s0 = ((x >>> 7) | (x << 25)) ^ ((x >>> 18) | (x << 14)) ^ (x >>> 3);
    const s1 = ((y >>> 17) | (y << 15)) ^ ((y >>> 19) | (y << 13)) ^ (y >>> 10);
    w[i] = (w[i - 16] + s0 + w[i - 7] + s1) | 0;
  }

  let a = state[0];
  let b = state[1];
  let c = state[2];
  let d = state[3];
  let e = state[4];
  let f = state[5];
  let g = state[6];
  let h = state[7];
  for (let i = 0; i < 64; i++) {
    const sigma1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
    const choose = (e & f) ^ (~e & g);
    const t1 = (h + sigma1 + choose + K[i] + w[i]) | 0;
    const sigma0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
    const majority = (a & b) ^ (a & c) ^ (b & c);
    const t2 = (sigma0 + majority) | 0;
    h = g;
    g = f;
    f = e;
    e = (d + t1) | 0;
    d = c;
    c = b;
    b = a;
    a = (t1 + t2) | 0;
  }

  state[0] = (state[0] + a) | 0;
  state[1] = (state[1] + b) | 0;
  state[2] = (state[2] + c) | 0;
  state[3] = (state[3] + d) | 0;
  state[4] = (state[4] + e) | 0;
  state[5] = (state[5] + f) | 0;
  state[6] = (state[6] + g) | 0;
  state[7] = (state[7] + h) | 0;
}

/**
 * An incremental SHA-256. `update` any number of times, then `digest` once; `clone` first to take
 * the digest of several continuations of one prefix without hashing the prefix again.
 */
export class Sha256 {
  private readonly state = new Int32Array(INITIAL);
  private readonly buffer = new Uint8Array(BLOCK_BYTES);
  /** How many bytes of `buffer` hold input not yet mixed in. */
  private buffered = 0;
  /** How many bytes have been hashed so far. */
  private total = 0;

  /** Adds `data` to the message. Returns this hash, so calls chain. */
  update(data: Uint8Array): this {
    const length = data.length;
    this.total += length;
    let offset = 0;
    if (this.buffered > 0) {
      const take = Math.min(BLOCK_BYTES - this.buffered, length);
      this.buffer.set(data.subarray(0, take), this.buffered);
      this.buffered += take;
      offset = take;
      if (this.buffered === BLOCK_BYTES) {
        compress(this.state, this.buffer, 0);
        this.buffered = 0;
      }
    }
    for (; offset + BLOCK_BYTES <= length; offset += BLOCK_BYTES) compress(this.state, data, offset);
    if (offset < length) {
      this.buffer.set(data.subarray(offset), 0);
      this.buffered = length - offset;
    }
    return this;
  }

  /** A hash with the same input so far, which goes on independently of this one. */
  clone(): Sha256 {
    const copy = new Sha256();
    copy.state.set(this.state);
    copy.buffer.set(this.buffer);
    copy.buffered = this.buffered;
    copy.total = this.total;
    return copy;
  }

  /** The 32-byte digest of everything added. Ends this hash: `clone` it first to keep going. */
  digest(): Uint8Array {
    const { buffer, state } = this;
    // The length in bits, as two words. `total` stays below 2^53, so the split is exact.
    const high = Math.floor(this.total / 0x20000000);
    const low = ((this.total % 0x20000000) * 8) >>> 0;

    buffer[this.buffered++] = 0x80;
    if (this.buffered > BLOCK_BYTES - 8) {
      buffer.fill(0, this.buffered);
      compress(state, buffer, 0);
      this.buffered = 0;
    }
    buffer.fill(0, this.buffered, BLOCK_BYTES - 8);
    for (let i = 0; i < 4; i++) {
      buffer[BLOCK_BYTES - 8 + i] = high >>> (24 - i * 8);
      buffer[BLOCK_BYTES - 4 + i] = low >>> (24 - i * 8);
    }
    compress(state, buffer, 0);
    this.buffered = 0;

    const out = new Uint8Array(32);
    for (let i = 0; i < 8; i++) {
      for (let j = 0; j < 4; j++) out[i * 4 + j] = state[i] >>> (24 - j * 8);
    }
    return out;
  }
}

/** The SHA-256 digest of `data`. */
export function sha256(data: Uint8Array): Uint8Array {
  return new Sha256().update(data).digest();
}

/** The bytes of `text`, which must be ASCII (every character below U+0080). Anything else is a mistake of the caller's. */
export function asciiBytes(text: string): Uint8Array {
  const bytes = new Uint8Array(text.length);
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    if (code > 0x7f) throw new RangeError('asciiBytes: the text is not ASCII');
    bytes[i] = code;
  }
  return bytes;
}

/** Lowercase hexadecimal, two digits per byte. */
export function toHex(bytes: Uint8Array): string {
  let hex = '';
  for (let i = 0; i < bytes.length; i++) hex += (bytes[i] < 16 ? '0' : '') + bytes[i].toString(16);
  return hex;
}
