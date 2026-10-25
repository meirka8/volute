/**
 * Long strings from the wire (issue #118, revision round): the validator of a node accepts an id, a tool name or a path
 * a mebibyte long. Nothing the search keys by such a string may take time in proportion to the square of anything, and
 * two strings that differ only in their last character must stay two.
 *
 * The engine hashes a string of more than 16,383 characters by its length alone, so a `Map` of strings of one length is
 * one long chain; and the names of conversations were made by cutting each id a few characters at a time up to its
 * length, which for two ids of a mebibyte took 72 seconds. Both are what these tests are about.
 */
import { describe, expect, it } from 'vitest';
import { buildFacets, conversationNameOf, filterChips, forgetLastSearch, getTimelineIndex, LABEL_LIMIT, NO_FILTERS, parseQuery, runSearch, type Filters } from './timelineSearch';
import { encodeFilters } from './timelineSearchUrl';
import { makeNode, wireToolCall } from '../test/fixtures';
import type { InteractionNode } from '../types/cvc';

const MIB = 1024 * 1024;
/** `count` strings of a mebibyte, all of one length, that differ only in their last characters. */
const distinct = (count: number, filler = 'i') => Array.from({ length: count }, (_, k) => `${filler.repeat(MIB - 8)}${String(k).padStart(8, '0')}`);
const time = (run: () => void) => {
  const started = performance.now();
  run();
  return performance.now() - started;
};
const only = (partial: Partial<Filters>): Filters => ({ ...NO_FILTERS, ...partial });

describe.each([2, 20])('%i ids of a mebibyte that differ only at the end', (count) => {
  it('stay that many conversations, with that many different names, and the index, the facets and a filter take a fraction of a second', () => {
    const ids = distinct(count);
    // Two steps in each conversation, so that an id is looked up more than once.
    const nodes: InteractionNode[] = ids.flatMap((id, k) => [
      makeNode({ id: `a${k}`, conversation_id: id, timestamp: 2 * k, user_prompt: `first ${k}` }),
      makeNode({ id: `b${k}`, conversation_id: id, timestamp: 2 * k + 1, user_prompt: `second ${k}` }),
    ]);
    let found: string[] = [];
    let labels: string[] = [];
    const elapsed = time(() => {
      const index = getTimelineIndex(nodes);
      const facets = buildFacets(index);
      labels = facets.conversations.map((option) => option.label);
      found = runSearch(index, { query: parseQuery(''), filters: only({ conv: [ids[count - 1]] }), file: null }).ids;
      filterChips(only({ conv: [ids[count - 1]] }), facets, null);
    });
    expect(elapsed).toBeLessThan(300);
    expect(found).toEqual([`a${count - 1}`, `b${count - 1}`]); // in the order the steps were given
    expect(labels).toHaveLength(count);
    expect(new Set(labels).size).toBe(count);
    for (const label of labels) expect(label.length).toBeLessThanOrEqual(LABEL_LIMIT);
    // The notes and hints are short too.
    for (const option of buildFacets(getTimelineIndex(nodes)).conversations) {
      expect((option.note ?? '').length).toBeLessThanOrEqual(LABEL_LIMIT);
      expect((option.hint ?? '').length).toBeLessThanOrEqual(LABEL_LIMIT);
    }
  });

  it('are told apart when they differ in the middle, where nothing that samples a string looks', () => {
    const base = 'm'.repeat(MIB);
    const ids = Array.from({ length: count }, (_, k) => `${base.slice(0, 500_000 + k)}Z${base.slice(500_001 + k)}`);
    const nodes = ids.map((id, k) => makeNode({ id: `n${k}`, conversation_id: id, timestamp: k }));
    let conversations = 0;
    const elapsed = time(() => {
      conversations = buildFacets(getTimelineIndex(nodes)).conversations.length;
    });
    expect(conversations).toBe(count);
    expect(elapsed).toBeLessThan(1_500);
  });

  it('subagents of one very long session are named apart, and so are very long agent ids', () => {
    const session = 's'.repeat(MIB - 20);
    const nodes = Array.from({ length: count }, (_, k) => makeNode({ id: `n${k}`, conversation_id: `${session}:agent-${'a'.repeat(100_000)}${k}`, timestamp: k }));
    let labels: string[] = [];
    const elapsed = time(() => {
      labels = buildFacets(getTimelineIndex(nodes)).conversations.map((option) => option.label);
    });
    expect(elapsed).toBeLessThan(500);
    expect(new Set(labels).size).toBe(count);
    for (const label of labels) expect(label.startsWith('Subagent ')).toBe(true);
  });
});

describe.each([2, 20])('%i tool names of a mebibyte that differ only at the end', (count) => {
  it('stay that many tools, with that many labels of a bounded length, and can be searched and filtered', () => {
    const names = distinct(count, 't');
    const nodes = names.map((name, k) => makeNode({ id: `n${k}`, author: 'human', timestamp: k, tool_executions: [wireToolCall(name, { command: `run ${k}` })] }));
    let labels: string[] = [];
    let narrowed: string[] = [];
    let named: string[] = [];
    const elapsed = time(() => {
      const index = getTimelineIndex(nodes);
      const tools = buildFacets(index).tools;
      labels = tools.map((option) => option.label);
      narrowed = runSearch(index, { query: parseQuery(''), filters: only({ tool: [names[count - 1]] }), file: null }).ids;
      named = runSearch(index, { query: parseQuery('tool:ttt'), filters: NO_FILTERS, file: null }).ids;
    });
    expect(elapsed).toBeLessThan(300);
    expect(labels).toHaveLength(count);
    expect(new Set(labels).size).toBe(count);
    for (const label of labels) expect(label.length).toBeLessThanOrEqual(LABEL_LIMIT + 8); // and the ellipsis and the number of a repeat
    expect(narrowed).toEqual([`n${count - 1}`]);
    expect(named).toHaveLength(count);
    // The chip of a filter by such a tool is short as well.
    const chip = filterChips(only({ tool: [names[0]] }), buildFacets(getTimelineIndex(nodes)), null)[0];
    expect(chip.label.length).toBeLessThanOrEqual(LABEL_LIMIT + 1);
  });
});

describe.each([2, 20])('%i paths of a mebibyte that differ only at the end', (count) => {
  it('stay that many paths, in one step or in many, and the file filter and path: find the right ones', () => {
    // A very long directory and a file name that is different for each.
    const paths = Array.from({ length: count }, (_, k) => `/repo/${'d'.repeat(MIB - 40)}/file${String(k).padStart(4, '0')}.ts`);
    const oneStep = makeNode({ id: 'one', author: 'human', timestamp: 0, tool_executions: paths.map((path) => wireToolCall('Read', { file_path: path })), context_items: paths.map((file_path) => ({ file_path })) });
    const manySteps = paths.map((path, k) => makeNode({ id: `m${k}`, author: 'human', timestamp: k + 1, tool_executions: [wireToolCall('Read', { file_path: path })] }));
    const nodes = [oneStep, ...manySteps];
    const wanted = `file${String(count - 1).padStart(4, '0')}.ts`;
    let touching: string[] = [];
    let byPath: string[] = [];
    const elapsed = time(() => {
      const index = getTimelineIndex(nodes);
      buildFacets(index);
      forgetLastSearch(index);
      touching = runSearch(index, { query: parseQuery(''), filters: NO_FILTERS, file: wanted }).ids;
      byPath = runSearch(index, { query: parseQuery(`path:${wanted}`), filters: NO_FILTERS, file: null }).ids;
    });
    expect(elapsed).toBeLessThan(600);
    // The step that read all of them, and the step that read this one; in the order the steps were given.
    expect(touching).toEqual(['one', `m${count - 1}`]);
    expect(byPath).toEqual(['one', `m${count - 1}`]);
    // The first file is touched by its own step and by the one that read them all, and by no other.
    expect(runSearch(getTimelineIndex(nodes), { query: parseQuery(''), filters: NO_FILTERS, file: 'file0000.ts' }).ids).toEqual(['one', 'm0']);
  });
});

describe('the address', () => {
  it('leaves out of itself a filter value too long for it, and never writes one cut short', () => {
    const [long] = distinct(1);
    const written = encodeFilters(only({ conv: [long, 'ok'], tool: [long, 'Bash'] }), null);
    expect(written).toBe('conv=ok&tool=Bash');
    expect(written.length).toBeLessThan(100);
  });

  it('names a conversation whose id is not in the timeline from its first characters only', () => {
    const [long] = distinct(1);
    expect(conversationNameOf(long)).toBe(`Session ${long.slice(0, 8)}`);
    expect(conversationNameOf(`${long}:agent-${long}`)).toBe(`Subagent ${long.slice(0, 8)}`);
  });
});
