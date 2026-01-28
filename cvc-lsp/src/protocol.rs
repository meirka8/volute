use cvc_core::models::Author;
use cvc_core::vscode::ResponsePartRaw;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionStartParams {
    pub title: String,
    pub timestamp: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub id: String, // Correlation ID for start/end
    pub prompt: String,
    pub author: Author, // Type-safe enum
    pub context_files: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnEndParams {
    pub id: String, // Correlation ID
    pub response: Option<String>,
    pub chain_of_thought: Option<String>,
    pub model: Option<String>,
    pub raw_response: Option<Vec<ResponsePartRaw>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LinkCommitParams {
    pub commit_sha: String,
    pub interaction_ids: Vec<String>,
}

// --- Batch Turn Types (for Chat Session Watcher) ---

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnBatchParams {
    /// The VS Code requestId - used for deduplication.
    /// When re-processing the same request, old interactions are deleted first.
    pub source_request_id: String,
    /// The session/conversation ID to group interactions under.
    pub session_id: String,
    /// Model name/identifier used for all segments.
    pub model: Option<String>,
    /// Ordered list of interaction segments (human turn first, then agent turns).
    pub interactions: Vec<InteractionSegment>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionSegment {
    /// Author of this segment: "human" for the initial prompt turn, "agent" for model reasoning turns.
    pub author: Author,
    /// The user prompt (only present on the first/human segment).
    pub user_prompt: Option<String>,
    /// Chain of thought / thinking block (only on agent segments).
    pub chain_of_thought: Option<String>,
    /// The visible response text for this segment.
    pub response: Option<String>,
    /// Context files attached to this segment (typically only on human segment).
    pub context_files: Option<Vec<String>>,
}

// --- Timeline Request/Response Types ---

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineGetParams {
    pub max_items: Option<u32>,
    pub include_unbound: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineGetResponse {
    pub pending: Vec<InteractionSummary>,
    pub commits: Vec<CommitWithThoughts>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionSummary {
    pub id: String,
    pub prompt_preview: String,
    pub timestamp: i64,
    pub author: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitWithThoughts {
    pub sha: String,
    pub message: String,
    pub timestamp: i64,
    pub thoughts: Vec<InteractionSummary>,
}

// --- Interaction Detail Request/Response Types ---

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionGetParams {
    pub id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionDetail {
    pub id: String,
    pub timestamp: i64,
    pub author: String,
    pub user_prompt: String,
    pub model_name: Option<String>,
    pub model_response: Option<String>,
    pub chain_of_thought: Option<String>,
    pub linked_commit: Option<String>,
    pub context_files: Vec<ContextFileInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextFileInfo {
    pub file_path: String,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
}
