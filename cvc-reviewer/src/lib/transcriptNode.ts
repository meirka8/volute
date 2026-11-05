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
 * The stimulus comes in two encodings. Version 1 marks its parts with bracketed prefixes that a
 * tool's own output can imitate, so it is split by heuristics (`parseStimulus`, `groupStimulus`,
 * `toolResultsMatchParent`). Version 2 marks every part with a boundary token that is checked
 * against the node's id (`stimulusV2.ts`): `readStimulus` says which one a node holds, and a
 * stimulus that carries version 2 tokens but does not verify is shown undivided, never read as
 * version 1. `viewStimulus` turns either reading into what the stimulus pane draws.
 *
 * Everything in this module is pure: no React, no DOM.
 */
import type { InteractionNode } from '../types/cvc';
import {
  NO_STIMULUS,
  REDACTED_TOOL,
  UNKNOWN_TOOL,
  isValidToolName,
  opensWithMarker,
  readStimulusV2,
  type ContentPiece,
  type ReadPart,
} from './stimulusV2';
import { SERVER_PREFIX, splitOmitted, toolOutcome, toolTitle } from './toolCalls';

export type NodeLayout = 'triad' | 'transcript';

/**
 * RFC 9562 version 8 with the RFC variant bits, which is what the harness's
 * deterministic `derived_interaction_id` writes. Every other producer uses a
 * random (version 4) id. The pattern is anchored and fixed-width, so it cannot
 * backtrack.
 */
const UUID_V8 = /^[0-9a-f]{8}-[0-9a-f]{4}-8[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

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
 * 3. A `user_prompt` that starts with a harness stimulus marker, version 1 or version 2 (see
 *    `stimulusV2.ts`; the marker only has to be spelled right, whether it verifies is for the
 *    reader): transcript.
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
  return prompt === NO_STIMULUS || STIMULUS_PREFIXES.some((prefix) => prompt.startsWith(prefix)) || opensWithMarker(prompt);
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
  | 'Task'
  | 'Tool results'
  | 'Compaction summary'
  | 'Context'
  | 'Unmarked text'
  | 'Unverified stimulus'
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

/**
 * A tool result of a stimulus whose markers verified (see `readStimulus`). It carries its content as
 * pieces, text and the notes the capture left where it cut the result, instead of one string.
 */
export type VerifiedToolResult = {
  kind: 'verified_tool_result';
  /** The tool's name as the marker spells it; `unknown-tool` and `redacted-tool` stand in for a name that is not shown. */
  tool: string;
  isError: boolean;
  content: readonly ContentPiece[];
  /** The previous step is loaded, and none of its tool calls accounts for this result. */
  unmatched: boolean;
};

/** What a group of tool results holds, in the order it came (version 1). */
export type ToolResultsItem = ToolResultSegment | OutputSegment;

/** What the pane draws as one group of tool results: the items of a version 1 stimulus, or the results of a verified one. */
export type ResultsGroupItem = ToolResultsItem | VerifiedToolResult;

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
export function toolResultsSummary(results: readonly ResultsGroupItem[]): string {
  // Names as the tool calls show them (see `toolTitle`), so a call and its result read alike.
  const counts = new Map<string, number>();
  let errors = 0;
  for (const result of results) {
    if (result.kind === 'output') continue;
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
  /**
   * Set only on an entry that is not a call: it stands for this many calls the capture left out of the
   * previous step (see `splitOmitted`). Their names are not known, so a result that no recorded call
   * accounts for may be the result of one of them, once each.
   */
  unrecorded?: number;
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
 * (cvc-core/src/claude_code/transcript.rs), whatever protocol and status the call
 * was recorded with, so a `[tool_result server:...]` marker can never be the
 * capture's own.
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
  let unrecorded = 0;
  for (const call of parentToolCalls) {
    if (call.unrecorded !== undefined) {
      unrecorded += call.unrecorded;
      continue;
    }
    if (call.name.startsWith(SERVER_PREFIX)) continue;
    const key = slotKey(call.name, call.failed);
    slots.set(key, (slots.get(key) ?? 0) + 1);
  }
  for (const segment of segments) {
    if (segment.kind !== 'tool_result') continue;
    const key = slotKey(segment.tool, segment.isError);
    const left = slots.get(key) ?? 0;
    if (left > 0) {
      slots.set(key, left - 1);
    } else if (unrecorded > 0 && !segment.tool.startsWith(SERVER_PREFIX)) {
      // The capture left calls out of the previous step; this may be the answer to one of them.
      unrecorded--;
    } else {
      return false;
    }
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
    // The capture's note that it left calls out is not a call (see `splitOmitted`); the calls it stands for are
    // handed on as one entry that any result nothing else accounts for may answer.
    const { calls: made, omitted } = splitOmitted(tools ?? []);
    calls.set(
      node.id,
      parent
        ? [
            ...made.map((tool) => ({
              // An entry without a usable name cannot be matched by any result.
              name: typeof tool?.tool_name === 'string' ? tool.tool_name : '',
              failed: tool?.status !== 'success',
            })),
            ...(omitted !== null && omitted.toolCalls > 0 ? [{ name: '', failed: false, unrecorded: omitted.toolCalls }] : []),
          ]
        : null,
    );
  }
  return calls;
}

/**
 * How many of the node's tool calls failed, interrupted calls included.
 * A server-side call written by cvc 0.7.0 is not counted: that version recorded
 * every one as failed only because their result is never seen. One recorded with
 * the `server` protocol is, when its recorded status is a failure (see `toolOutcome`).
 * The capture's note that it left calls out is not a call and is never counted.
 */
export function failedToolCount(node: Pick<InteractionNode, 'tool_executions'>): number {
  if (!Array.isArray(node.tool_executions)) return 0;
  return splitOmitted(node.tool_executions).calls.filter((tool) => toolOutcome(tool) === 'failure').length;
}

// ---------------------------------------------------------------------------
// Stimulus, version 2: markers verified against the node's id
// ---------------------------------------------------------------------------

/** What a node's stimulus was read as. */
export type StimulusRead =
  /** No version 2 token anywhere: version 1, or text no harness wrote. Split by `parseStimulus`. */
  | { kind: 'legacy'; segments: StimulusSegment[] }
  /** The boundary token verified: the parts are the capture's own. */
  | { kind: 'verified'; parts: ReadPart[]; omittedParts: bigint | null }
  /** Version 2 tokens that do not verify (damaged, or not written for this node): `text` is shown as it is, undivided. */
  | { kind: 'unverified'; text: string };

/**
 * Reads a node's stimulus, as `readStimulusV2` does, and falls back to version 1 only for text that
 * has no version 2 token at all. A stimulus whose tokens do not verify is NEVER split by the version 1
 * heuristics: its markers are text, and so is everything around them.
 *
 * `author` matters only to version 1 (whether Claude Code's interruption line can start a person's
 * reply); a verified stimulus says what each part is itself.
 */
export function readStimulus(node: Pick<InteractionNode, 'id' | 'author' | 'user_prompt'>): StimulusRead {
  const prompt = node.user_prompt;
  if (typeof prompt === 'string') {
    const reading = readStimulusV2(node.id, prompt);
    if (reading.kind === 'verified') return { kind: 'verified', parts: reading.parts, omittedParts: reading.omittedParts };
    if (reading.kind === 'undivided') return { kind: 'unverified', text: prompt };
  }
  return { kind: 'legacy', segments: parseStimulus(prompt, { interruptionBoundary: node.author === 'human' }) };
}

/** What the stimulus pane draws for a verified stimulus: parts in order, consecutive tool results as one group. */
export type VerifiedBlock =
  /** A person's text. */
  | { kind: 'prompt'; content: readonly ContentPiece[] }
  /** What the agent that delegated this work told a subagent to do. No person typed it. */
  | { kind: 'task'; content: readonly ContentPiece[] }
  | { kind: 'meta'; content: readonly ContentPiece[] }
  | { kind: 'compaction'; content: readonly ContentPiece[] }
  | { kind: 'tool_results'; results: VerifiedToolResult[] };

const NO_INDEXES: ReadonlySet<number> = new Set();

/**
 * Arranges the parts of a verified stimulus for the pane. Unlike version 1 (see `groupStimulus`), a
 * `meta` or compaction part after a tool result is still drawn as context or a summary: the boundary
 * says the capture wrote it, and tool output cannot have. `unmatched` holds the indexes of the parts
 * (see `unmatchedResults`) whose results the previous step's calls do not account for.
 *
 * A part with nothing in it (the empty text of a person, a summary) draws nothing; an empty tool
 * result is still a result, and says so.
 */
export function groupVerified(parts: readonly ReadPart[], unmatched: ReadonlySet<number> = NO_INDEXES): VerifiedBlock[] {
  const blocks: VerifiedBlock[] = [];
  parts.forEach((part, index) => {
    if (part.kind === 'tool_result') {
      const result: VerifiedToolResult = {
        kind: 'verified_tool_result',
        tool: part.name ?? UNKNOWN_TOOL,
        isError: part.isError,
        content: part.content,
        unmatched: unmatched.has(index),
      };
      const last = blocks[blocks.length - 1];
      if (last?.kind === 'tool_results') last.results.push(result);
      else blocks.push({ kind: 'tool_results', results: [result] });
      return;
    }
    if (part.content.length === 0) return;
    blocks.push({ kind: part.kind === 'compaction_summary' ? 'compaction' : part.kind, content: part.content });
  });
  return blocks;
}

/**
 * The label for what a verified stimulus holds. A person's words come first, since they are what the
 * step is an answer to; a delegated task is not one, and is named for what it is.
 */
export function verifiedOrigin(blocks: readonly VerifiedBlock[]): StimulusOrigin {
  if (blocks.some((block) => block.kind === 'prompt')) return 'Prompt';
  if (blocks.some((block) => block.kind === 'task')) return 'Task';
  if (blocks.some((block) => block.kind === 'tool_results')) return 'Tool results';
  if (blocks.some((block) => block.kind === 'compaction')) return 'Compaction summary';
  if (blocks.some((block) => block.kind === 'meta')) return 'Context';
  return 'No stimulus recorded';
}

/** A privacy redaction inside a recorded name: `[CVC_REDACTED:v1:<kind>:<uuid>]`. The id is random for every redaction. */
const REDACTION_MARKER = /\[CVC_REDACTED:v1:[a-z_]+:[0-9a-fA-F-]+\]/;

/**
 * The name a tool result carries for a call recorded under `callName`, which is what the capture's
 * writer makes of it: a name the scrubber changed is `redacted-tool` (whatever random ids its
 * redaction markers hold, so two recordings of one name agree), a name a marker cannot spell is
 * `unknown-tool`, and any other name is itself. A call with no usable name (`''`) has no result.
 */
export function toolResultName(callName: string): string {
  if (callName === '') return '';
  if (REDACTION_MARKER.test(callName)) return REDACTED_TOOL;
  return isValidToolName(callName) ? callName : UNKNOWN_TOOL;
}

/**
 * The tool results of a verified stimulus that the PREVIOUS step's calls do not account for, as the
 * indexes of their parts. The same rule as `toolResultsMatchParent` (a result takes a call of the same
 * name and outcome, server-side calls have no slot) with names compared as `toolResultName` spells
 * them, so a redacted call and its `redacted-tool` result agree.
 *
 * The division of a verified stimulus is not in doubt, so a result with no call does not undo it: the
 * pane puts a note on that result and nowhere else.
 */
export function unmatchedResults(parts: readonly ReadPart[], parentToolCalls: readonly ParentToolCall[]): Set<number> {
  const slots = new Map<string, number>();
  let unrecorded = 0;
  for (const call of parentToolCalls) {
    if (call.unrecorded !== undefined) {
      unrecorded += call.unrecorded;
      continue;
    }
    if (call.name.startsWith(SERVER_PREFIX)) continue;
    const key = slotKey(toolResultName(call.name), call.failed);
    slots.set(key, (slots.get(key) ?? 0) + 1);
  }
  const unmatched = new Set<number>();
  parts.forEach((part, index) => {
    if (part.kind !== 'tool_result') return;
    const name = part.name ?? UNKNOWN_TOOL;
    const key = slotKey(name, part.isError);
    const left = slots.get(key) ?? 0;
    if (left > 0) slots.set(key, left - 1);
    else if (unrecorded > 0 && !name.startsWith(SERVER_PREFIX)) unrecorded--; // the answer to a call the capture left out
    else unmatched.add(index);
  });
  return unmatched;
}

/** What the stimulus pane draws. */
export type StimulusView =
  /** A version 1 stimulus split by its markers. */
  | { kind: 'segments'; segments: StimulusSegment[]; promptIsEmbedded: boolean }
  /** A version 1 stimulus whose tool results do not match the previous step's calls (see `toolResultsMatchParent`). */
  | { kind: 'undivided'; text: string }
  /** A stimulus with version 2 tokens that do not verify. */
  | { kind: 'unverified'; text: string }
  /** A stimulus whose markers verified. */
  | { kind: 'verified'; blocks: VerifiedBlock[]; omittedParts: bigint | null };

/**
 * What the pane draws for a node's stimulus, and the label it goes under in the header.
 *
 * `parentToolCalls` is what the PREVIOUS step called, or null when that step is not loaded (see
 * `parentToolNamesByNode`). A version 1 stimulus whose results do not match is shown undivided, since
 * its markers are plain text. A verified one is not: it keeps its division and the results that have
 * no call are marked one by one. Only version 1 has the interruption line and the embedded prompt.
 */
export function viewStimulus(
  node: Pick<InteractionNode, 'author' | 'user_prompt'>,
  read: StimulusRead,
  parentToolCalls: readonly ParentToolCall[] | null,
): { view: StimulusView; origin: StimulusOrigin } {
  switch (read.kind) {
    case 'legacy': {
      const origin = stimulusOrigin(node, read.segments);
      const undivided = parentToolCalls !== null && !toolResultsMatchParent(read.segments, parentToolCalls);
      if (undivided && typeof node.user_prompt === 'string') return { view: { kind: 'undivided', text: node.user_prompt }, origin };
      return {
        view: { kind: 'segments', segments: read.segments, promptIsEmbedded: !undivided && promptIsEmbedded(node, read.segments) },
        origin,
      };
    }
    case 'unverified':
      return { view: { kind: 'unverified', text: read.text }, origin: 'Unverified stimulus' };
    case 'verified': {
      const unmatched = parentToolCalls === null ? NO_INDEXES : unmatchedResults(read.parts, parentToolCalls);
      const blocks = groupVerified(read.parts, unmatched);
      return { view: { kind: 'verified', blocks, omittedParts: read.omittedParts }, origin: verifiedOrigin(blocks) };
    }
  }
}

// ---------------------------------------------------------------------------
// Fields the capture could not scrub
// ---------------------------------------------------------------------------

const WITHHELD_FIELD = /^\[cvc: withheld (0|[1-9][0-9]{0,17}) bytes that could not be scrubbed\]$/;

/** What a capture records as the response of a step it had to withhold whole (see `isWithheldResponse`). */
export const WITHHELD_RESPONSE = '[cvc: withheld this response: it could not be scrubbed within the limits]';

/**
 * Whether the node is a STAND-IN: the capture could not store this response within its limits, so it
 * recorded the step (its id, its place in the conversation and its stimulus) with this note in place of
 * the response, and no reasoning, no tool calls and no context items. Rare. All of that must hold, so
 * that a response that merely says the sentence, in a step that has calls or reasoning, is text.
 */
export function isWithheldResponse(
  node: Pick<InteractionNode, 'model_response' | 'model_cot' | 'tool_executions' | 'context_items'>,
): boolean {
  return (
    node.model_response === WITHHELD_RESPONSE &&
    node.model_cot == null &&
    Array.isArray(node.tool_executions) &&
    node.tool_executions.length === 0 &&
    Array.isArray(node.context_items) &&
    node.context_items.length === 0
  );
}

/**
 * How many bytes the capture withheld, when `value` is exactly the note it records in place of a
 * response or reasoning it could not scrub: `[cvc: withheld N bytes that could not be scrubbed]`.
 * Text that merely contains the sentence is text, and `null`.
 */
export function withheldFieldBytes(value: string | null | undefined): bigint | null {
  if (typeof value !== 'string') return null;
  const match = WITHHELD_FIELD.exec(value);
  return match === null ? null : BigInt(match[1]);
}
