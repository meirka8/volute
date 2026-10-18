/**
 * The Claude Code transcript layout (issue #105): a step reads as stimulus →
 * reasoning → response → tool calls, with commands and diffs shown as such.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TimelineNode } from './TimelineNode';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import {
  agentStep,
  CHECK_SCRIPT_COMMAND,
  compactionStep,
  crlfStep,
  embeddedPromptStep,
  failingStep,
  imagePromptStep,
  interruptedSyntheticStep,
  longTokensStep,
  makeNode,
  mcpToolStep,
  multiEditStep,
  noStimulusStep,
  nullArgsStep,
  promptStep,
  rawToolCall,
  rejectedToolStep,
  serverToolStep,
  toolCall,
  truncatedArgsStep,
} from '../../test/fixtures';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

function renderNode(node: InteractionNode, onSelect: () => void = () => {}) {
  return render(<TimelineNode interaction={node} onSelect={onSelect} />);
}

/** The card of the tool call with this heading. */
function toolCard(title: string): HTMLElement {
  const card = screen.getByText(title).closest('[data-status]');
  if (!(card instanceof HTMLElement)) throw new Error(`No tool card titled ${title}`);
  return card;
}

const FORBIDDEN_SPEAKERS = ['User', 'System', 'AI Agent', 'External'];
const FORBIDDEN_SIGNALS = ['Green check', 'Review required', 'Critical alert'];
const TRIAD_LABELS = ['Thought', 'Action', 'Observation', 'Decision Factors'];

describe('transcript header', () => {
  it.each([
    ['a prompt step', promptStep, 'Prompt'],
    ['a step reacting to tool results', failingStep, 'Tool results'],
    ['a step reacting to a compaction summary', compactionStep, 'Compaction summary'],
  ])('shows "AI step", the model and the stimulus origin for %s', (_label, node, origin) => {
    renderNode(node);
    expect(screen.getByText('AI step')).toBeInTheDocument();
    expect(screen.getByText(node.model_name!)).toBeInTheDocument();
    // The origin appears as the badge (and, for some steps, again as a pane label).
    expect(screen.getAllByText(origin).length).toBeGreaterThan(0);
    expect(screen.getByText(/^<2026-03-02T09:\d\d:00\.000Z>$/)).toBeInTheDocument();
  });

  it.each([
    ['human prompt', promptStep],
    ['tool results', failingStep],
    ['compaction', compactionStep],
    ['no stimulus', noStimulusStep],
    ['embedded prompt', embeddedPromptStep],
    ['MCP tool', mcpToolStep],
    ['rejected tool call', rejectedToolStep],
    ['server-side tool', serverToolStep],
    ['no-argument tool', nullArgsStep],
    ['Windows line endings', crlfStep],
    ['image placeholders', imagePromptStep],
    ['synthetic interruption', interruptedSyntheticStep],
  ])('never labels a %s step as a speaker, a verdict or a triad pane', (_label, node) => {
    renderNode(node);
    for (const text of [...FORBIDDEN_SPEAKERS, ...FORBIDDEN_SIGNALS, ...TRIAD_LABELS]) {
      expect(screen.queryByText(text)).not.toBeInTheDocument();
    }
  });

  it('reports failed tool calls as a count, not a verdict', () => {
    renderNode(failingStep);
    expect(screen.getByText('1 tool call failed')).toBeInTheDocument();
  });

  it('pluralises the count and stays silent when nothing failed', () => {
    renderNode(
      makeNode({
        ...promptStep,
        tool_executions: [toolCall('Bash', { command: 'a' }, 'failure'), toolCall('Bash', { command: 'b' }, 'failure')],
      }),
    );
    expect(screen.getByText('2 tool calls failed')).toBeInTheDocument();
    cleanup();

    renderNode(promptStep);
    expect(screen.queryByText(/tool calls? failed/)).not.toBeInTheDocument();
  });

  it('shows the model name verbatim, including the synthetic placeholder', () => {
    renderNode(noStimulusStep);
    expect(screen.getByText('<synthetic>')).toBeInTheDocument();
  });

  it('omits the model line when there is no model name', () => {
    const { container } = renderNode(makeNode({ ...promptStep, model_name: null }));
    expect(container.querySelector('.uppercase.tracking-\\[0\\.16em\\]')).toBeNull();
  });
});

describe('stimulus', () => {
  it('shows a human prompt as prose under a Prompt label', () => {
    renderNode(promptStep);
    const prompt = screen.getByText(promptStep.user_prompt!);
    expect(prompt).toHaveClass('font-serif');
    expect(screen.getAllByText('Prompt')).toHaveLength(2); // badge and pane label
  });

  it('folds tool results behind a one-line summary, with nothing labelled Thought', () => {
    renderNode(failingStep);
    expect(screen.queryByText('Thought')).not.toBeInTheDocument();

    const toggle = screen.getByRole('button', { name: 'Tool results · Bash, Read · 1 error' });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    // The output is not on the page until asked for.
    expect(screen.queryByText(/error TS2322/)).not.toBeInTheDocument();

    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText(/error TS2322/)).toBeInTheDocument();
  });

  it('shows each expanded tool result with its tool name and an error badge where it failed', () => {
    renderNode(failingStep);
    fireEvent.click(screen.getByRole('button', { name: /^Tool results/ }));

    const stimulus = screen.getByText(/error TS2322/).closest('.rr-thought') as HTMLElement;
    const sections = within(stimulus);
    expect(sections.getByText('Bash')).toBeInTheDocument();
    expect(sections.getByText('Read')).toBeInTheDocument();
    expect(sections.getAllByText('error')).toHaveLength(1);
  });

  it('collapses a long tool result to ten lines with "Show all N lines"', () => {
    renderNode(failingStep);
    fireEvent.click(screen.getByRole('button', { name: /^Tool results/ }));

    expect(screen.queryByText(/test line 30/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Show all 40 lines' }));
    expect(screen.getByText(/test line 30/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Show less' }));
    expect(screen.queryByText(/test line 30/)).not.toBeInTheDocument();
  });

  it('shows an empty tool output as such', () => {
    renderNode(makeNode({ ...promptStep, author: 'system', user_prompt: '[tool_result Bash] ', tool_executions: [] }));
    fireEvent.click(screen.getByRole('button', { name: 'Tool results · Bash' }));
    expect(screen.getByText('No output.')).toBeInTheDocument();
  });

  it('keeps tool results open next to a human prompt, since human text can follow them', () => {
    renderNode(
      makeNode({
        ...promptStep,
        user_prompt: 'Please rerun the tests.\n\n[tool_result Bash error] 2 failed',
        tool_executions: [],
      }),
    );
    expect(screen.getByText('Please rerun the tests.')).toBeInTheDocument();
    const toggle = screen.getByRole('button', { name: 'Tool results · Bash · 1 error' });
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('2 failed')).toBeInTheDocument();

    // It can still be folded away.
    fireEvent.click(toggle);
    expect(screen.queryByText('2 failed')).not.toBeInTheDocument();
  });

  it('folds tool results only when the stimulus is a system one', () => {
    const results = '[tool_result Bash] first\n\n[tool_result Read error] second';
    renderNode(makeNode({ ...promptStep, author: 'system', user_prompt: results, tool_executions: [] }));
    expect(screen.getByRole('button', { name: 'Tool results · Bash, Read · 1 error' })).toHaveAttribute(
      'aria-expanded',
      'false',
    );
    expect(screen.queryByText('first')).not.toBeInTheDocument();
    cleanup();

    renderNode(makeNode({ ...promptStep, author: 'human', user_prompt: results, tool_executions: [] }));
    expect(screen.getByRole('button', { name: 'Tool results · Bash, Read · 1 error' })).toHaveAttribute(
      'aria-expanded',
      'true',
    );
    expect(screen.getByText('first')).toBeInTheDocument();
    expect(screen.getByText('second')).toBeInTheDocument();
    cleanup();

    // Only a system stimulus is folded: an author we know nothing about is not.
    renderNode(makeNode({ ...promptStep, author: 'external', user_prompt: results, tool_executions: [] }));
    expect(screen.getByRole('button', { name: 'Tool results · Bash, Read · 1 error' })).toHaveAttribute(
      'aria-expanded',
      'true',
    );
  });

  it('still clamps a long tool result that starts open on a human step', () => {
    const output = Array.from({ length: 30 }, (_, i) => `output line ${i + 1}`).join('\n');
    renderNode(makeNode({ ...promptStep, author: 'human', user_prompt: `[tool_result Bash] ${output}`, tool_executions: [] }));
    expect(screen.queryByText(/output line 20/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Show all 30 lines' }));
    expect(screen.getByText(/output line 20/)).toBeInTheDocument();
  });

  it("shows a user's new instruction after a rejected tool call as a prompt, not as tool output", () => {
    renderNode(rejectedToolStep);
    const stimulus = screen.getByText(/The user doesn't want to proceed/).closest('.rr-thought') as HTMLElement;
    expect(within(stimulus).getByText('error')).toBeInTheDocument(); // the tool result's badge

    const instruction = screen.getByText(/Use the existing check script instead of writing a new one\./);
    expect(instruction).toHaveClass('font-serif'); // prose, like any prompt
    expect(instruction.textContent).toBe(
      '[Request interrupted by user for tool use]\n\nUse the existing check script instead of writing a new one.',
    );
    // The instruction is separated from the tool output, so nothing needs explaining.
    expect(screen.queryByText(/the capture does not mark where it starts/)).not.toBeInTheDocument();
    expect(screen.getAllByText('Prompt')).toHaveLength(2);
  });

  it('labels a compaction summary and injected context separately', () => {
    renderNode(compactionStep);
    expect(screen.getAllByText('Compaction summary')).toHaveLength(2); // badge and pane label
    expect(screen.getByText('Context')).toBeInTheDocument();
    expect(screen.getByText(/The user asked for a retry limit/)).toBeInTheDocument();
    expect(screen.getByText('Continue from where the conversation left off.')).toBeInTheDocument();
  });

  it('says so when no stimulus was recorded: the badge keeps its name, the pane explains', () => {
    renderNode(noStimulusStep);
    expect(screen.getAllByText('No stimulus recorded')).toHaveLength(1); // the badge
    expect(screen.getByText('Nothing was recorded for what this step reacted to.')).toBeInTheDocument();
  });

  it('keeps a human message that cannot be separated visible, and says so', () => {
    renderNode(embeddedPromptStep);
    expect(screen.getAllByText('Prompt')).toHaveLength(2);
    expect(
      screen.getByText('The prompt is inside the text below; the capture does not mark where it starts.'),
    ).toBeInTheDocument();
    // The text is on the page without any click, because the prompt is inside it.
    expect(screen.getByText(/Please rename the config loader to readConfig\./)).toBeInTheDocument();
  });

  it('shows human text that follows a tool result, which cannot be separated from it', () => {
    renderNode(
      makeNode({
        ...promptStep,
        user_prompt: '[tool_result Bash error] stopped\n\nActually, use the other script.',
        tool_executions: [],
      }),
    );
    expect(screen.getByRole('button', { name: 'Tool results · Bash · 1 error' })).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText(/Actually, use the other script\./)).toBeInTheDocument();
    expect(screen.getByText(/The prompt is inside the text below/)).toBeInTheDocument();
  });
});

describe('reasoning and response', () => {
  it('shows reasoning and the response in their own panes', () => {
    renderNode(promptStep);
    expect(screen.getByText('Reasoning')).toBeInTheDocument();
    expect(screen.getByText(promptStep.model_cot!)).toBeInTheDocument();
    expect(screen.getByText('Response')).toBeInTheDocument();
    expect(screen.getByText(promptStep.model_response!)).toBeInTheDocument();
  });

  it('omits the Reasoning pane when there is none, and never fills it with the prompt', () => {
    renderNode(failingStep);
    expect(screen.queryByText('Reasoning')).not.toBeInTheDocument();
    cleanup();

    renderNode(makeNode({ ...promptStep, model_cot: '  \n ' }));
    expect(screen.queryByText('Reasoning')).not.toBeInTheDocument();
    // The prompt appears once: in the stimulus pane.
    expect(screen.getAllByText(promptStep.user_prompt!)).toHaveLength(1);
  });

  it('omits the Response pane when the step only called tools', () => {
    renderNode(mcpToolStep);
    expect(screen.queryByText('Response')).not.toBeInTheDocument();
    expect(screen.getByText('Tool calls')).toBeInTheDocument();
  });

  it('renders bracketed placeholders for unrecorded blocks as plain text', () => {
    renderNode(makeNode({ ...promptStep, model_response: 'Searching. [server_tool_use]' }));
    expect(screen.getByText('Searching. [server_tool_use]')).toBeInTheDocument();
  });

  it('clamps long reasoning and offers Show more', () => {
    const cot = Array.from({ length: 8 }, (_, i) => `Reasoning line ${i + 1}`).join('\n');
    renderNode(makeNode({ ...promptStep, model_cot: cot }));
    const reasoning = screen.getByText(/Reasoning line 1/);
    expect(reasoning).toHaveClass('line-clamp-4');
    fireEvent.click(screen.getByRole('button', { name: 'Show more' }));
    expect(reasoning).not.toHaveClass('line-clamp-4');
    expect(screen.getByRole('button', { name: 'Show less' })).toHaveAttribute('aria-expanded', 'true');
  });

  it('also offers Show more when the browser reports the text is cut off', async () => {
    vi.spyOn(Element.prototype, 'scrollHeight', 'get').mockReturnValue(300);
    vi.spyOn(Element.prototype, 'clientHeight', 'get').mockReturnValue(80);
    renderNode(promptStep);
    await waitFor(() => expect(screen.getAllByRole('button', { name: 'Show more' }).length).toBeGreaterThan(0));
  });
});

describe('tool calls', () => {
  it('shows a Bash heredoc with real line breaks, not escaped JSON', () => {
    renderNode(promptStep);
    const card = toolCard('Bash');
    const code = card.querySelector('pre')!;
    expect(code.textContent).toBe(CHECK_SCRIPT_COMMAND);
    expect(code.textContent).toContain('EOF\nchmod +x');
    expect(code.textContent).not.toContain('\\n');
    expect(within(card).getByText('Create the check script')).toBeInTheDocument();
    expect(within(card).getByText('timeout 120s')).toBeInTheDocument();
    // The wire value really is one escaped JSON string.
    expect(promptStep.tool_executions[0].arguments).toContain('\\n');
  });

  it('shows an Edit as a diff with removed and added lines', () => {
    const { container } = renderNode(promptStep);
    const card = toolCard('Edit');
    expect(within(card).getByText('/repo/src/net/fetch.ts')).toBeInTheDocument();

    const removed = Array.from(card.querySelectorAll('del')).map((el) => el.textContent);
    const added = Array.from(card.querySelectorAll('ins')).map((el) => el.textContent);
    expect(removed).toEqual(['-  for (;;) {', '-      await sleep(100);']);
    expect(added).toEqual([
      '+const MAX_RETRIES = 3;',
      '+',
      '+  for (let attempt = 0; ; attempt++) {',
      '+      if (attempt === MAX_RETRIES - 1) throw err;',
      '+      await sleep(100 * 2 ** attempt);',
    ]);
    // Unchanged lines are context, not changes.
    expect(within(card).getByText('export async function fetchWithRetry(url: string): Promise<Response> {')).toBeInTheDocument();
    // The old and new text are never dumped as JSON.
    expect(container.textContent).not.toContain('old_string');
  });

  it('shows a MultiEdit as a numbered series of diffs with per-edit options', () => {
    renderNode(multiEditStep);
    const card = toolCard('MultiEdit');
    expect(within(card).getByText('/repo/src/net/fetch.test.ts')).toBeInTheDocument();
    expect(within(card).getByText('Edit 1 of 2')).toBeInTheDocument();
    expect(within(card).getByText('Edit 2 of 2')).toBeInTheDocument();
    expect(within(card).getByText('replace all occurrences')).toBeInTheDocument();
    expect(card.querySelectorAll('del').length).toBe(2);
    expect(card.querySelectorAll('ins').length).toBe(2);
  });

  it('shows a Write as the new file content under its path', () => {
    renderNode(multiEditStep);
    const card = toolCard('Write');
    expect(within(card).getByText('/repo/docs/RETRY.md')).toBeInTheDocument();
    expect(card.querySelector('pre')!.textContent).toContain('# Retry policy\n\nRequests are retried up to three times.');
  });

  it('shows a failed tool with a failure badge, and the others as successful', () => {
    renderNode(failingStep);
    expect(within(toolCard('Grep')).getByText('success')).toBeInTheDocument();
    expect(within(toolCard('Read')).getByText('success')).toBeInTheDocument();

    // "Bash" is also a name in the tool-results summary, so look at the cards only.
    const failed = Array.from(document.querySelectorAll('[data-status="failure"]'));
    expect(failed).toHaveLength(1);
    expect(within(failed[0] as HTMLElement).getByText('Bash')).toBeInTheDocument();
    expect(within(failed[0] as HTMLElement).getByText('failed')).toBeInTheDocument();
  });

  it('shows Grep, Read and Glob as a subject with options', () => {
    renderNode(failingStep);
    const grep = toolCard('Grep');
    expect(within(grep).getByText('fetchWithRetry')).toBeInTheDocument();
    expect(within(grep).getByText('in /repo/src')).toBeInTheDocument();
    expect(within(grep).getByText('output content')).toBeInTheDocument();
    expect(within(grep).getByText('line numbers')).toBeInTheDocument();

    const read = toolCard('Read');
    expect(within(read).getByText('/repo/src/net/fetch.test.ts')).toBeInTheDocument();
    expect(within(read).getByText('lines 1–40')).toBeInTheDocument();
  });

  it('explains arguments that exceeded the capture limit', () => {
    renderNode(truncatedArgsStep);
    expect(
      screen.getByText('Arguments not recorded: 131072 bytes exceeded the capture limit.'),
    ).toBeInTheDocument();
    // None of the sentinel's JSON leaks onto the page.
    expect(screen.queryByText(/cvc_truncated/)).not.toBeInTheDocument();
  });

  it('shows an MCP tool as server · tool with its fields as rows', () => {
    renderNode(mcpToolStep);
    const card = toolCard('example_server · create_item');
    expect(within(card).getByText('title')).toBeInTheDocument();
    expect(within(card).getByText('Cap fetch retries')).toBeInTheDocument();
    expect(within(card).getByText('priority')).toBeInTheDocument();
    // Nested values are pretty-printed, one level per two spaces.
    expect(card.textContent).toContain('[\n  "bug",\n  "network"\n]');
    expect(card.textContent).toContain('{\n  "key": "EX",\n  "name": "Example"\n}');
  });

  it('shows sub-agent, web and checklist tools, with the sub-agent prompt hidden until asked for', () => {
    const { container } = renderNode(agentStep);

    const agent = toolCard('Agent');
    expect(within(agent).getByText('Survey retry helpers')).toBeInTheDocument();
    expect(within(agent).getByText('subagent Explore')).toBeInTheDocument();
    expect(within(agent).queryByText(/Search the repository/)).not.toBeInTheDocument();
    fireEvent.click(within(agent).getByRole('button', { name: 'Show subagent prompt' }));
    expect(within(agent).getByText(/Search the repository/)).toBeInTheDocument();
    fireEvent.click(within(agent).getByRole('button', { name: 'Hide subagent prompt' }));
    expect(within(agent).queryByText(/Search the repository/)).not.toBeInTheDocument();

    // A URL is text the model chose, never a link.
    const fetchCard = toolCard('WebFetch');
    expect(within(fetchCard).getByText('https://example.com/docs/retries?lang=en')).toBeInTheDocument();
    expect(container.querySelectorAll('a')).toHaveLength(0);

    expect(within(toolCard('WebSearch')).getByText('exponential backoff with jitter')).toBeInTheDocument();
    expect(within(toolCard('Glob')).getByText('src/**/*.test.ts')).toBeInTheDocument();

    const todos = toolCard('TodoWrite');
    expect(within(todos).getByText('Cap retries')).toHaveClass('line-through');
    expect(within(todos).getByText('Document the policy')).not.toHaveClass('line-through');
    expect(todos.textContent).toContain('completed: ');
    expect(todos.textContent).toContain('in progress: '); // spoken as words, not as an identifier
    expect(todos.textContent).not.toContain('in_progress');
    // The present-tense wording is shown beside the item when it adds something.
    expect(within(todos).getByText('Documenting the policy')).toHaveClass('text-muted');
  });

  it('falls back to rows for a known tool with unexpected fields, hiding nothing', () => {
    renderNode(
      makeNode({ ...promptStep, tool_executions: [toolCall('Bash', { cmd: 'ls', shell: 'fish' })] }),
    );
    const card = toolCard('Bash');
    expect(within(card).getByText('cmd')).toBeInTheDocument();
    expect(within(card).getByText('fish')).toBeInTheDocument();
  });

  it('shows arguments that are not JSON as they were recorded', () => {
    renderNode(makeNode({ ...promptStep, tool_executions: [rawToolCall('Bash', 'ls -la /repo')] }));
    expect(within(toolCard('Bash')).getByText('ls -la /repo')).toBeInTheDocument();
    cleanup();

    renderNode(makeNode({ ...promptStep, tool_executions: [rawToolCall('Bash', '')] }));
    expect(within(toolCard('Bash')).getByText('No arguments recorded.')).toBeInTheDocument();
  });

  it('keeps the order the tools were called in', () => {
    renderNode(failingStep);
    const titles = Array.from(document.querySelectorAll('[data-status]')).map(
      (card) => card.firstElementChild!.firstElementChild!.textContent,
    );
    expect(titles).toEqual(['Grep', 'Read', 'Bash']);
  });

  it('collapses a long command to twelve lines', () => {
    const command = Array.from({ length: 30 }, (_, i) => `echo line-${i + 1}`).join('\n');
    renderNode(makeNode({ ...promptStep, tool_executions: [toolCall('Bash', { command })] }));
    const card = toolCard('Bash');
    expect(card.querySelector('pre')!.textContent).toContain('echo line-12');
    expect(card.querySelector('pre')!.textContent).not.toContain('echo line-13');

    fireEvent.click(within(card).getByRole('button', { name: 'Show all 30 lines' }));
    expect(card.querySelector('pre')!.textContent).toContain('echo line-30');
    expect(within(card).getByRole('button', { name: 'Show less' })).toHaveAttribute('aria-expanded', 'true');
  });

  it('shows a block that is only a line or two over the limit in full', () => {
    const lines = (count: number) => Array.from({ length: count }, (_, i) => `echo line-${i + 1}`).join('\n');
    renderNode(makeNode({ ...promptStep, tool_executions: [toolCall('Bash', { command: lines(14) })] }));
    expect(toolCard('Bash').querySelector('pre')!.textContent).toContain('echo line-14');
    expect(screen.queryByRole('button', { name: /Show all/ })).not.toBeInTheDocument();
    cleanup();

    renderNode(makeNode({ ...promptStep, tool_executions: [toolCall('Bash', { command: lines(15) })] }));
    expect(toolCard('Bash').querySelector('pre')!.textContent).not.toContain('echo line-13');
    expect(screen.getByRole('button', { name: 'Show all 15 lines' })).toBeInTheDocument();
  });

  it('collapses a long diff the same way', () => {
    const oldText = Array.from({ length: 40 }, (_, i) => `old ${i + 1}`).join('\n');
    const newText = Array.from({ length: 40 }, (_, i) => `new ${i + 1}`).join('\n');
    renderNode(
      makeNode({
        ...promptStep,
        tool_executions: [toolCall('Edit', { file_path: '/repo/a.ts', old_string: oldText, new_string: newText })],
      }),
    );
    const card = toolCard('Edit');
    expect(card.querySelectorAll('del').length).toBe(12);
    expect(card.querySelectorAll('ins').length).toBe(0);
    fireEvent.click(within(card).getByRole('button', { name: 'Show all 80 lines' }));
    expect(card.querySelectorAll('del').length).toBe(40);
    expect(card.querySelectorAll('ins').length).toBe(40);
  });

  it('keeps long unbroken text from widening the card', () => {
    const { container } = renderNode(longTokensStep);
    // Paths wrap anywhere; code scrolls inside its own box; wrapped output breaks long tokens.
    expect(screen.getAllByText(/very\/long\/directory\/name/)[0]).toHaveClass('break-words');
    expect(within(toolCard('Read')).getByText(/nested-directory-name/)).toHaveClass('break-all');
    expect(toolCard('Bash').querySelector('pre')).toHaveClass('overflow-x-auto', 'whitespace-pre');
    fireEvent.click(screen.getByRole('button', { name: /^Tool results/ }));
    const output = container.querySelector('.rr-thought pre')!;
    expect(output).toHaveClass('whitespace-pre-wrap', 'break-words');
  });
});

describe('server-side tools', () => {
  // cvc 0.7.0 records every server-side tool as `failure`: its result never
  // arrives as a user-side tool_result. That must not read as a failure.
  it('shows a recorded failure of a server-side tool as unknown, not as failed', () => {
    renderNode(serverToolStep);
    const card = toolCard('web_search (server-side)');
    expect(card).toHaveAttribute('data-status', 'unrecorded');
    expect(within(card).getByText('server-side · result not recorded')).toBeInTheDocument();
    expect(within(card).queryByText('failed')).not.toBeInTheDocument();
    expect(within(card).getByText('exponential backoff with jitter')).toBeInTheDocument();
    expect(within(card).getByText('query')).toBeInTheDocument();
  });

  it('counts only the genuine failures in the header', () => {
    renderNode(serverToolStep);
    expect(screen.getByText('1 tool call failed')).toBeInTheDocument();
    expect(within(toolCard('Bash')).getByText('failed')).toBeInTheDocument();
  });

  it('shows no failure chip when a server-side tool is the only "failure"', () => {
    renderNode(
      makeNode({
        ...serverToolStep,
        tool_executions: [toolCall('server:web_search', { query: 'x' }, 'failure')],
      }),
    );
    expect(screen.queryByText(/tool calls? failed/)).not.toBeInTheDocument();
  });

  it('renders a server-side tool recorded as a success normally', () => {
    renderNode(
      makeNode({
        ...serverToolStep,
        tool_executions: [toolCall('server:web_search', { query: 'x' }, 'success')],
      }),
    );
    const card = toolCard('web_search (server-side)');
    expect(card).toHaveAttribute('data-status', 'success');
    expect(within(card).getByText('success')).toBeInTheDocument();
    expect(within(card).queryByText('server-side · result not recorded')).not.toBeInTheDocument();
  });

  it('shows placeholders for unrecorded blocks in the response as plain text', () => {
    renderNode(serverToolStep);
    expect(screen.getByText(/\[web_search_tool_result\]/)).toBeInTheDocument();
  });
});

describe('arguments that were never recorded', () => {
  it('says so for a tool call without an input (JSON null)', () => {
    renderNode(nullArgsStep);
    const card = toolCard('example_server · list_items');
    expect(nullArgsStep.tool_executions[0].arguments).toBe('null');
    expect(within(card).getByText('No arguments recorded.')).toBeInTheDocument();
    expect(card.querySelector('pre')).toBeNull();
  });
});

describe('shapes seen in real transcripts', () => {
  it('shows Windows line endings and tabs without a stray carriage return', () => {
    const { container } = renderNode(crlfStep);

    const prompt = screen.getByText(/Try again/);
    expect(prompt.textContent).toBe('Try again\nsecond line\ttabbed');

    expect(toolCard('Bash').querySelector('pre')!.textContent).toBe('echo one\necho two');
    // Only the changed line differs; the carriage returns do not turn every line into a change.
    const edit = toolCard('Edit');
    expect(Array.from(edit.querySelectorAll('del')).map((el) => el.textContent)).toEqual(['-beta']);
    expect(Array.from(edit.querySelectorAll('ins')).map((el) => el.textContent)).toEqual(['+BETA']);
    expect(container.textContent).not.toContain('\r');
  });

  it('shows image and document placeholders in a prompt as plain text', () => {
    renderNode(imagePromptStep);
    const prompt = screen.getByText(/What is wrong with this screenshot/);
    expect(prompt.textContent).toBe('[image]\n\n[document]\n\nWhat is wrong with this screenshot?');
  });

  it("shows Claude Code's synthetic interruption step verbatim", () => {
    renderNode(interruptedSyntheticStep);
    expect(screen.getByText('<synthetic>')).toBeInTheDocument();
    expect(screen.getByText('[Request interrupted by user]')).toBeInTheDocument();
    expect(screen.getByText('No response requested.')).toBeInTheDocument();
    expect(screen.getAllByText('Prompt')).toHaveLength(2);
  });

  it('shows a redaction marker that sits right against the next token', () => {
    const text = 'Clone [CVC_REDACTED:v1:url_credentials:11111111-2222-4333-8444-555555555555]example.invalid/repo.git now';
    renderNode(makeNode({ ...promptStep, user_prompt: text, tool_executions: [] }));
    expect(screen.getByText(text)).toBeInTheDocument();
  });

  it('shows a credential-named key whose value was replaced by a marker', () => {
    const marker = '[CVC_REDACTED:v1:secret_assignment:22222222-3333-4444-8555-666666666666]';
    renderNode(
      makeNode({ ...promptStep, tool_executions: [rawToolCall('mcp__example_server__configure', `{"api_key":"${marker}"}`)] }),
    );
    const card = toolCard('example_server · configure');
    expect(within(card).getByText('api_key')).toBeInTheDocument();
    expect(within(card).getByText(marker)).toBeInTheDocument();
  });
});

describe('controls', () => {
  it('toggles never select the card', () => {
    const onSelect = vi.fn();
    renderNode(failingStep, onSelect);

    fireEvent.click(screen.getByRole('button', { name: /^Tool results/ })); // expand
    fireEvent.click(screen.getByRole('button', { name: 'Show all 40 lines' }));
    fireEvent.click(screen.getByRole('button', { name: 'Show less' }));
    fireEvent.click(screen.getByRole('button', { name: /^Tool results/ })); // collapse
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('prose, diff, code and prompt toggles never select the card either', () => {
    const onSelect = vi.fn();
    const longText = Array.from({ length: 9 }, (_, i) => `line ${i + 1}`).join('\n');
    renderNode(
      makeNode({
        ...promptStep,
        user_prompt: longText,
        model_cot: longText,
        model_response: longText,
        tool_executions: [
          toolCall('Bash', { command: longText.repeat(2) }),
          toolCall('Edit', { file_path: '/repo/a.ts', old_string: longText.repeat(2), new_string: 'x' }),
          toolCall('Agent', { prompt: 'do it' }),
        ],
      }),
      onSelect,
    );

    for (const button of screen.getAllByRole('button', { name: /^(Show more|Show all [\d,]+ lines|Show subagent prompt)$/ })) {
      fireEvent.click(button);
    }
    expect(onSelect).not.toHaveBeenCalled();
    // Every control now offers the way back, and says so.
    for (const button of screen.getAllByRole('button', { name: /^(Show less|Hide subagent prompt)$/ })) {
      expect(button).toHaveAttribute('aria-expanded', 'true');
    }
  });

  it('are real buttons that report their state', () => {
    renderNode(failingStep);
    const toggle = screen.getByRole('button', { name: /^Tool results/ });
    expect(toggle.tagName).toBe('BUTTON');
    expect(toggle).toHaveAttribute('type', 'button');
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
  });

  it('ignores key events from nested controls but still selects on Enter or Space on the card', () => {
    const onSelect = vi.fn();
    renderNode(failingStep, onSelect);
    const card = screen.getAllByRole('button')[0];
    const toggle = screen.getByRole('button', { name: /^Tool results/ });

    fireEvent.keyDown(toggle, { key: 'Enter' });
    fireEvent.keyDown(toggle, { key: ' ' });
    expect(onSelect).not.toHaveBeenCalled();

    fireEvent.keyDown(card, { key: 'Enter' });
    fireEvent.keyDown(card, { key: ' ' });
    expect(onSelect).toHaveBeenCalledTimes(2);
  });

  it('still selects when the card body is clicked', () => {
    const onSelect = vi.fn();
    renderNode(promptStep, onSelect);
    fireEvent.click(screen.getByText('AI step'));
    expect(onSelect).toHaveBeenCalledTimes(1);
  });
});

describe('untrusted text', () => {
  const hostile = '<img src=x onerror=alert(1)><script>alert(2)</script>';

  it('renders every wire string as text, never as markup', () => {
    const { container } = renderNode(
      makeNode({
        id: promptStep.id,
        author: 'human',
        model_name: hostile,
        user_prompt: `${hostile}\n\n[tool_result ${'Bash'} error] ${hostile}`,
        model_cot: hostile,
        model_response: hostile,
        tool_executions: [
          toolCall('Bash', { command: hostile, description: hostile }),
          toolCall('Edit', { file_path: hostile, old_string: hostile, new_string: `${hostile}!` }),
          toolCall('WebFetch', { url: 'javascript:alert(3)', prompt: hostile }),
          toolCall('mcp__srv__tool', { [hostile]: hostile }),
          toolCall(hostile, { a: hostile }),
        ],
      }),
    );
    fireEvent.click(screen.getByRole('button', { name: /^Tool results/ }));

    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('script')).toBeNull();
    expect(container.querySelector('a')).toBeNull();
    expect(container.querySelector('iframe')).toBeNull();
    expect(container.textContent).toContain(hostile);
    expect(container.textContent).toContain('javascript:alert(3)');
  });

  it('shows privacy redaction tokens as the text they are', () => {
    const token = '[CVC_REDACTED:v1:secret_assignment:11111111-2222-4333-8444-555555555555]';
    renderNode(
      makeNode({
        ...promptStep,
        user_prompt: `Use the key ${token}`,
        tool_executions: [toolCall('Bash', { command: `curl -H "Authorization: ${token}" https://example.com` })],
      }),
    );
    expect(screen.getByText(`Use the key ${token}`)).toBeInTheDocument();
    expect(toolCard('Bash').querySelector('pre')!.textContent).toContain(token);
  });
});

describe('artifact links and layout choice', () => {
  it('shows the linked commits on a transcript node', () => {
    renderNode(promptStep);
    expect(screen.getByText('a1b2c3d')).toBeInTheDocument();
    expect(screen.getByText('(Original generated)')).toBeInTheDocument();
  });

  it('chooses the layout by node shape, not by author alone', () => {
    // A harness-shaped id with a plain prompt and no tools is still a transcript node.
    const { container } = renderNode(
      makeNode({ id: promptStep.id, author: 'human', user_prompt: 'hello', model_response: 'hi' }),
    );
    expect(screen.getByText('AI step')).toBeInTheDocument();
    expect(screen.queryByText('Thought')).not.toBeInTheDocument();
    expect(container.querySelector('[data-layout="transcript"]')).not.toBeNull();
  });

  it('survives malformed tool executions in an old, unvalidated blob', () => {
    const junk = [null, 'bash', { tool_name: 'Bash' }] as unknown as InteractionNode['tool_executions'];
    renderNode(makeNode({ ...promptStep, tool_executions: junk }));
    expect(screen.getByText('AI step')).toBeInTheDocument();
    expect(screen.getAllByText('No arguments recorded.')).toHaveLength(3);
    cleanup();

    renderNode(makeNode({ ...promptStep, tool_executions: 'nope' as unknown as InteractionNode['tool_executions'] }));
    expect(screen.queryByText('Tool calls')).not.toBeInTheDocument();
  });

  it('claims nothing about where the node came from', () => {
    const { container } = renderNode(promptStep);
    expect(container.textContent).not.toMatch(/claude code|captured by|verified|trusted/i);
  });

  it('survives a node with nothing recorded', () => {
    renderNode(
      makeNode({ id: promptStep.id, author: 'system', user_prompt: null, model_name: null, tool_executions: [] }),
    );
    expect(screen.getByText('AI step')).toBeInTheDocument();
    expect(screen.getAllByText('No stimulus recorded')).toHaveLength(1); // the badge; the pane explains in words
    expect(screen.getByText('Nothing was recorded for what this step reacted to.')).toBeInTheDocument();
  });
});

describe('wording', () => {
  const toolStep = (...tools: ReturnType<typeof toolCall>[]) =>
    makeNode({ id: promptStep.id, author: 'system', user_prompt: '[tool_result Bash] ok', model_response: 'r', tool_executions: tools });

  it('says what an empty write, an empty command and an argument-less call are', () => {
    renderNode(
      toolStep(
        toolCall('Write', { file_path: '/repo/empty.txt', content: '' }),
        toolCall('Bash', { command: '' }),
        toolCall('mcp__srv__tool', {}),
        rawToolCall('mcp__srv__other', 'null'),
      ),
    );
    expect(screen.getByText('New content is empty.')).toBeInTheDocument();
    expect(screen.getByText('Empty command.')).toBeInTheDocument();
    expect(screen.getByText('Called with no arguments.')).toBeInTheDocument();
    expect(screen.getByText('No arguments recorded.')).toBeInTheDocument();
    for (const old of ['Empty content.', 'No arguments.']) expect(screen.queryByText(old)).not.toBeInTheDocument();
  });

  it('labels a URL as a URL, and the other subjects by what they are', () => {
    renderNode(
      toolStep(
        toolCall('WebFetch', { url: 'https://example.com/a', prompt: 'p' }),
        toolCall('WebSearch', { query: 'q' }),
        toolCall('Glob', { pattern: '*.ts' }),
        toolCall('Read', { file_path: '/repo/a.ts' }),
      ),
    );
    const labels = Array.from(document.querySelectorAll('[data-status] span.uppercase')).map((el) => el.textContent);
    expect(labels).toEqual(['URL', 'query', 'pattern', 'file']);
  });

  it('describes options as what they do', () => {
    renderNode(
      toolStep(
        toolCall('Bash', { command: 'sleep 1', run_in_background: true, timeout: 5000 }),
        toolCall('Edit', { file_path: '/repo/a.ts', old_string: 'a', new_string: 'b', replace_all: true }),
        toolCall('Read', { file_path: '/repo/a.ts', offset: 10, limit: 1 }),
        toolCall('Read', { file_path: '/repo/b.ts', offset: 10, limit: 5 }),
      ),
    );
    expect(screen.getByText('runs in background')).toBeInTheDocument();
    expect(screen.getByText('timeout 5s')).toBeInTheDocument();
    expect(screen.getByText('replace all occurrences')).toBeInTheDocument();
    expect(screen.getByText('line 10')).toBeInTheDocument();
    expect(screen.getByText('lines 10–14')).toBeInTheDocument();
    for (const old of ['background', 'replace all', 'lines 10–10']) expect(screen.queryByText(old)).not.toBeInTheDocument();
  });

  it('names every block that scrolls sideways as a region, so a screen reader says what it holds', () => {
    const { container } = renderNode(
      toolStep(
        toolCall('Bash', { command: 'echo hi' }),
        toolCall('Write', { file_path: '/repo/a.md', content: 'hello' }),
        toolCall('Edit', { file_path: '/repo/a.ts', old_string: 'a', new_string: 'b' }),
        rawToolCall('Bash', 'not json'),
      ),
    );
    const regions = within(container).getAllByRole('region');
    expect(regions.map((region) => region.getAttribute('aria-label'))).toEqual(['Command', 'File content', 'Diff', 'Arguments']);
    // Exactly the focusable blocks inside the card (the card itself is focusable too), and each is focusable.
    expect(regions).toHaveLength((container.firstElementChild as HTMLElement).querySelectorAll('[tabindex]').length);
    for (const region of regions) expect(region).toHaveAttribute('tabindex', '0');
  });

  it('does not make a block that wraps a region, since it does not scroll and is not focusable', () => {
    const { container } = renderNode(
      makeNode({ id: promptStep.id, author: 'system', user_prompt: '[tool_result Bash] wrapped output', model_response: 'r' }),
    );
    fireEvent.click(screen.getByRole('button', { name: /^Tool results/ }));
    const output = container.querySelector('.rr-thought pre')!;
    expect(output).not.toHaveAttribute('role');
    expect(output).not.toHaveAttribute('tabindex');
    expect(within(container).queryAllByRole('region')).toHaveLength(0);
  });

  it('names the texts of a diff that is too large to compute', () => {
    const many = (prefix: string) => Array.from({ length: 2600 }, (_, i) => `${prefix}${i}`).join('\n');
    const { container } = renderNode(toolStep(toolCall('Edit', { file_path: '/repo/a.ts', old_string: many('o'), new_string: many('n') })));
    expect(within(container).getAllByRole('region').map((region) => region.getAttribute('aria-label'))).toEqual(['Old text', 'New text']);
  });

  it('hides the fold glyph from assistive technology and keeps the words', () => {
    const lines = (changed: number[]) => Array.from({ length: 40 }, (_, i) => (changed.includes(i) ? `changed ${i}` : `line ${i}`)).join('\n');
    const { container } = renderNode(toolStep(toolCall('Edit', { file_path: '/repo/a.ts', old_string: lines([]), new_string: lines([20]) })));
    const glyphs = Array.from(container.querySelectorAll('[aria-hidden="true"]')).filter((el) => el.textContent === '⋯');
    expect(glyphs).toHaveLength(2);
    for (const glyph of glyphs) {
      expect(glyph.parentElement!.textContent).toMatch(/^⋯ \d+ unchanged lines$/);
      expect(Array.from(glyph.parentElement!.childNodes).filter((n) => n.nodeType === Node.TEXT_NODE).map((n) => n.textContent!.trim())).toEqual([
        expect.stringMatching(/^\d+ unchanged lines$/),
      ]);
    }
  });

  it('shows a title the way the call shows it, in the stimulus too', () => {
    const node = makeNode({
      id: promptStep.id,
      author: 'human',
      user_prompt: '[tool_result mcp__github__create_issue] done\n\n[tool_result server:web_search] hmm\n\n[tool_result unknown-tool] orphan',
      model_response: 'r',
      tool_executions: [toolCall('mcp__github__create_issue', { title: 't' }), toolCall('server:web_search', { query: 'q' }, 'success')],
    });
    renderNode(node, undefined);
    const stimulus = document.querySelector('.rr-thought') as HTMLElement;
    const headings = Array.from(stimulus.querySelectorAll('span.font-mono.font-semibold')).map((el) => el.textContent);
    expect(headings).toEqual(['github · create_issue', 'web_search (server-side)', 'Unknown tool']);
    // The tool cards use the same two names.
    expect(screen.getAllByText('github · create_issue').length).toBeGreaterThanOrEqual(2);
  });
});
