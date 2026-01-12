export interface InteractionNode {
    id: string; // SHA-256 of content
    conversation_id: string;
    parent_id?: string;
    timestamp: number;
    author: 'human' | 'agent' | 'system' | 'external';
    user_prompt?: string;
    model_response?: string;
    model_cot?: string; // Chain of Thought

    // Embedded links (assuming Phase 1 simple structure for now)
    // In a full relational DB these are separate, but effectively serialized as:
    artifact_links?: ArtifactLink[];
}

export interface ArtifactLink {
    git_commit_hash: string;
    link_type: 'generated' | 'verified' | 'refactored';
}

export interface CVCTreeItem {
    path: string;
    mode: string;
    type: 'blob' | 'tree';
    sha: string;
    size?: number;
    url: string;
}
