import { useMemo } from 'react';
import { AnimatePresence } from 'framer-motion';
import { Brain, GitPullRequest } from 'lucide-react';
import { TimelineNode } from './TimelineNode';
import type { InteractionNode } from '../../types/cvc';

interface ShadowTimelineProps {
  interactions: InteractionNode[];
  selectedInteractionId?: string | null;
  onSelectInteraction?: (id: string) => void;
  prTitle?: string;
  isLoading?: boolean;
}

export function ShadowTimeline({
  interactions,
  selectedInteractionId,
  onSelectInteraction,
  prTitle,
  isLoading = false,
}: ShadowTimelineProps) {
  // Sort interactions by timestamp (newest first for review flow)
  const sortedInteractions = useMemo(() => {
    return [...interactions].sort((a, b) => b.timestamp - a.timestamp);
  }, [interactions]);

  if (isLoading) {
    return (
      <div className="h-full flex flex-col">
        <TimelineHeader prTitle={prTitle} count={0} />
        <div className="flex-1 flex items-center justify-center">
          <div className="text-center text-muted">
            <div className="animate-pulse">
              <Brain size={32} className="mx-auto mb-2 text-action" />
              <p className="text-sm">Loading cognitive history...</p>
            </div>
          </div>
        </div>
      </div>
    );
  }

  if (interactions.length === 0) {
    return (
      <div className="h-full flex flex-col">
        <TimelineHeader prTitle={prTitle} count={0} />
        <div className="flex-1 flex items-center justify-center p-4">
          <div className="text-center text-muted">
            <Brain size={32} className="mx-auto mb-3 opacity-50" />
            <p className="text-sm font-medium">No cognitive history</p>
            <p className="text-xs mt-1">
              No CVC interactions are linked to this PR's commits
            </p>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col">
      <TimelineHeader prTitle={prTitle} count={interactions.length} />

      <div className="flex-1 overflow-y-auto">
        <AnimatePresence mode="popLayout">
          {sortedInteractions.map((interaction) => (
            <TimelineNode
              key={interaction.id}
              interaction={interaction}
              isSelected={selectedInteractionId === interaction.id}
              onSelect={() => onSelectInteraction?.(interaction.id)}
            />
          ))}
        </AnimatePresence>
      </div>
    </div>
  );
}

function TimelineHeader({ prTitle, count }: { prTitle?: string; count: number }) {
  return (
    <div className="border-b border-line/80 px-4 py-4">
      <div className="flex items-center gap-2 mb-1">
        <Brain size={16} className="text-action" />
        <span className="font-serif text-lg font-semibold text-ink">Shadow Gallery</span>
      </div>

      {prTitle && (
        <div className="mt-1 flex items-center gap-1.5 text-xs text-muted">
          <GitPullRequest size={12} />
          <span className="truncate">{prTitle}</span>
        </div>
      )}

      <div className="mt-2 text-xs text-muted">
        {count} interaction{count !== 1 ? 's' : ''} linked to this PR
      </div>
    </div>
  );
}
