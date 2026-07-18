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
  link_type: ArtifactLinkType;
  /** Commit author attribution when the automatic link was created. */
  linked_by?: string | null;
}

/** Link types emitted by current automatic linking and supported legacy blobs. */
export type ArtifactLinkType = "generated" | "temporal" | "verified" | "refactored";

/** The append-only `links/<interaction-id>/<commit-sha>.json` record in sync format v3. */
export type CVCLinkRecord = ArtifactLinkData;

export interface CVCTombstone {
  format: "cvc.tombstone/v1";
  version: 1;
  interaction_id: string;
  deleted_at: string;
  reason_code: "user_requested" | "security" | "retention";
  previous_node_oid?: string | null;
}

export function validTombstone(value: unknown, pathId: string): value is CVCTombstone {
  if (!value || typeof value !== "object") return false;
  const t = value as Record<string, unknown>;
  const allowed = new Set(["format", "version", "interaction_id", "deleted_at", "reason_code", "previous_node_oid"]);
  if (Object.keys(t).some((key) => !allowed.has(key))) return false;
  const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
  const oid = /^[0-9a-f]{40}(?:[0-9a-f]{24})?$/i;
  return t.format === "cvc.tombstone/v1" && t.version === 1 &&
    typeof t.interaction_id === "string" && t.interaction_id === pathId && uuid.test(pathId) &&
    typeof t.deleted_at === "string" && !Number.isNaN(Date.parse(t.deleted_at)) &&
    (t.reason_code === "user_requested" || t.reason_code === "security" || t.reason_code === "retention") &&
    (t.previous_node_oid == null || (typeof t.previous_node_oid === "string" && oid.test(t.previous_node_oid)));
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

/**
 * Adds append-only v3 link records to an immutable node blob. A node may have been
 * pushed while floating, so its original `artifact_links` array cannot be rewritten.
 * Existing legacy links are retained; a matching v3 record fills in newer metadata.
 */
export function mergeArtifactLinks(
  interaction: InteractionNode,
  linkRecords: CVCLinkRecord[],
): InteractionNode {
  if (linkRecords.length === 0) return interaction;

  const links = new Map<string, ArtifactLinkData>();
  for (const link of interaction.artifact_links) {
    links.set(`${link.git_commit_hash}:${link.link_type}`, link);
  }
  for (const link of linkRecords) {
    const key = `${link.git_commit_hash}:${link.link_type}`;
    links.set(key, { ...links.get(key), ...link });
  }

  return { ...interaction, artifact_links: Array.from(links.values()) };
}

export interface CVCTreeItem {
  path: string;
  mode: string;
  type: "blob" | "tree";
  sha: string;
  size?: number;
  url: string;
}
