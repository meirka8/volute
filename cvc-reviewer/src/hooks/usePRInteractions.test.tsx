import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { fetchCanonicalTombstones, usePRInteractions, validateTombstoneTree } from "./usePRInteractions";
import { EMPTY_TREE_SHA } from "../lib/format5";

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
const ID_V4 = "123e4567-e89b-42d3-a456-426614174000";
// A real id written by the Claude Code harness: RFC 9562 version 8 with the RFC variant bits.
// (Every other producer uses version 4.)
const ID_V8 = "3ec4e81b-3ffc-8d60-b160-5c0b3a2407db";
const TEMPORAL_COMMIT_SHA = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/** An id of the canonical shape with its version and variant nibbles chosen (each is one hex digit). */
const idWith = (version: string, variant: string) => `123e4567-e89b-${version}2d3-${variant}456-426614174000`;

/**
 * Every real node id the shipped harness wrote (see src/test/wire/README.md), each once: the
 * edge-case step that holds a server-side search was recorded twice, by the 0.7.0 harness and
 * again by the one that records server-side outcomes, and is one interaction.
 */
const REAL_WIRE_IDS = [
  ...new Set(
    Object.values(import.meta.glob<string>("../test/wire/*/*.json", { eager: true, query: "?raw", import: "default" })).map(
      (raw) => (JSON.parse(raw) as { interaction: { id: string } }).interaction.id,
    ),
  ),
];

/** What Git reports for one tombstone: its shard directory, then its blob. */
function tombstoneEntries(id: string, shard = id.slice(0, 2)) {
  return [
    { path: `tombstones/${shard}`, type: "tree" },
    { path: `tombstones/${shard}/${id}.json`, type: "blob", url: `https://api.github.com/blob/tombstone/${id}` },
  ];
}

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
  return { queryClient, ...renderHook(() => usePRInteractions(owner, repo, prNumber), { wrapper }) };
}

function validTombstone(id: string) {
  return {
    format: "cvc.tombstone/v1",
    version: 1,
    interaction_id: id,
    deleted_at: "2026-01-01T00:00:00Z",
    reason_code: "security",
  };
}

/** A tombstone blob as Git lists it inside its shard directory. */
function blobEntry(id: string) {
  return { path: `${id}.json`, type: "blob", url: `https://api.github.com/blob/tombstone/${id}` };
}

/**
 * Mocks a destination whose cvc/main has a by-commit index listing `projected`, and a tombstones tree
 * made of the given shard directories (shard name -> the entries Git lists inside it). The payload of
 * a tombstone blob is the valid tombstone for the id in its URL. Returns the mocked `fetch`.
 */
function setupTombstoneDestination(
  projected: string[],
  shards: Record<string, Array<{ path: string; type: string; url?: string }>>,
) {
  mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
  mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }] });
  mockGetTree.mockImplementation(({ tree_sha }: { tree_sha: string }) => {
    if (tree_sha === CVC_MAIN_SHA) {
      return Promise.resolve({ data: { tree: [{ path: "by-commit", type: "tree" }, { path: "tombstones", type: "tree", sha: "tombstone-root" }] } });
    }
    if (tree_sha === "tombstone-root") {
      return Promise.resolve({ data: { tree: Object.keys(shards).map((shard) => ({ path: shard, type: "tree", sha: `tombstone-shard-${shard}` })) } });
    }
    return Promise.resolve({ data: { tree: shards[tree_sha.replace("tombstone-shard-", "")] ?? [] } });
  });
  mockGetContent.mockResolvedValue({ data: projected.map((name) => ({ name })) });
  const fetchMock = vi.fn().mockImplementation((url: string) => {
    const tombstone = /blob\/tombstone\/([^/]+)$/.exec(url);
    if (tombstone) {
      return Promise.resolve({ ok: true, status: 200, text: () => Promise.resolve(JSON.stringify(validTombstone(tombstone[1]))) });
    }
    const node = /nodes\/[0-9a-f]{2}\/([0-9a-f-]{36})\.json/.exec(url);
    return Promise.resolve({ ok: true, text: () => Promise.resolve(JSON.stringify(rawInteractionBlob(node ? node[1] : ID_ONE))) });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
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

describe("validateTombstoneTree: which interaction ids a tombstone path may carry", () => {
  type Entries = Parameters<typeof validateTombstoneTree>[0];
  const ids = (entries: Entries) => validateTombstoneTree(entries).map((blob) => blob.id);
  // The refusals below run for a version 4 id as well as a version 8 one, so each is a test of its own rule
  // (and not of the version) whichever way the version rule is written.
  const BASES = [
    ["version 4", ID_V4],
    ["version 8", ID_V8],
  ] as const;

  it("accepts a version 8 id, which is what the Claude Code harness writes", () => {
    const blobs = validateTombstoneTree(tombstoneEntries(ID_V8));
    expect(blobs.map((blob) => blob.id)).toEqual([ID_V8]);
    expect(blobs[0].url).toBe(`https://api.github.com/blob/tombstone/${ID_V8}`);
  });

  it("accepts the id of every real node the shipped harness wrote", () => {
    expect(REAL_WIRE_IDS.length).toBeGreaterThanOrEqual(7);
    for (const id of REAL_WIRE_IDS) expect(ids(tombstoneEntries(id)), id).toEqual([id]);
  });

  it.each(["1", "2", "3", "4", "5", "6", "7", "8"])("accepts RFC 9562 version %s", (version) => {
    const id = idWith(version, "a");
    expect(ids(tombstoneEntries(id))).toEqual([id]);
  });

  it.each(["8", "9", "a", "b"])("accepts the RFC variant digit %s", (variant) => {
    const id = idWith("8", variant);
    expect(ids(tombstoneEntries(id))).toEqual([id]);
  });

  it("accepts version 4 and version 8 ids side by side", () => {
    expect(ids([...tombstoneEntries(ID_V4), ...tombstoneEntries(ID_V8)]).sort()).toEqual([ID_V4, ID_V8].sort());
  });

  it.each(["0", "9", "a", "b", "c", "d", "e", "f"])("refuses version digit %s, which RFC 9562 does not define", (version) => {
    expect(() => validateTombstoneTree(tombstoneEntries(idWith(version, "a")))).toThrow("Invalid CVC tombstone path");
    expect(() => validateTombstoneTree(tombstoneEntries(idWith(version, "b")))).toThrow("Invalid CVC tombstone path");
  });

  it.each(["0", "1", "7", "c", "d", "e", "f"])("refuses variant digit %s, which is not the RFC variant", (variant) => {
    expect(() => validateTombstoneTree(tombstoneEntries(idWith("8", variant)))).toThrow("Invalid CVC tombstone path");
    expect(() => validateTombstoneTree(tombstoneEntries(idWith("4", variant)))).toThrow("Invalid CVC tombstone path");
  });

  it("refuses the all-zero and all-one ids", () => {
    for (const id of ["00000000-0000-0000-0000-000000000000", "ffffffff-ffff-ffff-ffff-ffffffffffff"]) {
      expect(() => validateTombstoneTree(tombstoneEntries(id)), id).toThrow("Invalid CVC tombstone path");
    }
  });

  describe("keeps the canonical form of the path", () => {
    it.each(BASES)("refuses an uppercase %s id, with its shard in either case", (_label, id) => {
      const upper = id.toUpperCase();
      // Uppercasing may change the shard too (3e -> 3E), or not (12 has no letters): refused as a shard or as a path.
      expect(() => validateTombstoneTree(tombstoneEntries(upper))).toThrow(/Invalid CVC tombstone (shard|path)/);
      // With the shard left in lowercase, only the id is wrong.
      expect(() => validateTombstoneTree(tombstoneEntries(upper, id.slice(0, 2)))).toThrow("Invalid CVC tombstone path");
    });

    it.each(BASES)("refuses a %s id with one hex letter in uppercase, wherever it is", (_label, id) => {
      let tried = 0;
      for (let at = 0; at < id.length; at++) {
        const upper = id.slice(0, at) + id[at].toUpperCase() + id.slice(at + 1);
        if (upper === id) continue; // a digit or a hyphen has no uppercase form
        tried++;
        // The shard stays as it should be (lowercase, the id's first two digits): only the id is wrong.
        expect(() => validateTombstoneTree(tombstoneEntries(upper, id.slice(0, 2))), upper).toThrow("Invalid CVC tombstone path");
      }
      expect(tried).toBeGreaterThan(3);
    });

    it.each(BASES)("refuses a %s id whose file has an uppercase extension, or none, or more than one", (_label, id) => {
      const shard = id.slice(0, 2);
      for (const name of [`${id}.JSON`, `${id}.Json`, id, `${id}.json.bak`, `${id}.jsonx`, `.json`]) {
        expect(
          () => validateTombstoneTree([{ path: `tombstones/${shard}`, type: "tree" }, { path: `tombstones/${shard}/${name}`, type: "blob", url: "u" }]),
          name,
        ).toThrow("Invalid CVC tombstone path");
      }
    });

    it.each(BASES)("refuses a %s id in the wrong shard", (_label, id) => {
      const right = id.slice(0, 2);
      const wrong = [right[0] + (right[1] === "0" ? "1" : "0"), (right[0] === "0" ? "1" : "0") + right[1], "ab", "00", "ff"].filter((shard) => shard !== right);
      for (const shard of wrong) {
        expect(() => validateTombstoneTree(tombstoneEntries(id, shard)), shard).toThrow("Invalid CVC tombstone path");
      }
    });

    it("refuses a shard that is not two lowercase hex digits", () => {
      for (const shard of ["3", "3ec", "3E", "3g", ""]) {
        expect(() => validateTombstoneTree(tombstoneEntries(ID_V8, shard)), JSON.stringify(shard)).toThrow("Invalid CVC tombstone shard");
        expect(() => validateTombstoneTree(tombstoneEntries(ID_V4, shard)), JSON.stringify(shard)).toThrow("Invalid CVC tombstone shard");
      }
    });

    it.each(BASES)("refuses a %s id that is not in the hyphenated form", (_label, id) => {
      // The last two put a whole, valid id after (or before) extra digits; the second also starts with the shard,
      // so only the pattern itself, not the shard check, can refuse it.
      const forms = [id.replaceAll("-", ""), `{${id}}`, `urn:uuid:${id}`, ` ${id}`, `${id}\n`, id.slice(0, -1), `${id}0`, `${id.slice(0, 2)}${id}`];
      for (const form of forms) {
        expect(() => validateTombstoneTree(tombstoneEntries(form, id.slice(0, 2))), JSON.stringify(form)).toThrow("Invalid CVC tombstone path");
      }
    });
  });

  describe("still refuses the whole namespace for one bad entry", () => {
    // A second id in the same shard as ID_V8, so an entry for it can be well formed in every respect but one.
    const SIBLING_V8 = "3ec4e81b-3ffc-8d60-b160-5c0b3a2407dc";
    const SIBLING_V4 = "123e4567-e89b-42d3-a456-426614174001";

    it.each(BASES)("%s: a file that is not an id, a nested path, a directory, a submodule, a blob where a shard belongs", (_label, id) => {
      const shard = id.slice(0, 2);
      const sibling = id === ID_V8 ? SIBLING_V8 : SIBLING_V4;
      const bad: Entries[number][] = [
        { path: `tombstones/${shard}/README.md`, type: "blob", url: "u" },
        { path: `tombstones/${shard}/${id}.json/extra`, type: "blob", url: "u" },
        { path: `tombstones/${shard}/nested`, type: "tree" },
        { path: `tombstones/${shard}/${sibling}.json`, type: "commit", url: "u" },
        { path: `tombstones/${sibling}.json`, type: "blob", url: "u" },
        { path: `tombstones/${shard}/${sibling}.json`, type: "blob" }, // no download URL
      ];
      for (const extra of bad) {
        expect(() => validateTombstoneTree([...tombstoneEntries(id), extra]), JSON.stringify(extra)).toThrow(/Invalid CVC tombstone/);
      }
    });

    it.each(BASES)("%s: a blob whose shard directory is missing", (_label, id) => {
      expect(() => validateTombstoneTree([tombstoneEntries(id)[1]])).toThrow("CVC tombstone blob has no shard tree");
    });

    it("a malformed id next to valid ones: no valid sibling is accepted in its place", () => {
      for (const bad of [
        "abcdef01-e89b-92d3-a456-426614174000", // version 9
        "abcdef01-e89b-02d3-a456-426614174000", // version 0
        "abcdef01-e89b-82d3-c456-426614174000", // variant c
        "ABCDEF01-E89B-82D3-A456-426614174000", // uppercase
      ]) {
        expect(
          () => validateTombstoneTree([...tombstoneEntries(ID_V8), ...tombstoneEntries(ID_V4), ...tombstoneEntries(bad, "ab")]),
          bad,
        ).toThrow(/Invalid CVC tombstone/);
      }
    });

    it.each(BASES)("%s: the same path twice", (_label, id) => {
      expect(() => validateTombstoneTree([...tombstoneEntries(id), tombstoneEntries(id)[1]])).toThrow("Duplicate CVC tombstone tree entry");
    });

    it("ignores entries outside the reserved namespace, as before", () => {
      const outside = [{ path: `nodes/3e/${ID_V8}.json`, type: "blob", url: "u" }, { path: "by-commit", type: "tree" }];
      expect(ids([...outside, ...tombstoneEntries(ID_V8)])).toEqual([ID_V8]);
      expect(ids([...outside, ...tombstoneEntries(ID_V4)])).toEqual([ID_V4]);
    });
  });
});

describe("usePRInteractions", () => {
  it("rejects duplicate tombstone paths in linear time before fetching payloads", () => {
    const path = `tombstones/${ID_V4.slice(0, 2)}/${ID_V4}.json`;
    expect(() => validateTombstoneTree([
      { path: `tombstones/${ID_V4.slice(0, 2)}`, type: "tree" },
      { path, type: "blob", url: "https://api.github.com/blob/one" },
      { path, type: "blob", url: "https://api.github.com/blob/two" },
    ])).toThrow("Duplicate CVC tombstone tree entry");
  });

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

  it("returns the pull request's commits, oldest first, with their subjects, for the timeline's commit filter", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({
      data: [{ sha: "commit-a", commit: { message: "feat: first\n\nbody" } }, { sha: "commit-b", commit: { message: "fix: second" } }],
    });
    mockGetTree.mockResolvedValue({ data: { tree: [{ path: "by-commit", type: "tree" }, { path: "nodes", type: "tree" }] } });
    mockGetContent.mockImplementation(({ path }: { path: string }) =>
      path === "by-commit/commit-a" ? Promise.resolve({ data: [{ name: ID_ONE }] }) : Promise.reject(notFoundError()),
    );
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, text: () => Promise.resolve(JSON.stringify(rawInteractionBlob(ID_ONE, ["commit-a"]))) }));

    const { result } = renderWithClient("owner", "repo", 1);

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.commits).toEqual([
      { sha: "commit-a", subject: "feat: first" },
      { sha: "commit-b", subject: "fix: second" },
    ]);
    // Looked up once, with the listing the interactions were found from: no request for the subjects.
    expect(mockListCommits).toHaveBeenCalledTimes(1);
  });

  it("returns the commits from the capped fallback too, and none when there is no history to look up", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a", commit: { message: "legacy commit" } }] });
    mockGetTree.mockResolvedValue({ data: { tree: [] } });
    const first = renderWithClient("owner", "repo", 1);
    await waitFor(() => expect(first.result.current.isSuccess).toBe(true));
    expect(first.result.current.data?.commits).toEqual([{ sha: "commit-a", subject: "legacy commit" }]);
    first.unmount();

    mockGetRef.mockRejectedValue(notFoundError());
    const second = renderWithClient("owner", "repo", 2);
    await waitFor(() => expect(second.result.current.isSuccess).toBe(true));
    expect(second.result.current.data?.commits).toBeUndefined();
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
      if (url.includes("/FORMAT?")) {
        return Promise.resolve({ ok: true, text: () => Promise.resolve("3") });
      }
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
    expect(fetchMock).toHaveBeenCalledTimes(3);
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

  it("enumerates tombstones outside by-commit and evicts their restored node cache", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }] });
    mockGetTree.mockImplementation(({ tree_sha }: { tree_sha: string }) => {
      if (tree_sha === CVC_MAIN_SHA) {
        return Promise.resolve({ data: { tree: [{ path: "by-commit", type: "tree" }, { path: "tombstones", type: "tree", sha: "tombstone-root" }] } });
      }
      if (tree_sha === "tombstone-root") {
        return Promise.resolve({ data: { tree: [{ path: ID_V4.slice(0, 2), type: "tree", sha: "tombstone-shard" }] } });
      }
      return Promise.resolve({ data: { tree: [{ path: `${ID_V4}.json`, type: "blob", url: "https://api.github.com/blob/tombstone" }] } });
    });
    // ID_V4 was removed from the projection, so it does not appear here.
    mockGetContent.mockResolvedValue({ data: [{ name: ID_ONE }] });
    const fetchMock = vi.fn().mockImplementation((url: string) => {
      if (url.includes("blob/tombstone")) {
        return Promise.resolve({
          ok: true,
          status: 200,
          text: () => Promise.resolve(JSON.stringify(validTombstone(ID_V4))),
        });
      }
      return Promise.resolve({ ok: true, text: () => Promise.resolve(JSON.stringify(rawInteractionBlob(ID_ONE))) });
    });
    vi.stubGlobal("fetch", fetchMock);

    const { result, queryClient } = renderWithClient("owner", "repo", 8);
    // This is the exact query persisted by App's IndexedDB persister. removeQueries
    // emits a cache update, so the persister rewrites without this record as well.
    queryClient.setQueryData(["cvc-node", "owner", "repo", ID_V4], rawInteractionBlob(ID_V4));

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.interactions.map((node) => node.id)).toEqual([ID_ONE]);
    expect(queryClient.getQueryData(["cvc-node", "owner", "repo", ID_V4])).toBeUndefined();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it.each([
    ["an uppercase shard", { path: "AB", type: "tree" }, undefined],
    ["a non-canonical file", { path: "unexpected.json", type: "blob" }, undefined],
    ["a non-tree shard", { path: ID_V4.slice(0, 2), type: "blob" }, undefined],
    ["a subtree below a shard", { path: ID_V4.slice(0, 2), type: "tree", sha: "tombstone-shard" }, { path: "nested", type: "tree" }],
  ])("fails closed for %s in the reserved tombstone tree", async (_label, malformedEntry, shardEntry) => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }] });
    mockGetTree.mockImplementation(({ tree_sha }: { tree_sha: string }) => {
      if (tree_sha === CVC_MAIN_SHA) {
        return Promise.resolve({ data: { tree: [{ path: "by-commit", type: "tree" }, { path: "tombstones", type: "tree", sha: "tombstone-root" }] } });
      }
      if (tree_sha === "tombstone-root") return Promise.resolve({ data: { tree: [malformedEntry] } });
      return Promise.resolve({ data: { tree: shardEntry ? [shardEntry] : [] } });
    });

    const { result } = renderWithClient("owner", "repo", 9);
    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(mockGetContent).not.toHaveBeenCalled();
    expect(globalThis.fetch).not.toHaveBeenCalled();
  });

  it.each([
    ["root", CVC_MAIN_SHA],
    ["tombstones subtree", "tombstone-root"],
  ])("fails closed when the %s tree response is truncated", async (_label, truncatedSha) => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }] });
    mockGetTree.mockImplementation(({ tree_sha }: { tree_sha: string }) => {
      if (tree_sha === CVC_MAIN_SHA) {
        return Promise.resolve({
          data: {
            truncated: truncatedSha === CVC_MAIN_SHA,
            tree: [{ path: "by-commit", type: "tree" }, { path: "tombstones", type: "tree", sha: "tombstone-root" }],
          },
        });
      }
      return Promise.resolve({ data: { truncated: true, tree: [] } });
    });

    const { result, queryClient } = renderWithClient("owner", "repo", 10);
    queryClient.setQueryData(["cvc-node", "owner", "repo", ID_V4], rawInteractionBlob(ID_V4));
    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(mockGetContent).not.toHaveBeenCalled();
    expect(globalThis.fetch).not.toHaveBeenCalled();
    expect(result.current.data?.interactions).toEqual([]);
    expect(queryClient.getQueryData(["cvc-node", "owner", "repo", ID_V4])).toBeUndefined();
  });

  describe("tombstones of version 8 interaction ids (the Claude Code harness's)", () => {
    const visibleIds = (result: { current: { data?: { interactions: { id: string }[] } } }) =>
      (result.current.data?.interactions ?? []).map((node) => node.id).sort();

    it("renders the overlay with a tombstoned version 8 interaction suppressed", async () => {
      // The by-commit index still lists ID_V8 (a client may see an older projection); it is tombstoned.
      const fetchMock = setupTombstoneDestination([ID_ONE, ID_V8], { [ID_V8.slice(0, 2)]: [blobEntry(ID_V8)] });

      const { result, queryClient } = renderWithClient("owner", "repo", 20);
      // A copy cached before the redaction (App persists these in IndexedDB).
      queryClient.setQueryData(["cvc-node", "owner", "repo", ID_V8], rawInteractionBlob(ID_V8));

      await waitFor(() => expect(result.current.isSuccess).toBe(true));
      expect(result.current.data?.interactions.map((node) => node.id)).toEqual([ID_ONE]);
      expect(queryClient.getQueryData(["cvc-node", "owner", "repo", ID_V8])).toBeUndefined();
      // The tombstone payload and the one visible node; the suppressed node's blob is never read.
      expect(fetchMock).toHaveBeenCalledTimes(2);
      expect(fetchMock.mock.calls.some(([url]) => String(url).includes(ID_V8) && String(url).includes("/nodes/"))).toBe(false);
    });

    it("suppresses version 4 and version 8 interactions together, and shows the rest", async () => {
      const fetchMock = setupTombstoneDestination([ID_ONE, ID_V4, ID_V8, ID_TWO], {
        [ID_V4.slice(0, 2)]: [blobEntry(ID_V4)],
        [ID_V8.slice(0, 2)]: [blobEntry(ID_V8)],
      });

      const { result } = renderWithClient("owner", "repo", 21);

      await waitFor(() => expect(result.current.isSuccess).toBe(true));
      expect(visibleIds(result)).toEqual([ID_ONE, ID_TWO]);
      expect(fetchMock).toHaveBeenCalledTimes(4); // two tombstone payloads, two visible nodes
    });

    it("suppresses by id: a version 8 interaction with no tombstone of its own stays", async () => {
      setupTombstoneDestination([ID_V8, ID_ONE], { [ID_V4.slice(0, 2)]: [blobEntry(ID_V4)] });

      const { result } = renderWithClient("owner", "repo", 22);

      await waitFor(() => expect(result.current.isSuccess).toBe(true));
      expect(visibleIds(result)).toEqual([ID_ONE, ID_V8].sort());
    });

    it("renders every interaction the harness wrote when all of them are tombstoned except one", async () => {
      expect(REAL_WIRE_IDS.length).toBeGreaterThanOrEqual(7);
      const [kept, ...redacted] = REAL_WIRE_IDS;
      const shards: Record<string, ReturnType<typeof blobEntry>[]> = {};
      for (const id of redacted) (shards[id.slice(0, 2)] ??= []).push(blobEntry(id));
      setupTombstoneDestination(REAL_WIRE_IDS, shards);

      const { result } = renderWithClient("owner", "repo", 23);

      await waitFor(() => expect(result.current.isSuccess).toBe(true));
      expect(visibleIds(result)).toEqual([kept]);
    });

    // One malformed entry refuses the whole overlay: a valid sibling must not be shown in its place.
    it.each([
      ["an uppercase id", `${ID_V8.toUpperCase()}.json`],
      ["a version digit of 0", ID_V8.replace("-8d60-", "-0d60-") + ".json"],
      ["a version digit of 9", ID_V8.replace("-8d60-", "-9d60-") + ".json"],
      ["a version digit of f", ID_V8.replace("-8d60-", "-fd60-") + ".json"],
      ["a variant digit of c", ID_V8.replace("-b160-", "-c160-") + ".json"],
      ["a variant digit of 7", ID_V8.replace("-b160-", "-7160-") + ".json"],
      ["an id that belongs in another shard", `ab${ID_V8.slice(2)}.json`],
      ["a file that is not an id", "README.md"],
      ["a file with an uppercase extension", `${ID_V8}.JSON`],
    ])("refuses the whole overlay for %s next to a valid version 8 tombstone", async (_label, path) => {
      setupTombstoneDestination([ID_ONE, ID_V8], {
        [ID_V8.slice(0, 2)]: [blobEntry(ID_V8), { path, type: "blob", url: "https://api.github.com/blob/tombstone/other" }],
      });

      const { result } = renderWithClient("owner", "repo", 24);

      await waitFor(() => expect(result.current.isError).toBe(true));
      expect(result.current.data?.interactions).toEqual([]); // nothing renders, not even the valid sibling
      expect(mockGetContent).not.toHaveBeenCalled();
      expect(globalThis.fetch).not.toHaveBeenCalled(); // no payload and no node was read
    });

    it("refuses the whole overlay for a directory inside a shard that holds a valid version 8 tombstone", async () => {
      setupTombstoneDestination([ID_ONE, ID_V8], {
        [ID_V8.slice(0, 2)]: [blobEntry(ID_V8), { path: "nested", type: "tree" }],
      });

      const { result } = renderWithClient("owner", "repo", 25);

      await waitFor(() => expect(result.current.isError).toBe(true));
      expect(result.current.data?.interactions).toEqual([]);
      expect(globalThis.fetch).not.toHaveBeenCalled();
    });
  });

  it("fails closed when tombstone enumeration exceeds its count limit", async () => {
    mockGetRef.mockResolvedValue({ data: { object: { sha: CVC_MAIN_SHA } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }] });
    mockGetTree.mockImplementation(({ tree_sha }: { tree_sha: string }) => {
      if (tree_sha === CVC_MAIN_SHA) {
        return Promise.resolve({ data: { tree: [{ path: "by-commit", type: "tree" }, { path: "tombstones", type: "tree", sha: "tombstone-root" }] } });
      }
      return Promise.resolve({ data: { tree: Array.from({ length: 10_001 }, (_, index) => ({ path: `${index.toString(16).padStart(2, "0").slice(-2)}`, type: "tree", sha: `shard-${index}` })) } });
    });
    const { result, queryClient } = renderWithClient("owner", "repo", 12);
    queryClient.setQueryData(["cvc-node", "owner", "repo", ID_V4], rawInteractionBlob(ID_V4));

    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(queryClient.getQueryData(["cvc-node", "owner", "repo", ID_V4])).toBeUndefined();
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
          // No by-commit/ entry at all -- this is a legacy flat-layout (v1) repo.
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

  it("clears a previously rendered timeline and cognitive trust cache when an advanced ref has malformed FORMAT", async () => {
    mockGetRef
      .mockResolvedValueOnce({ data: { object: { sha: "cvc-before" } } })
      .mockResolvedValueOnce({ data: { object: { sha: "cvc-malformed" } } });
    mockListCommits.mockResolvedValue({ data: [{ sha: "commit-a" }] });
    mockGetTree.mockResolvedValue({
      data: { tree: [{ path: "by-commit", type: "tree" }, { path: "FORMAT", type: "blob" }] },
    });
    mockGetContent.mockResolvedValue({ data: [{ name: ID_ONE }] });
    vi.stubGlobal("fetch", vi.fn().mockImplementation((url: string) => {
      if (url.includes("cvc-before") && url.includes("/FORMAT?")) {
        return Promise.resolve({ ok: true, text: () => Promise.resolve("4") });
      }
      if (url.includes("cvc-malformed") && url.includes("/FORMAT?")) {
        return Promise.resolve({ ok: true, text: () => Promise.resolve("not-a-format") });
      }
      return Promise.resolve({ ok: true, text: () => Promise.resolve(JSON.stringify(rawInteractionBlob(ID_ONE, ["commit-a"]))) });
    }));

    const { result, queryClient } = renderWithClient("owner", "repo", 11);
    await waitFor(() => expect(result.current.data?.interactions.map((node) => node.id)).toEqual([ID_ONE]));
    expect(queryClient.getQueryData(["cvc-node", "owner", "repo", "cvc-before", ID_ONE])).toBeDefined();

    await result.current.refetch();
    await waitFor(() => expect(result.current.isError).toBe(true));

    // React Query normally retains previous successful data on a failed refetch.
    // FORMAT/tombstone trust failures are exceptional: no stale interaction may render.
    expect(result.current.data?.interactions).toEqual([]);
    expect(queryClient.getQueryData(["cvc-node", "owner", "repo", "cvc-before", ID_ONE])).toBeUndefined();
    expect(queryClient.getQueryData(["cvc-by-commit", "owner", "repo", "cvc-before", "commit-a"])).toBeUndefined();
  });
});

describe("fetchCanonicalTombstones empty-tree guard", () => {
  it("returns no suppressions for the canonical empty tree without fetching it", async () => {
    const getTree = vi.fn(async () => {
      throw new Error("must not fetch the canonical empty tree");
    });
    const client = { octokit: { rest: { git: { getTree } } } } as never;
    const tombstoned = await fetchCanonicalTombstones(client, "owner", "repo", EMPTY_TREE_SHA, "token");
    expect(tombstoned.size).toBe(0);
    expect(getTree).not.toHaveBeenCalled();
  });
});
