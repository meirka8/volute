// The actual structure stored in CVC blobs
export interface CVCBlobData {
  interaction: InteractionData;
  context_items: ContextItem[];
  tool_executions: ToolExecution[];
  artifact_links: ArtifactLinkData[];
}

// The interaction data as stored in the blob
export interface InteractionData {
  id: string;
  conversation_id: string;
  parent_id: string | null;
  timestamp: string; // ISO 8601 string
  author: "human" | "agent" | "system" | "external";
  user_prompt?: string | null;
  model_name?: string | null;
  model_cot?: string | null;
  model_response?: string | null;
}

export interface ContextItem {
  file_path: string;
  git_blob_sha?: string;
  dirty_patch?: string;
  start_line?: number;
  end_line?: number;
}

export interface ToolExecution {
  tool_protocol: string;
  tool_name: string;
  arguments: string;
  status: "success" | "failure";
}

export interface ArtifactLinkData {
  interaction_id: string;
  git_commit_hash: string;
  link_type: "generated" | "verified" | "refactored";
}

// Normalized InteractionNode for UI use (flattened structure)
export interface InteractionNode {
  id: string;
  conversation_id: string;
  parent_id: string | null;
  timestamp: number; // Unix timestamp in ms for easier date handling
  timestampRaw: string; // Original ISO string
  author: "human" | "agent" | "system" | "external";
  user_prompt: string | null;
  model_name: string | null;
  model_cot: string | null;
  model_response: string | null;
  context_items: ContextItem[];
  tool_executions: ToolExecution[];
  artifact_links: ArtifactLinkData[];
}

// Helper to normalize blob data to InteractionNode
export function normalizeInteraction(blob: CVCBlobData): InteractionNode {
  const { interaction, context_items, tool_executions, artifact_links } = blob;

  // Parse ISO timestamp to Unix ms
  let timestamp: number;
  try {
    timestamp = new Date(interaction.timestamp).getTime();
  } catch {
    timestamp = 0;
  }

  return {
    id: interaction.id,
    conversation_id: interaction.conversation_id,
    parent_id: interaction.parent_id,
    timestamp,
    timestampRaw: interaction.timestamp,
    author: interaction.author,
    user_prompt: interaction.user_prompt ?? null,
    model_name: interaction.model_name ?? null,
    model_cot: interaction.model_cot ?? null,
    model_response: interaction.model_response ?? null,
    context_items: context_items || [],
    tool_executions: tool_executions || [],
    artifact_links: artifact_links || [],
  };
}

export interface CVCTreeItem {
  path: string;
  mode: string;
  type: "blob" | "tree";
  sha: string;
  size?: number;
  url: string;
}
