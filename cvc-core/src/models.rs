use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

/// The intentionally small, non-free-text reason vocabulary used in sync tombstones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TombstoneReasonCode {
    UserRequested,
    Security,
    Retention,
}

/// Immutable suppression record.  It deliberately contains no prompt, title, path,
/// or other user supplied content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tombstone {
    pub format: String,
    pub version: u8,
    pub interaction_id: InteractionId,
    pub deleted_at: DateTime<Utc>,
    pub reason_code: TombstoneReasonCode,
    #[serde(default)]
    pub previous_node_oid: Option<String>,
}

/// A non-secret, destination-bound description of a local hard-redaction
/// candidate.  It is deliberately a plan, never a remote transport command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionPlan {
    pub format: String,
    pub version: u8,
    pub repository_fingerprint: String,
    pub destination_fingerprint: String,
    pub target_id: InteractionId,
    /// The advertised `refs/cvc/main` tip, or `None` when advertisement proved it absent.
    pub expected_remote_tip: Option<String>,
    pub replacement_commit: String,
    pub temporary_ref: String,
    pub removed_nodes: u64,
    pub removed_by_commit_entries: u64,
    pub removed_link_entries: u64,
    pub unrelated_entries_retained: u64,
    pub tombstone_oid: String,
    pub created_at: DateTime<Utc>,
    pub warning: String,
}

impl Tombstone {
    pub fn new(
        interaction_id: InteractionId,
        reason_code: TombstoneReasonCode,
        previous_node_oid: Option<String>,
    ) -> Self {
        Self {
            format: "cvc.tombstone/v1".into(),
            version: 1,
            interaction_id,
            // SQLite stores seconds; canonicalize before serializing so a later
            // projection is byte/semantic stable.
            deleted_at: Utc::now()
                .with_nanosecond(0)
                .expect("zero nanoseconds is always valid"),
            reason_code,
            previous_node_oid,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InteractionId(Uuid);

impl InteractionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for InteractionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for InteractionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for InteractionId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::from_str(s)?))
    }
}

impl InteractionId {
    pub fn as_str(&self) -> String {
        self.0.to_string()
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

    /// Tracks which VS Code chat request generated this interaction.
    /// Used for deduplication when re-processing the same request.
    pub source_request_id: Option<String>,
}

/// Closed persistence visibility state. Local captures are always private.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    #[default]
    Private,
    Shared,
}

/// Whether a user explicitly chose to share future turns in a conversation.
/// This is deliberately separate from the current rows' visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FutureSharePolicy {
    Private,
    Shared,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationState {
    Pending,
    Published,
    Unknown,
}

/// Trusted capture provenance; this is assigned by adapters, never decoded from payloads.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CaptureSource {
    Mcp,
    VscodePassive,
    VscodeExplicit,
    CliRun,
    SyncImport,
    #[default]
    Legacy,
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
    /// Git author email from the commit that caused an automatic link.
    /// Absent for links created before this metadata was introduced.
    #[serde(default)]
    pub linked_by: Option<String>,
}
