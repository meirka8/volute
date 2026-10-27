import { useCallback, useEffect, useId, useRef, useState, type RefObject } from 'react';
import { ChevronDown, ChevronRight, ChevronUp, Link2, ListFilter, Search, X } from 'lucide-react';
import { clsx } from 'clsx';
import { useSlashShortcut } from '../../hooks/useSearchShortcut';
import type { TimelineSearch } from '../../hooks/useTimelineSearch';
import { hiddenAsText } from '../../lib/hiddenChars';
import { looksLikeSecret } from '../../lib/secretLike';
import { MAX_QUERY_TERMS, type FacetOption, type FilterGroup } from '../../lib/timelineSearch';
import { HiddenText } from './HiddenText';
import { countText, SEARCH_PLACEHOLDER } from './searchText';
import { PANE_LABEL } from './styles';

const count = (value: number) => value.toLocaleString('en-US');

const ICON_BUTTON =
  'rounded-full border border-line bg-canvas/80 p-1.5 text-muted transition-colors hover:bg-surface hover:text-ink disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-canvas/80 disabled:hover:text-muted';

interface OptionGroupProps {
  legend: string;
  options: readonly FacetOption[];
  selected: readonly string[];
  onToggle: (value: string) => void;
}

/** One kind of filter: a check box for each value that is on offer, with how many steps hold it. */
function OptionGroup({ legend, options, selected, onToggle }: OptionGroupProps) {
  if (options.length === 0) return null;
  return (
    <fieldset className="min-w-0">
      <legend className={clsx(PANE_LABEL, 'mb-1 text-muted')}>{legend}</legend>
      <div className="space-y-0.5">
        {options.map((option) => (
          <label key={option.value} className="flex min-w-0 cursor-pointer items-start gap-2 rounded-lg px-1 py-0.5 text-xs text-ink hover:bg-surface/60">
            <input type="checkbox" className="mt-0.5 shrink-0 accent-action" checked={selected.includes(option.value)} onChange={() => onToggle(option.value)} />
            <span className="min-w-0 flex-1 break-words">
              <span>
                <HiddenText text={option.label} mode="strict" marks={false} /> <span className="text-muted">{count(option.count)}</span>
              </span>
              {option.note !== undefined && (
                <span className="block truncate text-[11px] text-muted">
                  <HiddenText text={option.note} mode="strict" marks={false} />
                </span>
              )}
              {option.hint !== undefined && (
                <span className="block truncate text-[11px] text-muted">
                  <HiddenText text={option.hint} mode="prose" marks={false} />
                </span>
              )}
            </span>
          </label>
        ))}
      </div>
    </fieldset>
  );
}

/**
 * What the Copy link control is showing besides its button, and for which search: a line that asks about a text, or a link
 * shown to copy by hand, is about the search it was made for, and goes away when the text or the filters change.
 */
type CopyPanel = { readonly kind: 'closed' } | { readonly kind: 'ask'; readonly of: string } | { readonly kind: 'manual'; readonly link: string; readonly of: string };
const CLOSED: CopyPanel = { kind: 'closed' };

/** How long "Link copied" stays in the count's place. */
const SAID_FOR_MS = 3_000;

const NOTE_CLASS = 'mt-2 rounded-xl border border-line bg-canvas/60 px-3 py-1.5 text-[11px] text-muted';
const TEXT_BUTTON = 'rounded-full px-2 py-1 text-[11px] text-action transition-colors hover:opacity-80';

/** What a search or filters that came with the link are called in the line that says so. */
function whatTheLinkBrought(search: TimelineSearch): string {
  const text = search.query !== '';
  const filters = search.chips.length > 0;
  if (text && filters) return 'a search and filters';
  return text ? 'a search' : 'filters';
}

/**
 * The search box of the timeline, its count and arrows, the filters and the chips of the ones that are on
 * (issue #118). It holds no state of the search: that is `useTimelineSearch`'s, passed in as `search`.
 *
 * - The box: Enter goes to the next match and Shift+Enter to the previous; Escape clears it, and when it is
 *   empty leaves it; `/` anywhere on the page that does not use the key comes to it.
 * - The count is a live region, so a screen reader hears "3 of 12" as the query narrows. While anything narrows the
 *   list it also says how many of the steps are shown, and a line says so when a link brought the search or the filters.
 * - The filters are check boxes in a disclosure; the ones that are on are also chips, each of which removes it.
 * - "Copy link to this view" copies the address with the filters and the search text in its fragment. A text that
 *   looks like a secret is not copied before the person has been asked; where the browser does not let the page write
 *   to the clipboard, the link is shown in a field to copy from.
 */
export function TimelineSearchBar({ search, inputRef }: { search: TimelineSearch; inputRef: RefObject<HTMLInputElement | null> }) {
  const filtersRef = useRef<HTMLButtonElement>(null);
  const copyRef = useRef<HTMLButtonElement>(null);
  const safeRef = useRef<HTMLButtonElement>(null);
  const linkFieldRef = useRef<HTMLInputElement>(null);
  const inputId = useId();
  const panelId = useId();
  const askId = useId();
  const linkFieldId = useId();
  const [open, setOpen] = useState(false);
  const [shown, setPanel] = useState<CopyPanel>(CLOSED);
  const [said, setSaid] = useState<{ message: string; of: string } | null>(null);

  const focus = useCallback(() => {
    const input = inputRef.current;
    if (input === null) return;
    input.focus();
    input.select();
  }, [inputRef]);
  useSlashShortcut(focus);

  const { facets, filters, chips } = search;
  const pathShown = search.file ?? search.openFile;
  const canNavigate = search.hasQuery && search.total > 1;
  const toggle = (group: FilterGroup) => (value: string) => search.toggleFilter(group, value);

  // "Link copied" takes the place of the count for a moment, and only for the search it was said about.
  const searchKey = `${search.query}\u0000${chips.map((chip) => chip.key).join('\u0000')}`;
  const panel: CopyPanel = shown.kind !== 'closed' && shown.of === searchKey ? shown : CLOSED;
  const announcement = said !== null && said.of === searchKey ? said.message : null;
  useEffect(() => {
    if (said === null) return;
    const timer = window.setTimeout(() => setSaid(null), SAID_FOR_MS);
    return () => window.clearTimeout(timer);
  }, [said]);

  // Where the keyboard goes when the line that asks, or the field with the link, comes up.
  useEffect(() => {
    if (panel.kind === 'ask') safeRef.current?.focus();
    if (panel.kind === 'manual') {
      linkFieldRef.current?.focus();
      linkFieldRef.current?.select();
    }
  }, [panel.kind]);

  const copy = (withText: boolean) => {
    const link = search.linkToView(withText);
    const clipboard = typeof navigator === 'undefined' ? undefined : navigator.clipboard;
    const showIt = () => setPanel({ kind: 'manual', link, of: searchKey });
    if (clipboard === undefined || typeof clipboard.writeText !== 'function') {
      showIt();
      return;
    }
    clipboard.writeText(link).then(() => {
      setPanel(CLOSED);
      setSaid({ message: 'Link copied', of: searchKey });
      copyRef.current?.focus();
    }, showIt);
  };

  const closePanel = () => {
    setPanel(CLOSED);
    copyRef.current?.focus();
  };

  return (
    <div className="border-b border-line/80 px-4 py-3">
      <div className="relative">
        <label htmlFor={inputId} className="sr-only">
          Search steps
        </label>
        <Search size={14} aria-hidden="true" className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-muted" />
        <input
          ref={inputRef}
          id={inputId}
          type="search"
          value={search.query}
          maxLength={500}
          autoComplete="off"
          spellCheck={false}
          enterKeyHint="search"
          aria-keyshortcuts="/"
          placeholder={SEARCH_PLACEHOLDER}
          className="w-full min-w-0 rounded-full border border-line bg-canvas/80 py-1.5 pl-8 pr-8 text-sm text-ink placeholder:text-muted [&::-webkit-search-cancel-button]:hidden [&::-webkit-search-decoration]:hidden"
          onFocus={search.engage}
          onChange={(event) => search.setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.nativeEvent.isComposing) return;
            if (event.key === 'Enter') {
              event.preventDefault();
              search.step(event.shiftKey ? -1 : 1);
            } else if (event.key === 'Escape') {
              // The first Escape clears what was typed; the next leaves the box.
              event.preventDefault();
              if (search.query !== '') search.clearQuery();
              else event.currentTarget.blur();
            }
          }}
        />
        {search.query !== '' && (
          <button
            type="button"
            aria-label="Clear search"
            title="Clear search (Esc)"
            className="absolute right-2 top-1/2 -translate-y-1/2 rounded-full p-1 text-muted transition-colors hover:text-ink"
            onClick={() => {
              search.clearQuery();
              inputRef.current?.focus();
            }}
          >
            <X size={14} aria-hidden="true" />
          </button>
        )}
      </div>

      <div className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1.5">
        <div className="flex items-center gap-1">
          <button type="button" aria-label="Previous match" title="Previous match (Shift+Enter)" className={ICON_BUTTON} disabled={!canNavigate} onClick={() => search.step(-1)}>
            <ChevronUp size={14} aria-hidden="true" />
          </button>
          <button type="button" aria-label="Next match" title="Next match (Enter)" className={ICON_BUTTON} disabled={!canNavigate} onClick={() => search.step(1)}>
            <ChevronDown size={14} aria-hidden="true" />
          </button>
        </div>
        <span role="status" aria-live="polite" aria-atomic="true" className="min-w-0 text-xs text-muted">
          {announcement ?? countText(search)}
        </span>
        <div className="ml-auto flex items-center gap-1.5">
          <button
            ref={copyRef}
            type="button"
            aria-label="Copy link to this view"
            title="Copy link to this view"
            className="flex items-center gap-1 rounded-full border border-line bg-canvas/80 px-2.5 py-1 text-[11px] font-medium text-action transition-colors hover:bg-surface"
            onClick={() => {
              if (search.query !== '' && looksLikeSecret(search.query)) setPanel({ kind: 'ask', of: searchKey });
              else copy(search.query !== '');
            }}
          >
            <Link2 size={12} aria-hidden="true" />
            <span>Copy link</span>
          </button>
          <button
            ref={filtersRef}
            type="button"
            aria-expanded={open}
            aria-controls={open ? panelId : undefined}
            className="flex items-center gap-1 rounded-full border border-line bg-canvas/80 px-2.5 py-1 text-[11px] font-medium text-action transition-colors hover:bg-surface"
            onClick={() => {
              search.engage();
              setOpen((value) => !value);
            }}
          >
            {open ? <ChevronDown size={12} aria-hidden="true" /> : <ChevronRight size={12} aria-hidden="true" />}
            <ListFilter size={12} aria-hidden="true" />
            <span>{chips.length > 0 ? `Filters (${chips.length} active)` : 'Filters'}</span>
          </button>
        </div>
      </div>

      {panel.kind === 'ask' && (
        <div
          role="group"
          aria-labelledby={askId}
          className={clsx(NOTE_CLASS, 'flex flex-wrap items-center gap-x-3 gap-y-1.5 border-warning/60 text-ink')}
          onKeyDown={(event) => {
            if (event.key !== 'Escape') return;
            event.preventDefault();
            event.stopPropagation();
            closePanel();
          }}
        >
          <p id={askId} role="alert" className="min-w-0 flex-1 basis-48">
            The search text looks like a secret; anyone with the link will see it
          </p>
          <div className="flex flex-wrap items-center gap-1.5">
            <button
              ref={safeRef}
              type="button"
              className="rounded-full border border-line bg-canvas/80 px-2.5 py-1 text-[11px] font-medium text-action transition-colors hover:bg-surface"
              onClick={() => copy(false)}
            >
              Copy without the search text
            </button>
            <button type="button" className={TEXT_BUTTON} onClick={() => copy(true)}>
              Copy anyway
            </button>
          </div>
        </div>
      )}

      {panel.kind === 'manual' && (
        <div
          role="group"
          aria-label="Link to this view"
          className={NOTE_CLASS}
          onKeyDown={(event) => {
            if (event.key !== 'Escape') return;
            event.preventDefault();
            event.stopPropagation();
            closePanel();
          }}
        >
          <label htmlFor={linkFieldId} className="block">
            The browser did not let this page copy the link. Copy it from here:
          </label>
          <div className="mt-1 flex items-center gap-1.5">
            <input
              id={linkFieldId}
              ref={linkFieldRef}
              type="text"
              readOnly
              value={panel.link}
              onFocus={(event) => event.currentTarget.select()}
              className="min-w-0 flex-1 rounded-lg border border-line bg-canvas/80 px-2 py-1 font-mono text-[11px] text-ink"
            />
            <button type="button" className={TEXT_BUTTON} onClick={closePanel}>
              Close
            </button>
          </div>
        </div>
      )}

      {search.termsDropped && (
        <p role="note" className={NOTE_CLASS}>
          Only the first {MAX_QUERY_TERMS} terms are used
        </p>
      )}

      {search.fromAddress && (
        <p role="note" className={clsx(NOTE_CLASS, 'flex flex-wrap items-center gap-x-2 gap-y-1')}>
          <span className="min-w-0">{`This view was opened from a link with ${whatTheLinkBrought(search)} applied.`}</span>
          <button
            type="button"
            className={TEXT_BUTTON}
            onClick={() => {
              search.clearAll();
              inputRef.current?.focus();
            }}
          >
            Clear
          </button>
        </p>
      )}

      {open && (
        <div id={panelId} role="group" aria-label="Filters" className="mt-2 max-h-[min(12rem,30vh)] space-y-3 overflow-auto rounded-xl border border-line bg-canvas/60 p-3">
          <OptionGroup legend="Conversation" options={facets.conversations} selected={filters.conv} onToggle={toggle('conv')} />
          <OptionGroup legend="Stimulus origin" options={facets.origins} selected={filters.origin} onToggle={toggle('origin')} />
          <OptionGroup legend="Tool" options={facets.tools} selected={filters.tool} onToggle={toggle('tool')} />
          <OptionGroup legend="Link type" options={facets.links} selected={filters.link} onToggle={toggle('link')} />
          <OptionGroup legend="Commit" options={facets.commits} selected={filters.commit} onToggle={toggle('commit')} />
          <label className="flex min-w-0 cursor-pointer items-start gap-2 rounded-lg px-1 py-0.5 text-xs text-ink hover:bg-surface/60">
            <input
              type="checkbox"
              className="mt-0.5 shrink-0 accent-action"
              checked={filters.file.mode !== 'off'}
              disabled={filters.file.mode === 'off' && search.openFile === null}
              onChange={(event) => search.setFileFilter(event.target.checked)}
            />
            <span className="min-w-0 flex-1">
              Touches the open file
              {pathShown !== null && (
                <span className="block break-all font-mono text-[11px] text-muted">
                  <HiddenText text={pathShown} mode="strict" marks={false} />
                </span>
              )}
            </span>
          </label>
        </div>
      )}

      {chips.length > 0 && (
        <ul aria-label="Active filters" className="mt-2 flex flex-wrap gap-1.5">
          {chips.map((chip) => (
            <li key={chip.key} className="min-w-0 max-w-full">
              <button
                type="button"
                aria-label={`Remove filter ${chip.kind}: ${hiddenAsText(chip.label, 'strict')}`}
                className="inline-flex max-w-full items-center gap-1 rounded-full border border-line bg-canvas/80 px-2.5 py-1 text-[11px] text-ink transition-colors hover:bg-surface"
                onClick={() => {
                  search.removeChip(chip);
                  // The chip is gone: keyboard focus goes to the control that opens the filters, not to nowhere.
                  filtersRef.current?.focus();
                }}
              >
                <span className="min-w-0 truncate">
                  <span className="text-muted">{chip.kind}:</span> <HiddenText text={chip.label} mode="strict" marks={false} />
                </span>
                <X size={11} aria-hidden="true" className="shrink-0 text-muted" />
              </button>
            </li>
          ))}
          {chips.length > 1 && (
            <li>
              <button
                type="button"
                className={TEXT_BUTTON}
                onClick={() => {
                  search.clearFilters();
                  filtersRef.current?.focus();
                }}
              >
                Clear filters
              </button>
            </li>
          )}
        </ul>
      )}
    </div>
  );
}
