/** Class names shared by the transcript layout's panes. Same tokens as the triad layout. */

/** The small upper-case heading of a pane; add a colour token (`text-action`, `text-muted`). */
export const PANE_LABEL = 'text-[11px] font-semibold uppercase tracking-[0.18em]';

/** A pill-shaped badge; add border/background/text tokens. */
export const BADGE = 'rounded-full px-2.5 py-1 text-[11px] font-medium';

export const BADGE_NEUTRAL = `${BADGE} border border-line bg-canvas/80 text-muted`;
export const BADGE_SUCCESS = `${BADGE} border border-success/20 bg-success/10 text-success`;
export const BADGE_DANGER = `${BADGE} border border-danger/20 bg-danger/10 text-danger`;

/** A small option chip on a tool call, e.g. "timeout 120s". */
export const CHIP = 'rounded-full border border-line bg-canvas/80 px-2 py-0.5 font-mono text-[11px] text-muted';
