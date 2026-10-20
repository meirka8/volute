import { memo, useCallback, useLayoutEffect, useMemo, useRef, useState, type RefObject } from 'react';
import { AlertTriangle, Brain, GitPullRequest } from 'lucide-react';
import { useWindowedList } from '../../hooks/useWindowedList';
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
  /** The steps the timeline has drawn before. A card for one of those is not animated in again. */
  seen: RefObject<ReadonlySet<string>>;
  /** Where this step is among all of them (from 1) and how many there are, when the list is windowed. */
  position?: number;
  setSize?: number;
}

/**
 * One step, guarded and memoised.
 *
 * Every prop is a value that stays the same while its step does: the node, the
 * list of the previous step's calls (made once per set of interactions), the one
 * select function, and the set of steps already seen. So selecting a step redraws
 * the step that lost the selection and the one that gained it, and no other: a card
 * can be thousands of elements, and drawing all of them again for a click on one is
 * what made selection slow.
 */
const TimelineItem = memo(function TimelineItem({
  interaction,
  isSelected,
  onSelectInteraction,
  parentToolNames,
  seen,
  position,
  setSize,
}: TimelineItemProps) {
  const id = interaction.id;
  const onSelect = useCallback(() => onSelectInteraction(id), [onSelectInteraction, id]);
  const listPosition = useMemo(
    () => (position === undefined || setSize === undefined ? undefined : { position, size: setSize }),
    [position, setSize],
  );
  // A card plays its entrance when its step is new to the timeline. One that a scroll brings
  // back into the window is not arriving, and would otherwise flicker in as the reader scrolls.
  const [enter] = useState(() => !seen.current.has(id));
  return (
    <NodeErrorBoundary interaction={interaction} listPosition={listPosition}>
      <TimelineNode
        interaction={interaction}
        isSelected={isSelected}
        onSelect={onSelect}
        parentToolNames={parentToolNames}
        enter={enter}
        listPosition={listPosition}
      />
    </NodeErrorBoundary>
  );
});

const NO_STEPS: ReadonlySet<string> = new Set();

interface TimelineListProps {
  /** Every step, newest first. */
  interactions: readonly InteractionNode[];
  parentToolNames: ReadonlyMap<string, readonly ParentToolCall[] | null>;
  selectedInteractionId?: string | null;
  onSelectInteraction: (id: string) => void;
}

/**
 * The scrolling list of steps. A long one is windowed (see `useWindowedList`): only
 * the cards near the viewport are mounted, with padding as tall as the cards left
 * out above and below them, so the scrollbar and the scroll position are those of
 * the whole list. Everything about the list as a whole (the order, the parent of
 * each step, the selection, the count in the header) is worked out by the caller
 * over every step; this only chooses which cards to put on the page.
 */
function TimelineList({ interactions, parentToolNames, selectedInteractionId, onSelectInteraction }: TimelineListProps) {
  const ids = useMemo(() => interactions.map((interaction) => interaction.id), [interactions]);
  const { win, windowed, scrollerRef, listRef } = useWindowedList(ids);
  // The steps of this list, for each card to ask whether its step came with it. Noted a frame after the
  // list is drawn, not at once: the cards drawn while it settles (the window grows by a card once the
  // heights are measured) belong to the same arrival and should fade in with the rest.
  const seen = useRef<ReadonlySet<string>>(NO_STEPS);
  useLayoutEffect(() => {
    const frame = requestAnimationFrame(() => {
      seen.current = new Set(ids);
    });
    return () => cancelAnimationFrame(frame);
  }, [ids]);

  const mounted = windowed ? interactions.slice(win.start, win.end) : interactions;
  return (
    // The browser's own scroll anchoring is off for a windowed list: the list holds the reader's place itself.
    <div ref={scrollerRef} className="flex-1 overflow-y-auto" style={windowed ? { overflowAnchor: 'none' } : undefined}>
      {/*
        The steps are a list, named for the people who hear it read out. It is `flow-root` so that it is a block
        formatting context: the margins of the cards stay inside it and add up as the windowing assumes. The role
        is written out because a list with its bullets removed is not announced as one by every screen reader.
      */}
      <ol
        ref={listRef}
        role="list"
        aria-label="Interactions linked to this PR"
        className="flow-root"
        style={windowed ? { paddingTop: win.before, paddingBottom: win.after } : undefined}
      >
        {mounted.map((interaction, offset) => (
          <TimelineItem
            key={interaction.id}
            interaction={interaction}
            isSelected={selectedInteractionId === interaction.id}
            onSelectInteraction={onSelectInteraction}
            parentToolNames={parentToolNames.get(interaction.id) ?? null}
            seen={seen}
            position={windowed ? win.start + offset + 1 : undefined}
            setSize={windowed ? interactions.length : undefined}
          />
        ))}
      </ol>
    </div>
  );
}

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

      <TimelineList
        interactions={sortedInteractions}
        parentToolNames={parentToolNames}
        selectedInteractionId={selectedInteractionId}
        onSelectInteraction={selectInteraction}
      />
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
