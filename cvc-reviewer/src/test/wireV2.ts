/**
 * The nodes of `./wire/cvc-0.7.1-stimulus-v2/`, by what each one is for.
 *
 * They are the steps of one synthetic Claude Code session, as the harness that marks every part of the
 * stimulus with a boundary token verified against the node's id (issue #133) wrote them: a person's words,
 * a subagent's task, injected context, a compaction summary and each tool result are parts of their own.
 * Several have the id of a step in `edge-cases/` or `cvc-0.7.1-server-tools/`, recorded again by that harness.
 * Shared by the suites that read them: `wireNodes.test.tsx` and `wireNodes.v2.test.tsx`.
 */
import { normalizeInteraction, type CVCBlobData, type InteractionNode } from '../types/cvc';

export const V2_FOLDER = 'cvc-0.7.1-stimulus-v2';

/** Fixture keys (`<folder>/<interaction id>`), named for what the step shows. */
export const V2_NODES = {
  /** A person's prompt of 64 KiB, cut at its end by the capture. */
  longPrompt: `${V2_FOLDER}/00421188-5eae-86e7-b033-ef664ee82ae0`,
  /** A prompt with Windows line endings, and a tool result whose call is not known. */
  orphanResult: `${V2_FOLDER}/04d711be-f819-8298-9c67-babb2f00b891`,
  /** The result of an MCP tool, in a step that runs a server-side search. */
  serverSearch: `${V2_FOLDER}/0d3a13e0-0ad7-8fb2-959d-fe4eb3499aa0`,
  /** Twelve results of one tool, each cut head and tail. */
  twelveResults: `${V2_FOLDER}/0f18d1f8-42e5-8506-a1fe-de2e53044f08`,
  /** One Bash result. */
  commitResult: `${V2_FOLDER}/267710d5-0ae8-80c5-884a-2b1f0a1e185b`,
  /** A step whose response, reasoning and a tool call's arguments the capture could not scrub. */
  withheldFields: `${V2_FOLDER}/542d66c1-f085-8e3e-b543-7becdf6f6e97`,
  /** The results of a Write and a Read. */
  writeAndRead: `${V2_FOLDER}/7ccc6fb3-c982-8d8e-b806-8063439ebc10`,
  /** Injected context, then a person's prompt; twelve Read calls. */
  contextAndPrompt: `${V2_FOLDER}/8441d350-34cb-87b5-9eec-a0879822d82e`,
  /** One Bash result. */
  listing: `${V2_FOLDER}/87686162-3097-84de-a7b6-27fe39214f77`,
  /** The sentinel for no stimulus, which every version writes the same. */
  noStimulus: `${V2_FOLDER}/a3c1e9a5-9640-88c9-b751-233c60810621`,
  /** A compaction summary. */
  compaction: `${V2_FOLDER}/a4680142-349b-884f-8170-84e51fb18a9b`,
  /** Four results, one an error and one cut head and tail. */
  batchResults: `${V2_FOLDER}/a5eb6e77-a1d9-85d4-93d0-fe978a93e042`,
  /** A prompt asking for five calls, one of them to a tool whose name was redacted. */
  fiveCalls: `${V2_FOLDER}/a68c65a0-793c-8274-b974-e567e8c84ed1`,
  /** The first step of a subagent: the task its delegating agent sent. */
  task: `${V2_FOLDER}/a6c820c1-a3b8-8766-ba5b-e6ab38c4eeb7`,
  /** A prompt of four parts (two paragraphs, an image and a document), with redaction markers. */
  redactedPrompt: `${V2_FOLDER}/ac5cc79b-a8ab-843a-83b2-2094b61f9da2`,
  /** The interruption line Claude Code records, as a person's text. */
  interrupted: `${V2_FOLDER}/add98e0a-844d-8c31-ad8a-8366ea0f4e7f`,
  /** Five results, the output of one imitating markers of every kind, and a person's words after them. */
  forgedResults: `${V2_FOLDER}/c727978d-df55-8fa3-98c1-02b7698dda1c`,
  /** A person's prompt and six calls, three of which failed. */
  failures: `${V2_FOLDER}/e05d5e7c-2937-84c5-8410-0160c72ab220`,
  /** A prompt asking for seven server-side calls and a Read. */
  serverTools: `${V2_FOLDER}/8fc40ad6-0885-8c48-8d10-ddec124a3a0a`,
  /** The Read result that answers `serverTools`. */
  serverFollowUp: `${V2_FOLDER}/94705671-dba9-8399-8430-839c3fb758d0`,
} as const;

export type V2Key = (typeof V2_NODES)[keyof typeof V2_NODES];

export const V2_KEYS: readonly V2Key[] = Object.values(V2_NODES);

const RAW = import.meta.glob<string>('./wire/cvc-0.7.1-stimulus-v2/*.json', { eager: true, query: '?raw', import: 'default' });

/** The text of every file of the folder, by fixture key. */
export const V2_RAW = new Map<string, string>(
  Object.entries(RAW).map(([path, raw]) => [path.replace(/^\.\/wire\//, '').replace(/\.json$/, ''), raw]),
);

export function v2Blob(key: V2Key): CVCBlobData {
  const raw = V2_RAW.get(key);
  if (raw === undefined) throw new Error(`No wire fixture ${key}`);
  return JSON.parse(raw) as CVCBlobData;
}

export function v2Node(key: V2Key): InteractionNode {
  return normalizeInteraction(v2Blob(key));
}
