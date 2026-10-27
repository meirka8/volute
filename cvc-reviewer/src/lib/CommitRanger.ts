import { GithubClient } from "../api/github";
import { cutAtCodePoint } from "./textLines";

/** A commit of a pull request: its hash, and what is known about it without another request. */
export interface PRCommit {
    sha: string;
    /** The first line of the commit message, as the commit list reports it. */
    subject?: string;
    /** The commit that merged the pull request, which is none of the pull request's own. */
    merge?: boolean;
}

/** The most of a message's first line that is kept. */
const MAX_SUBJECT = 200;

function subjectOf(commit: unknown): string | undefined {
    const message = (commit as { commit?: { message?: unknown } } | null)?.commit?.message;
    if (typeof message !== "string") return undefined;
    const end = message.indexOf("\n");
    const line = (end < 0 ? message : message.slice(0, end)).trim();
    return line === "" ? undefined : cutAtCodePoint(line, MAX_SUBJECT);
}

export class CommitRanger {
    private client: GithubClient;

    constructor(client: GithubClient) {
        this.client = client;
    }

    // GitHub returns at most 100 commits/page; bound pagination and total work.
    async getCommits(owner: string, repo: string, pullNumber: number): Promise<PRCommit[]> {
        const commits: PRCommit[] = [];
        const maxPages = 100;
        for (let page = 1; page <= maxPages; page++) {
            const { data } = await this.client.octokit.rest.pulls.listCommits({ owner, repo, pull_number: pullNumber, per_page: 100, ...(page === 1 ? {} : { page }) });
            if (!Array.isArray(data) || data.some((commit) => typeof commit.sha !== "string" || !commit.sha)) throw new Error("GitHub returned an invalid PR commit SHA");
            for (const commit of data) {
                const subject = subjectOf(commit);
                commits.push(subject === undefined ? { sha: commit.sha } : { sha: commit.sha, subject });
            }
            if (data.length < 100) break;
            if (page === maxPages) throw new Error("PR commit pagination exceeds reviewer limit");
        }
        // Each commit once, as the first mention of it has it (a rebased merge can report a commit the list has already).
        const unique = (list: PRCommit[]) => {
            const seen = new Set<string>();
            return list.filter((commit) => !seen.has(commit.sha) && !!seen.add(commit.sha));
        };
        // `get` exists on real Octokit. Keeping this guarded also lets old API
        // adapters retain their v1-v4 behavior rather than manufacturing a merge.
        if (typeof this.client.octokit.rest.pulls.get !== "function") return unique(commits);
        const { data: pr } = await this.client.octokit.rest.pulls.get({ owner, repo, pull_number: pullNumber });
        const mergeSha = pr.merged_at ? pr.merge_commit_sha : null;
        if (mergeSha != null && (typeof mergeSha !== "string" || !/^[0-9a-f]{40}$/i.test(mergeSha))) throw new Error("GitHub returned an invalid merge commit SHA");
        return unique([...commits, ...(mergeSha ? [{ sha: mergeSha, merge: true }] : [])]);
    }

    async getCommitShas(owner: string, repo: string, pullNumber: number): Promise<string[]> {
        return (await this.getCommits(owner, repo, pullNumber)).map((commit) => commit.sha);
    }
}
