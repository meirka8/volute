import { useCallback, useEffect, useState } from 'react';
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
import type { InteractionNode } from '../../types/cvc';

interface TimelineNodeProps {
  interaction: InteractionNode;
  isSelected?: boolean;
  onSelect?: () => void;
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

function formatTimestamp(timestamp: number): string {
  const date = new Date(timestamp);
  return date.toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
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
  return (
    interaction.user_prompt ||
    interaction.model_cot ||
    interaction.model_response ||
    'No explicit thought was recorded for this step.'
  );
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
      (link) => `${link.link_type} ${link.git_commit_hash.substring(0, 7)}`,
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

export function TimelineNode({
  interaction,
  isSelected = false,
  onSelect,
}: TimelineNodeProps) {
  const [isReasoningExpanded, setIsReasoningExpanded] = useState(false);
  const [isThoughtExpanded, setIsThoughtExpanded] = useState(false);
  const [isObservationExpanded, setIsObservationExpanded] = useState(false);
  const [thoughtElement, setThoughtElement] = useState<HTMLParagraphElement | null>(null);
  const [observationElement, setObservationElement] = useState<HTMLParagraphElement | null>(null);
  const [isThoughtTruncated, setIsThoughtTruncated] = useState(false);
  const [isObservationTruncated, setIsObservationTruncated] = useState(false);

  const hasReasoning = hasContent(interaction.model_cot);
  const showWhyButton = interaction.author === 'agent';
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
    <motion.div
      layout
      initial={{ opacity: 0, x: 20 }}
      animate={{ opacity: 1, x: 0 }}
      exit={{ opacity: 0, x: -20 }}
      transition={{ duration: 0.15 }}
        className={clsx(
        'mx-3 my-3 cursor-pointer rounded-[1.5rem] border p-4 transition-colors',
        isSelected
          ? 'rr-selected-card border-action bg-action/10'
          : 'border-line bg-surface/55 hover:border-action/35 hover:bg-canvas/70'
      )}
      onClick={onSelect}
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onSelect?.();
        }
      }}
    >
      <div className="mb-4 flex items-start gap-3">
        <div className="flex h-9 w-9 items-center justify-center rounded-full bg-canvas/80">
          {getAuthorIcon(interaction.author)}
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium text-ink">
              {getAuthorLabel(interaction.author)}
            </span>
            <span className={clsx('rounded-full px-2.5 py-1 text-[11px] font-medium', signal.className)}>
              {signal.label}
            </span>
            <span className="text-xs text-muted">
              {formatTimestamp(interaction.timestamp)}
            </span>
          </div>
          {interaction.model_name && (
            <div className="mt-1 text-xs uppercase tracking-[0.16em] text-muted">
              {interaction.model_name}
            </div>
          )}
        </div>
      </div>

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
            {thoughtText}
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
                {line}
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
            {observationText}
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
                    {hasReasoning ? interaction.model_cot : 'No recorded decision factors for this AI step.'}
                  </pre>
                </div>
              </motion.div>
            )}
          </AnimatePresence>
        </div>

        {interaction.artifact_links && interaction.artifact_links.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
          {interaction.artifact_links.map((link) => (
            <span
              key={`${link.git_commit_hash}-${link.link_type}`}
              className="inline-flex items-center gap-1 rounded-full bg-canvas/80 px-3 py-1 text-xs text-muted"
            >
              <GitCommit size={10} />
              <span className="font-mono">{link.git_commit_hash.substring(0, 7)}</span>
              <span className="text-muted/80">({link.link_type})</span>
            </span>
          ))}
          </div>
        )}
      </div>
    </motion.div>
  );
}
