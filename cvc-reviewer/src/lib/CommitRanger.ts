import { GithubClient } from "../api/github";

export class CommitRanger {
    private client: GithubClient;

    constructor(client: GithubClient) {
        this.client = client;
    }

    // Fetch all commit SHAs in a Pull Request
    async getCommitShas(owner: string, repo: string, pullNumber: number): Promise<string[]> {
        const { data } = await this.client.octokit.rest.pulls.listCommits({
            owner,
            repo,
            pull_number: pullNumber,
            per_page: 100, // Handle pagination in real app
        });

        return data.map(commit => commit.sha);
    }
}
