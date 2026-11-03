/**
 * Search and filters for the shadow timeline (issue #118): the index, the query
 * language, the filters, and the code that runs them. Pure: no React, no DOM, and no
 * regular expression that the text could make slow (the text searched is wire data, the
 * text searched for is typed by a person, and neither gets to choose how much work a search
 * costs: the only patterns are the two class tests of `lib/searchFold`, which have nothing
 * to backtrack, and nothing here is ever read as a pattern).
 *
 * # The index
 *
 * `getTimelineIndex(nodes)` reads every step once and keeps, per step, the text a
 * person could find on its card, folded so a plain substring test can find it. The fold
 * is the one `lib/searchFold` describes, and it is the one the page marks matches with:
 * case (in full: ß is "ss"), compatibility forms (full-width letters, ligatures,
 * precomposed accents), every White_Space character as a space, and every character of
 * the hidden class left out, so an invisible character can not hide a match.
 *
 * What is kept is what a card DRAWS, and only that, so that a step the search leaves
 * always has something on its card to show why:
 *
 * - `text`: the stimulus (the prompt, the context, the compaction summary, the tool results with
 *   the names they are shown under), the reasoning, the response and the arguments of the tool
 *   calls, as the layout of the step draws them. The markers of the wire (`[tool_result Bash]`,
 *   `[meta]`), the placeholders the Reviewer writes when nothing was recorded, the names of
 *   arguments, the words of the options ("runs in background") and reasoning a card has no place
 *   for are not in it. The pieces are joined by NUL, which the fold leaves out of every one of
 *   them, so a phrase never matches across two of them.
 * - `tools`: each call's recorded name and the name the cards show (`toolTitle`).
 * - `paths`: the files the step touched, from its context items and from the files its calls name, with `/` for
 *   separators. Joined by NUL, so a path with a line break in it is one path.
 * - `commits`: the full hashes of the commits its links point at.
 *
 * The index also holds, per step, what the filters need: the conversation, the stimulus
 * origin, the tools called, the kinds of link, and the commits linked. The wire strings that
 * key a table here (an id can be a mebibyte long) go through `StringTable`.
 *
 * It is built once per array of steps (memoised by identity), in the order the array is
 * in, which is the order the timeline draws them in; a search returns positions in that
 * order, so the order of the steps that match is the order they have without a search.
 *
 * # The query
 *
 * Terms are separated by white space and ALL must match. Double quotes group a phrase
 * (`"retry limit"`). `path:`, `tool:` and `commit:` (in any case) before a term narrow it
 * to that field; `path:"my dir/a.ts"` quotes the value. Any other `word:` is plain text.
 * Matching is by substring of the folded text. A prefix with nothing after it, a pair of quotes
 * with nothing in them and a phrase of spaces are not a term (the person is still typing).
 */
import type { ArtifactLinkData, InteractionNode } from '../types/cvc';
import { fold, isWhiteSpaceCode, withSlashes } from './searchFold';
import { StringTable } from './stringTable';
import { cutAtCodePoint } from './textLines';
import { toolCallPieces, toolTitle, type DrawnPiece } from './toolCalls';
import { getActionLines, getObservation, getThought, showsReasoning } from './triadNode';
import {
  groupStimulus,
  hasText,
  nodeLayout,
  parentToolNamesByNode,
  parseStimulus,
  stimulusOrigin,
  toolResultsMatchParent,
  type ParentToolCall,
  type StimulusBlock,
} from './transcriptNode';

// ---------------------------------------------------------------------------
// Normalising
// ---------------------------------------------------------------------------

/** The text of `value` as the index keeps it (see `fold`); anything that is not text is nothing. */
export function normalizeForSearch(value: unknown): string {
  if (typeof value !== 'string' || value === '') return '';
  return fold(value);
}

/**
 * A file path as the index keeps it: folded like any text, with `/` for separators and without a
 * leading `./`. (Paths are compared case-insensitively too: the file systems most people review on are.)
 */
export function normalizePath(value: unknown): string {
  let path = normalizeForSearch(value);
  if (path === '') return '';
  path = withSlashes(path);
  while (path.startsWith('./')) path = path.slice(2);
  return path;
}

// ---------------------------------------------------------------------------
// The query
// ---------------------------------------------------------------------------

export type QueryScope = 'path' | 'tool' | 'commit';

export interface QueryTerm {
  /** Folded (see `normalizeForSearch`); never empty, never only spaces. */
  readonly text: string;
  /** The one field the term is looked for in, or null for any. */
  readonly scope: QueryScope | null;
}

export interface ParsedQuery {
  readonly terms: readonly QueryTerm[];
  /** More different terms were typed than `MAX_QUERY_TERMS`, and the later ones are not used. */
  readonly dropped?: true;
}

/** More terms than this are ignored, and a longer term is cut: a query is not a way to ask for unbounded work. */
export const MAX_QUERY_TERMS = 16;
export const MAX_TERM_LENGTH = 512;

export const NO_QUERY: ParsedQuery = { terms: [] };

const QUOTE = 0x22;
const COLON = 0x3a;

function scopeOf(head: string): QueryScope | null {
  switch (head.toLowerCase()) {
    case 'path':
      return 'path';
    case 'tool':
      return 'tool';
    case 'commit':
      return 'commit';
    default:
      return null;
  }
}

/** Whether a folded text is nothing but spaces (or nothing). */
function isBlank(text: string): boolean {
  for (let at = 0; at < text.length; at++) if (text.charCodeAt(at) !== 0x20) return false;
  return true;
}

/**
 * Reads a query into terms. Total: any text at all is a query, and none of it is an error. The work is
 * linear in the text: each stretch of it is read once, and a term that was typed before is not worked
 * out again.
 */
export function parseQuery(input: string): ParsedQuery {
  if (typeof input !== 'string' || input === '') return NO_QUERY;
  const terms: QueryTerm[] = [];
  const seen = new Set<string>();
  let dropped = false;
  /** Adds a term; false once a different one comes that does not fit. */
  const add = (scope: QueryScope | null, raw: string): boolean => {
    // A term is cut before it is folded (folding can only make it longer by a small factor) and after.
    const cut = raw.length > MAX_TERM_LENGTH * 2 ? raw.slice(0, MAX_TERM_LENGTH * 2) : raw;
    let text = scope === 'path' ? normalizePath(cut) : normalizeForSearch(cut);
    if (isBlank(text)) return true;
    if (text.length > MAX_TERM_LENGTH) text = cutAtCodePoint(text, MAX_TERM_LENGTH);
    const key = `${scope ?? ''}:${text}`;
    if (seen.has(key)) return true;
    if (terms.length >= MAX_QUERY_TERMS) {
      dropped = true;
      return false;
    }
    seen.add(key);
    terms.push({ text, scope });
    return true;
  };

  const length = input.length;
  let at = 0;
  while (at < length) {
    while (at < length && isWhiteSpaceCode(input.charCodeAt(at))) at++;
    if (at >= length) break;

    if (input.charCodeAt(at) === QUOTE) {
      // A phrase. Without a closing quote it runs to the end: the person is still typing it.
      const close = input.indexOf('"', at + 1);
      if (!add(null, close < 0 ? input.slice(at + 1) : input.slice(at + 1, close))) break;
      at = close < 0 ? length : close + 1;
      continue;
    }

    // One word, and the first colon in it (looked for only within the word).
    let end = at;
    let colon = -1;
    while (end < length && !isWhiteSpaceCode(input.charCodeAt(end))) {
      if (colon < 0 && input.charCodeAt(end) === COLON) colon = end;
      end++;
    }
    const scope = colon > at ? scopeOf(input.slice(at, colon)) : null;
    if (scope === null) {
      if (!add(null, input.slice(at, end))) break;
      at = end;
    } else if (input.charCodeAt(colon + 1) === QUOTE) {
      const close = input.indexOf('"', colon + 2);
      if (!add(scope, close < 0 ? input.slice(colon + 2) : input.slice(colon + 2, close))) break;
      at = close < 0 ? length : close + 1;
    } else {
      if (!add(scope, input.slice(colon + 1, end))) break;
      at = end;
    }
  }
  if (terms.length === 0) return NO_QUERY;
  return dropped ? { terms, dropped: true } : { terms };
}

/**
 * Whether every step that matches `next` also matches `previous`, so a search for `next` can
 * start from what `previous` found. True when each earlier term is contained in a later one
 * of the same scope (typing on at the end of a term, or adding a term): a step that has the
 * longer text has the shorter one too, in the same field.
 */
export function narrows(previous: ParsedQuery, next: ParsedQuery): boolean {
  return previous.terms.every((before) =>
    next.terms.some((after) => after.scope === before.scope && after.text.includes(before.text)),
  );
}

/** The terms to mark on a surface, by what the surface shows. `plain` is for any text. */
export interface HighlightTerms {
  readonly plain: readonly string[] | null;
  readonly tool: readonly string[] | null;
  readonly path: readonly string[] | null;
  readonly commit: readonly string[] | null;
}

/**
 * The terms that mark text, per surface: a plain term marks wherever it occurs, and a term
 * with a prefix marks only on the surface that shows that field (a tool's name, a file path,
 * a commit). Null when there is nothing to mark. On a surface that shows a file path, `\` and
 * `/` are one separator, as they are in the index.
 */
export function highlightTerms(query: ParsedQuery): HighlightTerms | null {
  if (query.terms.length === 0) return null;
  const plain = query.terms.filter((term) => term.scope === null).map((term) => term.text);
  const scoped = (scope: QueryScope): string[] => query.terms.filter((term) => term.scope === scope).map((term) => term.text);
  const orNull = (terms: readonly string[]): readonly string[] | null => (terms.length === 0 ? null : terms);
  return {
    plain: orNull(plain),
    tool: orNull([...plain, ...scoped('tool')]),
    path: orNull([...plain.map(withSlashes), ...scoped('path')]),
    commit: orNull([...plain, ...scoped('commit')]),
  };
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

export type OriginKey = 'prompt' | 'tool-results' | 'other';
export type LinkKind = 'generated' | 'temporal' | 'verified' | 'publisher-asserted';

export const ORIGIN_KEYS: readonly OriginKey[] = ['prompt', 'tool-results', 'other'];
export const LINK_KINDS: readonly LinkKind[] = ['generated', 'temporal', 'verified', 'publisher-asserted'];

export const ORIGIN_LABELS: Readonly<Record<OriginKey, string>> = {
  prompt: 'Prompt',
  'tool-results': 'Tool results',
  other: 'Other',
};

export const LINK_LABELS: Readonly<Record<LinkKind, string>> = {
  generated: 'Generated',
  temporal: 'Temporal',
  verified: 'Verified',
  'publisher-asserted': 'Publisher-asserted',
};

/** Which file the "touches" filter means: none, whichever is open, or one named (as a shared link names it). */
export type FileFilter = { readonly mode: 'off' } | { readonly mode: 'open' } | { readonly mode: 'path'; readonly path: string };

/**
 * What narrows the steps shown. Within one kind of filter a step needs ANY of the values
 * chosen; across kinds it needs one of each.
 */
export interface Filters {
  /** Conversation ids. */
  readonly conv: readonly string[];
  readonly origin: readonly OriginKey[];
  /** The names tools are shown under (`toolTitle`). */
  readonly tool: readonly string[];
  readonly link: readonly LinkKind[];
  /** Commit hashes, in full or the first characters of one. */
  readonly commit: readonly string[];
  readonly file: FileFilter;
}

export const NO_FILTERS: Filters = { conv: [], origin: [], tool: [], link: [], commit: [], file: { mode: 'off' } };

/** `list` with `value` added, or taken out if it was there. */
export function toggled<T>(list: readonly T[], value: T): T[] {
  return list.includes(value) ? list.filter((item) => item !== value) : [...list, value];
}

/** How many values are chosen in all (each is a chip), counting the file filter as one. */
export function filterCount(filters: Filters): number {
  return filters.conv.length + filters.origin.length + filters.tool.length + filters.link.length + filters.commit.length + (filters.file.mode === 'off' ? 0 : 1);
}

const sameList = (a: readonly string[], b: readonly string[]): boolean => a.length === b.length && a.every((value, index) => value === b[index]);

function sameFile(a: FileFilter, b: FileFilter): boolean {
  return a.mode === b.mode && (a.mode !== 'path' || a.path === (b as { path: string }).path);
}

/** Whether two sets of filters narrow the steps in the same way. */
export function sameFilters(a: Filters, b: Filters): boolean {
  return (
    a === b ||
    (sameList(a.conv, b.conv) && sameList(a.origin, b.origin) && sameList(a.tool, b.tool) && sameList(a.link, b.link) && sameList(a.commit, b.commit) && sameFile(a.file, b.file))
  );
}

/** The file a filter means, given the file that is open; null for none. */
export function effectiveFile(filter: FileFilter, openFile: string | null | undefined): string | null {
  if (filter.mode === 'path') return filter.path;
  if (filter.mode === 'open') return typeof openFile === 'string' && openFile !== '' ? openFile : null;
  return null;
}

// ---------------------------------------------------------------------------
// The index
// ---------------------------------------------------------------------------

/** The kind a link is shown as: the same split the link chips make (see `confidence` in TimelineNode). */
export function linkKindOf(link: Pick<ArtifactLinkData, 'link_type' | 'derivation_event'>): LinkKind {
  if (link?.derivation_event) return 'publisher-asserted';
  switch (link?.link_type) {
    case 'temporal':
      return 'temporal';
    case 'verified':
      return 'verified';
    default:
      return 'generated';
  }
}

const LINK_BIT: Record<LinkKind, number> = { generated: 1, temporal: 2, verified: 4, 'publisher-asserted': 8 };
const ORIGIN_CODE: Record<OriginKey, number> = { prompt: 0, 'tool-results': 1, other: 2 };

const NONE: readonly never[] = Object.freeze([]);

/** A step that names this many files is not checked for the same file twice. */
const MANY_FILES = 16;

/** Separates the pieces of a field of the index. The fold leaves it out of every piece, so no piece holds one. */
const SEPARATOR = '\0';

export interface TimelineIndex {
  /** The steps, in the order they are drawn in. */
  readonly nodes: readonly InteractionNode[];
  readonly size: number;
  /** Per step: the text its card draws, a piece after another, joined by NUL. */
  readonly text: readonly string[];
  /** Per step: the recorded names of its calls and the names they are shown under, joined by NUL. */
  readonly tools: readonly string[];
  /** Per step: NUL before, between and after the paths; empty when there are none. */
  readonly paths: readonly string[];
  /** Per step: the hashes of the commits its links point at, joined by NUL. */
  readonly commits: readonly string[];
  /** Per step: the conversation, as a position in `conversationIds`. */
  readonly conversation: Uint32Array;
  readonly conversationIds: readonly string[];
  readonly origin: Uint8Array;
  /** Per step: the names its calls are shown under, as positions in `toolNames`. */
  readonly toolOf: readonly (readonly number[])[];
  readonly toolNames: readonly string[];
  readonly linkMask: Uint8Array;
  /** Per step: the full hashes of the commits its links point at, in lower case, once each. */
  readonly commitsOf: readonly (readonly string[])[];
  /** Per step: the same, as positions in `commitHashes`. */
  readonly commitIdsOf: readonly (readonly number[])[];
  readonly commitHashes: readonly string[];
  /** Where the wire strings above are, by value. */
  readonly conversationTable: StringTable;
  readonly toolTable: StringTable;
  readonly commitTable: StringTable;
}

/** What a step's card draws, as the search reads it. */
interface StepParts {
  /** The pieces of text, as drawn (not folded yet); each is a block of its own. */
  texts: string[];
  /** The raw names of the calls, and the names they are shown under. */
  toolNames: string[];
  toolTitles: string[];
  /** The files the step names, as the card draws them (not folded yet). */
  paths: string[];
  origin: OriginKey;
}

/** The texts of a block of the stimulus, as `StimulusPane` draws them. */
function stimulusTexts(block: StimulusBlock, into: string[]): void {
  switch (block.kind) {
    case 'prompt':
    case 'meta':
    case 'compaction':
      into.push(block.text);
      break;
    case 'tool_results':
      for (const result of block.results) {
        if (result.kind === 'tool_result') into.push(toolTitle(result.tool));
        into.push(result.text);
      }
      break;
    case 'none':
      break;
  }
}

/** The lines of a text that is drawn a line at a time (the rows of a diff), less the empty ones. */
function linesOf(text: string): string[] {
  const lines = text.includes('\r') ? text.split('\r\n').join('\n').split('\r').join('\n').split('\n') : text.split('\n');
  return lines.filter((line) => line !== '');
}

/** A text to count the matches in, and the surface it is drawn on (a text that can be anything is just the string). */
export type CountedText = string | { readonly text: string; readonly scope: QueryScope };

/**
 * What one tool call's card draws, as texts to count matches in: the name it is shown under (a tool name), its
 * file (a path) and the rest, one text for each thing the card draws separately. For a control that pages through
 * the calls of a step to say how many matches the calls not drawn yet hold (see `useHiddenMatches`).
 */
export function toolCallTexts(exec: { tool_name?: unknown; arguments?: unknown } | null | undefined): CountedText[] {
  const texts: CountedText[] = [];
  try {
    for (const piece of toolCallPieces(exec as Parameters<typeof toolCallPieces>[0])) {
      for (const text of piece.lines ? linesOf(piece.text) : [piece.text]) texts.push(piece.scope === undefined ? text : { text, scope: piece.scope });
    }
  } catch {
    // Arguments that can not be read have nothing to find.
  }
  return texts;
}

/**
 * Reads one step the way its card draws it. The two layouts draw different things from the same fields (see
 * `nodeLayout`): the transcript layout draws the stimulus in its parts, the reasoning and the response if
 * they have any text, and each tool call as a card; the Thought / Action / Observation layout draws a prompt, the
 * calls as the lines they were recorded in, a response, and offers the reasoning of an AI agent behind "Why?".
 *
 * A node that cannot be read in full (data from before blobs were validated) keeps what was read before the failure,
 * in `parts`, and the failure is the caller's to catch.
 */
function readStep(node: InteractionNode, parentTools: readonly ParentToolCall[] | null, parts: StepParts): void {
  const transcript = nodeLayout(node) === 'transcript';

  if (transcript) {
    const segments = parseStimulus(node.user_prompt, { interruptionBoundary: node.author === 'human' });
    const origin = stimulusOrigin(node, segments);
    parts.origin = origin === 'Prompt' ? 'prompt' : origin === 'Tool results' ? 'tool-results' : 'other';
    // A stimulus whose tool results are not those of the step before is drawn as one block of text, markers and all.
    const undivided = parentTools !== null && !toolResultsMatchParent(segments, parentTools);
    if (undivided) {
      if (typeof node.user_prompt === 'string') parts.texts.push(node.user_prompt);
    } else {
      for (const block of groupStimulus(segments)) stimulusTexts(block, parts.texts);
    }
    if (hasText(node.model_cot)) parts.texts.push(node.model_cot);
    if (hasText(node.model_response)) parts.texts.push(node.model_response);
  } else {
    const thought = getThought(node);
    if (thought.recorded) parts.texts.push(thought.text);
    // The Action pane draws each call as one line: the recorded name, then the arguments as they were recorded.
    for (const line of getActionLines(node)) if (line.kind === 'tool' && line.args !== '') parts.texts.push(line.args);
    const observation = getObservation(node);
    if (observation.recorded && observation.text !== thought.text) parts.texts.push(observation.text);
    if (showsReasoning(node)) parts.texts.push(node.model_cot as string);
  }

  if (Array.isArray(node.tool_executions)) {
    for (const exec of node.tool_executions) {
      const name = typeof exec?.tool_name === 'string' ? exec.tool_name : '';
      const title = toolTitle(name);
      if (name !== '') parts.toolNames.push(name);
      parts.toolTitles.push(title);
      const pieces: DrawnPiece[] = toolCallPieces(exec);
      for (const piece of pieces) {
        if (piece.scope === 'tool') continue;
        if (piece.scope === 'path') parts.paths.push(piece.text);
        else if (transcript) {
          if (piece.lines) parts.texts.push(...linesOf(piece.text));
          else parts.texts.push(piece.text);
        }
      }
    }
  }
  if (Array.isArray(node.context_items)) {
    for (const item of node.context_items) if (typeof item?.file_path === 'string') parts.paths.push(item.file_path);
  }
}

/**
 * A fold for the short strings that come again and again in a timeline (the name of a tool, the hash of a commit): worked out
 * once for each. The memory it takes is bounded (a few thousand short strings), and it is the index's own: it lives as long as
 * the build does.
 */
function shortFolds(): (text: string) => string {
  const known = new Map<string, string>();
  return (text) => {
    if (text.length > 64) return fold(text);
    let folded = known.get(text);
    if (folded === undefined) {
      if (known.size >= 4096) known.clear();
      folded = fold(text);
      known.set(text, folded);
    }
    return folded;
  };
}

function buildIndex(nodes: readonly InteractionNode[], parentTools: ReadonlyMap<string, readonly ParentToolCall[] | null>): TimelineIndex {
  const foldShort = shortFolds();
  const size = nodes.length;
  const text: string[] = new Array(size);
  const tools: string[] = new Array(size);
  const paths: string[] = new Array(size);
  const commits: string[] = new Array(size);
  const conversation = new Uint32Array(size);
  const origin = new Uint8Array(size);
  const toolOf: (readonly number[])[] = new Array(size);
  const linkMask = new Uint8Array(size);
  const commitsOf: (readonly string[])[] = new Array(size);
  const commitIdsOf: (readonly number[])[] = new Array(size);

  const conversationTable = new StringTable();
  const toolTable = new StringTable();
  const commitTable = new StringTable();

  for (let at = 0; at < size; at++) {
    const node = nodes[at];
    let nodeText = '';
    let nodeTools = '';
    let nodePaths = '';
    let nodeCommits = '';
    let nodeOrigin: OriginKey = 'other';
    let nodeToolIds: number[] | readonly never[] = NONE;
    let nodeMask = 0;
    let nodeHashes: string[] | readonly never[] = NONE;
    let nodeHashIds: number[] | readonly never[] = NONE;
    let conversationId = '';

    // A node from before blobs were validated can hold anything: one that can not be read is searched as empty,
    // and never takes the timeline with it.
    const parts: StepParts = { texts: [], toolNames: [], toolTitles: [], paths: [], origin: 'other' };
    try {
      conversationId = typeof node.conversation_id === 'string' ? node.conversation_id : '';

      try {
        readStep(node, parentTools.get(node.id) ?? null, parts);
      } catch {
        // Whatever was read before the failure stays; the rest of this step is empty.
      }
      nodeOrigin = parts.origin;

      const pieces: string[] = [];
      for (const piece of parts.texts) {
        const folded = fold(piece);
        if (folded !== '') pieces.push(folded);
      }
      nodeText = pieces.join(SEPARATOR);

      const names: string[] = [];
      for (const name of parts.toolNames) {
        const folded = foldShort(name);
        if (folded !== '') names.push(folded);
      }
      const toolIds: number[] = [];
      for (const title of parts.toolTitles) {
        const id = toolTable.add(title);
        if (!toolIds.includes(id)) toolIds.push(id);
        const folded = foldShort(title);
        if (folded !== '' && !names.includes(folded)) names.push(folded);
      }
      nodeTools = names.join(SEPARATOR);
      nodeToolIds = toolIds;

      // Each file once, as far as that is cheap to see: a step names a handful of files, and one named twice is only
      // longer in the index. (A set keyed by the paths would be a table keyed by wire strings, which can be a mebibyte each.)
      const files: string[] = [];
      for (const path of parts.paths) {
        const normalized = normalizePath(path);
        if (normalized !== '' && !(files.length < MANY_FILES && files.includes(normalized))) files.push(normalized);
      }
      nodePaths = files.length === 0 ? '' : `${SEPARATOR}${files.join(SEPARATOR)}${SEPARATOR}`;

      const hashes: string[] = [];
      const hashIds: number[] = [];
      if (Array.isArray(node.artifact_links)) {
        for (const link of node.artifact_links) {
          if (link === null || typeof link !== 'object') continue;
          const hash = typeof link.git_commit_hash === 'string' ? foldShort(link.git_commit_hash) : '';
          if (hash !== '') {
            const id = commitTable.add(hash);
            if (!hashIds.includes(id)) {
              hashIds.push(id);
              hashes.push(hash);
            }
          }
          nodeMask |= LINK_BIT[linkKindOf(link)];
        }
      }
      nodeCommits = hashes.join(SEPARATOR);
      nodeHashes = hashes;
      nodeHashIds = hashIds;
    } catch {
      // Whatever was read before the failure stays; the rest of this step is empty.
    }

    text[at] = nodeText;
    tools[at] = nodeTools;
    paths[at] = nodePaths;
    commits[at] = nodeCommits;
    conversation[at] = conversationTable.add(conversationId);
    origin[at] = ORIGIN_CODE[nodeOrigin];
    toolOf[at] = nodeToolIds;
    linkMask[at] = nodeMask;
    commitsOf[at] = nodeHashes;
    commitIdsOf[at] = nodeHashIds;
  }

  return {
    nodes,
    size,
    text,
    tools,
    paths,
    commits,
    conversation,
    conversationIds: conversationTable.values,
    origin,
    toolOf,
    toolNames: toolTable.values,
    linkMask,
    commitsOf,
    commitIdsOf,
    commitHashes: commitTable.values,
    conversationTable,
    toolTable,
    commitTable,
  };
}

const indexes = new WeakMap<readonly InteractionNode[], TimelineIndex>();

/**
 * The index of `nodes`, built the first time it is asked for and kept for as long as the array lives.
 * `parentTools` is what each step's parent called (see `parentToolNamesByNode`), which the stimulus of a
 * step is read against; the timeline has it already, and without it the index works it out.
 */
export function getTimelineIndex(
  nodes: readonly InteractionNode[],
  parentTools?: ReadonlyMap<string, readonly ParentToolCall[] | null>,
): TimelineIndex {
  let index = indexes.get(nodes);
  if (index === undefined) {
    index = buildIndex(nodes, parentTools ?? parentToolNamesByNode(nodes));
    indexes.set(nodes, index);
  }
  return index;
}

/**
 * What the step matches in that its card does not draw, for the card to say so: the paths of the files in its
 * context (the capture records them with the step; no card shows them). Empty when nothing is searched for or
 * nothing is matched there.
 */
export function undrawnMatches(node: InteractionNode, terms: HighlightTerms | null): string[] {
  if (terms === null || terms.path === null || !Array.isArray(node.context_items)) return [];
  const wanted = terms.path;
  for (const item of node.context_items) {
    const path = normalizePath(item?.file_path);
    if (path !== '' && wanted.some((term) => path.includes(term))) return ['context file paths'];
  }
  return [];
}

// ---------------------------------------------------------------------------
// Running a search
// ---------------------------------------------------------------------------

export interface SearchRequest {
  readonly query: ParsedQuery;
  readonly filters: Filters;
  /** The file the "touches" filter means (see `effectiveFile`), or null. */
  readonly file: string | null;
}

export interface SearchResult {
  /** The steps that match, as positions in the index, in order. */
  readonly positions: Uint32Array;
  readonly nodes: InteractionNode[];
  readonly ids: string[];
}

function termMatches(index: TimelineIndex, at: number, term: QueryTerm): boolean {
  const needle = term.text;
  switch (term.scope) {
    case 'tool':
      return index.tools[at].includes(needle);
    case 'path':
      return index.paths[at].includes(needle);
    case 'commit':
      return index.commits[at].includes(needle);
    default:
      // The small fields first: most terms that match at all match there or not in the big one.
      return (
        index.tools[at].includes(needle) ||
        index.paths[at].includes(needle) ||
        index.commits[at].includes(needle) ||
        index.text[at].includes(needle)
      );
  }
}

/**
 * Whether the paths of a step (NUL before, between and after) include `file`: a path that is the
 * file, or that ends with it after a separator (the steps record where the file was on the machine
 * that wrote them, and the pull request names it from the root of the repository). The separator of the
 * paths is NUL and never a line break, which a path may hold: one path is never taken for two.
 */
export function touchesFile(pathsText: string, file: string): boolean {
  const wanted = normalizePath(file);
  return wanted !== '' && touchesNormalized(pathsText, wanted);
}

function touchesNormalized(pathsText: string, wanted: string): boolean {
  return pathsText !== '' && (pathsText.includes(`${SEPARATOR}${wanted}${SEPARATOR}`) || pathsText.includes(`/${wanted}${SEPARATOR}`));
}

/** The filters, ready to test a step against without looking anything up again. */
interface CompiledFilters {
  readonly any: boolean;
  readonly conversations: ReadonlySet<number> | null;
  readonly origins: number | null;
  readonly tools: Uint8Array | null;
  readonly links: number;
  readonly commits: readonly string[];
  readonly file: string | null;
}

function compileFilters(index: TimelineIndex, filters: Filters, file: string | null): CompiledFilters {
  let conversations: Set<number> | null = null;
  if (filters.conv.length > 0) {
    conversations = new Set();
    for (const id of filters.conv) {
      const at = index.conversationTable.indexOf(id);
      if (at >= 0) conversations.add(at);
    }
  }
  let origins: number | null = null;
  if (filters.origin.length > 0) {
    origins = 0;
    for (const key of filters.origin) origins |= 1 << ORIGIN_CODE[key];
  }
  let tools: Uint8Array | null = null;
  if (filters.tool.length > 0) {
    tools = new Uint8Array(index.toolNames.length);
    for (const name of filters.tool) {
      const at = index.toolTable.indexOf(name);
      if (at >= 0) tools[at] = 1;
    }
  }
  let links = 0;
  for (const kind of filters.link) links |= LINK_BIT[kind];
  const commits = filters.commit.map((hash) => hash.toLowerCase());
  const wantedFile = file === null ? null : normalizePath(file);
  const any = conversations !== null || origins !== null || tools !== null || links !== 0 || commits.length > 0 || wantedFile !== null;
  return { any, conversations, origins, tools, links, commits, file: wantedFile };
}

function passesFilters(index: TimelineIndex, at: number, filters: CompiledFilters): boolean {
  if (filters.conversations !== null && !filters.conversations.has(index.conversation[at])) return false;
  if (filters.origins !== null && (filters.origins & (1 << index.origin[at])) === 0) return false;
  if (filters.links !== 0 && (filters.links & index.linkMask[at]) === 0) return false;
  if (filters.tools !== null) {
    const called = index.toolOf[at];
    let found = false;
    for (let k = 0; k < called.length; k++) {
      if (filters.tools[called[k]] === 1) {
        found = true;
        break;
      }
    }
    if (!found) return false;
  }
  if (filters.commits.length > 0) {
    const linked = index.commitsOf[at];
    let found = false;
    for (let k = 0; k < linked.length && !found; k++) {
      for (const wanted of filters.commits) {
        if (linked[k].startsWith(wanted)) {
          found = true;
          break;
        }
      }
    }
    if (!found) return false;
  }
  if (filters.file !== null && !touchesNormalized(index.paths[at], filters.file)) return false;
  return true;
}

/** The last search run over an index, for the next one to start from when it can. */
interface LastRun {
  readonly request: SearchRequest;
  readonly positions: Uint32Array;
}
const lastRuns = new WeakMap<TimelineIndex, LastRun>();

/** Runs `request` over `index`. Pure: the same request gives the same steps whatever was searched before. */
export function runSearch(index: TimelineIndex, request: SearchRequest): SearchResult {
  const { query, filters, file } = request;
  const previous = lastRuns.get(index);

  // Typing on after the last search, with the same filters: only the steps that matched can match now.
  const reuse = previous !== undefined && previous.request.file === file && sameFilters(previous.request.filters, filters) && narrows(previous.request.query, query);
  const candidates: ArrayLike<number> | null = reuse ? previous.positions : null;
  const compiled = reuse ? null : compileFilters(index, filters, file);

  // Terms with a prefix look in short text, so they go first; then the longer a term, the less likely it is to match.
  const terms = [...query.terms].sort((a, b) => (a.scope === null ? 1 : 0) - (b.scope === null ? 1 : 0) || b.text.length - a.text.length);

  const count = candidates === null ? index.size : candidates.length;
  const found: number[] = [];
  for (let k = 0; k < count; k++) {
    const at = candidates === null ? k : candidates[k];
    if (compiled !== null && compiled.any && !passesFilters(index, at, compiled)) continue;
    let all = true;
    for (let t = 0; t < terms.length; t++) {
      if (!termMatches(index, at, terms[t])) {
        all = false;
        break;
      }
    }
    if (all) found.push(at);
  }

  const positions = Uint32Array.from(found);
  lastRuns.set(index, { request, positions });
  const nodes: InteractionNode[] = new Array(positions.length);
  const ids: string[] = new Array(positions.length);
  for (let k = 0; k < positions.length; k++) {
    const node = index.nodes[positions[k]];
    nodes[k] = node;
    ids[k] = node.id;
  }
  return { positions, nodes, ids };
}

/** Forgets the last search over `index`; for tests that compare a fresh search with one that started from another. */
export function forgetLastSearch(index: TimelineIndex): void {
  lastRuns.delete(index);
}

// ---------------------------------------------------------------------------
// What the filters offer
// ---------------------------------------------------------------------------

export interface FacetOption {
  /** What the filter holds when this is chosen (and what the URL carries). */
  readonly value: string;
  /** What the option is called. */
  readonly label: string;
  /** How many steps it holds. */
  readonly count: number;
  /** A line that tells it from others like it: the session a subagent belongs to ("of session 5c1d8e2a"). */
  readonly note?: string;
  /** A line that says more about it: the first words of a conversation, the subject of a commit. */
  readonly hint?: string;
}

export interface Facets {
  readonly conversations: readonly FacetOption[];
  readonly origins: readonly FacetOption[];
  readonly tools: readonly FacetOption[];
  readonly links: readonly FacetOption[];
  readonly commits: readonly FacetOption[];
}

export const NO_FACETS: Facets = { conversations: [], origins: [], tools: [], links: [], commits: [] };

/** A commit of the pull request, as the filter lists it. */
export interface CommitInfo {
  readonly sha: string;
  /** The first line of the message, when it is known. */
  readonly subject?: string;
  /** The commit that merged the pull request, which is not one of its own. */
  readonly merge?: boolean;
}

const AGENT_MARK = ':agent-';
const HINT_LENGTH = 80;

/** The conversation id of a Claude Code subagent is `<session id>:agent-<agent id>`. */
export function isSubagentConversation(id: string): boolean {
  return id.indexOf(AGENT_MARK) > 0;
}

function firstLine(text: string): string | null {
  let at = 0;
  while (at <= text.length) {
    let end = text.indexOf('\n', at);
    if (end < 0) end = text.length;
    const line = text.slice(at, end).replace('\r', '').trim();
    if (line !== '') return cutAtCodePoint(line, HINT_LENGTH);
    at = end + 1;
  }
  return null;
}

/**
 * The first line of what the conversation was asked, from its earliest step: the prompt when the step has
 * one, or the injected context it starts with. (Conversation titles are not on the wire; this is the hint
 * the Reviewer has.)
 */
export function conversationHint(node: InteractionNode): string | null {
  const prompt = typeof node.user_prompt === 'string' ? node.user_prompt : '';
  if (prompt === '') return null;
  if (nodeLayout(node) !== 'transcript') return firstLine(prompt);
  const segments = parseStimulus(prompt, { interruptionBoundary: node.author === 'human' });
  for (const kind of ['prompt', 'meta'] as const) {
    for (const segment of segments) {
      if (segment.kind === kind && segment.text.trim() !== '') return firstLine(segment.text);
    }
  }
  return null;
}

interface ConversationDraft {
  id: string;
  count: number;
  earliest: number;
}

/** Text order that does not depend on the locale: case-insensitive first, then as written. */
const byText = (a: string, b: string): number => {
  const x = a.toLowerCase();
  const y = b.toLowerCase();
  return x < y ? -1 : x > y ? 1 : a < b ? -1 : a > b ? 1 : 0;
};

/** How many characters of a long id a name starts with (more, where two would read the same). */
const SHORT_ID = 8;
/** An id no longer than this is shown whole: a session id is a UUID, and the ids people give their own are shorter. */
const WHOLE_SESSION_ID = 24;
/** An agent id is eight hexadecimal digits; a longer one is cut. */
const WHOLE_AGENT_ID = SHORT_ID;
/**
 * How far into an id a name ever looks. An id can be a mebibyte long, and the names are for people: ids that are the
 * same this far are told apart by a number (see `conversationNames`), not by more of the id.
 */
const NAME_LOOK = 64;
/** The longest a name or label is drawn: the rest of a long one is left out, and its place is marked. */
export const LABEL_LIMIT = 120;

/** `text` as a label: cut where a label ends, with an ellipsis where something was left out. */
export function boundedLabel(text: string): string {
  return text.length <= LABEL_LIMIT ? text : `${cutAtCodePoint(text, LABEL_LIMIT)}…`;
}

/** The id of the session a subagent conversation (`<session>:agent-<agent>`) belongs to. */
const sessionOfSubagent = (id: string): string => id.slice(0, id.indexOf(AGENT_MARK));
const agentOfSubagent = (id: string): string => id.slice(id.indexOf(AGENT_MARK) + AGENT_MARK.length);

/**
 * `ids` as short as they can be and still read differently from each other: each is cut to its first characters, as
 * many as it takes (a few more at a time) for no two to be the same; one no longer than `whole` is not cut. The result
 * is for `ids` position by position (an id that is given twice is cut the same way twice). It looks at the first
 * `NAME_LOOK` characters of an id and no further, so it takes time in proportion to the number of ids and not to
 * their length; two ids that are the same up to there keep the same form.
 */
function shortForms(ids: readonly string[], whole: number): string[] {
  const distinct = new StringTable();
  const at = ids.map((id) => distinct.add(id));
  const longs = distinct.values;
  let length = SHORT_ID;
  for (;;) {
    const forms = longs.map((id) => (id.length <= whole ? id : id.slice(0, Math.min(length, NAME_LOOK))));
    if (length >= NAME_LOOK || new Set(forms).size === forms.length) return at.map((k) => forms[k]);
    length += 4;
  }
}

interface ConversationName {
  readonly label: string;
  readonly note?: string;
}

/**
 * What a conversation is called when its id is not in the timeline's list (a filter named by a link to a timeline this
 * one does not match): the same words as the list uses, from the id alone, cut as far as the list cuts one that
 * collides with nothing.
 */
export function conversationNameOf(id: string): string {
  if (isSubagentConversation(id)) {
    const agent = agentOfSubagent(id);
    return agent === '' ? 'Subagent' : `Subagent ${agent.length > WHOLE_AGENT_ID ? agent.slice(0, SHORT_ID) : agent}`;
  }
  return id === '' ? 'Unknown session' : `Session ${id.length > WHOLE_SESSION_ID ? id.slice(0, SHORT_ID) : id}`;
}

/**
 * The names of the conversations, one each and different from each other. A name is a matter of the conversation's id,
 * as the capture writes it, and of nothing else: not of what its steps look like, which is a choice of how to draw them
 * and says nothing of where they came from.
 *
 * - `<session>:agent-<agent>` is "Subagent <agent>", with the session it belongs to as its note ("of session <session>");
 * - every other conversation is "Session <id>": a Claude Code session, or the conversation of the MCP server, the
 *   editor or `cvc run`, whose ids are their own.
 *
 * Ids are cut to their first characters (see `shortForms`): the sessions together, those a subagent names among them, so
 * a note reads as the session's own name does; and the agents together. The same agent id under two sessions is the same
 * name twice, and the later is numbered; so are ids that are the same as far as a name looks.
 */
function conversationNames(drafts: readonly ConversationDraft[]): ConversationName[] {
  const subagent = drafts.map((draft) => isSubagentConversation(draft.id));
  // The ids that name a session: a session's own, and the one each subagent belongs to.
  const sessionIds: string[] = [];
  const sessionAt: number[] = drafts.map((draft, k) => {
    if (subagent[k]) return sessionIds.push(sessionOfSubagent(draft.id)) - 1;
    return draft.id === '' ? -1 : sessionIds.push(draft.id) - 1;
  });
  const sessionForms = shortForms(sessionIds, WHOLE_SESSION_ID);
  const agentIds: string[] = [];
  const agentAt: number[] = drafts.map((draft, k) => (subagent[k] ? agentIds.push(agentOfSubagent(draft.id)) - 1 : -1));
  const agentForms = shortForms(agentIds, WHOLE_AGENT_ID);

  const names = drafts.map((draft, k): ConversationName => {
    if (subagent[k]) {
      const agent = agentForms[agentAt[k]];
      return { label: agent === '' ? 'Subagent' : `Subagent ${agent}`, note: `of session ${sessionForms[sessionAt[k]]}` };
    }
    return { label: draft.id === '' ? 'Unknown session' : `Session ${sessionForms[sessionAt[k]]}` };
  });
  return numbered(names);
}

/** Different names for the same label: the first keeps it and the next are "(2)", "(3)". */
function numbered<T extends { readonly label: string }>(items: readonly T[]): T[] {
  const seen = new Map<string, number>();
  return items.map((item) => {
    const times = (seen.get(item.label) ?? 0) + 1;
    seen.set(item.label, times);
    return times === 1 ? item : { ...item, label: `${item.label} (${times})` };
  });
}

/** The options the filters offer for `index`, with how many steps each holds. */
export function buildFacets(index: TimelineIndex, prCommits?: readonly CommitInfo[]): Facets {
  const { nodes, size } = index;

  // Conversations
  const drafts: ConversationDraft[] = index.conversationIds.map((id) => ({ id, count: 0, earliest: -1 }));
  for (let at = 0; at < size; at++) {
    const draft = drafts[index.conversation[at]];
    draft.count++;
    if (draft.earliest < 0 || nodes[at].timestamp <= nodes[draft.earliest].timestamp) draft.earliest = at;
  }
  const names = conversationNames(drafts);
  const conversations = drafts
    .map((draft, k) => {
      let hint: string | null = null;
      try {
        hint = draft.earliest >= 0 ? conversationHint(nodes[draft.earliest]) : null;
      } catch {
        hint = null;
      }
      const { label, note } = names[k];
      const option: FacetOption = { value: draft.id, label, count: draft.count, ...(note === undefined ? {} : { note }), ...(hint === null ? {} : { hint }) };
      return { option, started: draft.earliest >= 0 ? nodes[draft.earliest].timestamp : 0 };
    })
    .sort((a, b) => a.started - b.started || byText(a.option.label, b.option.label))
    .map((entry) => entry.option);

  // Stimulus origin
  const originCounts = [0, 0, 0];
  for (let at = 0; at < size; at++) originCounts[index.origin[at]]++;
  const origins = ORIGIN_KEYS.map((key) => ({ value: key, label: ORIGIN_LABELS[key], count: originCounts[ORIGIN_CODE[key]] })).filter((option) => option.count > 0);

  // Tools
  const toolCounts = new Array<number>(index.toolNames.length).fill(0);
  for (let at = 0; at < size; at++) for (const id of index.toolOf[at]) toolCounts[id]++;
  const tools = numbered(
    index.toolNames
      .map((name, id) => ({ value: name, label: boundedLabel(name), count: toolCounts[id] }))
      .filter((option) => option.count > 0)
      .sort((a, b) => b.count - a.count || byText(a.label, b.label)),
  );

  // Links
  const linkCounts: Record<LinkKind, number> = { generated: 0, temporal: 0, verified: 0, 'publisher-asserted': 0 };
  for (let at = 0; at < size; at++) for (const kind of LINK_KINDS) if ((index.linkMask[at] & LINK_BIT[kind]) !== 0) linkCounts[kind]++;
  const links = LINK_KINDS.map((kind) => ({ value: kind, label: LINK_LABELS[kind], count: linkCounts[kind] })).filter((option) => option.count > 0);

  // Commits: the pull request's own, newest first; or, when they are not known, the ones the steps are linked to.
  const commitCounts = new Array<number>(index.commitHashes.length).fill(0);
  for (let at = 0; at < size; at++) for (const id of index.commitIdsOf[at]) commitCounts[id]++;
  const listed: CommitInfo[] = prCommits && prCommits.length > 0 ? [...prCommits].reverse() : index.commitHashes.map((sha) => ({ sha }));
  const commits = listed.map((commit): FacetOption => {
    const sha = commit.sha.toLowerCase();
    const subject = commit.merge === true ? 'merge commit' : commit.subject;
    const known = index.commitTable.indexOf(sha);
    return { value: sha, label: sha.slice(0, 7), count: known < 0 ? 0 : commitCounts[known], ...(subject ? { hint: subject } : {}) };
  });

  return { conversations, origins, tools, links, commits };
}

// ---------------------------------------------------------------------------
// The chips
// ---------------------------------------------------------------------------

export type FilterGroup = 'conv' | 'origin' | 'tool' | 'link' | 'commit';

/** One filter that is on, as the chip that removes it names it. */
export interface FilterChip {
  /** Unique among the chips shown. */
  readonly key: string;
  readonly group: FilterGroup | 'file';
  /** What to take out of the filter to remove the chip (the path, for the file). */
  readonly value: string;
  /** What kind of filter: "Tool". */
  readonly kind: string;
  /** What it filters by: "Bash". */
  readonly label: string;
}

/** The filters that are on, one chip each, in the order the panel lists them. */
export function filterChips(filters: Filters, facets: Facets, openFile: string | null): FilterChip[] {
  const named = (options: readonly FacetOption[], value: string) => options.find((option) => option.value === value)?.label;
  const chips: FilterChip[] = [];
  for (const value of filters.conv) {
    chips.push({ key: `conv:${value}`, group: 'conv', value, kind: 'Conversation', label: named(facets.conversations, value) ?? conversationNameOf(value) });
  }
  for (const value of filters.origin) chips.push({ key: `origin:${value}`, group: 'origin', value, kind: 'Origin', label: ORIGIN_LABELS[value] ?? value });
  for (const value of filters.tool) chips.push({ key: `tool:${value}`, group: 'tool', value, kind: 'Tool', label: boundedLabel(value) });
  for (const value of filters.link) chips.push({ key: `link:${value}`, group: 'link', value, kind: 'Link', label: LINK_LABELS[value] ?? value });
  for (const value of filters.commit) chips.push({ key: `commit:${value}`, group: 'commit', value, kind: 'Commit', label: value.slice(0, 7) });
  if (filters.file.mode !== 'off') {
    const file = effectiveFile(filters.file, openFile);
    chips.push({ key: 'file', group: 'file', value: file ?? '', kind: 'File', label: file ?? 'the open file' });
  }
  return chips;
}

/** `filters` without the filter a chip stands for. */
export function withoutChip(filters: Filters, chip: FilterChip): Filters {
  if (chip.group === 'file') return { ...filters, file: { mode: 'off' } };
  return { ...filters, [chip.group]: (filters[chip.group] as readonly string[]).filter((value) => value !== chip.value) } as unknown as Filters;
}

/** `filters` with `value` added to one kind of filter, or taken out if it was there. */
export function withToggled(filters: Filters, group: FilterGroup, value: string): Filters {
  return { ...filters, [group]: toggled(filters[group] as readonly string[], value) } as unknown as Filters;
}

/** The filters that narrow anything now (the file filter does only while there is a file for it to mean). */
export function activeFilterCount(filters: Filters, openFile: string | null): number {
  return filterCount({ ...filters, file: effectiveFile(filters.file, openFile) === null ? { mode: 'off' } : filters.file });
}
