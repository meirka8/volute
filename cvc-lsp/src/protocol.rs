use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionStartParams {
    pub title: String,
    pub timestamp: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TurnStartParams {
    pub prompt: String,
    pub author: String, // 'human', 'agent', etc.
    pub context_files: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TurnEndParams {
    pub response: Option<String>,
    pub chain_of_thought: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LinkCommitParams {
    pub commit_sha: String,
    pub interaction_ids: Vec<String>,
}
