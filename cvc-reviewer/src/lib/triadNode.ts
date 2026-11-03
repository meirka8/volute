/**
 * What the Thought / Action / Observation layout says for a node (the one written for MCP `commit_thought`
 * records, which also draws VS Code/LSP, `cvc run` and legacy nodes). Pure: no React, no DOM.
 *
 * These were the helpers of `TimelineNode`; they are here so that the search can read a node exactly as the
 * card draws it: what the three panes say, which of it the capture recorded and which is the Reviewer's own
 * wording for there being nothing, and whether the reasoning has a place on the card at all.
 */
import type { InteractionNode } from '../types/cvc';

/** The characters `\s` matches, and the quotes and backtick that wrap a placeholder. */
function isBlank(code: number): boolean {
  return (
    code === 0x20 ||
    (code >= 0x09 && code <= 0x0d) ||
    code === 0xa0 ||
    code === 0x1680 ||
    (code >= 0x2000 && code <= 0x200a) ||
    code === 0x2028 ||
    code === 0x2029 ||
    code === 0x202f ||
    code === 0x205f ||
    code === 0x3000 ||
    code === 0xfeff ||
    code === 0x22 || // "
    code === 0x27 || // '
    code === 0x60 // `
  );
}

/**
 * Check if text has meaningful content (not just whitespace, newlines, quotes). One scan that stops at the first
 * character that counts, so a megabyte of text costs one step.
 */
export function hasContent(text: string | null | undefined): boolean {
  if (!text) return false;
  for (let at = 0; at < text.length; at++) if (!isBlank(text.charCodeAt(at))) return true;
  return false;
}

/** What the Thought pane says, and whether that was recorded (or is the Reviewer's own words for nothing having been). */
export function getThought(interaction: InteractionNode): { text: string; recorded: boolean } {
  if (hasContent(interaction.user_prompt)) {
    return { text: interaction.user_prompt!, recorded: true };
  }

  if (!hasContent(interaction.model_cot) && hasContent(interaction.model_response)) {
    return { text: interaction.model_response!, recorded: true };
  }

  if (hasContent(interaction.model_cot)) {
    return { text: 'No primary prompt was recorded for this AI step.', recorded: false };
  }

  return { text: 'No explicit thought was recorded for this step.', recorded: false };
}

/** What the Observation pane says, and whether that was recorded (the pass / fail lines and the placeholder are the Reviewer's own words). */
export function getObservation(interaction: InteractionNode): { text: string; recorded: boolean } {
  if (hasContent(interaction.model_response)) {
    return { text: interaction.model_response!, recorded: true };
  }

  if (interaction.tool_executions.length > 0) {
    return {
      text: interaction.tool_executions.map((tool) => `${tool.status === 'success' ? 'pass' : 'fail'} ${tool.tool_name}`).join('\n'),
      recorded: false,
    };
  }

  return { text: 'No recorded observation.', recorded: false };
}

/**
 * One line of the Action pane, in the parts it is made of, so that the parts the capture recorded can be
 * told from the Reviewer's words between them.
 *
 * - a tool call: the recorded name of the tool and the arguments as recorded (the JSON text, as it is);
 * - a link, when the step has no call: the word for the kind of link (the Reviewer's), and the first seven
 *   characters of the commit it points at (`full` is the whole hash);
 * - nothing: the Reviewer's words for there being no call and no link.
 */
export type ActionLine =
  | { kind: 'tool'; name: string; args: string }
  | { kind: 'link'; word: string; hash: string; full: string }
  | { kind: 'none'; text: string };

/** The text of a line, as the pane writes it. */
export function actionLineText(line: ActionLine): string {
  switch (line.kind) {
    case 'tool':
      return line.args ? `${line.name} ${line.args}` : line.name;
    case 'link':
      return `${line.word} ${line.hash}`;
    case 'none':
      return line.text;
  }
}

export function getActionLines(interaction: InteractionNode): ActionLine[] {
  if (interaction.tool_executions.length > 0) {
    return interaction.tool_executions.map((tool): ActionLine => ({ kind: 'tool', name: tool.tool_name, args: tool.arguments?.trim() ?? '' }));
  }

  if (interaction.artifact_links.length > 0) {
    return interaction.artifact_links.map(
      (link): ActionLine => ({
        kind: 'link',
        word: link.link_type === 'temporal' ? 'low-confidence temporal link' : link.link_type,
        hash: link.git_commit_hash.substring(0, 7),
        full: link.git_commit_hash,
      }),
    );
  }

  return [{ kind: 'none', text: 'No recorded tool execution.' }];
}

/** Whether the card has a "Why?" for the step: only an AI agent's reasoning is offered, and only when there is some. */
export function showsReasoning(interaction: InteractionNode): boolean {
  return interaction.author === 'agent' && hasContent(interaction.model_cot);
}
