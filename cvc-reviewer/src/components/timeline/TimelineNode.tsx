import { useCallback, useEffect, useId, useState } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import {
  User,
  Bot,
  Terminal,
  ExternalLink,
  ChevronDown,
  ChevronRight,
  GitCommit
} from 'lucide-react';
import { clsx } from 'clsx';
import { formatTimestamp } from '../../lib/formatTimestamp';
import { hiddenAsText } from '../../lib/hiddenChars';
import { nodeLayout, type ParentToolCall } from '../../lib/transcriptNode';
import type { InteractionNode } from '../../types/cvc';
import { CardHeader, type CardSelection } from './CardHeader';
import { HiddenCharsNotice, HiddenCharsScope, HiddenText } from './HiddenText';
import { TranscriptNodeBody } from './TranscriptNodeBody';

interface TimelineNodeProps {
  interaction: InteractionNode;
  isSelected?: boolean;
  onSelect?: () => void;
  /**
   * The tool calls this node's PREVIOUS step made (name, and whether each failed),
   * or null when that step is not loaded. Used by the transcript layout to check
   * the tool results its stimulus claims; the triad layout ignores it.
   */
  parentToolNames?: readonly ParentToolCall[] | null;
  /** Fade the card in. Set for a step that is new to the timeline, not for one scrolled into view. */
  enter?: boolean;
  /**
   * Where the card is in the list, when the list does not put every step on the page
   * (it is windowed): the card's place among all of them (from 1) and how many there
   * are, so assistive technology can say "3 of 10,000" and not "3 of 12".
   */
  listPosition?: { position: number; size: number };
}

function getAuthorIcon(author: InteractionNode['author']) {
  switch (author) {
    case 'human':
      return <User size={16} className="text-ink" />;
    case 'agent':
      return <Bot size={16} className="text-action" />;
    case 'system':
      return <Terminal size={16} className="text-warning" />;
    case 'external':
      return <ExternalLink size={16} className="text-muted" />;
    default:
      return <User size={16} className="text-muted" />;
  }
}

function getAuthorLabel(author: InteractionNode['author']) {
  switch (author) {
    case 'human':
      return 'User';
    case 'agent':
      return 'AI Agent';
    case 'system':
      return 'System';
    case 'external':
      return 'External';
    default:
      return 'Unknown';
  }
}

/**
 * Check if text has meaningful content (not just whitespace, newlines, quotes).
 */
function hasContent(text: string | null | undefined): boolean {
  if (!text) return false;
  const cleaned = text.replace(/[\s\n\r\t"'`]/g, '');
  return cleaned.length > 0;
}

function getThoughtText(interaction: InteractionNode): string {
  if (hasContent(interaction.user_prompt)) {
    return interaction.user_prompt!;
  }

  if (!hasContent(interaction.model_cot) && hasContent(interaction.model_response)) {
    return interaction.model_response!;
  }

  if (hasContent(interaction.model_cot)) {
    return 'No primary prompt was recorded for this AI step.';
  }

  return 'No explicit thought was recorded for this step.';
}

function getActionLines(interaction: InteractionNode): string[] {
  if (interaction.tool_executions.length > 0) {
    return interaction.tool_executions.map((tool) => {
      const args = tool.arguments?.trim();
      return args ? `${tool.tool_name} ${args}` : tool.tool_name;
    });
  }

  if (interaction.artifact_links.length > 0) {
    return interaction.artifact_links.map(
      (link) =>
        `${link.link_type === 'temporal' ? 'low-confidence temporal link' : link.link_type} ${link.git_commit_hash.substring(0, 7)}`,
    );
  }

  return ['No recorded tool execution.'];
}

function getObservationText(interaction: InteractionNode): string {
  if (hasContent(interaction.model_response)) {
    return interaction.model_response!;
  }

  if (interaction.tool_executions.length > 0) {
    return interaction.tool_executions
      .map((tool) => `${tool.status === 'success' ? 'pass' : 'fail'} ${tool.tool_name}`)
      .join('\n');
  }

  return 'No recorded observation.';
}

function getSignal(interaction: InteractionNode) {
  if (interaction.tool_executions.some((tool) => tool.status === 'failure')) {
    return {
      label: 'Critical alert',
      className: 'border border-danger/20 bg-danger/10 text-danger',
    };
  }

  if (interaction.author === 'agent' && interaction.tool_executions.length === 0) {
    return {
      label: 'Review required',
      className: 'border border-warning/20 bg-warning/10 text-warning',
    };
  }

  return {
    label: 'Green check',
    className: 'border border-success/20 bg-success/10 text-success',
  };
}

function confidence(link: InteractionNode['artifact_links'][number]) {
  const event = link.derivation_event;
  // Every event arrived through a Git publisher. Claimed local origin/evidence is
  // wire data, not independent reviewer verification.
  if (event?.relation === 'rewrite_exact') return { label: 'Publisher-asserted Git rewrite', detail: `Publisher-asserted, unverified Git rewrite from ${event.old_oid?.slice(0, 12) ?? 'an earlier commit'}.`, className: 'border border-dashed border-warning/50 text-warning' };
  if (event?.relation === 'squash_exact') return { label: 'Publisher-asserted squash equivalence', detail: `Publisher-asserted, unverified squash equivalence; source range ${event.range_id?.slice(0, 12) ?? 'recorded'}.`, className: 'border border-dashed border-warning/50 text-warning' };
  if (event) return { label: 'Publisher assertion — unverified', detail: 'This Git-published relationship has not been independently verified by the reviewer.', className: 'border border-dashed border-warning/50 text-warning' };
  switch (link.link_type) {
    case 'temporal': return { label: 'Temporal — lower confidence', detail: 'Inferred from timing rather than exact file context.', className: 'border border-dashed border-line text-muted' };
    case 'rewrite_exact': return { label: 'Original generated', detail: 'Generated from the original CVC link.', className: 'border border-line text-muted' };
    case 'squash_exact': return { label: 'Original generated', detail: 'Generated from the original CVC link.', className: 'border border-line text-muted' };
    case 'verified': return { label: 'User verified', detail: 'Explicitly verified by a user.', className: 'border border-action/40 text-action' };
    default: return { label: 'Original generated', detail: 'Generated from the original CVC link.', className: 'border border-line text-muted' };
  }
}

/**
 * Thought / Action / Observation: the layout written for MCP `commit_thought`
 * records. It also renders VS Code/LSP, `cvc run` and legacy nodes, and must keep
 * doing so exactly as before (see TimelineNode.triad.test.tsx).
 */
function TriadNodeBody({ interaction, selection }: { interaction: InteractionNode; selection: CardSelection }) {
  const [isReasoningExpanded, setIsReasoningExpanded] = useState(false);
  const [isThoughtExpanded, setIsThoughtExpanded] = useState(false);
  const [isObservationExpanded, setIsObservationExpanded] = useState(false);
  const [thoughtElement, setThoughtElement] = useState<HTMLParagraphElement | null>(null);
  const [observationElement, setObservationElement] = useState<HTMLParagraphElement | null>(null);
  const [isThoughtTruncated, setIsThoughtTruncated] = useState(false);
  const [isObservationTruncated, setIsObservationTruncated] = useState(false);

  const hasReasoning = hasContent(interaction.model_cot);
  const showWhyButton = interaction.author === 'agent' && hasReasoning;
  const thoughtText = getThoughtText(interaction);
  const observationText = getObservationText(interaction);
  const actionLines = getActionLines(interaction);
  const signal = getSignal(interaction);

  const thoughtRef = useCallback((node: HTMLParagraphElement | null) => {
    setThoughtElement(node);
  }, []);

  const observationRef = useCallback((node: HTMLParagraphElement | null) => {
    setObservationElement(node);
  }, []);

  useEffect(() => {
    if (!thoughtElement) {
      return;
    }

    const checkTruncation = () => {
      setIsThoughtTruncated(thoughtElement.scrollHeight > thoughtElement.clientHeight + 1);
    };

    const rafId = window.requestAnimationFrame(checkTruncation);
    const observer = new ResizeObserver(checkTruncation);
    observer.observe(thoughtElement);

    return () => {
      window.cancelAnimationFrame(rafId);
      observer.disconnect();
    };
  }, [isThoughtExpanded, thoughtElement, thoughtText]);

  useEffect(() => {
    if (!observationElement) {
      return;
    }

    const checkTruncation = () => {
      setIsObservationTruncated(
        observationElement.scrollHeight > observationElement.clientHeight + 1,
      );
    };

    const rafId = window.requestAnimationFrame(checkTruncation);
    const observer = new ResizeObserver(checkTruncation);
    observer.observe(observationElement);

    return () => {
      window.cancelAnimationFrame(rafId);
      observer.disconnect();
    };
  }, [isObservationExpanded, observationElement, observationText]);

  return (
    <>
      <CardHeader
        selection={selection}
        icon={getAuthorIcon(interaction.author)}
        speaker={getAuthorLabel(interaction.author)}
        badges={
          <>
            <span className={clsx('rounded-full px-2.5 py-1 text-[11px] font-medium', signal.className)}>{signal.label}</span>{' '}
          </>
        }
        time={formatTimestamp(interaction.timestamp)}
        model={interaction.model_name ? <HiddenText text={interaction.model_name} mode="strict" /> : null}
      />

      <HiddenCharsNotice />

      <div className="space-y-3">
        <div className="rr-thought rounded-[1.25rem] p-4">
          <div className="text-[11px] font-semibold uppercase tracking-[0.18em] text-action">Thought</div>
          <p
            ref={thoughtRef}
            className={clsx(
              'mt-2 whitespace-pre-wrap font-serif text-sm leading-relaxed text-ink',
              !isThoughtExpanded && 'line-clamp-4'
            )}
          >
            <HiddenText text={thoughtText} mode="prose" />
          </p>
          {(isThoughtTruncated || isThoughtExpanded) && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                setIsThoughtExpanded(!isThoughtExpanded);
              }}
              className="mt-2 text-xs text-action transition-colors hover:opacity-80"
            >
              {isThoughtExpanded ? 'Show less' : 'Show more'}
            </button>
          )}
        </div>

        <div className="rr-code rounded-[1.25rem] p-4">
          <div className="text-[11px] font-semibold uppercase tracking-[0.18em] text-action">Action</div>
          <div className="mt-2 space-y-2">
            {actionLines.map((line, index) => (
              <div key={`${index}-${line}`} className="rounded-2xl bg-canvas/75 px-3 py-2 font-mono text-xs text-ink">
                <HiddenText text={line} mode="strict" />
              </div>
            ))}
          </div>
        </div>

        <div className="rr-code rounded-[1.25rem] p-4">
          <div className="flex items-center justify-between gap-3">
            <div className="text-[11px] font-semibold uppercase tracking-[0.18em] text-success">Observation</div>
            {showWhyButton && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  setIsReasoningExpanded(!isReasoningExpanded);
                }}
                className="flex items-center gap-1.5 rounded-full border border-line bg-canvas/80 px-3 py-1 text-[11px] font-medium text-action transition-colors hover:bg-surface"
              >
                {isReasoningExpanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                <span>Why?</span>
              </button>
            )}
          </div>
          <p
            ref={observationRef}
            className={clsx(
              'mt-2 whitespace-pre-wrap font-mono text-xs leading-relaxed text-ink',
              !isObservationExpanded && 'line-clamp-4'
            )}
          >
            <HiddenText text={observationText} mode="strict" />
          </p>
          {(isObservationTruncated || isObservationExpanded) && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                setIsObservationExpanded(!isObservationExpanded);
              }}
              className="mt-2 text-xs text-action transition-colors hover:opacity-80"
            >
              {isObservationExpanded ? 'Show less' : 'Show more'}
            </button>
          )}

          <AnimatePresence>
            {showWhyButton && isReasoningExpanded && (
              <motion.div
                initial={{ height: 0, opacity: 0 }}
                animate={{ height: 'auto', opacity: 1 }}
                exit={{ height: 0, opacity: 0 }}
                transition={{ duration: 0.15, ease: 'easeInOut' }}
                className="overflow-hidden"
              >
                <div className="rr-thought mt-3 rounded-[1.25rem] p-3">
                  <div className="mb-2 text-[11px] font-semibold uppercase tracking-[0.18em] text-action">
                    Decision Factors
                  </div>
                  <pre className="overflow-x-auto whitespace-pre-wrap font-mono text-xs leading-relaxed text-muted">
                    <HiddenText
                      text={hasReasoning ? interaction.model_cot! : 'No recorded decision factors for this AI step.'}
                      mode="prose"
                    />
                  </pre>
                </div>
              </motion.div>
            )}
          </AnimatePresence>
        </div>

        <ArtifactLinkChips links={interaction.artifact_links} />
      </div>
    </>
  );
}

/** The commits linked to a node, with how confident the link is. Shared by both layouts. */
function ArtifactLinkChips({ links }: { links: InteractionNode['artifact_links'] }) {
  if (!links || links.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-1.5">
      {links.map((link) => {
        const confidenceInfo = confidence(link);
        return (
          <span
            key={link.derivation_event?.event_id ?? `${link.git_commit_hash}-${link.link_type}-${link.linked_by ?? ''}`}
            className={clsx(
              'inline-flex items-center gap-1 rounded-full bg-canvas/80 px-3 py-1 text-xs text-muted',
              confidenceInfo.className,
            )}
            aria-label={hiddenAsText(
              `${confidenceInfo.label}: commit ${link.git_commit_hash.substring(0, 7)}. ${confidenceInfo.detail}`,
              'strict',
            )}
            title={hiddenAsText(confidenceInfo.detail, 'strict')}
          >
            <GitCommit size={10} />
            <span className="font-mono">
              <HiddenText text={link.git_commit_hash.substring(0, 7)} mode="strict" />
            </span>
            <span className="text-muted/80">
              ({confidenceInfo.label})
            </span>
          </span>
        );
      })}
    </div>
  );
}

/**
 * Whether the user has text selected inside `card`. Dragging across text to copy
 * it ends in a click on the card, which must not also select the node.
 */
function hasSelectionInside(card: HTMLElement): boolean {
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed) return false;
  return (
    (selection.anchorNode !== null && card.contains(selection.anchorNode)) ||
    (selection.focusNode !== null && card.contains(selection.focusNode))
  );
}

/**
 * One step in the timeline: a list item, named by the speaker and the time in its
 * header, that holds the step's contents and, inside them, the controls that open
 * and close its parts.
 *
 * The card is not itself interactive. Selecting it is the job of the header, a real
 * toggle button (see `CardHeader`), which is what Tab reaches and Enter or Space
 * presses; a card that were a button could not hold the other buttons. For the
 * mouse, a click anywhere on the card selects it too, unless the reader was
 * selecting text (dragging across it ends in a click on the card). That click
 * handler is a convenience for pointers, so it needs no key handler and no place in
 * the tab order: everything it does a keyboard user does through the header.
 */
export function TimelineNode({
  interaction,
  isSelected = false,
  onSelect,
  parentToolNames = null,
  enter = false,
  listPosition,
}: TimelineNodeProps) {
  const baseId = useId();
  const selection: CardSelection = { pressed: isSelected, onSelect, speakerId: `${baseId}-speaker`, timeId: `${baseId}-time` };
  return (
    <li
      aria-labelledby={`${selection.speakerId} ${selection.timeId}`}
      aria-posinset={listPosition?.position}
      aria-setsize={listPosition?.size}
      className={clsx(
        'mx-3 my-3 cursor-pointer rounded-[1.5rem] border p-4 transition-colors',
        enter && 'rr-card-enter',
        isSelected
          ? 'rr-selected-card border-action bg-action/10'
          : 'border-line bg-surface/55 hover:border-action/35 hover:bg-canvas/70'
      )}
      onClick={(e) => {
        if (hasSelectionInside(e.currentTarget)) return;
        onSelect?.();
      }}
    >
      <HiddenCharsScope>
        {nodeLayout(interaction) === 'transcript' ? (
          <TranscriptNodeBody interaction={interaction} parentToolNames={parentToolNames} selection={selection}>
            <ArtifactLinkChips links={interaction.artifact_links} />
          </TranscriptNodeBody>
        ) : (
          <TriadNodeBody interaction={interaction} selection={selection} />
        )}
      </HiddenCharsScope>
    </li>
  );
}
