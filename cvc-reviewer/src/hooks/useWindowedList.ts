import { useCallback, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type RefObject } from 'react';
import {
  anchorAt,
  computeWindow,
  offsetsOf,
  sameWindow,
  scrollTopFor,
  type TimelineWindow,
} from '../lib/timelineWindow';

/**
 * Windowing for a long list of cards of unknown, different heights (issue #135).
 *
 * The list mounts only the cards near the viewport (`TimelineWindow`, worked out by
 * `lib/timelineWindow`) and stands in for the rest with empty space: padding above
 * and below the mounted cards, as tall as the cards left out. A card's height is
 * measured when it is mounted and remembered by its id; until then the list uses an
 * estimate. The whole job is in this file:
 *
 * - `WindowedListController` holds the heights, the scroll position and the window,
 *   and reads and writes the page (scroll events, a resize observer, `scrollTop`).
 * - `useWindowedList` connects it to React: the window is an external store, so a
 *   change in it draws before the browser paints.
 *
 * The one hard problem is a jump. When the reader scrolls up, the cards that come
 * into the window above have only been estimated; once they are drawn they have
 * their real height, and everything below them, including what the reader is
 * looking at, moves by the difference. So the controller remembers which card is at
 * the top of the viewport and how far into it, and after every measurement scrolls
 * to where that card now is. That happens before the paint, so the page does not
 * visibly move. (The browser's own scroll anchoring is switched off for the list,
 * since two corrections would add up, and not every browser has it.)
 */

/** A list this long or shorter is mounted whole, as it was before windowing: nothing to measure or hold in place. */
export const WINDOWING_THRESHOLD = 30;

/** The height assumed for a card that has not been measured, until the first ones have been. */
export const DEFAULT_CARD_HEIGHT = 320;
/** No card is shorter than this, whatever was measured (a pane that is hidden measures as zero). */
export const MIN_CARD_HEIGHT = 40;
/** The estimate learned from the cards measured is kept within this. */
const MAX_CARD_ESTIMATE = 2000;
/** How tall the viewport is taken to be until it can be measured: about the height of the browser window. */
const guessViewportHeight = (): number => (typeof window === 'undefined' ? 0 : window.innerHeight) || 900;
/** Cards this far (at least) above and below the viewport are mounted too. */
const MIN_OVERSCAN = 600;
/** The estimate is the average height of the cards measured, once this many have been. */
const CARDS_TO_LEARN_FROM = 5;
/** Heights this close are the same height: a browser reports sub-pixel noise. */
const SAME_HEIGHT = 0.05;

const EMPTY_WINDOW: TimelineWindow = { start: 0, end: 0, before: 0, after: 0, total: 0 };

/** A list no one is ever given: what the controller holds when it has to lay the next one out afresh. */
const NO_IDS: readonly string[] = [];

/** Everything mounted, no space: the window of a list that is not windowed. */
const wholeList = (count: number): TimelineWindow => ({ start: 0, end: count, before: 0, after: 0, total: 0 });

/**
 * A card's outer height: its box plus the margin that separates it from the next
 * card. The cards stack in a block formatting context, where the bottom margin of
 * one and the top margin of the next collapse into one gap, so each card accounts
 * for one margin (the larger of its two, which are the same). The gap after the
 * last card then comes from the margin that last card has left over, as it does
 * when nothing is windowed.
 */
function outerHeight(element: Element): number {
  const style = getComputedStyle(element);
  const margin = Math.max(parseFloat(style.marginTop) || 0, parseFloat(style.marginBottom) || 0);
  return element.getBoundingClientRect().height + margin;
}

/** The place in the list to hold still, by the id of the card, so it survives the list changing. */
export interface HeldPlace {
  id: string;
  within: number;
}

/** How tall a part of a card has to be on the page for it to count as in view (see `scrollToIndex`). */
const MIN_VISIBLE = 160;

export class WindowedListController {
  private readonly listeners = new Set<() => void>();

  /** The outer height of every card measured so far, by id. It outlives changes to the list. */
  private readonly measured = new Map<string, number>();
  /** The height used for a card that has not been measured. */
  private estimate = DEFAULT_CARD_HEIGHT;
  private learned = false;

  // The list the numbers below are for.
  private ids: readonly string[] = [];
  private indexById = new Map<string, number>();
  private sizes: Float64Array = new Float64Array(0);
  private offsets: Float64Array = new Float64Array(1);

  private scrollTop = 0;
  /** The scroll position of the page when `held` was taken, to tell whether the reader has scrolled since. */
  private knownScrollTop = 0;
  private viewportHeight = guessViewportHeight();
  private width = -1;
  private held: HeldPlace | null = null;
  /** Set by `holdPlaceForNextList`: the place the next list is to open at, whatever the reader was looking at. */
  private nextPlace: HeldPlace | null = null;
  /** The list just laid out opens at a place it was told to, which the page has not been scrolled to yet. */
  private placed = false;

  private current: TimelineWindow = EMPTY_WINDOW;
  /** The cards mounted as of the last commit: `list.children[k]` is card `start + k`. */
  private mounted = { start: 0, end: 0 };

  private scroller: HTMLElement | null = null;
  private list: HTMLElement | null = null;
  private observer: ResizeObserver | null = null;
  private readonly observed = new Set<Element>();
  private frame = 0;

  // ----- what React reads ---------------------------------------------------

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /**
   * The window for `ids`. Cheap unless `ids` is a new list, when the heights are laid
   * out again (and the reader's place in it is kept, see `layOut`).
   */
  getWindow(ids: readonly string[]): TimelineWindow {
    if (ids !== this.ids) this.layOut(ids);
    return this.current;
  }

  // ----- connecting to the page ----------------------------------------------

  /** Starts watching `scroller`; returns the function that stops. */
  attach(scroller: HTMLElement, list: HTMLElement): () => void {
    this.scroller = scroller;
    this.list = list;
    scroller.addEventListener('scroll', this.onScroll, { passive: true });
    if (typeof ResizeObserver !== 'undefined') {
      this.observer = new ResizeObserver(this.onResize);
      this.observer.observe(scroller);
    }
    this.width = scroller.clientWidth;
    // The window is brought up to date by `afterCommit`, which runs next and has measured the cards by then:
    // doing it here as well could take a card out of the page only for it to be put back a moment later.
    if (this.placed) {
      // The list was laid out to open at a place the page has not been scrolled to yet: that place is the truth, not where the page is.
      this.knownScrollTop = scroller.scrollTop;
      this.viewportHeight = scroller.clientHeight || guessViewportHeight();
    } else {
      this.readScroll();
    }
    return () => this.detach();
  }

  private detach(): void {
    this.scroller?.removeEventListener('scroll', this.onScroll);
    this.observer?.disconnect();
    this.observer = null;
    this.observed.clear();
    if (this.frame !== 0) cancelAnimationFrame(this.frame);
    this.frame = 0;
    this.scroller = null;
    this.list = null;
  }

  /**
   * Called after React has drawn the window `drawn` for `ids`: measures the cards that are
   * there, keeps the reader's place, and, if what is drawn is no longer the right
   * window, says so (React then draws again before the paint).
   */
  afterCommit(ids: readonly string[], drawn: TimelineWindow): void {
    if (this.scroller === null || this.list === null || ids !== this.ids) return;
    this.syncHeld();
    this.mounted = { start: drawn.start, end: drawn.end };
    this.observeMounted();
    this.measureMounted();
    this.restorePlace();
    this.refresh();
  }

  // ----- moving the reader, for code that is not a scroll ---------------------------------

  /** The card at the top of the viewport and how far into it; null at the very top. */
  captureAnchor(): HeldPlace | null {
    this.syncHeld();
    return this.held === null ? null : { ...this.held };
  }

  /**
   * The next list to be laid out opens with `place` at the top of the viewport (if it holds that card). The list
   * is laid out again whatever it is: the one that comes next can be the very list that was on the page before a
   * search narrowed it (it was never laid out while the search was on, if the search left few enough steps).
   */
  holdPlaceForNextList(place: HeldPlace | null): void {
    this.nextPlace = place;
    this.ids = NO_IDS;
  }

  /** Takes in a scroll position that was set by code, which the browser reports a frame later. */
  noteScrolled(): void {
    this.readScroll();
    this.refresh();
  }

  /**
   * Scrolls card `index` to the top of the viewport, unless it is in view already (its top is, and
   * enough of it). The place of a card nobody has measured is an estimate; once it is on the page
   * the measuring corrects it, and the card stays where it was put (see `restorePlace`).
   */
  scrollToIndex(index: number): void {
    const scroller = this.scroller;
    if (scroller === null || !(index >= 0 && index < this.ids.length)) return;
    this.syncHeld();
    const top = this.offsets[index];
    const height = this.offsets[index + 1] - top;
    const view = scroller.scrollTop;
    if (top >= view && top + Math.min(height, MIN_VISIBLE) <= view + this.viewportHeight) return;
    // Whole pixels: the browser rounds the position to the pixel.
    scroller.scrollTop = Math.round(top);
    this.noteScrolled();
  }

  /** Called by the hook after every commit: whatever the page was told to open at, it has by now. */
  settled(): void {
    this.nextPlace = null;
    this.placed = false;
  }

  // ----- events ----------------------------------------------------------------

  private readonly onScroll = (): void => {
    this.readScroll();
    this.refresh();
  };

  /** Cards changed height after they were drawn (a block opened, a font arrived), or the pane changed size. */
  private readonly onResize = (): void => {
    const scroller = this.scroller;
    if (scroller === null) return;
    const width = scroller.clientWidth;
    // What the reader sees is what to hold still: take it before anything is measured.
    this.syncHeld();
    this.viewportHeight = scroller.clientHeight || guessViewportHeight();
    if (this.width >= 0 && Math.abs(width - this.width) > 0.5) this.forgetMeasurements();
    this.width = width;
    this.measureMounted();
    this.restorePlace();
    // Drawing again from inside a resize notification would make the browser report that it
    // could not deliver the notifications for the cards just drawn; the next frame is soon enough.
    this.refreshSoon();
  };

  private readScroll(): void {
    const scroller = this.scroller;
    if (scroller === null) return;
    this.scrollTop = scroller.scrollTop;
    this.knownScrollTop = this.scrollTop;
    this.viewportHeight = scroller.clientHeight || guessViewportHeight();
    this.held = this.holdPlace();
  }

  /**
   * Takes the reader's place again if the page has been scrolled since it was last taken. A scroll
   * event comes a frame after the scroll (and a script that scrolls during a frame, or a smooth
   * scroll, is not heard until the next one), so a resize or a draw can be the first to find the
   * reader somewhere new. Holding the old place then would scroll them back.
   */
  private syncHeld(): void {
    const scroller = this.scroller;
    if (scroller === null || scroller.scrollTop === this.knownScrollTop) return;
    this.readScroll();
  }

  // ----- the list ----------------------------------------------------------------

  /** Works out the heights, and so the window, for a new list; the reader stays where they were in it. */
  private layOut(ids: readonly string[]): void {
    // The reader may have scrolled since the last scroll event: find where they are in the list as it was.
    this.syncHeld();
    this.ids = ids;
    this.indexById = new Map();
    for (let index = 0; index < ids.length; index++) this.indexById.set(ids[index], index);
    // A card that is not in the list any more will not be measured again.
    for (const id of this.measured.keys()) if (!this.indexById.has(id)) this.measured.delete(id);

    this.sizes = new Float64Array(ids.length);
    for (let index = 0; index < ids.length; index++) this.sizes[index] = this.measured.get(ids[index]) ?? this.estimate;
    this.offsets = offsetsOf(this.sizes);

    // A list that replaces this one on purpose (the filters were cleared) opens where it is told to.
    if (this.nextPlace !== null) {
      this.held = this.nextPlace;
      this.nextPlace = null;
      this.placed = true;
    }
    // Steps arriving above the reader push what they are reading down: keep the same card in view.
    const place = this.held;
    const index = place === null ? undefined : this.indexById.get(place.id);
    if (place !== null && index !== undefined) this.scrollTop = this.offsets[index] + place.within;
    this.current = this.windowNow();
  }

  private windowNow(): TimelineWindow {
    return computeWindow(this.offsets, {
      scrollTop: this.scrollTop,
      viewportHeight: this.viewportHeight,
      overscan: Math.max(this.viewportHeight, MIN_OVERSCAN),
    });
  }

  /** Tells React when the window is a different one. */
  private refresh(): void {
    const next = this.windowNow();
    if (sameWindow(next, this.current)) return;
    this.current = next;
    for (const listener of [...this.listeners]) listener();
  }

  private refreshSoon(): void {
    if (this.frame !== 0) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = 0;
      this.refresh();
    });
  }

  // ----- measuring ------------------------------------------------------------------

  /** Keeps `onResize` told about the cards that are mounted now, and only those. */
  private observeMounted(): void {
    const observer = this.observer;
    const list = this.list;
    if (observer === null || list === null) return;
    const present = new Set<Element>(Array.from(list.children));
    for (const element of this.observed) {
      if (present.has(element)) continue;
      observer.unobserve(element);
      this.observed.delete(element);
    }
    for (const element of present) {
      if (this.observed.has(element)) continue;
      observer.observe(element);
      this.observed.add(element);
    }
  }

  /** Measures every card that is mounted, and remembers what it finds. */
  private measureMounted(): void {
    const list = this.list;
    if (list === null) return;
    const { start, end } = this.mounted;
    const cards = list.children;
    // Anything else means the page is between two draws; the next one measures.
    if (cards.length !== end - start) return;

    let changed = false;
    for (let offset = 0; offset < cards.length; offset++) {
      const index = start + offset;
      const id = this.ids[index];
      if (id === undefined) break;
      const height = Math.max(outerHeight(cards[offset]), MIN_CARD_HEIGHT);
      const before = this.measured.get(id);
      if (before !== undefined && Math.abs(before - height) <= SAME_HEIGHT) continue;
      this.measured.set(id, height);
      this.sizes[index] = height;
      changed = true;
    }
    if (!this.learned && this.measured.size >= CARDS_TO_LEARN_FROM) {
      this.learn();
      changed = true;
    }
    if (changed) this.offsets = offsetsOf(this.sizes);
  }

  /** From now on a card not measured is taken to be as tall as the average of those that were. */
  private learn(): void {
    let sum = 0;
    for (const height of this.measured.values()) sum += height;
    this.estimate = Math.min(Math.max(sum / this.measured.size, MIN_CARD_HEIGHT), MAX_CARD_ESTIMATE);
    this.learned = true;
    for (let index = 0; index < this.ids.length; index++) {
      if (!this.measured.has(this.ids[index])) this.sizes[index] = this.estimate;
    }
  }

  /** The pane changed width, so what text wraps to, so every height measured is out of date. */
  private forgetMeasurements(): void {
    this.measured.clear();
    this.learned = false;
    this.sizes.fill(this.estimate);
    this.offsets = offsetsOf(this.sizes);
  }

  // ----- holding the reader's place ----------------------------------------------------

  /** The card at the top of the viewport, and how far into it. Nothing at the very top of the list. */
  private holdPlace(): HeldPlace | null {
    const anchor = anchorAt(this.offsets, this.scrollTop);
    return anchor === null ? null : { id: this.ids[anchor.index], within: anchor.within };
  }

  /** Scrolls so the held card is where it was in the viewport, whatever the cards above it measured. */
  private restorePlace(): void {
    const scroller = this.scroller;
    const place = this.held;
    if (scroller === null || place === null) return;
    const index = this.indexById.get(place.id);
    if (index === undefined) return;
    const wanted = scrollTopFor(this.offsets, { index, within: place.within });
    if (Math.abs(scroller.scrollTop - wanted) < 1) {
      this.scrollTop = scroller.scrollTop;
      this.knownScrollTop = this.scrollTop;
      return;
    }
    // Whole pixels: the browser rounds the position to the pixel (a fraction of one would land on either side).
    scroller.scrollTop = Math.round(wanted);
    // The browser keeps the position within the list, so this is where the reader is now.
    this.scrollTop = scroller.scrollTop;
    this.knownScrollTop = this.scrollTop;
    this.held = this.holdPlace();
  }
}

/** Where the reader was in the whole list: for coming back to it after the list was narrowed and then widened again. */
export interface ScrollMemory {
  /** The card at the top of the viewport and how far into it. Null at the very top, and for a list that is not windowed. */
  readonly anchor: HeldPlace | null;
  /** The scroll position, which is what a list that is not windowed comes back to. */
  readonly scrollTop: number;
}

/** What code that moves the reader (search, for one) can ask of the list. */
export interface ListControls {
  /** Brings card `index` (a place in the list the hook was given) into view, at the top unless it is there already. */
  scrollToIndex(index: number): void;
  scrollToTop(): void;
  /** Where the reader is now; null when the list is not on the page. */
  capture(): ScrollMemory | null;
  /** Opens the list that comes next at `memory`. A null memory (the list was not on the page) does nothing. */
  restore(memory: ScrollMemory | null): void;
}

/** What `useWindowedList` returns. */
export interface WindowedList {
  /** The cards to mount, and the space for the others. The whole list when it is short. */
  win: TimelineWindow;
  /** Whether the list is windowed, that is, long enough for cards to be left out. */
  windowed: boolean;
  /** Goes on the element that scrolls. */
  scrollerRef: RefObject<HTMLDivElement | null>;
  /** Goes on the element that holds the mounted cards, as their only children. */
  listRef: RefObject<HTMLOListElement | null>;
  controls: ListControls;
}

/**
 * Windows a list identified by `ids` (one per card, in order, each different). The
 * caller draws `ids.slice(win.start, win.end)` as the only children of the list
 * element, with `win.before` and `win.after` pixels of padding around them.
 */
export function useWindowedList(ids: readonly string[]): WindowedList {
  const windowed = ids.length > WINDOWING_THRESHOLD;
  const [controller] = useState(() => new WindowedListController());
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const listRef = useRef<HTMLOListElement | null>(null);

  const whole = useMemo(() => wholeList(ids.length), [ids.length]);
  const getSnapshot = useCallback(() => (windowed ? controller.getWindow(ids) : whole), [controller, ids, windowed, whole]);
  const win = useSyncExternalStore(controller.subscribe, getSnapshot);

  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    const list = listRef.current;
    if (!windowed || scroller === null || list === null) return;
    return controller.attach(scroller, list);
  }, [controller, windowed]);

  useLayoutEffect(() => {
    if (windowed) controller.afterCommit(ids, win);
  }, [controller, windowed, ids, win]);

  // Where a list that replaces this one is to open, for the case that the controller does not do it: a list that is not windowed.
  const reopenAt = useRef<ScrollMemory | null>(null);
  const latest = useRef({ windowed, ids });
  useLayoutEffect(() => {
    latest.current = { windowed, ids };
    const memory = reopenAt.current;
    reopenAt.current = null;
    const scroller = scrollerRef.current;
    if (memory !== null && scroller !== null && (!windowed || memory.anchor === null)) {
      scroller.scrollTop = memory.scrollTop;
      // The page reports a scroll a frame later; the window is told now, so the right cards are drawn before the paint.
      if (windowed) controller.noteScrolled();
    }
    controller.settled();
  });

  const controls = useMemo<ListControls>(
    () => ({
      scrollToIndex(index) {
        const scroller = scrollerRef.current;
        if (scroller === null) return;
        if (latest.current.windowed) {
          controller.scrollToIndex(index);
          return;
        }
        // Everything is on the page: ask the page where the card is.
        const card = listRef.current?.children[index];
        if (!card) return;
        const area = scroller.getBoundingClientRect();
        const box = card.getBoundingClientRect();
        const top = box.top - area.top;
        if (top >= 0 && top + Math.min(box.height, MIN_VISIBLE) <= scroller.clientHeight) return;
        scroller.scrollTop = Math.max(0, Math.round(scroller.scrollTop + top));
      },
      scrollToTop() {
        const scroller = scrollerRef.current;
        if (scroller === null) return;
        scroller.scrollTop = 0;
        if (latest.current.windowed) controller.noteScrolled();
      },
      capture() {
        const scroller = scrollerRef.current;
        if (scroller === null) return null;
        return { anchor: latest.current.windowed ? controller.captureAnchor() : null, scrollTop: scroller.scrollTop };
      },
      restore(memory) {
        if (memory === null) return;
        controller.holdPlaceForNextList(memory.anchor);
        reopenAt.current = memory;
      },
    }),
    [controller],
  );

  return { win, windowed, scrollerRef, listRef, controls };
}
