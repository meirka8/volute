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

  it("v2 path: looks up by-commit/<sha> per PR commit and fetches only the referenced blobs", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }, { sha: "commit-b" }] });
    mockGetTree.mockResolvedValue({
      data: { tree: [{ path: "by-commit", type: "tree" }, { path: "nodes", type: "tree" }] },
    });
    mockGetContent.mockImplementation(({ path }: { path: string }) => {
      if (path === "by-commit/commit-a") {
        return Promise.resolve({ data: [{ name: "id-1" }] });
      }
      if (path === "by-commit/commit-b") {
        // No thoughts recorded for this commit -- the common case.
        return Promise.reject(notFoundError());
      }
      throw new Error(`unexpected getContent path: ${path}`);
    });

    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(JSON.stringify(rawInteractionBlob("id-1", ["commit-a"]))),
    });
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderWithClient("owner", "repo", 1);

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.hasHistory).toBe(true);
    expect(result.current.data?.truncated).toBe(false);
    expect(result.current.data?.interactions).toHaveLength(1);
    expect(result.current.data?.interactions[0].id).toBe("id-1");

    // Exactly one by-commit lookup per PR commit, and only one blob fetched
    // (the tree walk / whole-history path must never be touched here).
    expect(mockGetContent).toHaveBeenCalledTimes(2);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toContain("nodes/id/id-1.json");
    expect(mockGetTree).toHaveBeenCalledWith(
      expect.objectContaining({ recursive: "false" }),
    );
  });

  it("deduplicates an interaction referenced by multiple PR commits into a single blob fetch", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }, { sha: "commit-b" }] });
    mockGetTree.mockResolvedValue({
      data: { tree: [{ path: "by-commit", type: "tree" }] },
    });
    // Same interaction id shows up under both commits' by-commit index.
    mockGetContent.mockResolvedValue({ data: [{ name: "shared-id" }] });

    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: () =>
        Promise.resolve(
          JSON.stringify(rawInteractionBlob("shared-id", ["commit-a", "commit-b"])),
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
    mockGetContent.mockResolvedValue({ data: [{ name: "id-1" }] });

    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(JSON.stringify(rawInteractionBlob("id-1", ["commit-a"]))),
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
});
