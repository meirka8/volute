/**
 * How many items of a list-shaped part of a node are drawn at once.
 *
 * The data behind each list is kept whole (the view models are lossless); these
 * caps apply when it is drawn. A node can be a megabyte of crafted text, and
 * drawing every item of it as DOM would be hundreds of thousands of elements, so
 * each list draws one page and offers "Show N more …" for the rest, one page per
 * click.
 *
 * The lists nest (a step's tool cards hold edits, which hold diff rows, next to
 * field rows), so what a step draws at first is roughly the PRODUCT of the pages
 * along one path. The pages of the lists that sit inside one another are kept
 * small for that reason: a step built to hit every cap at once (700 KB of
 * MultiEdits and many-field calls) draws about 1,800 elements at first, where
 * pages of 50, 20 and 100 drew about 34,000. Each "Show N more" asks for one
 * more page of a single list.
 */
export const RENDER_LIMITS = {
  /** Blocks of one step's stimulus (prompts, notes, groups of tool results). */
  stimulusBlocks: 100,
  /** Tool results drawn in one group of them. */
  toolResultsPerGroup: 20,
  /** Items of one TodoWrite checklist. */
  todos: 100,
  /** Key/value rows of one tool call (its fields, and its extras). */
  fieldRows: 25,
  /** Tool-call cards of one step. */
  toolCalls: 10,
  /** Edits of one MultiEdit, each of them a diff. */
  edits: 5,
} as const;
