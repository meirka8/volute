/**
 * A page layout for jsdom, which has none: every element is zero tall and the
 * viewport is zero high. The windowed timeline measures cards and the scrolling
 * area, so tests of it say how tall each card is and how big the viewport is.
 *
 * A card is identified by the step number in its text ("Step 17"), which is how
 * the tests name their steps; a card without one is asked about as step -1. Call it
 * before rendering; pair it with `restoreDomStubs`, which puts back what it replaced.
 */

/**
 * The step number a card shows ("Step 17" as a text of its own), or null for anything
 * that is not a card. Looked for in the text nodes: the text of a whole card runs
 * its words together ("ThoughtStep 17Action"), which no pattern can take apart.
 */
export function stepOf(element: Element): number | null {
  const walker = element.ownerDocument.createTreeWalker(element, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node !== null; node = walker.nextNode()) {
    const match = /^Step (\d+)$/.exec(node.nodeValue ?? '');
    if (match !== null) return Number(match[1]);
  }
  return null;
}

export interface PageLayout {
  /** How high the scrolling area is. */
  viewport: number;
  /** How wide it is. */
  width: number;
  /** How tall the card for a step is. Change it to simulate a card growing or shrinking. */
  heightOf: (step: number, card: Element) => number;
  /** The steps whose card the page has been asked to measure, in the order they were first measured. */
  measured: Set<number>;
}

/**
 * What `stubPageLayout` replaced, to put back. (It does not use `vi.spyOn`: a spy keeps the element and the result of every
 * call it is given, and a test that draws a few hundred cards asks about each of them many times; one run of the random walks
 * ran a worker out of memory that way. A plain replacement keeps nothing.)
 */
const restorers: Array<() => void> = [];

function replace(target: object, key: string, descriptor: PropertyDescriptor): void {
  const original = Object.getOwnPropertyDescriptor(target, key);
  Object.defineProperty(target, key, { configurable: true, ...descriptor });
  restorers.push(() => {
    if (original === undefined) delete (target as Record<string, unknown>)[key];
    else Object.defineProperty(target, key, original);
  });
}

/** Puts back what `stubPageLayout` replaced. `restoreDomStubs` calls it. */
export function restoreLayoutStubs(): void {
  while (restorers.length > 0) restorers.pop()!();
}

export function stubPageLayout(layout: Partial<PageLayout> & Pick<PageLayout, 'heightOf'>): PageLayout {
  const page: PageLayout = { viewport: 800, width: 480, measured: new Set(), ...layout };

  replace(HTMLElement.prototype, 'getBoundingClientRect', {
    writable: true,
    value: function (this: HTMLElement) {
      let height = 0;
      // The cards are the children of the list element, which the timeline gives the class `flow-root`.
      if (this.matches('.flow-root > *')) {
        const step = stepOf(this);
        height = page.heightOf(step ?? -1, this);
        if (step !== null) page.measured.add(step);
      }
      return { x: 0, y: 0, top: 0, left: 0, right: 0, bottom: height, width: 0, height, toJSON: () => ({}) } as DOMRect;
    },
  });
  replace(Element.prototype, 'clientHeight', {
    get(this: Element) {
      return this.classList.contains('overflow-y-auto') ? page.viewport : 0;
    },
  });
  replace(Element.prototype, 'clientWidth', {
    get(this: Element) {
      return this.classList.contains('overflow-y-auto') ? page.width : 0;
    },
  });
  return page;
}

/** A resize observer the test can fire: `trigger()` runs the callback as the browser would after a resize. */
export class FakeResizeObserver {
  static instances: FakeResizeObserver[] = [];
  readonly observed = new Set<Element>();
  private readonly callback: ResizeObserverCallback;

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    FakeResizeObserver.instances.push(this);
  }

  observe(element: Element): void {
    this.observed.add(element);
  }

  unobserve(element: Element): void {
    this.observed.delete(element);
  }

  disconnect(): void {
    this.observed.clear();
  }

  trigger(): void {
    this.callback([], this as unknown as ResizeObserver);
  }

  /** The observer that is watching `element`, if one is. */
  static watching(element: Element): FakeResizeObserver | undefined {
    return FakeResizeObserver.instances.find((observer) => observer.observed.has(element));
  }
}
