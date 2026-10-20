import { useMemo, type ReactNode } from 'react';
import { Bot, ExternalLink, User } from 'lucide-react';
import { clsx } from 'clsx';
import { usePaged } from '../../hooks/usePaged';
import { formatTimestamp } from '../../lib/formatTimestamp';
import { describeOmitted, splitOmitted, type OmittedCounts } from '../../lib/toolCalls';
import {
  failedToolCount,
  hasText,
  isWithheldResponse,
  readStimulus,
  viewStimulus,
  withheldFieldBytes,
  type ParentToolCall,
  type StimulusOrigin,
} from '../../lib/transcriptNode';
import type { InteractionNode, ToolExecution } from '../../types/cvc';
import { CardHeader, type CardSelection } from './CardHeader';
import { ClampedText } from './ClampedText';
import { HiddenCharsNotice, HiddenText } from './HiddenText';
import { RENDER_LIMITS } from './limits';
import { ShowMore } from './ShowMore';
import { StimulusPane } from './StimulusPane';
import { ToolCallCard } from './ToolCallCard';
import { BADGE_DANGER, BADGE_NEUTRAL, PANE_LABEL } from './styles';

const FAILURE_CHIP_TITLE =
  'Tool calls that reported an error or have no recorded result, such as an interrupted call. Server-side calls that cvc 0.7.0 recorded as failed without knowing how they ended are not counted.';

/**
 * The header's name for a step. A harness node is written by the harness from a
 * transcript of the AI's work, whoever started it, so for a `human` or `system`
 * author the step is "AI step". Any other author is not something this layout
 * knows how to read, so the header says what the author field says rather than
 * attributing the step to the AI.
 */
function headerLabel(author: InteractionNode['author']): string {
  if (author === 'human' || author === 'system') return 'AI step';
  return author === 'external' ? 'External' : 'Unknown';
}

function HeaderIcon({ author }: { author: InteractionNode['author'] }) {
  if (author === 'human' || author === 'system') return <Bot size={16} className="text-action" />;
  if (author === 'external') return <ExternalLink size={16} className="text-muted" />;
  return <User size={16} className="text-muted" />;
}

interface HeaderProps {
  interaction: InteractionNode;
  origin: StimulusOrigin;
  failedTools: number;
  selection: CardSelection;
}

/**
 * Who spoke is not shown: a transcript node's `author` is the origin of its
 * stimulus (the human, or tool output), not the speaker, and the step itself is
 * the assistant's. The outcome is shown only as far as it is recorded: a count of
 * failed tool calls, never a "green check".
 */
function TranscriptHeader({ interaction, origin, failedTools, selection }: HeaderProps) {
  const modelName = typeof interaction.model_name === 'string' && interaction.model_name !== '' ? interaction.model_name : null;
  return (
    <CardHeader
      selection={selection}
      icon={<HeaderIcon author={interaction.author} />}
      speaker={headerLabel(interaction.author)}
      badges={
        <>
          <span className={BADGE_NEUTRAL}>{origin}</span>{' '}
          {failedTools > 0 && (
            <>
              <span className={BADGE_DANGER} title={FAILURE_CHIP_TITLE}>
                {failedTools === 1 ? '1 tool call failed' : `${failedTools} tool calls failed`}
              </span>{' '}
            </>
          )}
        </>
      }
      time={formatTimestamp(interaction.timestamp)}
      model={modelName ? <HiddenText text={modelName} mode="strict" /> : null}
    />
  );
}

/** The note for a response the capture had to withhold whole: see `isWithheldResponse`. */
const WITHHELD_RESPONSE_NOTE = 'This response was withheld: it could not be scrubbed within the capture limits.';

/**
 * The model's text or reasoning. When the capture could not scrub it, it recorded a note in its place
 * (`[cvc: withheld N bytes that could not be scrubbed]`), and the pane says so in its own words rather
 * than drawing the capture's bracketed sentence as if the model had said it. `note` stands in for the
 * text when the caller knows better still (a response withheld whole).
 */
function TextPane({
  label,
  labelClass,
  text,
  note,
  lines,
  textClass,
}: {
  label: string;
  labelClass: string;
  text: string;
  note?: string;
  lines: 3 | 4 | 6;
  textClass: string;
}) {
  const withheld = withheldFieldBytes(text);
  const said = note ?? (withheld === null ? null : `Withheld: ${withheld.toLocaleString('en-US')} bytes could not be scrubbed.`);
  return (
    <div className="rr-thought min-w-0 rounded-[1.25rem] p-4">
      <div className={clsx(PANE_LABEL, labelClass)}>{label}</div>
      {said === null ? (
        <ClampedText text={text} lines={lines} className={clsx('mt-2', textClass)} />
      ) : (
        <p className="mt-2 text-xs italic text-muted">{said}</p>
      )}
    </div>
  );
}

/**
 * The tool cards of a step, a page at a time. The heading counts the calls the model made, all of them.
 * When the capture left calls or context items out, a note at the end says how many: it follows the last
 * card, so it waits until the cards are all on the page.
 */
function ToolCalls({ tools, omitted }: { tools: readonly ToolExecution[]; omitted: OmittedCounts | null }) {
  const { visible, remaining, showMore } = usePaged(tools.length, RENDER_LIMITS.toolCalls);
  return (
    <div className="rr-code min-w-0 rounded-[1.25rem] p-4">
      <div className={clsx(PANE_LABEL, 'text-action')}>
        Tool calls
        {tools.length > 1 && <span className="ml-1.5 text-muted">{tools.length}</span>}
      </div>
      {tools.length > 0 && (
        <ul className="mt-2 space-y-2">
          {tools.slice(0, visible).map((exec, index) => (
            <li key={index} className="min-w-0">
              <ToolCallCard exec={exec} />
            </li>
          ))}
        </ul>
      )}
      <ShowMore remaining={remaining} page={RENDER_LIMITS.toolCalls} one="tool call" many="tool calls" onShow={showMore} />
      {omitted !== null && remaining === 0 && (
        <p className="mt-2 text-xs italic text-muted">{describeOmitted(omitted)}</p>
      )}
    </div>
  );
}

interface TranscriptNodeBodyProps {
  interaction: InteractionNode;
  /**
   * The tool calls the PREVIOUS step made (name, and whether each failed), or null
   * when that step is not loaded. When given, the stimulus's tool results are checked
   * against those calls. A version 1 stimulus (marked by plain text that tool output can
   * imitate) whose results do not match is shown undivided (see `toolResultsMatchParent`);
   * one whose markers verified (see `readStimulus`) keeps its division, and a result
   * that no call accounts for gets a note.
   */
  parentToolNames?: readonly ParentToolCall[] | null;
  /** What the header needs to select the card and to name it. */
  selection: CardSelection;
  /** Rendered last, inside the same spacing container (the artifact-link chips both layouts share). */
  children?: ReactNode;
}

/**
 * The card contents for a node captured from a Claude Code transcript, read in
 * the order the step happened: what it reacted to, its reasoning, what it said,
 * and what it did.
 */
export function TranscriptNodeBody({ interaction, parentToolNames = null, selection, children }: TranscriptNodeBodyProps) {
  const { id, author, user_prompt: prompt } = interaction;
  // What the stimulus is, in order of trust: split by its version 2 markers when they verify against this
  // node's id; one block of text when they carry tokens that do not; and, only when there are no tokens at
  // all, by the version 1 heuristics (where only a human can interrupt, so elsewhere
  // "[Request interrupted by user" is just output).
  const read = useMemo(() => readStimulus({ id, author, user_prompt: prompt }), [id, author, prompt]);
  const { view, origin } = useMemo(
    () => viewStimulus({ author, user_prompt: prompt }, read, parentToolNames),
    [author, prompt, read, parentToolNames],
  );
  // The capture's note that it left calls out is the last record: it is not a call the model made (see `splitOmitted`).
  const executions = interaction.tool_executions;
  const { calls: tools, omitted } = useMemo(() => splitOmitted(Array.isArray(executions) ? executions : []), [executions]);
  // A step the capture had to withhold whole is recorded with its stimulus and a note in place of the response.
  const withheldResponse = isWithheldResponse(interaction);

  return (
    <>
      <TranscriptHeader interaction={interaction} origin={origin} failedTools={failedToolCount(interaction)} selection={selection} />
      <HiddenCharsNotice />
      <div className="space-y-3" data-layout="transcript">
        <StimulusPane view={view} origin={origin} author={author} />
        {hasText(interaction.model_cot) && (
          <TextPane
            label="Reasoning"
            labelClass="text-muted"
            text={interaction.model_cot}
            lines={4}
            textClass="font-serif text-sm leading-relaxed text-muted"
          />
        )}
        {hasText(interaction.model_response) && (
          <TextPane
            label="Response"
            labelClass="text-action"
            text={interaction.model_response}
            note={withheldResponse ? WITHHELD_RESPONSE_NOTE : undefined}
            lines={6}
            textClass="font-serif text-sm leading-relaxed text-ink"
          />
        )}
        {(tools.length > 0 || omitted !== null) && <ToolCalls tools={tools} omitted={omitted} />}
        {children}
      </div>
    </>
  );
}
