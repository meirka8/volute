import { useId, useMemo, useState, type ReactNode } from 'react';
import { ChevronDown, ChevronRight, CircleAlert } from 'lucide-react';
import { clsx } from 'clsx';
import { usePaged } from '../../hooks/usePaged';
import { toolTitle } from '../../lib/toolCalls';
import {
  groupStimulus,
  toolResultsSummary,
  type StimulusBlock,
  type StimulusOrigin,
  type StimulusSegment,
  type ToolResultSegment,
  type ToolResultsItem,
} from '../../lib/transcriptNode';
import type { InteractionNode } from '../../types/cvc';
import { ClampedText } from './ClampedText';
import { CollapsibleCode } from './CollapsibleCode';
import { HiddenText } from './HiddenText';
import { RENDER_LIMITS } from './limits';
import { ShowMore } from './ShowMore';
import { BADGE_DANGER, PANE_LABEL } from './styles';

/** The line a group of tool results folds behind. */
function SummaryToggle({
  open,
  controls,
  onToggle,
  hasError = false,
  children,
}: {
  open: boolean;
  controls: string;
  onToggle: () => void;
  hasError?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-expanded={open}
      aria-controls={open ? controls : undefined}
      onClick={(event) => {
        event.stopPropagation();
        onToggle();
      }}
      className="flex w-full items-start gap-1.5 rounded-2xl border border-line bg-canvas/80 px-3 py-1.5 text-left text-[11px] font-medium text-action transition-colors hover:bg-surface"
    >
      {open ? (
        <ChevronDown size={12} className="mt-0.5 shrink-0" aria-hidden="true" />
      ) : (
        <ChevronRight size={12} className="mt-0.5 shrink-0" aria-hidden="true" />
      )}
      <span className="min-w-0 break-words">{children}</span>
      {hasError && <CircleAlert size={12} className="mt-0.5 shrink-0 text-danger" aria-hidden="true" />}
    </button>
  );
}

/** Output of unknown origin, in the style tool output has: monospace, in its own box. */
function OutputBox({ children }: { children: ReactNode }) {
  return <div className="min-w-0 rounded-2xl bg-canvas/75 px-3 py-2.5">{children}</div>;
}

function ToolResultItem({ result }: { result: ToolResultSegment }) {
  return (
    <OutputBox>
      <div className="mb-2 flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="min-w-0 break-words font-mono text-xs font-semibold text-ink">
          <HiddenText text={toolTitle(result.tool)} mode="strict" />
        </span>
        {result.isError && <span className={clsx(BADGE_DANGER, 'ml-auto shrink-0')}>error</span>}
      </div>
      {result.text === '' ? (
        <p className="text-xs italic text-muted">No output.</p>
      ) : (
        <CollapsibleCode text={result.text} wrap maxLines={10} label="Tool output" />
      )}
    </OutputBox>
  );
}

/**
 * Text that came after a tool result and is drawn as output, marker and all: it
 * claims to be injected context or a summary, which tool output can imitate (see
 * `groupStimulus`).
 */
function OutputItem({ text }: { text: string }) {
  return (
    <OutputBox>
      <CollapsibleCode text={text} wrap maxLines={10} label="Tool output" />
    </OutputBox>
  );
}

/**
 * Tool output is what the previous step's tools returned, not what this step
 * thought, and it can be long: it stays folded behind a one-line summary unless
 * the caller says to start it open. It draws a page of results at a time.
 */
function ToolResultsGroup({ results, defaultOpen }: { results: ToolResultsItem[]; defaultOpen: boolean }) {
  const [open, setOpen] = useState(defaultOpen);
  const id = useId();
  const summary = useMemo(() => toolResultsSummary(results), [results]);
  const hasError = useMemo(() => results.some((result) => result.kind === 'tool_result' && result.isError), [results]);
  const { visible, remaining, showMore } = usePaged(results.length, RENDER_LIMITS.toolResultsPerGroup);

  return (
    <div className="min-w-0">
      <SummaryToggle open={open} controls={id} onToggle={() => setOpen((value) => !value)} hasError={hasError}>
        <HiddenText text={summary} mode="strict" />
      </SummaryToggle>
      {open && (
        <div id={id} className="mt-2 space-y-2">
          {results.slice(0, visible).map((result, index) =>
            result.kind === 'tool_result' ? (
              <ToolResultItem key={index} result={result} />
            ) : (
              <OutputItem key={index} text={result.text} />
            ),
          )}
          <ShowMore
            remaining={remaining}
            page={RENDER_LIMITS.toolResultsPerGroup}
            one="tool result"
            many="tool results"
            onShow={showMore}
          />
        </div>
      )}
    </div>
  );
}

const UNDIVIDED_NOTE =
  "Shown undivided: the tool results named here don't match the previous step's tool calls, so the markers in this text may not be the capture's own.";

/**
 * The whole stimulus as ONE block of verbatim text, for when its division into
 * parts cannot be trusted (see `toolResultsMatchParent`). The summary line says
 * only "Tool results": the names in the markers are exactly what is in doubt.
 */
function UndividedStimulus({ text, defaultOpen }: { text: string; defaultOpen: boolean }) {
  const [open, setOpen] = useState(defaultOpen);
  const id = useId();
  return (
    <div className="min-w-0">
      <SummaryToggle open={open} controls={id} onToggle={() => setOpen((value) => !value)}>
        Tool results
      </SummaryToggle>
      <p className="mt-2 text-xs italic text-muted">{UNDIVIDED_NOTE}</p>
      {open && (
        <div id={id} className="mt-2">
          <OutputBox>
            <CollapsibleCode text={text} wrap maxLines={10} label="Tool output" />
          </OutputBox>
        </div>
      )}
    </div>
  );
}

/** Injected context (Claude Code's own caveats and skill text) and compaction summaries. */
function NoteBlock({ label, text }: { label: string | null; text: string }) {
  return (
    <div className="min-w-0">
      {label && <div className={clsx(PANE_LABEL, 'text-muted')}>{label}</div>}
      <ClampedText
        text={text}
        lines={3}
        mode="strict"
        className={clsx('font-mono text-xs leading-relaxed text-muted', label && 'mt-1')}
      />
    </div>
  );
}

const BLOCK_LABEL: Partial<Record<StimulusBlock['kind'], StimulusOrigin>> = {
  meta: 'Context',
  compaction: 'Compaction summary',
};

interface StimulusPaneProps {
  segments: readonly StimulusSegment[];
  origin: StimulusOrigin;
  /**
   * Who the node says wrote the stimulus. Plain text is drawn as a human's prose
   * ONLY for `human`; for any other author it is drawn like tool output, so that
   * text that happens to sit in a machine's output cannot pass for a person's.
   * Tool results also start folded only for `system`: for any other author a
   * human's text may follow a tool result with nothing on the wire marking where
   * one ends and the other begins, so it must not sit behind a toggle.
   */
  author: InteractionNode['author'];
  /** The human's words are inside a marker segment; see `promptIsEmbedded`. */
  promptIsEmbedded: boolean;
  /** Draw the stimulus as this one block instead of its segments. */
  undividedText?: string | null;
}

/**
 * What the step reacted to, labelled by where it came from: the human's prompt,
 * the previous step's tool results, a compaction summary, injected context, or
 * nothing recorded. It never carries the step's own reasoning or response. It
 * draws a page of blocks at a time.
 */
export function StimulusPane({ segments, origin, author, promptIsEmbedded, undividedText = null }: StimulusPaneProps) {
  const human = author === 'human';
  const startOpen = author !== 'system';
  const blocks = useMemo(() => groupStimulus(segments), [segments]);
  const { visible, remaining, showMore } = usePaged(blocks.length, RENDER_LIMITS.stimulusBlocks);

  if (undividedText !== null) {
    return (
      <div className="rr-thought min-w-0 rounded-[1.25rem] p-4">
        {human && <div className={clsx(PANE_LABEL, 'mb-2 text-action')}>{origin}</div>}
        <UndividedStimulus text={undividedText} defaultOpen={startOpen} />
      </div>
    );
  }

  if (blocks.length === 1 && blocks[0].kind === 'none') {
    return (
      <div className="rr-thought min-w-0 rounded-[1.25rem] p-4">
        <p className="text-sm italic text-muted">Nothing was recorded for what this step reacted to.</p>
      </div>
    );
  }

  // A pane made only of tool-result groups is labelled by the group's own toggle,
  // unless it has to explain that the human's message is inside one of them.
  const labelled = promptIsEmbedded || blocks.some((block) => block.kind !== 'tool_results');

  return (
    <div className="rr-thought min-w-0 rounded-[1.25rem] p-4">
      {labelled && <div className={clsx(PANE_LABEL, 'text-action')}>{origin}</div>}
      {promptIsEmbedded && (
        <p className="mt-2 text-xs italic text-muted">
          The prompt is inside the text below; the capture does not mark where it starts.
        </p>
      )}
      <div className={clsx('space-y-3', labelled && 'mt-2')}>
        {blocks.slice(0, visible).map((block, index) => {
          switch (block.kind) {
            case 'prompt':
              return human ? (
                <ClampedText
                  key={index}
                  text={block.text}
                  lines={4}
                  className="font-serif text-sm leading-relaxed text-ink"
                />
              ) : (
                <OutputBox key={index}>
                  <CollapsibleCode text={block.text} wrap maxLines={10} label="Unmarked text" />
                </OutputBox>
              );
            case 'meta':
            case 'compaction': {
              const label = BLOCK_LABEL[block.kind] ?? null;
              return <NoteBlock key={index} label={label === origin ? null : label} text={block.text} />;
            }
            case 'tool_results':
              return <ToolResultsGroup key={index} results={block.results} defaultOpen={startOpen} />;
            case 'none':
              return null;
          }
        })}
        <ShowMore remaining={remaining} page={RENDER_LIMITS.stimulusBlocks} one="part" many="parts" onShow={showMore} />
      </div>
    </div>
  );
}
