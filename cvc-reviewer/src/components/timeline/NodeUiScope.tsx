import { useContext, useMemo, useState, type ReactNode } from 'react';
import { NodeUiStore, ScopeContext, StoreContext, type Scope } from './nodeUiState';

/** The components that give folds their places (see `nodeUiState`). None of them draws an element. */

/** Gives the cards below it a store to keep their folds in. */
export function NodeUiProvider({ store, children }: { store: NodeUiStore; children: ReactNode }) {
  return <StoreContext.Provider value={store}>{children}</StoreContext.Provider>;
}

/** Says which step the folds below it belong to. */
export function NodeUiScope({ nodeId, children }: { nodeId: string; children: ReactNode }) {
  const shared = useContext(StoreContext);
  // For a card with no timeline around it: a store of its own, which is what local state was.
  const [own] = useState(() => new NodeUiStore());
  const store = shared ?? own;
  const value = useMemo<Scope>(() => ({ store, path: String(nodeId) }), [store, nodeId]);
  return <ScopeContext.Provider value={value}>{children}</ScopeContext.Provider>;
}

/**
 * Names a part of a card. Folds in different parts of a card have different places in the
 * store, however many of the same kind the card has (the third tool call's "Show all" is not
 * the first's). Names must be the same every time the part is drawn: an index in a list of
 * the step's data, not anything that depends on what else is on the page.
 */
export function Section({ name, children }: { name: string; children: ReactNode }) {
  const parent = useContext(ScopeContext);
  const value = useMemo<Scope | null>(() => (parent === null ? null : { store: parent.store, path: `${parent.path}/${name}` }), [parent, name]);
  return <ScopeContext.Provider value={value}>{children}</ScopeContext.Provider>;
}
