import { describe, it, expect } from "vitest";
import { InteractionMapper } from "./InteractionMapper";
import type { InteractionNode } from "../types/cvc";

function node(id: string, commitShas: string[]): InteractionNode {
  return {
    id,
    conversation_id: "conv-1",
    parent_id: null,
    timestamp: 0,
    timestampRaw: "2026-01-01T00:00:00Z",
    author: "human",
    user_prompt: `prompt ${id}`,
    model_name: null,
    model_cot: null,
    model_response: null,
    context_items: [],
    tool_executions: [],
    artifact_links: commitShas.map((sha) => ({
      interaction_id: id,
      git_commit_hash: sha,
      link_type: "generated",
    })),
  };
}

describe("InteractionMapper", () => {
  it("indexes interactions by the commits they're linked to", () => {
    const a = node("a", ["commit-1"]);
    const b = node("b", ["commit-2"]);
    const mapper = new InteractionMapper([a, b]);

    expect(mapper.getInteractionsForCommit("commit-1")).toEqual([a]);
    expect(mapper.getInteractionsForCommit("commit-2")).toEqual([b]);
    expect(mapper.getInteractionsForCommit("commit-missing")).toEqual([]);
  });

  it("indexes an interaction under every commit it's linked to", () => {
    const shared = node("shared", ["commit-1", "commit-2"]);
    const mapper = new InteractionMapper([shared]);

    expect(mapper.getInteractionsForCommit("commit-1")).toEqual([shared]);
    expect(mapper.getInteractionsForCommit("commit-2")).toEqual([shared]);
  });

  it("ignores interactions with no artifact links (floating thoughts)", () => {
    const floating = node("floating", []);
    const mapper = new InteractionMapper([floating]);

    expect(mapper.getInteractionsForRange(["commit-1"])).toEqual([]);
  });

  it("dedupes an interaction linked to multiple commits within a range", () => {
    const shared = node("shared", ["commit-1", "commit-2"]);
    const mapper = new InteractionMapper([shared]);

    const result = mapper.getInteractionsForRange(["commit-1", "commit-2"]);
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("shared");
  });

  it("returns the union across a range of commits", () => {
    const a = node("a", ["commit-1"]);
    const b = node("b", ["commit-2"]);
    const c = node("c", ["commit-3"]);
    const mapper = new InteractionMapper([a, b, c]);

    const result = mapper.getInteractionsForRange(["commit-1", "commit-2"]);
    expect(result.map((n) => n.id).sort()).toEqual(["a", "b"]);
  });
});
