/**
 * Presentation logic for interaction nodes captured from Claude Code
 * transcripts (issue #105).
 *
 * The timeline's original layout (Thought / Action / Observation) was written
 * for MCP `commit_thought` records, where `user_prompt` is the agent's task and
 * `model_response` is its answer. A Claude Code node means something else by the
 * same fields:
 *
 * - `user_prompt` is the STIMULUS the response reacted to: the human's prompt,
 *   or the previous step's tool results rendered as `[tool_result Bash] ...`;
 * - `model_response` is the assistant's text, not what a tool returned;
 * - `tool_executions[].arguments` is the tool input as one JSON string;
 * - `author` is the stimulus origin (`human` or `system`), never `agent`.
 *
 * Everything in this module is pure: no React, no DOM.
 */
import type { InteractionNode } from '../types/cvc';
import { SERVER_PREFIX, toolOutcome, toolTitle } from './toolCalls';

export type NodeLayout = 'triad' | 'transcript';

/**
 * RFC 9562 version 8 with the RFC variant bits, which is what the harness's
 * deterministic `derived_interaction_id` writes. Every other producer uses a
 * random (version 4) id. The pattern is anchored and fixed-width, so it cannot
 * backtrack.
 */
const UUID_V8 = /^[0-9a-f]{8}-[0-9a-f]{4}-8[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

const NO_STIMULUS = '[no stimulus recorded]';
/** The exact spellings of the markers that start a `meta` and a `compaction` part. */
const META_MARKER = '[meta] ';
const COMPACTION_MARKER = '[compaction summary]\n';
const STIMULUS_PREFIXES = ['[tool_result ', META_MARKER, '[compaction summary]'];

/**
 * Chooses how a node is laid out.
 *
 * The result is PRESENTATIONAL ONLY. It is a guess from the shape of the node,
 * not provenance: it makes no claim about which tool captured the node, and it
 * must never drive a badge, label or tooltip saying "captured by Claude Code"
 * (or anything else about origin). A forged or hand-edited node can land in
 * either layout; the layout only decides which fields are shown under which
 * heading.
 *
 * Rules, in order:
 * 1. `agent` author: triad. MCP and legacy agent nodes keep their rendering,
 *    even when they carry tool executions.
 * 2. A version 8 UUID: transcript.
 * 3. A `user_prompt` that starts with a harness stimulus marker: transcript.
 * 4. Any tool execution: transcript (only the harness writes them, and agent
 *    nodes are already excluded by rule 1).
 * 5. Otherwise triad: VS Code/LSP, `cvc run` and legacy nodes are unchanged.
 */
export function nodeLayout(
  node: Pick<InteractionNode, 'author' | 'id' | 'user_prompt' | 'tool_executions'>,
): NodeLayout {
  if (node.author === 'agent') return 'triad';
  if (typeof node.id === 'string' && UUID_V8.test(node.id)) return 'transcript';
  if (hasStimulusMarker(node.user_prompt)) return 'transcript';
  if (Array.isArray(node.tool_executions) && node.tool_executions.length > 0) return 'transcript';
  return 'triad';
}

function hasStimulusMarker(prompt: string | null | undefined): boolean {
  if (typeof prompt !== 'string') return false;
  return prompt === NO_STIMULUS || STIMULUS_PREFIXES.some((prefix) => prompt.startsWith(prefix));
}

/** True when the value holds something other than whitespace. */
export function hasText(value: string | null | undefined): value is string {
  return typeof value === 'string' && value.trim().length > 0;
}

// ---------------------------------------------------------------------------
// Stimulus
// ---------------------------------------------------------------------------

export type ToolResultSegment = {
  kind: 'tool_result';
  /** The tool's name as recorded, e.g. `Bash`, `mcp__server__tool` or `unknown-tool`. */
  tool: string;
  isError: boolean;
  text: string;
};

export type StimulusSegment =
  | { kind: 'prompt'; text: string }
  | { kind: 'meta'; text: string }
  | { kind: 'compaction'; text: string }
  | ToolResultSegment
  | { kind: 'none' };

interface Marker {
  kind: 'meta' | 'compaction' | 'tool_result' | 'prompt';
  /**
   * Index of the first character after the marker. The text of a `prompt`
   * boundary begins AT the boundary (its marker is part of the text), so there
   * `end` is where the marker starts.
   */
  end: number;
  tool?: string;
  isError?: boolean;
}

/** A tool name is a short token; anything longer is not a marker. */
const MAX_TOOL_NAME = 200;
const CHAR_OPEN_BRACKET = 91; // [
const CHAR_CLOSE_BRACKET = 93; // ]

/**
 * Recognises the exact marker spellings the harness writes, at `pos`:
 *
 *   `[meta] <text>`
 *   `[compaction summary]\n<text>`
 *   `[tool_result <Tool>] <content>` and `[tool_result <Tool> error] <content>`
 *
 * plus one boundary that is not the harness's own: Claude Code records a user's
 * interruption as the text `[Request interrupted by user]` or `[Request
 * interrupted by user for tool use]`. When a user rejects a tool call and types
 * a new instruction, the stimulus reads
 *
 *   `[tool_result Bash error] The user doesn't want to proceed...`
 *   `\n\n[Request interrupted by user for tool use]\n\n<the new instruction>`
 *
 * and without this boundary the instruction would be swallowed into the tool
 * result. It starts a `prompt` segment whose text includes the bracketed line.
 * Only a human can interrupt, so the boundary is looked for only when the caller
 * says the node is human-authored (`ParseOptions.interruptionBoundary`): in any
 * other node the same words are tool output (a fetched web page can contain
 * them) and must not be able to start something that reads like a human's text.
 * (`parseStimulus` goes on to honour at most one such line; see there.)
 *
 * Anything else, including near misses, is not a marker and stays part of the
 * surrounding text, so unusual input is shown rather than reinterpreted. The
 * scan is bounded (a tool name is at most MAX_TOOL_NAME characters) and never
 * backtracks, so hostile multi-KB text cannot make it slow.
 */
function markerAt(text: string, pos: number, interruptions: boolean): Marker | null {
  if (text.charCodeAt(pos) !== CHAR_OPEN_BRACKET) return null;
  if (interruptions && text.startsWith('[Request interrupted by user', pos)) return { kind: 'prompt', end: pos };
  if (text.startsWith(META_MARKER, pos)) return { kind: 'meta', end: pos + META_MARKER.length };
  if (text.startsWith(COMPACTION_MARKER, pos)) {
    return { kind: 'compaction', end: pos + COMPACTION_MARKER.length };
  }
  const head = '[tool_result ';
  if (!text.startsWith(head, pos)) return null;

  const nameStart = pos + head.length;
  const limit = Math.min(text.length, nameStart + MAX_TOOL_NAME + 1);
  let nameEnd = nameStart;
  while (nameEnd < limit) {
    const code = text.charCodeAt(nameEnd);
    if (code === CHAR_CLOSE_BRACKET || code === 32 || code === 10 || code === 13 || code === 9) break;
    nameEnd++;
  }
  if (nameEnd === nameStart || nameEnd - nameStart > MAX_TOOL_NAME) return null;
  const tool = text.slice(nameStart, nameEnd);

  if (text.startsWith(' error] ', nameEnd)) {
    return { kind: 'tool_result', tool, isError: true, end: nameEnd + ' error] '.length };
  }
  if (text.startsWith('] ', nameEnd)) {
    return { kind: 'tool_result', tool, isError: false, end: nameEnd + '] '.length };
  }
  return null;
}

function buildSegment(marker: Marker | null, body: string): StimulusSegment {
  if (!marker) return { kind: 'prompt', text: body };
  if (marker.kind === 'tool_result') {
    return { kind: 'tool_result', tool: marker.tool ?? 'unknown-tool', isError: marker.isError === true, text: body };
  }
  return { kind: marker.kind, text: body };
}

export interface ParseOptions {
  /**
   * Whether Claude Code's interruption line (see `markerAt`) can start the
   * human's reply. Pass `true` only for a node a human wrote
   * (`author === 'human'`). Off by default, so a caller that forgets to say gets
   * the cautious reading: the same words are tool output.
   */
  interruptionBoundary?: boolean;
}

/** One stretch of the prompt between two markers, by position. */
interface Part {
  marker: Marker | null;
  bodyStart: number;
  bodyEnd: number;
  /** Begins at an interruption line that follows a blank line (never the first part). */
  interruption: boolean;
}

/**
 * Which interruption line, if any, may start the human's reply: the LAST one,
 * and only when no tool result follows it. A real interruption is the last thing
 * before the human's new instruction; an interruption line with a tool result
 * after it is text inside tool output imitating one. Returns the part's index, or
 * -1. (A forged line in the LAST result of a stimulus cannot be told from a real
 * one by its text, so it still counts.)
 */
function honouredInterruption(parts: readonly Part[]): number {
  let last = -1;
  for (let i = 0; i < parts.length; i++) if (parts[i].interruption) last = i;
  if (last === -1) return -1;
  for (let i = last + 1; i < parts.length; i++) {
    if (parts[i].marker?.kind === 'tool_result') return -1;
  }
  return last;
}

/**
 * Splits a node's `user_prompt` into ordered stimulus segments.
 *
 * The harness joins its parts with a blank line (`\n\n`), each part starting
 * with one of the markers described at `markerAt`; a plain human message has no
 * marker. A new segment starts only at the beginning of the text, or after a
 * `\n\n` where a marker begins. Human text that merely mentions a marker
 * therefore yields, at worst, one extra segment.
 *
 * The split is lossless: re-joining the segments (each marker re-attached in its
 * exact spelling, separated by `\n\n`) reproduces the input byte for byte. Text
 * is never dropped, trimmed or duplicated, including truncation markers such as
 * `\n…[cvc: truncated 5120 bytes]…\n`, which stay inside the segment they cut.
 *
 * Known limit: segment bodies may themselves contain blank lines, so plain text
 * that FOLLOWS a marker segment (a message typed after tool results, say) cannot
 * be told apart from that segment's own content and is kept inside it. The one
 * exception is Claude Code's interruption line (see `markerAt` and
 * `honouredInterruption`), which precedes a human's reply to a rejected tool
 * call. `promptIsEmbedded` reports the case where the limit matters.
 *
 * A missing, blank or `[no stimulus recorded]` prompt yields a single `none`.
 */
export function parseStimulus(
  prompt: string | null | undefined,
  options: ParseOptions = {},
): StimulusSegment[] {
  if (typeof prompt !== 'string' || prompt.trim() === '' || prompt === NO_STIMULUS) {
    return [{ kind: 'none' }];
  }

  const interruptions = options.interruptionBoundary === true;
  const parts: Part[] = [];
  let marker = markerAt(prompt, 0, interruptions);
  let bodyStart = marker ? marker.end : 0;
  let interruption = false;
  // A separator can only start at or after the current segment's body.
  let from = bodyStart;
  for (;;) {
    const separator = prompt.indexOf('\n\n', from);
    if (separator === -1) break;
    const next = markerAt(prompt, separator + 2, interruptions);
    if (next) {
      parts.push({ marker, bodyStart, bodyEnd: separator, interruption });
      marker = next;
      interruption = next.kind === 'prompt';
      bodyStart = next.end;
      from = bodyStart;
    } else {
      from = separator + 1;
    }
  }
  parts.push({ marker, bodyStart, bodyEnd: prompt.length, interruption });

  // An interruption line that is not honoured is not a boundary at all: it is
  // text of the part before it, exactly as if it had not been looked for.
  const honoured = interruptions ? honouredInterruption(parts) : -1;
  const kept: Part[] = [];
  parts.forEach((part, index) => {
    if (part.interruption && index !== honoured) kept[kept.length - 1].bodyEnd = part.bodyEnd;
    else kept.push(part);
  });
  return kept.map((part) => buildSegment(part.marker, prompt.slice(part.bodyStart, part.bodyEnd)));
}

function isPromptWithText(segment: StimulusSegment): boolean {
  return segment.kind === 'prompt' && hasText(segment.text);
}

export type StimulusOrigin =
  | 'Prompt'
  | 'Tool results'
  | 'Compaction summary'
  | 'Context'
  | 'Unmarked text'
  | 'No stimulus recorded';

/**
 * The label for what the step reacted to. A node authored by `human` reacted to
 * a prompt; any other node reacted to tool results, a compaction summary, or
 * injected context (Claude Code's `isMeta` messages), in that order of
 * precedence, or to `Unmarked text` when it holds plain text and none of those.
 */
export function stimulusOrigin(
  node: Pick<InteractionNode, 'author'>,
  segments: readonly StimulusSegment[],
): StimulusOrigin {
  if (segments.every((segment) => segment.kind === 'none')) return 'No stimulus recorded';
  if (node.author === 'human') return 'Prompt';
  if (segments.some((segment) => segment.kind === 'tool_result')) return 'Tool results';
  if (segments.some((segment) => segment.kind === 'compaction')) return 'Compaction summary';
  if (segments.some((segment) => segment.kind === 'meta')) return 'Context';
  // Plain text in a stimulus nobody human wrote: its origin is not known, so it is
  // not called a prompt (see StimulusPane, which also keeps it out of prose styling).
  if (segments.some(isPromptWithText)) return 'Unmarked text';
  return 'No stimulus recorded';
}

/**
 * True when the node says a human wrote something (`author === 'human'`) yet no
 * plain-text segment could be separated out: the human's words are then
 * necessarily inside a marker segment (see `parseStimulus`, "Known limit").
 */
export function promptIsEmbedded(
  node: Pick<InteractionNode, 'author'>,
  segments: readonly StimulusSegment[],
): boolean {
  return (
    node.author === 'human' &&
    !segments.some(isPromptWithText) &&
    segments.some((segment) => segment.kind !== 'none')
  );
}

/**
 * Text drawn as output because nothing tells it from output: see `groupStimulus`.
 * Its marker is part of `text`, exactly as recorded.
 */
export type OutputSegment = { kind: 'output'; text: string };

/** What a group of tool results holds, in the order it came. */
export type ToolResultsItem = ToolResultSegment | OutputSegment;

/** What the stimulus pane shows: consecutive tool results are shown as one group. */
export type StimulusBlock =
  | { kind: 'prompt'; text: string }
  | { kind: 'meta'; text: string }
  | { kind: 'compaction'; text: string }
  | { kind: 'tool_results'; results: ToolResultsItem[] }
  | { kind: 'none' };

/** A segment of the kind that can be forged inside tool output, as it was written. */
function verbatim(segment: { kind: 'meta' | 'compaction'; text: string }): OutputSegment {
  return { kind: 'output', text: (segment.kind === 'meta' ? META_MARKER : COMPACTION_MARKER) + segment.text };
}

/**
 * Arranges segments for the stimulus pane: consecutive tool results become one
 * group, and everything else keeps its own block, in order.
 *
 * A `[meta]` or `[compaction summary]` part that comes AFTER a tool result is not
 * drawn as injected context or a summary. No check of the text can tell the
 * harness's own part from tool output imitating one (a fetched page can contain
 * `\n\n[meta] ...`), and a block labelled "Context" would lend such text the look
 * of Claude Code's own. It joins the latest group of tool results as plain
 * output, marker included. (A genuine reminder that follows tool results then
 * sits inside the group too.) Context or a summary BEFORE any tool result is
 * unchanged.
 */
export function groupStimulus(segments: readonly StimulusSegment[]): StimulusBlock[] {
  const blocks: StimulusBlock[] = [];
  let group: Extract<StimulusBlock, { kind: 'tool_results' }> | null = null;
  for (const segment of segments) {
    // A blank leading prompt is an artefact of the lossless split, not content.
    if (segment.kind === 'prompt' && !hasText(segment.text)) continue;
    if (segment.kind === 'tool_result') {
      const last = blocks[blocks.length - 1];
      if (last?.kind === 'tool_results') {
        last.results.push(segment);
        group = last;
      } else {
        group = { kind: 'tool_results', results: [segment] };
        blocks.push(group);
      }
    } else if (group && (segment.kind === 'meta' || segment.kind === 'compaction')) {
      group.results.push(verbatim(segment));
    } else {
      blocks.push(segment);
    }
  }
  return blocks;
}

const MAX_NAMES_IN_SUMMARY = 4;

/**
 * One-line summary of a group of tool results, e.g.
 * `Tool results · Bash ×2, Read · 1 error`. Text that is drawn as output (see
 * `groupStimulus`) is not a result and is not counted.
 */
export function toolResultsSummary(results: readonly ToolResultsItem[]): string {
  // Names as the tool calls show them (see `toolTitle`), so a call and its result read alike.
  const counts = new Map<string, number>();
  let errors = 0;
  for (const result of results) {
    if (result.kind !== 'tool_result') continue;
    const name = toolTitle(result.tool);
    counts.set(name, (counts.get(name) ?? 0) + 1);
    if (result.isError) errors++;
  }
  const names = Array.from(counts, ([name, count]) => (count > 1 ? `${name} ×${count}` : name));
  const shown = names.slice(0, MAX_NAMES_IN_SUMMARY);
  if (names.length > shown.length) shown.push(`+${names.length - shown.length} more`);

  const parts = ['Tool results'];
  if (shown.length > 0) parts.push(shown.join(', '));
  if (errors > 0) parts.push(errors === 1 ? '1 error' : `${errors} errors`);
  return parts.join(' · ');
}

/** A call the PREVIOUS step made, as far as a stimulus can answer it. */
export interface ParentToolCall {
  /** The recorded tool name (an entry without a usable one is `''`, which no result matches). */
  name: string;
  /** Recorded as anything but a success: it errored, or it never got a result. */
  failed: boolean;
}

/** Slots for results are counted per name AND outcome. */
function slotKey(name: string, failed: boolean): string {
  return `${failed ? 'error' : 'ok'}:${name}`;
}

/**
 * Whether the tool results a stimulus claims are plausible results of the
 * PREVIOUS step's tool calls. Each result must take a call of the same name AND
 * outcome: an `error` result answers a call that failed, a plain one a call that
 * succeeded, and slots are counted per (name, outcome). Fewer results than calls
 * is fine, since a call that was interrupted has no result. More, a name the
 * parent never called, or the wrong outcome, is not.
 *
 * Server-side calls (names starting `server:`) have no slot at all: their results
 * arrive inside the assistant's own message and are never a user-side result
 * (cvc 0.7.0, cvc-core/src/claude_code/transcript.rs), so a
 * `[tool_result server:...]` marker can never be the capture's own.
 *
 * The harness writes tool results as `[tool_result <Tool>] ...`, and so can any
 * text inside a tool's output (a fetched web page, a file the model read). When
 * a marker names a call that was never made, the segmentation cannot be trusted
 * and the stimulus is better shown undivided.
 */
export function toolResultsMatchParent(
  segments: readonly StimulusSegment[],
  parentToolCalls: readonly ParentToolCall[],
): boolean {
  const slots = new Map<string, number>();
  for (const call of parentToolCalls) {
    if (call.name.startsWith(SERVER_PREFIX)) continue;
    const key = slotKey(call.name, call.failed);
    slots.set(key, (slots.get(key) ?? 0) + 1);
  }
  for (const segment of segments) {
    if (segment.kind !== 'tool_result') continue;
    const key = slotKey(segment.tool, segment.isError);
    const left = slots.get(key) ?? 0;
    if (left === 0) return false;
    slots.set(key, left - 1);
  }
  return true;
}

/**
 * For each node, the tool calls its PREVIOUS step (its parent) made, or null when
 * it has no parent or the parent is not among `nodes` (outside the PR, say).
 * Feeds `toolResultsMatchParent`. The arrays are made once per call: as long as
 * the same `nodes` are passed in, the same arrays come out of a `useMemo` over it.
 */
export function parentToolNamesByNode(
  nodes: readonly InteractionNode[],
): Map<string, readonly ParentToolCall[] | null> {
  const byId = new Map<string, InteractionNode>();
  for (const node of nodes) byId.set(node.id, node);

  const calls = new Map<string, readonly ParentToolCall[] | null>();
  for (const node of nodes) {
    const parent = typeof node.parent_id === 'string' ? byId.get(node.parent_id) : undefined;
    const tools = parent && Array.isArray(parent.tool_executions) ? parent.tool_executions : null;
    calls.set(
      node.id,
      parent
        ? (tools ?? []).map((tool) => ({
            // An entry without a usable name cannot be matched by any result.
            name: typeof tool?.tool_name === 'string' ? tool.tool_name : '',
            failed: tool?.status !== 'success',
          }))
        : null,
    );
  }
  return calls;
}

/**
 * How many of the node's tool executions failed, interrupted calls included.
 * Server-side tools are not counted: the harness records them as failed only
 * because their result is never seen (see `toolOutcome`).
 */
export function failedToolCount(node: Pick<InteractionNode, 'tool_executions'>): number {
  if (!Array.isArray(node.tool_executions)) return 0;
  return node.tool_executions.filter((tool) => toolOutcome(tool) === 'failure').length;
}
