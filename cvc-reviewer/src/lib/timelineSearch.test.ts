/**
 * The index, the query language and the filters of the timeline search (issue #118).
 * Everything here is pure, so each case is a few nodes and a query.
 */
import { describe, expect, it } from 'vitest';
import {
  activeFilterCount,
  buildFacets,
  conversationHint,
  conversationNameOf,
  effectiveFile,
  filterChips,
  filterCount,
  forgetLastSearch,
  getTimelineIndex,
  highlightTerms,
  isSubagentConversation,
  linkKindOf,
  narrows,
  NO_FILTERS,
  normalizeForSearch,
  normalizePath,
  parseQuery,
  runSearch,
  sameFilters,
  toggled,
  touchesFile,
  undrawnMatches,
  withToggled,
  withoutChip,
  type Filters,
} from './timelineSearch';
import { generatedLink, makeNode, rawToolCall, toolCall, wireToolCall } from '../test/fixtures';
import { FIXTURE_PATH, searchFixture } from '../test/searchFixtures';
import type { ArtifactLinkData, InteractionNode } from '../types/cvc';

const ZWSP = '\u200B';
const RLO = '\u202E';

const H1 = 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678';
const H2 = 'b2c3d4e5f60718293a4b5c6d7e8f901234567890';
const SESSION = '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70';
const AGENT = `${SESSION}:agent-ab12cd34ef56`;
const harnessId = (n: number) => `1f0e5c3a-9d2b-8c4e-a1b7-${n.toString(16).padStart(12, '0')}`;

/** Runs a query over `nodes` from scratch (never from the last search) and gives the ids found. */
function ids(nodes: InteractionNode[], query = '', filters: Filters = NO_FILTERS, file: string | null = null): string[] {
  const index = getTimelineIndex(nodes);
  forgetLastSearch(index);
  return runSearch(index, { query: parseQuery(query), filters, file }).ids;
}

const only = (partial: Partial<Filters>): Filters => ({ ...NO_FILTERS, ...partial });

// A small, varied timeline: every kind of step the filters tell apart.
const prompt = makeNode({
  id: harnessId(1),
  conversation_id: SESSION,
  author: 'human',
  timestamp: 6000,
  user_prompt: 'Add a retry limit to fetchWithRetry',
  model_cot: 'Exponential backoff with jitter avoids retry storms.',
  model_response: 'Capped it at three attempts.',
  tool_executions: [
    wireToolCall('Bash', { command: 'npm test -- --run "fetch"\necho done', description: 'Run the tests' }),
    wireToolCall('Edit', { file_path: '/work/repo/src/net/Fetch.ts', old_string: 'sleep(100)', new_string: 'sleep(100 * 2 ** n)' }),
  ],
  context_items: [{ file_path: '/work/repo/src/net/Fetch.ts' }],
  artifact_links: [generatedLink(harnessId(1), H1.toUpperCase())],
});
const results = makeNode({
  id: harnessId(2),
  conversation_id: SESSION,
  author: 'system',
  timestamp: 5000,
  user_prompt: '[tool_result Bash] 12 passed\n\n[tool_result Edit error] no match for the old text',
  model_response: 'The edit did not apply.',
  tool_executions: [wireToolCall('Read', { file_path: '/work/repo/README.md' }), wireToolCall('mcp__github__create_issue', { title: 'Track it' })],
  artifact_links: [
    { interaction_id: harnessId(2), git_commit_hash: H1, link_type: 'temporal' },
    generatedLink(harnessId(2), H2),
  ],
});
const mcp = makeNode({
  id: '3b9f0f1e-6a4c-4d6e-9b1a-2f6c8e7d5a10',
  conversation_id: 'agent-session-default',
  author: 'agent',
  timestamp: 4000,
  user_prompt: 'Review the change',
  model_cot: 'Looks safe.',
  model_response: 'Approved.',
  artifact_links: [{ interaction_id: 'x', git_commit_hash: H2, link_type: 'verified' }],
});
const compaction = makeNode({
  id: harnessId(3),
  conversation_id: SESSION,
  author: 'system',
  timestamp: 3000,
  user_prompt: '[compaction summary]\nThe user asked for a retry limit.',
  model_response: 'Continuing.',
});
const subagent = makeNode({
  id: harnessId(4),
  conversation_id: AGENT,
  author: 'human',
  timestamp: 2000,
  user_prompt: 'Survey the repository for other helpers.\nReport each path.',
  model_response: 'Found two.',
  tool_executions: [wireToolCall('Grep', { pattern: 'TODO', path: '/work/repo/src' })],
  artifact_links: [generatedLink(harnessId(4), H1)],
});
const asserted = makeNode({
  id: harnessId(5),
  conversation_id: SESSION,
  author: 'system',
  timestamp: 1000,
  user_prompt: '[tool_result Bash] ok',
  model_response: 'Done.',
  artifact_links: [
    {
      interaction_id: harnessId(5),
      git_commit_hash: H2,
      link_type: 'generated',
      derivation_event: {
        event_id: 'e'.repeat(64),
        interaction_id: harnessId(5),
        target_commit: H2,
        relation: 'generated',
        evidence: { version: 1, kind: 'remote_assertion' },
        origin: 'remote_assertion',
        source_event_ids: [],
      },
    },
  ],
});
const TIMELINE = [prompt, results, mcp, compaction, subagent, asserted];

// ---------------------------------------------------------------------------

describe('normalising', () => {
  it('lower-cases, and turns every kind of white space into a space (CRLF is one line break, and so is CR)', () => {
    expect(normalizeForSearch('Hello\r\nWorld\rAgain\tTabbed')).toBe('hello world again tabbed');
    expect(normalizeForSearch('retry\u00a0limit')).toBe('retry limit');
  });

  it('leaves out every hidden character, so an invisible one can not hide a match', () => {
    expect(normalizeForSearch(`pass${ZWSP}word`)).toBe('password');
    expect(normalizeForSearch(`${RLO}evil\u2066x\u2069\ufeff`)).toBe('evilx');
    expect(normalizeForSearch('a\u0000b\u007fc')).toBe('abc');
    expect(normalizeForSearch(`t${String.fromCodePoint(0xe0041)}ag`)).toBe('tag');
  });

  it('treats anything that is not text as nothing', () => {
    for (const value of [undefined, null, 7, {}, [], '']) expect(normalizeForSearch(value)).toBe('');
  });

  it('writes a path with / and without a leading ./, in lower case', () => {
    expect(normalizePath('C:\\Work\\Repo\\Src\\A.ts')).toBe('c:/work/repo/src/a.ts');
    expect(normalizePath('./src/a.ts')).toBe('src/a.ts');
    expect(normalizePath('././src/a.ts')).toBe('src/a.ts');
    expect(normalizePath(`src/${ZWSP}a.ts`)).toBe('src/a.ts');
    expect(normalizePath(undefined)).toBe('');
  });
});

// ---------------------------------------------------------------------------

describe('the query language', () => {
  const terms = (input: string) => parseQuery(input).terms.map((term) => (term.scope ? `${term.scope}:${term.text}` : term.text));

  it('reads nothing, or only white space, as no query', () => {
    expect(parseQuery('').terms).toEqual([]);
    expect(parseQuery('   \t\n  ').terms).toEqual([]);
  });

  it('splits on white space into terms, in lower case', () => {
    expect(terms('  Fetch   RETRY\tLimit ')).toEqual(['fetch', 'retry', 'limit']);
  });

  it('groups a phrase in double quotes, and a phrase keeps its white space', () => {
    expect(terms('"retry limit" fetch')).toEqual(['retry limit', 'fetch']);
    expect(terms('a "two  spaces" b')).toEqual(['a', 'two  spaces', 'b']);
  });

  it('runs a phrase that is not closed to the end: the person is still typing it', () => {
    expect(terms('fetch "retry lim')).toEqual(['fetch', 'retry lim']);
  });

  it('ignores an empty pair of quotes, and a quote in the middle of a word is just a character', () => {
    expect(terms('"" fetch')).toEqual(['fetch']);
    expect(terms('say"hi there"')).toEqual(['say"hi', 'there"']);
    expect(terms('5"')).toEqual(['5"']);
  });

  it('narrows a term to a field with path:, tool: or commit:, in any case', () => {
    expect(terms('path:src/net tool:Bash commit:ABC123')).toEqual(['path:src/net', 'tool:bash', 'commit:abc123']);
    expect(terms('PATH:A Tool:B COMMIT:c')).toEqual(['path:a', 'tool:b', 'commit:c']);
  });

  it('takes a quoted value after a prefix', () => {
    expect(terms('path:"my dir/a.ts" next')).toEqual(['path:my dir/a.ts', 'next']);
    expect(terms('tool:"github · create_issue"')).toEqual(['tool:github · create_issue']);
    expect(terms('path:"never closed')).toEqual(['path:never closed']);
  });

  it('reads any other word: as plain text, whatever comes before the colon', () => {
    expect(terms('http://example.com')).toEqual(['http://example.com']);
    expect(terms('foo:bar')).toEqual(['foo:bar']);
    expect(terms(':x')).toEqual([':x']);
    expect(terms('paths:x')).toEqual(['paths:x']);
    expect(terms('"path:x"')).toEqual(['path:x']); // quoted: a phrase, not a prefix
    expect(parseQuery('"path:x"').terms[0].scope).toBeNull();
  });

  it('does not take a prefix with nothing after it for a term: the person is still typing', () => {
    expect(terms('path:')).toEqual([]);
    expect(terms('tool: fetch')).toEqual(['fetch']);
    expect(terms('commit:"" x')).toEqual(['x']);
  });

  it('writes the path of a path: term the way the index writes paths', () => {
    expect(terms('path:.\\Src\\Net')).toEqual(['path:src/net']);
  });

  it('leaves hidden characters out of a term, as it does from the text', () => {
    expect(terms(`pass${ZWSP}word`)).toEqual(['password']);
    expect(terms(ZWSP)).toEqual([]);
  });

  it('keeps one of each term, and stops at sixteen terms', () => {
    expect(terms('a A a b')).toEqual(['a', 'b']);
    const many = Array.from({ length: 40 }, (_, k) => `t${k}`).join(' ');
    expect(parseQuery(many).terms).toHaveLength(16);
  });

  it('cuts a term that is far longer than anything worth looking for', () => {
    expect(parseQuery('x'.repeat(5000)).terms[0].text).toHaveLength(512);
  });

  it('never reads any of it as a regular expression', () => {
    expect(terms('.* (a+)+$ [a-')).toEqual(['.*', '(a+)+$', '[a-']);
    const hostile = makeNode({ id: 'h', user_prompt: `${'a'.repeat(50_000)}!` });
    const started = performance.now();
    expect(ids([hostile], '(a+)+$')).toEqual([]);
    expect(performance.now() - started).toBeLessThan(500);
    const literal = makeNode({ id: 'l', user_prompt: 'the pattern (a+)+$ is slow' });
    expect(ids([hostile, literal], '(a+)+$')).toEqual(['l']);
    expect(ids([hostile, literal], '.*')).toEqual([]);
  });

  describe('narrows', () => {
    const q = parseQuery;
    it('holds when each earlier term is inside a later one of the same scope', () => {
      expect(narrows(q('fet'), q('fetch'))).toBe(true);
      expect(narrows(q('fetch'), q('fetch retry'))).toBe(true);
      expect(narrows(q('path:src'), q('path:src/net'))).toBe(true);
      expect(narrows(q(''), q('anything'))).toBe(true);
      expect(narrows(q('a'), q('a'))).toBe(true);
    });
    it('does not hold when the query got longer in another way', () => {
      expect(narrows(q('fetch'), q('fet'))).toBe(false);
      expect(narrows(q('fetch'), q(''))).toBe(false);
      expect(narrows(q('fetch'), q('path:fetch'))).toBe(false);
      expect(narrows(q('path:fetch'), q('fetch'))).toBe(false);
      expect(narrows(q('retry'), q('fetch'))).toBe(false);
    });
  });

  it('says which terms mark text on which surface', () => {
    expect(highlightTerms(parseQuery(''))).toBeNull();
    expect(highlightTerms(parseQuery('fetch'))).toEqual({ plain: ['fetch'], tool: ['fetch'], path: ['fetch'], commit: ['fetch'] });
    expect(highlightTerms(parseQuery('fetch tool:bash path:src'))).toEqual({
      plain: ['fetch'],
      tool: ['fetch', 'bash'],
      path: ['fetch', 'src'],
      commit: ['fetch'],
    });
    // A term with a prefix marks nothing but the surface that shows that field.
    expect(highlightTerms(parseQuery('tool:bash'))).toEqual({ plain: null, tool: ['bash'], path: null, commit: null });
  });
});

// ---------------------------------------------------------------------------

describe('the index', () => {
  it('is built once per array and kept for as long as the array lives', () => {
    const nodes = [prompt, results];
    expect(getTimelineIndex(nodes)).toBe(getTimelineIndex(nodes));
    expect(getTimelineIndex([...nodes])).not.toBe(getTimelineIndex(nodes));
  });

  it('keeps the steps in the order it was given them', () => {
    const index = getTimelineIndex(TIMELINE);
    expect(index.nodes).toBe(TIMELINE);
    expect(index.size).toBe(TIMELINE.length);
  });

  it('holds what the card draws: the stimulus, the reasoning, the response and the values of the arguments, folded, a piece after another', () => {
    const [text] = getTimelineIndex([prompt]).text;
    expect(text.split('\0')).toEqual([
      'add a retry limit to fetchwithretry',
      'exponential backoff with jitter avoids retry storms.',
      'capped it at three attempts.',
      // The values of the arguments as the card draws them, in order: the description and the command, the lines of the diff
      // of the edit (the file is in `paths`). Not the names of the arguments, the JSON, or the escapes.
      'run the tests',
      'npm test -- --run "fetch" echo done',
      'sleep(100)',
      'sleep(100 * 2 ** n)',
    ]);
  });

  it('draws the raw line of a call in the Thought / Action / Observation layout, and so holds it, names and punctuation too', () => {
    const triad = makeNode({ id: 'triad', author: 'agent', user_prompt: 'Look', model_response: 'Done', tool_executions: [toolCall('Bash', { command: 'ls -la' })] });
    const pieces = getTimelineIndex([triad]).text[0].split('\0');
    expect(pieces).toEqual(['look', '{"command":"ls -la"}', 'done']);
    expect(ids([triad], 'command')).toEqual(['triad']); // the line is drawn, name of the argument and all
  });

  it('holds arguments that are not a JSON object as they were recorded, when the card draws them, and nothing for the rest', () => {
    const odd = makeNode({
      id: 'odd',
      author: 'human',
      tool_executions: [rawToolCall('Bash', 'not json at all'), rawToolCall('Bash', '[1,"two"]'), rawToolCall('Bash', 'null'), rawToolCall('Bash', '{"bytes":99999,"cvc_truncated":true}')],
    });
    const pieces = getTimelineIndex([odd]).text[0].split('\0');
    expect(pieces).toEqual(['not json at all', '[1,"two"]']);
    // `null` is a call recorded with no input, and the sentinel is arguments too large to keep: the card says so in its own
    // words, and neither is something a person would look for.
    expect(ids([odd], 'null')).toEqual([]);
    expect(ids([odd], 'cvc_truncated')).toEqual([]);
    expect(ids([odd], '99999')).toEqual([]);
  });

  it('holds the values of nested arguments as the card draws them, numbers included, and not the names of the arguments', () => {
    const nested = makeNode({
      id: 'nested',
      author: 'human',
      tool_executions: [wireToolCall('mcp__srv__tool', { count: 12345, flag: true, list: ['alpha', { deep: 'beta' }], plain: 'gamma' })],
    });
    const pieces = getTimelineIndex([nested]).text[0].split('\0');
    // Each value is a row of the card: a number or a boolean as it is written, a list as the JSON the card shows.
    // (The indented JSON of the list has its line breaks and indentation folded to spaces, one for one.)
    expect(pieces).toEqual(['12345', 'true', '[   "alpha",   {     "deep": "beta"   } ]', 'gamma']);
    expect(ids([nested], 'count')).toEqual([]);
    expect(ids([nested], 'flag')).toEqual([]);
    expect(ids([nested], 'plain')).toEqual([]);
    expect(ids([nested], '12345')).toEqual(['nested']);
    expect(ids([nested], 'true')).toEqual(['nested']);
    expect(ids([nested], 'beta')).toEqual(['nested']);
    expect(ids([nested], 'deep')).toEqual(['nested']); // inside a value the card draws as JSON
  });

  it('holds the values of the options of a call and never the words the Reviewer writes for them', () => {
    const options = makeNode({
      id: 'options',
      author: 'human',
      tool_executions: [
        wireToolCall('Bash', { command: 'true', timeout: 120000, run_in_background: true }),
        wireToolCall('Grep', { pattern: 'x', '-i': true, path: '/r', glob: '*.ts', '-A': 3 }),
        wireToolCall('Edit', { file_path: '/r/a.ts', old_string: 'a', new_string: 'b', replace_all: true }),
      ],
    });
    expect(ids([options], '120s')).toEqual(['options']);
    expect(ids([options], '*.ts')).toEqual(['options']);
    for (const words of ['runs in background', 'ignore case', 'replace all occurrences', 'timeout', 'glob']) expect(ids([options], `"${words}"`), words).toEqual([]);
  });

  it('holds what the stimulus shows and not the markers of the wire, nor the placeholders the Reviewer writes', () => {
    const shown = makeNode({
      id: harnessId(90),
      author: 'system',
      user_prompt: '[tool_result Bash] 12 passed\n\n[tool_result Read error] no such file',
      model_response: 'ok',
    });
    const pieces = getTimelineIndex([shown]).text[0].split('\0');
    expect(pieces).toEqual(['bash', '12 passed', 'read', 'no such file', 'ok']);
    expect(ids([shown], 'tool_result')).toEqual([]);
    expect(ids([shown], 'error]')).toEqual([]);
    const meta = makeNode({ id: harnessId(91), author: 'system', user_prompt: '[meta] Caveat: the messages below', model_response: 'ok' });
    expect(ids([meta], 'meta')).toEqual([]);
    expect(ids([meta], 'caveat')).toEqual([harnessId(91)]);
    const none = makeNode({ id: harnessId(92), author: 'system', user_prompt: '[no stimulus recorded]', model_response: 'ok' });
    expect(ids([none], 'stimulus')).toEqual([]);
    expect(ids([none], 'recorded')).toEqual([]);
    const placeholders = makeNode({ id: 'ph', author: 'agent' });
    expect(ids([placeholders], 'recorded')).toEqual([]);
    expect(ids([placeholders], 'tool execution')).toEqual([]);
  });

  it('holds the reasoning where the card has a place for it, and nowhere else', () => {
    const transcript = makeNode({ id: harnessId(93), author: 'human', user_prompt: 'q', model_cot: 'private reasoning here', model_response: 'r' });
    expect(ids([transcript], 'reasoning')).toEqual([harnessId(93)]);
    // The other layout offers a "Why?" for an AI agent only: a human's reasoning there is on no card.
    const humanTriad = makeNode({ id: 'ht', author: 'human', user_prompt: 'q', model_cot: 'private reasoning here', model_response: 'r' });
    expect(ids([humanTriad], 'reasoning')).toEqual([]);
    const agentTriad = makeNode({ id: 'at', author: 'agent', user_prompt: 'q', model_cot: 'private reasoning here', model_response: 'r' });
    expect(ids([agentTriad], 'reasoning')).toEqual(['at']);
  });

  it('holds both names of a tool: the one recorded and the one the cards show', () => {
    const [tools] = getTimelineIndex([results]).tools;
    // Joined by NUL; a name that is the same twice (Read) is held once.
    expect(tools.split('\0')).toEqual(['read', 'mcp__github__create_issue', 'github · create_issue']);
    expect(getTimelineIndex([results]).toolNames).toEqual(['Read', 'github · create_issue']);
  });

  it('holds the files a step touched, from its context and from the path arguments of its calls', () => {
    const [paths] = getTimelineIndex([prompt]).paths;
    // Joined by NUL, which no path holds: a path with a line break in it is one path.
    expect(paths).toBe('\0/work/repo/src/net/fetch.ts\0');
    const calls = makeNode({
      id: 'p',
      context_items: [{ file_path: 'C:\\w\\Ctx.ts' }],
      tool_executions: [
        wireToolCall('Read', { file_path: '/a/Read.ts' }),
        wireToolCall('Glob', { path: '/a/dir', pattern: '*.ts' }),
        wireToolCall('NotebookEdit', { notebook_path: '/a/n.ipynb', new_source: 'x' }),
        wireToolCall('Other', { target: { file_path: '/nested/ignored.ts' }, file_path: 7 }),
      ],
    });
    expect(getTimelineIndex([calls]).paths[0].split('\0')).toEqual(['', '/a/read.ts', '/a/dir', '/a/n.ipynb', 'c:/w/ctx.ts', '']);
    expect(getTimelineIndex([mcp]).paths[0]).toBe('');
  });

  it('holds the commits a step is linked to, in full and in lower case, once each', () => {
    expect(getTimelineIndex([prompt]).commits[0]).toBe(H1);
    expect(getTimelineIndex([results]).commits[0]).toBe(`${H1}\0${H2}`);
    const twice = makeNode({ id: 't', artifact_links: [generatedLink('t', H1), { interaction_id: 't', git_commit_hash: H1, link_type: 'verified' }] });
    expect(getTimelineIndex([twice]).commits[0]).toBe(H1);
  });

  it('keeps one field from running into the next: nothing matches across the end of one', () => {
    const joined = makeNode({ id: 'j', user_prompt: 'alpha', model_cot: 'beta' });
    expect(ids([joined], 'alpha')).toEqual(['j']);
    expect(ids([joined], 'alphabeta')).toEqual([]);
    expect(ids([joined], '"alpha beta"')).toEqual([]);
  });

  it('reads a step that holds nothing it can read as an empty one, instead of failing', () => {
    const hostile = [
      makeNode({ id: 'h1', tool_executions: 'nope' as unknown as InteractionNode['tool_executions'] }),
      makeNode({ id: 'h2', artifact_links: [null as unknown as ArtifactLinkData], context_items: [null as unknown as { file_path: string }] }),
      makeNode({ id: 'h3', conversation_id: 42 as unknown as string, user_prompt: 7 as unknown as string, model_response: { x: 1 } as unknown as string }),
      makeNode({ id: 'h4', tool_executions: [null as unknown as InteractionNode['tool_executions'][number], { tool_protocol: 'native', tool_name: 7 as unknown as string, arguments: { path: '/x/y.ts' } as unknown as string, status: 'success' }] }),
      makeNode({ id: 'h5', tool_executions: [toolCall('Bash', {}), { ...toolCall('Bash', {}), arguments: undefined as unknown as string }] }),
    ];
    expect(() => getTimelineIndex(hostile)).not.toThrow();
    expect(ids(hostile, '')).toEqual(['h1', 'h2', 'h3', 'h4', 'h5']);
    expect(ids(hostile, 'zzz')).toEqual([]);
  });

  it('reads arguments recorded as an object, before blobs were validated, as the JSON they would be', () => {
    const legacy = makeNode({ id: 'legacy', author: 'human', tool_executions: [{ tool_protocol: 'native', tool_name: 'Bash', arguments: { command: 'ls' } as unknown as string, status: 'success' }] });
    expect(ids([legacy], 'ls')).toEqual(['legacy']);
  });

  it('keeps what it read of a step it could not read to the end', () => {
    // The Action pane of this card would fail on arguments that are not text; the rest of the step is still searched.
    const broken = makeNode({ id: 'broken', author: 'agent', user_prompt: 'findable prompt', tool_executions: [{ tool_protocol: 'native', tool_name: 'Bash', arguments: { command: 'ls' } as unknown as string, status: 'success' }] });
    expect(ids([broken], 'findable')).toEqual(['broken']);
  });
});

// ---------------------------------------------------------------------------

describe('searching', () => {
  it('finds a plain term in the stimulus, the reasoning, the response, a tool name and its arguments', () => {
    expect(ids(TIMELINE, 'fetchwithretry')).toEqual([prompt.id]); // stimulus
    expect(ids(TIMELINE, 'jitter')).toEqual([prompt.id]); // reasoning
    expect(ids(TIMELINE, 'attempts three')).toEqual([prompt.id]); // two words are two terms, in any order...
    expect(ids(TIMELINE, '"attempts three"')).toEqual([]); // ...a phrase is one, in the order it is written
    expect(ids(TIMELINE, '"three attempts"')).toEqual([prompt.id]);
    expect(ids(TIMELINE, 'approved')).toEqual([mcp.id]); // response
    expect(ids(TIMELINE, 'create_issue')).toEqual([results.id]); // tool name, as recorded
    expect(ids(TIMELINE, '"create · github"')).toEqual([]);
    expect(ids(TIMELINE, '"github · create"')).toEqual([results.id]); // tool name, as the cards show it
    expect(ids(TIMELINE, 'sleep')).toEqual([prompt.id]); // arguments
    expect(ids(TIMELINE, 'todo')).toEqual([subagent.id]);
  });

  it('finds a path, and a commit hash in full or in part', () => {
    expect(ids(TIMELINE, 'readme')).toEqual([results.id]);
    expect(ids(TIMELINE, 'src/net')).toEqual([prompt.id]);
    expect(ids(TIMELINE, 'a1b2c3d')).toEqual([prompt.id, results.id, subagent.id]);
    expect(ids(TIMELINE, H2)).toEqual([results.id, mcp.id, asserted.id]);
  });

  it('matches whatever the case', () => {
    expect(ids(TIMELINE, 'FETCHWITHRETRY')).toEqual([prompt.id]);
    expect(ids(TIMELINE, 'FeTcH')).toEqual([prompt.id]);
    expect(ids(TIMELINE, 'src/net/FETCH.TS')).toEqual([prompt.id]);
  });

  it('needs every term (AND), in any field', () => {
    expect(ids(TIMELINE, 'retry limit')).toEqual([prompt.id, compaction.id]);
    expect(ids(TIMELINE, 'retry limit jitter')).toEqual([prompt.id]);
    expect(ids(TIMELINE, 'jitter readme')).toEqual([]);
    // One term in the stimulus, another in the arguments of a call: the same step.
    expect(ids(TIMELINE, 'add sleep')).toEqual([prompt.id]);
  });

  it('keeps the order the steps are in, whatever the query', () => {
    expect(ids(TIMELINE, '')).toEqual(TIMELINE.map((node) => node.id));
    expect(ids(TIMELINE, 'a')).toEqual(TIMELINE.filter((node) => ids([node], 'a').length > 0).map((node) => node.id));
    const reversed = [...TIMELINE].reverse();
    expect(ids(reversed, 'retry')).toEqual([compaction.id, prompt.id]);
  });

  it('finds nothing for a term that is nowhere, and everything for no term', () => {
    expect(ids(TIMELINE, 'zzzzzz')).toEqual([]);
    expect(ids(TIMELINE, '')).toHaveLength(TIMELINE.length);
    expect(ids([], 'x')).toEqual([]);
  });

  it('can not be hidden from by a character that draws nothing', () => {
    const hidden = makeNode({ id: 'hid', user_prompt: `reset the pass${ZWSP}word ${RLO}now`, model_response: 'ok' });
    expect(ids([hidden], 'password')).toEqual(['hid']);
    expect(ids([hidden], `pass${ZWSP}word`)).toEqual(['hid']);
    expect(ids([hidden], `"the password now"`)).toEqual(['hid']);
    const inCommand = makeNode({ id: 'cmd', tool_executions: [wireToolCall('Bash', { command: `rm${ZWSP} -rf build` })] });
    expect(ids([inCommand], 'rm -rf')).toEqual(['cmd']);
  });

  it('reads CRLF and CR as a newline, in the text and in a quoted phrase', () => {
    const windows = makeNode({ id: 'w', model_response: 'first line\r\nsecond line\rthird' });
    expect(ids([windows], '"first line\nsecond line"')).toEqual(['w']);
    expect(ids([windows], '"line\r\nsecond"')).toEqual(['w']);
    expect(ids([windows], '"second line\nthird"')).toEqual(['w']);
  });

  it('looks in the one field a prefix names, and in no other', () => {
    const crossed = makeNode({
      id: 'x',
      user_prompt: 'open the fetch file and run bash',
      tool_executions: [wireToolCall('Read', { file_path: '/repo/lib.ts' })],
      artifact_links: [generatedLink('x', H1)],
    });
    // "fetch" is in the prompt only; "lib" in a path only; "read" in a tool name only; "a1b2" in a commit only.
    expect(ids([crossed], 'fetch')).toEqual(['x']);
    expect(ids([crossed], 'path:fetch')).toEqual([]);
    expect(ids([crossed], 'tool:fetch')).toEqual([]);
    expect(ids([crossed], 'commit:fetch')).toEqual([]);
    expect(ids([crossed], 'path:lib')).toEqual(['x']);
    expect(ids([crossed], 'path:read')).toEqual([]);
    expect(ids([crossed], 'tool:read')).toEqual(['x']);
    expect(ids([crossed], 'tool:lib')).toEqual([]);
    expect(ids([crossed], 'commit:a1b2')).toEqual(['x']);
    expect(ids([crossed], 'commit:lib')).toEqual([]);
    // bash is in the prompt (as a word), not in a tool name.
    expect(ids([crossed], 'bash')).toEqual(['x']);
    expect(ids([crossed], 'tool:bash')).toEqual([]);
  });

  it('combines a prefix term with plain ones', () => {
    expect(ids(TIMELINE, 'tool:read readme')).toEqual([results.id]);
    expect(ids(TIMELINE, 'tool:bash path:fetch')).toEqual([prompt.id]);
    expect(ids(TIMELINE, 'tool:bash path:readme')).toEqual([]);
    expect(ids(TIMELINE, 'tool:grep commit:a1b2')).toEqual([subagent.id]);
  });

  it('finds a tool by either of its names, and a quoted value with spaces', () => {
    expect(ids(TIMELINE, 'tool:github')).toEqual([results.id]);
    expect(ids(TIMELINE, 'tool:mcp__github')).toEqual([results.id]);
    expect(ids(TIMELINE, 'tool:"github · create_issue"')).toEqual([results.id]);
  });

  it('does not let a term spill over the end of one field into the next', () => {
    const spill = makeNode({ id: 's', tool_executions: [wireToolCall('Bash', { command: 'x' })], context_items: [{ file_path: '/a/b.ts' }] });
    // tools is "bash\nbash" and paths "\n/a/b.ts\n": a term can not start in one and end in the other.
    expect(ids([spill], 'tool:"bash\n/a"')).toEqual([]);
    expect(ids([spill], '"bash\n/a"')).toEqual([]);
    expect(ids([spill], 'tool:bash path:/a')).toEqual(['s']);
  });

  it('starts from the last search when the query only got longer, and gets the same steps as a fresh one', () => {
    const { nodes } = searchFixture(300);
    const sorted = [...nodes].sort((a, b) => b.timestamp - a.timestamp);
    const index = getTimelineIndex(sorted);
    const prompts = only({ origin: ['prompt'] });
    const sequence: Array<[string, Filters, string | null]> = [
      ['', NO_FILTERS, null],
      ['r', NO_FILTERS, null],
      ['re', NO_FILTERS, null],
      ['ref', NO_FILTERS, null],
      ['refund', NO_FILTERS, null],
      ['refun', NO_FILTERS, null], // shorter: starts again
      ['refund', NO_FILTERS, null],
      ['refund path:handler', NO_FILTERS, null],
      ['refund path:handler tool:edit', NO_FILTERS, null],
      ['refund tool:edit', NO_FILTERS, null], // dropped a term: starts again
      ['refund tool:edit', prompts, null], // other filters: starts again
      ['refund tool:edit', prompts, FIXTURE_PATH], // another file: starts again
      ['refund tool:edit ', prompts, FIXTURE_PATH],
      ['xyz', prompts, null],
      ['xyzzy', prompts, null],
      ['', NO_FILTERS, null],
    ];
    for (const [query, filters, file] of sequence) {
      const request = { query: parseQuery(query), filters, file };
      const incremental = runSearch(index, request).ids;
      forgetLastSearch(index);
      expect(incremental, `"${query}"`).toEqual(runSearch(index, request).ids);
    }
  });
});

// ---------------------------------------------------------------------------

describe('the filters', () => {
  describe('conversation', () => {
    it('keeps the steps of the conversations chosen', () => {
      expect(ids(TIMELINE, '', only({ conv: [AGENT] }))).toEqual([subagent.id]);
      expect(ids(TIMELINE, '', only({ conv: [SESSION] }))).toEqual([prompt.id, results.id, compaction.id, asserted.id]);
      expect(ids(TIMELINE, '', only({ conv: [AGENT, 'agent-session-default'] }))).toEqual([mcp.id, subagent.id]);
    });
    it('keeps nothing for a conversation that is not in the timeline', () => {
      expect(ids(TIMELINE, '', only({ conv: ['not-there'] }))).toEqual([]);
    });
  });

  describe('stimulus origin', () => {
    it('tells a person from tool results from everything else', () => {
      expect(ids(TIMELINE, '', only({ origin: ['prompt'] }))).toEqual([prompt.id, subagent.id]);
      expect(ids(TIMELINE, '', only({ origin: ['tool-results'] }))).toEqual([results.id, asserted.id]);
      // Context and compaction, and the MCP, editor and `cvc run` layout, are "other".
      expect(ids(TIMELINE, '', only({ origin: ['other'] }))).toEqual([mcp.id, compaction.id]);
    });
    it('accepts several origins at once', () => {
      expect(ids(TIMELINE, '', only({ origin: ['prompt', 'other'] }))).toEqual([prompt.id, mcp.id, compaction.id, subagent.id]);
    });
    it('puts a step with no stimulus recorded, and a human step that is not drawn as a transcript, under other', () => {
      const none = makeNode({ id: harnessId(90), author: 'system', user_prompt: '[no stimulus recorded]', model_response: 'x' });
      const editor = makeNode({ id: 'editor', author: 'human', user_prompt: 'Explain parse_header.', model_response: 'It returns a pair.' });
      const unmarked = makeNode({ id: harnessId(91), author: 'system', user_prompt: 'plain text no one marked' });
      expect(ids([none, editor, unmarked], '', only({ origin: ['other'] }))).toEqual([none.id, editor.id, unmarked.id]);
      expect(ids([none, editor, unmarked], '', only({ origin: ['prompt'] }))).toEqual([]);
    });
  });

  describe('tool name', () => {
    it('keeps the steps that called any tool chosen, by the name the cards show', () => {
      expect(ids(TIMELINE, '', only({ tool: ['Bash'] }))).toEqual([prompt.id]);
      expect(ids(TIMELINE, '', only({ tool: ['Read'] }))).toEqual([results.id]);
      expect(ids(TIMELINE, '', only({ tool: ['github · create_issue'] }))).toEqual([results.id]);
      expect(ids(TIMELINE, '', only({ tool: ['Bash', 'Grep'] }))).toEqual([prompt.id, subagent.id]);
    });
    it('keeps nothing for a tool no step called', () => {
      expect(ids(TIMELINE, '', only({ tool: ['Teleport'] }))).toEqual([]);
    });
  });

  describe('link type', () => {
    it('tells generated, temporal, verified and publisher-asserted links apart', () => {
      expect(ids(TIMELINE, '', only({ link: ['generated'] }))).toEqual([prompt.id, results.id, subagent.id]);
      expect(ids(TIMELINE, '', only({ link: ['temporal'] }))).toEqual([results.id]);
      expect(ids(TIMELINE, '', only({ link: ['verified'] }))).toEqual([mcp.id]);
      expect(ids(TIMELINE, '', only({ link: ['publisher-asserted'] }))).toEqual([asserted.id]);
      expect(ids(TIMELINE, '', only({ link: ['temporal', 'verified'] }))).toEqual([results.id, mcp.id]);
    });
    it('labels a link as its chip does: a legacy rewrite is "generated", and any derivation event is publisher-asserted', () => {
      const base = { interaction_id: 'x', git_commit_hash: H1 };
      expect(linkKindOf({ ...base, link_type: 'rewrite_exact' })).toBe('generated');
      expect(linkKindOf({ ...base, link_type: 'squash_exact' })).toBe('generated');
      expect(linkKindOf({ ...base, link_type: 'generated' })).toBe('generated');
      expect(linkKindOf({ ...base, link_type: 'temporal' })).toBe('temporal');
      expect(linkKindOf({ ...base, link_type: 'verified' })).toBe('verified');
      expect(linkKindOf(asserted.artifact_links[0])).toBe('publisher-asserted');
      expect(linkKindOf({ ...asserted.artifact_links[0], link_type: 'temporal' })).toBe('publisher-asserted');
    });
    it('keeps nothing for a step without links', () => {
      expect(ids([compaction], '', only({ link: ['generated'] }))).toEqual([]);
    });
  });

  describe('commit', () => {
    it('keeps the steps linked to a commit, by its full hash or the first characters of it', () => {
      expect(ids(TIMELINE, '', only({ commit: [H1] }))).toEqual([prompt.id, results.id, subagent.id]);
      expect(ids(TIMELINE, '', only({ commit: [H2.slice(0, 7)] }))).toEqual([results.id, mcp.id, asserted.id]);
      expect(ids(TIMELINE, '', only({ commit: [H1.toUpperCase()] }))).toEqual([prompt.id, results.id, subagent.id]);
      expect(ids(TIMELINE, '', only({ commit: [H1, H2] }))).toEqual([prompt.id, results.id, mcp.id, subagent.id, asserted.id]);
    });
    it('keeps nothing for a commit no step is linked to', () => {
      expect(ids(TIMELINE, '', only({ commit: ['0'.repeat(40)] }))).toEqual([]);
    });
  });

  describe('touches the open file', () => {
    it('keeps the steps that touched the file, whichever directory it was in on the machine that recorded it', () => {
      expect(ids(TIMELINE, '', NO_FILTERS, 'src/net/fetch.ts')).toEqual([prompt.id]);
      expect(ids(TIMELINE, '', NO_FILTERS, 'README.md')).toEqual([results.id]);
      expect(ids(TIMELINE, '', NO_FILTERS, 'src/net/Fetch.TS')).toEqual([prompt.id]);
    });
    it('matches whole path segments only', () => {
      expect(ids(TIMELINE, '', NO_FILTERS, 'etch.ts')).toEqual([]);
      expect(ids(TIMELINE, '', NO_FILTERS, 'net/fetch')).toEqual([]);
      expect(touchesFile('\0/work/repo/src/net/fetch.ts\0', 'fetch.ts')).toBe(true);
      expect(touchesFile('\0/work/repo/src/net/prefetch.ts\0', 'fetch.ts')).toBe(false);
      expect(touchesFile('\0src/net/fetch.ts\0', 'src/net/fetch.ts')).toBe(true);
      expect(touchesFile('\0src/net/fetch.ts\0', 'net/fetch.ts')).toBe(true);
      expect(touchesFile('', 'a.ts')).toBe(false);
      expect(touchesFile('\0a.ts\0', '')).toBe(false);
    });
    it('reads a Windows path and a leading ./ the same as the other', () => {
      expect(touchesFile('\0c:/work/repo/src/a.ts\0', '.\\src\\a.ts')).toBe(true);
    });
    it('finds the file in the context items and in the arguments of the calls alike', () => {
      const viaContext = makeNode({ id: 'c', context_items: [{ file_path: '/r/src/ctx.ts' }] });
      const viaArgs = makeNode({ id: 'a', tool_executions: [wireToolCall('Read', { file_path: '/r/src/ctx.ts' })] });
      expect(ids([viaContext, viaArgs], '', NO_FILTERS, 'src/ctx.ts')).toEqual(['c', 'a']);
    });
    it('means the file named, the open one, or none', () => {
      expect(effectiveFile({ mode: 'off' }, 'a.ts')).toBeNull();
      expect(effectiveFile({ mode: 'open' }, 'a.ts')).toBe('a.ts');
      expect(effectiveFile({ mode: 'open' }, null)).toBeNull();
      expect(effectiveFile({ mode: 'open' }, '')).toBeNull();
      expect(effectiveFile({ mode: 'path', path: 'b.ts' }, 'a.ts')).toBe('b.ts');
    });
  });

  it('needs one value of each kind chosen, and any of the values of one kind', () => {
    expect(ids(TIMELINE, '', only({ conv: [SESSION], origin: ['tool-results'] }))).toEqual([results.id, asserted.id]);
    expect(ids(TIMELINE, '', only({ conv: [SESSION], origin: ['tool-results'], tool: ['Read'] }))).toEqual([results.id]);
    expect(ids(TIMELINE, '', only({ conv: [AGENT], origin: ['tool-results'] }))).toEqual([]);
    expect(ids(TIMELINE, 'readme', only({ conv: [SESSION] }), 'README.md')).toEqual([results.id]);
    expect(ids(TIMELINE, 'jitter', only({ conv: [SESSION] }))).toEqual([prompt.id]);
    expect(ids(TIMELINE, 'jitter', only({ conv: [AGENT] }))).toEqual([]);
  });

  it('counts, compares and toggles filters', () => {
    expect(filterCount(NO_FILTERS)).toBe(0);
    expect(filterCount(only({ conv: ['a', 'b'], tool: ['Bash'], file: { mode: 'open' } }))).toBe(4);
    expect(sameFilters(only({ conv: ['a'] }), only({ conv: ['a'] }))).toBe(true);
    expect(sameFilters(only({ conv: ['a'] }), only({ conv: ['b'] }))).toBe(false);
    expect(sameFilters(only({ file: { mode: 'path', path: 'a' } }), only({ file: { mode: 'path', path: 'a' } }))).toBe(true);
    expect(sameFilters(only({ file: { mode: 'path', path: 'a' } }), only({ file: { mode: 'path', path: 'b' } }))).toBe(false);
    expect(sameFilters(only({ file: { mode: 'open' } }), only({ file: { mode: 'off' } }))).toBe(false);
    expect(toggled(['a'], 'b')).toEqual(['a', 'b']);
    expect(toggled(['a', 'b'], 'a')).toEqual(['b']);
  });
});

// ---------------------------------------------------------------------------

describe('what the filters offer', () => {
  const facets = buildFacets(getTimelineIndex(TIMELINE));
  const labels = (options: readonly { label: string }[]) => options.map((option) => option.label);

  it('names a subagent by its id, and every other conversation a session: Claude Code, the MCP server, the editor, `cvc run`', () => {
    const byValue = Object.fromEntries(facets.conversations.map((option) => [option.value, option.label]));
    expect(byValue[SESSION]).toBe('Session 5c1d8e2a');
    expect(byValue[AGENT]).toBe('Subagent ab12cd34');
    expect(byValue['agent-session-default']).toBe('Session agent-session-default');
  });

  it('does not read a name from what the steps of a conversation look like', () => {
    // Two conversations with ids of one shape: steps drawn as a transcript, and steps drawn as thought, action and observation.
    const transcript = makeNode({ id: harnessId(80), conversation_id: '11111111-2222-4333-8444-555555555555', author: 'human', user_prompt: 'x', tool_executions: [toolCall('Bash', {})] });
    const thought = makeNode({ id: '3b9f0f1e-6a4c-4d6e-9b1a-000000000081', conversation_id: '66666666-7777-4888-8999-aaaaaaaaaaaa', author: 'agent', user_prompt: 'x' });
    expect(labels(buildFacets(getTimelineIndex([transcript, thought])).conversations).sort()).toEqual(['Session 11111111', 'Session 66666666']);
  });

  it('counts the steps of each conversation, and lists them from the one that began first', () => {
    expect(facets.conversations.map((option) => [option.label, option.count])).toEqual([
      ['Session 5c1d8e2a', 4], // its earliest step is at 1000
      ['Subagent ab12cd34', 1], // 2000
      ['Session agent-session-default', 1], // 4000
    ]);
  });

  it('gives a subagent the session it belongs to as a note, and a session none', () => {
    const notes = Object.fromEntries(facets.conversations.map((option) => [option.label, option.note]));
    expect(notes['Subagent ab12cd34']).toBe('of session 5c1d8e2a');
    expect(notes['Session 5c1d8e2a']).toBeUndefined();
    expect(notes['Session agent-session-default']).toBeUndefined();
    // The session need not be in the timeline for the note to name it.
    const alone = buildFacets(getTimelineIndex([subagent])).conversations;
    expect(alone.map((option) => [option.label, option.note])).toEqual([['Subagent ab12cd34', 'of session 5c1d8e2a']]);
  });

  it('hints at a conversation with the first line of what its earliest step was asked', () => {
    const hints = Object.fromEntries(facets.conversations.map((option) => [option.label, option.hint]));
    expect(hints['Subagent ab12cd34']).toBe('Survey the repository for other helpers.');
    // The earliest step of the session is the one with the least recent time: `asserted`, a tool result with no prompt.
    expect(hints['Session 5c1d8e2a']).toBeUndefined();
    expect(hints['Session agent-session-default']).toBe('Review the change');
  });

  it('takes the hint from the prompt of a transcript step, and from the injected context when there is no prompt', () => {
    const prompted = makeNode({ id: harnessId(60), author: 'human', user_prompt: '\n\n  Survey the files.\nThen report.', model_response: 'x' });
    expect(conversationHint(prompted)).toBe('Survey the files.');
    const onlyMeta = makeNode({ id: harnessId(61), author: 'system', user_prompt: '[meta] Caveat: generated while running local commands.', model_response: 'x' });
    expect(conversationHint(onlyMeta)).toBe('Caveat: generated while running local commands.');
    const results = makeNode({ id: harnessId(62), author: 'system', user_prompt: '[tool_result Bash] ok', model_response: 'x' });
    expect(conversationHint(results)).toBeNull();
    expect(conversationHint(makeNode({ id: 'e', author: 'agent', user_prompt: '' }))).toBeNull();
    const long = makeNode({ id: 'l', author: 'agent', user_prompt: `\n\n  ${'word '.repeat(60)}\nsecond` });
    expect(conversationHint(long)!.length).toBeLessThanOrEqual(80);
    expect(conversationHint(long)!.startsWith('word word')).toBe(true);
  });

  it('names every session by its id, so that several in one timeline are never taken for one another', () => {
    const ids = ['5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70', '0b9e7d6c-1a2b-4c3d-8e4f-5a6b7c8d9e0f', '9a3e7c10-2b5d-4f68-8c1a-d4e5f6a7b8c9'];
    const sessions = ids.map((conversation_id, k) => makeNode({ id: harnessId(70 + k), conversation_id, author: 'human', user_prompt: 'x', tool_executions: [toolCall('Bash', {})] }));
    expect(labels(buildFacets(getTimelineIndex(sessions)).conversations).sort()).toEqual(['Session 0b9e7d6c', 'Session 5c1d8e2a', 'Session 9a3e7c10']);
  });

  it('cuts long ids where they stop being the same, and no further, and never gives two conversations the same name', () => {
    const first = `${'a'.repeat(20)}1${'b'.repeat(20)}`;
    const second = `${'a'.repeat(20)}2${'b'.repeat(20)}`;
    const sessions = [first, second].map((conversation_id, k) => makeNode({ id: `c${k}`, conversation_id, author: 'agent' }));
    const clashing = labels(buildFacets(getTimelineIndex(sessions)).conversations);
    expect(new Set(clashing).size).toBe(2);
    expect(clashing[0].length).toBeLessThan('Session '.length + 41);
    expect(clashing[0].startsWith('Session ')).toBe(true);

    // A subagent names its session as the session is named: with the same cut.
    const withSubagent = [...sessions, makeNode({ id: 's', conversation_id: `${second}:agent-aaaa1111`, author: 'human' })];
    const options = buildFacets(getTimelineIndex(withSubagent)).conversations;
    const sessionLabel = options.find((option) => option.value === second)!.label;
    const subagentNote = options.find((option) => option.value === `${second}:agent-aaaa1111`)!.note!;
    expect(subagentNote).toBe(`of session ${sessionLabel.slice('Session '.length)}`);
    expect(subagentNote.length).toBeGreaterThan('of session '.length + 8);
  });

  it('cuts the agent ids of subagents the same way, and numbers the same agent id under two sessions', () => {
    const agents = buildFacets(
      getTimelineIndex(['x:agent-12345678aaaa', 'y:agent-12345678bbbb'].map((conversation_id, k) => makeNode({ id: `s${k}`, conversation_id, author: 'human' }))),
    ).conversations;
    expect(labels(agents)).toEqual(['Subagent 12345678aaaa', 'Subagent 12345678bbbb']);
    expect(agents.map((option) => option.note)).toEqual(['of session x', 'of session y']);

    // The same agent id under two sessions is the same name twice: the later is numbered, and the notes tell them apart too.
    const twins = buildFacets(getTimelineIndex(['x:agent-abc', 'y:agent-abc'].map((conversation_id, k) => makeNode({ id: `t${k}`, conversation_id, author: 'human' })))).conversations;
    expect(labels(twins)).toEqual(['Subagent abc', 'Subagent abc (2)']);
    expect(twins.map((option) => option.note)).toEqual(['of session x', 'of session y']);
  });

  it('names a conversation that is not in the list the same way, from its id alone', () => {
    expect(conversationNameOf(SESSION)).toBe('Session 5c1d8e2a');
    expect(conversationNameOf('agent-session-default')).toBe('Session agent-session-default');
    expect(conversationNameOf(AGENT)).toBe('Subagent ab12cd34');
    expect(conversationNameOf('x:agent-abc')).toBe('Subagent abc');
    expect(conversationNameOf('x:agent-')).toBe('Subagent');
    expect(conversationNameOf('')).toBe('Unknown session');
  });

  it('knows a subagent conversation by the shape of its id', () => {
    expect(isSubagentConversation(AGENT)).toBe(true);
    expect(isSubagentConversation(SESSION)).toBe(false);
    expect(isSubagentConversation(':agent-x')).toBe(false);
    expect(isSubagentConversation('agent-x')).toBe(false);
  });

  it('names a conversation with no usable id', () => {
    const odd = makeNode({ id: 'odd', conversation_id: '' , author: 'agent' });
    expect(labels(buildFacets(getTimelineIndex([odd])).conversations)).toEqual(['Unknown session']);
    const bare = makeNode({ id: 'bare', conversation_id: 'x:agent-', author: 'human' });
    expect(labels(buildFacets(getTimelineIndex([bare])).conversations)).toEqual(['Subagent']);
  });

  it('offers the origins that are present, with their counts', () => {
    expect(facets.origins).toEqual([
      { value: 'prompt', label: 'Prompt', count: 2 },
      { value: 'tool-results', label: 'Tool results', count: 2 },
      { value: 'other', label: 'Other', count: 2 },
    ]);
    expect(buildFacets(getTimelineIndex([compaction])).origins).toEqual([{ value: 'other', label: 'Other', count: 1 }]);
  });

  it('offers the tools called, the most used first', () => {
    expect(facets.tools.map((option) => [option.value, option.count])).toEqual([
      ['Bash', 1],
      ['Edit', 1],
      ['github · create_issue', 1],
      ['Grep', 1],
      ['Read', 1],
    ]);
    const busy = makeNode({ id: 'b', tool_executions: [toolCall('Bash', {}), toolCall('Bash', {}), toolCall('Edit', {})] });
    expect(buildFacets(getTimelineIndex([busy, busy])).tools.map((option) => [option.value, option.count])).toEqual([
      ['Bash', 2],
      ['Edit', 2],
    ]);
  });

  it('offers the kinds of link that are present', () => {
    expect(facets.links.map((option) => [option.value, option.count])).toEqual([
      ['generated', 3],
      ['temporal', 1],
      ['verified', 1],
      ['publisher-asserted', 1],
    ]);
    expect(buildFacets(getTimelineIndex([compaction])).links).toEqual([]);
  });

  it('offers the commits of the pull request, newest first, with how many steps each holds and the subject', () => {
    const commits = [
      { sha: H1, subject: 'feat: retry limit' },
      { sha: H2, subject: 'fix: backoff' },
      { sha: 'c'.repeat(40), subject: 'chore: nothing links here' },
    ];
    const offered = buildFacets(getTimelineIndex(TIMELINE), commits).commits;
    expect(offered.map((option) => [option.label, option.count, option.hint])).toEqual([
      ['cccccccc'.slice(0, 7), 0, 'chore: nothing links here'],
      [H2.slice(0, 7), 3, 'fix: backoff'],
      [H1.slice(0, 7), 3, 'feat: retry limit'],
    ]);
    expect(offered.map((option) => option.value)).toEqual(['c'.repeat(40), H2, H1]);
  });

  it('calls the merge commit of a merged pull request that, since it has no subject of its own', () => {
    const offered = buildFacets(getTimelineIndex(TIMELINE), [{ sha: H1, subject: 'a' }, { sha: H2, merge: true }]).commits;
    expect(offered[0].hint).toBe('merge commit');
  });

  it('offers the commits the steps are linked to when the pull request is not known', () => {
    const offered = buildFacets(getTimelineIndex(TIMELINE)).commits;
    expect(offered.map((option) => [option.value, option.count, option.hint])).toEqual([
      [H1, 3, undefined],
      [H2, 3, undefined],
    ]);
  });
});

// ---------------------------------------------------------------------------

describe('a long timeline', () => {
  const fixture = searchFixture(300);
  const sorted = [...fixture.nodes].sort((a, b) => b.timestamp - a.timestamp);

  it('narrows to the steps that touched a file, newest first', () => {
    expect(fixture.fileSteps).toHaveLength(3);
    expect(ids(sorted, 'path:refund_handler')).toEqual(fixture.fileSteps);
    expect(ids(sorted, '', NO_FILTERS, FIXTURE_PATH)).toEqual(fixture.fileSteps);
    // A plain term finds those steps and the ones that read the result of their edit.
    const plain = ids(sorted, 'refund_handler');
    for (const id of fixture.fileSteps) expect(plain).toContain(id);
    expect(plain.length).toBeGreaterThan(fixture.fileSteps.length);
    expect(plain.length).toBeLessThan(15);
  });

  it('keeps a subagent to its own steps', () => {
    for (const [conversation, steps] of Object.entries(fixture.subagentSteps)) {
      expect(steps.length).toBeGreaterThan(10);
      expect(ids(sorted, '', only({ conv: [conversation] }))).toEqual(steps);
    }
  });

  it('answers a query in well under a frame', () => {
    const index = getTimelineIndex(sorted);
    runSearch(index, { query: parseQuery(''), filters: NO_FILTERS, file: null });
    forgetLastSearch(index);
    const started = performance.now();
    for (let run = 0; run < 20; run++) {
      forgetLastSearch(index);
      runSearch(index, { query: parseQuery('refund_handler'), filters: NO_FILTERS, file: null });
    }
    expect((performance.now() - started) / 20).toBeLessThan(25);
  });
});

// ---------------------------------------------------------------------------

describe('the chips', () => {
  const facets = buildFacets(getTimelineIndex(TIMELINE), [{ sha: H1, subject: 'feat' }]);

  it('names each filter that is on, in the order the panel lists them', () => {
    const chips = filterChips(
      only({ conv: [AGENT], origin: ['prompt'], tool: ['Bash'], link: ['publisher-asserted'], commit: [H1], file: { mode: 'path', path: 'src/a.ts' } }),
      facets,
      null,
    );
    expect(chips.map((chip) => `${chip.kind}: ${chip.label}`)).toEqual([
      'Conversation: Subagent ab12cd34',
      'Origin: Prompt',
      'Tool: Bash',
      'Link: Publisher-asserted',
      `Commit: ${H1.slice(0, 7)}`,
      'File: src/a.ts',
    ]);
    expect(new Set(chips.map((chip) => chip.key)).size).toBe(chips.length);
  });

  it('names a filter whose value the steps do not have, rather than leaving it out', () => {
    const chips = filterChips(only({ conv: ['0b9e7d6c-1a2b-4c3d-8e4f-5a6b7c8d9e0f', 'short', 'zz:agent-0123456789ab'] }), facets, null);
    expect(chips.map((chip) => chip.label)).toEqual(['Session 0b9e7d6c', 'Session short', 'Subagent 01234567']);
  });

  it('means the open file, by name, for a filter that follows it, and says so when none is open', () => {
    expect(filterChips(only({ file: { mode: 'open' } }), facets, 'src/open.ts')[0]).toMatchObject({ group: 'file', label: 'src/open.ts', value: 'src/open.ts' });
    expect(filterChips(only({ file: { mode: 'open' } }), facets, null)[0].label).toBe('the open file');
  });

  it('takes a chip out of the filters, and nothing else', () => {
    const filters = only({ conv: [SESSION, AGENT], tool: ['Bash', 'Read'], file: { mode: 'open' } });
    const chips = filterChips(filters, facets, 'a.ts');
    const byKey = (key: string) => chips.find((chip) => chip.key === key)!;
    expect(withoutChip(filters, byKey(`conv:${AGENT}`)).conv).toEqual([SESSION]);
    expect(withoutChip(filters, byKey('tool:Bash')).tool).toEqual(['Read']);
    expect(withoutChip(filters, byKey('file')).file).toEqual({ mode: 'off' });
    expect(withoutChip(filters, byKey('file')).tool).toEqual(['Bash', 'Read']);
  });

  it('adds a value to one kind of filter and takes it out again, leaving the other kinds alone', () => {
    const on = withToggled(NO_FILTERS, 'tool', 'Bash');
    expect(on).toEqual({ ...NO_FILTERS, tool: ['Bash'] });
    expect(withToggled(on, 'tool', 'Read').tool).toEqual(['Bash', 'Read']);
    expect(withToggled(on, 'tool', 'Bash')).toEqual(NO_FILTERS);
    expect(withToggled(on, 'link', 'temporal')).toEqual({ ...NO_FILTERS, tool: ['Bash'], link: ['temporal'] });
    expect(NO_FILTERS.tool).toEqual([]); // never changed in place
  });

  it('counts a filter that follows the open file only while a file is open', () => {
    expect(activeFilterCount(only({ file: { mode: 'open' } }), null)).toBe(0);
    expect(activeFilterCount(only({ file: { mode: 'open' } }), 'a.ts')).toBe(1);
    expect(activeFilterCount(only({ file: { mode: 'path', path: 'b.ts' } }), null)).toBe(1);
    expect(activeFilterCount(only({ tool: ['Bash'], conv: ['x'] }), null)).toBe(2);
  });
});

describe('terms: the work, and what is said of the terms that are not used', () => {
  it('says there were more different terms than are used, and not when only the same ones were repeated', () => {
    const terms = Array.from({ length: 16 }, (_, k) => `t${k}`);
    expect(parseQuery(terms.join(' ')).dropped).toBeUndefined();
    expect(parseQuery(`${terms.join(' ')} ${terms.join(' ')}`).dropped).toBeUndefined();
    const more = parseQuery(`${terms.join(' ')} t16 t17`);
    expect(more.dropped).toBe(true);
    expect(more.terms.map((term) => term.text)).toEqual(terms);
    // A term that is nothing (a bare prefix, an empty pair, a phrase of spaces) is not one that was dropped.
    expect(parseQuery(`${terms.join(' ')} tool: "" "   "`).dropped).toBeUndefined();
    // The same text under another prefix is a different term.
    expect(parseQuery(`${terms.join(' ')} path:t0`).dropped).toBe(true);
  });

  it('reads a megabyte of one term repeated, or of colons, in a fraction of a second', () => {
    const started = performance.now();
    expect(parseQuery('ab '.repeat(350_000)).terms).toHaveLength(1);
    expect(parseQuery(`${'x'.repeat(500_000)}:${'y '.repeat(250_000)}`).terms.length).toBeLessThanOrEqual(16);
    expect(parseQuery('a'.repeat(1_000_000)).terms[0].text).toHaveLength(512);
    // A third of a second on a machine with nothing else to do; a quadratic reading of a megabyte takes minutes.
    expect(performance.now() - started).toBeLessThan(5_000);
  });

  it('does not take a phrase of nothing but spaces, or of spaces and hidden characters, for a term', () => {
    for (const query of ['" "', '"   "', '"\u00a0 \u200b"', 'path:"  "', 'tool:" "']) expect(parseQuery(query).terms, query).toEqual([]);
    expect(parseQuery('" a "').terms.map((term) => term.text)).toEqual([' a ']);
  });
});

describe('what a step matches in that its card does not draw', () => {
  const withContext = makeNode({ id: 'c', context_items: [{ file_path: '/work/Repo/src/Secret_Module.ts' }], user_prompt: 'hello' });
  const matched = (query: string, node = withContext) => undrawnMatches(node, highlightTerms(parseQuery(query)));

  it('is the files of the context of the step, for a term that is plain or of a path', () => {
    expect(matched('secret_module')).toEqual(['context file paths']);
    expect(matched('path:src/secret')).toEqual(['context file paths']);
    expect(matched('SRC\\SECRET')).toEqual(['context file paths']); // either separator
  });

  it('is nothing for a term that is elsewhere, for a term of another prefix, and when nothing is searched', () => {
    expect(matched('hello')).toEqual([]);
    expect(matched('tool:secret_module')).toEqual([]);
    expect(matched('commit:secret')).toEqual([]);
    expect(undrawnMatches(withContext, null)).toEqual([]);
  });

  it('does not fail on a step with no usable context', () => {
    expect(matched('x', makeNode({ id: 'n', context_items: [null as unknown as { file_path: string }, { file_path: 7 as unknown as string }] }))).toEqual([]);
    expect(matched('x', makeNode({ id: 'n', context_items: 'no' as unknown as [] }))).toEqual([]);
  });
});
