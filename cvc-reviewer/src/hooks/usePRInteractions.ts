import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthContext";
import { createGithubClient, type GithubClient } from "../api/github";
import { CommitRanger } from "../lib/CommitRanger";
import { InteractionMapper } from "../lib/InteractionMapper";
import {
  normalizeInteraction,
  type CVCBlobData,
  type InteractionNode,
} from "../types/cvc";

const CVC_REF = "cvc/main";

// Format-v1 repos have no by-commit/ index to narrow the fetch with, so the fallback
// walks the whole tree like before HEL-65 -- but capped, so a large repo can't burn
// the caller's GitHub rate limit or hang the UI. See the "truncated" result field.
const LEGACY_FETCH_CAP = 200;

const BATCH_SIZE = 10;

export interface PRInteractionsResult {
  interactions: InteractionNode[];
  /** True when we hit the legacy fallback's cap -- the result may be incomplete. */
  truncated: boolean;
  /** False when refs/cvc/main doesn't exist at all yet (no CVC history, not an error). */
  hasHistory: boolean;
}

function isNotFound(error: unknown): boolean {
  return (
    !!error &&
    typeof error === "object" &&
    "status" in error &&
    (error as { status: unknown }).status === 404
  );
}

function idFromBlobPath(path: string): string {
  const base = path.split("/").pop() ?? path;
  return base.endsWith(".json") ? base.slice(0, -".json".length) : base;
}

async function fetchRawFile(
  owner: string,
  repo: string,
  path: string,
  ref: string,
  token: string,
): Promise<string> {
  const url = `https://api.github.com/repos/${owner}/${repo}/contents/${path}?ref=${ref}`;
  const response = await fetch(url, {
    headers: {
      Authorization: `token ${token}`,
      Accept: "application/vnd.github.v3.raw",
    },
  });
  if (!response.ok) {
    throw new Error(`Failed to fetch ${path}: ${response.statusText}`);
  }
  return response.text();
}

async function fetchBlobJson<T>(url: string, token: string): Promise<T> {
  const response = await fetch(url, {
    headers: {
      Authorization: `token ${token}`,
      Accept: "application/vnd.github.v3.raw",
    },
  });
  if (!response.ok) {
    throw new Error(`Failed to fetch blob: ${response.statusText}`);
  }
  return response.json();
}

// Path-based directory listing: resolves server-side in one request regardless of
// tree depth, and 404s cleanly when the path doesn't exist (e.g. a PR commit with no
// recorded thoughts) instead of requiring a separate existence check.
async function fetchDirectoryListing(
  client: GithubClient,
  owner: string,
  repo: string,
  path: string,
  ref: string,
): Promise<{ name: string }[]> {
  try {
    const { data } = await client.octokit.rest.repos.getContent({
      owner,
      repo,
      path,
      ref,
    });
    return Array.isArray(data) ? data : [];
  } catch (error: unknown) {
    if (isNotFound(error)) return [];
    throw error;
  }
}

// by-commit/<sha> is only ever written to once a commit is created and its floating
// thoughts get linked (see linker::link_current_commit_to_floating_nodes) -- nothing
// later adds more entries to an already-processed commit. Safe to cache forever.
function fetchByCommitCached(
  queryClient: QueryClient,
  client: GithubClient,
  owner: string,
  repo: string,
  commitSha: string,
  cvcMainSha: string,
): Promise<{ name: string }[]> {
  return queryClient.fetchQuery({
    queryKey: ["cvc-by-commit", owner, repo, commitSha],
    queryFn: () =>
      fetchDirectoryListing(client, owner, repo, `by-commit/${commitSha}`, cvcMainSha),
    staleTime: Infinity,
  });
}

// Interaction blobs are content-addressed by id and never rewritten once pushed (see
// cvc_core::sync's "immutable blobs" rule) -- cache forever, independent of which PR
// or ref happened to reference them.
function fetchNodeCached(
  queryClient: QueryClient,
  owner: string,
  repo: string,
  id: string,
  fetchData: () => Promise<CVCBlobData>,
): Promise<InteractionNode> {
  return queryClient.fetchQuery({
    queryKey: ["cvc-node", owner, repo, id],
    queryFn: async () => normalizeInteraction(await fetchData()),
    staleTime: Infinity,
  });
}

async function fetchNodesInBatches<T>(
  items: T[],
  fetchOne: (item: T) => Promise<InteractionNode>,
): Promise<InteractionNode[]> {
  const nodes: InteractionNode[] = [];
  for (let i = 0; i < items.length; i += BATCH_SIZE) {
    const batch = items.slice(i, i + BATCH_SIZE);
    nodes.push(...(await Promise.all(batch.map(fetchOne))));
  }
  return nodes;
}

// Format-v1 fallback: no by-commit/ index exists, so there's no cheap way to find just
// this PR's interactions without downloading (nearly) everything. Cap it, and still
// filter to the PR's commits so the result is useful when it does fit under the cap.
async function fetchLegacyCapped(
  queryClient: QueryClient,
  client: GithubClient,
  owner: string,
  repo: string,
  treeSha: string,
  commitShas: string[],
  token: string,
): Promise<PRInteractionsResult> {
  const { data: treeData } = await client.octokit.rest.git.getTree({
    owner,
    repo,
    tree_sha: treeSha,
    recursive: "true",
  });

  const blobs = treeData.tree.filter(
    (item) => item.type === "blob" && item.path?.endsWith(".json"),
  );
  const truncated = blobs.length > LEGACY_FETCH_CAP;
  const capped = blobs.slice(0, LEGACY_FETCH_CAP);

  const allInteractions = await fetchNodesInBatches(capped, (blob) =>
    fetchNodeCached(queryClient, owner, repo, idFromBlobPath(blob.path!), () =>
      fetchBlobJson<CVCBlobData>(blob.url!, token),
    ),
  );

  const mapper = new InteractionMapper(allInteractions);
  return {
    interactions: mapper.getInteractionsForRange(commitShas),
    truncated,
    hasHistory: true,
  };
}

/**
 * PR-scoped CVC history: resolves the PR's commit SHAs, looks up which ones have
 * linked thoughts via the by-commit/ index, and fetches only the referenced
 * interaction blobs -- instead of downloading the entire cognitive history and
 * filtering client-side. Falls back to a capped whole-tree walk for repos synced
 * before the v2 layout existed (see cvc_core::sync::push_to_ref).
 */
export function usePRInteractions(owner: string, repo: string, prNumber: number) {
  const { isAuthenticated, acquireToken } = useAuth();
  const queryClient = useQueryClient();

  return useQuery<PRInteractionsResult>({
    queryKey: ["pr-interactions", owner, repo, prNumber],
    queryFn: async (): Promise<PRInteractionsResult> => {
      const token = await acquireToken(owner, repo);
      if (!token) throw new Error("Could not acquire token");
      const client = createGithubClient(token);

      let cvcMainSha: string;
      try {
        const { data: refData } = await client.octokit.rest.git.getRef({
          owner,
          repo,
          ref: CVC_REF,
        });
        cvcMainSha = refData.object.sha;
      } catch (error: unknown) {
        if (isNotFound(error)) {
          return { interactions: [], truncated: false, hasHistory: false };
        }
        throw error;
      }

      const ranger = new CommitRanger(client);
      const commitShas = await ranger.getCommitShas(owner, repo, prNumber);

      const { data: rootTree } = await client.octokit.rest.git.getTree({
        owner,
        repo,
        tree_sha: cvcMainSha,
        recursive: "false",
      });
      const isV2 = rootTree.tree.some(
        (entry) => entry.path === "by-commit" && entry.type === "tree",
      );

      if (!isV2) {
        return fetchLegacyCapped(
          queryClient,
          client,
          owner,
          repo,
          cvcMainSha,
          commitShas,
          token,
        );
      }

      const perCommitEntries = await Promise.all(
        commitShas.map((sha) =>
          fetchByCommitCached(queryClient, client, owner, repo, sha, cvcMainSha),
        ),
      );

      const ids = new Set<string>();
      for (const entries of perCommitEntries) {
        for (const entry of entries) {
          ids.add(entry.name);
        }
      }

      const interactions = await fetchNodesInBatches(Array.from(ids), (id) =>
        fetchNodeCached(queryClient, owner, repo, id, async () => {
          const path = `nodes/${id.slice(0, 2)}/${id}.json`;
          const raw = await fetchRawFile(owner, repo, path, cvcMainSha, token);
          return JSON.parse(raw) as CVCBlobData;
        }),
      );

      return { interactions, truncated: false, hasHistory: true };
    },
    enabled: isAuthenticated && !!owner && !!repo && prNumber > 0,
    // refs/cvc/main advances as new thoughts are pushed; re-check periodically. The
    // per-blob and per-commit lookups underneath this are cached separately (see
    // fetchByCommitCached/fetchNodeCached) with staleTime: Infinity, since their
    // content is immutable once written.
    staleTime: 1000 * 60,
  });
}
