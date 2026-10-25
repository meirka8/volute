/**
 * What a reader has opened in a step outlives the card that shows it (issue #118): the folds keep
 * their place in a store the timeline owns, by step and by part of the step.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, describe, expect, it } from 'vitest';
import { usePaged } from '../../hooks/usePaged';
import { NodeUiProvider, NodeUiScope, Section } from './NodeUiScope';
import { NodeUiStore, usePersistentCount, usePersistentToggle } from './nodeUiState';

afterEach(cleanup);

/** A fold: a button that says whether it is open and opens and closes. */
function Fold({ name, initial = false, label = name }: { name: string; initial?: boolean; label?: string }) {
  const [open, setOpen] = usePersistentToggle(name, initial);
  return (
    <button type="button" aria-expanded={open} onClick={() => setOpen((value) => !value)}>
      {label}
    </button>
  );
}

/** Shows its children or not, to stand for a card that leaves the page and comes back. */
function Mounted({ children }: { children: React.ReactNode }) {
  const [shown, setShown] = useState(true);
  return (
    <>
      <button type="button" onClick={() => setShown((value) => !value)}>
        toggle card
      </button>
      {shown && children}
    </>
  );
}

const fold = (name: string) => screen.getByRole('button', { name });
const leaveAndReturn = () => {
  fireEvent.click(screen.getByRole('button', { name: 'toggle card' }));
  fireEvent.click(screen.getByRole('button', { name: 'toggle card' }));
};

describe('a fold with no timeline around it', () => {
  it('works as local state did: it opens and closes, and starts again when the card comes back', () => {
    render(
      <Mounted>
        <NodeUiScope nodeId="a">
          <Fold name="more" />
        </NodeUiScope>
      </Mounted>,
    );
    expect(fold('more')).toHaveAttribute('aria-expanded', 'false');
    fireEvent.click(fold('more'));
    expect(fold('more')).toHaveAttribute('aria-expanded', 'true');
    leaveAndReturn();
    expect(fold('more')).toHaveAttribute('aria-expanded', 'false');
  });

  it('works with no scope at all, for a part drawn on its own', () => {
    render(<Fold name="alone" />);
    fireEvent.click(fold('alone'));
    expect(fold('alone')).toHaveAttribute('aria-expanded', 'true');
  });
});

describe('a fold in a timeline', () => {
  it('is found as the reader left it when its card comes back', () => {
    const store = new NodeUiStore();
    render(
      <NodeUiProvider store={store}>
        <Mounted>
          <NodeUiScope nodeId="a">
            <Fold name="more" />
          </NodeUiScope>
        </Mounted>
      </NodeUiProvider>,
    );
    fireEvent.click(fold('more'));
    leaveAndReturn();
    expect(fold('more')).toHaveAttribute('aria-expanded', 'true');
    fireEvent.click(fold('more'));
    leaveAndReturn();
    expect(fold('more')).toHaveAttribute('aria-expanded', 'false');
  });

  it('keeps the folds of one step apart from those of another, and a part of a step from another part', () => {
    const store = new NodeUiStore();
    render(
      <NodeUiProvider store={store}>
        <NodeUiScope nodeId="a">
          <Fold name="more" label="a more" />
          <Section name="call-0">
            <Fold name="more" label="a call-0 more" />
          </Section>
          <Section name="call-1">
            <Fold name="more" label="a call-1 more" />
          </Section>
        </NodeUiScope>
        <NodeUiScope nodeId="b">
          <Fold name="more" label="b more" />
        </NodeUiScope>
      </NodeUiProvider>,
    );
    fireEvent.click(fold('a call-1 more'));
    const state = ['a more', 'a call-0 more', 'a call-1 more', 'b more'].map((name) => fold(name).getAttribute('aria-expanded'));
    expect(state).toEqual(['false', 'false', 'true', 'false']);
    fireEvent.click(fold('b more'));
    expect(['a more', 'a call-0 more', 'a call-1 more', 'b more'].map((name) => fold(name).getAttribute('aria-expanded'))).toEqual(['false', 'false', 'true', 'true']);
  });

  it('nests: the same name under the same sections is the same fold', () => {
    const store = new NodeUiStore();
    const Tree = () => (
      <NodeUiScope nodeId="a">
        <Section name="outer">
          <Section name="inner">
            <Fold name="deep" />
          </Section>
        </Section>
      </NodeUiScope>
    );
    const { unmount } = render(
      <NodeUiProvider store={store}>
        <Tree />
      </NodeUiProvider>,
    );
    fireEvent.click(fold('deep'));
    unmount();
    render(
      <NodeUiProvider store={store}>
        <Tree />
      </NodeUiProvider>,
    );
    expect(fold('deep')).toHaveAttribute('aria-expanded', 'true');
  });

  it('keeps the same fold, drawn twice, in step', () => {
    const store = new NodeUiStore();
    render(
      <NodeUiProvider store={store}>
        <NodeUiScope nodeId="a">
          <Fold name="more" label="first" />
          <Fold name="more" label="second" />
        </NodeUiScope>
      </NodeUiProvider>,
    );
    fireEvent.click(fold('first'));
    expect(fold('second')).toHaveAttribute('aria-expanded', 'true');
  });

  it('holds only what the reader chose: a fold they never touched follows the start its card gives it', () => {
    const store = new NodeUiStore();
    const View = ({ start }: { start: boolean }) => (
      <NodeUiProvider store={store}>
        <NodeUiScope nodeId="a">
          <Fold name="untouched" initial={start} label="untouched" />
          <Fold name="touched" initial={start} label="touched" />
        </NodeUiScope>
      </NodeUiProvider>
    );
    const { rerender } = render(<View start={false} />);
    fireEvent.click(fold('touched')); // now open
    rerender(<View start={true} />);
    expect(fold('untouched')).toHaveAttribute('aria-expanded', 'true'); // follows the start
    expect(fold('touched')).toHaveAttribute('aria-expanded', 'true'); // the reader's choice
    rerender(<View start={false} />);
    expect(fold('untouched')).toHaveAttribute('aria-expanded', 'false');
    expect(fold('touched')).toHaveAttribute('aria-expanded', 'true'); // still
  });

  it('reads a toggle open that was shut by the reader even when it starts open', () => {
    const store = new NodeUiStore();
    render(
      <NodeUiProvider store={store}>
        <Mounted>
          <NodeUiScope nodeId="a">
            <Fold name="results" initial />
          </NodeUiScope>
        </Mounted>
      </NodeUiProvider>,
    );
    expect(fold('results')).toHaveAttribute('aria-expanded', 'true');
    fireEvent.click(fold('results')); // shut
    leaveAndReturn();
    expect(fold('results')).toHaveAttribute('aria-expanded', 'false');
  });

  it('counts how far a list has been paged, and keeps that too', () => {
    const store = new NodeUiStore();
    function Pager() {
      const { visible, remaining, showMore } = usePaged(25, 10, 'items');
      return (
        <button type="button" onClick={showMore}>
          {`${visible} shown, ${remaining} left`}
        </button>
      );
    }
    render(
      <NodeUiProvider store={store}>
        <Mounted>
          <NodeUiScope nodeId="a">
            <Pager />
          </NodeUiScope>
        </Mounted>
      </NodeUiProvider>,
    );
    expect(fold('10 shown, 15 left')).toBeInTheDocument();
    fireEvent.click(fold('10 shown, 15 left'));
    fireEvent.click(fold('20 shown, 5 left'));
    expect(fold('25 shown, 0 left')).toBeInTheDocument(); // never more than there are
    leaveAndReturn();
    expect(fold('25 shown, 0 left')).toBeInTheDocument();
  });

  it('gives a count its own start until it is raised', () => {
    const store = new NodeUiStore();
    function Count({ start }: { start: number }) {
      const [count, setCount] = usePersistentCount('n', start);
      return (
        <button type="button" onClick={() => setCount(count + 1)}>
          {`n=${count}`}
        </button>
      );
    }
    const View = ({ start }: { start: number }) => (
      <NodeUiProvider store={store}>
        <NodeUiScope nodeId="a">
          <Count start={start} />
        </NodeUiScope>
      </NodeUiProvider>
    );
    const { rerender } = render(<View start={5} />);
    rerender(<View start={7} />);
    expect(fold('n=7')).toBeInTheDocument();
    fireEvent.click(fold('n=7'));
    rerender(<View start={3} />);
    expect(fold('n=8')).toBeInTheDocument();
  });
});

describe('the store', () => {
  it('tells only the folds that changed, and stops when they are gone', () => {
    const store = new NodeUiStore();
    let first = 0;
    let second = 0;
    const stopFirst = store.subscribe('a', () => first++);
    store.subscribe('b', () => second++);
    store.set('a', true);
    store.set('a', true); // no change
    store.set('a', false);
    expect(first).toBe(2);
    expect(second).toBe(0);
    stopFirst();
    store.set('a', true);
    expect(first).toBe(2);
    expect(store.get('a')).toBe(true);
    expect(store.get('never')).toBeUndefined();
  });
});
