import type { QueryClient } from "@tanstack/react-query";
import { del as idbDel } from "idb-keyval";

export const REVIEWER_QUERY_CACHE_KEY = "cvc-reviewer-query-cache";

/** Remove every cache entry that can contain a cognitive-graph projection. */
export function purgeCognitiveCache(queryClient: QueryClient, owner?: string, repo?: string, prNumber?: number) {
  const scopes: readonly unknown[][] = [
    ["cvc-node", owner, repo],
    ["cvc-by-commit", owner, repo],
    ["cvc-blobs", owner, repo],
  ];
  for (const queryKey of scopes) queryClient.removeQueries({ queryKey });

  // Removing the query currently executing its queryFn also removes its observer,
  // which leaves a hook stuck pending. Replace its projection with an explicitly
  // empty result instead; the subsequent validation error remains observable but
  // no previous interactions can render.
  if (prNumber !== undefined) {
    queryClient.setQueryData(["pr-interactions", owner, repo, prNumber], {
      interactions: [],
      truncated: false,
      hasHistory: false,
    });
  } else {
    // A non-PR cognitive reader (the diagnostic blob view) has discovered an
    // untrustworthy namespace: invalidate every PR projection for this repo too.
    queryClient.removeQueries({ queryKey: ["pr-interactions", owner, repo] });
  }
}

/** Account boundaries must not retain either hydrated or persisted reviewer data. */
export async function clearReviewerCaches(queryClient: QueryClient): Promise<void> {
  queryClient.clear();
  await idbDel(REVIEWER_QUERY_CACHE_KEY);
}
