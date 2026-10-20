/**
 * The transcript layout against nodes the real producer wrote.
 *
 * `./wire/` holds interaction blobs written by the Claude Code harness: the shipped
 * cvc 0.7.0 binary's, and, for server-side tools, the one that records the outcome
 * its result block reported (see `./wire/README.md`). The hand-written fixtures in
 * `fixtures.ts` follow what we believe the wire looks like; these files are what
 * it does look like, so they catch a gap between the two.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from '../components/timeline/ShadowTimeline';
import { TimelineNode } from '../components/timeline/TimelineNode';
import { displayText } from '../lib/textLines';
import {
  failedToolCount,
  nodeLayout,
  parseStimulus,
  stimulusOrigin,
  type StimulusOrigin,
} from '../lib/transcriptNode';
import { normalizeInteraction, type CVCBlobData, type InteractionNode } from '../types/cvc';
import { installDomStubs, restoreDomStubs } from './domStubs';

vi.mock('framer-motion', async () => (await import('./motionStub')).framerMotionStub);

// A React warning or error logged while rendering a real node fails the test that caused it.
let consoleProblems: string[] = [];
beforeEach(() => {
  installDomStubs();
  consoleProblems = [];
  const record = (...args: unknown[]) => {
    consoleProblems.push(args.map(String).join(' ').slice(0, 300));
  };
  vi.spyOn(console, 'error').mockImplementation(record);
  vi.spyOn(console, 'warn').mockImplementation(record);
});
afterEach(() => {
  cleanup();
  const problems = consoleProblems;
  restoreDomStubs();
  expect(problems, 'React or the components logged a warning or an error').toEqual([]);
});

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

const RAW = import.meta.glob<string>('./wire/*/*.json', { eager: true, query: '?raw', import: 'default' });

/** `v2.1.260-basic/<id>`, `edge-cases/<id>`: the path under ./wire/ without the extension. */
function keyOf(path: string): string {
  return path.replace(/^\.\/wire\//, '').replace(/\.json$/, '');
}

const BLOBS = new Map<string, CVCBlobData>(
  Object.entries(RAW).map(([path, raw]) => [keyOf(path), JSON.parse(raw) as CVCBlobData]),
);

const RAW_BY_KEY = new Map<string, string>(Object.entries(RAW).map(([path, raw]) => [keyOf(path), raw]));

function nodeFor(key: string): InteractionNode {
  const blob = BLOBS.get(key);
  if (!blob) throw new Error(`No wire fixture ${key}`);
  return normalizeInteraction(blob);
}

// The v2.1.260 transcript is one conversation: each step reacts to what the previous one did.
const CHAIN = [
  'v2.1.260-basic/916ca1ec-d59e-89f5-a596-7a1d355212c9',
  'v2.1.260-basic/44f4f161-a377-855d-bda5-bcf80f761c63',
  'v2.1.260-basic/daa5ceb6-7e4d-8a1d-8632-c6b36b563250',
  'v2.1.260-basic/3ec4e81b-3ffc-8d60-b160-5c0b3a2407db',
  'v2.1.260-basic/c218f992-6967-8f01-b169-4f47dd21bad4',
  'v2.1.260-basic/c8d6387b-1088-8c41-b825-eb3e2b05ebaf',
  'v2.1.260-basic/7dbdb307-1fb2-8a87-97b5-69be2b9b1f74',
] as const;

const [STEP_A, STEP_B, STEP_C, STEP_D, STEP_E, STEP_F, STEP_G] = CHAIN;

const E1_REDACTIONS = 'edge-cases/ac5cc79b-a8ab-843a-83b2-2094b61f9da2';
const E2_SERVER_TOOL = 'edge-cases/0d3a13e0-0ad7-8fb2-959d-fe4eb3499aa0';
const E3_FAILURES = 'edge-cases/e05d5e7c-2937-84c5-8410-0160c72ab220';
const E4_TRUNCATED_RESULTS = 'edge-cases/a5eb6e77-a1d9-85d4-93d0-fe978a93e042';
const E7_CRLF = 'edge-cases/04d711be-f819-8298-9c67-babb2f00b891';
const E8_NO_STIMULUS = 'edge-cases/a3c1e9a5-9640-88c9-b751-233c60810621';

// A harness that records the outcome of a server-side call (#134): `tool_protocol` "server" and the status
// the provider's own result block reported. S3 is the same step as E2, recorded again.
const S1_SERVER_TOOLS = 'cvc-0.7.1-server-tools/8fc40ad6-0885-8c48-8d10-ddec124a3a0a';
const S2_SERVER_FOLLOW_UP = 'cvc-0.7.1-server-tools/94705671-dba9-8399-8430-839c3fb758d0';
const S3_SERVER_SEARCH = 'cvc-0.7.1-server-tools/0d3a13e0-0ad7-8fb2-959d-fe4eb3499aa0';

type Outcome = 'success' | 'failure' | 'unrecorded';

interface Expected {
  origin: StimulusOrigin;
  /** Tool cards in the order the tools were called: heading and outcome. */
  tools: Array<[title: string, outcome: Outcome]>;
  /** How many tool calls the header reports as failed (0: no chip at all). */
  failed: number;
}

const EXPECTED: Record<string, Expected> = {
  [STEP_A]: { origin: 'Prompt', tools: [['Read', 'success']], failed: 0 },
  [STEP_B]: { origin: 'Tool results', tools: [['Edit', 'failure']], failed: 1 },
  [STEP_C]: { origin: 'Tool results', tools: [['Write', 'success']], failed: 0 },
  [STEP_D]: {
    origin: 'Tool results',
    tools: [
      ['Bash', 'success'],
      ['Read', 'success'],
    ],
    failed: 0,
  },
  [STEP_E]: { origin: 'Tool results', tools: [], failed: 0 },
  [STEP_F]: { origin: 'Compaction summary', tools: [['Read', 'success']], failed: 0 },
  [STEP_G]: { origin: 'Tool results', tools: [], failed: 0 },
  [E1_REDACTIONS]: { origin: 'Prompt', tools: [['github · create_issue', 'success']], failed: 0 },
  // The harness records every server-side tool as failed (it never sees the
  // result), so this node has a "failure" and must still report none.
  [E2_SERVER_TOOL]: { origin: 'Tool results', tools: [['web_search (server-side)', 'unrecorded']], failed: 0 },
  [E3_FAILURES]: {
    origin: 'Prompt',
    tools: [
      ['Write', 'success'],
      ['fixture · bulk_import', 'failure'],
      ['NotebookEdit', 'success'],
      ['Read', 'failure'],
      ['Read', 'failure'],
      ['Task', 'success'],
    ],
    failed: 3,
  },
  [E4_TRUNCATED_RESULTS]: { origin: 'Tool results', tools: [['Bash', 'success']], failed: 0 },
  [E7_CRLF]: { origin: 'Prompt', tools: [], failed: 0 },
  [E8_NO_STIMULUS]: { origin: 'No stimulus recorded', tools: [], failed: 0 },
  // The outcomes the provider's result blocks reported: a success is a success and a failure is counted.
  // The step is a prompt asking for seven server-side calls and a Read; the third `web_search` never got a result block.
  [S1_SERVER_TOOLS]: {
    origin: 'Prompt',
    tools: [
      ['web_search (server-side)', 'success'],
      ['web_search (server-side)', 'failure'],
      ['web_fetch (server-side)', 'success'],
      ['bash_code_execution (server-side)', 'success'],
      ['bash_code_execution (server-side)', 'failure'],
      ['text_editor_code_execution (server-side)', 'success'],
      ['web_search (server-side)', 'failure'],
      ['Read', 'success'],
    ],
    failed: 3,
  },
  [S2_SERVER_FOLLOW_UP]: { origin: 'Tool results', tools: [['web_search (server-side)', 'success']], failed: 0 },
  [S3_SERVER_SEARCH]: { origin: 'Tool results', tools: [['web_search (server-side)', 'success']], failed: 0 },
};

const ALL_KEYS = Object.keys(EXPECTED);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function renderNode(node: InteractionNode, onSelect: () => void = () => {}) {
  return render(<TimelineNode interaction={node} onSelect={onSelect} />);
}

const cardOf = (container: HTMLElement): HTMLElement => container.firstElementChild as HTMLElement;
/** The card's first child is its header (icon, "AI step", badges, time, model). */
const headerOf = (card: HTMLElement): HTMLElement => card.firstElementChild as HTMLElement;

function toolCardsOf(card: HTMLElement): Array<[string, string | null]> {
  return Array.from(card.querySelectorAll('[data-status]')).map((el) => [
    el.firstElementChild!.firstElementChild!.textContent ?? '',
    el.getAttribute('data-status'),
  ]);
}

function toolCard(card: HTMLElement, title: string, index = 0): HTMLElement {
  const matches = Array.from(card.querySelectorAll<HTMLElement>('[data-status]')).filter(
    (el) => el.firstElementChild!.firstElementChild!.textContent === title,
  );
  if (!matches[index]) throw new Error(`No tool card #${index} titled ${title}`);
  return matches[index];
}

/** Clicks every closed toggle until none is left, so that everything recorded is on the page. */
function expandAll(card: HTMLElement) {
  for (let pass = 0; pass < 10; pass++) {
    const closed = Array.from(card.querySelectorAll('button[aria-expanded="false"]'));
    if (closed.length === 0) return;
    for (const button of closed) fireEvent.click(button);
  }
  throw new Error('Toggles kept appearing');
}

/** What the page shows for a recorded string: carriage returns folded away, one trailing newline ignored. */
const shown = (text: string): string => displayText(text).replace(/\n$/, '');

// Words that belong to the Thought / Action / Observation layout or to a verdict.
const TRIAD_AND_VERDICT_WORDS = [
  'Thought',
  'Action',
  'Observation',
  'Decision Factors',
  'AI Agent',
  'Green check',
  'Review required',
  'Critical alert',
];

// ---------------------------------------------------------------------------
// The files
// ---------------------------------------------------------------------------

describe('wire fixtures', () => {
  it('are exactly the files this suite has expectations for', () => {
    expect(Array.from(BLOBS.keys()).sort()).toEqual([...ALL_KEYS].sort());
  });

  it('are named after the interaction they hold', () => {
    for (const [key, blob] of BLOBS) {
      expect(key.split('/')[1], key).toBe(blob.interaction.id);
      for (const tool of blob.tool_executions) {
        expect((tool as unknown as { interaction_id: string }).interaction_id, key).toBe(blob.interaction.id);
      }
    }
  });

  it('have the exact key sets the reviewer demands of a node blob', () => {
    // Mirrors `validNode` in hooks/usePRInteractions.ts: a blob that fails it
    // never reaches the timeline, so a fixture that fails it proves nothing.
    const keys = (value: object) => Object.keys(value).sort();
    for (const [key, blob] of BLOBS) {
      expect(keys(blob), key).toEqual(['artifact_links', 'context_items', 'interaction', 'tool_executions']);
      expect(keys(blob.interaction), key).toEqual(
        [
          'author',
          'conversation_id',
          'id',
          'model_cot',
          'model_name',
          'model_response',
          'parent_id',
          'source_request_id',
          'timestamp',
          'user_prompt',
        ].sort(),
      );
      expect(typeof blob.interaction.user_prompt, key).toBe('string');
      for (const tool of blob.tool_executions) {
        expect(keys(tool), key).toEqual(['arguments', 'id', 'interaction_id', 'status', 'tool_name', 'tool_protocol']);
        expect(typeof tool.arguments, key).toBe('string');
        expect(['success', 'failure'], key).toContain(tool.status);
      }
      for (const link of blob.artifact_links) {
        expect(['generated', 'temporal', 'verified'], key).toContain(link.link_type);
        expect(link.git_commit_hash, key).toMatch(/^[0-9a-f]{40}$/);
      }
      expect(['human', 'agent', 'system', 'external'], key).toContain(blob.interaction.author);
    }
  });

  // What a leaked developer machine or a pasted credential would look like. Guards the next file added to ./wire/.
  const FORBIDDEN: Array<[string, RegExp]> = [
    ['a home directory', /\/home\/|\/Users\/|C:\\\\Users|\/root\//],
    ['a mounted or temp path', /\/run\/media|\/tmp\/|\/var\/folders|\/\.claude\//],
    ['an AWS access key id', /\b(AKIA|ASIA)[0-9A-Z]{16}\b/],
    ['a GitHub token', /\b(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b/],
    ['a private key block', /-----BEGIN [A-Z ]*PRIVATE KEY-----/],
    ['a Slack token', /\bxox[abprs]-[A-Za-z0-9-]{10,}/],
    ['a JWT', /\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\./],
    ['a bearer credential', /bearer\s+[A-Za-z0-9._~+/=-]{16,}/i],
    ['credentials inside a URL', /[a-z][a-z0-9+.-]*:\/\/[^/\s:@]+:[^/\s@]+@/i],
  ];

  // One string is allowed, in one file: `/tmp/notes.txt` is a path inside the provider's code-execution
  // sandbox, written into the repo's own synthetic transcript
  // (cvc-core/tests/fixtures/claude-code/v2.1.300/server-tools.jsonl) and so into the file recorded from it,
  // which is kept as the producer wrote it. It is not a path on any machine; any other temp path, or this one
  // in any other file, still fails.
  const SANDBOX_PATH = '/tmp/notes.txt';
  const SANDBOX_FILES = new Set([S1_SERVER_TOOLS]);

  /** What `raw`, the text of the fixture `key`, leaks, by name. */
  function leaksIn(key: string, raw: string): string[] {
    const scanned = SANDBOX_FILES.has(key) ? raw.replaceAll(SANDBOX_PATH, '<sandbox path>') : raw;
    return FORBIDDEN.filter(([, pattern]) => pattern.test(scanned)).map(([what]) => what);
  }

  it('hold no local paths, personal addresses or secret-shaped strings', () => {
    for (const [key, raw] of RAW_BY_KEY) {
      expect(leaksIn(key, raw), key).toEqual([]);
      const addresses = raw.match(/[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/g) ?? [];
      for (const address of addresses) expect(address, key).toBe('fixture@example.invalid');
    }
  });

  it('allow the sandbox path in the one file that holds it, and nothing else of its kind', () => {
    expect(RAW_BY_KEY.get(S1_SERVER_TOOLS), 'the exception is in use: remove it when the file stops holding the path').toContain(SANDBOX_PATH);
    expect(leaksIn(S1_SERVER_TOOLS, `{"path":"${SANDBOX_PATH}"}`)).toEqual([]);
    // Any other temp path, in that file or another, and that path in another file, is a leak.
    expect(leaksIn(S1_SERVER_TOOLS, '{"path":"/tmp/other.txt"}')).toEqual(['a mounted or temp path']);
    expect(leaksIn(S1_SERVER_TOOLS, `{"path":"${SANDBOX_PATH}","cwd":"/tmp/work"}`)).toEqual(['a mounted or temp path']);
    expect(leaksIn(S1_SERVER_TOOLS, '{"path":"/run/media/someone/disk"}')).toEqual(['a mounted or temp path']);
    expect(leaksIn(S1_SERVER_TOOLS, '{"path":"/home/someone/repo"}')).toEqual(['a home directory']);
    for (const key of [E1_REDACTIONS, S2_SERVER_FOLLOW_UP, S3_SERVER_SEARCH]) {
      expect(leaksIn(key, `{"path":"${SANDBOX_PATH}"}`), key).toEqual(['a mounted or temp path']);
    }
  });

  it('keep every path the producer recorded under the placeholder root', () => {
    for (const [key, raw] of RAW_BY_KEY) {
      const paths = raw.match(/\/work\/repo[^"\\\s]*/g) ?? [];
      for (const path of paths) expect(path, key).toMatch(/^\/work\/repo(\/|$)/);
    }
    // ...and the sanitised root really is in use, so the check above is not vacuous.
    expect(Array.from(RAW_BY_KEY.values()).filter((raw) => raw.includes('/work/repo/')).length).toBeGreaterThan(3);
  });
});

// ---------------------------------------------------------------------------
// Every node
// ---------------------------------------------------------------------------

describe.each(ALL_KEYS)('wire node %s', (key) => {
  const expected = EXPECTED[key];

  it('is laid out as a transcript node', () => {
    expect(nodeLayout(nodeFor(key))).toBe('transcript');
  });

  it('renders without throwing, as an AI step and not as a triad', () => {
    const node = nodeFor(key);
    const { container } = renderNode(node);
    const card = cardOf(container);
    const header = headerOf(card);

    expect(screen.getByText('AI step')).toBeInTheDocument();
    expect(card.querySelector('[data-layout="transcript"]')).not.toBeNull();
    for (const word of TRIAD_AND_VERDICT_WORDS) {
      expect(container.textContent, word).not.toContain(word);
    }
    expect(screen.queryByRole('button', { name: 'Why?' })).not.toBeInTheDocument();

    // Nobody "spoke": the header names neither a user, a system nor an AI agent, whatever the author field says.
    expect(header).toHaveTextContent('AI step');
    expect(header.textContent).not.toMatch(/\b(User|System|AI Agent|External|Unknown)\b/);
    expect(header.querySelector('svg.lucide-user, svg.lucide-terminal, svg.lucide-external-link')).toBeNull();
    expect(header.querySelector('svg.lucide-bot')).not.toBeNull();
    expect(header).toHaveTextContent(node.model_name!);
  });

  it(`names the stimulus origin "${expected.origin}" in the header`, () => {
    const node = nodeFor(key);
    const { container } = renderNode(node);
    expect(within(headerOf(cardOf(container))).getByText(expected.origin)).toBeInTheDocument();
    expect(stimulusOrigin(node, parseStimulus(node.user_prompt))).toBe(expected.origin);
  });

  it('shows one tool card per tool execution, in call order, with the recorded outcome', () => {
    const node = nodeFor(key);
    const { container } = renderNode(node);
    const cards = toolCardsOf(cardOf(container));
    expect(cards).toHaveLength(node.tool_executions.length);
    expect(cards).toEqual(expected.tools);
  });

  it(expected.failed ? `reports ${expected.failed} failed tool call${expected.failed === 1 ? '' : 's'}` : 'reports no failed tool call', () => {
    const node = nodeFor(key);
    const { container } = renderNode(node);
    const chips = within(headerOf(cardOf(container))).queryAllByText(/tool calls? failed/);
    if (expected.failed === 0) {
      expect(chips).toHaveLength(0);
    } else {
      const text = expected.failed === 1 ? '1 tool call failed' : `${expected.failed} tool calls failed`;
      expect(chips.map((chip) => chip.textContent)).toEqual([text]);
    }
    expect(failedToolCount(node)).toBe(expected.failed);
  });

  it('shows no carriage return and renders every wire string as text', () => {
    const { container } = renderNode(nodeFor(key));
    expect(container.textContent).not.toContain('\r');
    expect(container.querySelector('a, img, script, iframe, system-reminder')).toBeNull();
  });

  it('leaves no recorded text out once every toggle is open', () => {
    const node = nodeFor(key);
    const { container } = renderNode(node);
    const card = cardOf(container);
    expandAll(card);
    const text = card.textContent ?? '';

    for (const segment of parseStimulus(node.user_prompt)) {
      if (segment.kind === 'none') continue;
      expect(text, `${key} stimulus ${segment.kind}`).toContain(shown(segment.text));
    }
    for (const field of [node.model_cot, node.model_response]) {
      if (field && field.trim() !== '') expect(text).toContain(shown(field));
    }
    // Every string a tool was given is on the page, except an Edit's old and new
    // text, which is shown as a line diff (each of its lines is checked instead).
    for (const exec of node.tool_executions) {
      const input = JSON.parse(exec.arguments) as Record<string, unknown>;
      if (input.cvc_truncated === true) continue;
      for (const [field, value] of Object.entries(input)) {
        if (typeof value !== 'string') continue;
        if (exec.tool_name === 'Edit' && (field === 'old_string' || field === 'new_string')) {
          for (const line of displayText(value).split('\n')) expect(text).toContain(line);
        } else {
          expect(text, `${key} ${exec.tool_name}.${field}`).toContain(shown(value));
        }
      }
    }
  });

  it('does not write to the node it is given', () => {
    const freeze = <T,>(value: T): T => {
      if (value !== null && typeof value === 'object' && !Object.isFrozen(value)) {
        Object.freeze(value);
        for (const item of Object.values(value)) freeze(item);
      }
      return value;
    };
    const { container } = renderNode(freeze(structuredClone(nodeFor(key))));
    expandAll(cardOf(container));
    expect(screen.getByText('AI step')).toBeInTheDocument();
  });

  it('selects on a click in the card body and never on a toggle', () => {
    const onSelect = vi.fn();
    const { container } = renderNode(nodeFor(key), onSelect);
    const card = cardOf(container);
    // Every control that opens or closes a part of the card (the header, a button too, is the one that selects it).
    for (const toggle of card.querySelectorAll('button:not([aria-pressed])')) fireEvent.click(toggle);
    expect(onSelect).not.toHaveBeenCalled();

    fireEvent.click(screen.getByText('AI step'));
    expect(onSelect).toHaveBeenCalledTimes(1);
  });
});

// ---------------------------------------------------------------------------
// What the producer's output looks like when rendered
// ---------------------------------------------------------------------------

describe('what the producer wrote, as a reviewer sees it', () => {
  it('shows a human prompt followed by injected context, with the markup-like context as text', () => {
    const { container } = renderNode(nodeFor(STEP_A));
    const card = cardOf(container);
    const prompt = within(card).getByText('Add a greeting to lib.rs');
    expect(prompt).toHaveClass('font-serif');
    expect(within(card).getByText('Context')).toBeInTheDocument();
    // The context is `<system-reminder>...</system-reminder>`: text, not an element.
    expect(within(card).getByText('<system-reminder>Fixture reminder text.</system-reminder>')).toBeInTheDocument();
    expect(container.querySelector('system-reminder')).toBeNull();
    expect(within(card).getByText('I should read lib.rs first.')).toBeInTheDocument();
    expect(within(card).getByText("I'll read the file first.")).toBeInTheDocument();
    expect(within(toolCard(card, 'Read')).getByText('/work/repo/src/lib.rs')).toBeInTheDocument();
    expect(within(toolCard(card, 'Read')).getByText('lines 1–20')).toBeInTheDocument();
  });

  it('shows a failed Edit as a diff, flagged as failed, and counts it', () => {
    const { container } = renderNode(nodeFor(STEP_B));
    const card = cardOf(container);
    const edit = toolCard(card, 'Edit');
    expect(edit).toHaveAttribute('data-status', 'failure');
    expect(within(edit).getByText('failed')).toBeInTheDocument();
    expect(Array.from(edit.querySelectorAll('del')).map((el) => el.textContent)).toEqual(['-fn main() {}']);
    expect(Array.from(edit.querySelectorAll('ins')).map((el) => el.textContent)).toEqual(['+fn main() { println!("hi"); }']);
    expect(within(headerOf(card)).getByText('1 tool call failed')).toBeInTheDocument();
  });

  it("folds the previous step's tool results behind a summary naming the failed Edit", () => {
    const { container } = renderNode(nodeFor(STEP_C));
    const card = cardOf(container);
    const toggle = within(card).getByRole('button', { name: 'Tool results · Edit · 1 error' });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(within(card).queryByText('String to replace not found in file.')).not.toBeInTheDocument();

    fireEvent.click(toggle);
    expect(within(card).getByText('String to replace not found in file.')).toBeInTheDocument();
    expect(within(card).getAllByText('error')).toHaveLength(1);
    // The Write is shown with its content, a trailing newline and all.
    const write = toolCard(card, 'Write');
    expect(write.querySelector('pre')!.textContent).toBe('fn main() { println!("hi"); }');
  });

  it('shows two tool calls of one step and the two results of the next', () => {
    const { container } = renderNode(nodeFor(STEP_D));
    const card = cardOf(container);
    const bash = toolCard(card, 'Bash');
    expect(within(bash).getByText('Commit the greeting')).toBeInTheDocument();
    expect(bash.querySelector('pre')!.textContent).toBe('git commit -am greet');
    expect(within(toolCard(card, 'Read')).getByText('/etc/hosts')).toBeInTheDocument();
    cleanup();

    const next = renderNode(nodeFor(STEP_E));
    const nextCard = cardOf(next.container);
    const toggle = within(nextCard).getByRole('button', { name: 'Tool results · Bash, Read' });
    fireEvent.click(toggle);
    // Blank-free results: two separate items, each under its tool's name.
    const pres = Array.from(nextCard.querySelectorAll('pre')).map((pre) => pre.textContent);
    expect(pres).toEqual(['[main 1234abc] greet\n 1 file changed, 1 insertion(+), 1 deletion(-)', '127.0.0.1 localhost']);
  });

  it('labels a compaction summary as such, and shows the Read that follows it', () => {
    const { container } = renderNode(nodeFor(STEP_F));
    const card = cardOf(container);
    // Once as the header badge, once as the pane label.
    expect(within(card).getAllByText('Compaction summary')).toHaveLength(2);
    expect(
      within(card).getByText('This session is being continued from a previous conversation. Summary: a greeting was added.'),
    ).toBeInTheDocument();
    expect(within(toolCard(card, 'Read')).getByText('/work/repo/secret/key.txt')).toBeInTheDocument();
  });

  it('keeps the content of a file the model read behind the fold on a system step', () => {
    const { container } = renderNode(nodeFor(STEP_G));
    const card = cardOf(container);
    expect(within(card).queryByText(/fixture-secret-content/)).not.toBeInTheDocument();
    fireEvent.click(within(card).getByRole('button', { name: 'Tool results · Read' }));
    expect(card.querySelector('pre')!.textContent).toBe('     1\tfixture-secret-content');
  });

  it('shows redaction markers verbatim in the prompt and in tool arguments, and placeholders as plain text', () => {
    const { container } = renderNode(nodeFor(E1_REDACTIONS));
    const card = cardOf(container);
    const prompt = nodeFor(E1_REDACTIONS).user_prompt!;
    // The marker that sits against the next token is not split or rewritten.
    expect(prompt).toContain('[CVC_REDACTED:v1:url_credentials:cd36faac-703b-4b35-bd13-8c9edb8687a5]example.invalid/repo.git');
    const promptElement = within(card).getByText(/Please deploy using the key/);
    expect(promptElement.textContent).toBe(prompt);
    expect(promptElement.textContent).toMatch(/\[CVC_REDACTED:v1:aws_access_key:[0-9a-f-]{36}\]/);
    expect(promptElement.textContent).toMatch(/\n\n\[image\]\n\n\[document\]$/);

    // Two thinking blocks arrive joined by a blank line; an unknown block is a bracketed placeholder.
    // (The identity normalizer keeps the blank lines, which getByText would otherwise collapse.)
    const verbatim = { normalizer: (text: string) => text };
    expect(
      within(card).getByText('Plan step one: file the issue.\n\nPlan step two: then search the web.', verbatim),
    ).toBeInTheDocument();
    expect(within(card).getByText("I'll create an issue.\n\nSecond text block.\n\n[mystery]", verbatim)).toBeInTheDocument();

    const issue = toolCard(card, 'github · create_issue');
    expect(within(issue).getByText('body')).toBeInTheDocument();
    expect(within(issue).getByText(/^\[CVC_REDACTED:v1:secret_assignment:[0-9a-f-]{36}\]$/)).toBeInTheDocument();
    expect(within(issue).getByText('Deploy')).toBeInTheDocument();
    expect(issue.textContent).toContain('"bug"');

    // The one generated link, labelled for what it is.
    expect(within(card).getByText('e71bb6d')).toBeInTheDocument();
    expect(within(card).getByText('(Original generated)')).toBeInTheDocument();
  });

  it('shows a server-side tool as unrecorded, not failed, and leaves the header without a count', () => {
    const { container } = renderNode(nodeFor(E2_SERVER_TOOL));
    const card = cardOf(container);
    const search = toolCard(card, 'web_search (server-side)');
    expect(search).toHaveAttribute('data-status', 'unrecorded');
    expect(within(search).getByText('server-side · result not recorded')).toBeInTheDocument();
    expect(within(search).queryByText('failed')).not.toBeInTheDocument();
    expect(within(search).getByText('query')).toBeInTheDocument();
    expect(within(search).getByText('cvc wire format')).toBeInTheDocument();
    expect(nodeFor(E2_SERVER_TOOL).tool_executions[0].status).toBe('failure'); // as recorded
    expect(screen.queryByText(/tool calls? failed/)).not.toBeInTheDocument();

    // The result of the step before is a redacted header, as text.
    // The result is named as the call is: `github · create_issue`, not `mcp__github__create_issue`.
    fireEvent.click(within(card).getByRole('button', { name: 'Tool results · github · create_issue' }));
    expect(
      within(card).getByText(/^Created issue #42 \(sent with Authorization: \[CVC_REDACTED:v1:authorization:[0-9a-f-]{36}\]\)$/),
    ).toBeInTheDocument();
    expect(within(card).getByText(/\[web_search_tool_result\]/)).toBeInTheDocument();
  });

  describe('server-side tools recorded by a harness that keeps the outcome (cvc-0.7.1-server-tools)', () => {
    const SERVER_FILES = [S1_SERVER_TOOLS, S2_SERVER_FOLLOW_UP, S3_SERVER_SEARCH];
    /** The cards of a node, with the badge each one shows, in call order. */
    const badgesOf = (card: HTMLElement) =>
      Array.from(card.querySelectorAll<HTMLElement>('[data-status]')).map((el) => [
        el.firstElementChild!.firstElementChild!.textContent,
        el.firstElementChild!.lastElementChild!.textContent,
      ]);
    const PROVIDER_SIDE = "Ran on the model provider's side, not in the local session.";
    const SUCCESS_TIP = `${PROVIDER_SIDE} The outcome is the one its result block reported: it reported no error.`;
    const FAILURE_TIP = `${PROVIDER_SIDE} Recorded as failed: its result block reported an error or a non-zero return code, or no result block arrived in the same response.`;

    it('are recorded with the protocol "server" and the status of the result block, and the 0.7.0 recording is not', () => {
      const statuses = (key: string) => nodeFor(key).tool_executions.map((tool) => [tool.tool_protocol, tool.tool_name, tool.status]);
      expect(statuses(S1_SERVER_TOOLS)).toEqual([
        ['server', 'server:web_search', 'success'],
        ['server', 'server:web_search', 'failure'],
        ['server', 'server:web_fetch', 'success'],
        ['server', 'server:bash_code_execution', 'success'],
        ['server', 'server:bash_code_execution', 'failure'],
        ['server', 'server:text_editor_code_execution', 'success'],
        ['server', 'server:web_search', 'failure'],
        ['native', 'Read', 'success'],
      ]);
      expect(statuses(S2_SERVER_FOLLOW_UP)).toEqual([['server', 'server:web_search', 'success']]);
      expect(statuses(S3_SERVER_SEARCH)).toEqual([['server', 'server:web_search', 'success']]);
      // The same call, as 0.7.0 wrote it: the protocol the harness itself uses, and a failure whatever happened.
      expect(statuses(E2_SERVER_TOOL)).toEqual([['native', 'server:web_search', 'failure']]);
      expect(nodeFor(S3_SERVER_SEARCH).tool_executions[0].arguments).toBe(nodeFor(E2_SERVER_TOOL).tool_executions[0].arguments);
    });

    it('record the outcome and never the result: only placeholders reach the text of the step', () => {
      for (const key of SERVER_FILES) {
        const raw = RAW_BY_KEY.get(key)!;
        expect(raw, key).not.toMatch(/THIRD-PARTY|encrypted_content|page_age|web_search_result|stdout|stderr/);
      }
      const response = nodeFor(S1_SERVER_TOOLS).model_response!;
      expect(response.match(/\[[a-z_]+_tool_result\]/g)).toEqual([
        '[web_search_tool_result]',
        '[web_search_tool_result]',
        '[web_fetch_tool_result]',
        '[bash_code_execution_tool_result]',
        '[bash_code_execution_tool_result]',
        '[text_editor_code_execution_tool_result]',
      ]);
    });

    it('shows each recorded outcome as recorded: successes as success, failures as failed', () => {
      const { container } = renderNode(nodeFor(S1_SERVER_TOOLS));
      expect(badgesOf(cardOf(container))).toEqual([
        ['web_search (server-side)', 'success'],
        ['web_search (server-side)', 'failed'],
        ['web_fetch (server-side)', 'success'],
        ['bash_code_execution (server-side)', 'success'],
        ['bash_code_execution (server-side)', 'failed'],
        ['text_editor_code_execution (server-side)', 'success'],
        ['web_search (server-side)', 'failed'],
        ['Read', 'success'],
      ]);
    });

    it('counts the three recorded failures in the header, and says what the count leaves out', () => {
      const { container } = renderNode(nodeFor(S1_SERVER_TOOLS));
      const chip = within(headerOf(cardOf(container))).getByText('3 tool calls failed');
      expect(chip).toHaveAttribute(
        'title',
        'Tool calls that reported an error or have no recorded result, such as an interrupted call. Server-side calls that cvc 0.7.0 recorded as failed without knowing how they ended are not counted.',
      );
      expect(failedToolCount(nodeFor(S1_SERVER_TOOLS))).toBe(3);
    });

    it('never says "result not recorded" about a call recorded with the protocol "server"', () => {
      for (const key of SERVER_FILES) {
        const { container, unmount } = renderNode(nodeFor(key));
        const card = cardOf(container);
        expandAll(card);
        expect(card.textContent, key).not.toContain('result not recorded');
        expect(card.querySelector('[data-status="unrecorded"]'), key).toBeNull();
        expect(card.querySelector('[data-status="unknown"]'), key).toBeNull();
        unmount();
      }
    });

    it('says, on the badge of a call the provider ran, that it ran there and where its outcome comes from', () => {
      const { container } = renderNode(nodeFor(S1_SERVER_TOOLS));
      const card = cardOf(container);
      const succeeded = [0, 2, 3, 5].map((index) => Array.from(card.querySelectorAll<HTMLElement>('[data-status]'))[index]);
      for (const call of succeeded) expect(within(call).getByText('success')).toHaveAttribute('title', SUCCESS_TIP);
      const failed = [1, 4, 6].map((index) => Array.from(card.querySelectorAll<HTMLElement>('[data-status]'))[index]);
      for (const call of failed) expect(within(call).getByText('failed')).toHaveAttribute('title', FAILURE_TIP);
      // The Read the harness itself ran has no such note, and neither tip names a vendor.
      expect(within(toolCard(card, 'Read')).getByText('success')).not.toHaveAttribute('title');
      expect(`${SUCCESS_TIP} ${FAILURE_TIP}`).not.toMatch(/anthropic|claude|openai/i);
    });

    it('shows the web search 0.7.0 recorded as unrecorded, and the same step recorded again as a success', () => {
      const old = renderNode(nodeFor(E2_SERVER_TOOL));
      expect(toolCardsOf(cardOf(old.container))).toEqual([['web_search (server-side)', 'unrecorded']]);
      expect(within(cardOf(old.container)).getByText('server-side · result not recorded')).toBeInTheDocument();
      old.unmount();

      const recorded = renderNode(nodeFor(S3_SERVER_SEARCH));
      const card = cardOf(recorded.container);
      expect(toolCardsOf(card)).toEqual([['web_search (server-side)', 'success']]);
      expect(within(card).queryByText('server-side · result not recorded')).not.toBeInTheDocument();
      expect(within(toolCard(card, 'web_search (server-side)')).getByText('cvc wire format')).toBeInTheDocument();
    });

    it('lists the two steps of the conversation in order, each checked against the step before it', () => {
      // S2 answers S1: its stimulus is the Read result, and the server-side calls of S1 have no result to match.
      const nodes = [nodeFor(S1_SERVER_TOOLS), nodeFor(S2_SERVER_FOLLOW_UP)];
      expect(nodes[1].parent_id).toBe(nodes[0].id);
      const { container } = render(<ShadowTimeline interactions={nodes} />);
      expect(screen.queryByText(/Shown undivided/)).not.toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Tool results · Read' })).toBeInTheDocument();
      const cards = Array.from(container.querySelectorAll<HTMLElement>('ol > li'));
      expect(cards).toHaveLength(2);
      // Newest first: the follow-up has no failure, the first step three.
      expect(within(cards[0]).queryByText(/tool calls? failed/)).not.toBeInTheDocument();
      expect(within(cards[1]).getByText('3 tool calls failed')).toBeInTheDocument();
      expect(screen.getAllByText(/tool calls? failed/)).toHaveLength(1);
    });
  });

  describe('a step with truncated arguments, redacted fields and unanswered calls', () => {
    it('counts the three failures: one errored call and two with no result', () => {
      const node = nodeFor(E3_FAILURES);
      const { container } = renderNode(node);
      const card = cardOf(container);
      expect(within(headerOf(card)).getByText('3 tool calls failed')).toBeInTheDocument();
      expect(node.tool_executions.filter((tool) => tool.status === 'failure')).toHaveLength(3);

      const unanswered = within(card).getAllByText('failed');
      expect(unanswered).toHaveLength(3);
      for (const badge of unanswered) expect(badge).toHaveAttribute('title', expect.stringContaining('no result was recorded'));
    });

    it('explains the arguments that exceeded the capture limit, by size', () => {
      const { container } = renderNode(nodeFor(E3_FAILURES));
      const card = cardOf(container);
      const bulk = toolCard(card, 'fixture · bulk_import');
      expect(bulk).toHaveAttribute('data-status', 'failure');
      expect(within(bulk).getByText('Arguments not recorded: 100041 bytes exceeded the capture limit.')).toBeInTheDocument();
      expect(card.textContent).not.toContain('cvc_truncated');
    });

    it('shows a string cut inside an argument together with the producer\'s own cut marker', () => {
      const { container } = renderNode(nodeFor(E3_FAILURES));
      const write = toolCard(cardOf(container), 'Write');
      expect(within(write).getByText('/work/repo/big.txt')).toBeInTheDocument();
      const content = write.querySelector('pre')!.textContent!;
      expect(content.startsWith('xxxx')).toBe(true);
      expect(content.endsWith('\n…[cvc: truncated 23616 bytes]')).toBe(true);
      expect(content.split('\n')).toHaveLength(2);
    });

    it('shows a field whose value was redacted next to the fields the tool is understood by', () => {
      const { container } = renderNode(nodeFor(E3_FAILURES));
      const notebook = toolCard(cardOf(container), 'NotebookEdit');
      expect(within(notebook).getByText('/work/repo/nb.ipynb')).toBeInTheDocument();
      expect(notebook.querySelector('pre')!.textContent).toBe('print(1)');
      expect(within(notebook).getByText('api_key')).toBeInTheDocument();
      expect(within(notebook).getByText(/^\[CVC_REDACTED:v1:secret_assignment:[0-9a-f-]{36}\]$/)).toBeInTheDocument();
    });

    it('shows the two Reads that got no result, and a sub-agent whose prompt waits behind a toggle', () => {
      const { container } = renderNode(nodeFor(E3_FAILURES));
      const card = cardOf(container);
      const first = toolCard(card, 'Read', 0);
      const second = toolCard(card, 'Read', 1);
      expect(within(first).getByText('/work/repo/src/lib.rs')).toBeInTheDocument();
      expect(within(first).getByText('from line 5')).toBeInTheDocument();
      expect(within(second).getByText('/work/repo/secret/key.txt')).toBeInTheDocument();

      const task = toolCard(card, 'Task');
      expect(task).toHaveAttribute('data-status', 'success');
      expect(within(task).getByText('Survey the repo layout')).toBeInTheDocument();
      expect(within(task).getByText('subagent Explore')).toBeInTheDocument();
      expect(within(task).queryByText('List the files')).not.toBeInTheDocument();
      fireEvent.click(within(task).getByRole('button', { name: 'Show subagent prompt' }));
      expect(within(task).getByText('List the files')).toBeInTheDocument();
    });
  });

  describe('a step reacting to results that were cut head and tail', () => {
    it('keeps each result whole: blank lines inside one do not split it', () => {
      const node = nodeFor(E4_TRUNCATED_RESULTS);
      const segments = parseStimulus(node.user_prompt);
      expect(segments.map((s) => (s.kind === 'tool_result' ? `${s.tool}${s.isError ? '!' : ''}` : s.kind))).toEqual([
        'Write',
        'mcp__fixture__bulk_import!',
        'NotebookEdit',
        'Task',
      ]);
      expect(segments[0]).toMatchObject({ text: 'File created successfully\n\nNote: 1 file written\n\nDone' });
    });

    it('shows the cut marker inside the result it cut, between the kept head and tail', () => {
      const { container } = renderNode(nodeFor(E4_TRUNCATED_RESULTS));
      const card = cardOf(container);
      const toggle = within(card).getByRole('button', {
        name: 'Tool results · Write, fixture · bulk_import, NotebookEdit, Task · 1 error',
      });
      expect(toggle).toHaveAttribute('aria-expanded', 'false');
      fireEvent.click(toggle);

      // The notebook result is one line of several thousand characters, so it starts cut to a
      // length budget (head only, the marker not yet in view) behind "Show all N characters".
      const [, , collapsed] = Array.from(card.querySelectorAll('.rr-thought pre')).map((pre) => pre.textContent!);
      expect(collapsed.startsWith('HEAD-MARKER m')).toBe(true);
      expect(collapsed).not.toContain('…[cvc: truncated');
      for (const more of within(card).getAllByRole('button', { name: /^Show all [\d,]+ characters$/ })) fireEvent.click(more);

      const [write, bulk, notebook, task] = Array.from(card.querySelectorAll('.rr-thought pre')).map((pre) => pre.textContent!);
      expect(write).toBe('File created successfully\n\nNote: 1 file written\n\nDone');
      expect(bulk).toBe('Error: payload too large');
      expect(notebook.startsWith('HEAD-MARKER m')).toBe(true);
      expect(notebook).toContain('\n…[cvc: truncated 3808 bytes]…\n');
      expect(notebook.endsWith('m TAIL-MARKER')).toBe(true);
      expect(task).toBe('Subagent finished');
      expect(within(card).getAllByText('error')).toHaveLength(1);
    });
  });

  describe('a human step with Windows line endings and an orphan tool result', () => {
    it('shows the prompt without carriage returns and keeps the tab', () => {
      const { container } = renderNode(nodeFor(E7_CRLF));
      const prompt = within(cardOf(container)).getByText(/Try again/);
      expect(nodeFor(E7_CRLF).user_prompt).toContain('\r\n');
      expect(prompt.textContent).toBe('Try again\nsecond line\ttabbed');
    });

    it('opens the orphan result, since a human wrote the step', () => {
      const { container } = renderNode(nodeFor(E7_CRLF));
      const card = cardOf(container);
      const toggle = within(card).getByRole('button', { name: 'Tool results · Unknown tool' });
      expect(toggle).toHaveAttribute('aria-expanded', 'true');
      expect(within(card).getByText('orphan output with no matching tool_use')).toBeInTheDocument();
    });
  });

  it('says when no stimulus was recorded and does not print the sentinel', () => {
    const { container } = renderNode(nodeFor(E8_NO_STIMULUS));
    const card = cardOf(container);
    expect(within(card).getAllByText('No stimulus recorded')).toHaveLength(1); // the badge
    expect(within(card).getByText('Nothing was recorded for what this step reacted to.')).toBeInTheDocument();
    expect(container.textContent).not.toContain('[no stimulus recorded]');
    expect(within(card).getByText('Resumed without a recorded prompt.')).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// Chains and layout signals
// ---------------------------------------------------------------------------

describe('the v2.1.260 conversation', () => {
  const nodes = CHAIN.map(nodeFor);

  it('is one conversation of seven steps, each the child of the one before', () => {
    expect(nodes).toHaveLength(7);
    expect(nodes[0].parent_id).toBeNull();
    for (let i = 1; i < nodes.length; i++) {
      expect(nodes[i].parent_id, `step ${i + 1}`).toBe(nodes[i - 1].id);
    }
    expect(new Set(nodes.map((node) => node.conversation_id)).size).toBe(1);
    const times = nodes.map((node) => node.timestamp);
    expect(times).toEqual([...times].sort((a, b) => a - b));
    expect(new Set(times).size).toBe(7);
  });

  it("reacts to the previous step's tool calls: the same tools, in order, with errors where they failed", () => {
    for (let i = 1; i < nodes.length; i++) {
      const parent = nodes[i - 1];
      const results = parseStimulus(nodes[i].user_prompt).filter((segment) => segment.kind === 'tool_result');
      if (parent.tool_executions.length === 0) {
        // Nothing was called, so nothing came back (step 6 reacts to a compaction summary instead).
        expect(results, `step ${i + 1}`).toEqual([]);
        continue;
      }
      expect(
        results.map((segment) => segment.tool),
        `step ${i + 1} tools`,
      ).toEqual(parent.tool_executions.map((tool) => tool.tool_name));
      expect(
        results.map((segment) => segment.isError),
        `step ${i + 1} errors`,
      ).toEqual(parent.tool_executions.map((tool) => tool.status === 'failure'));
    }
  });

  it('shows in each step the tool names the previous step called', () => {
    for (let i = 1; i < nodes.length; i++) {
      const parent = nodes[i - 1];
      if (parent.tool_executions.length === 0) continue;
      const parentCards = (() => {
        const { container, unmount } = renderNode(parent);
        const titles = toolCardsOf(cardOf(container)).map(([title]) => title);
        unmount();
        return titles;
      })();
      const { container, unmount } = renderNode(nodes[i]);
      const toggle = within(cardOf(container)).getByRole('button', { name: /^Tool results/ });
      for (const title of parentCards) expect(toggle.textContent, `step ${i + 1}`).toContain(title);
      unmount();
    }
  });

  it('classifies the steps by the stimulus they reacted to', () => {
    const origins = nodes.map((node) => stimulusOrigin(node, parseStimulus(node.user_prompt)));
    expect(origins).toEqual([
      'Prompt',
      'Tool results',
      'Tool results',
      'Tool results',
      'Tool results',
      'Compaction summary',
      'Tool results',
    ]);
  });

  it('renders as one timeline, newest first, with every step a transcript card', () => {
    const onSelect = vi.fn();
    const { container } = render(<ShadowTimeline interactions={nodes} onSelectInteraction={onSelect} />);
    expect(screen.getByText('7 interactions linked to this PR')).toBeInTheDocument();
    expect(container.querySelectorAll('[data-layout="transcript"]')).toHaveLength(7);
    expect(screen.getAllByText('AI step')).toHaveLength(7);

    const times = screen
      .getAllByText(/^<2026-09-10T10:00:\d\d\.000Z>$/)
      .map((el) => el.textContent!.slice(1, -1));
    expect(times).toEqual([...times].sort().reverse());
    expect(times).toHaveLength(7);
    expect(times[0]).toBe('2026-09-10T10:00:23.000Z'); // the last step is on top

    fireEvent.click(screen.getAllByText('AI step')[3]);
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenCalledWith(nodes[3].id); // fourth from the top = fourth from the end
  });
});

describe('the edge-case conversation', () => {
  it('chains from the redacted prompt through the server-side search to the failed batch and its results', () => {
    const e1 = nodeFor(E1_REDACTIONS);
    const e2 = nodeFor(E2_SERVER_TOOL);
    const e3 = nodeFor(E3_FAILURES);
    const e4 = nodeFor(E4_TRUNCATED_RESULTS);
    expect(e1.parent_id).toBeNull();
    expect(e2.parent_id).toBe(e1.id);
    expect(e3.parent_id).toBe(e2.id);
    expect(e4.parent_id).toBe(e3.id);
    expect(new Set([e1, e2, e3, e4].map((node) => node.conversation_id)).size).toBe(1);

    // E2 reacts to E1's one tool call.
    expect(parseStimulus(e2.user_prompt).map((s) => s.kind === 'tool_result' && s.tool)).toEqual(['mcp__github__create_issue']);
    // E4 reacts to E3's calls, minus the two Reads that never got a result.
    const answered = parseStimulus(e4.user_prompt).flatMap((s) => (s.kind === 'tool_result' ? [s.tool] : []));
    expect(answered).toEqual(['Write', 'mcp__fixture__bulk_import', 'NotebookEdit', 'Task']);
    expect(e3.tool_executions.map((tool) => tool.tool_name)).toEqual([
      'Write',
      'mcp__fixture__bulk_import',
      'NotebookEdit',
      'Read',
      'Read',
      'Task',
    ]);
  });
});

describe('what makes these nodes transcript nodes', () => {
  // Re-id every node with a random (version 4) id: only the other signals are left.
  const V4 = '3b9f0f1e-6a4c-4d6e-9b1a-2f6c8e7d5a10';

  it('is the version 8 id for a plain human message, and a marker or a tool call for the rest', () => {
    const viaId: string[] = [];
    for (const key of ALL_KEYS) {
      const node = nodeFor(key);
      expect(nodeLayout(node), key).toBe('transcript');
      if (nodeLayout({ ...node, id: V4 }) === 'triad') viaId.push(key);
    }
    // Only the CRLF step has neither a marker at the start of its prompt nor a tool call.
    expect(viaId).toEqual([E7_CRLF]);
  });

  it('keeps every one of them on the triad layout when authored as an agent', () => {
    for (const key of ALL_KEYS) {
      expect(nodeLayout({ ...nodeFor(key), author: 'agent' }), key).toBe('triad');
    }
  });

  it('writes the ids the layout check expects: version 8, RFC variant', () => {
    for (const key of ALL_KEYS) {
      expect(nodeFor(key).id, key).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-8[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    }
  });
});
