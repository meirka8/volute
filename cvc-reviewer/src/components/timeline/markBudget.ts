import { createContext } from 'react';

/**
 * The most matches marked on one card (issue #118), on top of the most in one block of text (100).
 *
 * A mark is an element with two text nodes around it, and a card can have thousands of blocks (a diff draws a block
 * for every line): a diff of two thousand lines searched for "e" drew 32,007 marks, which is a page that takes a
 * second to draw and a wall of yellow nobody can read. 500 is five full blocks. It is more highlights than a reader
 * can follow on one card (the next and previous arrows move between steps, and a reader who sees five hundred marks
 * needs a narrower term, not more marks), and it costs a few milliseconds to draw, where 32,000 cost seconds.
 * It limits what is drawn: the step still matches, the folds still count every match they hold, and the card
 * says that the marks stopped (see `MarkBudgetNotice`).
 */
export const MARKS_PER_CARD = 500;

/**
 * What one card has left to mark, shared by the text blocks of the card for as long as the search stays as it is.
 *
 * A block asks for its marks while it draws (that is the point: it does not draw the ones that are over), so the
 * answer has to be the same every time it asks, however often React draws it again (twice in development, again
 * when its parent does): a block is an owner, and what it was granted is kept for it. A block that goes away gives
 * its marks back, so folding a long text away and opening it again finds the room it had.
 *
 * The budget never tells anyone while a card is being drawn (that would be an update of one component while
 * another draws): `settle` is called when a block has been put on the page, and tells those who watch whether the
 * budget has run out by then.
 */
export class MarkBudget {
  private readonly grants = new Map<string, number>();
  /** The owners whose last request was granted in part only. */
  private readonly denied = new Set<string>();
  private used = 0;
  private told = false;
  private readonly listeners = new Set<() => void>();
  readonly limit: number;

  constructor(limit: number = MARKS_PER_CARD) {
    this.limit = limit;
  }

  /** How many marks `owner` could be granted, counting the ones it has now. */
  room(owner: string): number {
    return this.limit - this.used + (this.grants.get(owner) ?? 0);
  }

  /**
   * Grants `owner` up to `wanted` marks: the same answer for the same owner asking for the same, and a new one when it
   * asks for something else. Refusing any of them is remembered (see `ranOut`).
   */
  claim(owner: string, wanted: number): number {
    const had = this.grants.get(owner);
    if (had !== undefined) this.used -= had;
    const granted = Math.max(0, Math.min(wanted, this.limit - this.used));
    this.used += granted;
    this.grants.set(owner, granted);
    if (granted < wanted) this.denied.add(owner);
    else this.denied.delete(owner);
    return granted;
  }

  /** Makes sure `owner` holds `granted` marks (it did, before an effect gave them back and drew it again). */
  hold(owner: string, granted: number, denied: boolean): void {
    if (this.grants.has(owner)) return;
    this.grants.set(owner, granted);
    this.used += granted;
    if (denied) this.denied.add(owner);
  }

  /** Gives back what `owner` holds. */
  release(owner: string): void {
    const had = this.grants.get(owner);
    if (had === undefined) return;
    this.grants.delete(owner);
    this.denied.delete(owner);
    this.used -= had;
  }

  /** Whether a block that is on the card asked for more marks than it could be given. */
  get ranOut(): boolean {
    return this.denied.size > 0;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  getSnapshot = (): boolean => this.told;

  /** Tells the watchers, if the budget ran out since they were told. Called from an effect, never while drawing. */
  settle(): void {
    const now = this.ranOut;
    if (this.told === now) return;
    this.told = now;
    for (const listener of [...this.listeners]) listener();
  }
}

/** The budget of the card a block is on, if it is on one. A block drawn on its own has none and is limited by its own cap alone. */
export const MarkBudgetContext = createContext<MarkBudget | null>(null);
