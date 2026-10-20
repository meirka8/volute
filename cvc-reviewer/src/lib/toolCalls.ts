/**
 * Turns a recorded tool execution into a view model a reviewer can read.
 *
 * For Claude Code transcripts, `ToolExecution.arguments` is the tool's input as
 * one JSON string (a Bash heredoc, an Edit's old/new pair, a multi-KB script).
 * Rendered raw, that is a wall of `\n`-escaped JSON. Here it is parsed, and the
 * fields each known tool is understood by become typed view models: a command
 * with real line breaks, a before/after pair, a file path with a range.
 *
 * Pure functions only; no React, no DOM. The wire contract:
 *
 * - `arguments` is always valid JSON of the tool input, with string values over
 *   16 KiB truncated in place (`\n…[cvc: truncated N bytes]` suffix);
 * - when one call's input is too large, `arguments` is exactly
 *   `{"bytes":<N>,"cvc_truncated":true}` (the producer writes keys in
 *   alphabetical order) and nothing else is recorded;
 * - privacy redactions can appear inline as `[CVC_REDACTED:v1:<kind>:<uuid>]`.
 *   They are ordinary text here and are never expanded.
 *
 * Parsing is still defensive: redaction rewrites text, and older data exists.
 * A known tool whose fields are missing or of the wrong type is shown as plain
 * key/value rows rather than guessed at, and no recorded field is dropped:
 * fields a view does not use are listed in `extras`.
 *
 * Work on a value is bounded, because a node can be hostile: see `showValue`.
 */
import type { ToolExecution } from '../types/cvc';

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

export type ParsedArguments =
  | { kind: 'object'; value: Record<string, unknown> }
  /** The capture limit was exceeded; `bytes` is the size of the dropped input. */
  | { kind: 'truncated'; bytes: number }
  /** Not a JSON object: invalid JSON, an array, a primitive, or empty. Shown verbatim. */
  | { kind: 'raw'; text: string };

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * The exact `{"bytes":N,"cvc_truncated":true}` sentinel, and nothing that merely
 * resembles it. Found by parsing, never by comparing text, so key order and
 * spacing do not matter.
 */
function truncationSentinel(value: Record<string, unknown>): number | null {
  const keys = Object.keys(value);
  if (keys.length !== 2 || !keys.includes('cvc_truncated') || !keys.includes('bytes')) return null;
  const bytes = value.bytes;
  if (value.cvc_truncated !== true) return null;
  return typeof bytes === 'number' && Number.isSafeInteger(bytes) && bytes >= 0 ? bytes : null;
}

export function parseToolArguments(raw: string | null | undefined): ParsedArguments {
  if (typeof raw !== 'string') return { kind: 'raw', text: '' };
  if (raw.trim() === '') return { kind: 'raw', text: raw };

  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    // Invalid JSON (or nesting too deep for the parser): show the text as is.
    return { kind: 'raw', text: raw };
  }
  if (!isPlainObject(value)) return { kind: 'raw', text: raw };

  const bytes = truncationSentinel(value);
  if (bytes !== null) return { kind: 'truncated', bytes };
  return { kind: 'object', value };
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/** Tools the model provider runs are recorded as `server:<name>`, e.g. `server:web_search`. */
export const SERVER_PREFIX = 'server:';

/**
 * The `tool_protocol` of a call the capture records from the provider's own result
 * block, with the status that block reported. (Free text on the wire: a reader
 * tolerates values it does not know; this is the one that carries a recorded
 * outcome for a server-side call.)
 */
export const SERVER_PROTOCOL = 'server';

export function isServerSideTool(toolName: string): boolean {
  return typeof toolName === 'string' && toolName.startsWith(SERVER_PREFIX) && toolName.length > SERVER_PREFIX.length;
}

/**
 * Whether the capture recorded this call as run on the model provider's side, with
 * the outcome its result block reported (`tool_protocol` `server`). This says how
 * the status was obtained, which is why the card explains it; the outcome itself
 * comes from `toolOutcome`.
 */
export function ranProviderSide(exec: { tool_protocol?: string } | null | undefined): boolean {
  return exec?.tool_protocol === SERVER_PROTOCOL;
}

/**
 * What is known about how a tool call ended.
 *
 * - `success` and `failure`: as recorded;
 * - `unrecorded`: a server-side call written by cvc 0.7.0, which recorded `failure`
 *   for every one of them, which is not an outcome (below);
 * - `unknown`: the recorded status is neither `success` nor `failure` (data from
 *   before blobs were validated).
 */
export type ToolOutcome = 'success' | 'failure' | 'unrecorded' | 'unknown';

/**
 * The outcome of a recorded tool call.
 *
 * A server-side tool (one the model provider runs, recorded as `server:<name>`)
 * delivers its result inside the assistant's own message, never as a user-side
 * `tool_result` (see `server_tool_use` in cvc-core/src/claude_code/transcript.rs and
 * Design.md). Two generations of capture record such a call:
 *
 * - cvc 0.7.0 took every call's status from a user-side result that a server-side
 *   call never has, so it recorded `failure` for all of them, whatever happened, with
 *   `tool_protocol` `native`. That `failure` is an artefact of what was captured, not
 *   an outcome, so it is `unrecorded` and kept out of failure counts.
 * - Later versions take the status from the call's result block in the same message
 *   and record the call with `tool_protocol` `server`. That status is believed: `success`
 *   is a success, and `failure` a real failure (the block reported an error or a
 *   non-zero return code, or no block arrived), counted like any other.
 *
 * Only the exact shape 0.7.0 writes gets the `unrecorded` treatment: a name starting
 * with `server:` AND `tool_protocol` `native`, recorded as `failure`. A call of that
 * name with any other protocol is believed, as is any call recorded as `success`;
 * any other status (legacy, unvalidated data) is `unknown`: never shown as a success
 * and never counted as a failure.
 */
export function toolOutcome(
  exec: Pick<ToolExecution, 'tool_name' | 'status'> & { tool_protocol?: string },
): ToolOutcome {
  // `?.` throughout: blobs from before the format was validated can hold anything.
  const status = exec?.status;
  if (status === 'success') return 'success';
  if (status !== 'failure') return 'unknown';
  return isServerSideTool(exec.tool_name) && exec.tool_protocol === 'native' ? 'unrecorded' : 'failure';
}

// ---------------------------------------------------------------------------
// Showing a value
// ---------------------------------------------------------------------------

/**
 * The longest value shown for one field, in characters. The rest is counted, not
 * kept: what is kept is a copy of its own (see `detached`), so the rest can be
 * freed.
 */
const MAX_VALUE_CHARS = 256 * 1024;
/**
 * Indentation makes a value grow with the SQUARE of its nesting depth (every
 * level adds two spaces to every line below it). A value is indented only when it
 * is shallow, and then only when the result stays within INDENT_BUDGET, which is
 * estimated from the compact text: one line per token, indented by up to two
 * spaces per level, so about `compact length x (depth + 2)` characters.
 * Anything else is shown compactly.
 */
const MAX_INDENT_DEPTH = 32;
const INDENT_BUDGET = 64 * 1024;

/** Text for display, and what could not be shown. */
interface Shown {
  text: string;
  /** Characters beyond MAX_VALUE_CHARS that were left out. */
  omitted: number;
  /** The value cannot be printed at all (nested too deeply for the engine to walk). */
  unrenderable: boolean;
}

/**
 * A copy of `text` that owns its characters. A `slice` of a long string can be a
 * window onto the whole of it (V8 does this), which keeps the whole string alive
 * for as long as the slice is: 200 clipped values held on to 105 MB. Putting a
 * character in front and slicing it off again makes the engine write the
 * characters out as a string of their own.
 */
function detached(text: string): string {
  return (' ' + text).slice(1);
}

function clip(text: string): Shown {
  if (text.length <= MAX_VALUE_CHARS) return { text, omitted: 0, unrenderable: false };
  let end = MAX_VALUE_CHARS;
  const last = text.charCodeAt(end - 1);
  if (last >= 0xd800 && last <= 0xdbff) end--; // never cut a surrogate pair in half
  return { text: detached(text.slice(0, end)), omitted: text.length - end, unrenderable: false };
}

/**
 * How many containers deep `root` goes, counting `root` as 1, measured without
 * recursion and abandoned as soon as it reaches `stopAt` (which is returned).
 */
function nestingDepth(root: object, stopAt: number): number {
  let deepest = 1;
  const stack: object[] = [root];
  const depths: number[] = [1];
  while (stack.length > 0) {
    const node = stack.pop() as object;
    const depth = depths.pop() as number;
    if (depth > deepest) {
      deepest = depth;
      if (deepest >= stopAt) return stopAt;
    }
    const children: unknown[] = Array.isArray(node) ? node : Object.values(node);
    for (const child of children) {
      if (child !== null && typeof child === 'object') {
        stack.push(child);
        depths.push(depth + 1);
      }
    }
  }
  return deepest;
}

/** Compact JSON of `value`: linear in its size. `undefined` when the engine cannot walk it. */
function compactJson(value: unknown): string | undefined | null {
  try {
    // `undefined` for a value JSON has no spelling for (not reachable from parsed JSON).
    return JSON.stringify(value) ?? undefined;
  } catch {
    // Nesting deep enough to overflow the stack, or a BigInt.
    return null;
  }
}

/**
 * The text a field's value is shown as. A string is shown as it is. Anything
 * else is printed compactly first (linear in its size); it is indented only when
 * that stays cheap (see INDENT_BUDGET). The result is clipped to
 * MAX_VALUE_CHARS, with the number of characters left out reported.
 */
function showValue(value: unknown): Shown {
  if (typeof value === 'string') return clip(value);
  const compact = compactJson(value);
  if (compact === null) return { text: '', omitted: 0, unrenderable: true };
  if (compact === undefined) return clip(String(value));

  let text = compact;
  // Even a flat value is about three times its compact length once indented, so
  // anything past a third of the budget is not worth measuring.
  if (typeof value === 'object' && value !== null && compact.length * 3 <= INDENT_BUDGET) {
    const depth = nestingDepth(value, MAX_INDENT_DEPTH + 1);
    if (depth <= MAX_INDENT_DEPTH && compact.length * (depth + 2) <= INDENT_BUDGET) {
      text = JSON.stringify(value, null, 2);
    }
  }
  return clip(text);
}

// ---------------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------------

/**
 * One recorded field shown as a labelled row.
 *
 * Everything but `key` is worked out the first time any of it is read (see
 * `toField`), so a call with thousands of fields costs only for the rows drawn.
 */
export interface ToolField {
  key: string;
  /** Strings verbatim; anything else as JSON (indented only when small and shallow; see INDENT_BUDGET). Possibly clipped. */
  value: string;
  /** Multi-line or long: render under its key instead of beside it. */
  block: boolean;
  /** Characters of the value that were not kept (it was clipped). `undefined` when nothing was. */
  omitted?: number;
  /** The value is nested too deeply to print; `value` is empty. `undefined` otherwise. */
  unrenderable?: boolean;
}

export interface ToolEdit {
  oldText: string;
  newText: string;
  /** Options of this one edit, e.g. `replace all occurrences` inside a MultiEdit. */
  chips: string[];
}

export interface TodoItem {
  content: string;
  status: string;
  /** The present-tense wording of `content` ("Running tests"); only when it differs from it. */
  activeForm?: string;
}

export type ToolCallBody =
  /** Bash: a shell command. */
  | { kind: 'command'; caption: string | null; command: string; chips: string[] }
  /** Edit and MultiEdit: replacements within one file. */
  | { kind: 'edit'; filePath: string; edits: ToolEdit[]; chips: string[] }
  /** Write and NotebookEdit: the full new content of a file or cell. */
  | { kind: 'file_content'; filePath: string; content: string; chips: string[] }
  /** Read, Glob, Grep, WebSearch, WebFetch: one subject plus options. */
  | { kind: 'lookup'; label: string; value: string; detail: string | null; chips: string[] }
  /** Agent (formerly Task): work handed to a sub-agent. */
  | { kind: 'delegation'; caption: string | null; prompt: string; chips: string[] }
  /** TodoWrite: a checklist. */
  | { kind: 'todos'; todos: TodoItem[] }
  /** Any other tool, or a known one with unexpected fields. */
  | { kind: 'fields'; fields: ToolField[] }
  /** The capture limit was exceeded. */
  | { kind: 'truncated'; bytes: number }
  /** No input was recorded: empty `arguments`, or the JSON text `null` (a tool call without an input). */
  | { kind: 'none' }
  /**
   * `arguments` was not a JSON object (or, in legacy data, not text at all and is
   * shown as compact JSON). `omitted` and `unrenderable` as in `ToolField`.
   */
  | { kind: 'raw'; text: string; omitted?: number; unrenderable?: boolean };

export type ToolCallView = ToolCallBody & {
  /** The recorded tool name. */
  tool: string;
  /** Card heading: see `toolTitle`. */
  title: string;
  /** Recorded fields the view above does not already show. */
  extras: ToolField[];
};

/**
 * The name a tool is shown under, wherever it appears (a call, or its result in
 * the next step's stimulus), so the two read alike:
 *
 * - `mcp__server__tool` → `server · tool`;
 * - `server:web_search` → `web_search (server-side)`;
 * - `unknown-tool` (what the producer writes when a result's call is not known) →
 *   `Unknown tool`;
 * - every other name as recorded.
 */
export function toolTitle(toolName: string): string {
  if (toolName === '' || toolName === 'unknown-tool') return 'Unknown tool';
  if (isServerSideTool(toolName)) return `${toolName.slice(SERVER_PREFIX.length)} (server-side)`;
  if (toolName.startsWith('mcp__')) {
    const rest = toolName.slice('mcp__'.length);
    const split = rest.indexOf('__');
    if (split > 0 && split + 2 < rest.length) return `${rest.slice(0, split)} · ${rest.slice(split + 2)}`;
  }
  return toolName;
}

const INLINE_VALUE_LIMIT = 80;

/**
 * One recorded field as a row. What the row shows is worked out the first time any
 * of it is read, not when the row is made: a call can hold thousands of fields and
 * only a page of them is drawn, so the rest cost a key and a reference. Reading a
 * row reads the same as ever; only when the work happens differs.
 */
function toField(key: string, value: unknown): ToolField {
  let shown: Shown | undefined;
  const read = (): Shown => (shown ??= showValue(value));
  return {
    key,
    get value() {
      return read().text;
    },
    get block() {
      const { text } = read();
      return text.includes('\n') || text.length > INLINE_VALUE_LIMIT;
    },
    get omitted() {
      const { omitted } = read();
      return omitted > 0 ? omitted : undefined;
    },
    get unrenderable() {
      return read().unrenderable ? true : undefined;
    },
  };
}

function toFields(args: Record<string, unknown>): ToolField[] {
  return Object.entries(args).map(([key, value]) => toField(key, value));
}

/**
 * Reads typed fields out of the arguments and remembers which were used. A field
 * of the wrong type is not "used": it stays in `rest()` and is shown as is.
 */
interface Reader {
  str(key: string): string | undefined;
  num(key: string): number | undefined;
  bool(key: string): boolean | undefined;
  list(key: string): unknown[] | undefined;
  rest(): ToolField[];
}

function createReader(args: Record<string, unknown>): Reader {
  const used = new Set<string>();
  const take = <T>(key: string, accept: (value: unknown) => value is T): T | undefined => {
    const value = Object.hasOwn(args, key) ? args[key] : undefined;
    if (!accept(value)) return undefined;
    used.add(key);
    return value;
  };
  return {
    str: (key) => take(key, (v): v is string => typeof v === 'string'),
    num: (key) => take(key, (v): v is number => typeof v === 'number' && Number.isFinite(v)),
    bool: (key) => take(key, (v): v is boolean => typeof v === 'boolean'),
    list: (key) => take(key, (v): v is unknown[] => Array.isArray(v)),
    rest: () =>
      Object.entries(args)
        .filter(([key]) => !used.has(key))
        .map(([key, value]) => toField(key, value)),
  };
}

function formatDuration(ms: number): string {
  return Number.isInteger(ms) && ms >= 1000 && ms % 1000 === 0 ? `${ms / 1000}s` : `${ms}ms`;
}

/** `offset`/`limit` of a Read, as the lines they select (both are optional). */
function lineRange(offset: number | undefined, limit: number | undefined): string | null {
  const start = offset !== undefined && offset >= 1 ? Math.floor(offset) : undefined;
  const count = limit !== undefined && limit >= 1 ? Math.floor(limit) : undefined;
  if (start !== undefined && count !== undefined) {
    return count === 1 ? `line ${start}` : `lines ${start}–${start + count - 1}`;
  }
  if (start !== undefined) return `from line ${start}`;
  if (count !== undefined) return count === 1 ? 'line 1' : `lines 1–${count}`;
  return null;
}

type Body = ToolCallBody | null;

function describeBash(r: Reader): Body {
  const command = r.str('command');
  if (command === undefined) return null;
  const caption = r.str('description') ?? null;
  const chips: string[] = [];
  const timeout = r.num('timeout');
  if (timeout !== undefined) chips.push(`timeout ${formatDuration(timeout)}`);
  if (r.bool('run_in_background') === true) chips.push('runs in background');
  return { kind: 'command', caption, command, chips };
}

function describeEdit(r: Reader): Body {
  const filePath = r.str('file_path');
  const oldText = r.str('old_string');
  const newText = r.str('new_string');
  if (filePath === undefined || oldText === undefined || newText === undefined) return null;
  const chips = r.bool('replace_all') === true ? ['replace all occurrences'] : [];
  return { kind: 'edit', filePath, edits: [{ oldText, newText, chips: [] }], chips };
}

const MULTI_EDIT_KEYS = new Set(['old_string', 'new_string', 'replace_all']);

function describeMultiEdit(r: Reader): Body {
  const filePath = r.str('file_path');
  const items = r.list('edits');
  if (filePath === undefined || items === undefined || items.length === 0) return null;

  const edits: ToolEdit[] = [];
  for (const item of items) {
    // Any unexpected shape makes the whole call fall back to plain rows, so
    // nothing recorded is hidden behind a partial diff.
    if (!isPlainObject(item) || Object.keys(item).some((key) => !MULTI_EDIT_KEYS.has(key))) return null;
    const { old_string: oldText, new_string: newText, replace_all: replaceAll } = item;
    if (typeof oldText !== 'string' || typeof newText !== 'string') return null;
    if (replaceAll !== undefined && typeof replaceAll !== 'boolean') return null;
    edits.push({ oldText, newText, chips: replaceAll === true ? ['replace all occurrences'] : [] });
  }
  return { kind: 'edit', filePath, edits, chips: [] };
}

function describeWrite(r: Reader): Body {
  const filePath = r.str('file_path');
  const content = r.str('content');
  if (filePath === undefined || content === undefined) return null;
  return { kind: 'file_content', filePath, content, chips: [] };
}

function describeNotebookEdit(r: Reader): Body {
  const filePath = r.str('notebook_path');
  const content = r.str('new_source');
  if (filePath === undefined || content === undefined) return null;
  const chips: string[] = [];
  const cellId = r.str('cell_id');
  if (cellId !== undefined) chips.push(`cell ${cellId}`);
  const cellType = r.str('cell_type');
  if (cellType !== undefined) chips.push(`type ${cellType}`);
  const editMode = r.str('edit_mode');
  if (editMode !== undefined) chips.push(`mode ${editMode}`);
  return { kind: 'file_content', filePath, content, chips };
}

function describeRead(r: Reader): Body {
  const filePath = r.str('file_path');
  if (filePath === undefined) return null;
  const chips: string[] = [];
  const range = lineRange(r.num('offset'), r.num('limit'));
  if (range) chips.push(range);
  const pages = r.str('pages');
  if (pages !== undefined) chips.push(`pages ${pages}`);
  return { kind: 'lookup', label: 'file', value: filePath, detail: null, chips };
}

function describeGlob(r: Reader): Body {
  const pattern = r.str('pattern');
  if (pattern === undefined) return null;
  const path = r.str('path');
  return { kind: 'lookup', label: 'pattern', value: pattern, detail: null, chips: path === undefined ? [] : [`in ${path}`] };
}

function describeGrep(r: Reader): Body {
  const pattern = r.str('pattern');
  if (pattern === undefined) return null;
  const chips: string[] = [];
  const path = r.str('path');
  if (path !== undefined) chips.push(`in ${path}`);
  const glob = r.str('glob');
  if (glob !== undefined) chips.push(`glob ${glob}`);
  const type = r.str('type');
  if (type !== undefined) chips.push(`type ${type}`);
  const outputMode = r.str('output_mode');
  if (outputMode !== undefined) chips.push(`output ${outputMode}`);
  if (r.bool('-i') === true) chips.push('ignore case');
  if (r.bool('-n') === true) chips.push('line numbers');
  if (r.bool('multiline') === true) chips.push('multiline');
  for (const flag of ['-A', '-B', '-C']) {
    const lines = r.num(flag);
    if (lines !== undefined) chips.push(`${flag} ${lines}`);
  }
  const headLimit = r.num('head_limit');
  if (headLimit !== undefined) chips.push(`head_limit ${headLimit}`);
  return { kind: 'lookup', label: 'pattern', value: pattern, detail: null, chips };
}

function describeWebSearch(r: Reader): Body {
  const query = r.str('query');
  if (query === undefined) return null;
  return { kind: 'lookup', label: 'query', value: query, detail: null, chips: [] };
}

function describeWebFetch(r: Reader): Body {
  const url = r.str('url');
  if (url === undefined) return null;
  // The URL is plain text, never a link: it is data the model chose.
  return { kind: 'lookup', label: 'URL', value: url, detail: r.str('prompt') ?? null, chips: [] };
}

function describeAgent(r: Reader): Body {
  const prompt = r.str('prompt');
  if (prompt === undefined) return null;
  const caption = r.str('description') ?? null;
  const subagent = r.str('subagent_type');
  return { kind: 'delegation', caption, prompt, chips: subagent === undefined ? [] : [`subagent ${subagent}`] };
}

const TODO_KEYS = new Set(['content', 'status', 'activeForm']);

function describeTodoWrite(r: Reader): Body {
  const items = r.list('todos');
  if (items === undefined) return null;
  const todos: TodoItem[] = [];
  for (const item of items) {
    if (!isPlainObject(item) || Object.keys(item).some((key) => !TODO_KEYS.has(key))) return null;
    const { content, status, activeForm } = item;
    if (typeof content !== 'string' || typeof status !== 'string') return null;
    if (activeForm !== undefined && typeof activeForm !== 'string') return null;
    const todo: TodoItem = { content, status };
    // The present-tense wording of the item ("Running tests"), when it adds anything.
    if (activeForm !== undefined && activeForm !== content) todo.activeForm = activeForm;
    todos.push(todo);
  }
  return { kind: 'todos', todos };
}

const DESCRIBERS: Record<string, (r: Reader) => Body> = {
  Bash: describeBash,
  Edit: describeEdit,
  MultiEdit: describeMultiEdit,
  Write: describeWrite,
  NotebookEdit: describeNotebookEdit,
  Read: describeRead,
  Glob: describeGlob,
  Grep: describeGrep,
  Agent: describeAgent,
  Task: describeAgent,
  WebFetch: describeWebFetch,
  WebSearch: describeWebSearch,
  TodoWrite: describeTodoWrite,
};

/** Server-side tools take the same input as the client-side tool of the same purpose. */
const SERVER_DESCRIBERS: Record<string, (r: Reader) => Body> = {
  web_search: describeWebSearch,
  web_fetch: describeWebFetch,
};

function findDescriber(tool: string): ((r: Reader) => Body) | undefined {
  if (Object.hasOwn(DESCRIBERS, tool)) return DESCRIBERS[tool];
  if (isServerSideTool(tool)) {
    const name = tool.slice(SERVER_PREFIX.length);
    return Object.hasOwn(SERVER_DESCRIBERS, name) ? SERVER_DESCRIBERS[name] : undefined;
  }
  return undefined;
}

function rawBody(shown: Shown): ToolCallBody {
  const body: ToolCallBody = { kind: 'raw', text: shown.text };
  if (shown.omitted > 0) body.omitted = shown.omitted;
  if (shown.unrenderable) body.unrenderable = true;
  return body;
}

export function describeToolCall(
  exec: Pick<ToolExecution, 'tool_name' | 'arguments'>,
): ToolCallView {
  const tool = typeof exec?.tool_name === 'string' ? exec.tool_name : '';
  const title = toolTitle(tool);
  const recorded: unknown = exec?.arguments;

  if (recorded !== null && recorded !== undefined && typeof recorded !== 'string') {
    // Legacy, unvalidated data: arguments that are not text at all (an object, a
    // number). Shown as compact JSON, in the raw style, and bounded like any value.
    const compact = compactJson(recorded);
    const shown: Shown =
      compact === null
        ? { text: '', omitted: 0, unrenderable: true }
        : clip(compact === undefined ? String(recorded) : compact);
    return { ...rawBody(shown), tool, title, extras: [] };
  }

  const parsed = parseToolArguments(recorded);
  if (parsed.kind === 'truncated') return { kind: 'truncated', bytes: parsed.bytes, tool, title, extras: [] };
  if (parsed.kind === 'raw') {
    // A tool_use without an input is recorded as the JSON text `null`.
    const text = parsed.text.trim();
    if (text === '' || text === 'null') return { kind: 'none', tool, title, extras: [] };
    return { ...rawBody(clip(parsed.text)), tool, title, extras: [] };
  }

  const describe = findDescriber(tool);
  if (describe) {
    const reader = createReader(parsed.value);
    const body = describe(reader);
    if (body) return { ...body, tool, title, extras: reader.rest() };
  }
  return { kind: 'fields', fields: toFields(parsed.value), tool, title, extras: [] };
}
