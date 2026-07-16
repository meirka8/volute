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
});
