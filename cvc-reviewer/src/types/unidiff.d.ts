/**
 * `unidiff` ships no type declarations. This covers the one function we use:
 * it re-exports jsdiff's `diffArrays`, which compares two arrays element by
 * element (here: lines) and returns runs of equal, removed and added elements.
 */
declare module 'unidiff' {
  export interface ArrayChange {
    count?: number;
    value: string[];
    added?: boolean;
    removed?: boolean;
  }

  export interface DiffOptions {
    /**
     * Gives up once the edit script would need more than this many insertions
     * and deletions. The call then returns `undefined` instead of running on.
     */
    maxEditLength?: number;
    /** Gives up, the same way, after this many milliseconds. */
    timeout?: number;
  }

  interface Unidiff {
    diffArrays(
      oldLines: string[],
      newLines: string[],
      options?: DiffOptions,
    ): ArrayChange[] | undefined;
  }

  const unidiff: Unidiff;
  export default unidiff;
}
