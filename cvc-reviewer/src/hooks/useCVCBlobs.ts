import { useQuery } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthContext";
import { createGithubClient } from "../api/github";
import {
  normalizeInteraction,
  type CVCBlobData,
  type InteractionNode,
} from "../types/cvc";

// Helper to fetch raw blob content as JSON
async function fetchBlobJson<T>(url: string, token: string): Promise<T> {
  const response = await fetch(url, {
    headers: {
      Authorization: `token ${token}`,
      Accept: "application/vnd.github.v3.raw", // Request raw content
    },
  });
  if (!response.ok)
    throw new Error(`Failed to fetch blob: ${response.statusText}`);
  return response.json();
}

export function useCVCBlobs(owner: string, repo: string) {
  const { isAuthenticated, acquireToken } = useAuth();

  return useQuery({
    queryKey: ["cvc-blobs", owner, repo],
    queryFn: async (): Promise<InteractionNode[]> => {
      const currentToken = await acquireToken(owner, repo);
      if (!currentToken) throw new Error("Could not acquire token");
      
      const client = createGithubClient(currentToken);

      try {
        // 1. Get the Tree for refs/cvc/main
        // We use the git/trees API.
        // Note: We need to resolve the ref first, or pass the ref name if supported.
        // Octokit getTree requires a SHA.
        // So first, getRef.

        const { data: refData } = await client.octokit.rest.git.getRef({
          owner,
          repo,
          ref: "cvc/main", // refs/cvc/main
        });

        const treeSha = refData.object.sha;

        const { data: treeData } = await client.octokit.rest.git.getTree({
          owner,
          repo,
          tree_sha: treeSha,
          recursive: "true",
        });

        // 2. Only node blobs are interaction documents. Format-v3 also contains
        // FORMAT, by-commit pointers, and append-only links/* records; parsing any
        // of those as a node would corrupt this diagnostic view.
        const blobs = treeData.tree.filter(
          (item) =>
            item.type === "blob" &&
            !!item.path &&
            (item.path.startsWith("nodes/") ||
              (!item.path.includes("/") && item.path.endsWith(".json"))),
        );

        // 3. Fetch all blobs in parallel and normalize them
        // In a real app with thousands of nodes, we would paginate or lazy load.
        // For this phase, we fetch all but with concurrency limit.
        const BATCH_SIZE = 10;
        const blobDataList: CVCBlobData[] = [];

        for (let i = 0; i < blobs.length; i += BATCH_SIZE) {
          const batch = blobs.slice(i, i + BATCH_SIZE);
          const results = await Promise.all(
            batch.map((blob) => fetchBlobJson<CVCBlobData>(blob.url!, currentToken))
          );
          blobDataList.push(...results);
        }

        // 4. Normalize to InteractionNode format
        return blobDataList.map(normalizeInteraction);
      } catch (error: unknown) {
        // Handle 404 (Ref not found) gracefully - means no CVC history yet
        if (
          error &&
          typeof error === "object" &&
          "status" in error &&
          error.status === 404
        ) {
          return [];
        }
        throw error;
      }
    },
    enabled: isAuthenticated && !!owner && !!repo,
    staleTime: 1000 * 60 * 5, // 5 minutes
  });
}
