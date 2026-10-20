/**
 * The maths of the windowed timeline (issue #135): which steps to mount for a
 * scroll position, and how much empty space stands in for the others.
 *
 * Pure: no DOM and no React, so every case can be tested with plain numbers. The
 * code that measures the page and calls this lives in `hooks/useWindowedList`.
 *
 * A list of n steps is described by `offsets`, the running total of the steps'
 * heights with n + 1 entries: `offsets[i]` is where step i starts and
 * `offsets[n]` is the height of the whole list. A step's height is its outer
 * height, its own box and its margins, so that the steps tile the list exactly.
 */

/** What the reader can see, and how far beyond it steps are mounted as well. */
export interface TimelineView {
  /** How far the list is scrolled, in pixels from its top. */
  scrollTop: number;
  /** How tall the scrolling area is. */
  viewportHeight: number;
  /** Steps within this many pixels above and below the viewport are mounted too. */
  overscan: number;
}

/** The steps to mount, and the space that stands in for the rest. */
export interface TimelineWindow {
  /** The first step to mount. */
  start: number;
  /** One past the last step to mount, so steps start..end-1 are mounted. */
  end: number;
  /** The height of the steps before `start`, as empty space above the mounted ones. */
  before: number;
  /** The height of the steps from `end` on, as empty space below the mounted ones. */
  after: number;
  /** The height of the whole list; always `before` + the mounted steps + `after`. */
  total: number;
}

/** A place in the list that survives the steps around it changing height. */
export interface ScrollAnchor {
  /** The step that is at the top of the viewport. */
  index: number;
  /** How many pixels into that step the top of the viewport is. */
  within: number;
}

/** Numbers from the page can be NaN or infinite (a hidden pane); treat those as nothing. */
const finite = (value: number): number => (Number.isFinite(value) ? value : 0);

/**
 * The running total of `heights`: entry i is where step i starts, and the last
 * entry is the height of the whole list.
 */
export function offsetsOf(heights: ArrayLike<number>): Float64Array {
  const count = heights.length;
  const offsets = new Float64Array(count + 1);
  let sum = 0;
  for (let index = 0; index < count; index++) {
    offsets[index] = sum;
    sum += heights[index];
  }
  offsets[count] = sum;
  return offsets;
}

/**
 * The step that contains the point `y` pixels from the top of the list: the last
 * step that starts at or before `y`, so a step that is zero tall never contains
 * anything. Points above the list belong to the first step and points below it to
 * the last. An empty list has no step; this returns 0 for it.
 */
export function stepAt(offsets: ArrayLike<number>, y: number): number {
  const count = offsets.length - 1;
  if (count <= 0) return 0;
  let low = 0;
  let high = count - 1;
  while (low < high) {
    const middle = (low + high + 1) >>> 1;
    if (offsets[middle] <= y) low = middle;
    else high = middle - 1;
  }
  return low;
}

/** The first step that starts at or after `y`, or the step count when there is none. */
function firstStepStartingAtOrAfter(offsets: ArrayLike<number>, y: number): number {
  let low = 0;
  let high = offsets.length - 1;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (offsets[middle] >= y) high = middle;
    else low = middle + 1;
  }
  return low;
}

/**
 * The steps to mount for `view`: every step that touches the viewport or the
 * overscan around it, and always at least one (the list is never drawn empty).
 *
 * A step is mounted when any part of it lies in [scrollTop - overscan,
 * scrollTop + viewportHeight + overscan]; one that only touches the far edge of
 * that range is not.
 */
export function computeWindow(offsets: ArrayLike<number>, view: TimelineView): TimelineWindow {
  const count = offsets.length - 1;
  if (count <= 0) return { start: 0, end: 0, before: 0, after: 0, total: 0 };

  const total = offsets[count];
  const scrollTop = Math.min(Math.max(finite(view.scrollTop), 0), total);
  const viewport = Math.max(finite(view.viewportHeight), 0);
  const overscan = Math.max(finite(view.overscan), 0);

  const start = stepAt(offsets, scrollTop - overscan);
  const end = Math.max(start + 1, firstStepStartingAtOrAfter(offsets, scrollTop + viewport + overscan));
  return { start, end, before: offsets[start], after: total - offsets[end], total };
}

/** The same as `computeWindow`, for when only the heights are at hand. */
export function windowFor(heights: ArrayLike<number>, view: TimelineView): TimelineWindow {
  return computeWindow(offsetsOf(heights), view);
}

/** Whether two windows mount the same steps with the same space around them. */
export function sameWindow(a: TimelineWindow, b: TimelineWindow): boolean {
  return a.start === b.start && a.end === b.end && a.before === b.before && a.after === b.after && a.total === b.total;
}

/**
 * Where the top of the viewport is in the list, as a step and how far into it.
 * Null at the very top (and for an empty list): there nothing needs holding in
 * place, and a step added above should be seen.
 */
export function anchorAt(offsets: ArrayLike<number>, scrollTop: number): ScrollAnchor | null {
  const count = offsets.length - 1;
  const top = finite(scrollTop);
  if (count <= 0 || top <= 0) return null;
  const index = stepAt(offsets, top);
  return { index, within: top - offsets[index] };
}

/**
 * The scroll position that puts `anchor` where it was, whatever the steps above
 * it measure now. An anchor on a step that is gone clamps to the last one.
 */
export function scrollTopFor(offsets: ArrayLike<number>, anchor: ScrollAnchor): number {
  const count = offsets.length - 1;
  if (count <= 0) return 0;
  const index = Math.min(Math.max(anchor.index, 0), count - 1);
  return offsets[index] + anchor.within;
}
