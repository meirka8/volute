import { createContext, useCallback, useContext, useState, useSyncExternalStore } from 'react';

/**
 * What a reader has opened in a step, kept for as long as the timeline lives (issue #118).
 *
 * A card's folds ("Show more", "Show all 40 lines", a group of tool results, "Why?", the
 * pages of a long list) used to be state of the component that draws them, so a card lost
 * them whenever it left the page: scrolled out of the window of a long timeline, or filtered
 * out by a search. Now that state lives in a store the timeline owns, keyed by the step and
 * by where in the step the fold is, and a card that comes back finds its folds as the
 * reader left them.
 *
 * - The timeline provides the store (`NodeUiProvider`). A card names its step (`NodeUiScope`)
 *   and the parts of a card name the parts of it (`Section`): none of these draws an element.
 * - A fold reads and writes its own place in the store (`usePersistentToggle`,
 *   `usePersistentCount`), in the way `useState` is read and written.
 * - The store holds only what the reader chose. A fold they never touched has the value its
 *   component says it starts with, so a start that depends on the step (tool results start
 *   open in a step a person took part in) still follows the step.
 * - A card drawn without a timeline (in a test, or on its own) keeps its state to itself, as
 *   it always did.
 */

type Value = boolean | number;

export class NodeUiStore {
  private readonly values = new Map<string, Value>();
  private readonly listeners = new Map<string, Set<() => void>>();

  get(key: string): Value | undefined {
    return this.values.get(key);
  }

  set(key: string, value: Value): void {
    if (this.values.get(key) === value) return;
    this.values.set(key, value);
    const listeners = this.listeners.get(key);
    if (listeners) for (const listener of [...listeners]) listener();
  }

  subscribe(key: string, listener: () => void): () => void {
    let listeners = this.listeners.get(key);
    if (!listeners) {
      listeners = new Set();
      this.listeners.set(key, listeners);
    }
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
      if (listeners.size === 0) this.listeners.delete(key);
    };
  }
}

/** The store of the timeline the card is in, if it is in one. */
export const StoreContext = createContext<NodeUiStore | null>(null);

/** Where a fold is: the store, and the place in it that the step and the sections around the fold make. */
export interface Scope {
  readonly store: NodeUiStore;
  readonly path: string;
}
export const ScopeContext = createContext<Scope | null>(null);

type Setter<T> = (next: T | ((previous: T) => T)) => void;
function usePersistentValue<T extends Value>(name: string, initial: T): [T, Setter<T>] {
  const scope = useContext(ScopeContext);
  const [local, setLocal] = useState<T>(initial);
  const key = scope === null ? null : `${scope.path}/${name}`;
  const store = scope === null ? null : scope.store;

  const subscribe = useCallback((listener: () => void) => (store === null || key === null ? () => {} : store.subscribe(key, listener)), [store, key]);
  const getSnapshot = useCallback((): T => {
    if (store === null || key === null) return initial;
    const stored = store.get(key);
    return stored === undefined ? initial : (stored as T);
  }, [store, key, initial]);
  const stored = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);

  const set = useCallback<Setter<T>>(
    (next) => {
      if (store === null || key === null) {
        setLocal(next);
        return;
      }
      const current = store.get(key);
      const previous = current === undefined ? initial : (current as T);
      store.set(key, typeof next === 'function' ? next(previous) : next);
    },
    [store, key, initial],
  );
  return [store === null ? local : stored, set];
}

/** A fold that is open or closed. `initial` is what it is until the reader touches it. */
export function usePersistentToggle(name: string, initial: boolean): [boolean, Setter<boolean>] {
  return usePersistentValue<boolean>(name, initial);
}

/** A count the reader raises (how many items of a long list are drawn). `initial` is what it is until the reader touches it. */
export function usePersistentCount(name: string, initial: number): [number, Setter<number>] {
  return usePersistentValue<number>(name, initial);
}
