/// <reference types="node" />
import { describe, expect, it } from 'vitest';
import { REASONING_BOUNDARY_CANDIDATES, readReasoning, reasoningBoundary } from './reasoningMarker';

interface Vector {
  name: string;
  id: string;
  text: string;
  kind: 'provider_summary' | 'unverified';
  body: string;
}

// The vectors are shared with cvc-core (reasoning.rs `read`), and made by an independent port of the
// algorithm (cvc-reviewer/scripts/reasoning-marker-vectors.mjs).
// vitest runs from cvc-reviewer/; vite's own file loader may not read outside the app, node's can.
const { readFileSync } = await import('node:fs');
const fixture = JSON.parse(readFileSync('../cvc-core/tests/fixtures/reasoning-marker/v1.json', 'utf8')) as { version: number; vectors: Vector[] };

const A = '6b1f0c0e-1111-4aaa-8bbb-000000000001';
const WORDS = "provider summary of the model's reasoning";

describe('the shared vectors', () => {
  it('are exactly what the independent generator writes (its --check)', async () => {
    const { execFileSync } = await import('node:child_process');
    // Throws, with the script's message, when the committed file differs from what it would write.
    execFileSync(process.execPath, ['scripts/reasoning-marker-vectors.mjs', '--check'], { stdio: 'pipe' });
  });

  it('are the version this reader knows, and cover both outcomes', () => {
    expect(fixture.version).toBe(1);
    expect(fixture.vectors.length).toBeGreaterThanOrEqual(30);
    expect(fixture.vectors.some((v) => v.kind === 'provider_summary')).toBe(true);
    expect(fixture.vectors.some((v) => v.kind === 'unverified')).toBe(true);
  });

  for (const vector of fixture.vectors) {
    it(`reads: ${vector.name}`, () => {
      const reading = readReasoning(vector.id, vector.text);
      expect(reading.kind).toBe(vector.kind);
      expect(reading.kind === 'unverified' ? reading.text : reading.body).toBe(vector.body);
    });
  }
});

describe('the boundary', () => {
  it('is hashed as the producer lays it out (pinned against the first shared vector)', () => {
    const first = fixture.vectors[0];
    const token = /~cvc-[0-9a-f]{16}/.exec(first.text)![0];
    expect(reasoningBoundary(first.id, 'provider_summary', 0)).toBe(token);
  });

  it('depends on the id, the kind and the candidate', () => {
    const base = reasoningBoundary(A, 'provider_summary', 3);
    expect(base).toMatch(/^~cvc-[0-9a-f]{16}$/);
    expect(reasoningBoundary(A, 'exposed', 3)).not.toBe(base);
    expect(reasoningBoundary(A, 'provider_summary', 4)).not.toBe(base);
    expect(reasoningBoundary('6b1f0c0e-1111-4aaa-8bbb-000000000002', 'provider_summary', 3)).not.toBe(base);
  });

  it('tries 64 candidates', () => {
    expect(REASONING_BOUNDARY_CANDIDATES).toBe(64);
  });
});

describe('readReasoning', () => {
  const marked = (body: string, c = 0) => `[cvc: ${WORDS} ${reasoningBoundary(A, 'provider_summary', c)}]\n\n${body}`;

  it('strips the marker of a verified summary and keeps the body byte for byte', () => {
    expect(readReasoning(A, marked('  keeps\n  its   spacing\n'))).toEqual({ kind: 'provider_summary', body: '  keeps\n  its   spacing\n' });
  });

  it('shows every other text literally', () => {
    for (const text of ['thinking', '', marked(''), marked('x').replace('\n\n', '\n')]) {
      expect(readReasoning(A, text)).toEqual({ kind: 'unverified', text });
    }
  });

  it('cannot verify for an id that is not an interaction id, and does not throw', () => {
    for (const id of ['', 'not-an-id', 'codex:thread-1', A.slice(1), `${A}0`]) {
      expect(readReasoning(id, marked('summary')).kind).toBe('unverified');
    }
  });

  it('does not verify a text whose body carries the token again, as a second marker of the same id does', () => {
    const text = `${marked('one')}\n\n${marked('two')}`;
    expect(readReasoning(A, text)).toEqual({ kind: 'unverified', text });
  });

  it('is linear on a hostile text', () => {
    const long = 'x'.repeat(2_000_000);
    const started = performance.now();
    expect(readReasoning(A, marked(long))).toEqual({ kind: 'provider_summary', body: long });
    expect(readReasoning(A, `[cvc: ${WORDS} ~cvc-`.repeat(50_000)).kind).toBe('unverified');
    expect(performance.now() - started).toBeLessThan(1000);
  });
});
