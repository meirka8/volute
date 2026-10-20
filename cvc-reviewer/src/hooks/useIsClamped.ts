import { useEffect, useState } from 'react';

/**
 * One ResizeObserver serves every clamped block on the page: a timeline can hold
 * hundreds of them, and an observer per block would be hundreds of observers.
 * Created on first use, and again if the global constructor changes (tests swap it).
 */
let shared: {
  constructor: typeof ResizeObserver;
  observer: ResizeObserver;
  listeners: Map<Element, () => void>;
} | null = null;

function observe(element: Element, onResize: () => void): () => void {
  // jsdom and some embedded webviews have no ResizeObserver.
  if (typeof ResizeObserver === 'undefined') return () => {};

  if (shared === null || shared.constructor !== ResizeObserver) {
    const listeners = new Map<Element, () => void>();
    shared = {
      constructor: ResizeObserver,
      listeners,
      observer: new ResizeObserver((entries) => {
        for (const entry of entries) listeners.get(entry.target)?.();
      }),
    };
  }
  const { observer, listeners } = shared;
  listeners.set(element, onResize);
  observer.observe(element);
  return () => {
    listeners.delete(element);
    observer.unobserve(element);
  };
}

/**
 * Whether `element` is visibly cut off by a CSS line clamp (its content is taller
 * than its box). It measures again on the next frame and whenever the element
 * is resized, so it follows the pane being dragged or the font loading.
 *
 * `expanded` and `text` say when to measure again: an open block is not cut off,
 * so while `expanded` nothing is measured and the answer from the closed block
 * stands. (Measuring an open block would report "not cut off" and make the
 * "Show less" control disappear for a frame the moment it is pressed, dropping
 * keyboard focus.)
 */
export function useIsClamped(element: HTMLElement | null, expanded: boolean, text: string): boolean {
  const [clamped, setClamped] = useState(false);

  useEffect(() => {
    if (!element || expanded) return;

    const measure = () => setClamped(element.scrollHeight > element.clientHeight + 1);
    const frame = window.requestAnimationFrame(measure);
    const stop = observe(element, measure);

    return () => {
      window.cancelAnimationFrame(frame);
      stop();
    };
  }, [element, expanded, text]);

  return clamped;
}
