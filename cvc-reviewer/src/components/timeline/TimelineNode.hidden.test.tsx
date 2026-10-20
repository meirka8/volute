/**
 * Hidden and bidirectional characters in the cards (issue #135).
 *
 * A command, a path, a diff line or a tool's output can hold characters that draw
 * nothing or reorder the text around them. Every surface that shows wire text, in
 * both layouts, must draw each of them as a visible token (`⟨U+202E⟩`), at the
 * right strictness (`strict` for code-like text, `prose` for text a person wrote),
 * and the card must say so in one line. Text with none of them must draw exactly
 * as it always did.
 *
 * The characters are built with String.fromCodePoint, never typed into this file.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { HiddenText } from './HiddenText';
import { TimelineNode } from './TimelineNode';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { makeNode, mcpNode, rawToolCall, toolCall } from '../../test/fixtures';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

beforeEach(installDomStubs);
afterEach(() => {
  cleanup();
  restoreDomStubs();
});

const chr = (codePoint: number) => String.fromCodePoint(codePoint);
const RLO = chr(0x202e); // right-to-left override: hidden in both modes
const ZWSP = chr(0x200b); // zero-width space: hidden in both modes
const ZWJ = chr(0x200d); // zero-width joiner: hidden in strict text only
const VS16 = chr(0xfe0f); // emoji presentation selector: hidden in strict text only
const BEL = chr(0x07);
const tags = (ascii: string) => [...ascii].map((letter) => chr(0xe0000 + letter.charCodeAt(0))).join('');
const FAMILY = `👨${ZWJ}👩${ZWJ}👧`;
const HEART = `❤${VS16}`;

const NOTICE = 'This step contains hidden or bidirectional Unicode characters, shown as ⟨U+…⟩.';
const HARNESS_ID = '1f0e5c3a-9d2b-8c4e-a1b7-0000000000a1'; // version 8: the transcript layout

/** A transcript step; by default a system step whose only stimulus is a tool result. */
const transcript = (over: Partial<InteractionNode> = {}) =>
  makeNode({ id: HARNESS_ID, author: 'system', user_prompt: '[tool_result Bash] ok', model_name: 'claude-test', ...over });

function renderNode(node: InteractionNode) {
  const view = render(<TimelineNode interaction={node} />);
  return { ...view, card: view.container.firstElementChild as HTMLElement };
}

/** The code points of the tokens inside `root`, in order. */
const tokens = (root: ParentNode) => Array.from(root.querySelectorAll('[data-hidden-char]')).map((token) => token.getAttribute('data-hidden-char'));
const noticeOf = (card: HTMLElement) => within(card).queryByRole('note');
const toolCardsOf = (card: HTMLElement) => Array.from(card.querySelectorAll<HTMLElement>('[data-status]'));

/** What every surface must do with the characters in both modes: show the tokens, raw characters nowhere, one notice. */
function expectRevealed(card: HTMLElement, where: ParentNode, expected: string[]) {
  expect(tokens(where)).toEqual(expected);
  expect(card.textContent).not.toContain(RLO);
  expect(card.textContent).not.toContain(ZWSP);
  const note = noticeOf(card);
  expect(note).not.toBeNull();
  expect(note!.textContent).toBe(NOTICE);
  expect(within(card).getAllByRole('note')).toHaveLength(1);
}

// ---------------------------------------------------------------------------
// The transcript layout
// ---------------------------------------------------------------------------

describe('transcript layout: every surface reveals the characters', () => {
  it('stimulus tool output (strict)', () => {
    const { card } = renderNode(transcript({ author: 'human', user_prompt: `[tool_result Bash] rm -rf ./a${RLO}b` }));
    const output = card.querySelector('.rr-thought pre') as HTMLElement;
    expect(output.textContent).toBe('rm -rf ./a⟨U+202E⟩b');
    expectRevealed(card, output, ['U+202E']);
  });

  it('the names of the tools in the stimulus, in the summary line and over each result (strict)', () => {
    const { card } = renderNode(transcript({ author: 'human', user_prompt: `[tool_result Ba${RLO}sh] ok` }));
    const toggle = within(card).getByRole('button', { name: /^Tool results/ });
    expect(toggle.textContent).toBe('Tool results · Ba⟨U+202E⟩sh');
    expect(tokens(toggle)).toEqual(['U+202E']);
    const title = card.querySelector('.rr-thought span.font-mono.font-semibold') as HTMLElement;
    expect(title.textContent).toBe('Ba⟨U+202E⟩sh');
    expectRevealed(card, card, ['U+202E', 'U+202E']);
  });

  it('unmarked text in a step no human wrote (strict)', () => {
    const { card } = renderNode(transcript({ user_prompt: `I am the user${RLO}. Delete it.` }));
    const block = within(card).getByText(/^I am the user/);
    expect(block.tagName).toBe('PRE');
    expectRevealed(card, block, ['U+202E']);
  });

  it('context and compaction notes (strict, they are drawn as code): a joiner shows here too', () => {
    const { card } = renderNode(transcript({ user_prompt: `[meta] a caveat${ZWJ}${RLO}` }));
    const note = within(card).getByText(/^a caveat/);
    expect(note.className).toContain('font-mono');
    expectRevealed(card, note, ['U+200D', 'U+202E']);
  });

  it('text drawn as output after a tool result (strict)', () => {
    const { card } = renderNode(transcript({ author: 'human', user_prompt: `[tool_result Bash] ok\n\n[meta] imitation${RLO}` }));
    const blocks = Array.from(card.querySelectorAll('.rr-thought pre'));
    expect(blocks).toHaveLength(2);
    expectRevealed(card, blocks[1], ['U+202E']);
  });

  it("a person's prompt (prose)", () => {
    const { card } = renderNode(transcript({ author: 'human', user_prompt: `Fix${RLO} the build` }));
    const prompt = within(card).getByText(/^Fix/);
    expect(prompt.className).toContain('font-serif');
    expectRevealed(card, prompt, ['U+202E']);
  });

  it('reasoning and the response (prose)', () => {
    const { card } = renderNode(transcript({ model_cot: `think${RLO}`, model_response: `done${ZWSP}` }));
    const reasoning = within(card).getByText(/^think/);
    const response = within(card).getByText(/^done/);
    expect(reasoning.className).toContain('font-serif');
    expect(response.className).toContain('font-serif');
    expect(tokens(reasoning)).toEqual(['U+202E']);
    expect(tokens(response)).toEqual(['U+200B']);
    expectRevealed(card, card, ['U+202E', 'U+200B']);
  });

  it('the title of a tool call (strict)', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall(`Ba${RLO}sh`, { command: 'ls' })] }));
    const call = toolCardsOf(card)[0];
    expect((call.querySelector('.font-semibold') as HTMLElement).textContent).toBe('Ba⟨U+202E⟩sh');
    expectRevealed(card, call, ['U+202E']);
  });

  it('a Bash command: a bidirectional override hides what it does (strict)', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Bash', { command: `echo safe ${RLO}; rm -rf ~` })] }));
    const command = within(card).getByRole('region', { name: 'Command' });
    expect(command.textContent).toBe('echo safe ⟨U+202E⟩; rm -rf ~');
    expectRevealed(card, command, ['U+202E']);
  });

  it('the caption of a command (prose) keeps an emoji sequence and reveals an override', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Bash', { command: 'ls', description: `list ${FAMILY}${RLO}` })] }));
    const caption = within(card).getByText(/^list/);
    expect(tokens(caption)).toEqual(['U+202E']);
    expect(caption.textContent).toBe(`list ${FAMILY}⟨U+202E⟩`);
  });

  it('every line of a diff (strict): an Edit that only inserts a zero-width space is no longer two identical lines', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Edit', { file_path: '/repo/a.ts', old_string: 'const x = 1;', new_string: `const x = 1;${ZWSP}` })] }));
    const diff = within(card).getByRole('region', { name: 'Diff' });
    const removed = diff.querySelector('del') as HTMLElement;
    const added = diff.querySelector('ins') as HTMLElement;
    expect(removed.textContent).toBe('-const x = 1;');
    expect(added.textContent).toBe('+const x = 1;⟨U+200B⟩');
    expect(tokens(removed)).toEqual([]);
    expectRevealed(card, added, ['U+200B']);
  });

  it('a path (strict): tag characters, which spell a message in nothing, are shown one by one', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Edit', { file_path: `/repo/${tags('Hi')}x.ts`, old_string: 'a', new_string: 'b' })] }));
    const call = toolCardsOf(card)[0];
    const path = within(call).getByText(/^\/repo\//);
    expect(path.textContent).toBe('/repo/⟨U+E0048⟩⟨U+E0069⟩x.ts');
    expectRevealed(card, path, ['U+E0048', 'U+E0069']);
    // The tooltip says which ASCII character each one stands for.
    expect(within(path).getByRole('img', { name: 'TAG CHARACTER FOR "H" (U+E0048)' })).toHaveAttribute('title', 'TAG CHARACTER FOR "H" (U+E0048)');
  });

  it('a lookup value, such as a URL (strict), and its detail (prose)', () => {
    const { card } = renderNode(
      transcript({ tool_executions: [toolCall('WebFetch', { prompt: `summarise ${FAMILY}${RLO}`, url: `https://example.com/${RLO}x` })] }),
    );
    const call = toolCardsOf(card)[0];
    const url = within(call).getByText(/^https:/);
    expect(url.textContent).toBe('https://example.com/⟨U+202E⟩x');
    expect(tokens(url)).toEqual(['U+202E']);
    const detail = within(call).getByText(/^summarise/);
    expect(detail.textContent).toBe(`summarise ${FAMILY}⟨U+202E⟩`);
    expectRevealed(card, call, ['U+202E', 'U+202E']);
  });

  it('the content of a Write (strict), and its path', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Write', { content: `line${RLO}`, file_path: `/repo/n${ZWSP}.txt` })] }));
    const call = toolCardsOf(card)[0];
    expect(within(call).getByRole('region', { name: 'File content' }).textContent).toBe('line⟨U+202E⟩');
    expect(within(call).getByText(/^\/repo\/n/).textContent).toBe('/repo/n⟨U+200B⟩.txt');
    expectRevealed(card, call, ['U+200B', 'U+202E']);
  });

  it('field rows (strict): the key, the value, and a long value in its own block', () => {
    const { card } = renderNode(
      transcript({
        tool_executions: [toolCall('mcp__srv__tool', { [`key${RLO}`]: `value${ZWSP}`, [`long${RLO}`]: `first line\nsecond${BEL}` })],
      }),
    );
    const call = toolCardsOf(card)[0];
    const keys = Array.from(call.querySelectorAll('dt'));
    expect(keys.map((key) => key.textContent)).toEqual(['key⟨U+202E⟩', 'long⟨U+202E⟩']);
    expect(call.querySelector('dd')!.textContent).toBe('value⟨U+200B⟩');
    expect(call.querySelector('pre')!.textContent).toBe('first line\nsecond⟨U+0007⟩');
    expect(tokens(call)).toEqual(['U+202E', 'U+200B', 'U+202E', 'U+0007']);
    expect(call.textContent).not.toContain(BEL);
    expect(noticeOf(card)!.textContent).toBe(NOTICE);
  });

  it('the extras of a tool that is understood (strict)', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Bash', { command: 'ls', [`extra${RLO}`]: 'x' })] }));
    const call = toolCardsOf(card)[0];
    expect(call.querySelector('dt')!.textContent).toBe('extra⟨U+202E⟩');
    expectRevealed(card, call.querySelector('dl') as HTMLElement, ['U+202E']);
  });

  it('arguments that are not JSON (strict)', () => {
    const { card } = renderNode(transcript({ tool_executions: [rawToolCall('Bash', `not json ${RLO}`)] }));
    const arguments_ = within(card).getByRole('region', { name: 'Arguments' });
    expect(arguments_.textContent).toBe('not json ⟨U+202E⟩');
    expectRevealed(card, arguments_, ['U+202E']);
  });

  it('chips made from wire values (strict)', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Grep', { glob: `*.ts${RLO}`, pattern: 'todo' })] }));
    const chip = within(card).getByText(/^glob /);
    expect(chip.textContent).toBe('glob *.ts⟨U+202E⟩');
    expectRevealed(card, chip, ['U+202E']);
  });

  it('todo items (prose): an emoji sequence stays, an override is shown, and so is the active form', () => {
    const { card } = renderNode(
      transcript({
        tool_executions: [
          toolCall('TodoWrite', {
            todos: [
              { activeForm: `Reviewing ${FAMILY}${RLO}`, content: `Review ${FAMILY}`, status: 'in_progress' },
              { content: `Ship${ZWSP}`, status: 'pending' },
            ],
          }),
        ],
      }),
    );
    const items = Array.from(card.querySelectorAll('[data-status] li'));
    expect(items[0].textContent).toContain(`in progress: Review ${FAMILY}`);
    expect(items[0].textContent).toContain(`Reviewing ${FAMILY}⟨U+202E⟩`);
    expect(tokens(items[0])).toEqual(['U+202E']);
    expect(tokens(items[1])).toEqual(['U+200B']);
    expect(noticeOf(card)!.textContent).toBe(NOTICE);
  });

  it('the prompt of a subagent (prose)', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Agent', { description: 'look', prompt: `List ${FAMILY}${RLO}`, subagent_type: 'Explore' })] }));
    fireEvent.click(within(card).getByRole('button', { name: 'Show subagent prompt' }));
    const prompt = card.querySelector('[data-status] pre') as HTMLElement;
    expect(prompt.textContent).toBe(`List ${FAMILY}⟨U+202E⟩`);
    expectRevealed(card, prompt, ['U+202E']);
  });

  it('the model name (strict)', () => {
    const { card } = renderNode(transcript({ model_name: `claude${RLO}-opus` }));
    const header = card.firstElementChild as HTMLElement;
    expect(within(header).getByText(/^claude/).textContent).toBe('claude⟨U+202E⟩-opus');
    expectRevealed(card, header, ['U+202E']);
  });

  it('the note comes after the header and before what the step shows', () => {
    const { card } = renderNode(transcript({ model_response: `ok${RLO}` }));
    const header = card.firstElementChild as HTMLElement;
    const note = noticeOf(card)!;
    const body = card.querySelector('[data-layout="transcript"]') as HTMLElement;
    expect(header.compareDocumentPosition(note) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(note.compareDocumentPosition(body) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// The triad layout
// ---------------------------------------------------------------------------

describe('triad layout: every surface reveals the characters', () => {
  const triad = (over: Partial<InteractionNode> = {}) => makeNode({ ...mcpNode, ...over });

  it('the Thought (prose)', () => {
    const { card } = renderNode(triad({ user_prompt: `Fix${RLO} it ${FAMILY}` }));
    const thought = within(card).getByText(/^Fix/);
    expect(thought.className).toContain('font-serif');
    expect(thought.textContent).toBe(`Fix⟨U+202E⟩ it ${FAMILY}`);
    expectRevealed(card, thought, ['U+202E']);
  });

  it('the Action lines (strict): a joiner is shown here, where the text is code', () => {
    const { card } = renderNode(triad({ tool_executions: [rawToolCall('Bash', `{"command":"a${ZWJ}b${RLO}"}`)] }));
    const line = within(card).getByText(/^Bash /);
    expect(line.textContent).toBe('Bash {"command":"a⟨U+200D⟩b⟨U+202E⟩"}');
    expect(tokens(line)).toEqual(['U+200D', 'U+202E']);
    expect(noticeOf(card)!.textContent).toBe(NOTICE);
  });

  it('an Action line made of a tool name', () => {
    const { card } = renderNode(triad({ tool_executions: [rawToolCall(`Ba${RLO}sh`, '')] }));
    const line = within(card).getByText(/^Ba/);
    expectRevealed(card, line, ['U+202E']);
  });

  it('the Observation (strict)', () => {
    const { card } = renderNode(triad({ model_response: `built ${ZWJ}ok${BEL}` }));
    const observation = within(card).getByText(/^built/);
    expect(observation.className).toContain('font-mono');
    expect(observation.textContent).toBe('built ⟨U+200D⟩ok⟨U+0007⟩');
    expect(tokens(observation)).toEqual(['U+200D', 'U+0007']);
    expect(noticeOf(card)!.textContent).toBe(NOTICE);
  });

  it('the Decision Factors (prose), once opened', () => {
    const { card } = renderNode(triad({ model_cot: `because${RLO} ${FAMILY}` }));
    expect(noticeOf(card)).toBeNull(); // not drawn yet
    fireEvent.click(within(card).getByRole('button', { name: 'Why?' }));
    const factors = within(card).getByText(/^because/);
    expect(factors.textContent).toBe(`because⟨U+202E⟩ ${FAMILY}`);
    expectRevealed(card, factors, ['U+202E']);
  });

  it('the model name (strict)', () => {
    const { card } = renderNode(triad({ model_name: `agent${ZWSP}` }));
    const header = card.firstElementChild as HTMLElement;
    expect(within(header).getByText(/^agent/).textContent).toBe('agent⟨U+200B⟩');
    expectRevealed(card, header, ['U+200B']);
  });

  it('the commit shown on a link chip (strict), and its label for assistive technology', () => {
    const hash = `ab${RLO}cdef0123456789abcdef0123456789abcdef01`;
    const { card } = renderNode(triad({ artifact_links: [{ interaction_id: mcpNode.id, git_commit_hash: hash, link_type: 'generated' }] }));
    const chip = card.querySelector('[aria-label*="commit"]') as HTMLElement;
    expect(chip.getAttribute('aria-label')).toContain('commit ab⟨U+202E⟩cdef');
    expect(chip.getAttribute('aria-label')).not.toContain(RLO);
    expectRevealed(card, chip, ['U+202E']);
  });

  it('the note comes after the header and before the Thought', () => {
    const { card } = renderNode(triad({ user_prompt: `x${RLO}` }));
    const note = noticeOf(card)!;
    const thought = within(card).getByText('Thought');
    expect(note.compareDocumentPosition(thought) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(card.firstElementChild!.compareDocumentPosition(note) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// Prose and strict
// ---------------------------------------------------------------------------

describe('prose and strict text treat the joiners differently', () => {
  it('leaves emoji and joiner sequences alone where a person or the model wrote the text, with no notice', () => {
    const { card } = renderNode(
      transcript({
        author: 'human',
        user_prompt: `${FAMILY} ${HEART}`,
        model_cot: `${FAMILY} thinking`,
        model_response: `${HEART} done`,
        tool_executions: [toolCall('TodoWrite', { todos: [{ content: `Ship ${FAMILY}`, status: 'pending' }] })],
      }),
    );
    expect(tokens(card)).toEqual([]);
    expect(noticeOf(card)).toBeNull();
    expect(card.textContent).toContain(FAMILY);
    expect(card.textContent).toContain(HEART);
  });

  it('shows the same joiners and selectors as tokens where the text is code', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Bash', { command: `echo ${FAMILY} ${HEART}` })] }));
    const command = within(card).getByRole('region', { name: 'Command' });
    expect(command.textContent).toBe('echo 👨⟨U+200D⟩👩⟨U+200D⟩👧 ❤⟨U+FE0F⟩');
    expect(tokens(command)).toEqual(['U+200D', 'U+200D', 'U+FE0F']);
    expect(noticeOf(card)!.textContent).toBe(NOTICE);
  });

  it('raises the notice for the other characters even next to emoji it leaves alone', () => {
    const { card } = renderNode(transcript({ author: 'human', user_prompt: `${FAMILY}${RLO}` }));
    expect(tokens(card)).toEqual(['U+202E']);
    expect(noticeOf(card)).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// The notice
// ---------------------------------------------------------------------------

describe('the card-level notice', () => {
  it('is one line with the role note, and only one however many characters there are', () => {
    const { card } = renderNode(transcript({ model_response: `a${RLO}b${ZWSP}c`, model_cot: `x${BEL}`, tool_executions: [toolCall('Bash', { command: `ls${RLO}` })] }));
    const notes = within(card).getAllByRole('note');
    expect(notes).toHaveLength(1);
    expect(notes[0].textContent).toBe(NOTICE);
    expect(notes[0].tagName).toBe('P');
    expect(tokens(card)).toHaveLength(4);
  });

  it('is not there when no character is drawn, in either layout', () => {
    for (const node of [transcript({ model_response: 'plain', tool_executions: [toolCall('Bash', { command: 'ls' })] }), mcpNode]) {
      const { card, unmount } = renderNode(node);
      expect(tokens(card)).toEqual([]);
      expect(noticeOf(card)).toBeNull();
      expect(screen.queryByText(NOTICE)).not.toBeInTheDocument();
      unmount();
    }
  });

  it('follows the text on screen: a character in a folded block raises nothing until the block is opened', () => {
    const lines = Array.from({ length: 40 }, (_, index) => (index === 30 ? `line ${index}${RLO}` : `line ${index}`));
    const { card } = renderNode(transcript({ author: 'human', user_prompt: `[tool_result Bash] ${lines.join('\n')}` }));
    expect(tokens(card)).toEqual([]);
    expect(noticeOf(card)).toBeNull();

    fireEvent.click(within(card).getByRole('button', { name: 'Show all 40 lines' }));
    expect(tokens(card)).toEqual(['U+202E']);
    expect(noticeOf(card)!.textContent).toBe(NOTICE);

    fireEvent.click(within(card).getByRole('button', { name: 'Show less' }));
    expect(tokens(card)).toEqual([]);
    expect(noticeOf(card)).toBeNull();
  });

  it('leaves when the step stops drawing the character (for example when its data is replaced)', () => {
    const { rerender, container } = render(<TimelineNode interaction={transcript({ model_response: `a${RLO}` })} />);
    expect(within(container).queryByRole('note')).not.toBeNull();
    rerender(<TimelineNode interaction={transcript({ model_response: 'a' })} />);
    expect(within(container).queryByRole('note')).toBeNull();
    rerender(<TimelineNode interaction={transcript({ model_response: `a${ZWSP}` })} />);
    expect(within(container).queryByRole('note')).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

describe('a token', () => {
  it('says what it is: a visible monospace chip with a name, named for assistive technology', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Bash', { command: `a${RLO}b${ZWSP}c${chr(0x2066)}${chr(0xfeff)}` })] }));
    const command = within(card).getByRole('region', { name: 'Command' });
    const labels = Array.from(command.querySelectorAll('[data-hidden-char]')).map((token) => token.getAttribute('title'));
    expect(labels).toEqual([
      'RIGHT-TO-LEFT OVERRIDE (U+202E)',
      'ZERO WIDTH SPACE (U+200B)',
      'LEFT-TO-RIGHT ISOLATE (U+2066)',
      'ZERO WIDTH NO-BREAK SPACE (BYTE ORDER MARK) (U+FEFF)',
    ]);
    const token = within(command).getByRole('img', { name: 'RIGHT-TO-LEFT OVERRIDE (U+202E)' });
    expect(token.textContent).toBe('⟨U+202E⟩');
    expect(token.className).toContain('font-mono');
    expect(token.className).toContain('bg-warning');
    expect(token.tagName).toBe('SPAN');
  });

  it('is a copy of nothing: the raw character is nowhere in the text of the page', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Bash', { command: `x${RLO}${ZWSP}${chr(0xe0041)}` })] }));
    for (const raw of [RLO, ZWSP, chr(0xe0041)]) expect(card.textContent).not.toContain(raw);
    expect(within(card).getByRole('region', { name: 'Command' }).textContent).toBe('x⟨U+202E⟩⟨U+200B⟩⟨U+E0041⟩');
  });

  it('draws at most 200 of them in one block, then one summary, however many there are', () => {
    const { card } = renderNode(transcript({ tool_executions: [toolCall('Bash', { command: `start${ZWSP.repeat(5000)}end` })] }));
    const command = within(card).getByRole('region', { name: 'Command' });
    expect(command.querySelectorAll('[data-hidden-char]:not([data-hidden-char="more"])')).toHaveLength(200);
    const more = command.querySelector('[data-hidden-char="more"]') as HTMLElement;
    expect(more.textContent).toBe('⟨+4,800 more⟩');
    expect(more).toHaveAttribute('title', '4,800 more hidden characters, left out of this text');
    expect(command.textContent!.startsWith('start⟨U+200B⟩')).toBe(true);
    expect(command.textContent!.endsWith('⟨+4,800 more⟩end')).toBe(true);
    expect(command.textContent).not.toContain(ZWSP);
  });
});

describe('HiddenText on its own', () => {
  it('draws text with nothing in it exactly as a plain string would: no wrapper, no extra element', () => {
    const { container } = render(
      <p>
        <HiddenText text="plain text, 100% ordinary" mode="strict" />
      </p>,
    );
    expect(container.innerHTML).toBe('<p>plain text, 100% ordinary</p>');
    expect(container.firstElementChild!.childNodes).toHaveLength(1);
    expect(container.firstElementChild!.firstChild!.nodeType).toBe(Node.TEXT_NODE);
  });

  it('draws the tokens with no card around it, and is silent about it', () => {
    const { container } = render(
      <p>
        <HiddenText text={`a${RLO}b`} mode="prose" />
      </p>,
    );
    expect(container.querySelectorAll('[data-hidden-char]')).toHaveLength(1);
    expect(screen.queryByRole('note')).toBeNull();
  });

  it('leaves a value that is not text for React to draw, as before', () => {
    const { container } = render(
      <p>
        <HiddenText text={42 as unknown as string} mode="strict" />
      </p>,
    );
    expect(container.innerHTML).toBe('<p>42</p>');
    // ...and to refuse, as before: an object is not something React can draw.
    vi.spyOn(console, 'error').mockImplementation(() => {});
    expect(() => render(<p><HiddenText text={{ family: 'x' } as unknown as string} mode="strict" /></p>)).toThrow();
  });
});

// ---------------------------------------------------------------------------
// Text without any of it
// ---------------------------------------------------------------------------

describe('text without hidden characters draws as it always did', () => {
  const plain = transcript({
    author: 'human',
    user_prompt: 'Fix the build.\n\n[tool_result Bash] ok\ttabbed\r\nwindows line',
    model_cot: 'Some thoughts, with accents: é ü, and CJK: 中文, and an emoji: 😀.',
    model_response: 'Done.',
    tool_executions: [
      toolCall('Bash', { command: 'echo "hello"\nls -la', description: 'list files' }),
      toolCall('Edit', { file_path: '/repo/a.ts', old_string: 'one', new_string: 'two' }),
      toolCall('mcp__srv__tool', { key: 'value', long: 'a\nb' }),
    ],
  });

  it('has no token and no notice', () => {
    const { card } = renderNode(plain);
    expect(tokens(card)).toEqual([]);
    expect(noticeOf(card)).toBeNull();
  });

  it('keeps each block of text as one text node, with nothing around it', () => {
    const { card } = renderNode(plain);
    const command = within(card).getByRole('region', { name: 'Command' });
    expect(command.childNodes).toHaveLength(1);
    expect(command.firstChild!.nodeType).toBe(Node.TEXT_NODE);
    expect(command.textContent).toBe('echo "hello"\nls -la');
    const response = within(card).getByText('Done.');
    expect(response.childNodes).toHaveLength(1);
    expect(response.innerHTML).toBe('Done.');
    for (const line of Array.from(card.querySelectorAll('ins, del'))) {
      expect(line.querySelectorAll('span')).toHaveLength(2); // the sign and the text, as before
    }
  });

  it('draws the triad as it always did too', () => {
    const { card } = renderNode(mcpNode);
    expect(tokens(card)).toEqual([]);
    expect(noticeOf(card)).toBeNull();
    const thought = within(card).getByText(mcpNode.user_prompt!);
    expect(thought.childNodes).toHaveLength(1);
    expect(thought.innerHTML).toBe(mcpNode.user_prompt);
  });
});

// ---------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------

describe('cost on the largest strings', () => {
  const MIB = 1024 * 1024;

  it('draws a mebibyte of tool output made of nothing but hidden characters in a bounded number of elements', () => {
    const output = ZWSP.repeat(MIB);
    const started = performance.now();
    const { card } = renderNode(transcript({ author: 'human', user_prompt: `[tool_result Bash] ${output}` }));
    fireEvent.click(within(card).getByRole('button', { name: /^Show all [\d,]+ characters$/ }));
    const elapsed = performance.now() - started;
    expect(card.querySelectorAll('[data-hidden-char]').length).toBeLessThanOrEqual(201);
    expect(card.querySelectorAll('*').length).toBeLessThan(400);
    expect(card.textContent).not.toContain(ZWSP);
    expect(elapsed).toBeLessThan(5000);
  });

  it('draws a mebibyte of plain tool output with no token and no extra element', () => {
    const output = 'x'.repeat(MIB - 1024);
    const started = performance.now();
    const { card } = renderNode(transcript({ author: 'human', user_prompt: `[tool_result Bash] ${output}` }));
    fireEvent.click(within(card).getByRole('button', { name: /^Show all [\d,]+ characters$/ }));
    expect(performance.now() - started).toBeLessThan(5000);
    expect(tokens(card)).toEqual([]);
    expect((card.querySelector('.rr-thought pre') as HTMLElement).childNodes).toHaveLength(1);
  });

  it('works out each text once: drawing the card again with the same node scans nothing new', () => {
    const node = transcript({ tool_executions: [toolCall('Bash', { command: `x${RLO}`.repeat(1000) })] });
    const { rerender, card } = renderNode(node);
    const before = card.querySelectorAll('[data-hidden-char]').length;
    for (let pass = 0; pass < 5; pass++) rerender(<TimelineNode interaction={node} isSelected={pass % 2 === 0} />);
    expect(card.querySelectorAll('[data-hidden-char]').length).toBe(before);
  });
});
