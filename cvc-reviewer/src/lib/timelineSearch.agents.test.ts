import { describe, expect, it } from 'vitest';
import { buildFacets, conversationNameOf, getTimelineIndex, NO_FILTERS, parseQuery, runSearch } from './timelineSearch';
import { reasoningBoundary } from './reasoningMarker';
import { makeNode, promptStep } from '../test/fixtures';

describe('conversation names for the agents of 0.8.0 (#187)', () => {
  const node = (id: string, conversation: string) => makeNode({ ...promptStep, id, conversation_id: conversation });
  const ids = ['6b1f0c0e-1111-4aaa-8bbb-000000000001', '6b1f0c0e-1111-4aaa-8bbb-000000000002', '6b1f0c0e-1111-4aaa-8bbb-000000000003'];
  const SESSION = '4f2a9c1e-0000-4000-8000-000000000001';

  it('names a prefixed conversation by its agent, with the note that the publisher said so', () => {
    const facets = buildFacets(getTimelineIndex([node(ids[0], 'codex:thr_1'), node(ids[1], 'opencode:ses_2'), node(ids[2], SESSION)]));
    const byValue = new Map(facets.conversations.map((option) => [option.value, option]));
    expect(byValue.get('codex:thr_1')).toMatchObject({ label: 'Codex session thr_1', note: 'agent as recorded by the publisher' });
    expect(byValue.get('opencode:ses_2')?.label).toBe('OpenCode session ses_2');
    expect(byValue.get(SESSION)?.label).toBe('Session 4f2a9c1e');
    expect(byValue.get(SESSION)?.note).toBeUndefined();
  });

  it('names a conversation that is not in the list from its id alone', () => {
    expect(conversationNameOf('cursor:abc')).toBe('Cursor session abc');
    expect(conversationNameOf('antigravity:')).toBe('Antigravity session');
    // Only the lowercase prefix is one.
    expect(conversationNameOf('Codex:abc')).toBe('Session Codex:abc');
  });

  it('does not nest the subagents of a new agent: each is its own conversation, and Claude Code nesting is as before', () => {
    const facets = buildFacets(getTimelineIndex([node(ids[0], 'codex:parent'), node(ids[1], 'codex:child'), node(ids[2], `${SESSION}:agent-a1b2`)]));
    expect(facets.conversations.map((option) => option.label).sort()).toEqual(['Codex session child', 'Codex session parent', 'Subagent a1b2']);
  });
});

describe('the search index and a provider summary', () => {
  const id = '6b1f0c0e-1111-4aaa-8bbb-000000000001';
  const marker = `[cvc: provider summary of the model's reasoning ${reasoningBoundary(id, 'provider_summary', 0)}]\n\n`;
  const search = (cot: string, query: string) => {
    const index = getTimelineIndex([makeNode({ ...promptStep, id, conversation_id: 'codex:thr_1', model_cot: cot })]);
    return runSearch(index, { query: parseQuery(query), filters: NO_FILTERS, file: null }).ids;
  };

  it('finds the summary text, and not the marker the card does not draw', () => {
    expect(search(`${marker}Weighed the retry loop.`, 'retry loop')).toEqual([id]);
    expect(search(`${marker}Weighed the retry loop.`, 'provider summary')).toEqual([]);
    expect(search(`${marker}Weighed the retry loop.`, '~cvc-')).toEqual([]);
  });

  it('finds the marker of text it does not verify, which the card draws', () => {
    const forged = marker.replace(/~cvc-[0-9a-f]{16}/, '~cvc-0123456789abcdef');
    expect(search(`${forged}Weighed it.`, 'provider summary')).toEqual([id]);
  });
});
