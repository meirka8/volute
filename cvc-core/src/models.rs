use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

pub const MAX_RANGE_COMMITS: usize = 2048;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeObservationOrigin {
    Explicit,
    PrePush,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangeMember {
    pub commit_oid: CommitSha,
}

/// Immutable anchored proof that one ordered commit range has a particular net
/// tree transition. Trust, destination authority, timestamps and source-ref
/// names are observations stored beside this body and are not part of its ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangeEvidence {
    pub range_id: String,
    pub format: String,
    pub version: u8,
    pub repository_identity: String,
    pub object_format: String,
    pub base_oid: CommitSha,
    pub tip_oid: CommitSha,
    pub base_tree_oid: String,
    pub result_tree_oid: String,
    pub commits: Vec<RangeMember>,
    pub changeset_algorithm: String,
    pub changeset_digest: String,
}

/// Local-only source material captured with a range. Never serialized under
/// `ranges/` and always scoped to exactly one interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeSourceSnapshot {
    pub interaction_id: InteractionId,
    pub source_id: String,
    pub snapshot: SourceSnapshot,
}

impl RangeEvidence {
    pub fn canonical_id(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"cvc.range-evidence/canonical/v1\0");
        fn put(h: &mut Sha256, tag: u8, bytes: &[u8]) {
            h.update([tag]);
            h.update((bytes.len() as u64).to_be_bytes());
            h.update(bytes);
        }
        for (tag, value) in [
            (1, self.format.as_str()),
            (2, &self.version.to_string()),
            (3, self.repository_identity.as_str()),
            (4, self.object_format.as_str()),
            (5, self.base_oid.as_str()),
            (6, self.tip_oid.as_str()),
            (7, self.base_tree_oid.as_str()),
            (8, self.result_tree_oid.as_str()),
            (9, self.changeset_algorithm.as_str()),
            (10, self.changeset_digest.as_str()),
        ] {
            put(&mut h, tag, value.as_bytes());
        }
        h.update([11]);
        h.update((self.commits.len() as u64).to_be_bytes());
        for member in &self.commits {
            put(&mut h, 12, member.commit_oid.as_str().as_bytes());
        }
        hex::encode(h.finalize())
    }
    pub fn verify_id(&self) -> bool {
        fn oid(value: &str) -> bool {
            value.len() == 40
                && value
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        }
        let mut commits = std::collections::HashSet::new();
        self.range_id == self.canonical_id()
            && self.range_id.len() == 64
            && self
                .range_id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            && self.format == "cvc.range-evidence/v1"
            && self.version == 1
            && self.object_format == "sha1"
            && self.repository_identity.len() == 64
            && self
                .repository_identity
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            && oid(self.base_oid.as_str())
            && oid(self.tip_oid.as_str())
            && oid(&self.base_tree_oid)
            && oid(&self.result_tree_oid)
            && !self.commits.is_empty()
            && self.commits.len() <= MAX_RANGE_COMMITS
            && self
                .commits
                .iter()
                .all(|m| oid(m.commit_oid.as_str()) && commits.insert(m.commit_oid.as_str()))
            && self.changeset_algorithm == "cvc.changeset/v1"
            && self.changeset_digest.len() == 64
            && self
                .changeset_digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }
}

/// Closed relation vocabulary.  New relation kinds require a format change, not
/// an arbitrary string supplied by a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivationRelation {
    Generated,
    Temporal,
    Verified,
    RewriteExact,
    SquashExact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    LocallyObserved,
    ImportedLegacy,
    RemoteAssertion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub version: u8,
    pub kind: EvidenceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivationOrigin {
    LocalHook,
    LocalLinker,
    RemoteAssertion,
    LegacyImport,
}

/// Immutable, content-addressed explanation of why an interaction is related
/// to an artifact.  `event_id` is deliberately independent of wall time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivationEvent {
    pub event_id: String,
    pub interaction_id: InteractionId,
    pub target_commit: CommitSha,
    pub relation: DerivationRelation,
    pub evidence: Evidence,
    pub origin: DerivationOrigin,
    #[serde(default)]
    pub source_event_ids: Vec<String>,
    #[serde(default)]
    pub old_oid: Option<CommitSha>,
    #[serde(default)]
    pub new_oid: Option<CommitSha>,
    #[serde(default)]
    pub range_id: Option<String>,
    #[serde(default)]
    pub linked_by: Option<String>,
}

/// Immutable preflight input to a rewrite.  It is deliberately a value object:
/// the write phase re-reads it under BEGIN IMMEDIATE and requires byte-for-byte
/// equality before it can append any derived event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceSnapshot {
    Legacy {
        interaction: String,
        commit: String,
        link_type: String,
        linked_by: Option<String>,
        authorization_rows: Vec<SourceAuthorizationRow>,
    },
    Event {
        event_id: String,
        canonical_payload: String,
        canonical_payload_digest: String,
        trusted_local_observations: Vec<SourceObservationRow>,
        authorization_rows: Vec<SourceAuthorizationRow>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceObservationRow {
    pub source_fingerprint: Option<String>,
    pub source_key: String,
    pub origin: String,
    pub trusted_local: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthorizationRow {
    pub remote_fingerprint: String,
    pub source_event_id: String,
}

impl DerivationEvent {
    pub fn sources_are_canonical(&self) -> bool {
        self.source_event_ids
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    }
    /// Domain separated, unambiguous length-prefixed canonical payload.
    pub fn canonical_id(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"cvc.derivation-event/canonical/v2\0");
        // Every field is tagged and every optional value has a discriminator:
        // no concatenation/empty value ambiguity is possible.
        fn put(h: &mut Sha256, tag: u8, value: &str) {
            h.update([tag]);
            h.update((value.len() as u64).to_be_bytes());
            h.update(value.as_bytes());
        }
        put(&mut h, 1, &self.interaction_id.to_string());
        put(&mut h, 2, self.target_commit.as_str());
        put(
            &mut h,
            3,
            match self.relation {
                DerivationRelation::Generated => "generated",
                DerivationRelation::Temporal => "temporal",
                DerivationRelation::Verified => "verified",
                DerivationRelation::RewriteExact => "rewrite_exact",
                DerivationRelation::SquashExact => "squash_exact",
            },
        );
        put(&mut h, 4, &self.evidence.version.to_string());
        put(
            &mut h,
            5,
            match self.evidence.kind {
                EvidenceKind::LocallyObserved => "locally_observed",
                EvidenceKind::ImportedLegacy => "imported_legacy",
                EvidenceKind::RemoteAssertion => "remote_assertion",
            },
        );
        put(
            &mut h,
            6,
            match self.origin {
                DerivationOrigin::LocalHook => "local_hook",
                DerivationOrigin::LocalLinker => "local_linker",
                DerivationOrigin::RemoteAssertion => "remote_assertion",
                DerivationOrigin::LegacyImport => "legacy_import",
            },
        );
        let mut sources = self.source_event_ids.clone();
        sources.sort();
        sources.dedup();
        h.update([7]);
        h.update((sources.len() as u64).to_be_bytes());
        for id in &sources {
            put(&mut h, 8, id);
        }
        for (tag, value) in [
            (9, self.old_oid.as_ref().map(CommitSha::as_str)),
            (10, self.new_oid.as_ref().map(CommitSha::as_str)),
            (11, self.range_id.as_deref()),
            (12, self.linked_by.as_deref()),
        ] {
            h.update([tag, value.is_some() as u8]);
            if let Some(v) = value {
                put(&mut h, tag, v);
            }
        }
        format!("{:x}", h.finalize())
    }
    pub fn verify_id(&self) -> bool {
        self.event_id == self.canonical_id()
    }
}

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RedactionPlan {
    pub format: String,
    pub version: u8,
    /// v1: legacy active-worktree hash; v2: common-Git-dir identity hash.
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

impl RedactionPlan {
    pub fn v2(repository_fingerprint: String, common: RedactionPlanFields) -> Self {
        Self {
            format: "cvc.redaction-plan/v2".into(),
            version: 2,
            repository_fingerprint,
            ..common.into_plan()
        }
    }
    pub fn legacy_v1(repository_fingerprint: String, common: RedactionPlanFields) -> Self {
        Self {
            format: "cvc.redaction-plan/v1".into(),
            version: 1,
            repository_fingerprint,
            ..common.into_plan()
        }
    }
    pub fn destination_fingerprint(&self) -> &str {
        &self.destination_fingerprint
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionPlanFields {
    pub destination_fingerprint: String,
    pub target_id: InteractionId,
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
impl RedactionPlanFields {
    fn into_plan(self) -> RedactionPlan {
        RedactionPlan {
            format: String::new(),
            version: 0,
            repository_fingerprint: String::new(),
            destination_fingerprint: self.destination_fingerprint,
            target_id: self.target_id,
            expected_remote_tip: self.expected_remote_tip,
            replacement_commit: self.replacement_commit,
            temporary_ref: self.temporary_ref,
            removed_nodes: self.removed_nodes,
            removed_by_commit_entries: self.removed_by_commit_entries,
            removed_link_entries: self.removed_link_entries,
            unrelated_entries_retained: self.unrelated_entries_retained,
            tombstone_oid: self.tombstone_oid,
            created_at: self.created_at,
            warning: self.warning,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanWire {
    destination_fingerprint: String,
    target_id: InteractionId,
    expected_remote_tip: Option<String>,
    replacement_commit: String,
    temporary_ref: String,
    removed_nodes: u64,
    removed_by_commit_entries: u64,
    removed_link_entries: u64,
    unrelated_entries_retained: u64,
    tombstone_oid: String,
    created_at: DateTime<Utc>,
    warning: String,
}

// Deserialize the discriminator in the same pass as the payload. Going via a
// `serde_json::Value` would silently collapse duplicate object keys before the
// typed structs could reject them, making security-sensitive fields ambiguous.
#[derive(Deserialize)]
#[serde(tag = "format")]
enum PlanEnvelope {
    #[serde(rename = "cvc.redaction-plan/v1")]
    V1(V1PlanBody),
    #[serde(rename = "cvc.redaction-plan/v2")]
    V2(V2PlanBody),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1PlanBody {
    version: u8,
    repository_fingerprint: String,
    #[serde(flatten)]
    common: PlanWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V2PlanBody {
    version: u8,
    repository_fingerprint: String,
    #[serde(flatten)]
    common: PlanWire,
}
impl From<PlanWire> for RedactionPlanFields {
    fn from(x: PlanWire) -> Self {
        Self {
            destination_fingerprint: x.destination_fingerprint,
            target_id: x.target_id,
            expected_remote_tip: x.expected_remote_tip,
            replacement_commit: x.replacement_commit,
            temporary_ref: x.temporary_ref,
            removed_nodes: x.removed_nodes,
            removed_by_commit_entries: x.removed_by_commit_entries,
            removed_link_entries: x.removed_link_entries,
            unrelated_entries_retained: x.unrelated_entries_retained,
            tombstone_oid: x.tombstone_oid,
            created_at: x.created_at,
            warning: x.warning,
        }
    }
}
impl<'de> Deserialize<'de> for RedactionPlan {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match PlanEnvelope::deserialize(d)? {
            PlanEnvelope::V1(p) if p.version == 1 => {
                Ok(Self::legacy_v1(p.repository_fingerprint, p.common.into()))
            }
            PlanEnvelope::V2(p) if p.version == 2 => {
                Ok(Self::v2(p.repository_fingerprint, p.common.into()))
            }
            _ => Err(serde::de::Error::custom(
                "unsupported or mismatched redaction plan format/version",
            )),
        }
    }
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
