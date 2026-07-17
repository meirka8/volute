import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { usePRInteractions } from "./usePRInteractions";

const mockAcquireToken = vi.fn();
vi.mock("../auth/AuthContext", () => ({
  useAuth: () => ({
    isAuthenticated: true,
    acquireToken: mockAcquireToken,
  }),
}));

const mockGetRef = vi.fn();
const mockGetTree = vi.fn();
const mockGetContent = vi.fn();
const mockListCommits = vi.fn();

vi.mock("../api/github", () => ({
  createGithubClient: () => ({
    octokit: {
      rest: {
        git: { getRef: mockGetRef, getTree: mockGetTree },
        repos: { getContent: mockGetContent },
        pulls: { listCommits: mockListCommits },
      },
    },
  }),
}));

const CVC_MAIN_SHA = "cvcmain0000000000000000000000000000000";
const ID_ONE = "11111111-1111-1111-1111-111111111111";
const ID_TWO = "22222222-2222-2222-2222-222222222222";
const ID_THREE = "33333333-3333-3333-3333-333333333333";
const TEMPORAL_COMMIT_SHA = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

function notFoundError() {
  const error = new Error("Not Found") as Error & { status: number };
  error.status = 404;
  return error;
}

function rawInteractionBlob(id: string, commitShas: string[] = []) {
  return {
    interaction: {
      id,
      conversation_id: "conv-1",
      parent_id: null,
      timestamp: "2026-01-01T00:00:00Z",
      author: "human",
      user_prompt: `prompt for ${id}`,
    },
    context_items: [],
    tool_executions: [],
    artifact_links: commitShas.map((sha) => ({
      interaction_id: id,
      git_commit_hash: sha,
      link_type: "generated",
    })),
  };
}

function renderWithClient(owner: string, repo: string, prNumber: number) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  );
  return renderHook(() => usePRInteractions(owner, repo, prNumber), { wrapper });
}

function setupV3LinkEvent(payload: unknown) {
  mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
  mockListCommits.mockResolvedValue({ data: [{ sha: TEMPORAL_COMMIT_SHA }] });
  mockGetTree.mockResolvedValue({
    data: {
      tree: [
        { path: "by-commit", type: "tree" },
        { path: "nodes", type: "tree" },
        { path: "links", type: "tree" },
      ],
    },
  });
  mockGetContent.mockResolvedValue({ data: [{ name: ID_TWO }] });
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((url: string) => {
      if (url.includes(`links/${ID_TWO}/${TEMPORAL_COMMIT_SHA}.json`)) {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(payload) });
      }
      return Promise.resolve({
        ok: true,
        text: () => Promise.resolve(JSON.stringify(rawInteractionBlob(ID_TWO))),
      });
    }),
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mockAcquireToken.mockResolvedValue("fake-token");
  vi.stubGlobal("fetch", vi.fn());
});

describe("usePRInteractions", () => {
  it("returns hasHistory: false when refs/cvc/main doesn't exist yet", async () => {
    mockGetRef.mockRejectedValue(notFoundError());

    const { result } = renderWithClient("owner", "repo", 1);

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toEqual({
      interactions: [],
      truncated: false,
      hasHistory: false,
    });
    // No point resolving PR commits if there's nothing to look up.
    expect(mockListCommits).not.toHaveBeenCalled();
  });

  it("v2 path: preserves legacy node links without linked_by", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }, { sha: "commit-b" }] });
    mockGetTree.mockResolvedValue({
      data: { tree: [{ path: "by-commit", type: "tree" }, { path: "nodes", type: "tree" }] },
    });
    mockGetContent.mockImplementation(({ path }: { path: string }) => {
      if (path === "by-commit/commit-a") {
        return Promise.resolve({ data: [{ name: ID_ONE }] });
      }
      if (path === "by-commit/commit-b") {
        // No thoughts recorded for this commit -- the common case.
        return Promise.reject(notFoundError());
      }
      throw new Error(`unexpected getContent path: ${path}`);
    });

    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(JSON.stringify(rawInteractionBlob(ID_ONE, ["commit-a"]))),
    });
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderWithClient("owner", "repo", 1);

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.hasHistory).toBe(true);
    expect(result.current.data?.truncated).toBe(false);
    expect(result.current.data?.interactions).toHaveLength(1);
    expect(result.current.data?.interactions[0].id).toBe(ID_ONE);
    expect(result.current.data?.interactions[0].artifact_links[0].link_type).toBe("generated");
    expect(result.current.data?.interactions[0].artifact_links[0].linked_by).toBeUndefined();

    // Exactly one by-commit lookup per PR commit, and only one blob fetched
    // (the tree walk / whole-history path must never be touched here).
    expect(mockGetContent).toHaveBeenCalledTimes(2);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toContain(`nodes/${ID_ONE.slice(0, 2)}/${ID_ONE}.json`);
    expect(mockGetTree).toHaveBeenCalledWith(
      expect.objectContaining({ recursive: "false" }),
    );
  });

  it("v3 path: merges append-only temporal link metadata into a floating node", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: TEMPORAL_COMMIT_SHA }] });
    mockGetTree.mockResolvedValue({
      data: {
        tree: [
          { path: "by-commit", type: "tree" },
          { path: "nodes", type: "tree" },
          { path: "links", type: "tree" },
          { path: "FORMAT", type: "blob" },
        ],
      },
    });
    mockGetContent.mockResolvedValue({ data: [{ name: ID_TWO }] });
    const fetchMock = vi.fn().mockImplementation((url: string) => {
      if (url.includes(`links/${ID_TWO}/${TEMPORAL_COMMIT_SHA}.json`)) {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () => Promise.resolve({
            interaction_id: ID_TWO,
            git_commit_hash: TEMPORAL_COMMIT_SHA,
            link_type: "temporal",
            linked_by: "Ada Example <ada@example.test>",
          }),
        });
      }
      return Promise.resolve({
        ok: true,
        text: () => Promise.resolve(JSON.stringify(rawInteractionBlob(ID_TWO))),
      });
    });
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderWithClient("owner", "repo", 3);

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.interactions).toHaveLength(1);
    expect(result.current.data?.interactions[0].artifact_links).toEqual([
      {
        interaction_id: ID_TWO,
        git_commit_hash: TEMPORAL_COMMIT_SHA,
        link_type: "temporal",
        linked_by: "Ada Example <ada@example.test>",
      },
    ]);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("fails closed when a by-commit index entry is not an interaction UUID", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }] });
    mockGetTree.mockResolvedValue({ data: { tree: [{ path: "by-commit", type: "tree" }] } });
    mockGetContent.mockResolvedValue({ data: [{ name: "not-a-uuid" }] });

    const { result } = renderWithClient("owner", "repo", 4);

    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(globalThis.fetch).not.toHaveBeenCalled();
  });

  it("fails closed when a v3 event payload does not match its indexed path", async () => {
    setupV3LinkEvent({
      interaction_id: ID_THREE,
      git_commit_hash: TEMPORAL_COMMIT_SHA,
      link_type: "temporal",
    });

    const { result } = renderWithClient("owner", "repo", 5);

    await waitFor(() => expect(result.current.isError).toBe(true));
  });

  it("fails closed when a v3 event has an unsupported link type", async () => {
    setupV3LinkEvent({
      interaction_id: ID_TWO,
      git_commit_hash: TEMPORAL_COMMIT_SHA,
      link_type: "verified",
    });

    const { result } = renderWithClient("owner", "repo", 6);

    await waitFor(() => expect(result.current.isError).toBe(true));
  });

  it("fails closed when a v3 event has an invalid commit SHA", async () => {
    setupV3LinkEvent({
      interaction_id: ID_TWO,
      git_commit_hash: "not-a-sha",
      link_type: "temporal",
    });

    const { result } = renderWithClient("owner", "repo", 7);

    await waitFor(() => expect(result.current.isError).toBe(true));
  });

  it("deduplicates an interaction referenced by multiple PR commits into a single blob fetch", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }, { sha: "commit-b" }] });
    mockGetTree.mockResolvedValue({
      data: { tree: [{ path: "by-commit", type: "tree" }] },
    });
    // Same interaction id shows up under both commits' by-commit index.
    mockGetContent.mockResolvedValue({ data: [{ name: ID_THREE }] });

    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: () =>
        Promise.resolve(
            JSON.stringify(rawInteractionBlob(ID_THREE, ["commit-a", "commit-b"])),
        ),
    });
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderWithClient("owner", "repo", 2);

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.interactions).toHaveLength(1);
    // The blob is fetched exactly once even though two commits referenced it.
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("v1 fallback: walks the whole tree, caps it, and flags truncated", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }] });

    mockGetTree.mockImplementation(
      ({ recursive }: { recursive: string }) => {
        if (recursive === "false") {
          // No by-commit/ entry at all -- this is a pre-HEL-65 repo.
          return Promise.resolve({ data: { tree: [] } });
        }
        const tree = Array.from({ length: 250 }, (_, i) => ({
          path: `id-${i}.json`,
          type: "blob",
          url: `https://api.github.com/blob/id-${i}`,
        }));
        return Promise.resolve({ data: { tree } });
      },
    );

    const fetchMock = vi.fn().mockImplementation((url: string) => {
      const match = /id-(\d+)/.exec(url);
      const id = `id-${match ? match[1] : "0"}`;
      // Only interactions linked to "commit-a" should survive the PR-range filter.
      const linkedCommits = id === "id-0" ? ["commit-a"] : [];
      return Promise.resolve({
        ok: true,
        json: () => Promise.resolve(rawInteractionBlob(id, linkedCommits)),
      });
    });
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderWithClient("owner", "repo", 1);

    await waitFor(() => expect(result.current.isSuccess).toBe(true), { timeout: 5000 });
    expect(result.current.data?.hasHistory).toBe(true);
    expect(result.current.data?.truncated).toBe(true);
    // Only the interaction actually linked to the PR's commit should surface,
    // even in the capped fallback.
    expect(result.current.data?.interactions).toHaveLength(1);
    expect(result.current.data?.interactions[0].id).toBe("id-0");
    // Capped at 200, not all 250 fake blobs.
    expect(fetchMock).toHaveBeenCalledTimes(200);
  });

  it("caches by-commit and node lookups forever across renders of different PRs", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockGetTree.mockResolvedValue({ data: { tree: [{ path: "by-commit", type: "tree" }] } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }] });
    mockGetContent.mockResolvedValue({ data: [{ name: ID_ONE }] });

    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(JSON.stringify(rawInteractionBlob(ID_ONE, ["commit-a"]))),
    });
    vi.stubGlobal("fetch", fetchMock);

    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    const first = renderHook(() => usePRInteractions("owner", "repo", 1), { wrapper });
    await waitFor(() => expect(first.result.current.isSuccess).toBe(true));

    // A second PR that happens to reference the same commit/interaction should
    // reuse the cached by-commit and node lookups instead of refetching them.
    const second = renderHook(() => usePRInteractions("owner", "repo", 2), { wrapper });
    await waitFor(() => expect(second.result.current.isSuccess).toBe(true));

    expect(mockGetContent).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(second.result.current.data?.interactions).toHaveLength(1);
  });

  it("refreshes a by-commit directory when refs/cvc/main advances for a late link", async () => {
    mockGetRef
      .mockResolvedValueOnce({ data: { object: { sha: "cvc-before" } } })
      .mockResolvedValueOnce({ data: { object: { sha: "cvc-after" } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }] });
    mockGetTree.mockResolvedValue({ data: { tree: [{ path: "by-commit", type: "tree" }] } });
    mockGetContent
      .mockResolvedValueOnce({ data: [{ name: ID_ONE }] })
      .mockResolvedValueOnce({ data: [{ name: ID_TWO }] });
    const fetchMock = vi.fn().mockImplementation((url: string) => {
      const id = url.includes(ID_TWO) ? ID_TWO : ID_ONE;
      return Promise.resolve({
        ok: true,
        text: () => Promise.resolve(JSON.stringify(rawInteractionBlob(id, ["commit-a"]))),
      });
    });
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderWithClient("owner", "repo", 1);
    await waitFor(() => expect(result.current.data?.interactions[0]?.id).toBe(ID_ONE));

    await result.current.refetch();
    await waitFor(() => expect(result.current.data?.interactions[0]?.id).toBe(ID_TWO));
    expect(mockGetContent).toHaveBeenCalledTimes(2);
  });
});
