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

  describe("getCommits (the timeline's commit filter)", () => {
    it("keeps the first line of each commit message, from the same list, without another request", async () => {
      const { client, listCommits } = fakeClient([
        { sha: "aaa", commit: { message: "feat: add the thing\n\nA longer body\nover lines" } } as { sha: string },
        { sha: "bbb", commit: { message: "  fix: second  " } } as { sha: string },
        { sha: "ccc", commit: { message: "" } } as { sha: string },
        { sha: "ddd" },
        { sha: "eee", commit: { message: 42 } } as unknown as { sha: string },
      ]);
      const commits = await new CommitRanger(client).getCommits("owner", "repo", 42);
      expect(commits).toEqual([
        { sha: "aaa", subject: "feat: add the thing" },
        { sha: "bbb", subject: "fix: second" },
        { sha: "ccc" },
        { sha: "ddd" },
        { sha: "eee" },
      ]);
      expect(listCommits).toHaveBeenCalledTimes(1);
    });

    it("cuts a very long subject", async () => {
      const { client } = fakeClient([{ sha: "aaa", commit: { message: "x".repeat(1000) } } as { sha: string }]);
      const [commit] = await new CommitRanger(client).getCommits("owner", "repo", 1);
      expect(commit.subject).toHaveLength(200);
    });

    it("never cuts a subject in the middle of a character that takes two code units", async () => {
      // An emoji whose first half is the 200th code unit: the cut leaves it out whole, and no half of it is kept.
      const message = `${"x".repeat(199)}\u{1F600}${"y".repeat(50)}`;
      const { client } = fakeClient([{ sha: "aaa", commit: { message } } as { sha: string }]);
      const [commit] = await new CommitRanger(client).getCommits("owner", "repo", 1);
      expect(commit.subject).toBe("x".repeat(199));
      const last = commit.subject!.charCodeAt(commit.subject!.length - 1);
      expect(last >= 0xd800 && last <= 0xdbff).toBe(false);
      // An emoji that fits is kept.
      const fits = `${"x".repeat(198)}\u{1F600}${"y".repeat(50)}`;
      const second = fakeClient([{ sha: "bbb", commit: { message: fits } } as { sha: string }]);
      expect((await new CommitRanger(second.client).getCommits("owner", "repo", 1))[0].subject).toBe(`${"x".repeat(198)}\u{1F600}`);
    });

    it("lists the merge commit of a merged pull request last, marked, and each commit once", async () => {
      const listCommits = vi.fn().mockResolvedValue({ data: [{ sha: "a".repeat(40), commit: { message: "one" } }, { sha: "b".repeat(40) }] });
      const get = vi.fn().mockResolvedValue({ data: { merged_at: "2026-07-21T00:00:00Z", merge_commit_sha: "c".repeat(40) } });
      const client = { octokit: { rest: { pulls: { listCommits, get } } } } as unknown as GithubClient;
      expect(await new CommitRanger(client).getCommits("owner", "repo", 1)).toEqual([
        { sha: "a".repeat(40), subject: "one" },
        { sha: "b".repeat(40) },
        { sha: "c".repeat(40), merge: true },
      ]);
    });

    it("does not call a commit of the pull request the merge commit when a rebase merge reports one of them", async () => {
      const listCommits = vi.fn().mockResolvedValue({ data: [{ sha: "a".repeat(40), commit: { message: "one" } }] });
      const get = vi.fn().mockResolvedValue({ data: { merged_at: "2026-07-21T00:00:00Z", merge_commit_sha: "a".repeat(40) } });
      const client = { octokit: { rest: { pulls: { listCommits, get } } } } as unknown as GithubClient;
      expect(await new CommitRanger(client).getCommits("owner", "repo", 1)).toEqual([{ sha: "a".repeat(40), subject: "one" }]);
    });

    it("keeps getCommitShas exactly as it was", async () => {
      const { client } = fakeClient([{ sha: "aaa" }, { sha: "bbb" }, { sha: "aaa" }]);
      expect(await new CommitRanger(client).getCommitShas("owner", "repo", 1)).toEqual(["aaa", "bbb"]);
    });
  });
});
