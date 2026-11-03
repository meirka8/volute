/**
 * A long, typical timeline for the tests of search and filters (issue #118), and for the
 * local visual harness (`preview.local/`). Everything is made up and deterministic: paths
 * are `/home/dev/work/shop/...` placeholders, the commits are made-up hashes, and the same
 * `count` always gives the same steps.
 *
 * It is shaped like a Claude Code capture of a few hours of work on one pull request:
 *
 * - one session, steps that start with a person's prompt and steps that react to the tool
 *   results of the one before, each with a response, reasoning, and one to three tool calls
 *   (Bash, Edit, Read, Grep, TodoWrite, Write);
 * - two subagents of that session, whose conversation ids are `<session>:agent-<id>`, each
 *   starting with the task its parent sent;
 * - now and then a step from the MCP server (the Thought / Action / Observation layout), in
 *   a conversation of its own;
 * - every step linked to one of the pull request's commits, some also by a temporal, a
 *   verified or a publisher-asserted link.
 *
 * One file, `src/payments/refund_handler.ts`, is touched by every 97th step starting at the
 * 18th, which is how a test asks "which steps touched this file?" and knows the answer.
 */
import type { CommitInfo } from '../lib/timelineSearch';
import type { ArtifactLinkData, InteractionNode, ToolExecution } from '../types/cvc';
import { generatedLink, makeNode, wireToolCall } from './fixtures';

const BASE = Date.parse('2026-03-02T09:00:00.000Z');
const MINUTE = 60_000;
const hex12 = (n: number) => n.toString(16).padStart(12, '0');

export const FIXTURE_SESSION = '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70';
export const FIXTURE_SUBAGENTS = [`${FIXTURE_SESSION}:agent-a0bd78e0`, `${FIXTURE_SESSION}:agent-c4f19b73`] as const;
export const FIXTURE_MCP_CONVERSATION = '9a3e7c10-2b5d-4f68-8c1a-d4e5f6a7b8c9';

/** The file one in ninety-seven steps touches, and the name a person would type to find it. */
export const FIXTURE_PATH = 'src/payments/refund_handler.ts';
export const FIXTURE_FILE_NAME = 'refund_handler.ts';

const ROOT = '/home/dev/work/shop';

const AREAS = ['cart', 'catalog', 'checkout', 'search', 'shipping', 'tax', 'users', 'inventory'];
const MODULES = ['index', 'service', 'store', 'client', 'utils', 'types'];
const FILES = AREAS.flatMap((area) => MODULES.map((module) => `src/${area}/${module}.ts`));
const TOPICS = ['retry limit', 'pagination cursor', 'date parsing', 'price rounding', 'cache expiry', 'address validation', 'stock count', 'session timeout'];
const VERBS = ['Fix', 'Add', 'Tighten', 'Refactor', 'Document', 'Cover'];

/** The made-up hash of the pull request's `k`th commit (oldest is 0). The first seven characters tell the commits apart. */
export const fixtureCommit = (k: number): string => `${k.toString(16).padStart(2, '0')}c0ffee${'a1b2c3d4e5f60718293a4b5c6d7e8f90'}`;

/** The commits of the pull request, oldest first, as GitHub lists them. */
export function fixtureCommits(count = 5): CommitInfo[] {
  const subjects = ['feat: add the refund handler', 'fix: round prices half to even', 'test: cover the cart cursor', 'docs: describe the retry policy', 'chore: tidy the shipping client'];
  return Array.from({ length: count }, (_, k) => ({ sha: fixtureCommit(k), subject: subjects[k % subjects.length] }));
}

const isSpecial = (i: number) => i % 97 === 17;
const fileOf = (i: number) => (isSpecial(i) ? FIXTURE_PATH : FILES[i % FILES.length]);
const absolute = (file: string) => `${ROOT}/${file}`;

type Kind = 'prompt' | 'results' | 'reads' | 'plain';

function kindOf(i: number): Kind {
  if (isSpecial(i)) return 'prompt';
  return (['prompt', 'results', 'reads', 'plain'] as const)[i % 4];
}

function conversationOf(i: number): string {
  if (isSpecial(i)) return FIXTURE_SESSION;
  if (i % 25 === 24) return FIXTURE_MCP_CONVERSATION;
  if (i % 10 === 4 || i % 10 === 5) return FIXTURE_SUBAGENTS[Math.floor(i / 10) % 2];
  return FIXTURE_SESSION;
}

const topicOf = (i: number) => TOPICS[i % TOPICS.length];
const baseName = (file: string) => file.slice(file.lastIndexOf('/') + 1);

const EDIT_BEFORE = (topic: string) => `export function apply(input: Input): Output {\n  // TODO: ${topic}\n  const value = input.value;\n  return { value };\n}`;
const EDIT_AFTER = (topic: string, i: number) =>
  `export function apply(input: Input): Output {\n  // ${topic}, limit ${i % 7 + 2}\n  const value = clamp(input.value, 0, ${i % 7 + 2});\n  return { value, clamped: value !== input.value };\n}`;

function toolsOf(i: number, kind: Kind, subagent: boolean): ToolExecution[] {
  const file = fileOf(i);
  const topic = topicOf(i);
  if (subagent) {
    return [
      wireToolCall('Read', { file_path: absolute(file), limit: 80, offset: 1 }),
      wireToolCall('Grep', { output_mode: 'content', path: `${ROOT}/src`, pattern: topic.split(' ')[0] }),
    ];
  }
  switch (kind) {
    case 'prompt':
      return [
        wireToolCall('Bash', { command: `npm test -- --run ${baseName(file)} 2>&1 | tail -20`, description: 'Run the tests for the file', timeout: 120000 }),
        wireToolCall('Edit', { file_path: absolute(file), new_string: EDIT_AFTER(topic, i), old_string: EDIT_BEFORE(topic) }),
      ];
    case 'results':
      return [
        wireToolCall('Read', { file_path: absolute(file), limit: 120, offset: 1 }),
        wireToolCall('Grep', { output_mode: 'content', path: `${ROOT}/src`, pattern: topic.split(' ')[0] }),
        ...(i % 8 === 1 ? [wireToolCall('TodoWrite', { todos: [{ activeForm: 'Writing tests', content: 'Write tests', status: 'in_progress' }] })] : []),
      ];
    case 'reads':
      return [wireToolCall('Write', { content: `// ${topic}\nexport const LIMIT = ${i % 9 + 1};\n`, file_path: absolute(`src/generated/limit-${i}.ts`) })];
    case 'plain':
      return [];
  }
}

function stimulusOf(i: number, kind: Kind, subagent: boolean, firstOfConversation: boolean): { author: InteractionNode['author']; prompt: string } {
  const file = fileOf(i);
  const topic = topicOf(i);
  if (subagent && firstOfConversation) {
    return { author: 'human', prompt: `Survey how ${topic} is handled across src/ and report each file.\nBe brief.` };
  }
  if (kind === 'prompt' && !subagent) {
    return { author: 'human', prompt: `${VERBS[i % VERBS.length]} the ${topic} in ${file} and keep the existing tests green.` };
  }
  const previous = fileOf(i - 1);
  const output = Array.from({ length: 6 }, (_, line) => `  ${previous}:${line * 7 + 3}: const value${line} = ${topic.split(' ')[0]}(${line});`).join('\n');
  return {
    author: 'system',
    prompt: `[tool_result Bash] > shop@1.0.0 test\n> vitest --run\n\n ✓ ${baseName(previous)} (${i % 9 + 2} tests)\n\n Test Files  1 passed (1)\n\n[tool_result Edit] The file ${absolute(previous)} has been updated. Here's the result of running \`cat -n\` on a snippet:\n${output}`,
  };
}

/** The made-up steps of a long session, oldest first. */
export interface SearchFixture {
  readonly nodes: InteractionNode[];
  /** The pull request's commits, oldest first. */
  readonly commits: CommitInfo[];
  /** Ids of the steps that touched `FIXTURE_PATH`, newest first. */
  readonly fileSteps: string[];
  /** Ids of the steps of each subagent, newest first, by conversation id. */
  readonly subagentSteps: Record<string, string[]>;
  /** Ids of the steps that came from the MCP server, newest first. */
  readonly mcpSteps: string[];
}

export function searchFixture(count = 300, commitCount = 5): SearchFixture {
  const commits = fixtureCommits(commitCount);
  const firstOf = new Map<string, number>();
  const nodes: InteractionNode[] = [];
  const fileSteps: string[] = [];
  const subagentSteps: Record<string, string[]> = { [FIXTURE_SUBAGENTS[0]]: [], [FIXTURE_SUBAGENTS[1]]: [] };
  const mcpSteps: string[] = [];

  for (let i = 0; i < count; i++) {
    const conversation = conversationOf(i);
    const subagent = conversation === FIXTURE_SUBAGENTS[0] || conversation === FIXTURE_SUBAGENTS[1];
    const mcp = conversation === FIXTURE_MCP_CONVERSATION;
    if (!firstOf.has(conversation)) firstOf.set(conversation, i);
    const timestamp = BASE + i * MINUTE;
    const commit = commits[Math.min(commitCount - 1, Math.floor((i * commitCount) / count))].sha;
    const file = fileOf(i);

    if (mcp) {
      const id = `3b9f0f1e-6a4c-4d6e-9b1a-${hex12(i)}`;
      nodes.push(
        makeNode({
          id,
          conversation_id: conversation,
          author: 'agent',
          timestamp,
          model_name: 'agent',
          user_prompt: `Review the ${topicOf(i)} change before it is committed.`,
          model_cot: `The ${topicOf(i)} change touches ${file}. Edge cases: empty input, a limit of zero, and concurrent callers.`,
          model_response: `Reviewed ${baseName(file)}: no blocking issues, two follow-ups noted.`,
          artifact_links: [generatedLink(id, commit)],
        }),
      );
      mcpSteps.unshift(id);
      continue;
    }

    const kind = kindOf(i);
    const { author, prompt } = stimulusOf(i, kind, subagent, firstOf.get(conversation) === i);
    const id = `1f0e5c3a-9d2b-8c4e-a1b7-${hex12(i)}`;
    const tools = toolsOf(i, kind, subagent);
    const links: ArtifactLinkData[] = [generatedLink(id, commit)];
    if (i % 17 === 5) links.push({ interaction_id: id, git_commit_hash: commits[Math.max(0, Math.floor((i * commitCount) / count) - 1)].sha, link_type: 'temporal' });
    if (i % 41 === 7) links.push({ interaction_id: id, git_commit_hash: commit, link_type: 'verified' });
    if (i % 53 === 11) {
      links.push({
        interaction_id: id,
        git_commit_hash: commit,
        link_type: 'generated',
        derivation_event: {
          event_id: i.toString(16).padStart(64, 'f'),
          interaction_id: id,
          target_commit: commit,
          relation: 'generated',
          evidence: { version: 1, kind: 'remote_assertion' },
          origin: 'remote_assertion',
          source_event_ids: [],
        },
      });
    }
    nodes.push(
      makeNode({
        id,
        conversation_id: conversation,
        author,
        timestamp,
        model_name: 'claude-opus-4-5-20251101',
        user_prompt: prompt,
        model_cot: kind === 'plain' || subagent ? null : `The ${topicOf(i)} is handled in ${file}. A cap with a clear failure mode is safer than an unbounded loop, and the existing tests only cover the happy path, so the next step is to read how it is called before changing it.`,
        model_response: `${kind === 'plain' ? 'All checks pass.' : `Working on the ${topicOf(i)} in ${baseName(file)}.`} Next I will look at the callers.`,
        context_items: tools.length > 0 && !subagent && kind !== 'reads' ? [{ file_path: absolute(file) }] : [],
        tool_executions: tools,
        artifact_links: links,
      }),
    );
    if (isSpecial(i)) fileSteps.unshift(id);
    if (subagent) subagentSteps[conversation].unshift(id);
  }
  return { nodes, commits, fileSteps, subagentSteps, mcpSteps };
}
