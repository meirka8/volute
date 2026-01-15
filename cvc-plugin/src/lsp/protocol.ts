/**
 * CVC LSP Protocol Types
 *
 * These types mirror the Rust protocol types in cvc-lsp/src/protocol.rs
 * and define the custom LSP notifications used by CVC.
 */

/**
 * Author of an interaction - matches cvc-core's Author enum
 */
export type Author = 'human' | 'agent' | 'system' | 'external';

/**
 * Parameters for $/cvc/session/start notification
 * Signals the beginning of a logical task (e.g., opening a chat window)
 */
export interface SessionStartParams {
    /** Title/description of the session */
    title: string;
    /** Unix timestamp (optional, server will use current time if not provided) */
    timestamp?: number;
}

/**
 * Parameters for $/cvc/turn/start notification
 * Sent when user initiates a prompt/request
 */
export interface TurnStartParams {
    /** Correlation ID to match with turn/end */
    id: string;
    /** The user's prompt/question */
    prompt: string;
    /** Source of the prompt */
    author: Author;
    /** Paths of files in context (optional) */
    contextFiles?: string[];
}

/**
 * Parameters for $/cvc/turn/end notification
 * Sent when the model finishes responding
 */
export interface TurnEndParams {
    /** Correlation ID matching the turn/start */
    id: string;
    /** The model's response text */
    response?: string;
    /** Chain of thought / reasoning (if available) */
    chainOfThought?: string;
    /** Model name/identifier */
    model?: string;
}

/**
 * Parameters for $/cvc/link/commit notification
 * Associates interactions with a Git commit
 */
export interface LinkCommitParams {
    /** The Git commit SHA */
    commitSha: string;
    /** IDs of interactions to link to this commit */
    interactionIds: string[];
}

/**
 * Parameters for cvc/timeline/get request (future feature)
 * Requests the cognitive timeline for display
 */
export interface TimelineGetParams {
    /** Maximum number of items to return */
    maxItems?: number;
    /** Include unbound/floating thoughts */
    includeUnbound?: boolean;
}

/**
 * Response structure for cvc/timeline/get (future feature)
 */
export interface TimelineGetResponse {
    /** Floating/pending thoughts not yet linked to commits */
    pending: InteractionSummary[];
    /** Commits with their linked thoughts */
    commits: CommitWithThoughts[];
}

/**
 * Summary of an interaction for timeline display
 */
export interface InteractionSummary {
    /** Unique interaction ID */
    id: string;
    /** Truncated/preview of the prompt */
    promptPreview: string;
    /** Unix timestamp */
    timestamp: number;
    /** Author type */
    author: Author;
}

/**
 * A commit with its linked thoughts
 */
export interface CommitWithThoughts {
    /** Git commit SHA */
    sha: string;
    /** Commit message (first line) */
    message: string;
    /** Unix timestamp of commit */
    timestamp: number;
    /** Thoughts linked to this commit */
    thoughts: InteractionSummary[];
}
