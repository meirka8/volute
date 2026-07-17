use crate::models::{
    ArtifactLink, Author, CommitSha, ContextItem, Conversation, Interaction, InteractionId,
    ToolExecution, ToolStatus,
};
use chrono::{TimeZone, Utc};
use rusqlite::{ffi, params, Connection, ErrorCode, OptionalExtension, Row};
use std::path::Path;
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
}

pub type Result<T> = std::result::Result<T, DbError>;

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
}

impl CvcStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_initialized(path)
    }

    /// Open a store and apply every idempotent schema migration before it can
    /// be read or written. `open` delegates here for legacy callers.
    pub fn open_initialized<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;

        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    pub fn init(&self) -> Result<()> {
        self.conn
            .execute_batch(include_str!("../migrations/0001_initial_schema.sql"))?;

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

    pub fn create_conversation(&self, conv: &Conversation) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conversations (id, title, created_at) VALUES (?1, ?2, ?3)",
            params![conv.id, conv.title, conv.created_at.timestamp(),],
        )?;
        Ok(())
    }

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

    // --- Interactions ---

    pub fn create_interaction(&self, inter: &Interaction) -> Result<()> {
        let parent_id = inter.parent_id.as_ref().map(|id| id.as_str());
        let author_str = serde_json::to_string(&inter.author)
            .unwrap_or_default()
            .replace("\"", ""); // Simple enum to string

        self.conn.execute(
            "INSERT INTO interactions (
                id, conversation_id, parent_id, timestamp, author, user_prompt,
                model_name, model_cot, model_response, source_request_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                inter.id.to_string(),
                inter.conversation_id,
                parent_id,
                inter.timestamp.timestamp(),
                author_str,
                inter.user_prompt,
                inter.model_name,
                inter.model_cot,
                inter.model_response,
                inter.source_request_id,
            ],
        )?;
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

        Ok(())
    }

    // --- Context Items ---

    pub fn add_context_item(&self, item: &ContextItem) -> Result<()> {
        self.conn.execute(
            "INSERT INTO context_items (interaction_id, file_path, git_blob_sha, dirty_patch, start_line, end_line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                item.interaction_id.to_string(),
                item.file_path,
                item.git_blob_sha,
                item.dirty_patch,
                item.start_line,
                item.end_line
            ]
        )?;
        Ok(())
    }

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

    /// Import a link from sync with explicit conflict checks. Historical link
    /// types remain valid, while automatic types use the stricter batch path.
    pub fn import_artifact_link(
        &self,
        interaction_id: &InteractionId,
        commit_sha: &CommitSha,
        link_type: &str,
        linked_by: Option<&str>,
    ) -> Result<()> {
        if matches!(link_type, "generated" | "temporal") {
            self.link_automatic_interactions(
                std::slice::from_ref(interaction_id),
                commit_sha,
                link_type,
                linked_by,
            )?;
            return Ok(());
        }
        if !is_safe_commit_sha(commit_sha.as_str()) || link_type.trim().is_empty() {
            return Err(DbError::InvalidLink("invalid imported link".into()));
        }
        let transaction = self.conn.unchecked_transaction()?;
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
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(error, _)) if is_unique_constraint(&error) => {
                let existing: (String, Option<String>) = transaction.query_row(
                    "SELECT link_type, linked_by FROM artifact_links
                     WHERE interaction_id = ?1 AND git_commit_hash = ?2",
                    params![interaction_id.to_string(), commit_sha.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                if existing.0 != link_type
                    || matches!((existing.1.as_deref(), linked_by), (Some(left), Some(right)) if left != right)
                {
                    return Err(DbError::LinkConflict(
                        "imported link differs from existing provenance".into(),
                    ));
                }
                if existing.1.is_none() && linked_by.is_some() {
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
            }
            Err(error) => return Err(error.into()),
        }
        transaction.commit()?;
        Ok(())
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

    pub fn create_tool_execution(&self, exe: &ToolExecution) -> Result<()> {
        let status_str = match exe.status {
            ToolStatus::Success => "success",
            ToolStatus::Failure => "failure",
        };

        self.conn.execute(
            "INSERT INTO tool_executions (interaction_id, tool_protocol, tool_name, arguments, status) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                exe.interaction_id.to_string(),
                exe.tool_protocol,
                exe.tool_name,
                exe.arguments,
                status_str
            ]
        )?;
        Ok(())
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
    /// Returns a list of (commit_sha, Vec<Interaction>) pairs, ordered by the most recent interaction timestamp
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
