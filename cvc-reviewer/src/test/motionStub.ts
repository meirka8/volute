import { createElement, forwardRef, Fragment, type ReactNode } from 'react';

/**
 * A stand-in for the parts of `framer-motion` the timeline uses, so component
 * tests assert on our markup rather than on the animation library's inline
 * styles (which change between library versions).
 *
 * Usage, at the top of a test file (vitest hoists `vi.mock`):
 *
 *   vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);
 */
const ANIMATION_PROPS = ['layout', 'initial', 'animate', 'exit', 'transition'];

const motionDiv = forwardRef<HTMLDivElement, Record<string, unknown>>((props, ref) => {
  const domProps: Record<string, unknown> = { ...props };
  for (const name of ANIMATION_PROPS) delete domProps[name];
  return createElement('div', { ...domProps, ref });
});
motionDiv.displayName = 'MotionDivStub';

export const framerMotionStub = {
  motion: { div: motionDiv },
  AnimatePresence: ({ children }: { children?: ReactNode }) => createElement(Fragment, null, children),
};
