use crate::models::{
    ArtifactLink, Author, CommitSha, ContextItem, Conversation, Interaction, InteractionId,
    ToolExecution, ToolStatus,
};
use chrono::{TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension};
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
}

pub type Result<T> = std::result::Result<T, DbError>;

pub struct CvcStore {
    conn: Connection,
}

impl CvcStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path)?;

        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        Ok(Self { conn })
    }

    pub fn init(&self) -> Result<()> {
        self.conn
            .execute_batch(include_str!("../migrations/0001_initial_schema.sql"))?;
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
                model_name, model_cot, model_response
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
            ],
        )?;
        Ok(())
    }

    pub fn get_interaction(&self, id: &InteractionId) -> Result<Option<Interaction>> {
        self.conn
            .query_row(
                "SELECT id, conversation_id, parent_id, timestamp, author, user_prompt,
                    model_name, model_cot, model_response
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
                    })
                },
            )
            .optional()
            .map_err(Into::into)
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
                    i.model_name, i.model_cot, i.model_response
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
            })
        })?;

        let mut interactions = Vec::new();
        for person in rows {
            interactions.push(person?);
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
        self.conn.execute(
            "INSERT OR IGNORE INTO artifact_links (interaction_id, git_commit_hash, link_type) VALUES (?1, ?2, ?3)",
            params![
                interaction_id.to_string(),
                commit_sha.as_str(),
                link_type
            ]
        )?;
        Ok(())
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
        let mut stmt = self.conn.prepare("SELECT interaction_id, git_commit_hash, link_type FROM artifact_links WHERE interaction_id = ?1")?;

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
                    i.model_name, i.model_cot, i.model_response
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
            })
        })?;

        let mut interactions = Vec::new();
        for interaction in rows {
            interactions.push(interaction?);
        }
        Ok(interactions)
    }
}
