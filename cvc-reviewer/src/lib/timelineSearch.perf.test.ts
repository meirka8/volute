/**
 * How long the search takes (issue #118). The targets are the issue's: on a typical timeline of 300 steps a query
 * takes under 5 ms, so that typing a file name narrows within one frame, and on 10,000 steps (the most a FORMAT5
 * capture may hold) the search stays responsive, narrowing from the last result while the query is only extended.
 *
 * The bounds here are several times looser than what is measured (see the numbers in the pull request), because the
 * test files run side by side on a machine that may be busy: each is a fastest-of-several, and a bound is there to
 * catch a search that does work it should not (it would take seconds, not milliseconds), not to time one.
 */
import { describe, expect, it } from 'vitest';
import { forgetLastSearch, getTimelineIndex, NO_FILTERS, parseQuery, runSearch, type SearchRequest } from './timelineSearch';
import { searchFixture } from '../test/searchFixtures';
import type { InteractionNode } from '../types/cvc';

/** The fastest of `runs` runs of `work`, in milliseconds. */
function fastest(runs: number, work: () => void): number {
  let best = Infinity;
  for (let run = 0; run < runs; run++) {
    const started = performance.now();
    work();
    best = Math.min(best, performance.now() - started);
  }
  return best;
}

const request = (query: string): SearchRequest => ({ query: parseQuery(query), filters: NO_FILTERS, file: null });
const newestFirst = (nodes: InteractionNode[]) => [...nodes].sort((a, b) => b.timestamp - a.timestamp);

describe('300 typical steps', () => {
  const nodes = newestFirst(searchFixture(300).nodes);

  it('builds the index in a few milliseconds, once per array', () => {
    // Each run is a new array, so each builds (the first run also pays for the code being compiled).
    const built = fastest(15, () => getTimelineIndex([...nodes]));
    expect(built).toBeLessThan(150);
    const index = getTimelineIndex(nodes);
    expect(fastest(5, () => getTimelineIndex(nodes))).toBeLessThan(1);
    expect(getTimelineIndex(nodes)).toBe(index);
  });

  it.each([
    ['a file name', 'refund_handler'],
    ['a path', 'path:refund_handler'],
    ['a tool', 'tool:edit'],
    ['a commit', 'commit:01c0ffee'],
    ['a phrase', '"retry limit"'],
    ['a common word, which matches every step', 'the'],
    ['a word that is nowhere', 'zzzzzz'],
    ['two words', 'handler tests'],
  ])('answers a query for %s in well under a frame', (_label, query) => {
    const index = getTimelineIndex(nodes);
    const elapsed = fastest(20, () => {
      forgetLastSearch(index); // not narrowing from an earlier search: the worst case
      runSearch(index, request(query));
    });
    expect(elapsed).toBeLessThan(25);
  });

  it('answers each key of a file name as it is typed, from the keys before', () => {
    const index = getTimelineIndex(nodes);
    const word = 'refund_handler';
    forgetLastSearch(index);
    const started = performance.now();
    for (let at = 1; at <= word.length; at++) runSearch(index, request(word.slice(0, at)));
    expect(performance.now() - started).toBeLessThan(100);
  });
});

describe('10,000 steps', () => {
  const nodes = newestFirst(searchFixture(10_000, 40).nodes);

  it('builds the index once', () => {
    expect(fastest(3, () => getTimelineIndex([...nodes]))).toBeLessThan(5000);
  });

  it('answers a query with no earlier search to start from, in a fraction of a second', () => {
    const index = getTimelineIndex(nodes);
    for (const query of ['refund_handler', 'the', 'handler', 'path:refund_handler', 'tool:edit', 'retry limit', 'zzzzzz']) {
      const elapsed = fastest(5, () => {
        forgetLastSearch(index);
        runSearch(index, request(query));
      });
      expect(elapsed, query).toBeLessThan(500);
    }
  });

  it('narrows from the last result while the query is only extended, which is the cost of a key being typed', () => {
    const index = getTimelineIndex(nodes);
    const word = 'refund_handler';
    // The first key matches nearly everything and has nothing to narrow from; the keys after it are the point.
    forgetLastSearch(index);
    const costs: number[] = [];
    for (let at = 1; at <= word.length; at++) {
      const started = performance.now();
      runSearch(index, request(word.slice(0, at)));
      costs.push(performance.now() - started);
    }
    expect(Math.max(...costs.slice(3))).toBeLessThan(100);
    // After a few letters the result is a few hundred steps: what is searched for the rest of the word is that, not 10,000.
    expect(Math.max(...costs.slice(6))).toBeLessThan(20);
  });

  it('gives the same steps narrowing from the last result as starting again', () => {
    const index = getTimelineIndex(nodes);
    forgetLastSearch(index);
    runSearch(index, request('refund'));
    const narrowed = runSearch(index, request('refund_handler')).ids;
    forgetLastSearch(index);
    expect(narrowed).toEqual(runSearch(index, request('refund_handler')).ids);
  });
});
