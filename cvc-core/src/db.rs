use crate::models::{
    ArtifactLink, Author, CommitSha, ContextItem, Conversation, FutureSharePolicy, Interaction,
    InteractionId, PublicationState, Tombstone, TombstoneReasonCode, ToolExecution, ToolStatus,
};
use crate::privacy::{self, Capture};
use chrono::{TimeZone, Utc};
use rusqlite::{ffi, params, Connection, ErrorCode, OptionalExtension, Row, Transaction};
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
    #[error("local deletion completed, but filesystem snapshots, backups, and SSD wear-leveling may retain prior bytes")]
    ResidualStorageWarning,
}

pub type Result<T> = std::result::Result<T, DbError>;

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
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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
    pub(crate) fn import_sync_batch(
        &self,
        captures: Vec<Capture>,
        links: Vec<(InteractionId, CommitSha, String, Option<String>)>,
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
        tx.execute("INSERT INTO interactions (id,conversation_id,parent_id,timestamp,author,user_prompt,model_name,model_cot,model_response,source_request_id,visibility,capture_source,scrubber_version) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,1)", params![i.id.to_string(),i.conversation_id,parent,i.timestamp.timestamp(),author,i.user_prompt,i.model_name,i.model_cot,i.model_response,i.source_request_id,visibility,source])?;
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
        // Interactions that are NOT in artifact_links
        let mut stmt = self.conn.prepare(
            "SELECT i.id, i.conversation_id, i.parent_id, i.timestamp, i.author, i.user_prompt,
                    i.model_name, i.model_cot, i.model_response, i.source_request_id
             FROM interactions i
             LEFT JOIN artifact_links al ON i.id = al.interaction_id
             WHERE al.interaction_id IS NULL",
        )?;

        let rows = stmt.query_map([], |row| {
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
        self.conn.execute(
            "INSERT OR IGNORE INTO artifact_links (interaction_id, git_commit_hash, link_type, linked_by) VALUES (?1, ?2, ?3, ?4)",
            params![
                interaction_id.to_string(),
                commit_sha.as_str(),
                link_type,
                linked_by,
            ]
        )?;
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
        let mut stmt = self.conn.prepare("SELECT interaction_id, git_commit_hash, link_type, linked_by FROM artifact_links WHERE interaction_id = ?1")?;

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

    /// Get all unique commits that have linked interactions, along with their interactions
    /// Returns `(commit_sha, Vec<Interaction>)` pairs ordered by latest timestamp.
    pub fn get_commits_with_interactions(&self) -> Result<Vec<(CommitSha, Vec<Interaction>)>> {
        // First get all unique commits ordered by most recent interaction
        let mut commit_stmt = self.conn.prepare(
            "SELECT DISTINCT al.git_commit_hash, MAX(i.timestamp) as max_ts
             FROM artifact_links al
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
             JOIN artifact_links al ON i.id = al.interaction_id
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
