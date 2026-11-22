import { describe, expect, it } from 'vitest';
import { agentLabelTitle, agentOfConversation } from './agentLabel';

describe('agentOfConversation', () => {
  it.each([
    ['codex:019a-thread', 'Codex', '019a-thread'],
    ['opencode:ses_1', 'OpenCode', 'ses_1'],
    ['cursor:abc', 'Cursor', 'abc'],
    ['antigravity:x:y', 'Antigravity', 'x:y'],
  ])('%s is %s', (id, label, rest) => {
    const agent = agentOfConversation(id);
    expect(agent?.label).toBe(label);
    expect(agent?.rest).toBe(rest);
  });

  it('says nothing for unprefixed ids: Claude Code sessions and subagents, the editor, the MCP server', () => {
    for (const id of ['4f2a9c1e-0000-4000-8000-000000000001', '4f2a9c1e-0000-4000-8000-000000000001:agent-a1b2', 'vscode-1', '', 'mcp']) {
      expect(agentOfConversation(id)).toBeNull();
    }
  });

  it('says nothing for the claude-code: prefix, which cvc reserves but never writes', () => {
    for (const id of ['claude-code:4f2a9c1e-0000-4000-8000-000000000001', 'claude-code:x', 'claude-code:']) {
      expect(agentOfConversation(id)).toBeNull();
    }
  });

  it('says nothing for a bare prefix with nothing after it', () => {
    for (const id of ['codex:', 'opencode:', 'cursor:', 'antigravity:']) {
      expect(agentOfConversation(id)).toBeNull();
    }
    // One character after it is enough, as in cvc-core.
    expect(agentOfConversation('codex::')?.rest).toBe(':');
  });

  it('matches the lowercase prefix exactly', () => {
    for (const id of ['Codex:x', 'CODEX:x', 'codex', 'codex-x:y', ':x', ' codex:x', 'codexx:y', 'cursor :x', 'toString:x', '__proto__:x', 'constructor:x']) {
      expect(agentOfConversation(id)).toBeNull();
    }
  });

  it('handles values that are not strings', () => {
    for (const id of [undefined, null, 3, {}]) expect(agentOfConversation(id)).toBeNull();
  });

  it("titles the label as the publisher's claim", () => {
    expect(agentLabelTitle(agentOfConversation('codex:x')!)).toBe('Agent: Codex, as recorded by the publisher');
  });
});
