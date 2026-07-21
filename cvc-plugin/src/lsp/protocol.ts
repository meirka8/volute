/**
 * CVC LSP Protocol Types
 *
 * These types mirror the Rust protocol types in cvc-lsp/src/protocol.rs
 * and define the custom LSP notifications used by CVC.
 */

/**
 * Author of an interaction - matches cvc-core's Author enum
 */
export type Author = "human" | "agent" | "system" | "external";

/** Read-only policy state. It contains no interaction or remote identity data. */
export interface PrivacyStatus {
  captureAcknowledged: boolean;
  captureNoticeVersion: number;
  passiveCaptureAllowed: boolean;
  privateByDefault: boolean;
  privateDefaultStatement: string;
  sharingSummary: string;
  autoPushEnabled: boolean;
}

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
  /** Raw response parts for robust parsing on server */
  rawResponse?: unknown[];
}

/**
 * Parameters for $/cvc/turn/batch notification
 * Sends a complete set of segmented interactions for a single VS Code chat request.
 * Used by the Chat Session Watcher for retroactive parsing.
 */
export interface TurnBatchParams {
  /** The VS Code requestId - used for deduplication */
  sourceRequestId: string;
  /** The session/conversation ID */
  sessionId: string;
  /** Model name/identifier */
  model?: string;
  /** Ordered list of interaction segments */
  interactions: InteractionSegment[];
}

/**
 * A single segment of a chat interaction.
 * The first segment is typically author: "human" with the prompt.
 * Subsequent segments are author: "agent" with chain of thought + response.
 */
export interface InteractionSegment {
  /** Author of this segment */
  author: Author;
  /** User prompt (only on human segments) */
  userPrompt?: string;
  /** Chain of thought / thinking block (only on agent segments) */
  chainOfThought?: string;
  /** Visible response text */
  response?: string;
  /** Context files (typically only on human segment) */
  contextFiles?: string[];
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
  /** The SHA of the current HEAD commit to filter by */
  headSha?: string;
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
  /** Flags indicating which content types are present */
  hasPrompt: boolean;
  hasCot: boolean;
  hasResponse: boolean;
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

/**
 * Parameters for cvc/interaction/get request
 * Fetches full details of a specific interaction
 */
export interface InteractionGetParams {
  /** The interaction ID to fetch */
  id: string;
}

/**
 * Full interaction details returned by cvc/interaction/get
 */
export interface InteractionDetail {
  /** Unique interaction ID */
  id: string;
  /** Conversation this interaction belongs to */
  conversationId: string;
  /** Parent interaction ID (if any) */
  parentId?: string;
  /** Unix timestamp */
  timestamp: number;
  /** Author of the prompt */
  author: Author;
  /** Full user prompt */
  userPrompt: string;
  /** Model name used */
  modelName?: string;
  /** Chain of thought / reasoning */
  chainOfThought?: string;
  /** Model's response */
  modelResponse?: string;
  /** Context files attached to this interaction */
  contextFiles?: ContextFileInfo[];
  /** Tool executions during this interaction */
  toolExecutions?: ToolExecutionInfo[];
  /** Linked commit SHA (if bound) */
  linkedCommit?: string;
}

/**
 * Information about a context file
 */
export interface ContextFileInfo {
  /** File path */
  path: string;
  /** Git blob SHA (if file was clean) */
  blobSha?: string;
  /** Line range (if partial) */
  startLine?: number;
  endLine?: number;
}

/**
 * Information about a tool execution
 */
export interface ToolExecutionInfo {
  /** Tool name */
  name: string;
  /** Tool protocol (mcp, native, etc.) */
  protocol: string;
  /** Arguments passed to the tool */
  arguments?: string;
  /** Execution status */
  status: "success" | "failure";
}
