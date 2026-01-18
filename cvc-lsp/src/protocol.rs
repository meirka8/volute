use cvc_core::models::Author;
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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LinkCommitParams {
    pub commit_sha: String,
    pub interaction_ids: Vec<String>,
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
