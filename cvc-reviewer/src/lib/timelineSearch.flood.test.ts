/**
 * Strings from the wire that are chosen to be slow (issue #118, revision round 4): the table of conversation ids, tool
 * names and commit hashes, and the names made from the ids, must take time in proportion to the size of what they are
 * given, whichever way the strings are made.
 *
 * The first version of the table filed a long string under its length and a sample of its characters (the first and last 48
 * and 32 spread over the rest), which anyone who writes a capture can see: strings that are the same at those places went in
 * one bucket and were compared with each other in full, so 4,000 ids of 16 KiB took 8 seconds to index and 8 more to name,
 * and 40,000 tool names of 300 characters took 11. Every string here is the same as the others everywhere that sample
 * looked, and differs at places it did not look, as late in the string as they go.
 *
 * Each shape is held to a second in jsdom, and to a doubling check: twice the input costs less than three times the time (a
 * quadratic one costs four times, a linear one two), and a few tens of milliseconds more, for a scheduler that is not ours.
 */
import { describe, expect, it, vi } from 'vitest';
import { buildFacets, getTimelineIndex, LABEL_LIMIT, NO_FILTERS, parseQuery, runSearch, type Filters } from './timelineSearch';
import { StringTable } from './stringTable';
import { generatedLink, makeNode, wireToolCall } from '../test/fixtures';
import type { InteractionNode } from '../types/cvc';

/** What a measured time may exceed its share by, in milliseconds: the timer and the scheduler are not ours. */
const SLACK = 25;
// Several of these take a second or two on a machine with nothing else to do, and the default of five seconds is not enough for one that has.
vi.setConfig({ testTimeout: 30_000 });

const time = (run: () => void) => {
  const started = performance.now();
  run();
  return performance.now() - started;
};
const only = (partial: Partial<Filters>): Filters => ({ ...NO_FILTERS, ...partial });

/**
 * How long `act` takes on what `prepare` makes, the fastest of a few runs (noise only ever makes a run slower), with the
 * making of the input left out.
 */
function fastest<T>(prepare: () => T, act: (input: T) => void, times = 3): number {
  return Math.min(
    ...Array.from({ length: times }, () => {
      const input = prepare();
      return time(() => act(input));
    }),
  );
}

/**
 * The fastest run of `act` on a small input and on one twice as large, measured in turn, round after round: a load that comes
 * and goes (another test run on the same machine) is then in both, and what is compared is what the two cost.
 */
function fastestOfTwo<T>(small: () => T, large: () => T, act: (input: T) => void, rounds = 5): [number, number] {
  let fastestSmall = Infinity;
  let fastestLarge = Infinity;
  for (let round = 0; round < rounds; round++) {
    const smallInput = small();
    fastestSmall = Math.min(fastestSmall, time(() => act(smallInput)));
    const largeInput = large();
    fastestLarge = Math.min(fastestLarge, time(() => act(largeInput)));
  }
  return [fastestSmall, fastestLarge];
}

/** The places the first version of the table looked at in a string of `length` characters. */
function oldSample(length: number): Set<number> {
  const at = new Set<number>();
  for (let k = 0; k < 48; k++) {
    at.add(k);
    at.add(length - 1 - k);
  }
  const step = (length - 96) / 33;
  for (let k = 1; k <= 32; k++) at.add(48 + Math.floor(k * step));
  return at;
}

/**
 * `count` strings of `length` characters, all `filler` but for `digits` places that say which one it is (base 36), and those
 * are places the first version of the table did not look at, the last ones before the tail it read.
 */
function variants(count: number, length: number, digits: number, filler: string): string[] {
  if (36 ** digits < count) throw new Error('too few digits for that many strings');
  const looked = oldSample(length);
  const free: number[] = [];
  for (let at = 48; at < length - 48; at++) if (!looked.has(at)) free.push(at);
  // The places in the order they come in the string; the first of them holds the most significant digit.
  const places = Array.from({ length: digits }, (_, d) => free[free.length - 1 - 5 * d]).reverse();
  const base = filler.repeat(Math.ceil(length / filler.length)).slice(0, length);
  return Array.from({ length: count }, (_, k) => {
    let text = '';
    let from = 0;
    places.forEach((place, d) => {
      text += base.slice(from, place) + (Math.floor(k / 36 ** (digits - 1 - d)) % 36).toString(36);
      from = place + 1;
    });
    return text + base.slice(from);
  });
}

const conversationNodes = (ids: readonly string[]): InteractionNode[] => ids.map((id, k) => makeNode({ id: `n${k}`, conversation_id: id, timestamp: k, user_prompt: `step ${k}` }));
const toolNodes = (names: readonly string[]): InteractionNode[] =>
  names.map((name, k) => makeNode({ id: `n${k}`, author: 'human', timestamp: k, tool_executions: [wireToolCall(name, { command: `run ${k}` })] }));
const commitNodes = (hashes: readonly string[]): InteractionNode[] => hashes.map((hash, k) => makeNode({ id: `n${k}`, timestamp: k, artifact_links: [generatedLink(`n${k}`, hash)] }));

describe('the shapes themselves', () => {
  it('are the same as each other at every place the first version of the table looked, and different at the places it did not', () => {
    const ids = variants(200, 16 * 1024, 3, 'c');
    const looked = oldSample(16 * 1024);
    expect(new Set(ids).size).toBe(200);
    const different = new Set<number>();
    for (const id of ids) {
      expect(id.length).toBe(16 * 1024);
      for (let at = 0; at < id.length; at++) if (id[at] !== 'c') different.add(at);
    }
    expect(different.size).toBeGreaterThan(0);
    expect(different.size).toBeLessThanOrEqual(3);
    for (const at of different) expect(looked.has(at)).toBe(false);
  });

  it('are as many different strings as were asked for, as long as asked', () => {
    for (const [count, length, digits] of [[36 ** 2, 300, 2], [5_000, 300, 4], [1_000, 5_000, 3]]) {
      const all = variants(count, length, digits, 'xyz');
      expect(new Set(all).size).toBe(count);
      expect(all.every((text) => text.length === length)).toBe(true);
    }
  });
});

describe('conversation ids of 16 KiB that differ only off the places the first version sampled', () => {
  const ids = variants(2_000, 16 * 1024, 3, 'c');

  it('are 2,000 conversations, named apart, found by a filter, in a second', () => {
    let labels: string[] = [];
    let found: string[] = [];
    let conversations = 0;
    const elapsed = fastest(
      () => conversationNodes(ids),
      (nodes) => {
        const index = getTimelineIndex(nodes);
        const facets = buildFacets(index);
        conversations = index.conversationIds.length;
        labels = facets.conversations.map((option) => option.label);
        found = runSearch(index, { query: parseQuery(''), filters: only({ conv: [ids[1_234]] }), file: null }).ids;
      },
    );
    expect(elapsed).toBeLessThan(1_000);
    expect(conversations).toBe(2_000);
    expect(new Set(labels).size).toBe(2_000);
    for (const label of labels) expect(label.length).toBeLessThanOrEqual(LABEL_LIMIT + 8);
    expect(found).toEqual(['n1234']);
  });

  it('cost less than three times as much for twice as many', () => {
    const [small, large] = fastestOfTwo(
      () => conversationNodes(ids.slice(0, 500)),
      () => conversationNodes(ids.slice(0, 1_000)),
      (nodes) => buildFacets(getTimelineIndex(nodes)),
    );
    expect(large).toBeLessThan(3 * small + SLACK);
  });
});

describe('subagent conversations whose ids are long and differ only off the places the first version sampled', () => {
  it('are named apart, by their sessions and by their agents, in a second', () => {
    const session = 's'.repeat(300);
    const agents = variants(1_500, 12 * 1024, 3, 'a');
    let labels: string[] = [];
    const elapsed = fastest(
      () => conversationNodes(agents.map((agent) => `${session}:agent-${agent}`)),
      (nodes) => {
        labels = buildFacets(getTimelineIndex(nodes)).conversations.map((option) => option.label);
      },
    );
    expect(elapsed).toBeLessThan(1_000);
    expect(new Set(labels).size).toBe(1_500);
    for (const label of labels) expect(label.startsWith('Subagent ')).toBe(true);
  });

  it('and so are the sessions they belong to, when those are the long ones', () => {
    const sessions = variants(1_500, 12 * 1024, 3, 'x');
    let labels: string[] = [];
    const elapsed = fastest(
      () => conversationNodes(sessions.map((session) => `${session}:agent-0123abcd`)),
      (nodes) => {
        labels = buildFacets(getTimelineIndex(nodes)).conversations.map((option) => option.label);
      },
    );
    expect(elapsed).toBeLessThan(1_000);
    expect(new Set(labels).size).toBe(1_500);
  });
});

describe('tool names of 300 characters that differ only off the places the first version sampled', () => {
  const names = variants(20_000, 300, 4, 't');

  it('are 20,000 tools, found by a filter and by a field, in a second', () => {
    let tools = 0;
    let narrowed: string[] = [];
    let named: string[] = [];
    const elapsed = fastest(
      () => toolNodes(names),
      (nodes) => {
        const index = getTimelineIndex(nodes);
        tools = buildFacets(index).tools.length;
        narrowed = runSearch(index, { query: parseQuery(''), filters: only({ tool: [names[12_345]] }), file: null }).ids;
        named = runSearch(index, { query: parseQuery(`tool:${names[777]}`), filters: NO_FILTERS, file: null }).ids;
      },
    );
    expect(elapsed).toBeLessThan(1_000);
    expect(tools).toBe(20_000);
    expect(narrowed).toEqual(['n12345']);
    expect(named).toEqual(['n777']);
  });

  it('cost less than three times as much for twice as many', () => {
    const [small, large] = fastestOfTwo(
      () => toolNodes(names.slice(0, 5_000)),
      () => toolNodes(names.slice(0, 10_000)),
      (nodes) => buildFacets(getTimelineIndex(nodes)),
    );
    expect(large).toBeLessThan(3 * small + SLACK);
  });
});

describe('commit hashes of 300 characters that differ only off the places the first version sampled', () => {
  it('are 20,000 commits, found by a filter, in a second', () => {
    const hashes = variants(20_000, 300, 4, 'f');
    let commits = 0;
    let found: string[] = [];
    const elapsed = fastest(
      () => commitNodes(hashes),
      (nodes) => {
        const index = getTimelineIndex(nodes);
        commits = index.commitHashes.length;
        found = runSearch(index, { query: parseQuery(''), filters: only({ commit: [hashes[4_321]] }), file: null }).ids;
      },
    );
    expect(elapsed).toBeLessThan(1_000);
    expect(commits).toBe(20_000);
    expect(found).toEqual(['n4321']);
  });
});

describe('the table', () => {
  it('takes time in proportion to the number of strings it is given', () => {
    const names = variants(20_000, 1_000, 4, 'q');
    const [small, large] = fastestOfTwo(
      () => ({ table: new StringTable(), count: 10_000 }),
      () => ({ table: new StringTable(), count: 20_000 }),
      ({ table, count }) => {
        for (let k = 0; k < count; k++) table.add(names[k]);
        expect(table.size).toBe(count);
      },
    );
    expect(large).toBeLessThan(3 * small + SLACK);
    expect(large).toBeLessThan(1_000);
  });

  it('files each of 10,000 strings that are the same everywhere but at one late place, and finds each again', () => {
    const names = variants(10_000, 5_000, 3, 'w');
    const table = new StringTable();
    const numbers = names.map((name) => table.add(name));
    expect(numbers).toEqual(names.map((_, k) => k));
    expect(names.every((name, k) => table.indexOf(name) === k)).toBe(true);
    expect(table.indexOf(`${names[0].slice(0, 4_999)}!`)).toBe(-1);
  });
});
