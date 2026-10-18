import { memo, useCallback, useLayoutEffect, useMemo, useRef } from 'react';
import { AnimatePresence } from 'framer-motion';
import { AlertTriangle, Brain, GitPullRequest } from 'lucide-react';
import { parentToolNamesByNode, type ParentToolCall } from '../../lib/transcriptNode';
import { NodeErrorBoundary } from './NodeErrorBoundary';
import { TimelineNode } from './TimelineNode';
import type { InteractionNode } from '../../types/cvc';

interface ShadowTimelineProps {
  interactions: InteractionNode[];
  selectedInteractionId?: string | null;
  onSelectInteraction?: (id: string) => void;
  prTitle?: string;
  isLoading?: boolean;
  /** Shown as a small warning banner, e.g. when history was capped for an older repo. */
  notice?: string;
}

interface TimelineItemProps {
  interaction: InteractionNode;
  isSelected: boolean;
  /** One function for the whole timeline, for as long as it lives; called with this step's id. */
  onSelectInteraction: (id: string) => void;
  parentToolNames: readonly ParentToolCall[] | null;
}

/**
 * One step, guarded and memoised. A function component (not the boundary itself)
 * is what AnimatePresence holds as its keyed child, exactly as when the child was
 * TimelineNode.
 *
 * Every prop is a value that stays the same while its step does: the node, the
 * list of the previous step's calls (made once per set of interactions), and the
 * one select function. So selecting a step redraws the step that lost the
 * selection and the one that gained it, and no other: a card can be thousands of
 * elements, and drawing all of them again for a click on one is what made
 * selection slow.
 */
const TimelineItem = memo(function TimelineItem({
  interaction,
  isSelected,
  onSelectInteraction,
  parentToolNames,
}: TimelineItemProps) {
  const id = interaction.id;
  const onSelect = useCallback(() => onSelectInteraction(id), [onSelectInteraction, id]);
  return (
    <NodeErrorBoundary interaction={interaction}>
      <TimelineNode
        interaction={interaction}
        isSelected={isSelected}
        onSelect={onSelect}
        parentToolNames={parentToolNames}
      />
    </NodeErrorBoundary>
  );
});

export function ShadowTimeline({
  interactions,
  selectedInteractionId,
  onSelectInteraction,
  prTitle,
  isLoading = false,
  notice,
}: ShadowTimelineProps) {
  // Sort interactions by timestamp (newest first for review flow)
  const sortedInteractions = useMemo(() => {
    return [...interactions].sort((a, b) => b.timestamp - a.timestamp);
  }, [interactions]);
  // What each step's previous step called, to check the results the step claims.
  // The arrays inside stay the same objects for as long as `interactions` does.
  const parentToolNames = useMemo(() => parentToolNamesByNode(interactions), [interactions]);
  // The newest callback behind a function that never changes, so a parent that
  // passes a new closure on every render does not redraw every step.
  const latestSelect = useRef(onSelectInteraction);
  useLayoutEffect(() => {
    latestSelect.current = onSelectInteraction;
  }, [onSelectInteraction]);
  const selectInteraction = useCallback((id: string) => latestSelect.current?.(id), []);

  if (isLoading) {
    return (
      <div className="h-full flex flex-col">
        <TimelineHeader prTitle={prTitle} count={0} notice={notice} />
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
        <TimelineHeader prTitle={prTitle} count={0} notice={notice} />
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
      <TimelineHeader prTitle={prTitle} count={interactions.length} notice={notice} />

      <div className="flex-1 overflow-y-auto">
        <AnimatePresence mode="popLayout">
          {sortedInteractions.map((interaction) => (
            <TimelineItem
              key={interaction.id}
              interaction={interaction}
              isSelected={selectedInteractionId === interaction.id}
              onSelectInteraction={selectInteraction}
              parentToolNames={parentToolNames.get(interaction.id) ?? null}
            />
          ))}
        </AnimatePresence>
      </div>
    </div>
  );
}

function TimelineHeader({
  prTitle,
  count,
  notice,
}: {
  prTitle?: string;
  count: number;
  notice?: string;
}) {
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

      {notice && (
        <div className="mt-2 flex items-start gap-1.5 rounded-lg bg-amber-500/10 px-2 py-1.5 text-xs text-amber-600 dark:text-amber-400">
          <AlertTriangle size={12} className="mt-0.5 shrink-0" />
          <span>{notice}</span>
        </div>
      )}
    </div>
  );
}
