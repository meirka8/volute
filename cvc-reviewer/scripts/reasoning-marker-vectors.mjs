// Generates cvc-core/tests/fixtures/reasoning-marker/v1.json: the shared vectors for the reasoning marker
// (cvc-core/src/reasoning.rs `read`, cvc-reviewer/src/lib/reasoningMarker.ts `readReasoning`).
//
// This is a THIRD, independent port of the algorithm (plain node, node:crypto, no Reviewer code), written
// from the specification in reasoning.rs, so that the vectors are not produced by the code they check.
//
//   node cvc-reviewer/scripts/reasoning-marker-vectors.mjs          # rewrites the fixture
//   node cvc-reviewer/scripts/reasoning-marker-vectors.mjs --check  # fails if the fixture differs
//
// Algorithm:
//   token(id, kind, c) = "~cvc-" + hex( SHA-256( "cvc.reasoning-boundary/v1" 0x00 id 0x00 kind 0x00 be32(c) )[0..8] )
//   id: 36 lowercase ASCII characters; kind: "provider_summary"; c in 0..64 (the writer takes the smallest c
//   whose token is not in the body).
//   marked = "[cvc: provider summary of the model's reasoning " token "]" "\n\n" body
//   read: the marker must open the text exactly; the token is 21 chars (`~cvc-` and 16 lowercase hex); body is
//   not blank (Unicode White_Space, as Rust's str::trim) and does not contain the token; the token equals
//   token(id, kind, c) for some c in 0..64. Otherwise "unverified" and the body is the whole text.

import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const CANDIDATES = 64;
const WORDS = "provider summary of the model's reasoning";

function token(id, kind, c) {
  const be = Buffer.alloc(4);
  be.writeUInt32BE(c);
  const input = Buffer.concat([
    Buffer.from('cvc.reasoning-boundary/v1\0', 'latin1'),
    Buffer.from(id.toLowerCase(), 'latin1'),
    Buffer.from([0]),
    Buffer.from(kind, 'latin1'),
    Buffer.from([0]),
    be,
  ]);
  return '~cvc-' + createHash('sha256').update(input).digest().subarray(0, 8).toString('hex');
}

const marker = (tok) => `[cvc: ${WORDS} ${tok}]\n\n`;

// The writer: the smallest free candidate, or candidate 0 when every one is taken.
function mark(id, body) {
  const free = [...Array(CANDIDATES).keys()].find((c) => !body.includes(token(id, 'provider_summary', c)));
  return marker(token(id, 'provider_summary', free ?? 0)) + body;
}

const WHITESPACE = /^\p{White_Space}*$/u; // the Unicode White_Space property, as Rust's str::trim

function read(id, text) {
  const open = `[cvc: ${WORDS} `;
  if (text.startsWith(open)) {
    const rest = text.slice(open.length);
    const tok = rest.slice(0, 21);
    if (/^~cvc-[0-9a-f]{16}$/.test(tok) && rest.slice(21).startsWith(']\n\n')) {
      const body = rest.slice(24);
      if (!WHITESPACE.test(body) && !body.includes(tok)) {
        for (let c = 0; c < CANDIDATES; c++) {
          if (token(id, 'provider_summary', c) === tok) return { kind: 'provider_summary', body };
        }
      }
    }
  }
  return { kind: 'unverified', body: text };
}

const A = '6b1f0c0e-1111-4aaa-8bbb-000000000001';
const B = '6b1f0c0e-1111-4aaa-8bbb-000000000002';
const C = '0194f3a2-7c10-8e5d-9a3b-5d2c4e6f8a10';
const tA = (c) => token(A, 'provider_summary', c);

const cases = [];
const add = (name, id, text) => cases.push({ name, id, text, ...read(id, text) });

add('a marked summary', A, mark(A, 'Looking at the loop'));
add('a marked summary of another id', B, mark(B, 'Checking whether the retry is bounded.'));
add('a marked summary of a version 8 id', C, mark(C, 'Plan: read the config, then the loader.'));
add('a multi-line, non-ASCII body', A, mark(A, 'First line\n\nSecond line with é and 日本語 and an emoji \u{1F600}'));
add('a body that looks like a note is text', A, mark(A, '[cvc: withheld 12 bytes that could not be scrubbed]'));
add('the writer skips a token already in the body', A, mark(A, `mentions ${tA(0)} once`));
add('the writer skips two tokens already in the body', A, mark(A, `${tA(0)} and ${tA(1)}`));
add('an upper-case id is the same id', A.toUpperCase(), mark(A, 'Looking at the loop'));
add('the last candidate (63) verifies', A, marker(tA(63)) + 'summary');
add('a candidate past the range (64) does not verify', A, marker(tA(64)) + 'summary');
add('a made-up token', A, `${marker('~cvc-0123456789abcdef')}summary`);
add('the marker of another interaction', A, mark(B, 'summary'));
add('the token again in the body', A, `${mark(A, 'summary')} ${tA(0)}`);
add('a blank body: nothing', A, marker(tA(0)));
add('a blank body: spaces and a newline', A, `${marker(tA(0))} \n`);
add('a blank body: a no-break space and an ideographic space (white space in Rust)', A, `${marker(tA(0))} 　`);
add('a blank body: a next-line character (white space in Rust, not in JavaScript trim)', A, `${marker(tA(0))}\u0085`);
add('a body of a byte order mark is not blank (not white space in Rust)', A, `${marker(tA(0))}﻿`);
add('text that imitates the marker: not at the start', A, ` ${mark(A, 'summary')}`);
add('text that imitates the marker: after other text', A, `x\n\n${mark(A, 'summary')}`);
add('text that imitates the marker: upper-case hex', A, mark(A, 'summary').replace(tA(0), tA(0).toUpperCase().replace('~CVC-', '~cvc-')));
add('text that imitates the marker: a single newline', A, mark(A, 'summary').replace(']\n\n', ']\n'));
add('text that imitates the marker: other words', A, mark(A, 'summary').replace('provider summary', 'provider-summary'));
add('text that imitates the marker: no token', A, `[cvc: ${WORDS}]\n\nsummary`);
add('text that imitates the marker: a short token', A, `[cvc: ${WORDS} ~cvc-0123]\n\nsummary`);
add('text that imitates the marker: no space before the token', A, `[cvc: ${WORDS}${tA(0)}]\n\nsummary`);
add('text that imitates the marker: a plain sentence', A, "Here is a provider summary of the model's reasoning: it looked at the loop.");
add('text that imitates the marker: a full marker line with no separator', A, `[cvc: ${WORDS} ${tA(0)}]summary`);

// Every candidate taken: the writer marks with candidate 0 anyway, and the marker does not verify.
const crowded = [...Array(CANDIDATES).keys()].map((c) => tA(c)).join(' ');
add('every candidate taken', A, mark(A, crowded));

add('exposed reasoning', A, 'The function trims one optional newline before splitting on the first colon.');
add('exposed reasoning: empty', A, '');
add('exposed reasoning: only white space', A, '  \n');
add('a withheld note', A, '[cvc: withheld 7690 bytes that could not be scrubbed]');
add('a withheld note: zero bytes', A, '[cvc: withheld 0 bytes that could not be scrubbed]');

const fixture = {
  version: 1,
  description:
    'Shared vectors for the reasoning marker: cvc-core/src/reasoning.rs read() and cvc-reviewer/src/lib/reasoningMarker.ts readReasoning() must agree on each. Generated by cvc-reviewer/scripts/reasoning-marker-vectors.mjs; do not edit by hand. For each vector, read(id, text) is "provider_summary" with the body after the marker, or "unverified", whose body is the whole text (what ReasoningView::body() returns). A withheld note is not a marker: it reads "unverified"; readers that label it "Withheld" do so on top of read().',
  domain: 'cvc.reasoning-boundary/v1',
  vectors: cases,
};

const here = dirname(fileURLToPath(import.meta.url));
const target = resolve(here, '../../cvc-core/tests/fixtures/reasoning-marker/v1.json');
const json = JSON.stringify(fixture, null, 2) + '\n';
if (process.argv.includes('--check')) {
  if (readFileSync(target, 'utf8') !== json) {
    console.error(`${target} is out of date: run this script without --check`);
    process.exit(1);
  }
} else {
  writeFileSync(target, json);
  console.log(`wrote ${cases.length} vectors to ${target}`);
}
