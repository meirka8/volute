import { vi } from 'vitest';
import { restoreLayoutStubs } from './layoutStubs';

/**
 * Minimal `ResizeObserver` stand-in. jsdom does not implement it, and the
 * timeline measures clamped text with one.
 */
export class ResizeObserverStub {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}

/**
 * Makes jsdom render the timeline deterministically.
 *
 * - Stubs `ResizeObserver` (see above).
 * - Pins the timestamp text. `formatTimestamp` calls `toLocaleString(undefined,
 *   ...)`, so its output depends on the machine's locale and time zone; here it
 *   becomes the ISO instant, wrapped in angle brackets so it is easy to spot.
 *
 * Pair it with `afterEach(restoreDomStubs)`.
 */
export function installDomStubs(): void {
  vi.stubGlobal('ResizeObserver', ResizeObserverStub);
  vi.spyOn(Date.prototype, 'toLocaleString').mockImplementation(function (this: Date) {
    return `<${this.toISOString()}>`;
  });
}

export function restoreDomStubs(): void {
  restoreLayoutStubs();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
}
