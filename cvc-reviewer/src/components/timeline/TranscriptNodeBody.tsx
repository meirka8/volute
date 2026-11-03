import { useMemo, type ReactNode } from 'react';
import { Bot, ExternalLink, User } from 'lucide-react';
import { clsx } from 'clsx';
import { usePaged } from '../../hooks/usePaged';
import { formatTimestamp } from '../../lib/formatTimestamp';
import { toolCallTexts } from '../../lib/timelineSearch';
import {
  failedToolCount,
  hasText,
  parseStimulus,
  promptIsEmbedded,
  stimulusOrigin,
  toolResultsMatchParent,
  type ParentToolCall,
  type StimulusOrigin,
} from '../../lib/transcriptNode';
import type { InteractionNode, ToolExecution } from '../../types/cvc';
import { CardHeader, type CardSelection } from './CardHeader';
import { ClampedText } from './ClampedText';
import { HiddenCharsNotice, HiddenText } from './HiddenText';
import { RENDER_LIMITS } from './limits';
import { Section } from './NodeUiScope';
import { useHiddenMatches } from './searchHighlight';
import { SearchNotes } from './SearchNotes';
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
      model={modelName ? <HiddenText text={modelName} mode="strict" marks={false} /> : null}
    />
  );
}

function TextPane({
  label,
  labelClass,
  text,
  lines,
  textClass,
}: {
  label: string;
  labelClass: string;
  text: string;
  lines: 3 | 4 | 6;
  textClass: string;
}) {
  return (
    <div className="rr-thought min-w-0 rounded-[1.25rem] p-4">
      <div className={clsx(PANE_LABEL, labelClass)}>{label}</div>
      <ClampedText text={text} lines={lines} className={clsx('mt-2', textClass)} />
    </div>
  );
}

/** The tool cards of a step, a page at a time. The heading counts all of them. */
function ToolCalls({ tools }: { tools: ToolExecution[] }) {
  const { visible, remaining, showMore } = usePaged(tools.length, RENDER_LIMITS.toolCalls, 'calls');
  const hiddenMatches = useHiddenMatches(tools, visible, toolCallTexts);
  return (
    <div className="rr-code min-w-0 rounded-[1.25rem] p-4">
      <div className={clsx(PANE_LABEL, 'text-action')}>
        Tool calls
        {tools.length > 1 && <span className="ml-1.5 text-muted">{tools.length}</span>}
      </div>
      <ul className="mt-2 space-y-2">
        {tools.slice(0, visible).map((exec, index) => (
          <li key={index} className="min-w-0">
            <Section name={`call-${index}`}>
              <ToolCallCard exec={exec} />
            </Section>
          </li>
        ))}
      </ul>
      <ShowMore remaining={remaining} page={RENDER_LIMITS.toolCalls} one="tool call" many="tool calls" onShow={showMore} matches={hiddenMatches} />
    </div>
  );
}

interface TranscriptNodeBodyProps {
  interaction: InteractionNode;
  /**
   * The tool calls the PREVIOUS step made (name, and whether each failed), or null
   * when that step is not loaded. When given, the stimulus's tool results must be
   * results of those calls; if they are not, the stimulus is shown undivided (see
   * `toolResultsMatchParent`).
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
  const author = interaction.author;
  // Only a human can interrupt: elsewhere "[Request interrupted by user" is just output.
  const segments = useMemo(
    () => parseStimulus(interaction.user_prompt, { interruptionBoundary: author === 'human' }),
    [interaction.user_prompt, author],
  );
  const origin = stimulusOrigin(interaction, segments);
  const undivided = useMemo(
    () => parentToolNames !== null && !toolResultsMatchParent(segments, parentToolNames),
    [segments, parentToolNames],
  );
  const tools = Array.isArray(interaction.tool_executions) ? interaction.tool_executions : [];

  return (
    <>
      <TranscriptHeader interaction={interaction} origin={origin} failedTools={failedToolCount(interaction)} selection={selection} />
      <HiddenCharsNotice />
      <SearchNotes interaction={interaction} />
      <div className="space-y-3" data-layout="transcript">
        <Section name="stimulus">
          <StimulusPane
            segments={segments}
            origin={origin}
            author={author}
            promptIsEmbedded={!undivided && promptIsEmbedded(interaction, segments)}
            undividedText={undivided && typeof interaction.user_prompt === 'string' ? interaction.user_prompt : null}
          />
        </Section>
        {hasText(interaction.model_cot) && (
          <Section name="reasoning">
            <TextPane
              label="Reasoning"
              labelClass="text-muted"
              text={interaction.model_cot}
              lines={4}
              textClass="font-serif text-sm leading-relaxed text-muted"
            />
          </Section>
        )}
        {hasText(interaction.model_response) && (
          <Section name="response">
            <TextPane
              label="Response"
              labelClass="text-action"
              text={interaction.model_response}
              lines={6}
              textClass="font-serif text-sm leading-relaxed text-ink"
            />
          </Section>
        )}
        {tools.length > 0 && (
          <Section name="tools">
            <ToolCalls tools={tools} />
          </Section>
        )}
        {children}
      </div>
    </>
  );
}
