import { useState, useRef, useEffect, useCallback, type DependencyList } from 'react';
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
      return <User size={16} className="text-[#ededed]" />;
    case 'agent':
      return <Bot size={16} className="text-[#5e6ad2]" />;
    case 'system':
      return <Terminal size={16} className="text-[#f59e0b]" />;
    case 'external':
      return <ExternalLink size={16} className="text-[#888888]" />;
    default:
      return <User size={16} className="text-[#888888]" />;
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

/**
 * Hook that detects whether an element's text is being truncated by CSS line-clamp.
 */
function useIsTruncated(deps: DependencyList) {
  const ref = useRef<HTMLParagraphElement>(null);
  const [isTruncated, setIsTruncated] = useState(false);

  const check = useCallback(() => {
    const el = ref.current;
    if (el) {
      setIsTruncated(el.scrollHeight > el.clientHeight + 1);
    }
  }, []);

  useEffect(() => {
    check();

    const el = ref.current;
    if (!el) return;

    const observer = new ResizeObserver(() => {
      check();
    });

    observer.observe(el);

    return () => {
      observer.disconnect();
    };
  }, [check, ...deps]);

  return { ref, isTruncated };
}

export function TimelineNode({
  interaction,
  isSelected = false,
  onSelect,
}: TimelineNodeProps) {
  const [isReasoningExpanded, setIsReasoningExpanded] = useState(false);
  const [isPromptExpanded, setIsPromptExpanded] = useState(false);
  const [isResponseExpanded, setIsResponseExpanded] = useState(false);

  const hasPrompt = hasContent(interaction.user_prompt);
  const hasReasoning = hasContent(interaction.model_cot);
  const hasResponse = hasContent(interaction.model_response);

  const promptTruncation = useIsTruncated([interaction.user_prompt, isPromptExpanded]);
  const responseTruncation = useIsTruncated([interaction.model_response, isResponseExpanded]);

  return (
    <motion.div
      layout
      initial={{ opacity: 0, x: 20 }}
      animate={{ opacity: 1, x: 0 }}
      exit={{ opacity: 0, x: -20 }}
      transition={{ duration: 0.15 }}
      className={clsx(
        'border-l-2 pl-4 py-3 cursor-pointer transition-colors',
        isSelected
          ? 'border-[#5e6ad2] bg-[#5e6ad2]/10'
          : 'border-[#262626] hover:border-[#555555] hover:bg-[#111111]'
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
      {/* Header: Author + Timestamp */}
      <div className="flex items-center gap-2 mb-2">
        <div className="w-7 h-7 rounded-full bg-[#1c1c1c] flex items-center justify-center">
          {getAuthorIcon(interaction.author)}
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-[#ededed]">
              {getAuthorLabel(interaction.author)}
            </span>
            <span className="text-xs text-[#555555]">
              {formatTimestamp(interaction.timestamp)}
            </span>
          </div>
        </div>
      </div>

      {/* User Prompt */}
      {hasPrompt && (
        <div className="mb-2">
          <p
            ref={promptTruncation.ref}
            className={clsx(
              'text-sm text-[#ededed] whitespace-pre-wrap',
              !isPromptExpanded && 'line-clamp-3'
            )}
          >
            {interaction.user_prompt}
          </p>
          {(promptTruncation.isTruncated || isPromptExpanded) && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                setIsPromptExpanded(!isPromptExpanded);
              }}
              className="mt-1 text-xs text-[#5e6ad2] hover:text-[#7c86e2] transition-colors"
            >
              {isPromptExpanded ? 'Show less' : 'Show more'}
            </button>
          )}
        </div>
      )}

      {/* Model Response Preview */}
      {hasResponse && (
        <div className="mb-2 pl-3 border-l border-[#5e6ad2]/50">
          <p
            ref={responseTruncation.ref}
            className={clsx(
              'text-sm text-[#888888] whitespace-pre-wrap',
              !isResponseExpanded && 'line-clamp-2'
            )}
          >
            {interaction.model_response}
          </p>
          {(responseTruncation.isTruncated || isResponseExpanded) && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                setIsResponseExpanded(!isResponseExpanded);
              }}
              className="mt-1 text-xs text-[#5e6ad2] hover:text-[#7c86e2] transition-colors"
            >
              {isResponseExpanded ? 'Show less' : 'Show more'}
            </button>
          )}
        </div>
      )}

      {/* Chain of Thought (Reasoning) Accordion */}
      {hasReasoning && (
        <div className="mt-3">
          <button
            onClick={(e) => {
              e.stopPropagation();
              setIsReasoningExpanded(!isReasoningExpanded);
            }}
            className="flex items-center gap-1.5 text-xs text-[#5e6ad2] hover:text-[#7c86e2] transition-colors"
          >
            {isReasoningExpanded ? (
              <ChevronDown size={14} />
            ) : (
              <ChevronRight size={14} />
            )}
            <span>View reasoning</span>
          </button>

          <AnimatePresence>
            {isReasoningExpanded && (
              <motion.div
                initial={{ height: 0, opacity: 0 }}
                animate={{ height: 'auto', opacity: 1 }}
                exit={{ height: 0, opacity: 0 }}
                transition={{ duration: 0.15, ease: 'easeInOut' }}
                className="overflow-hidden"
              >
                <div className="mt-2 p-3 bg-[#0d0d0d] rounded border border-[#262626]">
                  <pre className="text-xs text-[#888888] whitespace-pre-wrap font-mono overflow-x-auto">
                    {interaction.model_cot}
                  </pre>
                </div>
              </motion.div>
            )}
          </AnimatePresence>
        </div>
      )}

      {/* Artifact Links */}
      {interaction.artifact_links && interaction.artifact_links.length > 0 && (
        <div className="mt-3 flex flex-wrap gap-1.5">
          {interaction.artifact_links.map((link) => (
            <span
              key={`${link.git_commit_hash}-${link.link_type}`}
              className="inline-flex items-center gap-1 px-2 py-0.5 bg-[#1c1c1c] rounded text-xs text-[#888888]"
            >
              <GitCommit size={10} />
              <span className="font-mono">{link.git_commit_hash.substring(0, 7)}</span>
              <span className="text-[#555555]">({link.link_type})</span>
            </span>
          ))}
        </div>
      )}
    </motion.div>
  );
}
