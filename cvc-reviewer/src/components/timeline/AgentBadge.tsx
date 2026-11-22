import { agentLabelTitle, type ConversationAgent } from '../../lib/agentLabel';
import { BADGE_NEUTRAL } from './styles';

/**
 * Which agent a conversation came from, read from the prefix of its id (see `agentLabel.ts`). It is the
 * publisher's claim and not proof, which the title says. Nothing is drawn for an unprefixed id.
 */
export function AgentBadge({ agent }: { agent: ConversationAgent | null }) {
  if (agent === null) return null;
  return (
    <>
      <span className={BADGE_NEUTRAL} title={agentLabelTitle(agent)} data-agent={agent.harness}>
        {agent.label}
      </span>{' '}
    </>
  );
}
