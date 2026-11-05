import { useId, useMemo, type ReactNode } from 'react';
import { ChevronDown, ChevronRight, CircleAlert } from 'lucide-react';
import { clsx } from 'clsx';
import { usePaged } from '../../hooks/usePaged';
import type { ContentPiece } from '../../lib/stimulusV2';
import { toolTitle } from '../../lib/toolCalls';
import {
  groupStimulus,
  toolResultsSummary,
  type ResultsGroupItem,
  type StimulusBlock,
  type StimulusOrigin,
  type StimulusSegment,
  type StimulusView,
  type ToolResultSegment,
  type VerifiedBlock,
  type VerifiedToolResult,
} from '../../lib/transcriptNode';
import type { InteractionNode } from '../../types/cvc';
import { ClampedText } from './ClampedText';
import { CollapsibleCode } from './CollapsibleCode';
import { HiddenText } from './HiddenText';
import { RENDER_LIMITS } from './limits';
import { usePersistentToggle } from './nodeUiState';
import { Section } from './NodeUiScope';
import { matchesLabel, useHiddenMatches, useMatchCount } from './searchHighlight';
import { ShowMore } from './ShowMore';
import { BADGE_DANGER, PANE_LABEL } from './styles';

/** A count as the notes say it: "4,436". */
const count = (value: bigint) => value.toLocaleString('en-US');

/**
 * A note the Reviewer wrote about how the capture recorded something, as opposed to text the capture
 * recorded. Never in brackets: bracketed text in this layout means "recorded, but not kept".
 */
function RecordNote({ children, className }: { children: ReactNode; className?: string }) {
  return <p className={clsx('text-xs italic text-muted', className)}>{children}</p>;
}

const NO_TEXTS: readonly string[] = [];

/**
 * What a piece of a verified part holds that a search can match: its text. The notes the capture left
 * where it cut a part or could not keep it are drawn in the Reviewer's own words, and hold nothing.
 */
const pieceTexts = (piece: ContentPiece): readonly string[] => (piece.kind === 'text' ? [piece.text] : NO_TEXTS);

/** What a tool result (or the output drawn like one) holds that a search can match. */
const resultTexts = (result: ResultsGroupItem): string | readonly string[] => {
  switch (result.kind) {
    case 'tool_result':
      return [toolTitle(result.tool), result.text];
    case 'verified_tool_result':
      return [toolTitle(result.tool), ...result.content.flatMap(pieceTexts)];
    case 'output':
      return result.text;
  }
};

/** What a block of a version 1 stimulus holds that a search can match. */
const blockTexts = (block: StimulusBlock): string | readonly string[] =>
  block.kind === 'tool_results' ? block.results.flatMap(resultTexts) : block.kind === 'none' ? '' : block.text;

/** What a block of a verified stimulus holds that a search can match. */
const verifiedBlockTexts = (block: VerifiedBlock): string | readonly string[] =>
  block.kind === 'tool_results' ? block.results.flatMap(resultTexts) : block.content.flatMap(pieceTexts);

/** The line a group of tool results folds behind. */
function SummaryToggle({
  open,
  controls,
  onToggle,
  hasError = false,
  matches = 0,
  children,
}: {
  open: boolean;
  controls: string;
  onToggle: () => void;
  hasError?: boolean;
  /** While the timeline is searched: how many matches the folded results hold, said while they are folded. */
  matches?: number;
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
      <span className="min-w-0 break-words">
        {children}
        {!open && matches > 0 && matchesLabel(matches)}
      </span>
      {hasError && <CircleAlert size={12} className="mt-0.5 shrink-0 text-danger" aria-hidden="true" />}
    </button>
  );
}

/** Output of unknown origin, in the style tool output has: monospace, in its own box. */
function OutputBox({ children }: { children: ReactNode }) {
  return <div className="min-w-0 rounded-2xl bg-canvas/75 px-3 py-2.5">{children}</div>;
}

/** The name of a tool result, and its error badge. */
function ToolResultHeader({ tool, isError }: { tool: string; isError: boolean }) {
  return (
    <div className="mb-2 flex flex-wrap items-center gap-x-2 gap-y-1">
      <span className="min-w-0 break-words font-mono text-xs font-semibold text-ink">
        <HiddenText text={toolTitle(tool)} mode="strict" />
      </span>
      {isError && <span className={clsx(BADGE_DANGER, 'ml-auto shrink-0')}>error</span>}
    </div>
  );
}

function ToolResultItem({ result }: { result: ToolResultSegment }) {
  return (
    <OutputBox>
      <ToolResultHeader tool={result.tool} isError={result.isError} />
      {result.text === '' ? (
        <RecordNote>No output.</RecordNote>
      ) : (
        <CollapsibleCode text={result.text} wrap maxLines={10} label="Tool output" />
      )}
    </OutputBox>
  );
}

/**
 * What a part of a verified stimulus holds: its text, drawn by `renderText`, and, where the capture
 * cut it or could not keep it, a note in the Reviewer's own words. The token the capture's note
 * carries is never drawn.
 */
function ContentPieces({
  content,
  renderText,
}: {
  content: readonly ContentPiece[];
  renderText: (text: string) => ReactNode;
}) {
  // A capture writes three pieces at most for a part; a crafted one can hold thousands, which draw a page at a time.
  const { visible, remaining, showMore } = usePaged(content.length, RENDER_LIMITS.contentPieces, 'pieces');
  const hiddenMatches = useHiddenMatches(content, visible, pieceTexts);
  return (
    <div className="min-w-0 space-y-2">
      {content.slice(0, visible).map((piece, index) => {
        switch (piece.kind) {
          case 'text':
            return (
              <Section key={index} name={`piece-${index}`}>
                <div className="min-w-0">{renderText(piece.text)}</div>
              </Section>
            );
          case 'truncated':
            return <RecordNote key={index}>{`… ${count(piece.bytes)} bytes truncated`}</RecordNote>;
          case 'withheld':
            return <RecordNote key={index}>Withheld: this part could not be scrubbed</RecordNote>;
        }
      })}
      <ShowMore remaining={remaining} page={RENDER_LIMITS.contentPieces} one="piece" many="pieces" onShow={showMore} matches={hiddenMatches} />
    </div>
  );
}

const UNMATCHED_NOTE = 'No tool call in the previous step matches this result.';

/**
 * A tool result of a stimulus whose markers verified. Its name is the capture's own, so the result
 * keeps its place; when the previous step is loaded and has no call that accounts for it, a note says so.
 */
function VerifiedToolResultItem({ result }: { result: VerifiedToolResult }) {
  return (
    <OutputBox>
      <ToolResultHeader tool={result.tool} isError={result.isError} />
      {result.unmatched && <RecordNote className="mb-2">{UNMATCHED_NOTE}</RecordNote>}
      {result.content.length === 0 ? (
        <RecordNote>No output.</RecordNote>
      ) : (
        <ContentPieces
          content={result.content}
          renderText={(text) => <CollapsibleCode text={text} wrap maxLines={10} label="Tool output" />}
        />
      )}
    </OutputBox>
  );
}

/**
 * Text that came after a tool result and is drawn as output, marker and all: it claims to be injected
 * context or a summary, which tool output can imitate (see `groupStimulus`).
 */
function OutputItem({ text }: { text: string }) {
  return (
    <OutputBox>
      <CollapsibleCode text={text} wrap maxLines={10} label="Tool output" />
    </OutputBox>
  );
}

/** One result of a group, as the group draws it. */
function ResultItem({ result }: { result: ResultsGroupItem }) {
  switch (result.kind) {
    case 'tool_result':
      return <ToolResultItem result={result} />;
    case 'verified_tool_result':
      return <VerifiedToolResultItem result={result} />;
    case 'output':
      return <OutputItem text={result.text} />;
  }
}

/**
 * Tool output is what the previous step's tools returned, not what this step
 * thought, and it can be long: it stays folded behind a one-line summary unless
 * the caller says to start it open. It draws a page of results at a time.
 */
function ToolResultsGroup({ results, defaultOpen }: { results: readonly ResultsGroupItem[]; defaultOpen: boolean }) {
  const [open, setOpen] = usePersistentToggle('open', defaultOpen);
  const id = useId();
  const summary = useMemo(() => toolResultsSummary(results), [results]);
  const hasError = useMemo(() => results.some((result) => result.kind !== 'output' && result.isError), [results]);
  const { visible, remaining, showMore } = usePaged(results.length, RENDER_LIMITS.toolResultsPerGroup, 'results');
  // What the folded results hold that the search matches: their names and their output.
  const texts = useMemo(() => results.flatMap(resultTexts), [results]);
  const matches = useMatchCount(texts, !open);
  const hiddenMatches = useHiddenMatches(results, visible, resultTexts);

  return (
    <div className="min-w-0">
      <SummaryToggle open={open} controls={id} onToggle={() => setOpen((value) => !value)} hasError={hasError} matches={matches}>
        <HiddenText text={summary} mode="strict" marks={false} />
      </SummaryToggle>
      {open && (
        <div id={id} className="mt-2 space-y-2">
          {results.slice(0, visible).map((result, index) => (
            <Section key={index} name={`result-${index}`}>
              <ResultItem result={result} />
            </Section>
          ))}
          <ShowMore
            remaining={remaining}
            page={RENDER_LIMITS.toolResultsPerGroup}
            one="tool result"
            many="tool results"
            onShow={showMore}
            matches={hiddenMatches}
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
  const [open, setOpen] = usePersistentToggle('open', defaultOpen);
  const id = useId();
  const matches = useMatchCount(text, !open);
  return (
    <div className="min-w-0">
      <SummaryToggle open={open} controls={id} onToggle={() => setOpen((value) => !value)} matches={matches}>
        Tool results
      </SummaryToggle>
      <p className="mt-2 text-xs italic text-muted">{UNDIVIDED_NOTE}</p>
      {open && (
        <div id={id} className="mt-2">
          <OutputBox>
            <Section name="text">
              <CollapsibleCode text={text} wrap maxLines={10} label="Tool output" />
            </Section>
          </OutputBox>
        </div>
      )}
    </div>
  );
}

const UNVERIFIED_NOTE = "Shown undivided: this step's markers could not be verified.";

/**
 * A stimulus that carries version 2 boundary tokens that do not verify: damaged, or written for some
 * other node. Nothing in it can be trusted to say where a person's words end and tool output begins,
 * so it is ONE block of verbatim monospace text, never a prompt and never split by the version 1
 * heuristics, whoever the node says wrote it. It folds as a step's tool results do (see `StimulusPane`).
 */
function UnverifiedStimulus({ text, defaultOpen }: { text: string; defaultOpen: boolean }) {
  const [open, setOpen] = usePersistentToggle('open', defaultOpen);
  const id = useId();
  const matches = useMatchCount(text, !open);
  return (
    <div className="rr-thought min-w-0 rounded-[1.25rem] p-4">
      <div className="min-w-0">
        <SummaryToggle open={open} controls={id} onToggle={() => setOpen((value) => !value)} matches={matches}>
          Stimulus text
        </SummaryToggle>
        <RecordNote className="mt-2">{UNVERIFIED_NOTE}</RecordNote>
        {open && (
          <div id={id} className="mt-2">
            <OutputBox>
              <Section name="text">
                <CollapsibleCode text={text} wrap maxLines={10} label="Stimulus text" />
              </Section>
            </OutputBox>
          </div>
        )}
      </div>
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

/** What a person wrote, as prose. */
const PROSE_CLASS = 'font-serif text-sm leading-relaxed text-ink';

const TASK_NOTE = 'Sent by the agent that delegated this work, not typed by a person.';

/**
 * One block of a verified stimulus. A person's words are prose; a task is what an agent told a
 * subagent, and is labelled so, since no person typed it. Notes the capture left where it cut a part
 * are drawn as the Reviewer's own muted notes.
 */
function VerifiedBlockView({ block, origin, startOpen }: { block: VerifiedBlock; origin: StimulusOrigin; startOpen: boolean }) {
  switch (block.kind) {
    case 'prompt':
      return (
        <ContentPieces
          content={block.content}
          renderText={(text) => <ClampedText text={text} lines={4} className={PROSE_CLASS} />}
        />
      );
    case 'task':
      return (
        <div className="min-w-0">
          {origin !== 'Task' && <div className={clsx(PANE_LABEL, 'text-muted')}>Task</div>}
          <RecordNote className={clsx(origin !== 'Task' && 'mt-1')}>{TASK_NOTE}</RecordNote>
          <div className="mt-2">
            <ContentPieces
              content={block.content}
              renderText={(text) => <ClampedText text={text} lines={4} className={PROSE_CLASS} />}
            />
          </div>
        </div>
      );
    case 'meta':
    case 'compaction': {
      const label = BLOCK_LABEL[block.kind] ?? null;
      const shown = label === origin ? null : label;
      return (
        <div className="min-w-0">
          {shown && <div className={clsx(PANE_LABEL, 'text-muted')}>{shown}</div>}
          <ContentPieces
            content={block.content}
            renderText={(text) => (
              <ClampedText
                text={text}
                lines={3}
                mode="strict"
                className={clsx('font-mono text-xs leading-relaxed text-muted', shown && 'mt-1')}
              />
            )}
          />
        </div>
      );
    }
    case 'tool_results':
      return <ToolResultsGroup results={block.results} defaultOpen={startOpen} />;
  }
}

const NOTHING_RECORDED = 'Nothing was recorded for what this step reacted to.';

/** The pane of a stimulus whose markers verified: what the capture marked, part by part. */
function VerifiedStimulus({
  blocks,
  omittedParts,
  origin,
  startOpen,
}: {
  blocks: readonly VerifiedBlock[];
  omittedParts: bigint | null;
  origin: StimulusOrigin;
  startOpen: boolean;
}) {
  const { visible, remaining, showMore } = usePaged(blocks.length, RENDER_LIMITS.stimulusBlocks, 'blocks');
  const hiddenMatches = useHiddenMatches(blocks, visible, verifiedBlockTexts);

  if (blocks.length === 0) {
    return (
      <div className="rr-thought min-w-0 rounded-[1.25rem] p-4">
        <p className="text-sm italic text-muted">{NOTHING_RECORDED}</p>
      </div>
    );
  }

  // As for version 1: a pane of nothing but tool results is labelled by the group's own toggle.
  const labelled = blocks.some((block) => block.kind !== 'tool_results');
  return (
    <div className="rr-thought min-w-0 rounded-[1.25rem] p-4">
      {labelled && <div className={clsx(PANE_LABEL, 'text-action')}>{origin}</div>}
      <div className={clsx('space-y-3', labelled && 'mt-2')}>
        {blocks.slice(0, visible).map((block, index) => (
          <Section key={index} name={`block-${index}`}>
            <VerifiedBlockView block={block} origin={origin} startOpen={startOpen} />
          </Section>
        ))}
        <ShowMore remaining={remaining} page={RENDER_LIMITS.stimulusBlocks} one="part" many="parts" onShow={showMore} matches={hiddenMatches} />
        {/* The closing note is about what follows the last part drawn, so it waits for the last one. */}
        {remaining === 0 && omittedParts !== null && (
          <RecordNote>{`${count(omittedParts)} more ${omittedParts === 1n ? 'part' : 'parts'} not recorded`}</RecordNote>
        )}
      </div>
    </div>
  );
}

interface SegmentsStimulusProps {
  segments: readonly StimulusSegment[];
  origin: StimulusOrigin;
  author: InteractionNode['author'];
  promptIsEmbedded: boolean;
}

/** A version 1 stimulus, split by its markers. */
function SegmentsStimulus({ segments, origin, author, promptIsEmbedded }: SegmentsStimulusProps) {
  const human = author === 'human';
  const startOpen = author !== 'system';
  const blocks = useMemo(() => groupStimulus(segments), [segments]);
  const { visible, remaining, showMore } = usePaged(blocks.length, RENDER_LIMITS.stimulusBlocks, 'blocks');
  const hiddenMatches = useHiddenMatches(blocks, visible, blockTexts);

  if (blocks.length === 1 && blocks[0].kind === 'none') {
    return (
      <div className="rr-thought min-w-0 rounded-[1.25rem] p-4">
        <p className="text-sm italic text-muted">{NOTHING_RECORDED}</p>
      </div>
    );
  }

  // A pane made only of tool-result groups is labelled by the group's own toggle,
  // unless it has to explain that the human's message is inside one of them.
  const labelled = promptIsEmbedded || blocks.some((block) => block.kind !== 'tool_results');

  const drawBlock = (block: StimulusBlock): ReactNode => {
    switch (block.kind) {
      case 'prompt':
        return human ? (
          <ClampedText text={block.text} lines={4} className="font-serif text-sm leading-relaxed text-ink" />
        ) : (
          <OutputBox>
            <CollapsibleCode text={block.text} wrap maxLines={10} label="Unmarked text" />
          </OutputBox>
        );
      case 'meta':
      case 'compaction': {
        const label = BLOCK_LABEL[block.kind] ?? null;
        return <NoteBlock label={label === origin ? null : label} text={block.text} />;
      }
      case 'tool_results':
        return <ToolResultsGroup results={block.results} defaultOpen={startOpen} />;
      case 'none':
        return null;
    }
  };

  return (
    <div className="rr-thought min-w-0 rounded-[1.25rem] p-4">
      {labelled && <div className={clsx(PANE_LABEL, 'text-action')}>{origin}</div>}
      {promptIsEmbedded && (
        <p className="mt-2 text-xs italic text-muted">
          The prompt is inside the text below; the capture does not mark where it starts.
        </p>
      )}
      <div className={clsx('space-y-3', labelled && 'mt-2')}>
        {blocks.slice(0, visible).map((block, index) => (
          <Section key={index} name={`block-${index}`}>
            {drawBlock(block)}
          </Section>
        ))}
        <ShowMore remaining={remaining} page={RENDER_LIMITS.stimulusBlocks} one="part" many="parts" onShow={showMore} matches={hiddenMatches} />
      </div>
    </div>
  );
}

interface StimulusPaneProps {
  /** What to draw: see `viewStimulus`. */
  view: StimulusView;
  /** The label for what the step reacted to; see `viewStimulus`. */
  origin: StimulusOrigin;
  /**
   * Who the node says wrote the stimulus. In a version 1 stimulus plain text is drawn as a human's
   * prose ONLY for `human`; for any other author it is drawn like tool output, so that text that
   * happens to sit in a machine's output cannot pass for a person's. Tool results also start folded
   * only for `system`: for any other author a human's text may follow a tool result with nothing on
   * the wire marking where one ends and the other begins, so it must not sit behind a toggle. (A
   * stimulus whose markers verified marks where each part ends, and says what each part is itself;
   * the folding rule is the same, and a person's text is prose only as a `prompt` part.)
   */
  author: InteractionNode['author'];
}

/**
 * What the step reacted to, labelled by where it came from: the human's prompt,
 * a subagent's task, the previous step's tool results, a compaction summary, injected
 * context, or nothing recorded. It never carries the step's own reasoning or response. It
 * draws a page of blocks at a time.
 */
export function StimulusPane({ view, origin, author }: StimulusPaneProps) {
  const startOpen = author !== 'system';
  switch (view.kind) {
    case 'segments':
      return <SegmentsStimulus segments={view.segments} origin={origin} author={author} promptIsEmbedded={view.promptIsEmbedded} />;
    case 'undivided':
      return (
        <div className="rr-thought min-w-0 rounded-[1.25rem] p-4">
          {author === 'human' && <div className={clsx(PANE_LABEL, 'mb-2 text-action')}>{origin}</div>}
          <UndividedStimulus text={view.text} defaultOpen={startOpen} />
        </div>
      );
    case 'unverified':
      return <UnverifiedStimulus text={view.text} defaultOpen={startOpen} />;
    case 'verified':
      return <VerifiedStimulus blocks={view.blocks} omittedParts={view.omittedParts} origin={origin} startOpen={startOpen} />;
  }
}
