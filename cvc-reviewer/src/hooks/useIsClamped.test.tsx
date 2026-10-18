/**
 * Measuring whether a clamped block is cut off: on the next frame, and again when
 * the block is resized, with one observer for the whole page.
 */
import { act, cleanup, render } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { useIsClamped } from './useIsClamped';

/** A ResizeObserver the test can fire by hand. A new class per call, so the hook sees a new constructor. */
function fakeObserver() {
  const instances: Array<{ observed: Set<Element>; fire: (element: Element) => void }> = [];
  class FakeResizeObserver {
    observed = new Set<Element>();
    callback: ResizeObserverCallback;
    constructor(callback: ResizeObserverCallback) {
      this.callback = callback;
      instances.push(this);
    }
    observe(element: Element) {
      this.observed.add(element);
    }
    unobserve(element: Element) {
      this.observed.delete(element);
    }
    disconnect() {
      this.observed.clear();
    }
    fire(element: Element) {
      this.callback([{ target: element } as ResizeObserverEntry], this as unknown as ResizeObserver);
    }
  }
  vi.stubGlobal('ResizeObserver', FakeResizeObserver);
  return instances;
}

/** Heights the stand-in layout reports; change them to simulate a resize. */
function layout(initial: { scroll: number; client: number }) {
  const heights = { ...initial };
  vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(() => heights.scroll);
  vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(() => heights.client);
  return heights;
}

function Probe({ text, expanded = false, id }: { text: string; expanded?: boolean; id: string }) {
  const [element, setElement] = useState<HTMLElement | null>(null);
  const clamped = useIsClamped(element, expanded, text);
  return (
    <p ref={setElement} data-testid={id} data-clamped={String(clamped)}>
      {text}
    </p>
  );
}

const frame = () =>
  act(async () => {
    await new Promise((resolve) => requestAnimationFrame(() => resolve(undefined)));
  });

const clampedOf = (container: HTMLElement, id: string) => container.querySelector(`[data-testid="${id}"]`)!.getAttribute('data-clamped');

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('useIsClamped', () => {
  it('is false until the first frame, then true if the content is taller than the box', async () => {
    fakeObserver();
    layout({ scroll: 300, client: 80 });
    const { container } = render(<Probe id="a" text="long" />);
    expect(clampedOf(container, 'a')).toBe('false');
    await frame();
    expect(clampedOf(container, 'a')).toBe('true');
  });

  it('is false when the content fits, allowing a pixel of rounding', async () => {
    fakeObserver();
    layout({ scroll: 81, client: 80 });
    const { container } = render(<Probe id="a" text="fits" />);
    await frame();
    expect(clampedOf(container, 'a')).toBe('false');
  });

  it('measures again when the block is resized', async () => {
    const observers = fakeObserver();
    const heights = layout({ scroll: 80, client: 80 });
    const { container } = render(<Probe id="a" text="text" />);
    await frame();
    expect(clampedOf(container, 'a')).toBe('false');

    // The pane is made narrower: the same text now wraps onto more lines than fit.
    heights.scroll = 300;
    act(() => observers[0].fire(container.querySelector('[data-testid="a"]')!));
    expect(clampedOf(container, 'a')).toBe('true');

    heights.scroll = 80; // ...and wider again
    act(() => observers[0].fire(container.querySelector('[data-testid="a"]')!));
    expect(clampedOf(container, 'a')).toBe('false');
  });

  it('uses ONE observer for every block on the page, and tells each block only about itself', async () => {
    const observers = fakeObserver();
    const heights = layout({ scroll: 80, client: 80 });
    const { container } = render(
      <>
        {Array.from({ length: 40 }, (_, i) => (
          <Probe key={i} id={`p${i}`} text={`block ${i}`} />
        ))}
      </>,
    );
    await frame();
    expect(observers).toHaveLength(1);
    expect(observers[0].observed.size).toBe(40);

    heights.scroll = 300;
    act(() => observers[0].fire(container.querySelector('[data-testid="p7"]')!));
    expect(clampedOf(container, 'p7')).toBe('true');
    expect(clampedOf(container, 'p8')).toBe('false'); // not told: it was not the one resized
  });

  it('stops observing a block when it goes away', async () => {
    const observers = fakeObserver();
    layout({ scroll: 80, client: 80 });
    const { rerender } = render(
      <>
        <Probe id="a" text="a" />
        <Probe id="b" text="b" />
      </>,
    );
    await frame();
    expect(observers[0].observed.size).toBe(2);
    rerender(<Probe id="a" text="a" />);
    expect(observers[0].observed.size).toBe(1);
    cleanup();
    expect(observers[0].observed.size).toBe(0);
  });

  it('does not measure an open block, and keeps what the closed block said', async () => {
    const observers = fakeObserver();
    const heights = layout({ scroll: 300, client: 80 });
    const { container, rerender } = render(<Probe id="a" text="text" expanded={false} />);
    await frame();
    expect(clampedOf(container, 'a')).toBe('true');

    // Opened: nothing is cut off any more, but that is not news; the answer stands.
    rerender(<Probe id="a" text="text" expanded />);
    heights.scroll = 80;
    await frame();
    expect(clampedOf(container, 'a')).toBe('true');
    expect(observers[0].observed.size).toBe(0); // not watched while open

    // Closed again: measured afresh.
    heights.scroll = 300;
    rerender(<Probe id="a" text="text" expanded={false} />);
    await frame();
    expect(clampedOf(container, 'a')).toBe('true');
    expect(observers[0].observed.size).toBe(1);
  });

  it('measures again when the text changes', async () => {
    fakeObserver();
    const heights = layout({ scroll: 80, client: 80 });
    const { container, rerender } = render(<Probe id="a" text="short" />);
    await frame();
    expect(clampedOf(container, 'a')).toBe('false');
    heights.scroll = 400;
    rerender(<Probe id="a" text="a much longer text" />);
    await frame();
    expect(clampedOf(container, 'a')).toBe('true');
  });

  it('works where there is no ResizeObserver: it measures on the next frame only', async () => {
    vi.stubGlobal('ResizeObserver', undefined);
    layout({ scroll: 300, client: 80 });
    const { container } = render(<Probe id="a" text="long" />);
    await frame();
    expect(clampedOf(container, 'a')).toBe('true');
    expect(() => cleanup()).not.toThrow();
  });

  it('starts a new observer when the page\'s ResizeObserver is replaced', async () => {
    const first = fakeObserver();
    layout({ scroll: 80, client: 80 });
    const view = render(<Probe id="a" text="a" />);
    await frame();
    expect(first).toHaveLength(1);
    view.unmount();

    const second = fakeObserver();
    render(<Probe id="b" text="b" />);
    await frame();
    expect(second).toHaveLength(1);
    expect(second[0].observed.size).toBe(1);
    expect(first[0].observed.size).toBe(0);
  });

  it('does not run its measurement after it is gone', async () => {
    fakeObserver();
    const heights = layout({ scroll: 300, client: 80 });
    const { unmount } = render(<Probe id="a" text="long" />);
    unmount(); // before the frame
    heights.scroll = 500;
    await frame(); // would throw (state update on an unmounted component) if the frame were not cancelled
    expect(true).toBe(true);
  });
});
