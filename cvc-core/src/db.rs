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
            | Self::ResidualStorageWarning => RetryDisposition::Permanent,
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

impl CvcStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_initialized(path)
    }

    /// Open a store and apply every idempotent schema migration before it can
    /// be read or written. `open` delegates here for legacy callers.
    pub fn open_initialized<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        prepare_store_path(path)?;
        let conn = Connection::open(path)?;

        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        // Hooks must fail quickly and leave their durable inbox for a later CVC
        // invocation rather than holding up Git's rewrite machinery.
        conn.busy_timeout(std::time::Duration::from_millis(250))?;
        enforce_store_permissions(path)?;

        let store = Self {
            conn,
            path: path.to_path_buf(),
        };
        store.init()?;
        enforce_store_permissions(&store.path)?;
        Ok(store)
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
        let tx = self.conn.unchecked_transaction()?;
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

    pub fn init(&self) -> Result<()> {
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
                self.conn.execute_batch("ALTER TABLE interactions ADD COLUMN capture_source TEXT NOT NULL DEFAULT 'legacy' CHECK(capture_source IN ('mcp','vscode_passive','vscode_explicit','cli_run','sync_import','legacy')); ")?;
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
            Ok(())
        })();
        match convergence {
            Ok(()) => self.conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }

        self.normalize_artifact_links_schema()?;

        Ok(())
    }

    fn normalize_artifact_links_schema(&self) -> Result<()> {
        let transaction = self.conn.unchecked_transaction()?;
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
        let tx = self.conn.unchecked_transaction()?;
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
            // conversation; `pull_from_ref_for_remote` separately records the
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
                tx.execute("INSERT INTO publications(interaction_id,remote_fingerprint,state,updated_at) VALUES(?1,?2,'published',?3) ON CONFLICT(interaction_id,remote_fingerprint) DO UPDATE SET state='published',updated_at=excluded.updated_at", params![id.to_string(), remote, Utc::now().timestamp()])?;
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
    fn capture(&self, capture: Capture) -> Result<InteractionId> {
        let capture = privacy::prepare(capture).map_err(|e| DbError::Migration(e.to_string()))?;
        let tx = self.conn.unchecked_transaction()?;
        if Self::is_tombstoned_tx(&tx, &capture.interaction.id)? {
            return Err(DbError::Migration("interaction is tombstoned".into()));
        }
        let id = Self::insert_capture(&tx, &capture)?;
        tx.commit()?;
        Ok(id)
    }

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
        let tx = self.conn.unchecked_transaction()?;
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
        Ok(())
    }

    fn insert_capture(tx: &Transaction<'_>, capture: &Capture) -> Result<InteractionId> {
        if Self::is_tombstoned_tx(tx, &capture.interaction.id)? {
            return Err(DbError::Migration("interaction is tombstoned".into()));
        }
        tx.execute(
            "INSERT INTO conversations (id,title,created_at) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET title=excluded.title",
            params![
                capture.conversation.id,
                capture.conversation.title,
                capture.conversation.created_at.timestamp()
            ],
        )?;
        let i = &capture.interaction;
        let parent = i.parent_id.as_ref().map(InteractionId::as_str);
        let author = serde_json::to_string(&i.author)
            .unwrap_or_default()
            .replace('"', "");
        let source = serde_json::to_string(&capture.source)
            .unwrap_or_default()
            .replace('"', "");
        let visibility = "private";
        tx.execute("INSERT INTO interactions (id,conversation_id,parent_id,timestamp,author,user_prompt,model_name,model_cot,model_response,source_request_id,visibility,capture_source,scrubber_version,capture_worktree) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,1,?13)", params![i.id.to_string(),i.conversation_id,parent,i.timestamp.timestamp(),author,i.user_prompt,i.model_name,i.model_cot,i.model_response,i.source_request_id,visibility,source,capture.capture_worktree])?;
        tx.execute("INSERT OR IGNORE INTO interaction_shares(interaction_id,remote_fingerprint) SELECT ?1,remote_fingerprint FROM conversation_shares WHERE conversation_id=?2 AND share_future=1", params![i.id.to_string(), i.conversation_id])?;
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
        let tx = self.conn.unchecked_transaction()?;
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
        let tx = self.conn.unchecked_transaction()?;
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
        let tx = self.conn.unchecked_transaction()?;
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
        let tx = self.conn.unchecked_transaction()?;
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
        let tx = self.conn.unchecked_transaction()?;
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
        let tx = self.conn.unchecked_transaction()?;
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

        let transaction = self.conn.unchecked_transaction()?;
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
        let tx = self.conn.unchecked_transaction()?;
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
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM interactions WHERE conversation_id=?1 ORDER BY id")?;
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
        let tx = self.conn.unchecked_transaction()?;
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
        let tx = self.conn.unchecked_transaction()?;
        let blocked: i64 = tx.query_row("SELECT COUNT(*) FROM publications p JOIN interactions i ON i.id=p.interaction_id WHERE i.conversation_id=?1 AND p.remote_fingerprint=?2 AND p.state IN ('pending','published','unknown')", params![conversation_id, remote], |r| r.get(0))?;
        if blocked != 0 {
            return Err(DbError::Migration(
                "cannot unshare pending, published, or unknown content".into(),
            ));
        }
        let count = tx.execute("DELETE FROM interaction_shares WHERE remote_fingerprint=?2 AND interaction_id IN (SELECT id FROM interactions WHERE conversation_id=?1)", params![conversation_id, remote])?;
        tx.execute("INSERT INTO conversation_shares(conversation_id,remote_fingerprint,share_future) VALUES(?1,?2,0) ON CONFLICT(conversation_id,remote_fingerprint) DO UPDATE SET share_future=0", params![conversation_id, remote])?;
        tx.commit()?;
        Ok(count)
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
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            tx.execute("INSERT INTO publications(interaction_id,remote_fingerprint,state,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(interaction_id,remote_fingerprint) DO UPDATE SET state=excluded.state,updated_at=excluded.updated_at", params![id.to_string(), remote, value, Utc::now().timestamp()])?;
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
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            tx.execute("DELETE FROM publications WHERE interaction_id=?1 AND remote_fingerprint=?2 AND state IN ('pending','unknown')", params![id.to_string(),remote])?;
        }
        tx.commit()?;
        Ok(())
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
    use crate::models::Evidence;
    use crate::privacy::{McpCapture, PreparedPolicy};

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
