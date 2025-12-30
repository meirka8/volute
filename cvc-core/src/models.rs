use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InteractionId(String);

impl InteractionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for InteractionId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<String> for InteractionId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommitSha(String);

impl CommitSha {
    pub fn new(sha: impl Into<String>) -> Self {
        Self(sha.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String, // UUID
    pub title: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interaction {
    pub id: InteractionId,
    pub conversation_id: String,
    pub parent_id: Option<InteractionId>,
    pub timestamp: DateTime<Utc>,
    
    pub author: Author,
    pub user_prompt: String,
    
    pub model_name: Option<String>,
    pub model_cot: Option<String>,
    pub model_response: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    Human,
    Agent,
    System,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    pub id: Option<i64>, // Database ID, None for new items
    pub interaction_id: InteractionId,
    pub file_path: String,
    
    pub git_blob_sha: Option<String>,
    pub dirty_patch: Option<String>,
    
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecution {
    pub id: Option<i64>, // Database ID
    pub interaction_id: InteractionId,
    
    pub tool_protocol: String, // 'mcp', 'native'
    pub tool_name: String,
    pub arguments: String, // JSON string
    
    pub status: ToolStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ToolStatus {
    Success,
    Failure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactLink {
    pub interaction_id: InteractionId,
    pub git_commit_hash: CommitSha,
    pub link_type: String, // 'generated', 'verified', etc.
}
