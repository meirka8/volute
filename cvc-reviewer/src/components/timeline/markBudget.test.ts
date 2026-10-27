import { describe, expect, it, vi } from 'vitest';
import { MARKS_PER_CARD, MarkBudget } from './markBudget';

describe('MarkBudget', () => {
  it('has a budget of five hundred marks to a card, by default', () => {
    expect(MARKS_PER_CARD).toBe(500);
    expect(new MarkBudget().limit).toBe(500);
  });

  it('grants what is asked while there is room, and what is left after that', () => {
    const budget = new MarkBudget(10);
    expect(budget.claim('a', 4)).toBe(4);
    expect(budget.claim('b', 4)).toBe(4);
    expect(budget.claim('c', 4)).toBe(2);
    expect(budget.claim('d', 1)).toBe(0);
    expect(budget.room('d')).toBe(0);
  });

  it('gives the same answer to the same owner asking for the same, however often it asks (React draws twice)', () => {
    const budget = new MarkBudget(10);
    expect(budget.claim('a', 6)).toBe(6);
    expect(budget.claim('b', 6)).toBe(4);
    for (let k = 0; k < 5; k++) {
      expect(budget.claim('a', 6)).toBe(6);
      expect(budget.claim('b', 6)).toBe(4);
    }
    expect(budget.room('a')).toBe(6);
  });

  it('says how much an owner could have, counting what it holds, and one that asks for something else is answered anew', () => {
    const budget = new MarkBudget(10);
    budget.claim('a', 3);
    expect(budget.room('a')).toBe(10);
    expect(budget.room('b')).toBe(7);
    expect(budget.claim('a', 8)).toBe(8);
    expect(budget.room('b')).toBe(2);
    expect(budget.claim('a', 1)).toBe(1);
    expect(budget.room('b')).toBe(9);
  });

  it('knows when a block that is on the card was granted less than it asked for, and forgets it when the block goes', () => {
    const budget = new MarkBudget(5);
    budget.claim('a', 5);
    expect(budget.ranOut).toBe(false);
    budget.claim('b', 1);
    expect(budget.ranOut).toBe(true);
    budget.release('b');
    expect(budget.ranOut).toBe(false);
    // Giving back makes room for the block that comes next.
    budget.release('a');
    expect(budget.claim('c', 5)).toBe(5);
  });

  it('puts a block back as it was, when an effect gave it up and drew it again (hold)', () => {
    const budget = new MarkBudget(5);
    budget.claim('a', 3);
    budget.release('a');
    budget.hold('a', 3, false);
    expect(budget.room('b')).toBe(2);
    budget.hold('b', 2, true);
    expect(budget.ranOut).toBe(true);
    // Holding what it already holds changes nothing.
    budget.hold('a', 99, false);
    expect(budget.room('c')).toBe(0);
  });

  it('tells those who watch once it has run out, only from `settle`, and not again for the same answer', () => {
    const budget = new MarkBudget(2);
    const listener = vi.fn();
    const stop = budget.subscribe(listener);
    budget.claim('a', 3);
    expect(listener).not.toHaveBeenCalled(); // drawing is not the moment to tell anyone
    expect(budget.getSnapshot()).toBe(false);
    budget.settle();
    expect(listener).toHaveBeenCalledTimes(1);
    expect(budget.getSnapshot()).toBe(true);
    budget.settle();
    expect(listener).toHaveBeenCalledTimes(1);
    budget.release('a');
    budget.settle();
    expect(listener).toHaveBeenCalledTimes(2);
    expect(budget.getSnapshot()).toBe(false);
    stop();
    budget.claim('b', 3);
    budget.settle();
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it('does not grant less than nothing', () => {
    const budget = new MarkBudget(3);
    expect(budget.claim('a', -5)).toBe(0);
    expect(budget.claim('b', 0)).toBe(0);
    expect(budget.ranOut).toBe(false);
  });
});
