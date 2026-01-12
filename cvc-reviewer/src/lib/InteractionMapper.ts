import type { InteractionNode } from "../types/cvc";

export class InteractionMapper {
    private index: Map<string, InteractionNode[]> = new Map();

    constructor(nodes: InteractionNode[]) {
        this.buildIndex(nodes);
    }

    private buildIndex(nodes: InteractionNode[]) {
        for (const node of nodes) {
            if (node.artifact_links) {
                for (const link of node.artifact_links) {
                    const sha = link.git_commit_hash;
                    if (!this.index.has(sha)) {
                        this.index.set(sha, []);
                    }
                    this.index.get(sha)!.push(node);
                }
            }
        }
    }

    // Get interactions for a specific commit
    getInteractionsForCommit(commitSha: string): InteractionNode[] {
        return this.index.get(commitSha) || [];
    }

    // Get interactions for a list of commits (e.g. PR range)
    getInteractionsForRange(commitShas: string[]): InteractionNode[] {
        const result = new Set<InteractionNode>();
        for (const sha of commitShas) {
            const nodes = this.getInteractionsForCommit(sha);
            nodes.forEach(node => result.add(node));
        }
        return Array.from(result);
    }
}
