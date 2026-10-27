/**
 * What the Thought / Action / Observation layout says (issue #118, revision round: the helpers of the card, moved so that the
 * search reads a step as its card draws it).
 */
import { describe, expect, it } from 'vitest';
import { actionLineText, getActionLines, getObservation, getThought, hasContent, showsReasoning } from './triadNode';
import { generatedLink, makeNode, toolCall } from '../test/fixtures';

describe('hasContent', () => {
  /** What the card used to ask: the text with white space and quotes taken out is not empty. */
  const wasContent = (text: string | null | undefined) => (text ? text.replace(/[\s\n\r\t"'`]/g, '').length > 0 : false);

  it('says what the expression the card used to ask said, for every code point on its own and in a context', () => {
    const wrong: string[] = [];
    for (let codePoint = 0; codePoint <= 0x10ffff; codePoint += codePoint < 0x3100 ? 1 : 7) {
      if (codePoint >= 0xd800 && codePoint <= 0xdfff) continue;
      const character = String.fromCodePoint(codePoint);
      for (const text of [character, `  ${character} `, `"${character}"`]) if (hasContent(text) !== wasContent(text)) wrong.push(`U+${codePoint.toString(16)}`);
    }
    expect(wrong).toEqual([]);
  });

  it('is false for nothing, for blank text and for text of quotes', () => {
    for (const text of [undefined, null, '', ' ', '\n\t', '"', "''", '`"\' \n', '  ﻿']) expect(hasContent(text)).toBe(false);
    for (const text of ['a', ' a ', '"a"', '.', '0', '​']) expect(hasContent(text)).toBe(true);
  });

  it('stops at the first character that counts, so a megabyte of text is one step', () => {
    const started = performance.now();
    expect(hasContent(`x${' '.repeat(5_000_000)}`)).toBe(true);
    expect(performance.now() - started).toBeLessThan(100);
  });
});

describe('the panes', () => {
  it('has the prompt as the thought, and says it was recorded', () => {
    expect(getThought(makeNode({ id: 'a', user_prompt: 'Do it', model_response: 'Done' }))).toEqual({ text: 'Do it', recorded: true });
  });

  it('has the response as the thought when there is no prompt and no reasoning', () => {
    expect(getThought(makeNode({ id: 'a', user_prompt: ' ', model_response: 'Done' }))).toEqual({ text: 'Done', recorded: true });
  });

  it('says in its own words that there is none, and that it was not recorded', () => {
    expect(getThought(makeNode({ id: 'a', model_cot: 'why', model_response: 'x' }))).toEqual({ text: 'No primary prompt was recorded for this AI step.', recorded: false });
    expect(getThought(makeNode({ id: 'a' }))).toEqual({ text: 'No explicit thought was recorded for this step.', recorded: false });
    expect(getObservation(makeNode({ id: 'a' }))).toEqual({ text: 'No recorded observation.', recorded: false });
  });

  it('has the response as the observation, or the outcome of each call in the Reviewer\'s words', () => {
    expect(getObservation(makeNode({ id: 'a', model_response: 'Done' }))).toEqual({ text: 'Done', recorded: true });
    const calls = makeNode({ id: 'a', tool_executions: [toolCall('Bash', {}), { ...toolCall('Read', {}), status: 'failure' }] });
    expect(getObservation(calls)).toEqual({ text: 'pass Bash\nfail Read', recorded: false });
  });

  it('offers the reasoning of an AI agent behind "Why?", and nobody else\'s', () => {
    expect(showsReasoning(makeNode({ id: 'a', author: 'agent', model_cot: 'why' }))).toBe(true);
    expect(showsReasoning(makeNode({ id: 'a', author: 'agent', model_cot: ' ' }))).toBe(false);
    expect(showsReasoning(makeNode({ id: 'a', author: 'human', model_cot: 'why' }))).toBe(false);
    expect(showsReasoning(makeNode({ id: 'a', author: 'agent' }))).toBe(false);
  });
});

describe('the lines of the Action pane', () => {
  it('writes a call as the name that was recorded and the arguments as they were recorded', () => {
    const [line] = getActionLines(makeNode({ id: 'a', tool_executions: [toolCall('Bash', { command: 'ls' })] }));
    expect(line).toEqual({ kind: 'tool', name: 'Bash', args: '{"command":"ls"}' });
    expect(actionLineText(line)).toBe('Bash {"command":"ls"}');
  });

  it('writes a call with no arguments as its name alone', () => {
    const [line] = getActionLines(makeNode({ id: 'a', tool_executions: [{ ...toolCall('Bash', {}), arguments: '  ' }] }));
    expect(actionLineText(line)).toBe('Bash');
  });

  it('writes the links of a step with no call as a word for the kind and the first seven characters of the commit', () => {
    const hash = 'abcdef0123456789abcdef0123456789abcdef01';
    const lines = getActionLines(makeNode({ id: 'a', artifact_links: [generatedLink('a', hash), { ...generatedLink('a', hash), link_type: 'temporal' }] }));
    expect(lines.map(actionLineText)).toEqual(['generated abcdef0', 'low-confidence temporal link abcdef0']);
    expect(lines[0]).toEqual({ kind: 'link', word: 'generated', hash: 'abcdef0', full: hash });
  });

  it('says in its own words that there is nothing, when there is nothing', () => {
    const lines = getActionLines(makeNode({ id: 'a' }));
    expect(lines).toEqual([{ kind: 'none', text: 'No recorded tool execution.' }]);
    expect(actionLineText(lines[0])).toBe('No recorded tool execution.');
  });
});
