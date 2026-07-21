import { describe, it, expect, vi } from "vitest";
import { CommitRanger } from "./CommitRanger";
import type { GithubClient } from "../api/github";

function fakeClient(commits: { sha: string }[]) {
  const listCommits = vi.fn().mockResolvedValue({ data: commits });
  const client = {
    octokit: { rest: { pulls: { listCommits } } },
  } as unknown as GithubClient;
  return { client, listCommits };
}

describe("CommitRanger", () => {
  it("extracts just the SHAs from the PR's commit list", async () => {
    const { client } = fakeClient([{ sha: "aaa" }, { sha: "bbb" }]);
    const ranger = new CommitRanger(client);

    const shas = await ranger.getCommitShas("owner", "repo", 42);
    expect(shas).toEqual(["aaa", "bbb"]);
  });

  it("requests the PR's commits with a page size covering typical PRs", async () => {
    const { client, listCommits } = fakeClient([]);
    const ranger = new CommitRanger(client);

    await ranger.getCommitShas("owner", "repo", 42);
    expect(listCommits).toHaveBeenCalledWith({
      owner: "owner",
      repo: "repo",
      pull_number: 42,
      per_page: 100,
    });
  });

  it("returns an empty list for a PR with no commits", async () => {
    const { client } = fakeClient([]);
    const ranger = new CommitRanger(client);

    expect(await ranger.getCommitShas("owner", "repo", 1)).toEqual([]);
  });

  it("paginates commit lists and adds the merged commit SHA once", async () => {
    const firstPage = Array.from({ length: 100 }, (_, index) => ({ sha: `commit-${index}` }));
    const listCommits = vi
      .fn()
      .mockResolvedValueOnce({ data: firstPage })
      .mockResolvedValueOnce({ data: [{ sha: "commit-99" }, { sha: "commit-100" }] });
    const get = vi.fn().mockResolvedValue({
      data: { merged_at: "2026-07-21T00:00:00Z", merge_commit_sha: "a".repeat(40) },
    });
    const client = {
      octokit: { rest: { pulls: { listCommits, get } } },
    } as unknown as GithubClient;

    const shas = await new CommitRanger(client).getCommitShas("owner", "repo", 42);

    expect(listCommits).toHaveBeenNthCalledWith(1, {
      owner: "owner", repo: "repo", pull_number: 42, per_page: 100,
    });
    expect(listCommits).toHaveBeenNthCalledWith(2, {
      owner: "owner", repo: "repo", pull_number: 42, per_page: 100, page: 2,
    });
    expect(get).toHaveBeenCalledWith({ owner: "owner", repo: "repo", pull_number: 42 });
    expect(shas).toHaveLength(102);
    expect(shas).toContain("commit-100");
    expect(shas).toContain("a".repeat(40));
  });
});
