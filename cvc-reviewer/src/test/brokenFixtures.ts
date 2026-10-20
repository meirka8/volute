/**
 * Nodes that no layout can draw. They live apart from `fixtures.ts` because tests
 * walk every node exported there and render it; these are for the one place that
 * must survive them (the per-node error boundary in ShadowTimeline).
 */
import { makeNode } from './fixtures';

/**
 * A node the triad layout cannot draw: its model name is an object, which React
 * refuses to render. Real blobs are validated, but old ones were not; a card
 * that cannot be drawn must not take the timeline with it.
 */
export const undrawableNode = makeNode({
  id: 'e1d2c3b4-a596-4879-8a6b-5c4d3e2f1a0b',
  author: 'human',
  timestamp: Date.parse('2026-03-02T09:15:00.000Z') + 45 * 60_000,
  model_name: { family: 'broken' } as unknown as string,
  user_prompt: 'A node from before blobs were validated.',
  model_response: 'It cannot be drawn.',
});
