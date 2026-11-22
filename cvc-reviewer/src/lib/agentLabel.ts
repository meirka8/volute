/**
 * Which agent a conversation came from (#187), read from its id.
 *
 * `refs/cvc/main` stays on FORMAT 5, so there is no agent field on the wire. The conversations of the
 * agents added in 0.8.0 have ids `<harness>:<agent's own id>`, with the harness one of the names below,
 * in lowercase as the writer spells it. Claude Code's own ids are unprefixed UUIDs (and its subagents
 * `<session>:agent-<id>`), so an unprefixed id says nothing here.
 *
 * The label is the publisher's claim, not proof: anyone who can write to the ref can write an id with
 * any prefix. Wherever it is drawn with a title, the title says so (`agentLabelTitle`).
 *
 * A `remote:<fingerprint>:` namespace is local to a machine and never on the wire, so it is not handled.
 */

const HARNESSES = {
  codex: 'Codex',
  opencode: 'OpenCode',
  cursor: 'Cursor',
  antigravity: 'Antigravity',
  'claude-code': 'Claude Code',
} as const;

export type HarnessName = keyof typeof HARNESSES;

export interface ConversationAgent {
  readonly harness: HarnessName;
  /** The name to show: "Codex". */
  readonly label: string;
  /** The conversation's id without the prefix: the agent's own id for it. */
  readonly rest: string;
}

/** What the title of an agent label says about it. */
export const AGENT_LABEL_NOTE = 'as recorded by the publisher';

/** The agent of a conversation whose id starts with a reserved prefix, else `null` (an unprefixed id says nothing). */
export function agentOfConversation(conversationId: unknown): ConversationAgent | null {
  if (typeof conversationId !== 'string') return null;
  const colon = conversationId.indexOf(':');
  if (colon <= 0) return null;
  const prefix = conversationId.slice(0, colon);
  if (!Object.prototype.hasOwnProperty.call(HARNESSES, prefix)) return null;
  const harness = prefix as HarnessName;
  return { harness, label: HARNESSES[harness], rest: conversationId.slice(colon + 1) };
}

/** The title for an agent label: "Agent: Codex, as recorded by the publisher". */
export function agentLabelTitle(agent: ConversationAgent): string {
  return `Agent: ${agent.label}, ${AGENT_LABEL_NOTE}`;
}
