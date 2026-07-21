import { GithubClient } from "../api/github";

export class CommitRanger {
    private client: GithubClient;

    constructor(client: GithubClient) {
        this.client = client;
    }

    // GitHub returns at most 100 commits/page; bound pagination and total work.
    async getCommitShas(owner: string, repo: string, pullNumber: number): Promise<string[]> {
        const commits: string[] = [];
        const maxPages = 100;
        for (let page = 1; page <= maxPages; page++) {
            const { data } = await this.client.octokit.rest.pulls.listCommits({ owner, repo, pull_number: pullNumber, per_page: 100, ...(page === 1 ? {} : { page }) });
            if (!Array.isArray(data) || data.some((commit) => typeof commit.sha !== "string" || !commit.sha)) throw new Error("GitHub returned an invalid PR commit SHA");
            commits.push(...data.map((commit) => commit.sha));
            if (data.length < 100) break;
            if (page === maxPages) throw new Error("PR commit pagination exceeds reviewer limit");
        }
        // `get` exists on real Octokit. Keeping this guarded also lets old API
        // adapters retain their v1-v4 behavior rather than manufacturing a merge.
        if (typeof this.client.octokit.rest.pulls.get !== "function") return [...new Set(commits)];
        const { data: pr } = await this.client.octokit.rest.pulls.get({ owner, repo, pull_number: pullNumber });
        const mergeSha = pr.merged_at ? pr.merge_commit_sha : null;
        if (mergeSha != null && (typeof mergeSha !== "string" || !/^[0-9a-f]{40}$/i.test(mergeSha))) throw new Error("GitHub returned an invalid merge commit SHA");
        return [...new Set([...commits, ...(mergeSha ? [mergeSha] : [])])];
    }
}
