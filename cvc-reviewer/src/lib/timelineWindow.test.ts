/**
 * The maths of the windowed timeline (issue #135): heights, scroll position and
 * viewport in; the steps to mount and the space for the others out. Plain numbers.
 */
import { describe, expect, it } from 'vitest';
import {
  anchorAt,
  computeWindow,
  offsetsOf,
  sameWindow,
  scrollTopFor,
  stepAt,
  windowFor,
  type TimelineView,
  type TimelineWindow,
} from './timelineWindow';

/** Ten steps, each 100 tall: step i occupies [100 i, 100 i + 100). */
const TEN = Array.from({ length: 10 }, () => 100);

const view = (scrollTop: number, viewportHeight = 300, overscan = 0): TimelineView => ({ scrollTop, viewportHeight, overscan });

/** The height of the mounted steps. */
const mountedHeight = (heights: readonly number[], win: TimelineWindow) =>
  heights.slice(win.start, win.end).reduce((sum, height) => sum + height, 0);

describe('offsetsOf', () => {
  it('is the running total, with the height of the whole list last', () => {
    expect(Array.from(offsetsOf([10, 20, 30]))).toEqual([0, 10, 30, 60]);
  });

  it('is just a zero for an empty list', () => {
    expect(Array.from(offsetsOf([]))).toEqual([0]);
  });
});

describe('stepAt', () => {
  const offsets = offsetsOf([100, 200, 100]); // [0, 100, 300, 400]

  it('finds the step that contains a point', () => {
    expect(stepAt(offsets, 0)).toBe(0);
    expect(stepAt(offsets, 99.9)).toBe(0);
    expect(stepAt(offsets, 150)).toBe(1);
    expect(stepAt(offsets, 399)).toBe(2);
  });

  it('gives a point on the line between two steps to the one below it', () => {
    expect(stepAt(offsets, 100)).toBe(1);
    expect(stepAt(offsets, 300)).toBe(2);
  });

  it('gives points above the list to the first step and points below it to the last', () => {
    expect(stepAt(offsets, -50)).toBe(0);
    expect(stepAt(offsets, 400)).toBe(2);
    expect(stepAt(offsets, 1e9)).toBe(2);
  });

  it('never lands on a step that is zero tall, if there is a step with height to land on', () => {
    expect(stepAt(offsetsOf([100, 0, 0, 100]), 100)).toBe(3);
    expect(stepAt(offsetsOf([100, 0, 0, 100]), 99)).toBe(0);
  });

  it('returns 0 for an empty list', () => {
    expect(stepAt(offsetsOf([]), 50)).toBe(0);
  });
});

describe('computeWindow: which steps to mount', () => {
  it('mounts nothing for an empty list', () => {
    expect(windowFor([], view(0))).toEqual({ start: 0, end: 0, before: 0, after: 0, total: 0 });
  });

  it('mounts what the viewport shows, and gives the rest as space', () => {
    // The viewport [250, 550) touches steps 2, 3, 4 and 5.
    const win = windowFor(TEN, view(250, 300));
    expect(win).toEqual({ start: 2, end: 6, before: 200, after: 400, total: 1000 });
  });

  it('does not mount a step that only touches the edge of what is wanted', () => {
    // [200, 500) is exactly steps 2, 3 and 4: step 1 ends where it starts, step 5 starts where it ends.
    const win = windowFor(TEN, view(200, 300));
    expect([win.start, win.end]).toEqual([2, 5]);
  });

  it('mounts a step one pixel inside what is wanted', () => {
    expect(windowFor(TEN, view(199, 300)).start).toBe(1);
    expect(windowFor(TEN, view(200, 301)).end).toBe(6);
  });

  it('mounts the overscan above and below the viewport as well', () => {
    // [250 - 150, 550 + 150) = [100, 700): steps 1 to 6.
    const win = windowFor(TEN, view(250, 300, 150));
    expect([win.start, win.end, win.before, win.after]).toEqual([1, 7, 100, 300]);
  });

  it('starts at the first step at the top of the list, overscan or not', () => {
    const win = windowFor(TEN, view(0, 300, 500));
    expect([win.start, win.before]).toEqual([0, 0]);
    expect(win.end).toBe(8);
  });

  it('ends at the last step at the bottom of the list, and leaves no space below', () => {
    // The viewport [700, 1000) is steps 7 to 9; with 100 of overscan above, step 6 as well.
    const win = windowFor(TEN, view(700, 300, 100));
    expect(win).toEqual({ start: 6, end: 10, before: 600, after: 0, total: 1000 });
  });

  it('mounts the whole list when it all fits', () => {
    expect(windowFor(TEN, view(0, 5000))).toEqual({ start: 0, end: 10, before: 0, after: 0, total: 1000 });
    expect(windowFor(TEN, view(0, 400, 1000))).toEqual({ start: 0, end: 10, before: 0, after: 0, total: 1000 });
  });

  it('keeps the mounted steps and the space around them as tall as the list', () => {
    const heights = [120, 80, 300, 45, 220, 90, 410, 60, 150, 75];
    for (let scrollTop = -50; scrollTop <= 1700; scrollTop += 37) {
      const win = windowFor(heights, view(scrollTop, 260, 130));
      expect(win.before + mountedHeight(heights, win) + win.after).toBeCloseTo(win.total, 9);
      expect(win.total).toBe(1550);
    }
  });

  it('always mounts at least one step, even with a viewport of nothing', () => {
    const win = windowFor(TEN, view(300, 0, 0));
    expect(win.end - win.start).toBeGreaterThanOrEqual(1);
    expect(win.start).toBe(3);
  });

  it('mounts a step taller than the viewport while any part of it shows', () => {
    const heights = [2000, 100];
    // [1800, 2100) shows the bottom of the tall step and the whole of the next one.
    expect(windowFor(heights, view(1800, 300))).toMatchObject({ start: 0, end: 2, before: 0, after: 0 });
    // [1000, 1300) is inside the tall step alone.
    expect(windowFor(heights, view(1000, 300))).toMatchObject({ start: 0, end: 1, after: 100 });
  });

  it('clamps a scroll position past the end to the last step', () => {
    const win = windowFor(TEN, view(50_000, 300, 0));
    expect([win.start, win.end, win.before, win.after]).toEqual([9, 10, 900, 0]);
  });

  it('treats a scroll position above the list (an overscroll bounce) as the top', () => {
    expect(windowFor(TEN, view(-200, 300, 0))).toEqual(windowFor(TEN, view(0, 300, 0)));
  });

  it('treats numbers a hidden pane can produce (NaN, Infinity) as nothing rather than failing', () => {
    expect(windowFor(TEN, { scrollTop: NaN, viewportHeight: 300, overscan: 0 })).toEqual(windowFor(TEN, view(0, 300, 0)));
    expect(() => windowFor(TEN, { scrollTop: 0, viewportHeight: Infinity, overscan: NaN })).not.toThrow();
    expect(windowFor(TEN, { scrollTop: 0, viewportHeight: -50, overscan: -5 }).end).toBeGreaterThanOrEqual(1);
  });

  it('copes with steps that are zero tall', () => {
    const heights = [100, 0, 0, 100, 0, 100];
    const win = windowFor(heights, view(100, 100, 0)); // exactly step 3: [100, 200)
    expect([win.start, win.end]).toEqual([3, 4]);
    expect(win.before).toBe(100);
    expect(win.after).toBe(100);
  });

  it('takes a prebuilt running total as well as heights', () => {
    const offsets = offsetsOf(TEN);
    expect(computeWindow(offsets, view(250, 300))).toEqual(windowFor(TEN, view(250, 300)));
  });
});

describe('computeWindow: every step that touches what is wanted, and no other', () => {
  /** A small seeded generator: the same cases on every run. */
  function random(seed: number) {
    let state = seed >>> 0 || 1;
    return () => {
      state ^= state << 13;
      state >>>= 0;
      state ^= state >>> 17;
      state ^= state << 5;
      state >>>= 0;
      return state / 0x100000000;
    };
  }

  it('agrees with a plain scan over 3,000 random lists and views', () => {
    const next = random(135);
    for (let round = 0; round < 3000; round++) {
      const count = 1 + Math.floor(next() * 40);
      const heights = Array.from({ length: count }, () => (next() < 0.1 ? 0 : Math.round(next() * 600 * 4) / 4));
      const offsets = offsetsOf(heights);
      const total = offsets[count];
      const scrollTop = next() * (total + 400) - 100;
      const viewportHeight = next() * 900;
      const overscan = next() < 0.3 ? 0 : next() * 900;
      const win = computeWindow(offsets, { scrollTop, viewportHeight, overscan });

      const clamped = Math.min(Math.max(scrollTop, 0), total);
      const from = clamped - overscan;
      const to = clamped + viewportHeight + overscan;
      const wanted: number[] = [];
      for (let index = 0; index < count; index++) {
        if (offsets[index + 1] > from && offsets[index] < to) wanted.push(index);
      }

      if (wanted.length > 0) {
        expect([win.start, win.end - 1]).toEqual([wanted[0], wanted[wanted.length - 1]]);
      } else {
        // Nothing is in range (a list of steps that are all zero tall, or all of it above/below): one step is mounted.
        expect(win.end - win.start).toBe(1);
      }
      expect(win.before).toBe(offsets[win.start]);
      expect(win.after).toBeCloseTo(total - offsets[win.end], 9);
      expect(win.before + mountedHeight(heights, win) + win.after).toBeCloseTo(total, 6);
      expect(win.start).toBeGreaterThanOrEqual(0);
      expect(win.end).toBeLessThanOrEqual(count);
      expect(win.end).toBeGreaterThan(win.start);
    }
  });

  it('mounts a number of steps that does not depend on how many there are, for steps of ordinary height', () => {
    const heights = Array.from({ length: 10_000 }, (_, index) => 200 + (index % 7) * 50);
    const offsets = offsetsOf(heights);
    for (const scrollTop of [0, 1234, 500_000, 1_500_000, offsets[10_000] - 900]) {
      const win = computeWindow(offsets, { scrollTop, viewportHeight: 900, overscan: 900 });
      expect(win.end - win.start).toBeLessThanOrEqual(20);
      expect(win.end - win.start).toBeGreaterThanOrEqual(6);
    }
  });
});

describe('sameWindow', () => {
  it('compares every number', () => {
    const win = windowFor(TEN, view(250, 300));
    expect(sameWindow(win, { ...win })).toBe(true);
    for (const key of ['start', 'end', 'before', 'after', 'total'] as const) {
      expect(sameWindow(win, { ...win, [key]: win[key] + 1 })).toBe(false);
    }
  });
});

describe('holding a place in the list while the steps above it change height', () => {
  const heights = [100, 100, 100, 100, 100, 100];

  it('has no anchor at the very top, where nothing is to be held and a step added above should show', () => {
    expect(anchorAt(offsetsOf(heights), 0)).toBeNull();
    expect(anchorAt(offsetsOf(heights), -20)).toBeNull();
    expect(anchorAt(offsetsOf([]), 50)).toBeNull();
  });

  it('is the step at the top of the viewport and how far into it', () => {
    expect(anchorAt(offsetsOf(heights), 250)).toEqual({ index: 2, within: 50 });
    expect(anchorAt(offsetsOf(heights), 300)).toEqual({ index: 3, within: 0 });
  });

  it('gives back the same scroll position for the same heights', () => {
    const offsets = offsetsOf(heights);
    for (const scrollTop of [1, 99, 100, 250, 599]) {
      expect(scrollTopFor(offsets, anchorAt(offsets, scrollTop)!)).toBe(scrollTop);
    }
  });

  it('moves the scroll position by exactly what changed above the anchor', () => {
    const anchor = anchorAt(offsetsOf(heights), 250)!; // step 2, 50 in
    // Steps 0 and 1 turn out to be 160 and 130 tall, 90 more between them.
    const measured = [160, 130, 100, 100, 100, 100];
    expect(scrollTopFor(offsetsOf(measured), anchor)).toBe(250 + 90);
  });

  it('is not moved by steps changing below the anchor, or by the anchor changing itself', () => {
    const anchor = anchorAt(offsetsOf(heights), 250)!;
    expect(scrollTopFor(offsetsOf([100, 100, 400, 7, 7, 7]), anchor)).toBe(250);
  });

  it('clamps an anchor on a step that is no longer in the list to the last step', () => {
    expect(scrollTopFor(offsetsOf([100, 100]), { index: 9, within: 10 })).toBe(110);
    expect(scrollTopFor(offsetsOf([]), { index: 3, within: 10 })).toBe(0);
  });
});
