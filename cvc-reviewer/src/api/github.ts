import { Octokit } from "octokit";

export class GithubClient {
    public octokit: Octokit;

    constructor(token: string) {
        this.octokit = new Octokit({ auth: token });
    }

    async getUser() {
        const { data } = await this.octokit.rest.users.getAuthenticated();
        return data;
    }

    async getPullRequest(owner: string, repo: string, pullNumber: number) {
        const { data } = await this.octokit.rest.pulls.get({
            owner,
            repo,
            pull_number: pullNumber,
        });
        return data;
    }

    async getPullRequestFiles(owner: string, repo: string, pullNumber: number) {
        const { data } = await this.octokit.rest.pulls.listFiles({
            owner,
            repo,
            pull_number: pullNumber,
        });
        return data;
    }
}

export const createGithubClient = (token: string) => new GithubClient(token);
