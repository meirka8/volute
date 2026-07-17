import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthContext";
import { createGithubClient, type GithubClient } from "../api/github";
import { CommitRanger } from "../lib/CommitRanger";
import { InteractionMapper } from "../lib/InteractionMapper";
import {
  normalizeInteraction,
  mergeArtifactLinks,
  type CVCBlobData,
  type CVCLinkRecord,
  type InteractionNode,
} from "../types/cvc";

const CVC_REF = "cvc/main";

// Format-v1 repos have no by-commit/ index to narrow the fetch with, so the fallback
// walks the whole tree like before HEL-65 -- but capped, so a large repo can't burn
// the caller's GitHub rate limit or hang the UI. See the "truncated" result field.
const LEGACY_FETCH_CAP = 200;

const BATCH_SIZE = 10;
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const COMMIT_SHA_PATTERN = /^[0-9a-f]{40}$/i;

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

function encodeGithubPath(path: string): string {
  return path.split("/").map(encodeURIComponent).join("/");
}

function githubContentsUrl(owner: string, repo: string, path: string, ref: string): string {
  return `https://api.github.com/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/contents/${encodeGithubPath(path)}?ref=${encodeURIComponent(ref)}`;
}

function isInteractionId(value: string): boolean {
  return UUID_PATTERN.test(value);
}

function assertInteractionId(value: string): asserts value is string {
  if (!isInteractionId(value)) {
    throw new Error("Invalid interaction ID in CVC by-commit index");
  }
}

function isV3LinkRecord(
  value: unknown,
  interactionId: string,
  commitSha: string,
): value is CVCLinkRecord {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  const formatKeys = ["format", "version", "format_version"];
  if (formatKeys.some((key) => key in record && record[key] !== 3 && record[key] !== "3")) {
    return false;
  }
  return (
    typeof record.interaction_id === "string" &&
    isInteractionId(record.interaction_id) &&
    record.interaction_id === interactionId &&
    typeof record.git_commit_hash === "string" &&
    COMMIT_SHA_PATTERN.test(record.git_commit_hash) &&
    record.git_commit_hash === commitSha &&
    (record.link_type === "generated" || record.link_type === "temporal") &&
    (record.linked_by === undefined || record.linked_by === null || typeof record.linked_by === "string")
  );
}

async function fetchRawFile(
  owner: string,
  repo: string,
  path: string,
  ref: string,
  token: string,
): Promise<string> {
  const url = githubContentsUrl(owner, repo, path, ref);
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

/** Reads a v3 append-only link record. Missing records are normal for legacy links. */
async function fetchLinkRecord(
  owner: string,
  repo: string,
  interactionId: string,
  commitSha: string,
  ref: string,
  token: string,
): Promise<CVCLinkRecord | null> {
  const path = `links/${interactionId}/${commitSha}.json`;
  const url = githubContentsUrl(owner, repo, path, ref);
  const response = await fetch(url, {
    headers: { Authorization: `token ${token}`, Accept: "application/vnd.github.v3.raw" },
  });
  if (response.status === 404) return null;
  if (!response.ok) throw new Error(`Failed to fetch ${path}: ${response.statusText}`);
  const payload: unknown = await response.json();
  if (!isV3LinkRecord(payload, interactionId, commitSha)) {
    throw new Error(`Invalid v3 CVC link record at ${path}`);
  }
  return payload;
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

// A v3 late-link event can add a by-commit pointer after the node itself was first
// pushed. The directory is immutable for a particular CVC ref, so scope the cache
// key to that ref's commit instead of caching it across ref advances.
function fetchByCommitCached(
  queryClient: QueryClient,
  client: GithubClient,
  owner: string,
  repo: string,
  commitSha: string,
  cvcMainSha: string,
): Promise<{ name: string }[]> {
  return queryClient.fetchQuery({
    queryKey: ["cvc-by-commit", owner, repo, cvcMainSha, commitSha],
    queryFn: () =>
      fetchDirectoryListing(
        client,
        owner,
        repo,
        `by-commit/${encodeURIComponent(commitSha)}`,
        cvcMainSha,
      ),
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

async function fetchLinkRecordsInBatches(
  references: { interactionId: string; commitSha: string }[],
  fetchOne: (
    reference: { interactionId: string; commitSha: string },
  ) => Promise<CVCLinkRecord | null>,
): Promise<CVCLinkRecord[]> {
  const records: CVCLinkRecord[] = [];
  for (let i = 0; i < references.length; i += BATCH_SIZE) {
    const batch = references.slice(i, i + BATCH_SIZE);
    const results = await Promise.all(batch.map(fetchOne));
    records.push(...results.filter((record): record is CVCLinkRecord => record !== null));
  }
  return records;
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
      const hasByCommitIndex = rootTree.tree.some(
        (entry) => entry.path === "by-commit" && entry.type === "tree",
      );
      // Format v3 adds append-only link records under links/. The directory is absent
      // until an automatic link exists, so its presence (not FORMAT alone) determines
      // whether there are records to request.
      const hasV3LinkRecords = rootTree.tree.some(
        (entry) => entry.path === "links" && entry.type === "tree",
      );

      if (!hasByCommitIndex) {
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
      const linkReferences: { interactionId: string; commitSha: string }[] = [];
      for (const [index, entries] of perCommitEntries.entries()) {
        for (const entry of entries) {
          assertInteractionId(entry.name);
          ids.add(entry.name);
          linkReferences.push({ interactionId: entry.name, commitSha: commitShas[index] });
        }
      }

      const interactions = await fetchNodesInBatches(Array.from(ids), (id) =>
        fetchNodeCached(queryClient, owner, repo, id, async () => {
          const path = `nodes/${id.slice(0, 2)}/${id}.json`;
          const raw = await fetchRawFile(owner, repo, path, cvcMainSha, token);
          return JSON.parse(raw) as CVCBlobData;
        }),
      );

      const linkRecords = hasV3LinkRecords
        ? await fetchLinkRecordsInBatches(linkReferences, ({ interactionId, commitSha }) =>
            fetchLinkRecord(owner, repo, interactionId, commitSha, cvcMainSha, token),
          )
        : [];
      const recordsByInteraction = new Map<string, CVCLinkRecord[]>();
      for (const record of linkRecords) {
        const records = recordsByInteraction.get(record.interaction_id) ?? [];
        records.push(record);
        recordsByInteraction.set(record.interaction_id, records);
      }

      return {
        interactions: interactions.map((interaction) =>
          mergeArtifactLinks(interaction, recordsByInteraction.get(interaction.id) ?? []),
        ),
        truncated: false,
        hasHistory: true,
      };
    },
    enabled: isAuthenticated && !!owner && !!repo && prNumber > 0,
    // refs/cvc/main advances as new thoughts are pushed; re-check periodically. The
    // per-blob and per-commit lookups underneath this are cached separately (see
    // fetchByCommitCached/fetchNodeCached) with staleTime: Infinity, since their
    // content is immutable once written.
    staleTime: 1000 * 60,
  });
}
