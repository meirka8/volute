use crate::harness::{HarnessId, SUBAGENT_MARKER};
use crate::models::{
    ArtifactLink, Author, CommitSha, ContextItem, Conversation, DerivationEvent, DerivationOrigin,
    DerivationRelation, EvidenceKind, FutureSharePolicy, Interaction, InteractionId,
    PublicationState, RangeEvidence, SourceAuthorizationRow, SourceObservationRow, SourceSnapshot,
    Tombstone, TombstoneReasonCode, ToolExecution, ToolStatus,
};
use crate::privacy::{self, Capture};
use chrono::{TimeZone, Utc};
use rusqlite::{
    ffi, params, Connection, ErrorCode, OptionalExtension, Row, Transaction, TransactionBehavior,
};
use sha2::Digest;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Migration error: {0}")]
    Migration(String),
    #[error("Timestamp error: invalid timestamp {0}")]
    Timestamp(i64),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid automatic link: {0}")]
    InvalidLink(String),
    #[error("Artifact link integrity conflict: {0}")]
    LinkConflict(String),
    #[error("pending squash target capacity reached ({limit}); discovery deferred")]
    SquashQueueCapacity { limit: i64 },
    #[error("local deletion completed, but filesystem snapshots, backups, and SSD wear-leveling may retain prior bytes")]
    ResidualStorageWarning,
    /// A harness batch was refused before anything of it was stored: its
    /// captures, cursor, evidence or relations are not consistent with each
    /// other or with what the store holds. The text names the rule, never a
    /// captured value.
    #[error("harness batch refused: {0}")]
    HarnessBatch(String),
    /// A conversation relation conflicts with the one on record (another
    /// parent, another spawn, a cycle or too deep a chain). Relations are never
    /// overwritten, so the child's ingestion fails instead.
    #[error("conversation relation conflict: {0}")]
    RelationConflict(String),
    /// A capture by a harness this database cannot record yet: the one-time
    /// `capture_source` upgrade has not run here
    /// ([`CvcStore::needs_capture_source_upgrade`]). Nothing was stored. An
    /// operator command that opens with
    /// [`CvcStore::open_initialized_upgrading`] finishes the upgrade.
    #[error("the CVC database has not finished upgrading for {harness} capture")]
    CaptureSourceUpgradeRequired { harness: HarnessId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryDisposition {
    Retryable,
    Permanent,
}

impl DbError {
    pub(crate) fn retry_disposition(&self) -> RetryDisposition {
        match self {
            Self::Sqlite(_)
            | Self::Io(_)
            | Self::LinkConflict(_)
            | Self::SquashQueueCapacity { .. } => RetryDisposition::Retryable,
            Self::Migration(_)
            | Self::Timestamp(_)
            | Self::InvalidLink(_)
            | Self::ResidualStorageWarning
            | Self::HarnessBatch(_)
            | Self::RelationConflict(_)
            | Self::CaptureSourceUpgradeRequired { .. } => RetryDisposition::Permanent,
        }
    }
}

pub type Result<T> = std::result::Result<T, DbError>;
const MAX_PENDING_SQUASH_TARGETS: i64 = 4096;
const MAX_RESOLVED_SQUASH_TARGETS: i64 = 512;

fn prepare_store_path(path: &Path) -> Result<()> {
    // SQLite's in-memory URI has no filesystem object to secure.
    if path == Path::new(":memory:") {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| DbError::Migration("database path has no parent".into()))?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let meta = std::fs::symlink_metadata(parent)?;
        if meta.file_type().is_symlink()
            || !meta.is_dir()
            || meta.uid() != unsafe { libc::geteuid() }
        {
            return Err(DbError::Migration(
                "CVC directory is not an owned regular directory".into(),
            ));
        }
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        match std::fs::symlink_metadata(path) {
            Ok(meta)
                if meta.file_type().is_symlink()
                    || !meta.is_file()
                    || meta.uid() != unsafe { libc::geteuid() } =>
            {
                return Err(DbError::Migration(
                    "CVC database is not an owned regular file".into(),
                ))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    #[cfg(not(unix))]
    if !path.exists() {
        std::fs::File::create(path)?;
    }
    Ok(())
}

fn enforce_store_permissions(path: &Path) -> Result<()> {
    if path == Path::new(":memory:") {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let uid = unsafe { libc::geteuid() };
        for candidate in [
            path.to_path_buf(),
            PathBuf::from(format!("{}-wal", path.display())),
            PathBuf::from(format!("{}-shm", path.display())),
        ] {
            match std::fs::symlink_metadata(&candidate) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() || !meta.is_file() || meta.uid() != uid {
                        return Err(DbError::Migration("CVC SQLite sidecar is insecure".into()));
                    }
                    std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o600))?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

fn is_safe_commit_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
fn parse_legacy_source(value: &str) -> Option<(String, String)> {
    let rest = value.strip_prefix("legacy:")?;
    let (interaction, commit) = rest.split_once(':')?;
    let canonical = interaction.parse::<InteractionId>().ok()?.to_string();
    if canonical == interaction
        && is_safe_commit_sha(commit)
        && !commit.bytes().any(|b| b.is_ascii_uppercase())
    {
        Some((interaction.into(), commit.into()))
    } else {
        None
    }
}
fn is_event_source_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
fn relation_name(v: DerivationRelation) -> &'static str {
    match v {
        DerivationRelation::Generated => "generated",
        DerivationRelation::Temporal => "temporal",
        DerivationRelation::Verified => "verified",
        DerivationRelation::RewriteExact => "rewrite_exact",
        DerivationRelation::SquashExact => "squash_exact",
    }
}
fn evidence_name(v: EvidenceKind) -> &'static str {
    match v {
        EvidenceKind::LocallyObserved => "locally_observed",
        EvidenceKind::ImportedLegacy => "imported_legacy",
        EvidenceKind::RemoteAssertion => "remote_assertion",
    }
}
fn origin_name(v: DerivationOrigin) -> &'static str {
    match v {
        DerivationOrigin::LocalHook => "local_hook",
        DerivationOrigin::LocalLinker => "local_linker",
        DerivationOrigin::RemoteAssertion => "remote_assertion",
        DerivationOrigin::LegacyImport => "legacy_import",
    }
}

fn snapshot_interaction(snapshot: &SourceSnapshot) -> Option<String> {
    match snapshot {
        SourceSnapshot::Legacy { interaction, .. } => Some(interaction.clone()),
        SourceSnapshot::Event {
            canonical_payload, ..
        } => serde_json::from_str::<DerivationEvent>(canonical_payload)
            .ok()
            .map(|event| event.interaction_id.to_string()),
    }
}
fn source_id_for_snapshot(snapshot: &SourceSnapshot) -> Option<String> {
    match snapshot {
        SourceSnapshot::Legacy {
            interaction,
            commit,
            ..
        } => Some(format!("legacy:{interaction}:{commit}")),
        SourceSnapshot::Event {
            event_id,
            canonical_payload,
            ..
        } => serde_json::from_str::<DerivationEvent>(canonical_payload)
            .ok()
            .filter(|event| event.event_id == *event_id)
            .map(|_| event_id.clone()),
    }
}

fn is_unique_constraint(error: &rusqlite::ffi::Error) -> bool {
    error.code == ErrorCode::ConstraintViolation
        && matches!(
            error.extended_code,
            ffi::SQLITE_CONSTRAINT_PRIMARYKEY | ffi::SQLITE_CONSTRAINT_UNIQUE
        )
}

/// Maps a row selected as
/// `id, conversation_id, parent_id, timestamp, author, user_prompt,
///  model_name, model_cot, model_response, source_request_id`
/// (in that column order) into an `Interaction`.
fn map_interaction_row(row: &Row) -> rusqlite::Result<Interaction> {
    let parent_id_str: Option<String> = row.get(2)?;
    let timestamp: i64 = row.get(3)?;
    let author_str: String = row.get(4)?;
    let author = match author_str.as_str() {
        "human" => Author::Human,
        "agent" => Author::Agent,
        "system" => Author::System,
        _ => Author::External,
    };

    let dt = Utc.timestamp_opt(timestamp, 0).single().ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Integer,
            Box::new(DbError::Timestamp(timestamp)),
        )
    })?;

    Ok(Interaction {
        id: row.get::<_, String>(0)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        conversation_id: row.get(1)?,
        parent_id: parent_id_str.map(|s| s.parse().unwrap_or_default()),
        timestamp: dt,
        author,
        user_prompt: row.get(5)?,
        model_name: row.get(6)?,
        model_cot: row.get(7)?,
        model_response: row.get(8)?,
        source_request_id: row.get(9)?,
    })
}

pub struct CvcStore {
    conn: Connection,
    path: PathBuf,
}

/// Observable result of best-effort SQLite cleanup.  This reports database
/// state only; it intentionally makes no claim about filesystem media erasure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CleanupReport {
    pub busy: i64,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
    pub wal_bytes: u64,
}

/// Read-only per-conversation activity summary for local display. The share
/// and publication fields are scoped to one destination fingerprint and are
/// informational only; they grant nothing. Without a destination they are
/// uniformly absent.
///
/// Share state is derived from the conversation's interactions, never from the
/// conversation-level policy row, so it cannot outlive an unshare. "Shared for
/// the destination" means the same as it does to
/// [`CvcStore::projection_interaction_ids`]: an interaction share for that
/// fingerprint or for the `'*'` wildcard.
///
/// What the destination holds is a separate question from what this clone
/// shares with it, and a reader must not answer the second for the first:
/// "private" means no interaction is shared *and* the destination is not known
/// or suspected to hold any ([`held`](Self::held)). A conversation can be held
/// without being shared from here after an index rebuild or a re-ingest (Claude
/// Code interaction ids are deterministic, so the next fetch finds the
/// destination already holding them), and a pull records what it imports the
/// same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub thoughts: i64,
    pub linked: i64,
    pub first_activity: i64,
    pub last_activity: i64,
    /// Some interaction of the conversation is shared for the destination,
    /// that is `shared_count > 0`.
    pub shared: bool,
    /// How many of the conversation's interactions are currently shared for
    /// the destination.
    pub shared_count: i64,
    /// How many of the shared interactions the destination is known to hold
    /// (publication state `published`). It counts shared interactions only:
    /// an imported interaction that a pull recorded as published at its source
    /// is not counted until it is itself shared.
    pub published: i64,
    /// How many of the shared interactions have an uncertain publication: a
    /// push that began without concluding (`pending`) or failed ambiguously
    /// (`unknown`). `cvc push --manual` and `cvc privacy reconcile` settle
    /// them after a fresh fetch.
    pub unconfirmed: i64,
    /// What the destination holds for this conversation's future turns:
    /// `Some(true)` shares them as they are captured, `Some(false)` keeps them
    /// private (a share without `--future`, or an unshare), `None` when no
    /// share decision was ever recorded for the destination.
    pub share_future: Option<bool>,
    /// The conversation has at least one interaction and every one of them was
    /// imported from a remote (`capture_source = 'sync_import'`). A
    /// conversation with a single locally captured interaction is not imported,
    /// and neither is one with no interactions at all.
    pub imported: bool,
    /// How many of the conversation's interactions the destination holds or may
    /// hold: their publication state for it is `published`, `pending` or
    /// `unknown`, whether or not they are shared from here. Unlike
    /// [`published`](Self::published) and [`unconfirmed`](Self::unconfirmed) it
    /// does not stop at the shared ones, so it is what tells a conversation the
    /// destination holds from one it has never seen.
    pub held: i64,
    /// Of the held interactions, how many the destination is known to hold
    /// (state `published`), shared from here or not. Zero means that nothing is
    /// held for certain: every held interaction is still unconfirmed.
    pub held_published: i64,
    /// The agent harness the conversation came from: the harness that
    /// captured its interactions here (when they all name one), or, for a
    /// conversation imported from a remote, the harness its id's prefix names
    /// after `remote:<fingerprint>:`. The second is the publisher's claim, not
    /// provenance. `None` when neither says (Claude Code conversations imported
    /// from a remote among them: their ids are unprefixed).
    pub agent: Option<HarnessId>,
    /// The conversation this one is a subagent or fork of, from an explicit
    /// relation recorded by a harness ([`CvcStore::conversation_relation`]).
    /// Claude Code's `<session>:agent-<id>` convention is not read here; a
    /// reader applies it itself where no relation is recorded.
    pub parent: Option<String>,
}

/// Longest list of context paths [`CvcStore::conversation_details`] returns.
pub const CONVERSATION_DETAIL_PATH_LIMIT: usize = 10;

/// One context file path and how many of a conversation's interactions
/// reference it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextPathUse {
    /// The path exactly as stored. Locally captured paths are relative to the
    /// capturing worktree; paths of an imported conversation are whatever the
    /// remote's projection carried (secret-scrubbed, not otherwise validated),
    /// so treat them as untrusted text.
    pub path: String,
    /// Distinct interactions of the conversation that reference the path.
    pub interactions: i64,
}

/// Read-only detail view of one conversation for one destination, taken from a
/// single consistent snapshot of the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationDetails {
    /// The row [`CvcStore::list_conversation_summaries`] shows for the same
    /// conversation and destination: id, title, activity range, thoughts,
    /// linked, and the share, publication and import state.
    pub summary: ConversationSummary,
    /// The most used context paths, by number of interactions that reference
    /// them (most first), then by path (byte order). At most
    /// [`CONVERSATION_DETAIL_PATH_LIMIT`]. Empty and `NULL` paths are not
    /// counted.
    pub context_paths: Vec<ContextPathUse>,
    /// How many distinct context paths the conversation has in total, so a view
    /// can say "and N more".
    pub context_path_total: usize,
    /// How many interactions a share would cover now: exactly
    /// `share_snapshot(id).len()`, the count the `I SHARE` challenge carries.
    pub snapshot_count: usize,
    /// Of those, how many are already shared for the destination.
    pub snapshot_shared: usize,
    /// Of those, how many a share would newly mark shared
    /// (`snapshot_count - snapshot_shared`).
    pub snapshot_new: usize,
}

/// How much of what is shared with one destination has reached it. It is local
/// state: nothing was fetched, so it can lag the destination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DestinationPublicationCounts {
    /// Interactions currently shared for the destination: an interaction share
    /// for its fingerprint or for the `'*'` wildcard, each counted once. This is
    /// the set `projection_interaction_ids` starts from, before it drops those
    /// whose publication state is `unknown`. Interactions not shared are not
    /// counted, whatever their publication state (a pull records what it
    /// imports as published without sharing it).
    pub shared: i64,
    /// Of the shared ones, those whose publication state for the destination is
    /// `published`.
    pub published: i64,
    /// Of the shared ones, those whose publication state is `pending` (a push
    /// began and did not conclude) or `unknown` (a push failed ambiguously).
    /// They are not published, but a push may still deliver them: it first
    /// reconciles them against a fresh fetch.
    pub unconfirmed: i64,
}

impl DestinationPublicationCounts {
    /// Shared interactions the destination is not known to hold: never
    /// attempted, `pending` or `unknown` (`shared - published`).
    pub fn unpublished(&self) -> i64 {
        self.shared - self.published
    }
}

/// What stands in the way of unsharing one conversation for one destination.
/// Each field counts the conversation's interactions with that publication
/// state for the destination, whether or not they are still shared. Any of them
/// non-zero makes [`CvcStore::unshare_conversation_for_remote`] refuse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnshareCheck {
    pub published: i64,
    pub pending: i64,
    pub unknown: i64,
}

impl UnshareCheck {
    pub fn blocked(&self) -> i64 {
        self.published + self.pending + self.unknown
    }

    /// Whether unsharing would go ahead.
    pub fn is_allowed(&self) -> bool {
        self.blocked() == 0
    }
}

/// Where an agent-harness transcript ingest resumes for one session. Purely a
/// read optimization: captures carry deterministic ids, so a lost or stale
/// cursor only costs a re-read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessIngestCursor {
    pub transcript_path: String,
    pub byte_offset: u64,
    pub updated_at: i64,
}

pub struct HarnessCursorUpdate<'a> {
    pub harness: &'a str,
    pub session_id: &'a str,
    pub transcript_path: &'a str,
    pub byte_offset: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HarnessBatchOutcome {
    /// Captures stored, stand-ins included.
    pub inserted: usize,
    /// Captures skipped because their id is locally tombstoned or was recorded
    /// concurrently.
    pub suppressed: usize,
    /// Of `inserted`, the stand-ins: responses whose capture could not be
    /// prepared (a field the scrubber cannot process, a collection over its
    /// limit) and that are stored as a note instead, so that they do not keep
    /// the responses after them from being recorded.
    pub withheld: usize,
    /// Captures that could not be prepared and whose stand-in could not be
    /// either: skipped, so that the batch and its cursor can go on.
    pub lost: usize,
    /// Why captures were withheld or lost, each reason once, in the order met,
    /// at most [`HARNESS_REASONS_SHOWN`] of them.
    pub reasons: Vec<String>,
    /// [`CvcStore::capture_harness_batch`] only: the stored cursor was kept
    /// because the batch's cursor was behind it (an earlier epoch, the same
    /// epoch and not forward, or an older cursor version). The captures still
    /// committed.
    pub cursor_held: bool,
    /// [`CvcStore::capture_harness_batch`] only: commit-evidence rows that are
    /// now stored for the batch (new or already present).
    pub evidence_recorded: usize,
    /// [`CvcStore::capture_harness_batch`] only: evidence rows passed over,
    /// because their candidate is not a hex token of 4 to 64 digits or their
    /// interaction is not stored in the batch's conversation.
    pub evidence_skipped: usize,
    /// [`CvcStore::capture_harness_batch`] only: relations newly recorded or
    /// completed (a missing spawn filled in).
    pub relations_recorded: usize,
}

/// How many distinct reasons a [`HarnessBatchOutcome`] lists.
pub const HARNESS_REASONS_SHOWN: usize = 8;

/// The longest chain of relations above a conversation: a subagent of a
/// subagent ... at most this many levels below its root.
pub const MAX_RELATION_DEPTH: usize = 8;

/// The largest encoded cursor state [`CvcStore::capture_harness_batch`]
/// stores.
pub const MAX_CURSOR_STATE_BYTES: usize = 64 * 1024;

/// A typed ingest cursor as stored in `harness_cursors`. The state's meaning
/// belongs to the harness's cursor codec, which reads it by `kind` and
/// `version`; the store only keeps it and compares epochs.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredHarnessCursor {
    pub harness: String,
    pub cursor_key: String,
    pub kind: String,
    pub version: u32,
    pub epoch: u64,
    pub state: serde_json::Value,
    pub updated_at: i64,
}

/// How a new cursor of the same epoch compares with the stored one, as the
/// harness's codec judges it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorProgress {
    Forward,
    Same,
    Backward,
}

/// The typed cursor a [`HarnessBatch`] commits.
///
/// Inside the batch's transaction the store compares it with the stored one:
/// an earlier epoch, or an older cursor `version`, keeps the stored cursor;
/// the same epoch asks `progress` and keeps the stored one when it says
/// [`CursorProgress::Backward`]; exactly the next epoch replaces it (the
/// record was replaced, and the codec says why in `state`); any later epoch is
/// refused. A cursor is never moved backwards.
pub struct HarnessCursorWrite<'a> {
    /// The conversation id, or `scan:<db id>` for a scan watermark. A safe
    /// identifier.
    pub cursor_key: &'a str,
    /// The codec's kind: 1 to 32 bytes of `[a-z0-9-]`.
    pub kind: &'a str,
    /// The codec's version, at least 1.
    pub version: u32,
    pub epoch: u64,
    /// The codec's encoding: a JSON object of at most
    /// [`MAX_CURSOR_STATE_BYTES`] bytes.
    pub state: serde_json::Value,
    /// Compares this cursor with the stored one of the same epoch and kind.
    pub progress: &'a dyn Fn(&StoredHarnessCursor) -> CursorProgress,
}

/// One commit a response reported, as a raw hex token from the session's own
/// Git output. It is evidence to check, not a link: the family rescue verifies
/// every candidate against the repository again before it links anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessCommitEvidence {
    /// The conversation whose family the evidence belongs to.
    pub family_root: String,
    /// The response that reported the commit.
    pub interaction_id: InteractionId,
    /// 4 to 64 lowercase hex digits.
    pub candidate: String,
    /// When the response was made, in seconds.
    pub response_at: i64,
}

/// What a child conversation is to its parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationKind {
    /// Spawned by the parent to do part of its work. A subagent belongs to its
    /// parent's commit family.
    Subagent,
    /// A copy of the parent's history that went on separately. Never family.
    Fork,
}

impl RelationKind {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            RelationKind::Subagent => "subagent",
            RelationKind::Fork => "fork",
        }
    }
    fn from_db_str(value: &str) -> Option<Self> {
        [RelationKind::Subagent, RelationKind::Fork]
            .into_iter()
            .find(|kind| kind.as_db_str() == value)
    }
}

/// Where the exact evidence for a relation was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationEvidence {
    /// The child's own record names its parent.
    ChildRecord,
    /// The parent's record names the child it spawned.
    ParentRecord,
    /// The agent's hook payload named both.
    HookPayload,
    /// The agent's own session index names both.
    StoreIndex,
}

impl RelationEvidence {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            RelationEvidence::ChildRecord => "child_record",
            RelationEvidence::ParentRecord => "parent_record",
            RelationEvidence::HookPayload => "hook_payload",
            RelationEvidence::StoreIndex => "store_index",
        }
    }
    fn from_db_str(value: &str) -> Option<Self> {
        [
            RelationEvidence::ChildRecord,
            RelationEvidence::ParentRecord,
            RelationEvidence::HookPayload,
            RelationEvidence::StoreIndex,
        ]
        .into_iter()
        .find(|evidence| evidence.as_db_str() == value)
    }
}

/// A child conversation's parent, as one harness recorded it from exact
/// evidence. Local state only: it is never published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationRelation {
    pub child_id: String,
    pub parent_id: String,
    pub kind: RelationKind,
    pub harness: HarnessId,
    /// The parent's response that spawned the child, where the agent records
    /// it.
    pub spawn_interaction_id: Option<InteractionId>,
    /// The agent's own id for the spawning call: a safe identifier.
    pub spawn_call_id: Option<String>,
    pub evidence: RelationEvidence,
}

/// Everything one harness ingest batch commits for one conversation, in one
/// immediate transaction: all of it or none of it.
pub struct HarnessBatch<'a> {
    pub harness: HarnessId,
    /// The conversation every capture of the batch records into.
    pub conversation_id: &'a str,
    pub captures: Vec<crate::privacy::HarnessCaptureInput>,
    /// The typed cursor to advance, if the batch moves one.
    pub cursor: Option<HarnessCursorWrite<'a>>,
    /// Claude Code only: the 0.7.x cursor row, kept written for older binaries
    /// that share this database. It is written exactly when the typed cursor
    /// is (or when the batch has no typed cursor).
    pub legacy_cursor: Option<HarnessCursorUpdate<'a>>,
    pub evidence: Vec<HarnessCommitEvidence>,
    /// Relations in which this batch's conversation is the child or the
    /// parent. Never for Claude Code, whose relation is its id convention.
    pub relations: Vec<ConversationRelation>,
}

/// A harness batch's captures after the capture boundary, before SQLite is
/// touched.
struct PreparedHarnessBatch {
    /// Each prepared capture, and whether it is a stand-in.
    prepared: Vec<(Capture, bool)>,
    /// What could not be stored at all, and what each was a child of.
    lost: Vec<(InteractionId, Option<InteractionId>)>,
    reasons: Vec<String>,
}

impl PreparedHarnessBatch {
    /// A capture that cannot be prepared is stored as a stand-in, or skipped
    /// when not even that works; it never fails the batch, because the cursor
    /// only moves when the batch is stored.
    fn prepare(captures: impl Iterator<Item = Capture>) -> Self {
        let mut batch = Self {
            prepared: Vec::with_capacity(captures.size_hint().0),
            lost: Vec::new(),
            reasons: Vec::new(),
        };
        let note = |reasons: &mut Vec<String>, reason: String| {
            if reasons.len() < HARNESS_REASONS_SHOWN && !reasons.contains(&reason) {
                reasons.push(reason);
            }
        };
        for capture in captures {
            match privacy::prepare_harness(capture) {
                privacy::HarnessCapture::Prepared(capture) => batch.prepared.push((capture, false)),
                privacy::HarnessCapture::Withheld { stand_in, reason } => {
                    note(&mut batch.reasons, reason);
                    batch.prepared.push((stand_in, true));
                }
                privacy::HarnessCapture::Lost {
                    id,
                    parent_id,
                    reason,
                } => {
                    note(&mut batch.reasons, reason);
                    batch.lost.push((id, parent_id));
                }
            }
        }
        batch
    }
}

/// "Shared for the destination `?1`": an interaction share for that
/// fingerprint or for the `'*'` wildcard. This is the sharing test
/// `projection_interaction_ids` applies, without its publication-state filter.
/// No destination (`NULL`) shares nothing, wildcard included.
const SHARED_FOR_DESTINATION: &str = "(?1 IS NOT NULL AND EXISTS (SELECT 1 FROM interaction_shares s WHERE s.interaction_id = i.id AND s.remote_fingerprint IN (?1, '*')))";

/// Whether any row is kept under the destination fingerprint `?1`, in any of
/// the places [`ADOPT_DESTINATION_ROWS`] moves rows between fingerprints.
const DESTINATION_ROWS_EXIST: &str = "SELECT EXISTS(SELECT 1 FROM conversation_shares WHERE remote_fingerprint = ?1)
    OR EXISTS(SELECT 1 FROM interaction_shares WHERE remote_fingerprint = ?1)
    OR EXISTS(SELECT 1 FROM publications WHERE remote_fingerprint = ?1)
    OR EXISTS(SELECT 1 FROM tombstones WHERE scope_kind = 'remote' AND remote_fingerprint = ?1)
    OR EXISTS(SELECT 1 FROM derivation_authorizations WHERE remote_fingerprint = ?1)
    OR EXISTS(SELECT 1 FROM range_authorizations WHERE remote_fingerprint = ?1)
    OR EXISTS(SELECT 1 FROM derivation_observations WHERE origin = 'remote_sync' AND (source_fingerprint = ?1 OR source_key = ?1))
    OR EXISTS(SELECT 1 FROM range_observations WHERE origin = 'remote_sync' AND (source_key = ?1 OR source_remote = ?1))";

/// Moves every row kept under the destination fingerprint `?1` to `?2`. The
/// observations a pull recorded (`remote_sync`) carry the fingerprint in their
/// key column too. Run only when nothing is kept under `?2` yet, so that no
/// primary key can collide.
const ADOPT_DESTINATION_ROWS: [&str; 8] = [
    "UPDATE conversation_shares SET remote_fingerprint = ?2 WHERE remote_fingerprint = ?1",
    "UPDATE interaction_shares SET remote_fingerprint = ?2 WHERE remote_fingerprint = ?1",
    "UPDATE publications SET remote_fingerprint = ?2 WHERE remote_fingerprint = ?1",
    "UPDATE tombstones SET remote_fingerprint = ?2 WHERE scope_kind = 'remote' AND remote_fingerprint = ?1",
    "UPDATE derivation_authorizations SET remote_fingerprint = ?2 WHERE remote_fingerprint = ?1",
    "UPDATE range_authorizations SET remote_fingerprint = ?2 WHERE remote_fingerprint = ?1",
    "UPDATE derivation_observations SET
         source_fingerprint = CASE WHEN source_fingerprint = ?1 THEN ?2 ELSE source_fingerprint END,
         source_key = CASE WHEN source_key = ?1 THEN ?2 ELSE source_key END
     WHERE origin = 'remote_sync' AND (source_fingerprint = ?1 OR source_key = ?1)",
    "UPDATE range_observations SET
         source_key = CASE WHEN source_key = ?1 THEN ?2 ELSE source_key END,
         source_remote = CASE WHEN source_remote = ?1 THEN ?2 ELSE source_remote END
     WHERE origin = 'remote_sync' AND (source_key = ?1 OR source_remote = ?1)",
];

/// The summary query up to its `tail` (`WHERE`/`GROUP BY`/`ORDER BY`/`LIMIT`).
/// `?1` is the destination fingerprint, or `NULL` for none. Every reader of
/// summaries goes through this one statement, so no two of them can disagree
/// about what "shared" or "published" means.
fn conversation_summary_sql(tail: &str) -> String {
    let harness_sources = HarnessId::ALL
        .iter()
        .map(|harness| format!("'{}'", harness.capture_source_str()))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "SELECT c.id, c.title,
                COUNT(i.id),
                COALESCE(SUM(EXISTS(SELECT 1 FROM artifact_links a WHERE a.interaction_id = i.id)), 0),
                COALESCE(MIN(i.timestamp), c.created_at),
                COALESCE(MAX(i.timestamp), c.created_at),
                COALESCE(SUM({SHARED_FOR_DESTINATION}), 0),
                COALESCE(SUM({SHARED_FOR_DESTINATION} AND EXISTS(SELECT 1 FROM publications p WHERE p.interaction_id = i.id AND p.remote_fingerprint = ?1 AND p.state = 'published')), 0),
                COALESCE(SUM({SHARED_FOR_DESTINATION} AND EXISTS(SELECT 1 FROM publications p WHERE p.interaction_id = i.id AND p.remote_fingerprint = ?1 AND p.state IN ('pending', 'unknown'))), 0),
                (SELECT s.share_future FROM conversation_shares s WHERE s.conversation_id = c.id AND s.remote_fingerprint = ?1),
                (COUNT(i.id) > 0 AND COUNT(i.id) = COALESCE(SUM(i.capture_source = 'sync_import'), 0)),
                COALESCE(SUM(EXISTS(SELECT 1 FROM publications p WHERE p.interaction_id = i.id AND p.remote_fingerprint = ?1 AND p.state IN ('published', 'pending', 'unknown'))), 0),
                COALESCE(SUM(EXISTS(SELECT 1 FROM publications p WHERE p.interaction_id = i.id AND p.remote_fingerprint = ?1 AND p.state = 'published')), 0),
                (SELECT CASE WHEN COUNT(DISTINCT h.capture_source) = 1 THEN MIN(h.capture_source) END FROM interactions h WHERE h.conversation_id = c.id AND h.capture_source IN ({harness_sources})),
                (SELECT r.parent_id FROM conversation_relations r WHERE r.child_id = c.id)
         FROM conversations c
         LEFT JOIN interactions i ON i.conversation_id = c.id
         {tail}"
    )
}

/// The interactions a share of one conversation (`?1`) covers, in the order the
/// `I SHARE` token hashes them. `share_snapshot` returns exactly these ids and
/// `conversation_details` counts exactly these rows, so the number shown before
/// a share is the number in its challenge.
const SHARE_SNAPSHOT_SQL: &str = "SELECT id FROM interactions WHERE conversation_id=?1 ORDER BY id";

/// The context paths of one conversation (`?1`), shared by the listing of the
/// most used paths and the count of all of them.
const CONVERSATION_CONTEXT_PATHS: &str = "FROM context_items x JOIN interactions i ON i.id = x.interaction_id WHERE i.conversation_id = ?1 AND x.file_path IS NOT NULL AND x.file_path <> ''";

/// Maps a row of [`conversation_summary_sql`].
fn map_summary_row(row: &Row) -> rusqlite::Result<ConversationSummary> {
    let shared_count: i64 = row.get(6)?;
    let id: String = row.get(0)?;
    let imported: bool = row.get(10)?;
    let agent = row
        .get::<_, Option<String>>(13)?
        .as_deref()
        .and_then(HarnessId::from_capture_source_str)
        .or_else(|| imported.then(|| imported_agent(&id)).flatten());
    Ok(ConversationSummary {
        agent,
        parent: row.get(14)?,
        id,
        title: row.get(1)?,
        thoughts: row.get(2)?,
        linked: row.get(3)?,
        first_activity: row.get(4)?,
        last_activity: row.get(5)?,
        shared: shared_count > 0,
        shared_count,
        published: row.get(7)?,
        unconfirmed: row.get(8)?,
        share_future: row.get(9)?,
        imported,
        held: row.get(11)?,
        held_published: row.get(12)?,
    })
}

/// The agent an imported conversation's id names after `remote:<fingerprint>:`.
fn imported_agent(id: &str) -> Option<HarnessId> {
    let (_, original) = id.strip_prefix("remote:")?.split_once(':')?;
    HarnessId::from_conversation_id(original)
}

/// Maximum time a harness ingest waits for a busy shared database. Ingests are
/// not Git hooks: they may wait, but never indefinitely.
const HARNESS_WRITE_RETRY_BUDGET: Duration = Duration::from_secs(5);

// ---- capture_source: from an enumerated CHECK to triggers ---------------------
//
// cvc 0.6 and 0.7 enumerate the `capture_source` values in a column CHECK, and
// SQLite cannot alter a CHECK in place, so every new value would cost a rebuild
// of `interactions`. This version rebuilds it one last time (same columns, rows
// copied verbatim) with a CHECK on the value's shape only, and moves the list of
// values into two triggers: the next widening is a DROP TRIGGER and CREATE
// TRIGGER in one transaction.
//
// Compatibility: cvc 0.7.x rebuilds the table back to its own CHECK when its
// marker `capture-source-claude-code/v1` is missing and the table's SQL lacks
// `'claude_code'`. So this version always records that marker as well as its
// own, on new databases too, and the table's SQL names the value in a comment.
// Every value an older binary writes is in the triggers' list, so 0.6 and 0.7
// keep writing to the rebuilt table.

/// How much of the `capture_source` move an open does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureSourceUpgrade {
    /// Only what is cheap or what cvc 0.7.x would do in the same open: the
    /// rebuild of an empty table, of one that cannot hold `claude_code` yet,
    /// and the triggers of a table already rebuilt.
    IfCheap,
    /// The rebuild, however long it takes.
    Always,
}

/// The marker of the move to triggers.
const CAPTURE_SOURCE_TRIGGERS_MARKER: &str = "capture-source/v2";
/// cvc 0.7.x's marker; see above for why it is always recorded.
const CAPTURE_SOURCE_CLAUDE_CODE_MARKER: &str = "capture-source-claude-code/v1";

/// The columns of `interactions`, in their order since cvc 0.5. The rebuild
/// runs only on a table with exactly these, so no column can be lost.
const INTERACTION_COLUMNS: [&str; 14] = [
    "id",
    "conversation_id",
    "parent_id",
    "timestamp",
    "author",
    "user_prompt",
    "model_name",
    "model_cot",
    "model_response",
    "source_request_id",
    "visibility",
    "capture_source",
    "scrubber_version",
    "capture_worktree",
];

/// What `interactions` is rebuilt to.
const INTERACTIONS_TRIGGERED_SQL: &str = "CREATE TABLE interactions_cvc_rebuild (id TEXT PRIMARY KEY, conversation_id TEXT, parent_id TEXT, timestamp INTEGER, author TEXT, user_prompt TEXT, model_name TEXT, model_cot TEXT, model_response TEXT, source_request_id TEXT, visibility TEXT NOT NULL DEFAULT 'private' CHECK(visibility IN ('private','shared')), capture_source TEXT NOT NULL DEFAULT 'legacy' CHECK(capture_source <> '' AND length(capture_source) <= 32 AND capture_source NOT GLOB '*[^a-z_]*'), scrubber_version INTEGER NOT NULL DEFAULT 0 CHECK(scrubber_version BETWEEN 0 AND 1), capture_worktree TEXT CHECK(capture_worktree IS NULL OR (length(capture_worktree)=64 AND capture_worktree NOT GLOB '*[^0-9a-f]*')), FOREIGN KEY(conversation_id) REFERENCES conversations(id), FOREIGN KEY(parent_id) REFERENCES interactions(id)\n-- capture_source values are listed by the triggers interactions_capture_source_insert and interactions_capture_source_update, 'claude_code' among them\n)";

/// What the shape CHECK of the rebuilt table reads like, to recognise it.
const CAPTURE_SOURCE_SHAPE_CHECK: &str = "capture_source NOT GLOB '*[^a-z_]*'";

/// The indexes `interactions` has had since cvc 0.7.
const INTERACTION_INDEXES_SQL: &str = "CREATE INDEX IF NOT EXISTS idx_interactions_conversation_id ON interactions(conversation_id); CREATE INDEX IF NOT EXISTS idx_interactions_source_request_id ON interactions(source_request_id); CREATE INDEX IF NOT EXISTS idx_interactions_visibility ON interactions(visibility); CREATE INDEX IF NOT EXISTS idx_interactions_source_request ON interactions(source_request_id);";

/// Every value of `capture_source`, as the triggers list them: exactly
/// [`CaptureSource::ALL`] (a test pins the two against each other).
const CAPTURE_SOURCE_VALUES_SQL: &str = "'mcp','vscode_passive','vscode_explicit','cli_run','sync_import','legacy','claude_code','codex','opencode','cursor','antigravity'";

/// The names of the two triggers.
const CAPTURE_SOURCE_TRIGGERS: [&str; 2] = [
    "interactions_capture_source_insert",
    "interactions_capture_source_update",
];

fn capture_source_triggers_sql() -> String {
    format!(
        "CREATE TRIGGER interactions_capture_source_insert BEFORE INSERT ON interactions WHEN NEW.capture_source NOT IN ({CAPTURE_SOURCE_VALUES_SQL}) BEGIN SELECT RAISE(ABORT, 'unknown capture_source'); END; CREATE TRIGGER interactions_capture_source_update BEFORE UPDATE OF capture_source ON interactions WHEN NEW.capture_source NOT IN ({CAPTURE_SOURCE_VALUES_SQL}) BEGIN SELECT RAISE(ABORT, 'unknown capture_source'); END;"
    )
}

/// The harness tables of this version. Additive: older binaries never read
/// them, and no row of theirs is changed.
const HARNESS_TABLES_SQL: &str = "CREATE TABLE IF NOT EXISTS harness_cursors (harness TEXT NOT NULL CHECK(harness <> ''), cursor_key TEXT NOT NULL CHECK(cursor_key <> ''), kind TEXT NOT NULL CHECK(kind <> ''), version INTEGER NOT NULL CHECK(version >= 1), epoch INTEGER NOT NULL DEFAULT 0 CHECK(epoch >= 0), state TEXT NOT NULL CHECK(json_valid(state)), updated_at INTEGER NOT NULL, PRIMARY KEY(harness, cursor_key)); CREATE TABLE IF NOT EXISTS conversation_relations (child_id TEXT PRIMARY KEY, parent_id TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('subagent','fork')), harness TEXT NOT NULL, spawn_interaction_id TEXT, spawn_call_id TEXT, evidence TEXT NOT NULL CHECK(evidence IN ('child_record','parent_record','hook_payload','store_index')), recorded_at INTEGER NOT NULL, CHECK(child_id <> parent_id)); CREATE INDEX IF NOT EXISTS idx_conversation_relations_parent ON conversation_relations(parent_id); CREATE TABLE IF NOT EXISTS harness_commit_evidence (harness TEXT NOT NULL, family_root TEXT NOT NULL, interaction_id TEXT NOT NULL, candidate TEXT NOT NULL CHECK(length(candidate) BETWEEN 4 AND 64 AND candidate NOT GLOB '*[^0-9a-f]*'), response_at INTEGER NOT NULL, PRIMARY KEY(harness, family_root, interaction_id, candidate)); CREATE INDEX IF NOT EXISTS idx_harness_commit_evidence_interaction ON harness_commit_evidence(interaction_id);";

/// The one-time lossless copy of the 0.7.x Claude Code cursors into the typed
/// table, behind its marker. Rows already there are kept (`OR IGNORE`).
const COPY_LEGACY_CURSORS_SQL: &str = "INSERT OR IGNORE INTO harness_cursors(harness,cursor_key,kind,version,epoch,state,updated_at) SELECT harness, session_id, 'jsonl', 1, 0, json_object('path', transcript_path, 'byte_offset', byte_offset), updated_at FROM harness_ingest_cursors WHERE harness = 'claude-code'; INSERT INTO cvc_internal_migrations(name) VALUES('harness-cursors/v1');";

/// The 0.7.x cursor row, written as 0.7.x writes it.
const UPSERT_LEGACY_CURSOR_SQL: &str = "INSERT INTO harness_ingest_cursors(harness,session_id,transcript_path,byte_offset,updated_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(harness,session_id) DO UPDATE SET transcript_path=excluded.transcript_path, byte_offset=excluded.byte_offset, updated_at=excluded.updated_at";

/// The typed copy of a Claude Code cursor that [`UPSERT_LEGACY_CURSOR_SQL`]
/// wrote: the path and offset change, everything else the row holds stays.
const MIRROR_CLAUDE_CODE_CURSOR_SQL: &str = "INSERT INTO harness_cursors(harness,cursor_key,kind,version,epoch,state,updated_at) VALUES (?1,?2,'jsonl',1,0,json_object('path',?3,'byte_offset',?4),?5) ON CONFLICT(harness,cursor_key) DO UPDATE SET state=json_set(harness_cursors.state,'$.path',?3,'$.byte_offset',?4), updated_at=?5";

impl CvcStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_initialized(path)
    }

    /// Open a store and apply every idempotent schema migration that is cheap
    /// before it can be read or written. `open` delegates here for legacy
    /// callers. This is the open for hooks, the editor and MCP: it never
    /// rebuilds a populated `interactions` table that cvc 0.7.x already
    /// widened (see [`Self::open_initialized_upgrading`] and
    /// [`Self::needs_capture_source_upgrade`]).
    pub fn open_initialized<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_with(path.as_ref(), CaptureSourceUpgrade::IfCheap)
    }

    /// [`Self::open_initialized`], and also the one-time rebuild that moves
    /// the `capture_source` values into triggers, however long it takes. For
    /// commands a person runs and waits for, never for a hook: on a large
    /// store the rebuild holds the write lock for many seconds.
    pub fn open_initialized_upgrading<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_with(path.as_ref(), CaptureSourceUpgrade::Always)
    }

    fn open_with(path: &Path, upgrade: CaptureSourceUpgrade) -> Result<Self> {
        prepare_store_path(path)?;
        let conn = Connection::open(path)?;

        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        // Every writing transaction in this store begins IMMEDIATE, so this is
        // the longest any process waits for another writer before giving up.
        // Two seconds covers a sibling process's startup migrations and a
        // hook-driven batch on a slow disk while staying well under what a
        // Git hook may reasonably take; the rewrite inbox still classifies
        // and retries permanent-versus-retryable failures on its own.
        conn.busy_timeout(std::time::Duration::from_millis(2000))?;
        enforce_store_permissions(path)?;

        let store = Self {
            conn,
            path: path.to_path_buf(),
        };
        store.init_with(upgrade)?;
        enforce_store_permissions(&store.path)?;
        Ok(store)
    }

    /// Whether the one-time `capture_source` rebuild has still to run here.
    /// Until it has, captures by the harnesses after Claude Code fail closed
    /// with [`DbError::CaptureSourceUpgradeRequired`]; Claude Code and every
    /// older source keep working on the old constraint. An operator command
    /// that opens with [`Self::open_initialized_upgrading`] finishes it. It
    /// stays `true` for a table this version will not rebuild (one with
    /// columns it does not know).
    pub fn needs_capture_source_upgrade(&self) -> Result<bool> {
        let done: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM cvc_internal_migrations WHERE name=?1)",
            params![CAPTURE_SOURCE_TRIGGERS_MARKER],
            |row| row.get(0),
        )?;
        Ok(!done)
    }

    /// Atomically consumes the exact per-destination share/publication authority,
    /// creates a pending suppression record, and deletes the local projection.
    /// Repeating an already-created matching tombstone is intentionally harmless.
    pub fn authorize_and_tombstone_remote(
        &self,
        id: &InteractionId,
        remote: &str,
        reason: TombstoneReasonCode,
    ) -> Result<Tombstone> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let reason_text = match reason {
            TombstoneReasonCode::UserRequested => "user_requested",
            TombstoneReasonCode::Security => "security",
            TombstoneReasonCode::Retention => "retention",
        };
        if let Some((deleted_at, old_reason, oid)) = tx.query_row(
            "SELECT deleted_at,reason_code,previous_node_oid FROM tombstones WHERE interaction_id=?1 AND scope_kind='remote' AND remote_fingerprint=?2",
            params![id.as_str(), remote], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
        ).optional()? {
            if old_reason != reason_text { return Err(DbError::Migration("remote tombstone reason conflict".into())); }
            let tombstone = Tombstone { format: "cvc.tombstone/v1".into(), version: 1, interaction_id: id.clone(), deleted_at: Utc.timestamp_opt(deleted_at, 0).single().ok_or(DbError::Timestamp(deleted_at))?, reason_code: reason, previous_node_oid: oid };
            tx.commit()?;
            return Ok(tombstone);
        }
        let authorized: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM interactions i WHERE i.id=?1 AND (EXISTS (SELECT 1 FROM interaction_shares s WHERE s.interaction_id=i.id AND s.remote_fingerprint=?2) OR EXISTS (SELECT 1 FROM publications p WHERE p.interaction_id=i.id AND p.remote_fingerprint=?2 AND p.state IN ('pending','published','unknown'))))",
            params![id.as_str(), remote], |r| r.get(0))?;
        if !authorized {
            return Err(DbError::Migration(
                "remote redaction requires exact destination share or publication authority".into(),
            ));
        }
        // Node OIDs are Git projection state, not SQLite state.  Preserve it when a
        // future schema records it; currently this authoritative snapshot is None.
        let tombstone = Tombstone::new(id.clone(), reason, None);
        Self::apply_tombstones_tx(
            &tx,
            std::slice::from_ref(&tombstone),
            "remote-redact",
            Some(remote),
        )?;
        tx.commit()?;
        self.compact_after_deletion()?;
        Ok(tombstone)
    }

    /// Best-effort physical cleanup after logical deletion. It never promises
    /// erasure outside this database (snapshots/backups/SSDs are out of scope).
    pub fn compact_after_deletion(&self) -> Result<CleanupReport> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        let (busy, log_frames, checkpointed_frames) =
            self.conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
        enforce_store_permissions(&self.path)?;
        let wal_path = self.path.with_file_name(format!(
            "{}-wal",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("index")
        ));
        let wal_bytes = std::fs::metadata(wal_path)
            .map(|metadata| metadata.len())
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(0)
                } else {
                    Err(error)
                }
            })?;
        Ok(CleanupReport {
            busy,
            log_frames,
            checkpointed_frames,
            wal_bytes,
        })
    }

    /// Applies the migrations [`Self::open_initialized`] applies.
    pub fn init(&self) -> Result<()> {
        self.init_with(CaptureSourceUpgrade::IfCheap)
    }

    fn init_with(&self, upgrade: CaptureSourceUpgrade) -> Result<()> {
        self.conn
            .execute_batch(include_str!("../migrations/0001_initial_schema.sql"))?;

        // Run additive migrations as one convergence transaction; a crash or a
        // competing opener cannot leave half of the metadata columns behind.
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let convergence: Result<()> = (|| {
            // Run additive migrations idempotently.
            // ALTER TABLE will fail if column already exists, so we check first.
            let has_source_request_id: bool = self
                .conn
                .prepare("PRAGMA table_info(interactions)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .any(|name| name.as_deref() == Ok("source_request_id"));

            if !has_source_request_id {
                self.conn
                    .execute_batch(include_str!("../migrations/0002_add_source_request_id.sql"))?;
            }
            // Metadata convergence is additive so legacy databases retain every row.  We
            // deliberately classify old rows as legacy/private rather than guessing that
            // a row was imported.
            let columns: Vec<String> = self
                .conn
                .prepare("PRAGMA table_info(interactions)")?
                .query_map([], |row| row.get(1))?
                .collect::<std::result::Result<_, _>>()?;
            if !columns.iter().any(|c| c == "visibility") {
                self.conn.execute_batch("ALTER TABLE interactions ADD COLUMN visibility TEXT NOT NULL DEFAULT 'private' CHECK(visibility IN ('private','shared')); ")?;
            }
            if !columns.iter().any(|c| c == "capture_source") {
                self.conn.execute_batch("ALTER TABLE interactions ADD COLUMN capture_source TEXT NOT NULL DEFAULT 'legacy' CHECK(capture_source IN ('mcp','vscode_passive','vscode_explicit','cli_run','sync_import','legacy','claude_code')); ")?;
            }
            if !columns.iter().any(|c| c == "scrubber_version") {
                self.conn.execute_batch("ALTER TABLE interactions ADD COLUMN scrubber_version INTEGER NOT NULL DEFAULT 0 CHECK(scrubber_version BETWEEN 0 AND 1); ")?;
            }
            // Local-only worktree-origin fingerprint scoping automatic link
            // eligibility. NULL marks legacy and imported rows, which stay
            // eligible from any worktree. Never project this column.
            if !columns.iter().any(|c| c == "capture_worktree") {
                self.conn.execute_batch("ALTER TABLE interactions ADD COLUMN capture_worktree TEXT CHECK(capture_worktree IS NULL OR (length(capture_worktree)=64 AND capture_worktree NOT GLOB '*[^0-9a-f]*')); ")?;
            }
            self.conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_interactions_visibility ON interactions(visibility); CREATE INDEX IF NOT EXISTS idx_interactions_source_request ON interactions(source_request_id);")?;
            self.conn.execute_batch("CREATE TABLE IF NOT EXISTS conversation_share_policy (conversation_id TEXT PRIMARY KEY, future_shared INTEGER NOT NULL CHECK(future_shared IN (0,1))); CREATE TABLE IF NOT EXISTS conversation_shares (conversation_id TEXT NOT NULL, remote_fingerprint TEXT NOT NULL, share_future INTEGER NOT NULL CHECK(share_future IN (0,1)), PRIMARY KEY(conversation_id,remote_fingerprint)); CREATE TABLE IF NOT EXISTS interaction_shares (interaction_id TEXT NOT NULL, remote_fingerprint TEXT NOT NULL, PRIMARY KEY(interaction_id, remote_fingerprint)); CREATE INDEX IF NOT EXISTS idx_interaction_shares_remote ON interaction_shares(remote_fingerprint); CREATE TABLE IF NOT EXISTS publications (interaction_id TEXT NOT NULL, remote_fingerprint TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','published','unknown')), updated_at INTEGER NOT NULL, PRIMARY KEY(interaction_id, remote_fingerprint)); CREATE INDEX IF NOT EXISTS idx_publications_remote_state ON publications(remote_fingerprint,state);")?;
            // Agent-harness ingest cursors: one per harness and session.
            self.conn.execute_batch("CREATE TABLE IF NOT EXISTS harness_ingest_cursors (harness TEXT NOT NULL, session_id TEXT NOT NULL, transcript_path TEXT NOT NULL, byte_offset INTEGER NOT NULL CHECK(byte_offset >= 0), updated_at INTEGER NOT NULL, PRIMARY KEY(harness, session_id));")?;
            // v4.1: tombstone authority is destination scoped.  A received
            // suppression from A must never authorize projection to B.
            let tombstone_columns: Vec<String> = self
                .conn
                .prepare("PRAGMA table_info(tombstones)")?
                .query_map([], |r| r.get(1))?
                .collect::<std::result::Result<_, _>>()?;
            if tombstone_columns.is_empty() {
                self.conn.execute_batch("CREATE TABLE tombstones (interaction_id TEXT NOT NULL, scope_kind TEXT NOT NULL CHECK(scope_kind IN ('local','remote')), remote_fingerprint TEXT, format TEXT NOT NULL, version INTEGER NOT NULL, deleted_at INTEGER NOT NULL, reason_code TEXT NOT NULL CHECK(reason_code IN ('user_requested','security','retention')), previous_node_oid TEXT, source TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'published' CHECK(state IN ('pending','published')), CHECK(((scope_kind='local' AND remote_fingerprint IS NULL) OR (scope_kind='remote' AND remote_fingerprint IS NOT NULL)) AND format='cvc.tombstone/v1' AND version=1), PRIMARY KEY(interaction_id,scope_kind,remote_fingerprint));")?;
            } else if !tombstone_columns.iter().any(|c| c == "scope_kind") {
                self.conn.execute_batch("ALTER TABLE tombstones RENAME TO tombstones_legacy; CREATE TABLE tombstones (interaction_id TEXT NOT NULL, scope_kind TEXT NOT NULL CHECK(scope_kind IN ('local','remote')), remote_fingerprint TEXT, format TEXT NOT NULL, version INTEGER NOT NULL, deleted_at INTEGER NOT NULL, reason_code TEXT NOT NULL CHECK(reason_code IN ('user_requested','security','retention')), previous_node_oid TEXT, source TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'published' CHECK(state IN ('pending','published')), CHECK(((scope_kind='local' AND remote_fingerprint IS NULL) OR (scope_kind='remote' AND remote_fingerprint IS NOT NULL)) AND format='cvc.tombstone/v1' AND version=1), PRIMARY KEY(interaction_id,scope_kind,remote_fingerprint)); INSERT INTO tombstones(interaction_id,scope_kind,remote_fingerprint,format,version,deleted_at,reason_code,previous_node_oid,source,state) SELECT interaction_id,CASE WHEN remote_fingerprint IS NULL THEN 'local' ELSE 'remote' END,remote_fingerprint,format,version,deleted_at,reason_code,previous_node_oid,source,'published' FROM tombstones_legacy; DROP TABLE tombstones_legacy;")?;
            }
            let has_tombstone_state = self
                .conn
                .prepare("PRAGMA table_info(tombstones)")?
                .query_map([], |r| r.get::<_, String>(1))?
                .any(|x| x.as_deref() == Ok("state"));
            if !has_tombstone_state {
                self.conn.execute_batch("ALTER TABLE tombstones ADD COLUMN state TEXT NOT NULL DEFAULT 'published' CHECK(state IN ('pending','published'));")?;
            }
            self.conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_tombstones_remote ON tombstones(remote_fingerprint); CREATE UNIQUE INDEX IF NOT EXISTS idx_tombstones_scope_identity ON tombstones(interaction_id,scope_kind,COALESCE(remote_fingerprint,''));")?;
            // v5 derivations are immutable and intentionally not endpoint keyed:
            // several independently observed explanations can coexist.
            self.conn.execute_batch("CREATE TABLE IF NOT EXISTS derivation_events (event_id TEXT PRIMARY KEY, interaction_id TEXT NOT NULL, target_commit TEXT NOT NULL, relation TEXT NOT NULL CHECK(relation IN ('generated','temporal','verified','rewrite_exact','squash_exact')), evidence_version INTEGER NOT NULL, evidence_kind TEXT NOT NULL CHECK(evidence_kind IN ('locally_observed','imported_legacy','remote_assertion')), origin TEXT NOT NULL CHECK(origin IN ('local_hook','local_linker','remote_assertion','legacy_import')), source_event_ids TEXT NOT NULL, old_oid TEXT, new_oid TEXT, range_id TEXT, linked_by TEXT, payload TEXT NOT NULL); CREATE INDEX IF NOT EXISTS idx_derivation_endpoint ON derivation_events(interaction_id,target_commit); CREATE TABLE IF NOT EXISTS derivation_authorizations (event_id TEXT NOT NULL, remote_fingerprint TEXT NOT NULL, source_event_id TEXT NOT NULL, PRIMARY KEY(event_id,remote_fingerprint,source_event_id)); CREATE TABLE IF NOT EXISTS rewrite_batches (batch_id TEXT PRIMARY KEY, mode TEXT NOT NULL, payload_hash TEXT NOT NULL); ")?;
            let authorization_sql: String = self.conn.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='derivation_authorizations'", [], |row| row.get(0))?;
            let compact_authorization_sql: String = authorization_sql
                .chars()
                .filter(|character| !character.is_ascii_whitespace())
                .collect::<String>()
                .to_ascii_lowercase();
            if !compact_authorization_sql
                .contains("primarykey(event_id,remote_fingerprint,source_event_id)")
            {
                self.conn.execute_batch("ALTER TABLE derivation_authorizations RENAME TO derivation_authorizations_legacy; CREATE TABLE derivation_authorizations (event_id TEXT NOT NULL, remote_fingerprint TEXT NOT NULL, source_event_id TEXT NOT NULL, PRIMARY KEY(event_id,remote_fingerprint,source_event_id)); INSERT OR IGNORE INTO derivation_authorizations SELECT event_id,remote_fingerprint,source_event_id FROM derivation_authorizations_legacy; DROP TABLE derivation_authorizations_legacy;")?;
            }
            // Trust is an observation, not a property of immutable remote event
            // bytes. `source_key` makes NULL fingerprints addressable without
            // allowing SQLite's NULL uniqueness semantics to create duplicates.
            self.conn.execute_batch("CREATE TABLE IF NOT EXISTS derivation_observations (event_id TEXT NOT NULL, source_fingerprint TEXT, source_key TEXT NOT NULL, origin TEXT NOT NULL CHECK(origin IN ('local_hook','local_api','remote_sync')), trusted_local INTEGER NOT NULL CHECK(trusted_local IN (0,1)) CHECK(trusted_local=0 OR origin IN ('local_hook','local_api')), PRIMARY KEY(event_id,source_key)); CREATE INDEX IF NOT EXISTS idx_derivation_observations_trust ON derivation_observations(event_id,trusted_local); ")?;
            self.conn.execute_batch("CREATE TABLE IF NOT EXISTS range_evidence (range_id TEXT PRIMARY KEY, payload TEXT NOT NULL, payload_digest TEXT NOT NULL, changeset_algorithm TEXT NOT NULL, changeset_digest TEXT NOT NULL); CREATE INDEX IF NOT EXISTS idx_range_changeset ON range_evidence(changeset_algorithm,changeset_digest); CREATE TABLE IF NOT EXISTS range_observations (range_id TEXT NOT NULL, source_key TEXT NOT NULL, origin TEXT NOT NULL CHECK(origin IN ('explicit','pre_push','remote_sync')), trusted_local INTEGER NOT NULL CHECK(trusted_local IN (0,1)) CHECK(trusted_local=0 OR origin IN ('explicit','pre_push')), source_ref TEXT, source_remote TEXT, PRIMARY KEY(range_id,source_key)); CREATE INDEX IF NOT EXISTS idx_range_trust ON range_observations(range_id,trusted_local); CREATE TABLE IF NOT EXISTS range_authorizations (range_id TEXT NOT NULL,remote_fingerprint TEXT NOT NULL,PRIMARY KEY(range_id,remote_fingerprint)); CREATE TABLE IF NOT EXISTS range_source_snapshots (range_id TEXT NOT NULL,source_id TEXT NOT NULL,snapshot TEXT NOT NULL,PRIMARY KEY(range_id,source_id)); CREATE TABLE IF NOT EXISTS branch_scan_cursors (worktree_key TEXT NOT NULL,symbolic_ref TEXT NOT NULL,last_tip TEXT NOT NULL,PRIMARY KEY(worktree_key,symbolic_ref));")?;
            self.conn.execute_batch("CREATE TABLE IF NOT EXISTS range_interaction_sources (range_id TEXT NOT NULL,interaction_id TEXT NOT NULL,source_id TEXT NOT NULL,snapshot TEXT NOT NULL,PRIMARY KEY(range_id,interaction_id,source_id)); CREATE INDEX IF NOT EXISTS idx_range_interaction_sources ON range_interaction_sources(range_id,interaction_id); CREATE TABLE IF NOT EXISTS pending_squash_targets (worktree_key TEXT NOT NULL,symbolic_ref TEXT NOT NULL,target_commit TEXT NOT NULL,parent_commit TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('pending','matched','unsupported')),discovered_order INTEGER NOT NULL,last_attempt_seq INTEGER NOT NULL DEFAULT 0,attempt_count INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(worktree_key,symbolic_ref,target_commit)); CREATE INDEX IF NOT EXISTS idx_pending_squash ON pending_squash_targets(worktree_key,symbolic_ref,status,last_attempt_seq,discovered_order);")?;
            let pending_columns: Vec<String> = self
                .conn
                .prepare("PRAGMA table_info(pending_squash_targets)")?
                .query_map([], |r| r.get(1))?
                .collect::<std::result::Result<_, _>>()?;
            if !pending_columns
                .iter()
                .any(|column| column == "last_attempt_seq")
            {
                self.conn.execute_batch("ALTER TABLE pending_squash_targets ADD COLUMN last_attempt_seq INTEGER NOT NULL DEFAULT 0;")?;
            }
            if !pending_columns
                .iter()
                .any(|column| column == "attempt_count")
            {
                self.conn.execute_batch("ALTER TABLE pending_squash_targets ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0;")?;
            }
            // Migrate old endpoint-based local proof exactly once. Re-running
            // this on every open would let a later public/untrusted link become
            // trusted merely by reopening the store.
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS cvc_internal_migrations(name TEXT PRIMARY KEY);",
            )?;
            let migrated: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM cvc_internal_migrations WHERE name='legacy-link-observations/v1')", [], |row| row.get(0))?;
            if !migrated {
                self.conn.execute_batch("INSERT OR IGNORE INTO derivation_observations(event_id,source_fingerprint,source_key,origin,trusted_local) SELECT 'legacy:' || interaction_id || ':' || git_commit_hash,NULL,'legacy:' || interaction_id || ':' || git_commit_hash,'local_api',1 FROM artifact_links a JOIN interactions i ON i.id=a.interaction_id WHERE i.capture_source!='sync_import'; INSERT INTO cvc_internal_migrations(name) VALUES('legacy-link-observations/v1');")?;
            }
            // Typed harness cursors, the subagent relation and durable commit
            // evidence. The 0.7.x cursors are copied once, losslessly; the old
            // table stays and Claude Code ingests keep writing it.
            self.conn.execute_batch(HARNESS_TABLES_SQL)?;
            let cursors_copied: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM cvc_internal_migrations WHERE name='harness-cursors/v1')", [], |row| row.get(0))?;
            if !cursors_copied {
                self.conn.execute_batch(COPY_LEGACY_CURSORS_SQL)?;
            }
            Ok(())
        })();
        match convergence {
            Ok(()) => self.conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }

        self.converge_capture_source(upgrade)?;
        self.normalize_artifact_links_schema()?;

        Ok(())
    }

    /// Moves the `capture_source` value list from the column CHECK into
    /// triggers, once (see [`CAPTURE_SOURCE_TRIGGERS_MARKER`]), when `upgrade`
    /// allows it. Everything is decided and done inside one immediate
    /// transaction, so a process that waited for another one's rebuild finds
    /// it done and does nothing.
    fn converge_capture_source(&self, upgrade: CaptureSourceUpgrade) -> Result<()> {
        // Foreign-key enforcement must be off around a table rebuild (setting
        // it is a no-op inside a transaction), and it is restored whatever
        // happens.
        self.conn.pragma_update(None, "foreign_keys", "OFF")?;
        let converged = (|| -> Result<()> {
            let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            Self::converge_capture_source_tx(&tx, upgrade)?;
            tx.commit()?;
            Ok(())
        })();
        let restored = self.conn.pragma_update(None, "foreign_keys", "ON");
        converged?;
        restored?;
        Ok(())
    }

    fn converge_capture_source_tx(
        tx: &Transaction<'_>,
        upgrade: CaptureSourceUpgrade,
    ) -> Result<()> {
        let marked = |name: &str| -> Result<bool> {
            Ok(tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cvc_internal_migrations WHERE name=?1)",
                params![name],
                |row| row.get(0),
            )?)
        };
        let mark = |name: &str| -> Result<()> {
            tx.execute(
                "INSERT OR IGNORE INTO cvc_internal_migrations(name) VALUES(?1)",
                params![name],
            )?;
            Ok(())
        };
        if marked(CAPTURE_SOURCE_TRIGGERS_MARKER)? {
            return mark(CAPTURE_SOURCE_CLAUDE_CODE_MARKER);
        }
        let columns: Vec<String> = tx
            .prepare("PRAGMA table_info(interactions)")?
            .query_map([], |row| row.get(1))?
            .collect::<std::result::Result<_, _>>()?;
        if columns != INTERACTION_COLUMNS {
            // A table this version does not know column for column is left as
            // it is: its own CHECK keeps refusing the values it does not list,
            // so captures of new sources fail closed, and nothing is lost.
            return Ok(());
        }
        let table_sql: String = tx.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='interactions'",
            [],
            |row| row.get(0),
        )?;
        let converted = table_sql.contains(CAPTURE_SOURCE_SHAPE_CHECK);
        if upgrade == CaptureSourceUpgrade::IfCheap && !converted {
            // The default open rebuilds only what costs nothing (an empty
            // table) or what cvc 0.7.x would rebuild in the same open (a table
            // that cannot hold `claude_code` yet, so that Claude Code capture
            // keeps working there). A populated 0.7.x table waits for an
            // operator command.
            let empty: bool =
                tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM interactions)", [], |row| {
                    row.get(0)
                })?;
            if !empty && table_sql.contains("'claude_code'") {
                // What cvc 0.7.x records for such a table.
                return mark(CAPTURE_SOURCE_CLAUDE_CODE_MARKER);
            }
        }
        if !converted {
            // Every index and trigger the table has is created again on the
            // copy (our own triggers are created below).
            let dependents: Vec<String> = tx
                .prepare("SELECT sql FROM sqlite_master WHERE tbl_name='interactions' AND type IN ('index','trigger') AND sql IS NOT NULL AND name NOT IN (?1,?2) ORDER BY rowid")?
                .query_map(
                    params![CAPTURE_SOURCE_TRIGGERS[0], CAPTURE_SOURCE_TRIGGERS[1]],
                    |row| row.get(0),
                )?
                .collect::<std::result::Result<_, _>>()?;
            let columns = INTERACTION_COLUMNS.join(",");
            tx.execute_batch(INTERACTIONS_TRIGGERED_SQL)?;
            let copied = tx.execute(
                &format!("INSERT INTO interactions_cvc_rebuild ({columns}) SELECT {columns} FROM interactions"),
                [],
            )?;
            let rows: i64 = tx.query_row("SELECT COUNT(*) FROM interactions", [], |r| r.get(0))?;
            if i64::try_from(copied).ok() != Some(rows) {
                return Err(DbError::Migration(
                    "capture source rebuild did not copy every interaction".into(),
                ));
            }
            tx.execute_batch(
                "DROP TABLE interactions; ALTER TABLE interactions_cvc_rebuild RENAME TO interactions;",
            )?;
            for sql in dependents {
                tx.execute_batch(&sql)?;
            }
            tx.execute_batch(INTERACTION_INDEXES_SQL)?;
        }
        for name in CAPTURE_SOURCE_TRIGGERS {
            tx.execute_batch(&format!("DROP TRIGGER IF EXISTS {name};"))?;
        }
        tx.execute_batch(&capture_source_triggers_sql())?;
        mark(CAPTURE_SOURCE_TRIGGERS_MARKER)?;
        mark(CAPTURE_SOURCE_CLAUDE_CODE_MARKER)
    }

    fn normalize_artifact_links_schema(&self) -> Result<()> {
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let columns: Vec<(String, String, bool, Option<String>)> = transaction
            .prepare("PRAGMA table_info(artifact_links)")?
            .query_map([], |row| {
                Ok((
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get(4)?,
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let linked_by_present = columns.iter().any(|(name, _, _, _)| name == "linked_by");
        let link_type_converged = columns.iter().any(|(name, ty, not_null, default)| {
            name == "link_type"
                && ty.eq_ignore_ascii_case("TEXT")
                && *not_null
                && default
                    .as_deref()
                    .is_some_and(|value| value.contains("generated"))
        });
        if !linked_by_present || !link_type_converged {
            transaction.execute_batch(
                "CREATE TABLE artifact_links_normalized (
                    interaction_id TEXT,
                    git_commit_hash TEXT,
                    link_type TEXT NOT NULL DEFAULT 'generated',
                    linked_by TEXT,
                    PRIMARY KEY (interaction_id, git_commit_hash)
                );",
            )?;
            let linked_by = if linked_by_present {
                "linked_by"
            } else {
                "NULL"
            };
            transaction.execute_batch(&format!(
                "INSERT INTO artifact_links_normalized (interaction_id, git_commit_hash, link_type, linked_by)
                 SELECT interaction_id, git_commit_hash, COALESCE(link_type, 'generated'), {linked_by}
                 FROM artifact_links;
                 DROP TABLE artifact_links;
                 ALTER TABLE artifact_links_normalized RENAME TO artifact_links;"
            ))?;
        }
        transaction.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_artifact_links_commit_hash
             ON artifact_links(git_commit_hash);",
        )?;
        transaction.commit()?;
        Ok(())
    }

    // --- Conversations ---

    pub fn get_conversation(&self, id: &str) -> Result<Option<Conversation>> {
        self.conn
            .query_row(
                "SELECT id, title, created_at FROM conversations WHERE id = ?1",
                params![id],
                |row| {
                    let timestamp: i64 = row.get(2)?;
                    let dt = Utc.timestamp_opt(timestamp, 0).single().ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Integer,
                            Box::new(DbError::Timestamp(timestamp)),
                        )
                    })?;
                    Ok(Conversation {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        created_at: dt,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
    pub fn conversation_interaction_count(&self, conversation_id: &str) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM interactions WHERE conversation_id=?1",
            params![conversation_id],
            |r| r.get::<_, i64>(0),
        )? as usize)
    }
    /// How many of the conversation's interactions were recorded through MCP
    /// (`capture_source = 'mcp'`). Future sharing never extends to a thought
    /// recorded through MCP, so a front that records future sharing for the
    /// conversation can say that later ones will stay private.
    pub fn conversation_mcp_capture_count(&self, conversation_id: &str) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM interactions WHERE conversation_id=?1 AND capture_source='mcp'",
            params![conversation_id],
            |r| r.get::<_, i64>(0),
        )? as usize)
    }

    // --- Interactions ---

    /// Import a completely preflighted remote batch.  Preparing happens before
    /// opening the transaction so a hostile late payload cannot leave even a
    /// sanitized prefix in the WAL.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn import_sync_batch(
        &self,
        captures: Vec<Capture>,
        links: Vec<(InteractionId, CommitSha, String, Option<String>)>,
        events: Vec<DerivationEvent>,
        ranges: Vec<RangeEvidence>,
        remote_fingerprint: Option<&str>,
        tombstones: &[Tombstone],
        published_ids: &[InteractionId],
    ) -> Result<()> {
        let mut captures: Vec<Capture> = captures
            .into_iter()
            .map(privacy::prepare)
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| DbError::Migration(e.to_string()))?;
        if let Some(remote) = remote_fingerprint {
            // The remote payload remains immutable in Git; local conversation
            // grouping is namespaced so a hostile/accidental identical session
            // id cannot acquire local share policy.
            for capture in &mut captures {
                capture.interaction.conversation_id =
                    format!("remote:{remote}:{}", capture.interaction.conversation_id);
                capture.conversation.id = capture.interaction.conversation_id.clone();
            }
        }
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        // The complete, already validated pull is one commit: suppression,
        // detachment/deletion, sanitized imports, links and publication proof.
        Self::apply_tombstones_tx(&tx, tombstones, "sync", remote_fingerprint)?;
        Self::import_ranges_tx(&tx, &ranges, remote_fingerprint)?;
        for capture in &captures {
            if Self::is_tombstoned_for_source_tx(&tx, &capture.interaction.id, remote_fingerprint)?
            {
                continue;
            }
            let id = Self::insert_capture(&tx, capture)?;
            // Publication observed on one remote is not local sharing intent.  Keep
            // imported turns private until the user explicitly shares their
            // conversation; the pull separately records the
            // remote-specific publication proof.
            tx.execute("UPDATE interactions SET visibility='private', capture_source='sync_import', scrubber_version=1 WHERE id=?1", params![id.to_string()])?;
        }
        for (id, sha, kind, by) in &links {
            if Self::is_tombstoned_for_source_tx(&tx, id, remote_fingerprint)? {
                continue;
            }
            if !is_safe_commit_sha(sha.as_str())
                || !matches!(kind.as_str(), "generated" | "temporal" | "verified")
            {
                return Err(DbError::InvalidLink("invalid imported link".into()));
            }
            let by = by
                .as_deref()
                .map(privacy::scrub)
                .transpose()
                .map_err(|e| DbError::InvalidLink(e.to_string()))?;
            match tx.execute(
                "INSERT INTO artifact_links (interaction_id, git_commit_hash, link_type, linked_by) VALUES (?1, ?2, ?3, ?4)",
                params![id.to_string(), sha.as_str(), kind, by],
            ) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(error, _)) if is_unique_constraint(&error) => {
                    let existing: (String, Option<String>) = tx.query_row(
                        "SELECT link_type, linked_by FROM artifact_links WHERE interaction_id=?1 AND git_commit_hash=?2",
                        params![id.to_string(), sha.as_str()], |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    if existing.0 != *kind
                        || matches!((existing.1.as_deref(), by.as_deref()), (Some(left), Some(right)) if left != right)
                    {
                        return Err(DbError::LinkConflict("imported link differs from existing provenance".into()));
                    }
                    if existing.1.is_none() && by.is_some() {
                        tx.execute(
                            "UPDATE artifact_links SET linked_by=?3 WHERE interaction_id=?1 AND git_commit_hash=?2 AND linked_by IS NULL",
                            params![id.to_string(), sha.as_str(), by],
                        )?;
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        for event in events {
            if Self::is_tombstoned_for_source_tx(&tx, &event.interaction_id, remote_fingerprint)? {
                continue;
            }
            if !event.verify_id()
                || !event.sources_are_canonical()
                || !is_safe_commit_sha(event.target_commit.as_str())
            {
                return Err(DbError::InvalidLink(
                    "invalid imported derivation event".into(),
                ));
            }
            let payload =
                serde_json::to_string(&event).map_err(|e| DbError::Migration(e.to_string()))?;
            match tx.execute("INSERT INTO derivation_events(event_id,interaction_id,target_commit,relation,evidence_version,evidence_kind,origin,source_event_ids,old_oid,new_oid,range_id,linked_by,payload) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",params![event.event_id,event.interaction_id.to_string(),event.target_commit.as_str(),relation_name(event.relation),event.evidence.version,evidence_name(event.evidence.kind),origin_name(event.origin),serde_json::to_string(&event.source_event_ids).map_err(|e|DbError::Migration(e.to_string()))?,event.old_oid.as_ref().map(CommitSha::as_str),event.new_oid.as_ref().map(CommitSha::as_str),event.range_id,event.linked_by,payload]) { Ok(_)=>{},Err(rusqlite::Error::SqliteFailure(e,_)) if is_unique_constraint(&e)=>{let old:String=tx.query_row("SELECT payload FROM derivation_events WHERE event_id=?1",params![event.event_id],|r|r.get(0))?;if old!=payload{return Err(DbError::LinkConflict("imported derivation conflict".into()));}},Err(e)=>return Err(e.into()) }
            if let Some(remote) = remote_fingerprint {
                tx.execute("INSERT OR IGNORE INTO derivation_authorizations(event_id,remote_fingerprint,source_event_id) VALUES(?1,?2,?1)",params![event.event_id,remote])?;
            }
            let key = remote_fingerprint.unwrap_or("");
            tx.execute("INSERT OR IGNORE INTO derivation_observations(event_id,source_fingerprint,source_key,origin,trusted_local) VALUES(?1,?2,?3,'remote_sync',0)", params![event.event_id,remote_fingerprint,key])?;
        }
        if let Some(remote) = remote_fingerprint {
            for id in published_ids {
                // `updated_at` records when a row became published; observing a row that
                // is already published at a later fetch must not re-stamp it.
                tx.execute("INSERT INTO publications(interaction_id,remote_fingerprint,state,updated_at) VALUES(?1,?2,'published',?3) ON CONFLICT(interaction_id,remote_fingerprint) DO UPDATE SET state='published',updated_at=CASE WHEN publications.state='published' THEN publications.updated_at ELSE excluded.updated_at END", params![id.to_string(), remote, Utc::now().timestamp()])?;
            }
        }
        tx.commit()?;
        // Never vacuum inside the import transaction: it would both defeat
        // atomicity and turn a large remote tombstone batch into N rewrites.
        if !tombstones.is_empty() {
            self.compact_after_deletion()?;
        }
        Ok(())
    }

    /// The only production capture write boundary. Policy/scrubbing completes before
    /// the transaction starts; conversations, nodes and children commit together.
    ///
    /// The transaction begins `IMMEDIATE` on purpose: a deferred transaction
    /// that reads (the tombstone check) before writing cannot use the busy
    /// timeout when it upgrades to a write lock (SQLite refuses to wait there
    /// to avoid deadlock), so a concurrent writer would make the capture fail
    /// instantly instead of waiting its 250 ms.
    fn capture(&self, capture: Capture) -> Result<InteractionId> {
        let capture = privacy::prepare(capture).map_err(|e| DbError::Migration(e.to_string()))?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if Self::is_tombstoned_tx(&tx, &capture.interaction.id)? {
            return Err(DbError::Migration("interaction is tombstoned".into()));
        }
        let id = Self::insert_capture(&tx, &capture)?;
        tx.commit()?;
        self.ensure_wal_visible()?;
        Ok(id)
    }

    /// A WAL-mode connection's committed writes reach other processes only
    /// through the `-wal` file it appends to. If that file was unlinked under
    /// this connection (a foreign close that wrongly believed it was the last
    /// connection, or an operator's `rm`), the write that just succeeded lives
    /// in a file nobody else can open, so it is reported as a failure instead
    /// of a success.
    fn ensure_wal_visible(&self) -> Result<()> {
        if self.path == Path::new(":memory:") {
            return Ok(());
        }
        let wal = PathBuf::from(format!("{}-wal", self.path.display()));
        match std::fs::symlink_metadata(&wal) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(DbError::Migration(
                "the SQLite WAL file was removed under this connection; its writes are not visible to other processes, restart this process".into(),
            )),
            Err(error) => Err(error.into()),
        }
    }

    /// Records a thought received through the MCP server. It is stored private
    /// and never joins its conversation's future sharing, whichever conversation
    /// the client named; only an explicit share of that conversation covers it.
    pub fn capture_mcp(&self, capture: crate::privacy::McpCapture) -> Result<InteractionId> {
        self.capture(capture.into_capture())
    }
    pub fn capture_cli_run(&self, capture: crate::privacy::CliRunCapture) -> Result<InteractionId> {
        self.capture(capture.into_capture())
    }
    pub fn capture_lsp_explicit(
        &self,
        capture: crate::privacy::LspExplicitCapture,
    ) -> Result<InteractionId> {
        self.capture(capture.into_capture())
    }
    pub fn capture_lsp_passive(
        &self,
        capture: crate::privacy::LspPassiveCapture,
    ) -> Result<InteractionId> {
        self.capture(capture.into_capture())
    }

    /// Passive watcher replacement: every segment is prepared before touching
    /// SQLite, then deletion and all replacements share one transaction.
    pub fn replace_lsp_passive_capture_batch(
        &self,
        source_request_id: &str,
        captures: Vec<crate::privacy::LspPassiveCapture>,
    ) -> Result<()> {
        if captures.is_empty() {
            return Ok(());
        }
        let captures: Vec<Capture> = captures
            .into_iter()
            .map(|capture| capture.into_capture())
            .collect();
        if captures.iter().any(|capture| {
            capture.interaction.source_request_id.as_deref() != Some(source_request_id)
        }) {
            return Err(DbError::Migration(
                "passive capture source request id mismatch".into(),
            ));
        }
        let prepared: Vec<Capture> = captures
            .into_iter()
            .map(privacy::prepare)
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| DbError::Migration(e.to_string()))?;
        // Immediate for the same reason as `capture`: this transaction reads
        // before it writes.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let ids: Vec<String> = {
            let mut stmt = tx.prepare("SELECT id FROM interactions WHERE source_request_id=?1")?;
            let ids = stmt
                .query_map(params![source_request_id], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            ids
        };
        for id in &ids {
            tx.execute(
                "DELETE FROM context_items WHERE interaction_id=?1",
                params![id],
            )?;
            tx.execute(
                "DELETE FROM tool_executions WHERE interaction_id=?1",
                params![id],
            )?;
            tx.execute(
                "DELETE FROM artifact_links WHERE interaction_id=?1",
                params![id],
            )?;
        }
        tx.execute(
            "DELETE FROM interactions WHERE source_request_id=?1",
            params![source_request_id],
        )?;
        for capture in &prepared {
            Self::insert_capture(&tx, capture)?;
        }
        tx.commit()?;
        if !ids.is_empty() {
            self.compact_after_deletion()?;
        }
        self.ensure_wal_visible()?;
        Ok(())
    }

    pub fn interaction_exists(&self, id: &InteractionId) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM interactions WHERE id=?1",
                params![id.as_str()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn harness_ingest_cursor(
        &self,
        harness: &str,
        session_id: &str,
    ) -> Result<Option<HarnessIngestCursor>> {
        self.conn
            .query_row(
                "SELECT transcript_path, byte_offset, updated_at FROM harness_ingest_cursors WHERE harness=?1 AND session_id=?2",
                params![harness, session_id],
                |row| {
                    Ok(HarnessIngestCursor {
                        transcript_path: row.get(0)?,
                        byte_offset: row.get::<_, i64>(1)?.max(0) as u64,
                        updated_at: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Claude Code transcript ingest: every capture is prepared before SQLite
    /// is touched (one that cannot be is stored as a stand-in, see
    /// [`HarnessBatchOutcome::withheld`]), then the new captures and the advanced
    /// cursor commit in one immediate transaction. Ids are deterministic per
    /// response, so a capture that already exists or is locally tombstoned is
    /// skipped, never replaced: replacing would drop the commit links an earlier
    /// ingest has earned.
    ///
    /// The cursor is written to the 0.7.x table exactly as 0.7.x writes it, and
    /// mirrored into the typed `harness_cursors` row of the same session in the
    /// same transaction (its path and offset; whatever else the row holds
    /// stays).
    pub fn capture_claude_code_batch(
        &self,
        captures: Vec<crate::privacy::ClaudeCodeCapture>,
        cursor: HarnessCursorUpdate<'_>,
    ) -> Result<HarnessBatchOutcome> {
        let batch = PreparedHarnessBatch::prepare(
            captures
                .into_iter()
                .map(crate::privacy::ClaudeCodeCapture::into_capture),
        );
        let byte_offset = i64::try_from(cursor.byte_offset)
            .map_err(|_| DbError::Migration("harness cursor offset overflows".into()))?;
        let mut outcome = self.with_immediate_write_retry(|tx| {
            let outcome = Self::insert_harness_captures_tx(tx, &batch)?;
            let now = Utc::now().timestamp();
            Self::write_legacy_cursor_tx(tx, &cursor, byte_offset, now)?;
            if cursor.harness == HarnessId::ClaudeCode.name() {
                Self::mirror_claude_code_cursor_tx(tx, &cursor, byte_offset, now)?;
            }
            Ok(outcome)
        })?;
        outcome.lost = batch.lost.len();
        outcome.reasons = batch.reasons;
        self.ensure_wal_visible()?;
        Ok(outcome)
    }

    fn write_legacy_cursor_tx(
        tx: &Transaction<'_>,
        cursor: &HarnessCursorUpdate<'_>,
        byte_offset: i64,
        now: i64,
    ) -> Result<()> {
        tx.execute(
            UPSERT_LEGACY_CURSOR_SQL,
            params![
                cursor.harness,
                cursor.session_id,
                cursor.transcript_path,
                byte_offset,
                now
            ],
        )?;
        Ok(())
    }

    fn mirror_claude_code_cursor_tx(
        tx: &Transaction<'_>,
        cursor: &HarnessCursorUpdate<'_>,
        byte_offset: i64,
        now: i64,
    ) -> Result<()> {
        tx.execute(
            MIRROR_CLAUDE_CODE_CURSOR_SQL,
            params![
                cursor.harness,
                cursor.session_id,
                cursor.transcript_path,
                byte_offset,
                now
            ],
        )?;
        Ok(())
    }

    /// Stores the prepared captures of one harness batch, skipping what is
    /// already stored or locally tombstoned (never replacing it: replacing
    /// would drop the commit links an earlier ingest has earned).
    fn insert_harness_captures_tx(
        tx: &Transaction<'_>,
        batch: &PreparedHarnessBatch,
    ) -> Result<HarnessBatchOutcome> {
        let mut outcome = HarnessBatchOutcome::default();
        // What this batch did not store, and what each of those was a child
        // of: a capture whose parent is one of them is a child of that
        // parent instead, so the chain skips over what is missing. A parent
        // that is stored nowhere would fail the batch on the foreign key, so
        // the chain breaks there instead.
        let mut skipped: std::collections::HashMap<&InteractionId, Option<&InteractionId>> = batch
            .lost
            .iter()
            .map(|(id, parent)| (id, parent.as_ref()))
            .collect();
        for (capture, withheld) in &batch.prepared {
            let id = &capture.interaction.id;
            let exists = Self::interaction_exists_tx(tx, id)?;
            if exists || Self::is_tombstoned_tx(tx, id)? {
                outcome.suppressed += 1;
                if !exists {
                    skipped.insert(id, capture.interaction.parent_id.as_ref());
                }
                continue;
            }
            let mut parent = capture.interaction.parent_id.as_ref();
            for _ in 0..=skipped.len() {
                match parent.and_then(|parent| skipped.get(parent)) {
                    Some(next) => parent = *next,
                    None => break,
                }
            }
            let parent = match parent {
                Some(parent) if Self::interaction_exists_tx(tx, parent)? => Some(parent),
                _ => None,
            };
            Self::insert_capture_with_parent(tx, capture, parent)?;
            outcome.inserted += 1;
            outcome.withheld += usize::from(*withheld);
        }
        Ok(outcome)
    }

    /// The typed cursor `harness` keeps under `cursor_key`, if any.
    pub fn harness_cursor(
        &self,
        harness: HarnessId,
        cursor_key: &str,
    ) -> Result<Option<StoredHarnessCursor>> {
        Self::harness_cursor_in(&self.conn, harness.name(), cursor_key)
    }

    fn harness_cursor_in(
        conn: &Connection,
        harness: &str,
        cursor_key: &str,
    ) -> Result<Option<StoredHarnessCursor>> {
        let row = conn
            .query_row(
                "SELECT harness, cursor_key, kind, version, epoch, state, updated_at FROM harness_cursors WHERE harness=?1 AND cursor_key=?2",
                params![harness, cursor_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((harness, cursor_key, kind, version, epoch, state, updated_at)) = row else {
            return Ok(None);
        };
        let corrupt = || DbError::Migration("stored harness cursor is malformed".into());
        Ok(Some(StoredHarnessCursor {
            harness,
            cursor_key,
            kind,
            version: u32::try_from(version).map_err(|_| corrupt())?,
            epoch: u64::try_from(epoch).map_err(|_| corrupt())?,
            state: serde_json::from_str(&state).map_err(|_| corrupt())?,
            updated_at,
        }))
    }

    /// The relation recorded for the child conversation `child_id`, if any.
    pub fn conversation_relation(&self, child_id: &str) -> Result<Option<ConversationRelation>> {
        Self::conversation_relation_in(&self.conn, child_id)
    }

    fn conversation_relation_in(
        conn: &Connection,
        child_id: &str,
    ) -> Result<Option<ConversationRelation>> {
        let row = conn
            .query_row(
                "SELECT child_id, parent_id, kind, harness, spawn_interaction_id, spawn_call_id, evidence FROM conversation_relations WHERE child_id=?1",
                params![child_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((child_id, parent_id, kind, harness, spawn_interaction, spawn_call_id, evidence)) =
            row
        else {
            return Ok(None);
        };
        let corrupt = || DbError::Migration("stored conversation relation is malformed".into());
        Ok(Some(ConversationRelation {
            child_id,
            parent_id,
            kind: RelationKind::from_db_str(&kind).ok_or_else(corrupt)?,
            harness: HarnessId::from_name(&harness).ok_or_else(corrupt)?,
            spawn_interaction_id: spawn_interaction
                .map(|id| id.parse().map_err(|_| corrupt()))
                .transpose()?,
            spawn_call_id,
            evidence: RelationEvidence::from_db_str(&evidence).ok_or_else(corrupt)?,
        }))
    }

    /// The conversations recorded as children of `parent_id`, by id.
    pub fn conversation_children(&self, parent_id: &str) -> Result<Vec<ConversationRelation>> {
        let ids: Vec<String> = self
            .conn
            .prepare(
                "SELECT child_id FROM conversation_relations WHERE parent_id=?1 ORDER BY child_id",
            )?
            .query_map(params![parent_id], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        ids.iter()
            .filter_map(|id| Self::conversation_relation_in(&self.conn, id).transpose())
            .collect()
    }

    /// Every commit-evidence row of `harness` for the family rooted at
    /// `family_root`, ordered by response time, interaction and candidate.
    pub fn harness_commit_evidence(
        &self,
        harness: HarnessId,
        family_root: &str,
    ) -> Result<Vec<HarnessCommitEvidence>> {
        self.conn
            .prepare("SELECT family_root, interaction_id, candidate, response_at FROM harness_commit_evidence WHERE harness=?1 AND family_root=?2 ORDER BY response_at, interaction_id, candidate")?
            .query_map(params![harness.name(), family_root], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .map(|row| {
                let (family_root, interaction, candidate, response_at) = row?;
                Ok(HarnessCommitEvidence {
                    family_root,
                    interaction_id: interaction.parse().map_err(|_| {
                        DbError::Migration("stored commit evidence is malformed".into())
                    })?,
                    candidate,
                    response_at,
                })
            })
            .collect()
    }

    /// Commits one harness ingest batch for one conversation (see
    /// [`HarnessBatch`]): its captures (prepared at the capture boundary first,
    /// stand-ins included, exactly as [`Self::capture_claude_code_batch`]
    /// does), the typed cursor (never moved backwards), the Claude Code legacy
    /// cursor, durable commit evidence and conversation relations, in one
    /// immediate transaction.
    ///
    /// Refused as a whole, before anything is stored, when the batch is not
    /// consistent: a capture of another harness or conversation, a malformed
    /// cursor, a legacy cursor for a harness other than Claude Code, a relation
    /// for Claude Code or outside the harness's namespace. Refused inside the
    /// transaction when a prefixed harness would record into a conversation
    /// that holds interactions of another local source (a 0.7.x MCP client may
    /// have created `codex:x`), when the cursor's epoch skips ahead, and when a
    /// relation conflicts with the one on record.
    pub fn capture_harness_batch(&self, batch: HarnessBatch<'_>) -> Result<HarnessBatchOutcome> {
        let HarnessBatch {
            harness,
            conversation_id,
            captures,
            cursor,
            legacy_cursor,
            evidence,
            relations,
        } = batch;
        let refuse = |rule: &str| Err(DbError::HarnessBatch(rule.into()));
        if !crate::privacy::is_safe_identifier(conversation_id) {
            return refuse("the conversation id is not a safe identifier");
        }
        if captures
            .iter()
            .any(|c| c.harness() != harness || c.conversation_id() != conversation_id)
        {
            return refuse("a capture belongs to another harness or conversation");
        }
        let state = match &cursor {
            Some(cursor) => {
                if !crate::privacy::is_safe_identifier(cursor.cursor_key)
                    || cursor.kind.is_empty()
                    || cursor.kind.len() > 32
                    || !cursor
                        .kind
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                    || cursor.version == 0
                    || i64::try_from(cursor.epoch).is_err()
                    || !cursor.state.is_object()
                {
                    return refuse("the cursor is malformed");
                }
                let state = serde_json::to_string(&cursor.state)
                    .map_err(|e| DbError::HarnessBatch(e.to_string()))?;
                if state.len() > MAX_CURSOR_STATE_BYTES {
                    return refuse("the cursor state is too large");
                }
                Some(state)
            }
            None => None,
        };
        let legacy_offset =
            match &legacy_cursor {
                Some(legacy) => {
                    if harness != HarnessId::ClaudeCode
                        || legacy.harness != harness.name()
                        || cursor
                            .as_ref()
                            .is_some_and(|cursor| cursor.cursor_key != legacy.session_id)
                    {
                        return refuse("a legacy cursor is kept for Claude Code sessions only");
                    }
                    Some(i64::try_from(legacy.byte_offset).map_err(|_| {
                        DbError::Migration("harness cursor offset overflows".into())
                    })?)
                }
                None => None,
            };
        for relation in &relations {
            Self::check_relation(harness, conversation_id, relation)?;
        }
        let prepared =
            PreparedHarnessBatch::prepare(captures.into_iter().map(|c| c.into_capture()));
        let mut outcome = self.with_immediate_write_retry(|tx| {
            // Claude Code's value is in every constraint this version leaves
            // in place; the other harnesses' values only in the triggers.
            if harness != HarnessId::ClaudeCode {
                let upgraded: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM cvc_internal_migrations WHERE name=?1)",
                    params![CAPTURE_SOURCE_TRIGGERS_MARKER],
                    |row| row.get(0),
                )?;
                if !upgraded {
                    return Err(DbError::CaptureSourceUpgradeRequired { harness });
                }
            }
            if harness.conversation_prefix().is_some() {
                let foreign: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM interactions WHERE conversation_id=?1 AND capture_source NOT IN (?2,'sync_import'))",
                    params![conversation_id, harness.capture_source_str()],
                    |row| row.get(0),
                )?;
                if foreign {
                    return Err(DbError::HarnessBatch(
                        "the conversation holds interactions another source recorded".into(),
                    ));
                }
            }
            let mut outcome = Self::insert_harness_captures_tx(tx, &prepared)?;
            let now = Utc::now().timestamp();
            let cursor_written = match (&cursor, &state) {
                (Some(cursor), Some(state)) => {
                    Self::write_harness_cursor_tx(tx, harness, cursor, state, now)?
                }
                _ => true,
            };
            outcome.cursor_held = !cursor_written;
            if let (Some(legacy), Some(offset), true) =
                (&legacy_cursor, legacy_offset, cursor_written)
            {
                Self::write_legacy_cursor_tx(tx, legacy, offset, now)?;
            }
            for item in &evidence {
                if Self::write_commit_evidence_tx(tx, harness, conversation_id, item)? {
                    outcome.evidence_recorded += 1;
                } else {
                    outcome.evidence_skipped += 1;
                }
            }
            for relation in &relations {
                if Self::write_relation_tx(tx, relation, now)? {
                    outcome.relations_recorded += 1;
                }
            }
            Ok(outcome)
        })?;
        outcome.lost = prepared.lost.len();
        outcome.reasons = prepared.reasons;
        self.ensure_wal_visible()?;
        Ok(outcome)
    }

    /// Writes `cursor` unless the stored one is ahead of it. `Ok(false)` when
    /// the stored cursor is kept.
    fn write_harness_cursor_tx(
        tx: &Transaction<'_>,
        harness: HarnessId,
        cursor: &HarnessCursorWrite<'_>,
        state: &str,
        now: i64,
    ) -> Result<bool> {
        if let Some(stored) = Self::harness_cursor_in(tx, harness.name(), cursor.cursor_key)? {
            if cursor.version < stored.version || cursor.epoch < stored.epoch {
                return Ok(false);
            }
            if cursor.epoch == stored.epoch {
                if cursor.kind != stored.kind {
                    return Err(DbError::HarnessBatch(
                        "the cursor changes kind within an epoch".into(),
                    ));
                }
                if (cursor.progress)(&stored) == CursorProgress::Backward {
                    return Ok(false);
                }
            } else if cursor.epoch != stored.epoch + 1 {
                return Err(DbError::HarnessBatch(
                    "the cursor's epoch skips ahead of the stored one".into(),
                ));
            }
        }
        tx.execute(
            "INSERT INTO harness_cursors(harness,cursor_key,kind,version,epoch,state,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(harness,cursor_key) DO UPDATE SET kind=excluded.kind, version=excluded.version, epoch=excluded.epoch, state=excluded.state, updated_at=excluded.updated_at",
            params![
                harness.name(),
                cursor.cursor_key,
                cursor.kind,
                cursor.version,
                cursor.epoch as i64,
                state,
                now
            ],
        )?;
        Ok(true)
    }

    /// Stores one commit-evidence row. `Ok(false)` when it is passed over: a
    /// candidate that is not 4 to 64 lowercase hex digits, a family root that
    /// is not a safe identifier, or an interaction not stored in
    /// `conversation_id` (lost, tombstoned, or of another conversation).
    fn write_commit_evidence_tx(
        tx: &Transaction<'_>,
        harness: HarnessId,
        conversation_id: &str,
        evidence: &HarnessCommitEvidence,
    ) -> Result<bool> {
        let candidate = &evidence.candidate;
        if !(4..=64).contains(&candidate.len())
            || !candidate
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !crate::privacy::is_safe_identifier(&evidence.family_root)
        {
            return Ok(false);
        }
        let stored: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM interactions WHERE id=?1 AND conversation_id=?2)",
            params![evidence.interaction_id.as_str(), conversation_id],
            |row| row.get(0),
        )?;
        if !stored {
            return Ok(false);
        }
        tx.execute(
            "INSERT OR IGNORE INTO harness_commit_evidence(harness,family_root,interaction_id,candidate,response_at) VALUES (?1,?2,?3,?4,?5)",
            params![
                harness.name(),
                evidence.family_root,
                evidence.interaction_id.as_str(),
                candidate,
                evidence.response_at
            ],
        )?;
        Ok(true)
    }

    /// The checks on a relation that need no database.
    fn check_relation(
        harness: HarnessId,
        conversation_id: &str,
        relation: &ConversationRelation,
    ) -> Result<()> {
        let refuse = |rule: &str| Err(DbError::HarnessBatch(rule.into()));
        let Some(prefix) = harness.conversation_prefix() else {
            return refuse("Claude Code relations are its id convention; none is recorded");
        };
        if relation.harness != harness {
            return refuse("a relation belongs to another harness");
        }
        for id in [&relation.child_id, &relation.parent_id] {
            if !crate::privacy::is_safe_identifier(id)
                || !id.starts_with(prefix)
                || id.contains(SUBAGENT_MARKER)
            {
                return refuse("a relation names a conversation outside the harness namespace");
            }
        }
        if relation.child_id == relation.parent_id {
            return refuse("a conversation cannot be its own parent");
        }
        if relation.child_id != conversation_id && relation.parent_id != conversation_id {
            return refuse("a relation does not involve the batch's conversation");
        }
        if relation
            .spawn_call_id
            .as_deref()
            .is_some_and(|id| !crate::privacy::is_safe_identifier(id))
        {
            return refuse("a relation's spawn call id is not a safe identifier");
        }
        Ok(())
    }

    /// Records `relation`, never overwriting one on record: the same relation
    /// again does nothing, except that a missing spawn is filled in once; any
    /// other difference, a cycle, or a chain deeper than
    /// [`MAX_RELATION_DEPTH`] is refused. `Ok(true)` when a row was written or
    /// completed.
    fn write_relation_tx(
        tx: &Transaction<'_>,
        relation: &ConversationRelation,
        now: i64,
    ) -> Result<bool> {
        let conflict = |rule: &str| Err(DbError::RelationConflict(rule.into()));
        let spawn_interaction = relation
            .spawn_interaction_id
            .as_ref()
            .map(InteractionId::as_str);
        if let Some(stored) = Self::conversation_relation_in(tx, &relation.child_id)? {
            if stored.parent_id != relation.parent_id
                || stored.kind != relation.kind
                || stored.harness != relation.harness
            {
                return conflict("the child already has another parent");
            }
            let mut completed = false;
            for (column, stored, offered) in [
                (
                    "spawn_interaction_id",
                    stored
                        .spawn_interaction_id
                        .as_ref()
                        .map(InteractionId::as_str),
                    spawn_interaction.clone(),
                ),
                (
                    "spawn_call_id",
                    stored.spawn_call_id.clone(),
                    relation.spawn_call_id.clone(),
                ),
            ] {
                match (stored, offered) {
                    (Some(stored), Some(offered)) if stored != offered => {
                        return conflict("the child was spawned by another call");
                    }
                    (None, Some(offered)) => {
                        tx.execute(
                            &format!("UPDATE conversation_relations SET {column}=?2 WHERE child_id=?1 AND {column} IS NULL"),
                            params![relation.child_id, offered],
                        )?;
                        completed = true;
                    }
                    _ => {}
                }
            }
            return Ok(completed);
        }
        // The chain above the parent, nearest first, and how far the child's
        // own recorded descendants reach below it.
        let mut above = 0usize;
        let mut cursor = relation.parent_id.clone();
        loop {
            if cursor == relation.child_id {
                return conflict("the relation would make a cycle");
            }
            above += 1;
            if above > MAX_RELATION_DEPTH {
                return conflict("the relation chain is too deep");
            }
            match tx
                .query_row(
                    "SELECT parent_id FROM conversation_relations WHERE child_id=?1",
                    params![cursor],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                Some(parent) => cursor = parent,
                None => break,
            }
        }
        let mut below = 0usize;
        let mut level = vec![relation.child_id.clone()];
        while !level.is_empty() {
            let mut next = Vec::new();
            for id in &level {
                let children: Vec<String> = tx
                    .prepare("SELECT child_id FROM conversation_relations WHERE parent_id=?1")?
                    .query_map(params![id], |row| row.get(0))?
                    .collect::<std::result::Result<_, _>>()?;
                next.extend(children);
            }
            if next.is_empty() {
                break;
            }
            if next.contains(&relation.parent_id) {
                return conflict("the relation would make a cycle");
            }
            below += 1;
            if above + below > MAX_RELATION_DEPTH {
                return conflict("the relation chain is too deep");
            }
            level = next;
        }
        tx.execute(
            "INSERT INTO conversation_relations(child_id,parent_id,kind,harness,spawn_interaction_id,spawn_call_id,evidence,recorded_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                relation.child_id,
                relation.parent_id,
                relation.kind.as_db_str(),
                relation.harness.name(),
                spawn_interaction,
                relation.spawn_call_id,
                relation.evidence.as_db_str(),
                now
            ],
        )?;
        Ok(true)
    }

    /// Runs `body` inside an immediate transaction, retrying with backoff while
    /// another process holds the write lock, up to a bounded budget. The short
    /// connection-level busy timeout stays as it is because Git hooks must not
    /// wait; harness ingests may.
    fn with_immediate_write_retry<T>(
        &self,
        mut body: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let deadline = Instant::now() + HARNESS_WRITE_RETRY_BUDGET;
        let mut delay = Duration::from_millis(50);
        loop {
            let attempt = (|| {
                let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
                let value = body(&tx)?;
                tx.commit()?;
                Ok(value)
            })();
            match attempt {
                Err(DbError::Sqlite(rusqlite::Error::SqliteFailure(error, _)))
                    if matches!(
                        error.code,
                        ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
                    ) && Instant::now() + delay < deadline =>
                {
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(Duration::from_millis(800));
                }
                other => return other,
            }
        }
    }

    fn insert_capture(tx: &Transaction<'_>, capture: &Capture) -> Result<InteractionId> {
        Self::insert_capture_with_parent(tx, capture, capture.interaction.parent_id.as_ref())
    }

    fn interaction_exists_tx(tx: &Transaction<'_>, id: &InteractionId) -> Result<bool> {
        Ok(tx
            .query_row(
                "SELECT 1 FROM interactions WHERE id=?1",
                params![id.as_str()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Inserts `capture` as a child of `parent` rather than of the parent it
    /// names, for a batch whose chain had to skip a capture it did not store.
    fn insert_capture_with_parent(
        tx: &Transaction<'_>,
        capture: &Capture,
        parent: Option<&InteractionId>,
    ) -> Result<InteractionId> {
        if Self::is_tombstoned_tx(tx, &capture.interaction.id)? {
            return Err(DbError::Migration("interaction is tombstoned".into()));
        }
        // Every source titles a conversation it creates. Only a source that may
        // rename an existing one (`CaptureSource::refreshes_conversation_title`)
        // overwrites its title; an MCP thought leaves it exactly as it was.
        let conversation_sql = if capture.source.refreshes_conversation_title() {
            "INSERT INTO conversations (id,title,created_at) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET title=excluded.title"
        } else {
            "INSERT INTO conversations (id,title,created_at) VALUES (?1,?2,?3) ON CONFLICT(id) DO NOTHING"
        };
        tx.execute(
            conversation_sql,
            params![
                capture.conversation.id,
                capture.conversation.title,
                capture.conversation.created_at.timestamp()
            ],
        )?;
        let i = &capture.interaction;
        let parent = parent.map(InteractionId::as_str);
        let author = serde_json::to_string(&i.author)
            .unwrap_or_default()
            .replace('"', "");
        let source = capture.source.as_db_str();
        let visibility = "private";
        tx.execute("INSERT INTO interactions (id,conversation_id,parent_id,timestamp,author,user_prompt,model_name,model_cot,model_response,source_request_id,visibility,capture_source,scrubber_version,capture_worktree) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,1,?13)", params![i.id.to_string(),i.conversation_id,parent,i.timestamp.timestamp(),author,i.user_prompt,i.model_name,i.model_cot,i.model_response,i.source_request_id,visibility,source,capture.capture_worktree])?;
        // The conversation's future sharing extends only to captures whose
        // trusted source takes part in it (`CaptureSource::inherits_future_sharing`).
        // Every other capture, an MCP thought above all, is stored private and is
        // shared only when an explicit share names its conversation again.
        if capture.source.inherits_future_sharing() {
            tx.execute("INSERT OR IGNORE INTO interaction_shares(interaction_id,remote_fingerprint) SELECT ?1,remote_fingerprint FROM conversation_shares WHERE conversation_id=?2 AND share_future=1", params![i.id.to_string(), i.conversation_id])?;
        }
        for x in &capture.context_items {
            tx.execute("INSERT INTO context_items (interaction_id,file_path,git_blob_sha,dirty_patch,start_line,end_line) VALUES (?1,?2,?3,?4,?5,?6)",params![i.id.to_string(),x.file_path,x.git_blob_sha,x.dirty_patch,x.start_line,x.end_line])?;
        }
        for x in &capture.tool_executions {
            let status = match x.status {
                ToolStatus::Success => "success",
                ToolStatus::Failure => "failure",
            };
            tx.execute("INSERT INTO tool_executions (interaction_id,tool_protocol,tool_name,arguments,status) VALUES (?1,?2,?3,?4,?5)",params![i.id.to_string(),x.tool_protocol,x.tool_name,x.arguments,status])?;
        }
        Ok(i.id.clone())
    }

    fn is_tombstoned_tx(tx: &Transaction<'_>, id: &InteractionId) -> Result<bool> {
        Ok(tx
            .query_row(
                "SELECT 1 FROM tombstones WHERE interaction_id=?1 AND scope_kind='local'",
                params![id.as_str()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Effective pull scope is LocalOnly plus the exact source destination.
    /// A tombstone received from A has no authority to suppress B.
    fn is_tombstoned_for_source_tx(
        tx: &Transaction<'_>,
        id: &InteractionId,
        source_remote: Option<&str>,
    ) -> Result<bool> {
        Ok(tx.query_row(
            "SELECT 1 FROM tombstones WHERE interaction_id=?1 AND (scope_kind='local' OR (scope_kind='remote' AND remote_fingerprint IS ?2))",
            params![id.as_str(), source_remote], |_| Ok(())
        ).optional()?.is_some())
    }

    pub fn is_tombstoned(&self, id: &InteractionId) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM tombstones WHERE interaction_id=?1 AND scope_kind='local'",
                params![id.as_str()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Tombstones that this process may emit to `remote`.  Received remote
    /// tombstones are deliberately not transitive authorization.
    pub fn tombstones_for_projection(&self, remote: &str) -> Result<Vec<Tombstone>> {
        let mut stmt = self.conn.prepare(
            "SELECT interaction_id,deleted_at,reason_code,previous_node_oid FROM tombstones WHERE scope_kind='remote' AND remote_fingerprint=?1",
        )?;
        let rows = stmt
            .query_map(params![remote], |r| {
                let reason = match r.get::<_, String>(2)?.as_str() {
                    "user_requested" => TombstoneReasonCode::UserRequested,
                    "security" => TombstoneReasonCode::Security,
                    _ => TombstoneReasonCode::Retention,
                };
                let timestamp: i64 = r.get(1)?;
                Ok(Tombstone {
                    format: "cvc.tombstone/v1".into(),
                    version: 1,
                    interaction_id: r.get::<_, String>(0)?.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    deleted_at: Utc
                        .timestamp_opt(timestamp, 0)
                        .single()
                        .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
                    reason_code: reason,
                    previous_node_oid: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Tombstones win over every local projection and are idempotent only when
    /// their immutable semantics match exactly.
    pub fn apply_tombstones(
        &self,
        tombstones: &[Tombstone],
        source: &str,
        remote: Option<&str>,
    ) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        Self::apply_tombstones_tx(&tx, tombstones, source, remote)?;
        tx.commit()?;
        if !tombstones.is_empty() {
            self.compact_after_deletion()?;
        }
        Ok(())
    }

    fn apply_tombstones_tx(
        tx: &Transaction<'_>,
        tombstones: &[Tombstone],
        source: &str,
        remote: Option<&str>,
    ) -> Result<()> {
        for t in tombstones {
            let scope_kind = if remote.is_some() { "remote" } else { "local" };
            let existing: Option<(String,i64,String,Option<String>,String)> = tx.query_row("SELECT format,version,reason_code,previous_node_oid,state FROM tombstones WHERE interaction_id=?1 AND scope_kind=?2 AND remote_fingerprint IS ?3", params![t.interaction_id.as_str(),scope_kind,remote], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let reason = match t.reason_code {
                TombstoneReasonCode::UserRequested => "user_requested",
                TombstoneReasonCode::Security => "security",
                TombstoneReasonCode::Retention => "retention",
            };
            if let Some((format, version, old_reason, old_oid, state)) = existing {
                if format != t.format
                    || version != t.version as i64
                    || old_reason != reason
                    || old_oid != t.previous_node_oid
                {
                    return Err(DbError::Migration("tombstone semantic conflict".into()));
                }
                // A wire match is proof that an ambiguous/pending local redact
                // reached this exact destination.
                if source == "sync" && state == "pending" {
                    tx.execute("UPDATE tombstones SET state='published' WHERE interaction_id=?1 AND scope_kind=?2 AND remote_fingerprint IS ?3", params![t.interaction_id.as_str(), scope_kind, remote])?;
                }
            } else {
                let state = if source == "sync" {
                    "published"
                } else {
                    "pending"
                };
                tx.execute("INSERT INTO tombstones(interaction_id,scope_kind,remote_fingerprint,format,version,deleted_at,reason_code,previous_node_oid,source,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![t.interaction_id.as_str(),scope_kind,remote,t.format,t.version,t.deleted_at.timestamp(),reason,t.previous_node_oid,source,state])?;
            }
            // Children survive but cannot retain a dangling parent.
            tx.execute(
                "UPDATE interactions SET parent_id=NULL WHERE parent_id=?1",
                params![t.interaction_id.as_str()],
            )?;
            let all_events: Vec<(String, String, String)> = tx
                .prepare("SELECT event_id,interaction_id,source_event_ids FROM derivation_events")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<std::result::Result<_, _>>()?;
            let target = t.interaction_id.to_string();
            let mut removed: std::collections::HashSet<String> = all_events
                .iter()
                .filter(|(_, interaction, sources)| {
                    interaction == &target
                        || serde_json::from_str::<Vec<String>>(sources)
                            .ok()
                            .is_some_and(|sources| {
                                sources
                                    .iter()
                                    .any(|source| source.starts_with(&format!("legacy:{target}:")))
                            })
                })
                .map(|(id, _, _)| id.clone())
                .collect();
            loop {
                let before = removed.len();
                for (id, _, sources) in &all_events {
                    if serde_json::from_str::<Vec<String>>(sources)
                        .ok()
                        .is_some_and(|sources| {
                            sources.iter().any(|source| removed.contains(source))
                        })
                    {
                        removed.insert(id.clone());
                    }
                }
                if removed.len() == before {
                    break;
                }
            }
            for event_id in removed {
                tx.execute(
                    "DELETE FROM derivation_authorizations WHERE event_id=?1",
                    params![event_id],
                )?;
                tx.execute(
                    "DELETE FROM derivation_observations WHERE event_id=?1",
                    params![event_id],
                )?;
                tx.execute(
                    "DELETE FROM derivation_events WHERE event_id=?1",
                    params![event_id],
                )?;
            }
            for table in [
                "context_items",
                "tool_executions",
                "artifact_links",
                "interaction_shares",
                "publications",
                "harness_commit_evidence",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE interaction_id=?1"),
                    params![t.interaction_id.as_str()],
                )?;
            }
            tx.execute(
                "DELETE FROM interactions WHERE id=?1",
                params![t.interaction_id.as_str()],
            )?;
        }
        Ok(())
    }

    pub fn tombstone_local(
        &self,
        id: &InteractionId,
        reason: TombstoneReasonCode,
        previous_node_oid: Option<String>,
    ) -> Result<Tombstone> {
        let tombstone = Tombstone::new(id.clone(), reason, previous_node_oid);
        self.apply_tombstones(std::slice::from_ref(&tombstone), "local", None)?;
        Ok(tombstone)
    }

    pub fn tombstone_remote(
        &self,
        id: &InteractionId,
        remote: &str,
        reason: TombstoneReasonCode,
        previous_node_oid: Option<String>,
    ) -> Result<Tombstone> {
        if !self.remote_redaction_authorized(id, remote)? {
            return Err(DbError::Migration(
                "remote redaction requires an existing locally authorized interaction".into(),
            ));
        }
        let tombstone = Tombstone::new(id.clone(), reason, previous_node_oid);
        self.apply_tombstones(std::slice::from_ref(&tombstone), "local", Some(remote))?;
        Ok(tombstone)
    }

    pub fn remote_redaction_authorized(&self, id: &InteractionId, remote: &str) -> Result<bool> {
        let authorized: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM interactions i WHERE i.id=?1 AND (EXISTS (SELECT 1 FROM interaction_shares s WHERE s.interaction_id=i.id AND s.remote_fingerprint=?2) OR EXISTS (SELECT 1 FROM publications p WHERE p.interaction_id=i.id AND p.remote_fingerprint=?2 AND p.state IN ('pending','published','unknown')))",
            params![id.as_str(), remote], |r| r.get(0))?;
        Ok(authorized == 1)
    }

    pub fn mark_remote_tombstones_published(&self, remote: &str) -> Result<()> {
        self.conn.execute("UPDATE tombstones SET state='published' WHERE scope_kind='remote' AND remote_fingerprint=?1 AND state='pending'", params![remote])?;
        Ok(())
    }

    pub fn get_interaction(&self, id: &InteractionId) -> Result<Option<Interaction>> {
        self.conn
            .query_row(
                "SELECT id, conversation_id, parent_id, timestamp, author, user_prompt,
                    model_name, model_cot, model_response, source_request_id
             FROM interactions WHERE id = ?1",
                params![id.as_str()],
                |row| {
                    let parent_id_str: Option<String> = row.get(2)?;
                    let timestamp: i64 = row.get(3)?;
                    let author_str: String = row.get(4)?;
                    let author = match author_str.as_str() {
                        "human" => Author::Human,
                        "agent" => Author::Agent,
                        "system" => Author::System,
                        _ => Author::External,
                    };

                    let dt = Utc.timestamp_opt(timestamp, 0).single().ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Integer,
                            Box::new(DbError::Timestamp(timestamp)),
                        )
                    })?;

                    Ok(Interaction {
                        id: row.get::<_, String>(0)?.parse().map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                        conversation_id: row.get(1)?,
                        parent_id: parent_id_str.map(|s| s.parse().unwrap_or_default()), // Fallback or fail? Ideally fail. But map is easier. Let's make it robust.
                        timestamp: dt,
                        author,
                        user_prompt: row.get(5)?,
                        model_name: row.get(6)?,
                        model_cot: row.get(7)?,
                        model_response: row.get(8)?,
                        source_request_id: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Delete all interactions (and their related context_items, tool_executions, artifact_links)
    /// that were generated from a specific VS Code chat request.
    pub fn delete_interactions_by_source_request_id(&self, source_request_id: &str) -> Result<()> {
        // First collect the interaction IDs to cascade-delete related rows
        let ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM interactions WHERE source_request_id = ?1")?;
            let rows = stmt.query_map(params![source_request_id], |row| row.get(0))?;
            rows.collect::<std::result::Result<Vec<String>, _>>()?
        };

        if ids.is_empty() {
            return Ok(());
        }

        for id in &ids {
            self.conn.execute(
                "DELETE FROM context_items WHERE interaction_id = ?1",
                params![id],
            )?;
            self.conn.execute(
                "DELETE FROM tool_executions WHERE interaction_id = ?1",
                params![id],
            )?;
            self.conn.execute(
                "DELETE FROM artifact_links WHERE interaction_id = ?1",
                params![id],
            )?;
        }

        self.conn.execute(
            "DELETE FROM interactions WHERE source_request_id = ?1",
            params![source_request_id],
        )?;
        self.compact_after_deletion()?;
        Ok(())
    }

    // --- Context Items ---

    // --- Floating Nodes ---

    pub fn get_floating_interactions(&self) -> Result<Vec<Interaction>> {
        self.floating_interactions(None)
    }

    /// Floating nodes that automatic linking may consider from one active
    /// worktree: rows captured in that worktree plus legacy/imported rows with
    /// no recorded origin. Nodes captured by a sibling worktree are excluded so
    /// parallel checkouts cannot claim each other's pending thoughts.
    pub fn get_floating_interactions_for_worktree(
        &self,
        capture_worktree: &str,
    ) -> Result<Vec<Interaction>> {
        self.floating_interactions(Some(capture_worktree))
    }

    fn floating_interactions(&self, capture_worktree: Option<&str>) -> Result<Vec<Interaction>> {
        // Interactions that are NOT in artifact_links
        let mut stmt = self.conn.prepare(
            "SELECT i.id, i.conversation_id, i.parent_id, i.timestamp, i.author, i.user_prompt,
                    i.model_name, i.model_cot, i.model_response, i.source_request_id
             FROM interactions i
             LEFT JOIN artifact_links al ON i.id = al.interaction_id
             WHERE al.interaction_id IS NULL
               AND (?1 IS NULL OR i.capture_worktree IS NULL OR i.capture_worktree = ?1)",
        )?;

        let rows = stmt.query_map(params![capture_worktree], |row| {
            let parent_id_str: Option<String> = row.get(2)?;
            let timestamp: i64 = row.get(3)?;
            let author_str: String = row.get(4)?;
            let author = match author_str.as_str() {
                "human" => Author::Human,
                "agent" => Author::Agent,
                "system" => Author::System,
                _ => Author::External,
            };

            let dt = Utc.timestamp_opt(timestamp, 0).single().ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Integer,
                    Box::new(DbError::Timestamp(timestamp)),
                )
            })?;

            Ok(Interaction {
                id: row.get::<_, String>(0)?.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                conversation_id: row.get(1)?,
                parent_id: parent_id_str.map(|s| s.parse().unwrap_or_default()),
                timestamp: dt,
                author,
                user_prompt: row.get(5)?,
                model_name: row.get(6)?,
                model_cot: row.get(7)?,
                model_response: row.get(8)?,
                source_request_id: row.get(9)?,
            })
        })?;

        let mut interactions = Vec::new();
        for person in rows {
            interactions.push(person?);
        }
        Ok(interactions)
    }

    /// Most recent interactions in a single conversation, newest first, regardless of
    /// whether they've been linked to a commit yet.
    pub fn get_recent_interactions_for_conversation(
        &self,
        conversation_id: &str,
        limit: usize,
    ) -> Result<Vec<Interaction>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, conversation_id, parent_id, timestamp, author, user_prompt,
                    model_name, model_cot, model_response, source_request_id
             FROM interactions
             WHERE conversation_id = ?1
             ORDER BY timestamp DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![conversation_id, limit as i64], map_interaction_row)?;
        let mut interactions = Vec::new();
        for row in rows {
            interactions.push(row?);
        }
        Ok(interactions)
    }

    /// Most recent interactions across the whole repo, newest first, regardless of
    /// conversation or link status.
    pub fn get_recent_interactions(&self, limit: usize) -> Result<Vec<Interaction>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, conversation_id, parent_id, timestamp, author, user_prompt,
                    model_name, model_cot, model_response, source_request_id
             FROM interactions
             ORDER BY timestamp DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], map_interaction_row)?;
        let mut interactions = Vec::new();
        for row in rows {
            interactions.push(row?);
        }
        Ok(interactions)
    }

    // --- Artifact Links ---

    pub fn link_interaction(
        &self,
        interaction_id: &InteractionId,
        commit_sha: &CommitSha,
        link_type: &str,
    ) -> Result<()> {
        self.link_interaction_with_metadata(interaction_id, commit_sha, link_type, None)
    }

    /// Apply a completely built rewrite batch in one immediate transaction.
    /// Existing links are evidence and are never replaced or deleted.
    pub(crate) fn apply_rewrite_events(
        &mut self,
        batch_id: &str,
        mode: &str,
        payload_hash: &str,
        events: &[DerivationEvent],
        snapshots: &[SourceSnapshot],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((old_mode, old_hash)) = tx
            .query_row(
                "SELECT mode,payload_hash FROM rewrite_batches WHERE batch_id=?1",
                params![batch_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if old_mode == mode && old_hash == payload_hash {
                tx.commit()?;
                return Ok(());
            }
            return Err(DbError::LinkConflict(
                "rewrite batch identity conflict".into(),
            ));
        }
        // Full source CAS: source shape, local trust observations, destination
        // authority and tombstone state are all read again after the immediate
        // transaction has excluded competing writers.
        for snapshot in snapshots {
            if !Self::source_snapshot_matches_tx(&tx, snapshot)? {
                return Err(DbError::LinkConflict(
                    "rewrite source changed during preflight".into(),
                ));
            }
        }
        for event in events {
            if !event.verify_id()
                || !event.sources_are_canonical()
                || event.source_event_ids.is_empty()
                || event.relation != DerivationRelation::RewriteExact
                || event.evidence.kind != EvidenceKind::LocallyObserved
                || event.origin != DerivationOrigin::LocalHook
            {
                return Err(DbError::InvalidLink("invalid rewrite derivation".into()));
            }
            for source_id in &event.source_event_ids {
                let snapshot = snapshots
                    .iter()
                    .find(|snapshot| source_id_for_snapshot(snapshot).as_deref() == Some(source_id))
                    .ok_or_else(|| DbError::InvalidLink("rewrite source not snapshotted".into()))?;
                let owned = if let Some((interaction, _)) = parse_legacy_source(source_id) {
                    interaction == event.interaction_id.to_string()
                        && snapshot_interaction(snapshot).as_deref() == Some(interaction.as_str())
                } else if is_event_source_id(source_id) {
                    matches!(snapshot, SourceSnapshot::Event { event_id, .. } if event_id == source_id)
                        && tx
                            .query_row(
                                "SELECT interaction_id FROM derivation_events WHERE event_id=?1",
                                params![source_id],
                                |row| row.get::<_, String>(0),
                            )
                            .optional()?
                            .as_deref()
                            == Some(event.interaction_id.to_string().as_str())
                } else {
                    false
                };
                if !owned {
                    return Err(DbError::InvalidLink(
                        "rewrite source ownership mismatch".into(),
                    ));
                }
            }
            let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM interactions WHERE id=?1) AND NOT EXISTS(SELECT 1 FROM tombstones WHERE interaction_id=?1)", params![event.interaction_id.to_string()], |r|r.get(0))?;
            if !exists {
                return Err(DbError::InvalidLink(
                    "rewrite source interaction missing or tombstoned".into(),
                ));
            }
            let payload =
                serde_json::to_string(event).map_err(|e| DbError::Migration(e.to_string()))?;
            match tx.execute("INSERT INTO derivation_events(event_id,interaction_id,target_commit,relation,evidence_version,evidence_kind,origin,source_event_ids,old_oid,new_oid,range_id,linked_by,payload) VALUES(?1,?2,?3,'rewrite_exact',?4,'locally_observed','local_hook',?5,?6,?7,NULL,?8,?9)", params![event.event_id,event.interaction_id.to_string(),event.target_commit.as_str(),event.evidence.version,serde_json::to_string(&event.source_event_ids).map_err(|e| DbError::Migration(e.to_string()))?,event.old_oid.as_ref().map(CommitSha::as_str),event.new_oid.as_ref().map(CommitSha::as_str),event.linked_by,payload]) {
                Ok(_) => {}, Err(rusqlite::Error::SqliteFailure(e,_)) if is_unique_constraint(&e) => { let old:String=tx.query_row("SELECT payload FROM derivation_events WHERE event_id=?1",params![event.event_id],|r|r.get(0))?; if old != payload{return Err(DbError::LinkConflict("rewrite event conflict".into()));} }, Err(e)=>return Err(e.into()) }
            tx.execute("INSERT OR IGNORE INTO derivation_observations(event_id,source_fingerprint,source_key,origin,trusted_local) VALUES(?1,NULL,'local:hook','local_hook',1)", params![event.event_id])?;
            // Destination authority is inherited from the exact source rows
            // captured by preflight, never inferred from the interaction alone.
            for source in snapshots.iter().filter(|snapshot| match snapshot {
                SourceSnapshot::Legacy {
                    interaction,
                    commit,
                    ..
                } => event
                    .source_event_ids
                    .iter()
                    .any(|id| id == &format!("legacy:{interaction}:{commit}")),
                SourceSnapshot::Event { event_id, .. } => {
                    event.source_event_ids.iter().any(|id| id == event_id)
                }
            }) {
                let rows = match source {
                    SourceSnapshot::Legacy {
                        authorization_rows, ..
                    }
                    | SourceSnapshot::Event {
                        authorization_rows, ..
                    } => authorization_rows,
                };
                for row in rows {
                    tx.execute(
                        "INSERT OR IGNORE INTO derivation_authorizations(event_id,remote_fingerprint,source_event_id) VALUES(?1,?2,?3)",
                        params![event.event_id, row.remote_fingerprint, row.source_event_id],
                    )?;
                }
            }
        }
        tx.execute(
            "INSERT INTO rewrite_batches(batch_id,mode,payload_hash) VALUES(?1,?2,?3)",
            params![batch_id, mode, payload_hash],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn source_snapshot_matches_tx(tx: &Transaction<'_>, snapshot: &SourceSnapshot) -> Result<bool> {
        Ok(Self::source_snapshots_tx(
            tx,
            match snapshot {
                SourceSnapshot::Legacy {
                    interaction,
                    commit,
                    ..
                } => Some((interaction, commit)),
                SourceSnapshot::Event { event_id, .. } => {
                    // Event comparisons below do not need an endpoint filter.
                    let expected = Self::event_snapshot_tx(tx, event_id)?;
                    return Ok(expected.as_ref() == Some(snapshot));
                }
            },
        )?
        .iter()
        .any(|x| x == snapshot))
    }

    /// Capture the complete source set used by a rewrite preflight. Ordering is
    /// canonical so changing SQL iteration order cannot create false CAS hits.
    pub(crate) fn rewrite_source_snapshots(
        &self,
        id: &InteractionId,
        commit: &CommitSha,
    ) -> Result<Vec<SourceSnapshot>> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let id_text = id.to_string();
        let result = Self::source_snapshots_tx(&tx, Some((&id_text, commit.as_str())))?;
        tx.commit()?;
        Ok(result)
    }

    /// Persist one immutable range and the exact mutable observations captured
    /// with it. A remote authorization is explicit and destination-scoped.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn observe_range(
        &self,
        range: &RangeEvidence,
        snapshots: &[crate::RangeSourceSnapshot],
        origin: crate::RangeObservationOrigin,
        source_key: &str,
        source_ref: Option<&str>,
        source_remote: Option<&str>,
        authorized_remote: Option<&str>,
    ) -> Result<()> {
        if !range.verify_id() || source_key.is_empty() || source_key.len() > 1024 {
            return Err(DbError::InvalidLink("invalid range evidence".into()));
        }
        let payload =
            serde_json::to_string(range).map_err(|e| DbError::Migration(e.to_string()))?;
        let digest = hex::encode(sha2::Sha256::digest(payload.as_bytes()));
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        match tx.execute("INSERT INTO range_evidence(range_id,payload,payload_digest,changeset_algorithm,changeset_digest) VALUES(?1,?2,?3,?4,?5)",params![range.range_id,payload,digest,range.changeset_algorithm,range.changeset_digest]) {
            Ok(_)=>{}, Err(rusqlite::Error::SqliteFailure(e,_)) if is_unique_constraint(&e)=>{
                let old:(String,String)=tx.query_row("SELECT payload,payload_digest FROM range_evidence WHERE range_id=?1",params![range.range_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
                if old!=(payload.clone(),digest.clone()){return Err(DbError::LinkConflict("range identity conflict".into()));}
            }, Err(e)=>return Err(e.into())
        }
        tx.execute("INSERT INTO range_observations(range_id,source_key,origin,trusted_local,source_ref,source_remote) VALUES(?1,?2,?3,1,?4,?5) ON CONFLICT(range_id,source_key) DO UPDATE SET source_ref=excluded.source_ref,source_remote=excluded.source_remote WHERE origin=excluded.origin AND trusted_local=1",params![range.range_id,source_key,match origin { crate::RangeObservationOrigin::Explicit=>"explicit",crate::RangeObservationOrigin::PrePush=>"pre_push" },source_ref,source_remote])?;
        for source in snapshots {
            if snapshot_interaction(&source.snapshot).as_deref()
                != Some(&source.interaction_id.to_string())
                || source_id_for_snapshot(&source.snapshot).as_deref()
                    != Some(source.source_id.as_str())
            {
                return Err(DbError::InvalidLink(
                    "range source ownership mismatch".into(),
                ));
            }
            let encoded = serde_json::to_string(&source.snapshot)
                .map_err(|e| DbError::Migration(e.to_string()))?;
            match tx.execute(
                "INSERT INTO range_interaction_sources(range_id,interaction_id,source_id,snapshot) VALUES(?1,?2,?3,?4)",
                params![range.range_id, source.interaction_id.to_string(), source.source_id, encoded],
            ) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(e, _)) if is_unique_constraint(&e) => {
                    let old:String=tx.query_row("SELECT snapshot FROM range_interaction_sources WHERE range_id=?1 AND interaction_id=?2 AND source_id=?3",params![range.range_id,source.interaction_id.to_string(),source.source_id],|r|r.get(0))?;
                    if old != encoded {
                        return Err(DbError::LinkConflict(
                            "range source snapshot conflict".into(),
                        ));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(remote) = authorized_remote {
            tx.execute("INSERT OR IGNORE INTO range_authorizations(range_id,remote_fingerprint) VALUES(?1,?2)",params![range.range_id,remote])?;
        }
        tx.commit()?;
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn trusted_ranges_for_changeset(
        &self,
        algorithm: &str,
        digest: &str,
    ) -> Result<Vec<(RangeEvidence, Vec<crate::RangeSourceSnapshot>)>> {
        let mut stmt=self.conn.prepare("SELECT r.payload FROM range_evidence r WHERE r.changeset_algorithm=?1 AND r.changeset_digest=?2 AND EXISTS(SELECT 1 FROM range_observations o WHERE o.range_id=r.range_id AND o.trusted_local=1) ORDER BY r.range_id")?;
        let payloads = stmt
            .query_map(params![algorithm, digest], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for payload in payloads {
            let range: RangeEvidence =
                serde_json::from_str(&payload).map_err(|e| DbError::Migration(e.to_string()))?;
            if !range.verify_id() {
                return Err(DbError::LinkConflict("stored range hash mismatch".into()));
            }
            let mut s=self.conn.prepare("SELECT interaction_id,source_id,snapshot FROM range_interaction_sources WHERE range_id=?1 ORDER BY interaction_id,source_id")?;
            let snapshots = s
                .query_map(params![range.range_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .map(|(interaction, id, p)| {
                    serde_json::from_str(&p)
                        .and_then(|snapshot| {
                            interaction
                                .parse()
                                .map(|interaction_id| crate::RangeSourceSnapshot {
                                    interaction_id,
                                    source_id: id,
                                    snapshot,
                                })
                                .map_err(serde::de::Error::custom)
                        })
                        .map_err(|e| DbError::Migration(e.to_string()))
                })
                .collect::<Result<Vec<_>>>()?;
            out.push((range, snapshots));
        }
        Ok(out)
    }

    pub fn projection_ranges(&self, remote: &str) -> Result<Vec<RangeEvidence>> {
        let mut s=self.conn.prepare("SELECT r.payload FROM range_evidence r WHERE EXISTS(SELECT 1 FROM range_observations o WHERE o.range_id=r.range_id AND o.trusted_local=1) AND EXISTS(SELECT 1 FROM derivation_events e JOIN derivation_observations eo ON eo.event_id=e.event_id AND eo.trusted_local=1 JOIN derivation_authorizations a ON a.event_id=e.event_id WHERE e.range_id=r.range_id AND a.remote_fingerprint=?1) ORDER BY r.range_id")?;
        let result = s
            .query_map(params![remote], |r| r.get::<_, String>(0))?
            .map(|r| {
                serde_json::from_str(&r?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        result
    }

    pub(crate) fn import_ranges_tx(
        tx: &Transaction<'_>,
        ranges: &[RangeEvidence],
        remote: Option<&str>,
    ) -> Result<()> {
        for range in ranges {
            if !range.verify_id() {
                return Err(DbError::InvalidLink("invalid imported range".into()));
            }
            let payload =
                serde_json::to_string(range).map_err(|e| DbError::Migration(e.to_string()))?;
            let digest = hex::encode(sha2::Sha256::digest(payload.as_bytes()));
            match tx.execute("INSERT INTO range_evidence(range_id,payload,payload_digest,changeset_algorithm,changeset_digest) VALUES(?1,?2,?3,?4,?5)",params![range.range_id,payload,digest,range.changeset_algorithm,range.changeset_digest]) {Ok(_)=>{},Err(rusqlite::Error::SqliteFailure(e,_)) if is_unique_constraint(&e)=>{let old:String=tx.query_row("SELECT payload FROM range_evidence WHERE range_id=?1",params![range.range_id],|r|r.get(0))?;if old!=payload{return Err(DbError::LinkConflict("imported range conflict".into()));}},Err(e)=>return Err(e.into())}
            let key = remote.unwrap_or("");
            tx.execute("INSERT OR IGNORE INTO range_observations(range_id,source_key,origin,trusted_local,source_ref,source_remote) VALUES(?1,?2,'remote_sync',0,NULL,?2)",params![range.range_id,key])?;
        }
        Ok(())
    }

    pub fn scan_cursor(&self, worktree: &str, symbolic_ref: &str) -> Result<Option<String>> {
        self.conn.query_row("SELECT last_tip FROM branch_scan_cursors WHERE worktree_key=?1 AND symbolic_ref=?2",params![worktree,symbolic_ref],|r|r.get(0)).optional().map_err(Into::into)
    }

    pub fn discover_squash_targets(
        &self,
        worktree: &str,
        symbolic_ref: &str,
        expected_cursor: Option<&str>,
        new_cursor: &str,
        targets: &[(String, String, bool)],
    ) -> Result<bool> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let old:Option<String>=tx.query_row("SELECT last_tip FROM branch_scan_cursors WHERE worktree_key=?1 AND symbolic_ref=?2",params![worktree,symbolic_ref],|r|r.get(0)).optional()?;
        if old.as_deref() != expected_cursor {
            return Ok(false);
        }
        let unresolved:i64=tx.query_row("SELECT COUNT(*) FROM pending_squash_targets WHERE worktree_key=?1 AND symbolic_ref=?2 AND status='pending'",params![worktree,symbolic_ref],|r|r.get(0))?;
        let mut genuinely_new = 0i64;
        for (target, _, _) in targets {
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM pending_squash_targets WHERE worktree_key=?1 AND symbolic_ref=?2 AND target_commit=?3)",params![worktree,symbolic_ref,target],|r|r.get(0))?;
            if !exists {
                genuinely_new += 1;
            }
        }
        if unresolved.saturating_add(genuinely_new) > MAX_PENDING_SQUASH_TARGETS {
            return Err(DbError::SquashQueueCapacity {
                limit: MAX_PENDING_SQUASH_TARGETS,
            });
        }
        let mut order:i64=tx.query_row("SELECT COALESCE(MAX(discovered_order),0) FROM pending_squash_targets WHERE worktree_key=?1 AND symbolic_ref=?2",params![worktree,symbolic_ref],|r|r.get(0))?;
        for (target, parent, supported) in targets {
            order += 1;
            tx.execute("INSERT OR IGNORE INTO pending_squash_targets(worktree_key,symbolic_ref,target_commit,parent_commit,status,discovered_order) VALUES(?1,?2,?3,?4,?5,?6)",params![worktree,symbolic_ref,target,parent,if *supported{"pending"}else{"unsupported"},order])?;
        }
        tx.execute("INSERT INTO branch_scan_cursors(worktree_key,symbolic_ref,last_tip) VALUES(?1,?2,?3) ON CONFLICT(worktree_key,symbolic_ref) DO UPDATE SET last_tip=excluded.last_tip",params![worktree,symbolic_ref,new_cursor])?;
        tx.execute("DELETE FROM pending_squash_targets WHERE rowid IN (SELECT rowid FROM pending_squash_targets WHERE worktree_key=?1 AND symbolic_ref=?2 AND status!='pending' ORDER BY discovered_order DESC LIMIT -1 OFFSET ?3)",params![worktree,symbolic_ref,MAX_RESOLVED_SQUASH_TARGETS])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn pending_squash_targets(
        &self,
        worktree: &str,
        symbolic_ref: &str,
    ) -> Result<Vec<(String, String)>> {
        let mut s=self.conn.prepare("SELECT target_commit,parent_commit FROM pending_squash_targets WHERE worktree_key=?1 AND symbolic_ref=?2 AND status='pending' ORDER BY last_attempt_seq ASC,discovered_order ASC LIMIT 128")?;
        let result = s
            .query_map(params![worktree, symbolic_ref], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<std::result::Result<_, _>>()
            .map_err(Into::into);
        result
    }

    pub fn mark_squash_attempt(
        &self,
        worktree: &str,
        symbolic_ref: &str,
        target: &str,
    ) -> Result<()> {
        self.conn.execute("UPDATE pending_squash_targets SET attempt_count=attempt_count+1,last_attempt_seq=(SELECT COALESCE(MAX(last_attempt_seq),0)+1 FROM pending_squash_targets WHERE worktree_key=?1 AND symbolic_ref=?2) WHERE worktree_key=?1 AND symbolic_ref=?2 AND target_commit=?3 AND status='pending'",params![worktree,symbolic_ref,target])?;
        Ok(())
    }

    /// Atomic squash write with range/source/cursor CAS. The callback rechecks
    /// Git HEAD after BEGIN IMMEDIATE has excluded competing DB decisions.
    pub(crate) fn apply_squash_plan(
        &mut self,
        repo: &git2::Repository,
        plan: &crate::squash::ValidatedSquashPlan,
    ) -> Result<usize> {
        let view = plan.db_view();
        let worktree = view.worktree;
        let symbolic_ref = view.symbolic_ref;
        let expected_cursor = Some(view.expected_cursor);
        let candidate = view.candidate;
        let expected_parent = view.expected_parent;
        let expected_head = view.expected_head;
        let range = view.range;
        let snapshots = view.sources;
        let events = view.events;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cursor:Option<String>=tx.query_row("SELECT last_tip FROM branch_scan_cursors WHERE worktree_key=?1 AND symbolic_ref=?2",params![worktree,symbolic_ref],|r|r.get(0)).optional()?;
        let git_valid = repo.head().ok().is_some_and(|head| {
            head.name() == Some(symbolic_ref)
                && head
                    .peel_to_commit()
                    .ok()
                    .is_some_and(|commit| commit.id().to_string() == expected_head)
        }) && git2::Oid::from_str(candidate)
            .ok()
            .and_then(|oid| repo.find_commit(oid).ok())
            .is_some_and(|commit| {
                commit.parent_count() == 1
                    && commit
                        .parent_id(0)
                        .ok()
                        .is_some_and(|parent| parent.to_string() == expected_parent)
            });
        if cursor.as_deref() != expected_cursor || !git_valid {
            return Err(DbError::LinkConflict(
                "squash cursor or HEAD changed".into(),
            ));
        }
        let payload:String=tx.query_row("SELECT payload FROM range_evidence WHERE range_id=?1 AND EXISTS(SELECT 1 FROM range_observations WHERE range_id=?1 AND trusted_local=1)",params![range.range_id],|r|r.get(0))?;
        if payload != serde_json::to_string(range).map_err(|e| DbError::Migration(e.to_string()))? {
            return Err(DbError::LinkConflict("squash range changed".into()));
        }
        let persisted:Vec<(String,String,String)>=tx.prepare("SELECT interaction_id,source_id,snapshot FROM range_interaction_sources WHERE range_id=?1 ORDER BY interaction_id,source_id")?.query_map(params![range.range_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<std::result::Result<_,_>>()?;
        let supplied_rows: Vec<(String, String, String)> = snapshots
            .iter()
            .map(|source| {
                Ok((
                    source.interaction_id.to_string(),
                    source.source_id.clone(),
                    serde_json::to_string(&source.snapshot)
                        .map_err(|e| DbError::Migration(e.to_string()))?,
                ))
            })
            .collect::<Result<_>>()?;
        if persisted != supplied_rows {
            return Err(DbError::LinkConflict(
                "complete squash source set changed".into(),
            ));
        }
        for source in snapshots {
            if snapshot_interaction(&source.snapshot).as_deref()
                != Some(&source.interaction_id.to_string())
                || source_id_for_snapshot(&source.snapshot).as_deref()
                    != Some(source.source_id.as_str())
            {
                return Err(DbError::InvalidLink(
                    "squash source ownership mismatch".into(),
                ));
            }
            let actual: String = tx.query_row(
                "SELECT snapshot FROM range_interaction_sources WHERE range_id=?1 AND interaction_id=?2 AND source_id=?3",
                params![range.range_id, source.interaction_id.to_string(),source.source_id],
                |r| r.get(0),
            )?;
            if actual
                != serde_json::to_string(&source.snapshot)
                    .map_err(|e| DbError::Migration(e.to_string()))?
                || !Self::source_snapshot_matches_tx(&tx, &source.snapshot)?
            {
                return Err(DbError::LinkConflict("squash source changed".into()));
            }
        }
        let supplied: std::collections::HashSet<_> = snapshots
            .iter()
            .map(|source| (source.interaction_id.to_string(), source.source_id.clone()))
            .collect();
        let cited: std::collections::HashSet<_> = events
            .iter()
            .flat_map(|event| {
                event
                    .source_event_ids
                    .iter()
                    .map(move |source| (event.interaction_id.to_string(), source.clone()))
            })
            .collect();
        if supplied != cited {
            return Err(DbError::InvalidLink("squash source set mismatch".into()));
        }
        let mut inserted = 0;
        for event in events {
            if !event.verify_id()
                || event.relation != DerivationRelation::SquashExact
                || event.target_commit.as_str() != candidate
                || event.evidence.version != 1
                || event.evidence.kind != EvidenceKind::LocallyObserved
                || event.origin != DerivationOrigin::LocalHook
                || event.old_oid.is_some()
                || event.new_oid.is_some()
                || event.range_id.as_deref() != Some(&range.range_id)
                || !event.sources_are_canonical()
            {
                return Err(DbError::InvalidLink("invalid squash event".into()));
            }
            let alive:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM interactions WHERE id=?1) AND NOT EXISTS(SELECT 1 FROM tombstones WHERE interaction_id=?1)",params![event.interaction_id.to_string()],|r|r.get(0))?;
            if !alive {
                return Err(DbError::LinkConflict("squash source tombstoned".into()));
            }
            let body =
                serde_json::to_string(event).map_err(|e| DbError::Migration(e.to_string()))?;
            match tx.execute("INSERT INTO derivation_events(event_id,interaction_id,target_commit,relation,evidence_version,evidence_kind,origin,source_event_ids,old_oid,new_oid,range_id,linked_by,payload) VALUES(?1,?2,?3,'squash_exact',1,'locally_observed','local_hook',?4,NULL,NULL,?5,?6,?7)",params![event.event_id,event.interaction_id.to_string(),event.target_commit.as_str(),serde_json::to_string(&event.source_event_ids).unwrap_or_default(),event.range_id,event.linked_by,body]) {Ok(_)=>inserted+=1,Err(rusqlite::Error::SqliteFailure(e,_)) if is_unique_constraint(&e)=>{let old:String=tx.query_row("SELECT payload FROM derivation_events WHERE event_id=?1",params![event.event_id],|r|r.get(0))?;if old!=body{return Err(DbError::LinkConflict("squash event conflict".into()));}},Err(e)=>return Err(e.into())}
            tx.execute("INSERT OR IGNORE INTO derivation_observations(event_id,source_fingerprint,source_key,origin,trusted_local) VALUES(?1,NULL,'local:hook','local_hook',1)",params![event.event_id])?;
            let owned: Vec<_> = snapshots
                .iter()
                .filter(|source| {
                    source.interaction_id == event.interaction_id
                        && event.source_event_ids.contains(&source.source_id)
                })
                .collect();
            if owned.len() != event.source_event_ids.len()
                || owned
                    .iter()
                    .map(|s| s.source_id.as_str())
                    .collect::<Vec<_>>()
                    != event
                        .source_event_ids
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
            {
                return Err(DbError::InvalidLink(
                    "squash source ownership mismatch".into(),
                ));
            }
            for source in owned {
                let auth = match &source.snapshot {
                    SourceSnapshot::Legacy {
                        authorization_rows, ..
                    }
                    | SourceSnapshot::Event {
                        authorization_rows, ..
                    } => authorization_rows,
                };
                for row in auth {
                    tx.execute("INSERT OR IGNORE INTO derivation_authorizations(event_id,remote_fingerprint,source_event_id) VALUES(?1,?2,?3)",params![event.event_id,row.remote_fingerprint,row.source_event_id])?;
                }
            }
        }
        let parent:String=tx.query_row("SELECT parent_commit FROM pending_squash_targets WHERE worktree_key=?1 AND symbolic_ref=?2 AND target_commit=?3 AND status='pending'",params![worktree,symbolic_ref,candidate],|r|r.get(0))?;
        if parent != expected_parent {
            return Err(DbError::LinkConflict(
                "squash topology snapshot changed".into(),
            ));
        }
        tx.execute("UPDATE pending_squash_targets SET status='matched' WHERE worktree_key=?1 AND symbolic_ref=?2 AND target_commit=?3 AND status='pending'",params![worktree,symbolic_ref,candidate])?;
        tx.execute("DELETE FROM pending_squash_targets WHERE rowid IN (SELECT rowid FROM pending_squash_targets WHERE worktree_key=?1 AND symbolic_ref=?2 AND status!='pending' ORDER BY discovered_order DESC LIMIT -1 OFFSET ?3)",params![worktree,symbolic_ref,MAX_RESOLVED_SQUASH_TARGETS])?;
        tx.commit()?;
        Ok(inserted)
    }

    pub fn update_scan_cursor(
        &self,
        worktree: &str,
        symbolic_ref: &str,
        expected: Option<&str>,
        tip: &str,
    ) -> Result<bool> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let old:Option<String>=tx.query_row("SELECT last_tip FROM branch_scan_cursors WHERE worktree_key=?1 AND symbolic_ref=?2",params![worktree,symbolic_ref],|r|r.get(0)).optional()?;
        if old.as_deref() != expected {
            return Ok(false);
        };
        tx.execute("INSERT INTO branch_scan_cursors(worktree_key,symbolic_ref,last_tip) VALUES(?1,?2,?3) ON CONFLICT(worktree_key,symbolic_ref) DO UPDATE SET last_tip=excluded.last_tip",params![worktree,symbolic_ref,tip])?;
        tx.commit()?;
        Ok(true)
    }

    fn source_snapshots_tx(
        tx: &Transaction<'_>,
        endpoint: Option<(&str, &str)>,
    ) -> Result<Vec<SourceSnapshot>> {
        let Some((id, commit)) = endpoint else {
            return Ok(Vec::new());
        };
        let legacy_key = format!("legacy:{id}:{commit}");
        let mut auth: Vec<SourceAuthorizationRow> = tx.prepare("SELECT remote_fingerprint FROM interaction_shares WHERE interaction_id=?1 ORDER BY remote_fingerprint")?.query_map(params![id], |r| Ok(SourceAuthorizationRow { remote_fingerprint: r.get(0)?, source_event_id: legacy_key.clone() }))?.collect::<std::result::Result<_,_>>()?;
        auth.sort();
        auth.dedup();
        let mut out = Vec::new();
        let mut s = tx.prepare("SELECT link_type,linked_by FROM artifact_links WHERE interaction_id=?1 AND git_commit_hash=?2 ORDER BY link_type,COALESCE(linked_by,'')")?;
        for row in s.query_map(params![id, commit], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })? {
            let (link_type, linked_by) = row?;
            let key = legacy_key.clone();
            let trusted: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM derivation_observations WHERE event_id=?1 AND trusted_local=1)",params![key],|r|r.get(0))?;
            let alive: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM interactions WHERE id=?1) AND NOT EXISTS(SELECT 1 FROM tombstones WHERE interaction_id=?1)",params![id],|r|r.get(0))?;
            if trusted && alive {
                out.push(SourceSnapshot::Legacy {
                    interaction: id.into(),
                    commit: commit.into(),
                    link_type,
                    linked_by,
                    authorization_rows: auth.clone(),
                });
            }
        }
        let mut events = tx.prepare("SELECT event_id FROM derivation_events WHERE interaction_id=?1 AND target_commit=?2 ORDER BY event_id")?;
        for event_id in events.query_map(params![id, commit], |r| r.get::<_, String>(0))? {
            if let Some(snapshot) = Self::event_snapshot_tx(tx, &event_id?)? {
                if matches!(&snapshot, SourceSnapshot::Event { trusted_local_observations, .. } if !trusted_local_observations.is_empty())
                {
                    out.push(snapshot);
                }
            }
        }
        out.sort_by_key(|v| serde_json::to_string(v).unwrap_or_default());
        Ok(out)
    }

    fn event_snapshot_tx(tx: &Transaction<'_>, event_id: &str) -> Result<Option<SourceSnapshot>> {
        let payload: Option<String> = tx
            .query_row(
                "SELECT payload FROM derivation_events WHERE event_id=?1",
                params![event_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let digest = format!("{:x}", sha2::Sha256::digest(payload.as_bytes()));
        let mut observations: Vec<SourceObservationRow> = tx.prepare("SELECT source_fingerprint,source_key,origin,trusted_local FROM derivation_observations WHERE event_id=?1 AND trusted_local=1 ORDER BY source_key")?.query_map(params![event_id],|r|Ok(SourceObservationRow { source_fingerprint:r.get(0)?, source_key:r.get(1)?, origin:r.get(2)?, trusted_local:r.get::<_,i64>(3)? != 0 }))?.collect::<std::result::Result<_,_>>()?;
        observations.sort();
        observations.dedup();
        let mut auth: Vec<SourceAuthorizationRow> = tx.prepare("SELECT remote_fingerprint,source_event_id FROM derivation_authorizations WHERE event_id=?1 ORDER BY remote_fingerprint,source_event_id")?.query_map(params![event_id],|r|Ok(SourceAuthorizationRow { remote_fingerprint:r.get(0)?, source_event_id:r.get(1)? }))?.collect::<std::result::Result<_,_>>()?;
        auth.sort();
        auth.dedup();
        Ok(Some(SourceSnapshot::Event {
            event_id: event_id.into(),
            canonical_payload: payload,
            canonical_payload_digest: digest,
            trusted_local_observations: observations,
            authorization_rows: auth,
        }))
    }

    /// Add an immutable artifact link. `linked_by` records the author identity
    /// from the commit which triggered an automatic link, when available.
    pub fn link_interaction_with_metadata(
        &self,
        interaction_id: &InteractionId,
        commit_sha: &CommitSha,
        link_type: &str,
        linked_by: Option<&str>,
    ) -> Result<()> {
        if !matches!(link_type, "generated" | "temporal")
            || !is_safe_commit_sha(commit_sha.as_str())
        {
            return Err(DbError::InvalidLink("invalid automatic link".into()));
        }
        let linked_by = linked_by
            .map(privacy::scrub)
            .transpose()
            .map_err(|e| DbError::InvalidLink(e.to_string()))?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO artifact_links (interaction_id, git_commit_hash, link_type, linked_by) VALUES (?1, ?2, ?3, ?4)",
            params![
                interaction_id.to_string(),
                commit_sha.as_str(),
                link_type,
                linked_by,
            ]
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically create automatic links selected by the linker. Conflicts do
    /// not count as inserts; an existing row only receives attribution when it
    /// has the same type and currently lacks it.
    pub fn link_automatic_interactions(
        &self,
        interaction_ids: &[InteractionId],
        commit_sha: &CommitSha,
        link_type: &str,
        linked_by: Option<&str>,
    ) -> Result<usize> {
        let links: Vec<(&InteractionId, &str)> = interaction_ids
            .iter()
            .map(|interaction_id| (interaction_id, link_type))
            .collect();
        self.link_automatic_interaction_batch(&links, commit_sha, linked_by)
    }

    /// Atomically persist a complete linker decision, including generated and
    /// temporal rows, so an error never leaves a conversation only half bound.
    pub fn link_automatic_interaction_batch(
        &self,
        links: &[(&InteractionId, &str)],
        commit_sha: &CommitSha,
        linked_by: Option<&str>,
    ) -> Result<usize> {
        self.link_automatic_interaction_batch_inner(links, commit_sha, linked_by, false)
    }

    pub(crate) fn link_automatic_interaction_batch_trusted(
        &self,
        links: &[(&InteractionId, &str)],
        commit_sha: &CommitSha,
        linked_by: Option<&str>,
    ) -> Result<usize> {
        self.link_automatic_interaction_batch_inner(links, commit_sha, linked_by, true)
    }

    fn link_automatic_interaction_batch_inner(
        &self,
        links: &[(&InteractionId, &str)],
        commit_sha: &CommitSha,
        linked_by: Option<&str>,
        trusted_local: bool,
    ) -> Result<usize> {
        if let Some((_, link_type)) = links
            .iter()
            .find(|(_, link_type)| !matches!(*link_type, "generated" | "temporal"))
        {
            return Err(DbError::InvalidLink((*link_type).to_owned()));
        }
        if !is_safe_commit_sha(commit_sha.as_str()) {
            return Err(DbError::InvalidLink("invalid commit SHA".into()));
        }
        let linked_by = linked_by
            .map(privacy::scrub)
            .transpose()
            .map_err(|e| DbError::InvalidLink(e.to_string()))?;
        let linked_by = linked_by.as_deref();

        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut inserts = 0;
        for (interaction_id, link_type) in links {
            match transaction.execute(
                "INSERT INTO artifact_links (interaction_id, git_commit_hash, link_type, linked_by)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    interaction_id.to_string(),
                    commit_sha.as_str(),
                    link_type,
                    linked_by
                ],
            ) {
                Ok(_) => inserts += 1,
                Err(rusqlite::Error::SqliteFailure(error, _)) if is_unique_constraint(&error) => {
                    let existing: (String, Option<String>) = transaction.query_row(
                        "SELECT link_type, linked_by FROM artifact_links
                         WHERE interaction_id = ?1 AND git_commit_hash = ?2",
                        params![interaction_id.to_string(), commit_sha.as_str()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    if existing.0 != *link_type {
                        return Err(DbError::LinkConflict(format!(
                            "link type differs for {}@{}",
                            interaction_id,
                            commit_sha.as_str()
                        )));
                    }
                    match (existing.1.as_deref(), linked_by) {
                        (Some(current), Some(incoming)) if current != incoming => {
                            return Err(DbError::LinkConflict(format!(
                                "linked_by differs for {}@{}",
                                interaction_id,
                                commit_sha.as_str()
                            )));
                        }
                        (None, Some(_)) => {
                            transaction.execute(
                                "UPDATE artifact_links SET linked_by = ?4
                             WHERE interaction_id = ?1 AND git_commit_hash = ?2
                               AND link_type = ?3 AND linked_by IS NULL",
                                params![
                                    interaction_id.to_string(),
                                    commit_sha.as_str(),
                                    link_type,
                                    linked_by
                                ],
                            )?;
                        }
                        // Missing incoming attribution makes no claim and cannot
                        // downgrade a known value; exact duplicates are no-ops.
                        _ => {}
                    }
                }
                Err(error) => return Err(error.into()),
            }
            if trusted_local {
                let key = format!("legacy:{}:{}", interaction_id, commit_sha.as_str());
                transaction.execute("INSERT OR IGNORE INTO derivation_observations(event_id,source_fingerprint,source_key,origin,trusted_local) VALUES(?1,NULL,?1,'local_api',1)", params![key])?;
            }
        }
        transaction.commit()?;
        Ok(inserts)
    }

    /// Per-conversation activity and, when a destination fingerprint is given,
    /// its share/publication state — the read-only view behind
    /// `cvc conversations` and the interactive share picker. Most recent
    /// activity first.
    pub fn list_conversation_summaries(
        &self,
        destination_fingerprint: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>> {
        let mut stmt = self.conn.prepare(&conversation_summary_sql(
            "GROUP BY c.id, c.title
             ORDER BY COALESCE(MAX(i.timestamp), c.created_at) DESC
             LIMIT ?2",
        ))?;
        let rows = stmt.query_map(
            params![destination_fingerprint, limit as i64],
            map_summary_row,
        )?;
        let mut summaries = Vec::new();
        for summary in rows {
            summaries.push(summary?);
        }
        Ok(summaries)
    }

    pub fn get_all_interaction_ids(&self) -> Result<Vec<InteractionId>> {
        let mut stmt = self.conn.prepare("SELECT id FROM interactions")?;
        let rows = stmt.query_map([], |row| {
            let s: String = row.get(0)?;
            s.parse().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })?;
        let mut ids = Vec::new();
        for id in rows {
            ids.push(id?);
        }
        Ok(ids)
    }

    /// Destination-scoped sharing.  Visibility is only a local UI property; this
    /// explicit relation is the publication authority.
    pub fn share_conversation_for_remote(
        &self,
        conversation_id: &str,
        remote: &str,
        future: FutureSharePolicy,
    ) -> Result<usize> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let count = tx.execute(
            "UPDATE interactions SET visibility='shared' WHERE conversation_id=?1",
            params![conversation_id],
        )?;
        tx.execute("INSERT OR IGNORE INTO interaction_shares(interaction_id,remote_fingerprint) SELECT id,?2 FROM interactions WHERE conversation_id=?1", params![conversation_id, remote])?;
        tx.execute("INSERT INTO conversation_shares(conversation_id,remote_fingerprint,share_future) VALUES(?1,?2,?3) ON CONFLICT(conversation_id,remote_fingerprint) DO UPDATE SET share_future=excluded.share_future", params![conversation_id, remote, matches!(future, FutureSharePolicy::Shared) as i64])?;
        tx.commit()?;
        Ok(count)
    }
    pub fn share_snapshot(&self, conversation_id: &str) -> Result<Vec<InteractionId>> {
        let mut stmt = self.conn.prepare(SHARE_SNAPSHOT_SQL)?;
        let result = stmt
            .query_map(params![conversation_id], |r| r.get::<_, String>(0))?
            .map(|r| {
                r?.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        result
    }
    pub fn share_exact_snapshot(
        &self,
        conversation_id: &str,
        remote: &str,
        ids: &[InteractionId],
        future: FutureSharePolicy,
    ) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let actual: i64 = tx.query_row(
            "SELECT COUNT(*) FROM interactions WHERE conversation_id=?1",
            params![conversation_id],
            |r| r.get(0),
        )?;
        if actual != ids.len() as i64 {
            return Err(DbError::Migration(
                "conversation changed during authorization; retry".into(),
            ));
        }
        for id in ids {
            let found: i64 = tx.query_row(
                "SELECT COUNT(*) FROM interactions WHERE id=?1 AND conversation_id=?2",
                params![id.to_string(), conversation_id],
                |r| r.get(0),
            )?;
            if found != 1 {
                return Err(DbError::Migration(
                    "conversation changed during authorization; retry".into(),
                ));
            }
            tx.execute("INSERT OR IGNORE INTO interaction_shares(interaction_id,remote_fingerprint) VALUES(?1,?2)", params![id.to_string(),remote])?;
        }
        tx.execute("INSERT INTO conversation_shares(conversation_id,remote_fingerprint,share_future) VALUES(?1,?2,?3) ON CONFLICT(conversation_id,remote_fingerprint) DO UPDATE SET share_future=excluded.share_future", params![conversation_id,remote,matches!(future, FutureSharePolicy::Shared) as i64])?;
        tx.commit()?;
        Ok(())
    }
    pub fn unshare_conversation_for_remote(
        &self,
        conversation_id: &str,
        remote: &str,
    ) -> Result<usize> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if !Self::unshare_blockers(&tx, conversation_id, remote)?.is_allowed() {
            return Err(DbError::Migration(
                "cannot unshare pending, published, or unknown content".into(),
            ));
        }
        let count = tx.execute("DELETE FROM interaction_shares WHERE remote_fingerprint=?2 AND interaction_id IN (SELECT id FROM interactions WHERE conversation_id=?1)", params![conversation_id, remote])?;
        tx.execute("INSERT INTO conversation_shares(conversation_id,remote_fingerprint,share_future) VALUES(?1,?2,0) ON CONFLICT(conversation_id,remote_fingerprint) DO UPDATE SET share_future=0", params![conversation_id, remote])?;
        tx.commit()?;
        Ok(count)
    }
    /// Whether unsharing `conversation_id` for `remote` would be refused, and
    /// why, without changing anything. It is the very rule
    /// [`Self::unshare_conversation_for_remote`] enforces, so a view can say
    /// that unsharing is impossible before asking anyone for a phrase.
    pub fn can_unshare(&self, conversation_id: &str, remote: &str) -> Result<UnshareCheck> {
        Self::unshare_blockers(&self.conn, conversation_id, remote)
    }

    /// The one definition of what blocks an unshare: any interaction of the
    /// conversation with a `pending`, `published` or `unknown` publication for
    /// the destination. Used by the refusal and by [`Self::can_unshare`].
    fn unshare_blockers(
        conn: &Connection,
        conversation_id: &str,
        remote: &str,
    ) -> Result<UnshareCheck> {
        let mut stmt = conn.prepare("SELECT p.state, COUNT(*) FROM publications p JOIN interactions i ON i.id=p.interaction_id WHERE i.conversation_id=?1 AND p.remote_fingerprint=?2 AND p.state IN ('pending','published','unknown') GROUP BY p.state")?;
        let rows = stmt.query_map(params![conversation_id, remote], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut check = UnshareCheck::default();
        for row in rows {
            let (state, count) = row?;
            match state.as_str() {
                "published" => check.published = count,
                "pending" => check.pending = count,
                _ => check.unknown = count,
            }
        }
        Ok(check)
    }
    /// How much of what is shared for `destination_fingerprint` is known to be
    /// published. See [`DestinationPublicationCounts`] for what each number
    /// means; "shared" is the same test the summaries use.
    pub fn destination_publication_counts(
        &self,
        destination_fingerprint: &str,
    ) -> Result<DestinationPublicationCounts> {
        let sql = format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(EXISTS(SELECT 1 FROM publications p WHERE p.interaction_id = i.id AND p.remote_fingerprint = ?1 AND p.state = 'published')), 0),
                    COALESCE(SUM(EXISTS(SELECT 1 FROM publications p WHERE p.interaction_id = i.id AND p.remote_fingerprint = ?1 AND p.state IN ('pending', 'unknown'))), 0)
             FROM interactions i
             WHERE {SHARED_FOR_DESTINATION}"
        );
        Ok(self
            .conn
            .query_row(&sql, params![destination_fingerprint], |row| {
                Ok(DestinationPublicationCounts {
                    shared: row.get(0)?,
                    published: row.get(1)?,
                    unconfirmed: row.get(2)?,
                })
            })?)
    }

    /// Everything a view shows about one conversation for one destination, or
    /// `None` when there is no such conversation. All of it is read from one
    /// consistent snapshot, so the counts agree with each other even while a
    /// hook captures into the same store.
    pub fn conversation_details(
        &self,
        conversation_id: &str,
        destination_fingerprint: Option<&str>,
    ) -> Result<Option<ConversationDetails>> {
        // Dropped without committing: it only pins the snapshot for the reads.
        let snapshot = Transaction::new_unchecked(&self.conn, TransactionBehavior::Deferred)?;
        let Some(summary) = snapshot
            .query_row(
                &conversation_summary_sql("WHERE c.id = ?2 GROUP BY c.id, c.title"),
                params![destination_fingerprint, conversation_id],
                map_summary_row,
            )
            .optional()?
        else {
            return Ok(None);
        };
        let context_paths = snapshot
            .prepare(&format!(
                "SELECT x.file_path, COUNT(DISTINCT x.interaction_id) AS uses {CONVERSATION_CONTEXT_PATHS} GROUP BY x.file_path ORDER BY uses DESC, x.file_path LIMIT ?2"
            ))?
            .query_map(
                params![conversation_id, CONVERSATION_DETAIL_PATH_LIMIT as i64],
                |row| {
                    Ok(ContextPathUse {
                        path: row.get(0)?,
                        interactions: row.get(1)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let context_path_total: i64 = snapshot.query_row(
            &format!("SELECT COUNT(DISTINCT x.file_path) {CONVERSATION_CONTEXT_PATHS}"),
            params![conversation_id],
            |row| row.get(0),
        )?;
        let snapshot_count: i64 = snapshot.query_row(
            &format!("SELECT COUNT(*) FROM ({SHARE_SNAPSHOT_SQL})"),
            params![conversation_id],
            |row| row.get(0),
        )?;
        let snapshot_count = snapshot_count as usize;
        // The shared interactions of a conversation are among the ones a share
        // would cover, so the breakdown cannot go negative.
        let snapshot_shared = (summary.shared_count as usize).min(snapshot_count);
        Ok(Some(ConversationDetails {
            summary,
            context_paths,
            context_path_total: context_path_total as usize,
            snapshot_count,
            snapshot_shared,
            snapshot_new: snapshot_count - snapshot_shared,
        }))
    }

    pub fn projection_interaction_ids(&self, remote: &str) -> Result<Vec<InteractionId>> {
        let mut stmt = self.conn.prepare("SELECT i.id FROM interactions i LEFT JOIN publications p ON p.interaction_id=i.id AND p.remote_fingerprint=?1 WHERE EXISTS (SELECT 1 FROM interaction_shares s WHERE s.interaction_id=i.id AND s.remote_fingerprint IN (?1,'*')) AND (p.state IS NULL OR p.state IN ('pending','published'))")?;
        let result = stmt
            .query_map(params![remote], |r| r.get::<_, String>(0))?
            .map(|r| {
                r?.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        result
    }
    /// Records publication progress for `ids` at one destination. `Published` is
    /// terminal here: a row already `published` is never moved back to `pending` or
    /// `unknown` (its timestamp is also kept when `published` is re-asserted).
    pub fn mark_publication(
        &self,
        ids: &[InteractionId],
        remote: &str,
        state: PublicationState,
    ) -> Result<()> {
        let value = match state {
            PublicationState::Pending => "pending",
            PublicationState::Published => "published",
            PublicationState::Unknown => "unknown",
        };
        if ids.is_empty() {
            return Ok(());
        }
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        for id in ids {
            // A row that is `published` for this destination is never demoted: that fact
            // is what blocks `unshare` and authorizes `redact` for content the
            // destination holds, and a failed re-send of an id the remote lost must not
            // make it look as if it was never published. Re-asserting `published` keeps
            // the original timestamp; every genuine state change records a new one.
            tx.execute("INSERT INTO publications(interaction_id,remote_fingerprint,state,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(interaction_id,remote_fingerprint) DO UPDATE SET state=excluded.state,updated_at=CASE WHEN publications.state='published' AND excluded.state='published' THEN publications.updated_at ELSE excluded.updated_at END WHERE publications.state <> 'published' OR excluded.state = 'published'", params![id.to_string(), remote, value, Utc::now().timestamp()])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn uncertain_publication_ids(&self, remote: &str) -> Result<Vec<InteractionId>> {
        let mut stmt = self.conn.prepare("SELECT interaction_id FROM publications WHERE remote_fingerprint=?1 AND state IN ('pending','unknown')")?;
        let result = stmt
            .query_map(params![remote], |r| r.get::<_, String>(0))?
            .map(|r| {
                r?.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        result
    }
    /// Absence is only accepted after the caller successfully fetched/listed the
    /// exact destination baseline.
    pub fn clear_uncertain_publications(&self, ids: &[InteractionId], remote: &str) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        for id in ids {
            tx.execute("DELETE FROM publications WHERE interaction_id=?1 AND remote_fingerprint=?2 AND state IN ('pending','unknown')", params![id.to_string(),remote])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Whether this store keeps rows under the destination fingerprint `legacy`
    /// and none under `fingerprint`, so that
    /// [`adopt_destination_fingerprint`](Self::adopt_destination_fingerprint)
    /// would move them. Read-only.
    pub(crate) fn destination_rows_pending_adoption(
        &self,
        legacy: &str,
        fingerprint: &str,
    ) -> Result<bool> {
        Self::destination_rows_await_adoption(&self.conn, legacy, fingerprint)
    }

    fn destination_rows_await_adoption(
        conn: &Connection,
        legacy: &str,
        fingerprint: &str,
    ) -> Result<bool> {
        let holds = |value: &str| -> Result<bool> {
            Ok(conn.query_row(DESTINATION_ROWS_EXIST, params![value], |row| row.get(0))?)
        };
        Ok(holds(legacy)? && !holds(fingerprint)?)
    }

    /// Moves every row this store keeps under the destination fingerprint
    /// `legacy` to `fingerprint`, in one transaction, when there is at least one
    /// and none is kept under `fingerprint` yet: rows already kept under
    /// `fingerprint` win, and nothing is merged into them. Returns whether rows
    /// were moved.
    ///
    /// The rows are those of every table that records a destination: shares
    /// (`conversation_shares`, `interaction_shares`), publication state
    /// (`publications`), remote tombstones, derivation and range
    /// authorizations, and what a pull observed at the destination
    /// (`derivation_observations` and `range_observations` of origin
    /// `remote_sync`, which key those rows by the fingerprint). The
    /// `remote:<fingerprint>:` namespace of conversation ids imported by a pull
    /// is not touched.
    pub(crate) fn adopt_destination_fingerprint(
        &self,
        legacy: &str,
        fingerprint: &str,
    ) -> Result<bool> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if !Self::destination_rows_await_adoption(&tx, legacy, fingerprint)? {
            return Ok(false);
        }
        for statement in ADOPT_DESTINATION_ROWS {
            tx.execute(statement, params![legacy, fingerprint])?;
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn get_context_items(&self, interaction_id: &InteractionId) -> Result<Vec<ContextItem>> {
        let mut stmt = self.conn.prepare("SELECT id, interaction_id, file_path, git_blob_sha, dirty_patch, start_line, end_line FROM context_items WHERE interaction_id = ?1")?;

        let rows = stmt.query_map(params![interaction_id.to_string()], |row| {
            Ok(ContextItem {
                id: Some(row.get(0)?),
                interaction_id: row.get::<_, String>(1)?.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                file_path: row.get(2)?,
                git_blob_sha: row.get(3)?,
                dirty_patch: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
            })
        })?;
        let mut items = Vec::new();
        for item in rows {
            items.push(item?);
        }
        Ok(items)
    }

    pub fn get_tool_executions(
        &self,
        interaction_id: &InteractionId,
    ) -> Result<Vec<ToolExecution>> {
        let mut stmt = self.conn.prepare("SELECT id, interaction_id, tool_protocol, tool_name, arguments, status FROM tool_executions WHERE interaction_id = ?1")?;

        let rows = stmt.query_map(params![interaction_id.to_string()], |row| {
            let status_str: String = row.get(5)?;
            let status = match status_str.as_str() {
                "success" => ToolStatus::Success,
                _ => ToolStatus::Failure,
            };

            Ok(ToolExecution {
                id: Some(row.get(0)?),
                interaction_id: row.get::<_, String>(1)?.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                tool_protocol: row.get(2)?,
                tool_name: row.get(3)?,
                arguments: row.get(4)?,
                status,
            })
        })?;
        let mut items = Vec::new();
        for item in rows {
            items.push(item?);
        }
        Ok(items)
    }

    pub fn get_artifact_links(&self, interaction_id: &InteractionId) -> Result<Vec<ArtifactLink>> {
        let mut stmt = self.conn.prepare("SELECT interaction_id, git_commit_hash, link_type, linked_by FROM artifact_links WHERE interaction_id = ?1 UNION SELECT interaction_id, target_commit, relation, linked_by FROM derivation_events WHERE interaction_id=?1")?;

        let rows = stmt.query_map(params![interaction_id.to_string()], |row| {
            Ok(ArtifactLink {
                interaction_id: row.get::<_, String>(0)?.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                git_commit_hash: CommitSha::new(row.get::<_, String>(1)?),
                link_type: row.get(2)?,
                linked_by: row.get(3)?,
            })
        })?;
        let mut items = Vec::new();
        for item in rows {
            items.push(item?);
        }
        Ok(items)
    }
    /// Legacy endpoint rows only, for v1-v4 node/link serialization.
    pub fn get_legacy_artifact_links(
        &self,
        interaction_id: &InteractionId,
    ) -> Result<Vec<ArtifactLink>> {
        let mut stmt=self.conn.prepare("SELECT interaction_id,git_commit_hash,link_type,linked_by FROM artifact_links WHERE interaction_id=?1")?;
        let rows = stmt.query_map(params![interaction_id.to_string()], |row| {
            Ok(ArtifactLink {
                interaction_id: row.get::<_, String>(0)?.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                git_commit_hash: CommitSha::new(row.get::<_, String>(1)?),
                link_type: row.get(2)?,
                linked_by: row.get(3)?,
            })
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Events are publishable only with authority for this exact destination.
    /// A remote assertion is evidence received from elsewhere, never outbound
    /// authority and never a locally-observed rewrite source.
    pub fn projection_derivation_events(
        &self,
        _ids: &[InteractionId],
        remote: &str,
    ) -> Result<Vec<DerivationEvent>> {
        let mut out = Vec::new();
        // Include authorized suppressed sources in the candidate set so the
        // caller can compute transitive closure before removing them. Returning
        // only currently projectable interaction IDs would orphan descendants
        // and let them survive a tombstone.
        let mut s=self.conn.prepare("SELECT e.payload FROM derivation_events e WHERE EXISTS(SELECT 1 FROM interaction_shares i WHERE i.interaction_id=e.interaction_id AND i.remote_fingerprint=?1) AND EXISTS(SELECT 1 FROM derivation_observations o WHERE o.event_id=e.event_id AND o.trusted_local=1) AND EXISTS(SELECT 1 FROM derivation_authorizations a WHERE a.event_id=e.event_id AND a.remote_fingerprint=?1) ORDER BY e.event_id")?;
        for row in s.query_map(params![remote], |r| r.get::<_, String>(0))? {
            let e: DerivationEvent =
                serde_json::from_str(&row?).map_err(|e| DbError::Migration(e.to_string()))?;
            if e.verify_id() {
                out.push(e)
            } else {
                return Err(DbError::LinkConflict(
                    "stored derivation event hash mismatch".into(),
                ));
            }
        }
        Ok(out)
    }
    pub fn has_locally_observed_source(
        &self,
        id: &InteractionId,
        commit: &CommitSha,
    ) -> Result<bool> {
        self.conn.query_row("SELECT EXISTS(SELECT 1 FROM artifact_links a JOIN derivation_observations o ON o.event_id='legacy:' || a.interaction_id || ':' || a.git_commit_hash WHERE a.interaction_id=?1 AND a.git_commit_hash=?2 AND o.trusted_local=1) OR EXISTS(SELECT 1 FROM derivation_events e JOIN derivation_observations o ON o.event_id=e.event_id WHERE e.interaction_id=?1 AND e.target_commit=?2 AND o.trusted_local=1)",params![id.to_string(),commit.as_str()],|r|r.get(0)).map_err(Into::into)
    }

    /// Get all unique commits that have linked interactions, along with their interactions
    /// Returns `(commit_sha, Vec<Interaction>)` pairs ordered by latest timestamp.
    pub fn get_commits_with_interactions(&self) -> Result<Vec<(CommitSha, Vec<Interaction>)>> {
        // First get all unique commits ordered by most recent interaction
        let mut commit_stmt = self.conn.prepare(
            "SELECT DISTINCT al.git_commit_hash, MAX(i.timestamp) as max_ts
             FROM (SELECT interaction_id,git_commit_hash FROM artifact_links UNION SELECT interaction_id,target_commit FROM derivation_events) al
             JOIN interactions i ON al.interaction_id = i.id
             GROUP BY al.git_commit_hash
             ORDER BY max_ts DESC",
        )?;

        let commits: Vec<CommitSha> = commit_stmt
            .query_map([], |row| {
                let sha: String = row.get(0)?;
                Ok(CommitSha::new(sha))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // Now for each commit, get the linked interactions
        let mut result = Vec::new();
        for commit_sha in commits {
            let interactions = self.get_interactions_for_commit(&commit_sha)?;
            result.push((commit_sha, interactions));
        }

        Ok(result)
    }

    /// Get all interactions linked to a specific commit
    pub fn get_interactions_for_commit(&self, commit_sha: &CommitSha) -> Result<Vec<Interaction>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.id, i.conversation_id, i.parent_id, i.timestamp, i.author, i.user_prompt,
                    i.model_name, i.model_cot, i.model_response, i.source_request_id
             FROM interactions i
              JOIN (SELECT interaction_id,git_commit_hash FROM artifact_links UNION SELECT interaction_id,target_commit FROM derivation_events) al ON i.id = al.interaction_id
              WHERE al.git_commit_hash = ?1
             ORDER BY i.timestamp DESC",
        )?;

        let rows = stmt.query_map(params![commit_sha.as_str()], |row| {
            let parent_id_str: Option<String> = row.get(2)?;
            let timestamp: i64 = row.get(3)?;
            let author_str: String = row.get(4)?;
            let author = match author_str.as_str() {
                "human" => Author::Human,
                "agent" => Author::Agent,
                "system" => Author::System,
                _ => Author::External,
            };

            let dt = Utc.timestamp_opt(timestamp, 0).single().ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Integer,
                    Box::new(DbError::Timestamp(timestamp)),
                )
            })?;

            Ok(Interaction {
                id: row.get::<_, String>(0)?.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                conversation_id: row.get(1)?,
                parent_id: parent_id_str.map(|s| s.parse().unwrap_or_default()),
                timestamp: dt,
                author,
                user_prompt: row.get(5)?,
                model_name: row.get(6)?,
                model_cot: row.get(7)?,
                model_response: row.get(8)?,
                source_request_id: row.get(9)?,
            })
        })?;

        let mut interactions = Vec::new();
        for interaction in rows {
            interactions.push(interaction?);
        }
        Ok(interactions)
    }
}

#[cfg(test)]
mod trusted_api_tests {
    use super::*;
    use crate::models::{CaptureSource, Evidence};
    use crate::privacy::{McpCapture, PreparedPolicy};

    /// One thought from `source` into `conversation_id`, carrying `title` for
    /// its conversation, as the source's front would hand it to the store.
    fn fixture_capture(
        source: CaptureSource,
        conversation_id: &str,
        title: &str,
    ) -> (InteractionId, Capture) {
        let id = InteractionId::new();
        let capture = Capture {
            conversation: Conversation {
                id: conversation_id.into(),
                title: title.into(),
                created_at: Utc::now(),
            },
            interaction: Interaction {
                id: id.clone(),
                conversation_id: conversation_id.into(),
                parent_id: None,
                timestamp: Utc::now(),
                author: Author::Agent,
                user_prompt: format!("{source:?}"),
                model_name: None,
                model_cot: None,
                model_response: None,
                source_request_id: None,
            },
            context_items: Vec::new(),
            tool_executions: Vec::new(),
            source,
            policy: PreparedPolicy::built_ins_only(),
            capture_worktree: Some("0".repeat(64)),
        };
        (id, capture)
    }

    /// `name` in the conversation-id namespace `source` records into.
    fn in_namespace(source: CaptureSource, name: &str) -> String {
        match source {
            CaptureSource::Harness(harness) => harness.conversation_id(name).unwrap(),
            _ => name.to_owned(),
        }
    }

    /// The stored `capture_source` strings are persistent: each source keeps
    /// the spelling it has, the values 0.7.x wrote included, and the database
    /// triggers list exactly these.
    #[test]
    fn capture_source_strings_are_pinned_and_match_the_triggers() {
        let pinned = [
            (CaptureSource::Mcp, "mcp"),
            (CaptureSource::VscodePassive, "vscode_passive"),
            (CaptureSource::VscodeExplicit, "vscode_explicit"),
            (CaptureSource::CliRun, "cli_run"),
            (CaptureSource::SyncImport, "sync_import"),
            (CaptureSource::Legacy, "legacy"),
            (CaptureSource::Harness(HarnessId::ClaudeCode), "claude_code"),
            (CaptureSource::Harness(HarnessId::Codex), "codex"),
            (CaptureSource::Harness(HarnessId::OpenCode), "opencode"),
            (CaptureSource::Harness(HarnessId::Cursor), "cursor"),
            (
                CaptureSource::Harness(HarnessId::Antigravity),
                "antigravity",
            ),
        ];
        assert_eq!(
            pinned.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            CaptureSource::ALL
        );
        for (source, value) in pinned {
            assert_eq!(source.as_db_str(), value);
            // Every value passes the column's shape CHECK.
            assert!(!value.is_empty() && value.len() <= 32);
            assert!(value.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        }
        let listed = CAPTURE_SOURCE_VALUES_SQL
            .split(',')
            .map(|v| v.trim_matches('\''))
            .collect::<Vec<_>>();
        assert_eq!(listed, pinned.iter().map(|(_, v)| *v).collect::<Vec<_>>());
    }

    /// Every source titles a conversation it creates. Only the sources that
    /// observe a session rename one that already exists; a thought recorded
    /// through MCP and a capture whose source is not known leave its title
    /// exactly as it was, an empty one included.
    #[test]
    fn only_the_sources_that_observe_a_session_rename_an_existing_conversation(
    ) -> anyhow::Result<()> {
        let store = CvcStore::open(":memory:")?;
        let title_of = |conversation: &str| -> anyhow::Result<String> {
            Ok(store
                .get_conversation(conversation)?
                .ok_or_else(|| anyhow::anyhow!("no conversation {conversation}"))?
                .title)
        };
        let expectations = [
            (CaptureSource::Mcp, false),
            (CaptureSource::Legacy, false),
            (CaptureSource::VscodePassive, true),
            (CaptureSource::VscodeExplicit, true),
            (CaptureSource::Harness(HarnessId::ClaudeCode), true),
            (CaptureSource::Harness(HarnessId::Codex), true),
            (CaptureSource::Harness(HarnessId::OpenCode), true),
            (CaptureSource::Harness(HarnessId::Cursor), true),
            (CaptureSource::Harness(HarnessId::Antigravity), true),
            (CaptureSource::CliRun, true),
            (CaptureSource::SyncImport, true),
        ];
        assert_eq!(expectations.len(), CaptureSource::ALL.len());
        for (source, renames) in expectations {
            assert_eq!(source.refreshes_conversation_title(), renames, "{source:?}");
            let conversation = in_namespace(source, source.as_db_str());
            store.capture(fixture_capture(source, &conversation, "created as").1)?;
            assert_eq!(title_of(&conversation)?, "created as", "{source:?}");
            for existing in ["created as", ""] {
                store.conn.execute(
                    "UPDATE conversations SET title=?2 WHERE id=?1",
                    params![conversation, existing],
                )?;
                store.capture(fixture_capture(source, &conversation, "named again").1)?;
                let expected = if renames { "named again" } else { existing };
                assert_eq!(
                    title_of(&conversation)?,
                    expected,
                    "{source:?} {existing:?}"
                );
            }
        }
        Ok(())
    }

    /// The one write boundary decides, for every trusted source, whether a
    /// capture joins its conversation's future sharing. A thought recorded
    /// through MCP and a capture whose source is not known (`Legacy`, the
    /// default, which no public constructor produces) are stored private; the
    /// other sources join the destinations whose future sharing is on, and only
    /// those.
    #[test]
    fn future_sharing_extends_only_to_the_sources_that_take_part_in_it() -> anyhow::Result<()> {
        let store = CvcStore::open(":memory:")?;
        let future_shared = "7".repeat(64);
        let future_private = "8".repeat(64);
        assert_eq!(CaptureSource::default(), CaptureSource::Legacy);
        let expectations = [
            (CaptureSource::Mcp, false),
            (CaptureSource::Legacy, false),
            (CaptureSource::VscodePassive, true),
            (CaptureSource::VscodeExplicit, true),
            (CaptureSource::Harness(HarnessId::ClaudeCode), true),
            (CaptureSource::Harness(HarnessId::Codex), true),
            (CaptureSource::Harness(HarnessId::OpenCode), true),
            (CaptureSource::Harness(HarnessId::Cursor), true),
            (CaptureSource::Harness(HarnessId::Antigravity), true),
            (CaptureSource::CliRun, true),
            (CaptureSource::SyncImport, true),
        ];
        assert_eq!(expectations.len(), CaptureSource::ALL.len());
        for (source, inherits) in expectations {
            assert_eq!(source.inherits_future_sharing(), inherits, "{source:?}");
            let conversation = in_namespace(source, "policy");
            // Recorded before anything is in the conversation, as `cvc share
            // policy --future` would record it.
            if store.get_conversation(&conversation)?.is_none() {
                store.share_exact_snapshot(
                    &conversation,
                    &future_shared,
                    &[],
                    FutureSharePolicy::Shared,
                )?;
                store.share_exact_snapshot(
                    &conversation,
                    &future_private,
                    &[],
                    FutureSharePolicy::Private,
                )?;
            }
            let (id, capture) = fixture_capture(source, &conversation, "policy");
            store.capture(capture)?;
            let shares: Vec<String> = store
                .conn
                .prepare(
                    "SELECT remote_fingerprint FROM interaction_shares WHERE interaction_id=?1",
                )?
                .query_map(params![id.as_str()], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            let expected = if inherits {
                vec![future_shared.clone()]
            } else {
                Vec::new()
            };
            assert_eq!(shares, expected, "{source:?}");
        }
        Ok(())
    }

    #[test]
    fn squash_queue_capacity_is_classified_as_retryable() {
        assert_eq!(
            DbError::SquashQueueCapacity { limit: 1 }.retry_disposition(),
            RetryDisposition::Retryable
        );
        assert_eq!(
            DbError::InvalidLink("invalid squash plan".into()).retry_disposition(),
            RetryDisposition::Permanent
        );
    }

    #[test]
    fn private_rewrite_cas_rejects_changed_event_observation_and_authorization(
    ) -> anyhow::Result<()> {
        for mutation in 0..3 {
            let mut store = CvcStore::open(":memory:")?;
            let interaction = Interaction {
                id: InteractionId::new(),
                conversation_id: "cas".into(),
                parent_id: None,
                timestamp: Utc::now(),
                author: Author::Human,
                user_prompt: "cas".into(),
                model_name: None,
                model_cot: None,
                model_response: None,
                source_request_id: None,
            };
            store.capture_mcp(McpCapture::new(
                Conversation {
                    id: "cas".into(),
                    title: "cas".into(),
                    created_at: interaction.timestamp,
                },
                interaction.clone(),
                Vec::new(),
                Vec::new(),
                PreparedPolicy::built_ins_only(),
                "0".repeat(64),
            ))?;
            let mut source = DerivationEvent {
                event_id: String::new(),
                interaction_id: interaction.id.clone(),
                target_commit: CommitSha::new("a".repeat(40)),
                relation: DerivationRelation::Generated,
                evidence: Evidence {
                    version: 1,
                    kind: EvidenceKind::LocallyObserved,
                },
                origin: DerivationOrigin::LocalLinker,
                source_event_ids: Vec::new(),
                old_oid: None,
                new_oid: None,
                range_id: None,
                linked_by: None,
            };
            source.event_id = source.canonical_id();
            store.conn.execute("INSERT INTO derivation_events(event_id,interaction_id,target_commit,relation,evidence_version,evidence_kind,origin,source_event_ids,payload) VALUES(?1,?2,?3,'generated',1,'locally_observed','local_linker','[]',?4)", params![source.event_id,interaction.id.to_string(),source.target_commit.as_str(),serde_json::to_string(&source)?])?;
            store.conn.execute("INSERT INTO derivation_observations(event_id,source_key,origin,trusted_local) VALUES(?1,'local','local_api',1)",params![source.event_id])?;
            store.conn.execute("INSERT INTO derivation_authorizations(event_id,remote_fingerprint,source_event_id) VALUES(?1,'remote','source')",params![source.event_id])?;
            let snapshots =
                store.rewrite_source_snapshots(&interaction.id, &source.target_commit)?;
            let mut derived = DerivationEvent {
                event_id: String::new(),
                interaction_id: interaction.id.clone(),
                target_commit: CommitSha::new("b".repeat(40)),
                relation: DerivationRelation::RewriteExact,
                evidence: Evidence {
                    version: 1,
                    kind: EvidenceKind::LocallyObserved,
                },
                origin: DerivationOrigin::LocalHook,
                source_event_ids: vec![source.event_id.clone()],
                old_oid: Some(source.target_commit.clone()),
                new_oid: Some(CommitSha::new("b".repeat(40))),
                range_id: None,
                linked_by: None,
            };
            derived.event_id = derived.canonical_id();
            match mutation {
                0 => store.conn.execute(
                    "UPDATE derivation_events SET payload=payload || ' ' WHERE event_id=?1",
                    params![source.event_id],
                )?,
                1 => store.conn.execute(
                    "DELETE FROM derivation_observations WHERE event_id=?1",
                    params![source.event_id],
                )?,
                _ => store.conn.execute(
                    "DELETE FROM derivation_authorizations WHERE event_id=?1",
                    params![source.event_id],
                )?,
            };
            assert!(store
                .apply_rewrite_events("batch", "amend", "hash", &[derived], &snapshots)
                .is_err());
            assert_eq!(
                store
                    .conn
                    .query_row("SELECT COUNT(*) FROM derivation_events", [], |row| row
                        .get::<_, i64>(0))?,
                1
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod harness_batch_tests {
    use super::*;
    use crate::privacy::{HarnessCaptureInput, PreparedPolicy};

    const WORKTREE: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn capture(harness: HarnessId, conversation: &str, key: &str) -> HarnessCaptureInput {
        let id = crate::harness::derived_interaction_id(harness, conversation, key);
        HarnessCaptureInput::new(
            harness,
            Conversation {
                id: conversation.into(),
                title: "a session".into(),
                created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            },
            Interaction {
                id,
                conversation_id: conversation.into(),
                parent_id: None,
                timestamp: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
                author: Author::Human,
                user_prompt: "fix the flaky test".into(),
                model_name: None,
                model_cot: None,
                model_response: Some("done".into()),
                source_request_id: Some(key.into()),
            },
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            WORKTREE.into(),
        )
    }

    fn forward(_: &StoredHarnessCursor) -> CursorProgress {
        CursorProgress::Forward
    }
    fn backward(_: &StoredHarnessCursor) -> CursorProgress {
        CursorProgress::Backward
    }

    fn cursor<'a>(
        key: &'a str,
        epoch: u64,
        ordinal: u64,
        progress: &'a dyn Fn(&StoredHarnessCursor) -> CursorProgress,
    ) -> HarnessCursorWrite<'a> {
        HarnessCursorWrite {
            cursor_key: key,
            kind: "jsonl",
            version: 1,
            epoch,
            state: serde_json::json!({ "path": "/r.jsonl", "last_ordinal": ordinal }),
            progress,
        }
    }

    fn batch(conversation: &str) -> HarnessBatch<'_> {
        HarnessBatch {
            harness: HarnessId::Codex,
            conversation_id: conversation,
            captures: Vec::new(),
            cursor: None,
            legacy_cursor: None,
            evidence: Vec::new(),
            relations: Vec::new(),
        }
    }

    fn relation(child: &str, parent: &str) -> ConversationRelation {
        ConversationRelation {
            child_id: child.into(),
            parent_id: parent.into(),
            kind: RelationKind::Subagent,
            harness: HarnessId::Codex,
            spawn_interaction_id: None,
            spawn_call_id: None,
            evidence: RelationEvidence::ChildRecord,
        }
    }

    fn ordinal(store: &CvcStore, key: &str) -> (u64, u64) {
        let stored = store
            .harness_cursor(HarnessId::Codex, key)
            .unwrap()
            .unwrap();
        (stored.epoch, stored.state["last_ordinal"].as_u64().unwrap())
    }

    #[test]
    fn captures_and_cursor_commit_together_and_the_cursor_never_moves_back() {
        let store = CvcStore::open(":memory:").unwrap();
        let conv = "codex:t1";
        let mut first = batch(conv);
        first.captures = vec![capture(HarnessId::Codex, conv, "resp:a")];
        first.cursor = Some(cursor(conv, 0, 10, &forward));
        let outcome = store.capture_harness_batch(first).unwrap();
        assert_eq!((outcome.inserted, outcome.cursor_held), (1, false));
        assert_eq!(ordinal(&store, conv), (0, 10));
        let source: String = store
            .conn
            .query_row("SELECT capture_source FROM interactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(source, "codex");

        // The same response again is skipped; a backward cursor is held while
        // the captures still commit.
        let mut again = batch(conv);
        again.captures = vec![
            capture(HarnessId::Codex, conv, "resp:a"),
            capture(HarnessId::Codex, conv, "resp:b"),
        ];
        again.cursor = Some(cursor(conv, 0, 5, &backward));
        let outcome = store.capture_harness_batch(again).unwrap();
        assert_eq!((outcome.inserted, outcome.suppressed), (1, 1));
        assert!(outcome.cursor_held);
        assert_eq!(ordinal(&store, conv), (0, 10));

        // Forward in the same epoch moves; the next epoch replaces whatever
        // its codec says; an earlier epoch is held; one that skips ahead is
        // refused and stores nothing.
        let mut ahead = batch(conv);
        ahead.cursor = Some(cursor(conv, 0, 99, &forward));
        store.capture_harness_batch(ahead).unwrap();
        assert_eq!(ordinal(&store, conv), (0, 99));
        let mut next = batch(conv);
        next.cursor = Some(cursor(conv, 1, 3, &backward));
        assert!(!store.capture_harness_batch(next).unwrap().cursor_held);
        assert_eq!(ordinal(&store, conv), (1, 3));
        let mut stale = batch(conv);
        stale.cursor = Some(cursor(conv, 0, 500, &forward));
        assert!(store.capture_harness_batch(stale).unwrap().cursor_held);
        let mut skip = batch(conv);
        skip.captures = vec![capture(HarnessId::Codex, conv, "resp:c")];
        skip.cursor = Some(cursor(conv, 3, 1, &forward));
        assert!(matches!(
            store.capture_harness_batch(skip),
            Err(DbError::HarnessBatch(_))
        ));
        assert_eq!(store.conversation_interaction_count(conv).unwrap(), 2);
        assert_eq!(ordinal(&store, conv), (1, 3));

        // An older cursor version never overwrites a newer one.
        let mut newer = batch(conv);
        let mut write = cursor(conv, 1, 4, &forward);
        write.version = 2;
        newer.cursor = Some(write);
        store.capture_harness_batch(newer).unwrap();
        let mut older = batch(conv);
        older.cursor = Some(cursor(conv, 1, 9, &forward));
        assert!(store.capture_harness_batch(older).unwrap().cursor_held);
        assert_eq!(ordinal(&store, conv), (1, 4));
    }

    #[test]
    fn inconsistent_batches_are_refused_before_anything_is_stored() {
        let store = CvcStore::open(":memory:").unwrap();
        let conv = "codex:t1";
        let refused = |b: HarnessBatch<'_>| {
            assert!(matches!(
                store.capture_harness_batch(b),
                Err(DbError::HarnessBatch(_))
            ));
        };
        let mut other_conversation = batch(conv);
        other_conversation.captures = vec![capture(HarnessId::Codex, "codex:t2", "resp:a")];
        refused(other_conversation);
        let mut other_harness = batch("opencode:s");
        other_harness.captures = vec![capture(HarnessId::OpenCode, "opencode:s", "msg:a")];
        refused(other_harness);
        let mut legacy = batch(conv);
        legacy.legacy_cursor = Some(HarnessCursorUpdate {
            harness: "codex",
            session_id: conv,
            transcript_path: "/r",
            byte_offset: 1,
        });
        refused(legacy);
        let mut claude_relation = batch("s1");
        claude_relation.harness = HarnessId::ClaudeCode;
        claude_relation.relations = vec![ConversationRelation {
            harness: HarnessId::ClaudeCode,
            ..relation("s1:agent-a", "s1")
        }];
        refused(claude_relation);
        let mut outside = batch(conv);
        outside.relations = vec![relation("codex:a:agent-b", conv)];
        refused(outside);
        let mut unrelated = batch(conv);
        unrelated.relations = vec![relation("codex:x", "codex:y")];
        refused(unrelated);
        let mut bad_cursor = batch(conv);
        let mut write = cursor(conv, 0, 1, &forward);
        write.kind = "JSONL";
        bad_cursor.cursor = Some(write);
        refused(bad_cursor);
        let mut not_object = batch(conv);
        let mut write = cursor(conv, 0, 1, &forward);
        write.state = serde_json::json!([1]);
        not_object.cursor = Some(write);
        refused(not_object);
        assert!(store
            .list_conversation_summaries(None, 10)
            .unwrap()
            .is_empty());
        assert!(store
            .harness_cursor(HarnessId::Codex, conv)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_prefixed_harness_never_records_into_a_conversation_another_source_made() {
        let store = CvcStore::open(":memory:").unwrap();
        // What a 0.7.x MCP client could have created before the prefix was
        // reserved.
        store
            .conn
            .execute_batch(
                "INSERT INTO conversations(id,title,created_at) VALUES('codex:t1','mcp',1);
                 INSERT INTO interactions(id,conversation_id,timestamp,author,user_prompt,capture_source,scrubber_version) VALUES('6b1f0c0e-1111-4aaa-8bbb-000000000001','codex:t1',1,'agent','x','mcp',1);",
            )
            .unwrap();
        let mut b = batch("codex:t1");
        b.captures = vec![capture(HarnessId::Codex, "codex:t1", "resp:a")];
        assert!(matches!(
            store.capture_harness_batch(b),
            Err(DbError::HarnessBatch(_))
        ));
        assert_eq!(store.conversation_interaction_count("codex:t1").unwrap(), 1);
    }

    #[test]
    fn claude_code_batches_keep_the_legacy_cursor_written() {
        let store = CvcStore::open(":memory:").unwrap();
        let typed =
            |offset: u64, progress: &'static dyn Fn(&StoredHarnessCursor) -> CursorProgress| {
                HarnessCursorWrite {
                    cursor_key: "s1",
                    kind: "jsonl",
                    version: 1,
                    epoch: 0,
                    state: serde_json::json!({ "path": "/t.jsonl", "byte_offset": offset }),
                    progress,
                }
            };
        let legacy = |offset| HarnessCursorUpdate {
            harness: "claude-code",
            session_id: "s1",
            transcript_path: "/t.jsonl",
            byte_offset: offset,
        };
        let mut b = batch("s1");
        b.harness = HarnessId::ClaudeCode;
        b.captures = vec![capture(HarnessId::ClaudeCode, "s1", "req_1")];
        b.cursor = Some(typed(42, &forward));
        b.legacy_cursor = Some(legacy(42));
        store.capture_harness_batch(b).unwrap();
        let stored = store
            .harness_ingest_cursor("claude-code", "s1")
            .unwrap()
            .unwrap();
        assert_eq!(
            (stored.transcript_path.as_str(), stored.byte_offset),
            ("/t.jsonl", 42)
        );

        // The typed cursor held, the legacy one held with it.
        let mut b = batch("s1");
        b.harness = HarnessId::ClaudeCode;
        b.cursor = Some(typed(7, &backward));
        b.legacy_cursor = Some(legacy(7));
        assert!(store.capture_harness_batch(b).unwrap().cursor_held);
        let stored = store
            .harness_ingest_cursor("claude-code", "s1")
            .unwrap()
            .unwrap();
        assert_eq!(stored.byte_offset, 42);
    }

    #[test]
    fn the_claude_code_batch_mirrors_its_cursor_into_the_typed_table() {
        let store = CvcStore::open(":memory:").unwrap();
        let update = |offset| HarnessCursorUpdate {
            harness: "claude-code",
            session_id: "s1",
            transcript_path: "/t.jsonl",
            byte_offset: offset,
        };
        store
            .capture_claude_code_batch(Vec::new(), update(10))
            .unwrap();
        let typed = store
            .harness_cursor(HarnessId::ClaudeCode, "s1")
            .unwrap()
            .unwrap();
        assert_eq!(
            (typed.kind.as_str(), typed.version, typed.epoch),
            ("jsonl", 1, 0)
        );
        assert_eq!(
            typed.state,
            serde_json::json!({"path": "/t.jsonl", "byte_offset": 10})
        );
        let legacy = store
            .harness_ingest_cursor("claude-code", "s1")
            .unwrap()
            .unwrap();
        assert_eq!(typed.updated_at, legacy.updated_at);
        // What else a later codec keeps in the row stays; the offset follows
        // the legacy row, even when it goes back (a rewritten transcript).
        store
            .conn
            .execute(
                "UPDATE harness_cursors SET state=json_set(state,'$.carry','kept') WHERE cursor_key='s1'",
                [],
            )
            .unwrap();
        store
            .capture_claude_code_batch(Vec::new(), update(3))
            .unwrap();
        let typed = store
            .harness_cursor(HarnessId::ClaudeCode, "s1")
            .unwrap()
            .unwrap();
        assert_eq!(typed.state["byte_offset"], 3);
        assert_eq!(typed.state["carry"], "kept");
        assert_eq!(
            store
                .harness_ingest_cursor("claude-code", "s1")
                .unwrap()
                .unwrap()
                .byte_offset,
            3
        );
    }

    #[test]
    fn commit_evidence_is_durable_and_checked() {
        let store = CvcStore::open(":memory:").unwrap();
        let conv = "codex:t1";
        let a = capture(HarnessId::Codex, conv, "resp:a");
        let a_id = a.interaction_id().clone();
        let evidence = |candidate: &str, id: &InteractionId| HarnessCommitEvidence {
            family_root: conv.into(),
            interaction_id: id.clone(),
            candidate: candidate.into(),
            response_at: 1_700_000_000,
        };
        let mut b = batch(conv);
        b.captures = vec![a];
        b.evidence = vec![
            evidence("0123abcd", &a_id),
            evidence("0123abcd", &a_id),
            evidence("ABCD1234", &a_id),
            evidence("abc", &a_id),
            evidence(&"a".repeat(65), &a_id),
            evidence("0123abcd", &InteractionId::new()),
        ];
        let outcome = store.capture_harness_batch(b).unwrap();
        assert_eq!(
            (outcome.evidence_recorded, outcome.evidence_skipped),
            (2, 4)
        );
        let stored = store
            .harness_commit_evidence(HarnessId::Codex, conv)
            .unwrap();
        assert_eq!(stored, vec![evidence("0123abcd", &a_id)]);
        // A local deletion removes the interaction's evidence with it.
        store
            .tombstone_local(&a_id, TombstoneReasonCode::UserRequested, None)
            .unwrap();
        assert!(store
            .harness_commit_evidence(HarnessId::Codex, conv)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn relations_are_never_overwritten() {
        let store = CvcStore::open(":memory:").unwrap();
        let record = |child: &str, relation: ConversationRelation| {
            let mut b = batch(child);
            b.relations = vec![relation];
            store.capture_harness_batch(b)
        };
        let recorded = |result: Result<HarnessBatchOutcome>| result.unwrap().relations_recorded;
        assert_eq!(
            recorded(record("codex:c", relation("codex:c", "codex:p"))),
            1
        );
        // The same relation again does nothing.
        assert_eq!(
            recorded(record("codex:c", relation("codex:c", "codex:p"))),
            0
        );
        // A missing spawn is filled once; another spawn is a conflict.
        let spawn = InteractionId::new();
        let with_spawn = ConversationRelation {
            spawn_interaction_id: Some(spawn.clone()),
            spawn_call_id: Some("call_1".into()),
            ..relation("codex:c", "codex:p")
        };
        assert_eq!(recorded(record("codex:c", with_spawn.clone())), 1);
        assert_eq!(recorded(record("codex:c", with_spawn)), 0);
        let other_spawn = ConversationRelation {
            spawn_call_id: Some("call_2".into()),
            ..relation("codex:c", "codex:p")
        };
        assert!(matches!(
            record("codex:c", other_spawn),
            Err(DbError::RelationConflict(_))
        ));
        // Another parent, or another kind, is a conflict.
        assert!(matches!(
            record("codex:c", relation("codex:c", "codex:q")),
            Err(DbError::RelationConflict(_))
        ));
        let fork = ConversationRelation {
            kind: RelationKind::Fork,
            ..relation("codex:c", "codex:p")
        };
        assert!(matches!(
            record("codex:c", fork),
            Err(DbError::RelationConflict(_))
        ));
        let stored = store.conversation_relation("codex:c").unwrap().unwrap();
        assert_eq!(stored.parent_id, "codex:p");
        assert_eq!(stored.spawn_interaction_id, Some(spawn));
        assert_eq!(stored.spawn_call_id.as_deref(), Some("call_1"));
        assert_eq!(
            store.conversation_children("codex:p").unwrap(),
            vec![stored]
        );

        // A cycle is refused, from either end.
        assert!(matches!(
            record("codex:p", relation("codex:p", "codex:c")),
            Err(DbError::RelationConflict(_))
        ));
        record("codex:g", relation("codex:g", "codex:c")).unwrap();
        assert!(matches!(
            record("codex:p", relation("codex:p", "codex:g")),
            Err(DbError::RelationConflict(_))
        ));
    }

    #[test]
    fn relation_chains_stop_at_the_maximum_depth() {
        let store = CvcStore::open(":memory:").unwrap();
        let id = |n: usize| format!("codex:n{n}");
        let record = |child: &str, parent: &str| {
            let mut b = batch(child);
            b.relations = vec![relation(child, parent)];
            store.capture_harness_batch(b)
        };
        // n0 is the root; n8 is 8 levels below it.
        for n in 1..=MAX_RELATION_DEPTH {
            record(&id(n), &id(n - 1)).unwrap();
        }
        assert!(matches!(
            record(&id(MAX_RELATION_DEPTH + 1), &id(MAX_RELATION_DEPTH)),
            Err(DbError::RelationConflict(_))
        ));
        // Hanging an existing chain under a new root counts its depth too.
        assert!(matches!(
            record(&id(0), "codex:above"),
            Err(DbError::RelationConflict(_))
        ));
    }

    #[test]
    fn summaries_name_the_agent_and_the_recorded_parent() {
        let store = CvcStore::open(":memory:").unwrap();
        let mut root = batch("codex:p");
        root.captures = vec![capture(HarnessId::Codex, "codex:p", "resp:a")];
        store.capture_harness_batch(root).unwrap();
        let mut child = batch("codex:c");
        child.captures = vec![capture(HarnessId::Codex, "codex:c", "resp:b")];
        child.relations = vec![relation("codex:c", "codex:p")];
        store.capture_harness_batch(child).unwrap();
        let mut claude = batch("s1");
        claude.harness = HarnessId::ClaudeCode;
        claude.captures = vec![capture(HarnessId::ClaudeCode, "s1", "req_1")];
        store.capture_harness_batch(claude).unwrap();
        let remote = "a".repeat(64);
        store
            .conn
            .execute_batch(&format!(
                "INSERT INTO conversations(id,title,created_at) VALUES('remote:{remote}:opencode:s','i',1),('remote:{remote}:u1','i',1),('codex:mcp','m',1);
                 INSERT INTO interactions(id,conversation_id,timestamp,author,user_prompt,capture_source,scrubber_version) VALUES
                   ('6b1f0c0e-1111-4aaa-8bbb-000000000001','remote:{remote}:opencode:s',1,'agent','x','sync_import',1),
                   ('6b1f0c0e-1111-4aaa-8bbb-000000000002','remote:{remote}:u1',1,'agent','x','sync_import',1),
                   ('6b1f0c0e-1111-4aaa-8bbb-000000000003','codex:mcp',1,'agent','x','mcp',1);"
            ))
            .unwrap();
        let summaries = store.list_conversation_summaries(None, 10).unwrap();
        let of = |id: &str| {
            let s = summaries.iter().find(|s| s.id == id).unwrap();
            (s.agent, s.parent.clone())
        };
        assert_eq!(of("codex:p"), (Some(HarnessId::Codex), None));
        assert_eq!(
            of("codex:c"),
            (Some(HarnessId::Codex), Some("codex:p".into()))
        );
        assert_eq!(of("s1"), (Some(HarnessId::ClaudeCode), None));
        assert_eq!(
            of(&format!("remote:{remote}:opencode:s")),
            (Some(HarnessId::OpenCode), None)
        );
        assert_eq!(of(&format!("remote:{remote}:u1")), (None, None));
        // A prefix alone, written by another local source, is no provenance.
        assert_eq!(of("codex:mcp"), (None, None));
        let details = store
            .conversation_details("codex:c", None)
            .unwrap()
            .unwrap();
        assert_eq!(details.summary.parent.as_deref(), Some("codex:p"));
    }

    /// A populated 0.7.2 store as a hook would open it: not rebuilt, Claude
    /// Code capture works, a new harness fails closed with the typed error
    /// until an operator open finishes the upgrade.
    #[test]
    fn a_new_harness_waits_for_the_upgrade_and_claude_code_does_not() {
        let temp = tempfile::TempDir::new().unwrap();
        let dir = temp.path().join("cvc");
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = dir.join("index.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(include_str!("../tests/fixtures/db/0.7.2.sql"))
                .unwrap();
            conn.execute_batch(
                "INSERT INTO conversations VALUES('old','t',1);
                 INSERT INTO interactions(id,conversation_id,timestamp,author,user_prompt,capture_source,scrubber_version) VALUES('6b1f0c0e-1111-4aaa-8bbb-000000000001','old',1,'agent','x','mcp',1);",
            )
            .unwrap();
        }
        let store = CvcStore::open_initialized(&path).unwrap();
        assert!(store.needs_capture_source_upgrade().unwrap());

        let mut claude = batch("s1");
        claude.harness = HarnessId::ClaudeCode;
        claude.captures = vec![capture(HarnessId::ClaudeCode, "s1", "req_1")];
        assert_eq!(store.capture_harness_batch(claude).unwrap().inserted, 1);

        let mut codex = batch("codex:t1");
        codex.captures = vec![capture(HarnessId::Codex, "codex:t1", "resp:a")];
        codex.cursor = Some(cursor("codex:t1", 0, 1, &forward));
        let refused = store.capture_harness_batch(codex).unwrap_err();
        assert!(
            matches!(
                refused,
                DbError::CaptureSourceUpgradeRequired {
                    harness: HarnessId::Codex
                }
            ),
            "{refused}"
        );
        assert_eq!(refused.retry_disposition(), RetryDisposition::Permanent);
        assert!(store.get_conversation("codex:t1").unwrap().is_none());
        assert!(store
            .harness_cursor(HarnessId::Codex, "codex:t1")
            .unwrap()
            .is_none());
        drop(store);

        let store = CvcStore::open_initialized_upgrading(&path).unwrap();
        assert!(!store.needs_capture_source_upgrade().unwrap());
        let mut codex = batch("codex:t1");
        codex.captures = vec![capture(HarnessId::Codex, "codex:t1", "resp:a")];
        assert_eq!(store.capture_harness_batch(codex).unwrap().inserted, 1);
        assert_eq!(store.conversation_interaction_count("old").unwrap(), 1);
        assert_eq!(store.conversation_interaction_count("s1").unwrap(), 1);
    }
}
