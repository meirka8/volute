import { describe, expect, it, vi } from "vitest";
import { canonicalEventId, canonicalRangeId, fetchFormat5Evidence, readBoundedJson, validEvent, validRange } from "./format5";

const commit = "a".repeat(40);
const rangeBody = { format: "cvc.range-evidence/v1" as const, version: 1 as const, repository_identity: "b".repeat(64), object_format: "sha1" as const, base_oid: commit, tip_oid: "c".repeat(40), base_tree_oid: "d".repeat(40), result_tree_oid: "e".repeat(40), commits: [{ commit_oid: commit }], changeset_algorithm: "cvc.changeset/v1" as const, changeset_digest: "f".repeat(64) };

describe("FORMAT5 wire validation", () => {
  it("matches the shared Rust golden vectors", async () => {
    // Values are the fixed cvc-core/test-data/format5-golden.json vectors.
    const event = {
      interaction_id: "11111111-1111-4111-8111-111111111111",
      target_commit: "b".repeat(40), relation: "rewrite_exact" as const,
      evidence: { version: 1 as const, kind: "locally_observed" as const },
      origin: "local_hook" as const,
      source_event_ids: [`legacy:11111111-1111-4111-8111-111111111111:${"a".repeat(40)}`],
      old_oid: "a".repeat(40), new_oid: "b".repeat(40), range_id: null, linked_by: null,
    };
    const range = {
      format: "cvc.range-evidence/v1" as const, version: 1 as const,
      repository_identity: "c".repeat(64), object_format: "sha1" as const,
      base_oid: "a".repeat(40), tip_oid: "b".repeat(40),
      base_tree_oid: "d".repeat(40), result_tree_oid: "e".repeat(40),
      commits: [{ commit_oid: "b".repeat(40) }],
      changeset_algorithm: "cvc.changeset/v1" as const, changeset_digest: "f".repeat(64),
    };
    const event_id = "a5b182bd90e298bcb84948ce7d7d6caa1f9b4f2e993131267459776479556576";
    const range_id = "13e56effb5b051a74275009163cb3277f1188573b588474f5abb0d0feae2fd93";
    expect(await canonicalEventId(event)).toBe(event_id);
    expect(await canonicalRangeId(range)).toBe(range_id);
    expect(await validEvent({ ...event, event_id }, event_id)).toBe(true);
    expect(await validRange({ ...range, range_id }, range_id)).toBe(true);
  });

  it("accepts Rust-compatible length-prefixed range and rewrite event IDs", async () => {
    const range_id = await canonicalRangeId(rangeBody);
    expect(await validRange({ ...rangeBody, range_id }, range_id)).toBe(true);
    const eventBody = { interaction_id: "123e4567-e89b-42d3-a456-426614174000", target_commit: commit, relation: "rewrite_exact" as const, evidence: { version: 1 as const, kind: "locally_observed" as const }, origin: "local_hook" as const, source_event_ids: [`legacy:123e4567-e89b-42d3-a456-426614174000:${"1".repeat(40)}`], old_oid: "1".repeat(40), new_oid: commit, range_id: null, linked_by: null };
    const event_id = await canonicalEventId(eventBody);
    expect(await validEvent({ ...eventBody, event_id }, event_id)).toBe(true);
  });

  it("fails closed for path, fields, canonical hash, and range bounds", async () => {
    const range_id = await canonicalRangeId(rangeBody);
    expect(await validRange({ ...rangeBody, range_id, extra: true }, range_id)).toBe(false);
    expect(await validRange({ ...rangeBody, range_id }, "0".repeat(64))).toBe(false);
    const invalidEvent = { event_id: "0".repeat(64), interaction_id: "123e4567-e89b-42d3-a456-426614174000", target_commit: commit, relation: "generated", evidence: { version: 1, kind: "locally_observed" }, origin: "local_hook", source_event_ids: [], old_oid: null, new_oid: null, range_id: null, linked_by: null };
    expect(await validEvent(invalidEvent, invalidEvent.event_id)).toBe(false);
  });

  it("bounds streamed nodes and rejects invalid UTF-8 before JSON parsing", async () => {
    vi.stubGlobal("fetch", vi.fn()
      .mockResolvedValueOnce(new Response(new Uint8Array(4 * 1024 * 1024 + 1)))
      .mockResolvedValueOnce(new Response(new Uint8Array([0xff]))));
    await expect(readBoundedJson("https://api.github.com/repos/o/r/contents/node", "token", { bytes: 0 })).rejects.toThrow("safe size");
    await expect(readBoundedJson("https://api.github.com/repos/o/r/contents/node", "token", { bytes: 0 })).rejects.toThrow("UTF-8");
  });

  it("rejects an invalid tree blob SHA before making an API request", async () => {
    const client = { octokit: { rest: { git: { getTree: async () => ({ data: { tree: [{ path: "aa", type: "tree", sha: "b".repeat(40) }] } }) } } } } as never;
    vi.stubGlobal("fetch", vi.fn());
    // The second getTree response has an invalid blob SHA.
    let calls = 0;
    (client as { octokit: { rest: { git: { getTree: () => unknown } } } }).octokit.rest.git.getTree = async () => ({ data: { tree: ++calls === 1 ? [{ path: "aa", type: "tree", sha: "b".repeat(40) }] : [{ path: `${"a".repeat(64)}.json`, type: "blob", sha: "BAD" }] } });
    await expect(fetchFormat5Evidence(client, "owner", "repo", "a".repeat(40), "d".repeat(40), "token")).rejects.toThrow("Invalid or excessive");
    expect(fetch).not.toHaveBeenCalled();
  });
});
