import { memo, useCallback, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState, type ReactNode, type Ref, type RefObject } from 'react';
import { AlertTriangle, Brain, GitPullRequest } from 'lucide-react';
import { useTimelineSearch } from '../../hooks/useTimelineSearch';
import { useWindowedList, type ListControls } from '../../hooks/useWindowedList';
import type { CommitInfo } from '../../lib/timelineSearch';
import { parentToolNamesByNode, type ParentToolCall } from '../../lib/transcriptNode';
import { NodeErrorBoundary } from './NodeErrorBoundary';
import { NodeUiProvider } from './NodeUiScope';
import { NodeUiStore } from './nodeUiState';
import { HighlightContext } from './searchHighlight';
import { TimelineNode } from './TimelineNode';
import { TimelineSearchBar } from './TimelineSearchBar';
import type { InteractionNode } from '../../types/cvc';

interface ShadowTimelineProps {
  interactions: InteractionNode[];
  selectedInteractionId?: string | null;
  onSelectInteraction?: (id: string) => void;
  prTitle?: string;
  isLoading?: boolean;
  /** Shown as a small warning banner, e.g. when history was capped for an older repo. */
  notice?: string;
  /** The pull request's commits, oldest first, for the commit filter. Without them it offers the commits the steps are linked to. */
  commits?: readonly CommitInfo[];
  /** The file open in the diff pane, which the "touches the open file" filter means. */
  openFile?: string | null;
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
  /** The search is on this step: it is outlined (and is not the selected one). */
  isCurrent: boolean;
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
  isCurrent,
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
        isCurrent={isCurrent}
      />
    </NodeErrorBoundary>
  );
});

const NO_STEPS: ReadonlySet<string> = new Set();

interface TimelineListProps {
  /** The steps to list, newest first: all of them, or the ones a search leaves. */
  interactions: readonly InteractionNode[];
  /** The ids of `interactions`, in order. The same array for as long as the list is. */
  ids: readonly string[];
  /** The ids of every step, which is what "seen before" is judged against, whatever the search leaves of them. */
  arrivalIds: readonly string[];
  /** What the parent of each step called, over every step: a search does not change what the steps are checked against. */
  parentToolNames: ReadonlyMap<string, readonly ParentToolCall[] | null>;
  selectedInteractionId?: string | null;
  /** The step the search is on. */
  currentId?: string | null;
  onSelectInteraction: (id: string) => void;
  /** Drawn in place of the list while it is empty (a search left nothing). */
  empty?: ReactNode;
  /** How the parent moves the reader in the list (to a match, back to where they were). */
  ref?: Ref<ListControls>;
}

/**
 * The scrolling list of steps. A long one is windowed (see `useWindowedList`): only
 * the cards near the viewport are mounted, with padding as tall as the cards left
 * out above and below them, so the scrollbar and the scroll position are those of
 * the whole list. Everything about the list as a whole (the order, the parent of
 * each step, the selection, the count in the header) is worked out by the caller
 * over every step; this only chooses which cards to put on the page.
 */
function TimelineList({
  interactions,
  ids,
  arrivalIds,
  parentToolNames,
  selectedInteractionId,
  currentId = null,
  onSelectInteraction,
  empty,
  ref,
}: TimelineListProps) {
  const { win, windowed, scrollerRef, listRef, controls } = useWindowedList(ids);
  useImperativeHandle(ref, () => controls, [controls]);
  // The steps the timeline has, for each card to ask whether its step came with it. Noted a frame after the
  // list is drawn, not at once: the cards drawn while it settles (the window grows by a card once the
  // heights are measured) belong to the same arrival and should fade in with the rest. It is judged over
  // every step, not the ones a search leaves: a card the search brings back is not arriving.
  const seen = useRef<ReadonlySet<string>>(NO_STEPS);
  useLayoutEffect(() => {
    const frame = requestAnimationFrame(() => {
      seen.current = new Set(arrivalIds);
    });
    return () => cancelAnimationFrame(frame);
  }, [arrivalIds]);

  const mounted = windowed ? interactions.slice(win.start, win.end) : interactions;
  return (
    // The browser's own scroll anchoring is off for a windowed list: the list holds the reader's place itself.
    <div ref={scrollerRef} className="flex-1 overflow-y-auto" style={windowed ? { overflowAnchor: 'none' } : undefined}>
      {/*
        The steps are a list, named for the people who hear it read out. It is `flow-root` so that it is a block
        formatting context: the margins of the cards stay inside it and add up as the windowing assumes. The role
        is written out because a list with its bullets removed is not announced as one by every screen reader.
      */}
      {interactions.length === 0 ? (
        empty
      ) : (
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
              isCurrent={currentId === interaction.id}
            />
          ))}
        </ol>
      )}
    </div>
  );
}

/** What the list says in place of the steps when a search or a filter leaves none. */
function NoMatches({ onClear }: { onClear: () => void }) {
  return (
    <div className="flex flex-col items-center gap-2 px-6 py-10 text-center text-muted">
      <p className="text-sm font-medium">No steps match</p>
      <p className="text-xs">Nothing in this timeline has what the search and the filters ask for together.</p>
      <button type="button" onClick={onClear} className="text-xs text-action transition-colors hover:opacity-80">
        Clear search and filters
      </button>
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
  commits,
  openFile = null,
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
  // What the reader has opened in each step, which outlives the card (a search takes cards off the page, and so does scrolling).
  const [store] = useState(() => new NodeUiStore());
  const listControls = useRef<ListControls | null>(null);
  const searchBox = useRef<HTMLInputElement | null>(null);
  const search = useTimelineSearch({ steps: sortedInteractions, commits, openFile, listControls, parentToolNames });
  const allIds = useMemo(() => sortedInteractions.map((interaction) => interaction.id), [sortedInteractions]);
  const shown = search.steps;
  const shownIds = useMemo(() => (shown === sortedInteractions ? allIds : shown.map((interaction) => interaction.id)), [shown, sortedInteractions, allIds]);

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
    <NodeUiProvider store={store}>
      <div className="h-full flex flex-col">
        <TimelineHeader prTitle={prTitle} count={interactions.length} notice={notice} />
        <TimelineSearchBar search={search} inputRef={searchBox} />

        <HighlightContext.Provider value={search.highlight}>
          <TimelineList
            ref={listControls}
            interactions={shown}
            ids={shownIds}
            arrivalIds={allIds}
            parentToolNames={parentToolNames}
            selectedInteractionId={selectedInteractionId}
            currentId={search.hasQuery ? search.currentId : null}
            onSelectInteraction={selectInteraction}
            empty={
              <NoMatches
                onClear={() => {
                  search.clearAll();
                  // The button is gone with the empty state: the keyboard goes to the box, where the next search starts.
                  searchBox.current?.focus();
                }}
              />
            }
          />
        </HighlightContext.Provider>
      </div>
    </NodeUiProvider>
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
