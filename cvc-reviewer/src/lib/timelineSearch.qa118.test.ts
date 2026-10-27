/**
 * (Revision round: the rows that recorded how Unicode matched BEFORE the one fold, the oracle's idea of what the index
 * holds, and the tests of the arguments were brought up to the decisions of that round; each is marked "round 3" below.)
 *
 * Independent QA of the search library for issue #118 (src/lib/timelineSearch.ts), written without reading
 * the implementer's tests: the grammar's edge cases, what Unicode does to a match, the index and the filters
 * checked against a brute-force oracle that looks at the steps themselves, and the shortcut that narrows a
 * search from the one before it (checked against searching afresh, over random typing).
 *
 * The oracle is deliberately naive: it keeps no index, reads each step once per query, and shares with the
 * implementation only what is tested elsewhere (`normalizeForSearch`, which strips hidden characters and folds
 * case, `toolTitle`, `linkKindOf` and the stimulus classification the origin filter uses).
 */
import { describe, expect, it, vi } from 'vitest';
import {
  buildFacets,
  forgetLastSearch,
  getTimelineIndex,
  linkKindOf,
  MAX_QUERY_TERMS,
  MAX_TERM_LENGTH,
  NO_FILTERS,
  NO_QUERY,
  normalizeForSearch,
  normalizePath,
  parseQuery,
  runSearch,
  type Filters,
  type ParsedQuery,
  type QueryScope,
  type QueryTerm,
} from './timelineSearch';
import { toolCallPieces, toolTitle } from './toolCalls';
import { getActionLines, getObservation, getThought, showsReasoning } from './triadNode';
import { groupStimulus, hasText, nodeLayout, parseStimulus, stimulusOrigin } from './transcriptNode';
import { FIXTURE_SESSION, FIXTURE_SUBAGENTS, searchFixture } from '../test/searchFixtures';
import { generatedLink, makeNode, rawToolCall, toolCall } from '../test/fixtures';
import type { InteractionNode } from '../types/cvc';

// The oracle comparisons below take a couple of seconds on one slow core: no test of this file should time out for that.
vi.setConfig({ testTimeout: 30_000 });

// ---------------------------------------------------------------------------
// A seeded random source, so a failure can be replayed
// ---------------------------------------------------------------------------

function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const search = (nodes: InteractionNode[], query: string, filters: Filters = NO_FILTERS, file: string | null = null) =>
  runSearch(getTimelineIndex(nodes), { query: parseQuery(query), filters, file }).ids;

const terms = (query: string) => parseQuery(query).terms.map((term) => (term.scope === null ? term.text : `${term.scope}:${term.text}`));

// ---------------------------------------------------------------------------
// The grammar
// ---------------------------------------------------------------------------

describe('the query grammar: quotes', () => {
  it.each([
    ['an open quote runs to the end', '"retry limit', ['retry limit']],
    ['a closed phrase and a word', '"retry limit" tests', ['retry limit', 'tests']],
    ['a quote in the middle of a word is a character of it', 'retry" limit', ['retry"', 'limit']],
    ['a quote at the end of a word is a character of it', 'a"', ['a"']],
    ['two phrases that touch are two terms', '"a""b"', ['a', 'b']],
    ['an open quote after a closed phrase', '"a" "b', ['a', 'b']],
    ['a phrase straight after a word with no space', 'a"b c"', ['a"b', 'c"']],
    ['a lone quote is nothing yet', '"', []],
    ['an empty pair is nothing', '""', []],
    ['an empty pair beside a word', '"" word', ['word']],
    ['a phrase of only spaces is not a term (round 3: it would match every text with a space in it)', '" "', []],
    ['a phrase of spaces, no-break spaces and hidden characters is not a term either', '"  \u00a0\u200b "', []],
    ['the white space of a phrase is kept, even at its ends', '" a b "', [' a b ']],
    ['single quotes do not group', "'single quotes'", ["'single", "quotes'"]],
    ['a phrase after a prefix, closed', 'path:"my dir/a.ts" x', ['path:my dir/a.ts', 'x']],
    ['a phrase after a prefix, left open', 'path:"my dir/a.ts', ['path:my dir/a.ts']],
    ['a prefix with an empty pair of quotes is nothing', 'tool:""', []],
    ['a prefix with a lone quote is nothing', 'tool:"', []],
    ['a closed prefixed phrase then a stray quote', 'tool:"a"b"', ['tool:a', 'b"']],
    ['a backslash is only a backslash', '"a\\" b"', ['a\\', 'b"']],
  ])('%s', (_name, input, expected) => {
    expect(terms(input)).toEqual(expected);
  });
});

describe('the query grammar: prefixes', () => {
  it.each([
    ['a bare prefix is nothing yet', 'tool:', []],
    ['a bare prefix in capitals', 'TOOL:', []],
    ['a bare prefix and a space: the value is the next word, and a plain one', 'tool: bash', ['bash']],
    ['a prefix and its value in any case', 'ToOl:BaSh', ['tool:bash']],
    ['a repeated prefix: the second is a character of the value', 'tool:tool:bash', ['tool:tool:bash']],
    ['a repeated prefix of another kind', 'path:commit:abc', ['path:commit:abc']],
    ['the same prefix twice is two terms', 'tool:read tool:edit', ['tool:read', 'tool:edit']],
    ['the same term twice is one', 'tool:read tool:read', ['tool:read']],
    ['the same text under two prefixes is two terms', 'tool:read path:read', ['tool:read', 'path:read']],
    ['the same text plain and under a prefix is two terms', 'read tool:read', ['read', 'tool:read']],
    ['an unknown prefix is plain text, whole', 'file:src', ['file:src']],
    ['a url is plain text', 'http://example.com/a?b=c', ['http://example.com/a?b=c']],
    ['a drive letter is plain text, and keeps its backslash', 'c:\\dir\\a.ts', ['c:\\dir\\a.ts']],
    ['a colon at the start is plain text', ':tool', [':tool']],
    ['a lone colon', ':', [':']],
    ['a prefix with a space before the colon is two words', 'tool :bash', ['tool', ':bash']],
    ['a prefix in the middle of a word is text', 'xtool:bash', ['xtool:bash']],
    ['a path value is written as the index writes paths', 'path:.\\src\\A.ts', ['path:src/a.ts']],
    ['a commit value is lower case', 'commit:ABCDEF1', ['commit:abcdef1']],
    ['a prefix whose value is only hidden characters is nothing', 'tool:\u200b\u202e', []],
  ])('%s', (_name, input, expected) => {
    expect(terms(input)).toEqual(expected);
  });
});

describe('the query grammar: white space, hidden characters and caps', () => {
  it.each([
    ['empty', ''],
    ['spaces', '     '],
    ['tabs and new lines', ' \t\r\n '],
    ['a no-break space', '\u00a0'],
    ['an ideographic space', '\u3000'],
    ['line and paragraph separators', '\u2028\u2029'],
    ['a zero-width space', '\u200b'],
    ['a byte order mark', '\ufeff'],
    ['a right-to-left override', '\u202e'],
    ['a soft hyphen', '\u00ad'],
    ['a NUL', '\u0000'],
    ['a C1 control (next line)', '\u0085'],
    ['hidden characters and spaces', ' \u200b \u202e\u2060 '],
    ['a tag character (outside the BMP)', '\u{e0041}'],
  ])('%s is no query at all', (_name, input) => {
    expect(parseQuery(input)).toBe(NO_QUERY);
  });

  it('is not fooled by a hidden character inside a word or a phrase', () => {
    expect(terms('pa\u200bss\u202ewo\u00adrd')).toEqual(['password']);
    expect(terms('"pa\u200bss word"')).toEqual(['pass word']);
    expect(terms('pa\u200b ss')).toEqual(['pa', 'ss']);
  });

  it('keeps at most sixteen terms, the first ones, and one of each', () => {
    const many = Array.from({ length: 40 }, (_, k) => `t${k}`).join(' ');
    expect(terms(many)).toEqual(Array.from({ length: MAX_QUERY_TERMS }, (_, k) => `t${k}`));
    expect(MAX_QUERY_TERMS).toBe(16);
    // Repeats do not use up the sixteen.
    const repeated = `${'a '.repeat(30)}${Array.from({ length: 20 }, (_, k) => `b${k}`).join(' ')}`;
    expect(terms(repeated)).toEqual(['a', ...Array.from({ length: 15 }, (_, k) => `b${k}`)]);
  });

  it('cuts a term at 512 characters, and never in the middle of a surrogate pair', () => {
    expect(MAX_TERM_LENGTH).toBe(512);
    expect(parseQuery('x'.repeat(5_000)).terms[0].text).toHaveLength(512);
    expect(parseQuery(`"${'y'.repeat(5_000)}"`).terms[0].text).toHaveLength(512);
    const pairAtTheEdge = `${'a'.repeat(511)}😀${'b'.repeat(10)}`;
    const cut = parseQuery(pairAtTheEdge).terms[0].text;
    expect(cut).toBe('a'.repeat(511));
    expect(cut.charCodeAt(cut.length - 1)).toBe(97);
  });

  it('reads the longest query the box lets in (500 characters) in a few milliseconds, whatever it holds', () => {
    const shapes = ['a '.repeat(250), 'a:'.repeat(250), '"'.repeat(500), 'tool:'.repeat(100), `"${'ab '.repeat(166)}`, 'x'.repeat(500), 'tool:"'.repeat(80)];
    for (const shape of shapes) {
      const input = shape.slice(0, 500);
      const started = performance.now();
      parseQuery(input);
      expect(performance.now() - started, input.slice(0, 20)).toBeLessThan(200);
    }
  });

  it('stays linear enough in the number of words that a pasted megabyte does not stall the page (the box caps it at 500)', () => {
    // parseQuery is exported and takes any string; the caps (500 characters) are the box's and the address's.
    const started = performance.now();
    parseQuery('ab '.repeat(30_000));
    expect(performance.now() - started).toBeLessThan(2_000);
  });

  it('is total: any text at all is a query and none of it throws', () => {
    const rand = rng(11);
    const alphabet = ['a', 'B', ' ', '"', ':', 'tool:', 'path:', 'commit:', '\u200b', '\t', '\\', '😀', '\ud800', '\udc00', 'İ', 'ß', '\n', '%', '.', '*', '('];
    for (let k = 0; k < 3_000; k++) {
      let input = '';
      for (let n = rand() * 30; n > 0; n--) input += alphabet[Math.floor(rand() * alphabet.length)];
      const parsed = parseQuery(input);
      expect(parsed.terms.length).toBeLessThanOrEqual(MAX_QUERY_TERMS);
      for (const term of parsed.terms) {
        expect(term.text).not.toBe('');
        expect(term.text.length).toBeLessThanOrEqual(MAX_TERM_LENGTH);
      }
    }
  });
});

// ---------------------------------------------------------------------------
// Unicode: what a match is, written down
// ---------------------------------------------------------------------------

/**
 * Round 3: matching is the one fold of `lib/searchFold` on both sides: full case folding (ß is "ss", the final sigma is a
 * sigma), compatibility forms per code point (full-width letters, ligatures, precomposed accents), every White_Space
 * character as a space, hidden characters left out, and a plain substring test. These rows record what that does. (Before
 * the round they recorded `toLowerCase` and nothing more; the rows whose answer changed say so.)
 */
describe('Unicode: what a match is', () => {
  const found = (text: string, needle: string) => search([makeNode({ id: 'n', model_response: text })], needle).length === 1;

  it.each([
    // Turkish: a dotted capital I folds to "i" and a combining dot, so it is not an "i"; a dotless ı is its own letter.
    ['İstanbul', 'İstanbul', true],
    ['İstanbul', 'istanbul', false],
    ['İ', 'i', true],
    ['ISTANBUL', 'ıstanbul', false],
    ['ISTANBUL', 'istanbul', true],
    ['ıstanbul', 'ISTANBUL', false],
    // German: ß is "ss" in both directions; the capital ẞ folds the same way.
    ['straße', 'strasse', true], // was false
    ['STRASSE', 'straße', true], // was false
    ['Straße', 'STRAẞE', true],
    ['STRASSE', 'strasse', true],
    // Greek: a sigma is a sigma at the end of a word and in the middle of one, so a term can start or end anywhere.
    ['ΟΔΥΣΣΕΥΣ', 'οδυσσευς', true],
    ['ΟΔΥΣΣΕΥΣ', 'οδυσσευσ', true], // was false
    ['οδυσσευς', 'ΟΔΥΣΣΕΥΣ', true],
    // Normalisation: a precomposed é and an e with a combining acute accent are the same text, and a term without the accent finds both.
    ['café', 'café', true],
    ['caf\u00e9', 'cafe\u0301', true], // was false
    ['cafe\u0301', 'caf\u00e9', true], // was false
    ['cafe\u0301', 'cafe', true],
    ['caf\u00e9', 'cafe', true], // was false
    // Compatibility forms are folded.
    ['\ufb01le', 'file', true], // was false
    ['file', '\ufb01le', true], // was false
    ['\uff21\uff22\uff23', 'abc', true], // was false
    // White space: a no-break space is a space, in a quoted phrase too.
    ['retry\u00a0limit', '"retry limit"', true], // was false
    ['retry\u00a0limit', 'retry limit', true],
    ['retry limit', '"retry\u00a0limit"', true], // was false
    // A letter of another script that looks like a Latin one is not that letter.
    ['\u0430pple', 'apple', false],
    ['apple', '\u0430pple', false],
    // Plain cases that must work.
    ['ǅ', 'ǆ', true],
    ['日本語のテスト', '本語', true],
    ['😀 smile', '😀', true],
    ['MiXeD CaSe', 'mixed case', true],
  ])('%j searched for %j: %s', (text, needle, expected) => {
    expect(found(text, needle)).toBe(expected);
  });

  it('is not lengthened by the dotted capital I: the term and the text are lower-cased the same way', () => {
    expect(normalizeForSearch('İ')).toBe('i\u0307');
    expect(normalizeForSearch('İ')).toHaveLength(2);
    expect(parseQuery('İ').terms[0].text).toBe('i\u0307');
  });

  it('finds a text by a hidden character in the middle of what was typed, or in the text', () => {
    expect(found('the pass\u200bword is x', 'password')).toBe(true);
    expect(found('the password is x', 'pass\u200bword')).toBe(true);
    expect(found('rm -rf\u202e ./build', 'rm -rf ./build')).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// An oracle
// ---------------------------------------------------------------------------

const PATH_KEYS = ['file_path', 'path', 'notebook_path'];

interface Haystacks {
  text: string[];
  tools: string[];
  paths: string[];
  commits: string[];
}

/**
 * Round 3: what the card draws, read off the data. A step that draws the stimulus in parts (the transcript layout) holds
 * those parts, its reasoning and response when they have any text, and what each call draws; a step in the Thought / Action
 * / Observation layout holds the prompt, the raw line of each call, the response, and the reasoning of an AI agent. The
 * markers of the wire, the placeholders and the names of arguments are in neither. (The pieces of a call are the ones
 * `toolCallPieces` gives, which has tests of its own.)
 */
function haystacksOf(node: InteractionNode): Haystacks {
  const text: string[] = [];
  const tools: string[] = [];
  const paths: string[] = [];
  if (nodeLayout(node) === 'transcript') {
    for (const block of groupStimulus(parseStimulus(node.user_prompt, { interruptionBoundary: node.author === 'human' }))) {
      if (block.kind === 'prompt' || block.kind === 'meta' || block.kind === 'compaction') text.push(block.text);
      if (block.kind === 'tool_results') for (const result of block.results) text.push(...(result.kind === 'tool_result' ? [toolTitle(result.tool), result.text] : [result.text]));
    }
    if (hasText(node.model_cot)) text.push(node.model_cot);
    if (hasText(node.model_response)) text.push(node.model_response);
  } else {
    const thought = getThought(node);
    if (thought.recorded) text.push(thought.text);
    for (const line of getActionLines(node)) if (line.kind === 'tool' && line.args !== '') text.push(line.args);
    const observation = getObservation(node);
    if (observation.recorded) text.push(observation.text);
    if (showsReasoning(node)) text.push(node.model_cot as string);
  }
  for (const exec of node.tool_executions) {
    if (exec.tool_name !== '') tools.push(normalizeForSearch(exec.tool_name));
    tools.push(normalizeForSearch(toolTitle(exec.tool_name)));
    for (const piece of toolCallPieces(exec)) {
      if (piece.scope === 'path') paths.push(normalizePath(piece.text));
      else if (piece.scope !== 'tool' && nodeLayout(node) === 'transcript') text.push(...(piece.lines ? piece.text.split('\n').filter((line) => line !== '') : [piece.text]));
    }
  }
  for (const item of node.context_items) paths.push(normalizePath(item.file_path));
  return { text: text.map(normalizeForSearch).filter((piece) => piece !== ''), tools, paths: paths.filter((path) => path !== ''), commits: node.artifact_links.map((link) => normalizeForSearch(link.git_commit_hash)) };
}

function termFound(hay: Haystacks, term: QueryTerm): boolean {
  const has = (list: string[]) => list.some((item) => item.includes(term.text));
  switch (term.scope) {
    case 'tool':
      return has(hay.tools);
    case 'path':
      return has(hay.paths);
    case 'commit':
      return has(hay.commits);
    default:
      return has(hay.text) || has(hay.tools) || has(hay.paths) || has(hay.commits);
  }
}

function originOf(node: InteractionNode): string {
  if (nodeLayout(node) !== 'transcript') return 'other';
  const origin = stimulusOrigin(node, parseStimulus(node.user_prompt, { interruptionBoundary: node.author === 'human' }));
  return origin === 'Prompt' ? 'prompt' : origin === 'Tool results' ? 'tool-results' : 'other';
}

function passes(node: InteractionNode, filters: Filters, file: string | null): boolean {
  if (filters.conv.length > 0 && !filters.conv.includes(node.conversation_id)) return false;
  if (filters.origin.length > 0 && !filters.origin.includes(originOf(node) as never)) return false;
  if (filters.tool.length > 0 && !node.tool_executions.some((exec) => filters.tool.includes(toolTitle(exec.tool_name)))) return false;
  if (filters.link.length > 0 && !node.artifact_links.some((link) => filters.link.includes(linkKindOf(link)))) return false;
  if (filters.commit.length > 0 && !node.artifact_links.some((link) => filters.commit.some((wanted) => link.git_commit_hash.toLowerCase().startsWith(wanted.toLowerCase())))) return false;
  if (file !== null) {
    const wanted = normalizePath(file);
    const touched = haystacksOf(node).paths;
    if (!touched.some((path) => path === wanted || path.endsWith(`/${wanted}`))) return false;
  }
  return true;
}

function oracle(nodes: InteractionNode[], query: ParsedQuery, filters: Filters = NO_FILTERS, file: string | null = null): string[] {
  return nodes
    .filter((node) => passes(node, filters, file))
    .filter((node) => {
      const hay = haystacksOf(node);
      return query.terms.every((term) => termFound(hay, term));
    })
    .map((node) => node.id);
}

// ---------------------------------------------------------------------------
// Random steps
// ---------------------------------------------------------------------------

const WORDS = ['alpha', 'Beta', 'GAMMA', 'delta', 'retry', 'limit', 'Refund', 'handler', 'Cart', 'index.ts', 'src/cart/index.ts', 'C:\\Users\\Dev\\Cart\\a.ts', './rel/Path.ts', 'reindex.ts', 'xsrc/cart/index.ts', 'tool_result', 'needle', 'İstanbul', 'straße', 'e\u0301'];
const HIDDEN = ['\u200b', '\u202e', '\u00ad', '\ufeff'];
const CONVERSATIONS = ['s1', 's1:agent-aaaa1111', 's1:agent-bbbb2222', 's2'];
const TOOL_NAMES = ['Bash', 'Read', 'Edit', 'Grep', 'mcp__github__create_issue', 'server:web_search', 'Write'];
const HASHES = ['a1b2c3d4e5f60718293a4b5c6d7e8f9012345678', 'B2C3D4E5F60718293A4B5C6D7E8F901234567890', 'c3d4e5f60718293a4b5c6d7e8f90123456789012'];

function randomText(rand: () => number, min = 0): string {
  const count = min + Math.floor(rand() * 6);
  let out = '';
  for (let k = 0; k < count; k++) {
    let word = WORDS[Math.floor(rand() * WORDS.length)];
    if (rand() < 0.1) {
      const at = Math.floor(rand() * word.length);
      word = word.slice(0, at) + HIDDEN[Math.floor(rand() * HIDDEN.length)] + word.slice(at);
    }
    out += (k === 0 ? '' : [' ', ' ', '\n', '\r\n', '\t'][Math.floor(rand() * 5)]) + word;
  }
  return out;
}

function randomArguments(rand: () => number): string {
  const roll = rand();
  if (roll < 0.1) return 'not json at all ' + randomText(rand);
  if (roll < 0.15) return JSON.stringify(['array', randomText(rand)]);
  if (roll < 0.2) return JSON.stringify({ bytes: 5000, cvc_truncated: true });
  if (roll < 0.25) return '';
  const value: Record<string, unknown> = {};
  for (const key of ['command', 'file_path', 'path', 'pattern', 'old_string', 'new_string', 'notebook_path', 'description']) {
    // (A path with a line break in it is a different matter: see qa118.regressions.test.tsx.)
    const oneLine = PATH_KEYS.includes(key);
    if (rand() < 0.3) value[key] = !oneLine && rand() < 0.5 ? randomText(rand, 1) : WORDS[Math.floor(rand() * WORDS.length)];
  }
  if (rand() < 0.3) value.todos = [{ content: randomText(rand, 1), status: 'pending', n: 7 }];
  if (rand() < 0.3) value.timeout = 120000;
  if (rand() < 0.3) value.run_in_background = true;
  if (rand() < 0.2) value.nested = { deeper: [randomText(rand, 1), { deepest: randomText(rand, 1) }] };
  return JSON.stringify(value);
}

function randomNodes(seed: number, count: number): InteractionNode[] {
  const rand = rng(seed);
  return Array.from({ length: count }, (_, k) => {
    const id = `n${seed}-${k}`;
    const hashes = [HASHES[Math.floor(rand() * HASHES.length)]];
    if (rand() < 0.3) hashes.push(HASHES[Math.floor(rand() * HASHES.length)]);
    const kinds = ['generated', 'temporal', 'verified', 'rewrite_exact'] as const;
    return makeNode({
      id,
      conversation_id: CONVERSATIONS[Math.floor(rand() * CONVERSATIONS.length)],
      timestamp: 1_800_000_000_000 - k * 1000,
      author: (['human', 'agent', 'system', 'external'] as const)[Math.floor(rand() * 4)],
      model_name: rand() < 0.7 ? 'claude-opus-4-5-20251101' : rand() < 0.5 ? 'agent' : null,
      user_prompt: rand() < 0.15 ? null : rand() < 0.4 ? `[tool_result Bash] ${randomText(rand)}` : randomText(rand),
      model_cot: rand() < 0.4 ? randomText(rand) : null,
      model_response: rand() < 0.8 ? randomText(rand) : null,
      context_items: rand() < 0.3 ? [{ file_path: rand() < 0.5 ? 'src/cart/index.ts' : '/home/dev/Cart/' + WORDS[Math.floor(rand() * 5)] + '.ts' }] : [],
      tool_executions: Array.from({ length: Math.floor(rand() * 3) }, () => {
        const name = TOOL_NAMES[Math.floor(rand() * TOOL_NAMES.length)];
        return rand() < 0.2 ? rawToolCall(name, randomArguments(rand)) : { ...toolCall(name, {}), arguments: randomArguments(rand) };
      }),
      artifact_links: hashes.map((hash) => ({ ...generatedLink(id, hash), link_type: kinds[Math.floor(rand() * kinds.length)] })),
    });
  });
}

/** A query as a person would type it: the characters of some terms, one after the other, some of them taken back. */
function typingSequence(rand: () => number, nodes: InteractionNode[]): string[] {
  const picks: string[] = [];
  const pool = nodes.flatMap((node) => [node.model_response ?? '', node.user_prompt ?? '']).join(' ').split(/\s+/).filter((word) => word.length > 2);
  const termCount = 1 + Math.floor(rand() * 3);
  for (let k = 0; k < termCount; k++) {
    const word = rand() < 0.7 && pool.length > 0 ? pool[Math.floor(rand() * pool.length)] : WORDS[Math.floor(rand() * WORDS.length)];
    const prefix = rand() < 0.35 ? ['path:', 'tool:', 'commit:'][Math.floor(rand() * 3)] : '';
    picks.push(prefix + (rand() < 0.15 ? `"${word} x` : word));
  }
  const sequence: string[] = [];
  let typed = '';
  for (const pick of picks) {
    for (const character of Array.from(pick)) {
      typed += character;
      sequence.push(typed);
      if (rand() < 0.12 && typed.length > 1) {
        typed = typed.slice(0, -1);
        sequence.push(typed);
      }
    }
    typed += ' ';
    sequence.push(typed);
  }
  return sequence;
}

/** A query edited the way a person edits one: a slice taken out, a word or a prefix put in, a prefix taken off, anywhere in it. */
function editingSequence(rand: () => number, nodes: InteractionNode[]): string[] {
  const pool = nodes.flatMap((node) => [node.model_response ?? '', node.user_prompt ?? '']).join(' ').split(/\s+/).filter((word) => word.length > 2);
  const word = () => (rand() < 0.7 && pool.length > 0 ? pool[Math.floor(rand() * pool.length)] : WORDS[Math.floor(rand() * WORDS.length)]);
  const prefix = () => ['path:', 'tool:', 'commit:', 'PATH:', 'Tool:', 'file:'][Math.floor(rand() * 6)];
  let query = [word(), `${prefix()}${word()}`, word()].slice(0, 1 + Math.floor(rand() * 3)).join(' ');
  const sequence = [query];
  for (let k = 0; k < 10; k++) {
    const at = Math.floor(rand() * (query.length + 1));
    const roll = rand();
    if (roll < 0.25) query = query.slice(0, at) + query.slice(at + 1 + Math.floor(rand() * 4));
    else if (roll < 0.5) query = `${query.slice(0, at)}${prefix()}${query.slice(at)}`;
    else if (roll < 0.7) query = query.replace(/^[A-Za-z]+:/, '').replace(/ [A-Za-z]+:/, ' ');
    else if (roll < 0.85) query = `${query.slice(0, at)} ${word()}${query.slice(at)}`;
    else query = `${query.slice(0, at)}"${query.slice(at)}`;
    sequence.push(query);
  }
  return sequence;
}

describe('the index and the search, against a brute-force oracle', () => {
  it('agrees with the oracle when a query is edited anywhere in it, a prefix put on and taken off a term in the middle', () => {
    let checked = 0;
    let nonEmpty = 0;
    for (let seed = 200; seed < 240; seed++) {
      const rand = rng(seed * 104729);
      const nodes = randomNodes(seed, 40);
      const index = getTimelineIndex(nodes);
      for (let run = 0; run < 6; run++) {
        for (const edited of editingSequence(rand, nodes)) {
          const query = parseQuery(edited);
          const expected = oracle(nodes, query);
          expect(runSearch(index, { query, filters: NO_FILTERS, file: null }).ids, `seed ${seed}, edited to ${JSON.stringify(edited)}`).toEqual(expected);
          checked++;
          if (expected.length > 0 && query.terms.length > 0) nonEmpty++;
        }
      }
    }
    expect(checked).toBeGreaterThan(2_000);
    expect(nonEmpty).toBeGreaterThan(200);
  });

  it('agrees with the oracle on random steps and random typing, with and without a search to narrow from', () => {
    let checked = 0;
    let nonEmpty = 0;
    for (let seed = 1; seed <= 25; seed++) {
      const rand = rng(seed * 7919);
      const nodes = randomNodes(seed, 40);
      const index = getTimelineIndex(nodes);
      for (let run = 0; run < 4; run++) {
        const sequence = typingSequence(rand, nodes);
        forgetLastSearch(index);
        for (const typed of sequence) {
          const query = parseQuery(typed);
          const expected = oracle(nodes, query);
          const got = runSearch(index, { query, filters: NO_FILTERS, file: null }).ids;
          expect(got, `seed ${seed}, typed ${JSON.stringify(typed)}`).toEqual(expected);
          // Searching afresh gives the same steps as the search that started from the last one.
          forgetLastSearch(index);
          expect(runSearch(index, { query, filters: NO_FILTERS, file: null }).ids, `fresh: seed ${seed}, typed ${JSON.stringify(typed)}`).toEqual(expected);
          checked++;
          if (expected.length > 0 && query.terms.length > 0) nonEmpty++;
          // Put the narrowing state back as the sequence had it.
          runSearch(index, { query, filters: NO_FILTERS, file: null });
        }
      }
    }
    expect(checked).toBeGreaterThan(800);
    // The comparison is not between empty lists.
    expect(nonEmpty).toBeGreaterThan(120);
  });

  it('agrees with the oracle when the filters change in the middle of the typing', () => {
    const rand = rng(424242);
    let checked = 0;
    for (let seed = 100; seed < 140; seed++) {
      const nodes = randomNodes(seed, 50);
      const index = getTimelineIndex(nodes);
      let filters: Filters = NO_FILTERS;
      let file: string | null = null;
      for (const typed of typingSequence(rand, nodes)) {
        const roll = rand();
        if (roll < 0.15) {
          const group = (['conv', 'origin', 'tool', 'link', 'commit'] as const)[Math.floor(rand() * 5)];
          const pools = {
            conv: CONVERSATIONS,
            origin: ['prompt', 'tool-results', 'other'],
            tool: TOOL_NAMES.map(toolTitle),
            link: ['generated', 'temporal', 'verified'],
            commit: HASHES.map((hash) => hash.slice(0, 7 + Math.floor(rand() * 5))),
          };
          const value = pools[group][Math.floor(rand() * pools[group].length)];
          const list = filters[group] as readonly string[];
          filters = { ...filters, [group]: list.includes(value) ? list.filter((item) => item !== value) : [...list, value] } as unknown as Filters;
        } else if (roll < 0.2) {
          file = file === null ? ['index.ts', 'src/cart/index.ts', 'Cart/alpha.ts', 'rel/path.ts'][Math.floor(rand() * 4)] : null;
        } else if (roll < 0.23) {
          filters = NO_FILTERS;
        }
        const query = parseQuery(typed);
        const expected = oracle(nodes, query, filters, file);
        expect(runSearch(index, { query, filters, file }).ids, `seed ${seed}, typed ${JSON.stringify(typed)}, filters ${JSON.stringify(filters)}, file ${file}`).toEqual(expected);
        checked++;
      }
    }
    expect(checked).toBeGreaterThan(600);
  });

  it('keeps the order of the steps it was given', () => {
    const nodes = randomNodes(5, 60);
    const ids = search(nodes, 'a');
    const order = new Map(nodes.map((node, at) => [node.id, at]));
    const positions = ids.map((id) => order.get(id)!);
    expect(positions).toEqual([...positions].sort((a, b) => a - b));
  });

  it('finds nothing in the names of the arguments, nor in the words of its options, and everything in the values as the card draws them', () => {
    // Round 3: the values are what the card shows, numbers included; the names are labels of the card, and the options are
    // the Reviewer's words ("runs in background") with a value after them where there is one ("timeout 120s").
    const node = makeNode({
      id: 'args',
      author: 'human',
      tool_executions: [toolCall('Bash', { command: 'echo hi', timeout: 120000, run_in_background: true, nested: { deep: ['inner value'] } })],
    });
    for (const absent of ['command', 'timeout', '120000', 'true', 'run_in_background', 'nested', 'runs in background']) expect(search([node], absent), absent).toEqual([]);
    for (const present of ['echo hi', 'inner value', 'bash', '120s', 'deep']) expect(search([node], present), present).toEqual(['args']);
  });

  it('does not search the arguments the capture dropped', () => {
    const dropped = makeNode({ id: 'dropped', author: 'human', tool_executions: [rawToolCall('Bash', JSON.stringify({ bytes: 123456, cvc_truncated: true }))] });
    expect(search([dropped], 'cvc_truncated')).toEqual([]);
    expect(search([dropped], '123456')).toEqual([]);
    // A thing that only looks like the sentinel is arguments like any other.
    const lookalike = makeNode({ id: 'lookalike', author: 'human', tool_executions: [rawToolCall('Bash', JSON.stringify({ bytes: 'x', cvc_truncated: true, extra: 'needle' }))] });
    expect(search([lookalike], 'needle')).toEqual(['lookalike']);
  });

  it('survives a step that holds anything at all', () => {
    const circular: Record<string, unknown> = {};
    circular.self = circular;
    const hostile = [
      makeNode({ id: 'a', tool_executions: [{ tool_protocol: 'native', tool_name: 'Bash', arguments: circular as unknown as string, status: 'success' }] }),
      makeNode({ id: 'b', tool_executions: 'not a list' as unknown as InteractionNode['tool_executions'] }),
      makeNode({ id: 'c', artifact_links: [null as unknown as InteractionNode['artifact_links'][number], { interaction_id: 'c', git_commit_hash: 42 as unknown as string, link_type: 'generated' }] }),
      makeNode({ id: 'd', context_items: [null as unknown as InteractionNode['context_items'][number], { file_path: 7 as unknown as string }] }),
      makeNode({ id: 'e', user_prompt: 12 as unknown as string, model_response: { text: 'x' } as unknown as string }),
      makeNode({ id: 'f', conversation_id: undefined as unknown as string }),
      makeNode({ id: 'g', tool_executions: [{ tool_protocol: 'native', tool_name: 7 as unknown as string, arguments: null as unknown as string, status: 'success' }] }),
      makeNode({ id: 'h', tool_executions: [rawToolCall('Bash', JSON.stringify({ command: 'needle' }))] }),
    ];
    expect(search(hostile, 'needle')).toEqual(['h']);
    expect(() => buildFacets(getTimelineIndex(hostile))).not.toThrow();
    expect(search(hostile, 'tool:bash')).toContain('h');
  });

  it('reads at most a bounded amount of a huge set of arguments, and still finds what is early in it', () => {
    const strings = Array.from({ length: 50_000 }, (_, k) => `value ${k}`);
    const node = makeNode({ id: 'huge', tool_executions: [rawToolCall('Bash', JSON.stringify({ first: 'findme', rest: strings }))] });
    const started = performance.now();
    expect(search([node], 'findme')).toEqual(['huge']);
    expect(performance.now() - started).toBeLessThan(3_000);
  });
});

// ---------------------------------------------------------------------------
// Filters over a long timeline
// ---------------------------------------------------------------------------

describe('filters, alone and together, over a 300-step timeline', () => {
  const { nodes: oldestFirst, commits } = searchFixture(300);
  const nodes = [...oldestFirst].sort((a, b) => b.timestamp - a.timestamp);
  const index = getTimelineIndex(nodes);
  const facets = buildFacets(index, commits);
  const ids = (filters: Partial<Filters>, query = '', file: string | null = null) => runSearch(index, { query: parseQuery(query), filters: { ...NO_FILTERS, ...filters }, file }).ids;
  const set = (list: string[]) => new Set(list);

  it('agrees with the oracle for every single value of every kind on offer', () => {
    let tried = 0;
    for (const option of facets.conversations) {
      expect(ids({ conv: [option.value] })).toEqual(oracle(nodes, NO_QUERY, { ...NO_FILTERS, conv: [option.value] }));
      tried++;
    }
    for (const option of facets.origins) {
      expect(ids({ origin: [option.value as never] })).toEqual(oracle(nodes, NO_QUERY, { ...NO_FILTERS, origin: [option.value as never] }));
      tried++;
    }
    for (const option of facets.tools) {
      expect(ids({ tool: [option.value] })).toEqual(oracle(nodes, NO_QUERY, { ...NO_FILTERS, tool: [option.value] }));
      tried++;
    }
    for (const option of facets.links) {
      expect(ids({ link: [option.value as never] })).toEqual(oracle(nodes, NO_QUERY, { ...NO_FILTERS, link: [option.value as never] }));
      tried++;
    }
    for (const option of facets.commits) {
      expect(ids({ commit: [option.value] })).toEqual(oracle(nodes, NO_QUERY, { ...NO_FILTERS, commit: [option.value] }));
      tried++;
    }
    expect(tried).toBeGreaterThan(15);
  });

  it('shows, beside each option, exactly the number of steps that ticking it gives', () => {
    for (const option of facets.conversations) expect(ids({ conv: [option.value] }), option.label).toHaveLength(option.count);
    for (const option of facets.origins) expect(ids({ origin: [option.value as never] }), option.label).toHaveLength(option.count);
    for (const option of facets.tools) expect(ids({ tool: [option.value] }), option.label).toHaveLength(option.count);
    for (const option of facets.links) expect(ids({ link: [option.value as never] }), option.label).toHaveLength(option.count);
    for (const option of facets.commits) expect(ids({ commit: [option.value] }), option.label).toHaveLength(option.count);
  });

  it('adds up: the conversations, and the origins, partition the steps', () => {
    expect(facets.conversations.reduce((sum, option) => sum + option.count, 0)).toBe(300);
    expect(facets.origins.reduce((sum, option) => sum + option.count, 0)).toBe(300);
  });

  it('takes the union of the values of one kind, for every pair of values on offer, in every kind', () => {
    const kinds = [
      ['conv', facets.conversations],
      ['origin', facets.origins],
      ['tool', facets.tools],
      ['link', facets.links],
      ['commit', facets.commits],
    ] as const;
    let pairs = 0;
    for (const [kind, options] of kinds) {
      for (let a = 0; a < options.length; a++) {
        for (let b = a + 1; b < options.length; b++) {
          const union = new Set([...ids({ [kind]: [options[a].value] }), ...ids({ [kind]: [options[b].value] })]);
          const both = ids({ [kind]: [options[a].value, options[b].value] });
          expect(new Set(both), `${kind}: ${options[a].label} + ${options[b].label}`).toEqual(union);
          expect(both).toHaveLength(union.size);
          pairs++;
        }
      }
    }
    expect(pairs).toBeGreaterThan(20);
  });

  it('takes any value of one kind (union) and one of each kind (intersection)', () => {
    const [a, b] = FIXTURE_SUBAGENTS;
    const both = ids({ conv: [a, b] });
    expect(set(both)).toEqual(new Set([...ids({ conv: [a] }), ...ids({ conv: [b] })]));
    const edits = ids({ tool: ['Edit'] });
    const session = ids({ conv: [FIXTURE_SESSION] });
    const together = ids({ conv: [FIXTURE_SESSION], tool: ['Edit'] });
    expect(set(together)).toEqual(new Set(edits.filter((id) => set(session).has(id))));
    expect(together.length).toBeGreaterThan(0);
  });

  it('means "no steps" for a combination nothing satisfies, and says so for every query on top of it', () => {
    // The subagents only Read and Grep.
    const [a] = FIXTURE_SUBAGENTS;
    expect(ids({ conv: [a], tool: ['Edit'] })).toEqual([]);
    expect(ids({ conv: [a], tool: ['Edit'] }, 'the')).toEqual([]);
    expect(ids({ conv: ['no-such-conversation'] })).toEqual([]);
    expect(ids({ tool: ['No Such Tool'] })).toEqual([]);
    expect(ids({ commit: ['deadbee'] })).toEqual([]);
  });

  it('narrows a query by the filters, and a filter by the query: the same steps either way round', () => {
    const query = 'refund_handler';
    const byQuery = ids({}, query);
    const byTool = ids({ tool: ['Edit'] });
    expect(set(ids({ tool: ['Edit'] }, query))).toEqual(new Set(byQuery.filter((id) => set(byTool).has(id))));
    // A file filter says the same as the path: prefix when the file is spelled as the steps spell it.
    expect(ids({}, '', 'src/payments/refund_handler.ts')).toEqual(ids({}, 'path:src/payments/refund_handler.ts').filter((id) => ids({}, '', 'src/payments/refund_handler.ts').includes(id)));
    expect(ids({}, '', 'src/payments/refund_handler.ts').length).toBeGreaterThan(0);
  });

  it('keeps the commit links of a step whatever else is filtered: a filter chooses steps, it never changes them', () => {
    const [a] = FIXTURE_SUBAGENTS;
    const before = new Map(nodes.map((node) => [node.id, JSON.stringify(node.artifact_links)]));
    const result = runSearch(index, { query: NO_QUERY, filters: { ...NO_FILTERS, conv: [a] }, file: null });
    expect(result.nodes.length).toBeGreaterThan(10);
    for (const node of result.nodes) expect(JSON.stringify(node.artifact_links)).toBe(before.get(node.id));
    // And the nodes are the very objects that were given.
    for (const node of result.nodes) expect(nodes).toContain(node);
  });

  it('matches a commit filter by its first characters, in any case, and only by a prefix', () => {
    const sha = commits[2].sha;
    const full = ids({ commit: [sha] });
    expect(full.length).toBeGreaterThan(0);
    expect(ids({ commit: [sha.slice(0, 7)] })).toEqual(full);
    expect(ids({ commit: [sha.slice(0, 7).toUpperCase()] })).toEqual(full);
    expect(ids({ commit: [sha.slice(1, 8)] })).toEqual([]);
  });
});

describe('what the facets call things', () => {
  it('never offers two options under one name, and never names one after what its steps look like', () => {
    const { nodes } = searchFixture(300);
    const facets = buildFacets(getTimelineIndex(nodes));
    for (const list of [facets.conversations, facets.origins, facets.tools, facets.links, facets.commits]) {
      const labels = list.map((option) => `${option.label}|${option.note ?? ''}`);
      expect(new Set(labels).size).toBe(labels.length);
    }
    expect(facets.conversations.map((option) => option.label.split(' ')[0]).sort()).toEqual(['Session', 'Session', 'Subagent', 'Subagent'].sort());
  });

  it('names a conversation id that is long, empty or odd without failing', () => {
    const ids = ['', 'x'.repeat(300), 'a:agent-', ':agent-b', 'a:agent-b:agent-c', '日本語', 'with space'];
    const nodes = ids.map((conversation, k) => makeNode({ id: `c${k}`, conversation_id: conversation, timestamp: k }));
    const facets = buildFacets(getTimelineIndex(nodes));
    expect(facets.conversations).toHaveLength(ids.length);
    const labels = facets.conversations.map((option) => option.label);
    expect(new Set(labels).size).toBe(labels.length);
    for (const option of facets.conversations) expect(option.label).not.toBe('');
  });
});

describe('what is not searched', () => {
  it('does not search what the Reviewer writes itself, the model name, the author, the ids or the status of a call', () => {
    const node = makeNode({
      id: 'uuid-0000-1111',
      conversation_id: 'session-abcdef',
      author: 'human',
      model_name: 'claude-opus-4-5-20251101',
      user_prompt: 'hello',
      model_response: null,
      tool_executions: [{ ...toolCall('Bash', { command: 'true' }), status: 'failure' }],
      artifact_links: [generatedLink('uuid-0000-1111', 'ffeeddccbbaa00112233445566778899aabbccdd')],
    });
    const absent = [
      'claude', 'opus', '20251101', 'human', 'uuid-0000', 'session-abcdef', 'failure', 'success', 'failed', 'pass', 'fail',
      'No recorded observation', 'No explicit thought', 'No recorded tool execution', 'Observation', 'Thought', 'Action', 'Response', 'Reasoning', 'Tool calls', 'Show more',
      'generated', 'native', 'temporal',
    ];
    for (const word of absent) expect(search([node], `"${word}"`), word).toEqual([]);
    for (const word of ['hello', 'bash', 'true', 'ffeedd']) expect(search([node], word), word).toEqual(['uuid-0000-1111']);
  });

  it('finds the files only the context items name, though no card draws them (as the README says)', () => {
    const node = makeNode({ id: 'ctx', context_items: [{ file_path: '/home/dev/proj/docs/ONLY_IN_CONTEXT.md' }] });
    expect(search([node], 'path:only_in_context')).toEqual(['ctx']);
    expect(search([node], 'only_in_context')).toEqual(['ctx']);
    expect(search([node], 'tool:only_in_context')).toEqual([]);
  });
});

describe('scope a prefix gives a term, listed once', () => {
  const scopes: QueryScope[] = ['path', 'tool', 'commit'];
  it('looks in one field and not the others', () => {
    const node = makeNode({
      id: 'only',
      user_prompt: 'mentions bash and src/a.ts and a1b2c3d in the prompt',
      tool_executions: [toolCall('Read', { file_path: '/repo/lib/other.ts' })],
      artifact_links: [generatedLink('only', 'ffeeddccbbaa00112233445566778899aabbccdd')],
    });
    for (const [prefix, present, absent] of [
      ['tool', 'read', 'bash'],
      ['path', 'lib/other.ts', 'src/a.ts'],
      ['commit', 'ffeedd', 'a1b2c3d'],
    ]) {
      expect(search([node], `${prefix}:${present}`), `${prefix}:${present}`).toEqual(['only']);
      expect(search([node], `${prefix}:${absent}`), `${prefix}:${absent}`).toEqual([]);
      // The same word with no prefix is found wherever it is.
      expect(search([node], absent), absent).toEqual(['only']);
    }
    expect(scopes).toHaveLength(3);
  });
});
