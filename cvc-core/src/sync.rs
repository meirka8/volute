use crate::db::CvcStore;
use crate::models::{ArtifactLink, ContextItem, Interaction, ToolExecution};
use git2::{FileMode, ObjectType, Repository};
use serde::de::Error as SerdeError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SyncError {
    #[error("Git error: {0}")]
    Git(#[from] git2::Error),
    #[error("DB error: {0}")]
    Db(#[from] crate::db::DbError),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Reference error: {0}")]
    Ref(String),
}

pub type Result<T> = std::result::Result<T, SyncError>;

#[derive(Serialize, Deserialize, Debug)]
pub struct SyncNode {
    pub interaction: Interaction,
    pub context_items: Vec<ContextItem>,
    pub tool_executions: Vec<ToolExecution>,
    pub artifact_links: Vec<ArtifactLink>,
}

pub fn push_to_ref(repo: &Repository, db: &CvcStore, ref_name: &str) -> Result<()> {
    // 1. Get all interaction IDs from DB
    let all_ids = db.get_all_interaction_ids()?;

    // 2. Load TreeBuilder from current ref
    let mut tree_builder = repo.treebuilder(None)?;

    // Check if ref exists and points to a tree
    if let Ok(reference) = repo.find_reference(ref_name) {
        if let Ok(obj) = reference.peel(ObjectType::Tree) {
            if let Some(tree) = obj.as_tree() {
                tree_builder = repo.treebuilder(Some(tree))?;
            }
        }
    }

    // 3. Iterate IDs, check existence, add if missing
    for id in all_ids {
        let filename = format!("{}.json", id.as_str());

        // internal git2 optimization: get returns Entry which has oid
        if tree_builder.get(&filename)?.is_some() {
            continue; // Already synced
        }

        // Construct SyncNode
        let interaction = db.get_interaction(&id)?.ok_or_else(|| {
            SyncError::Db(crate::db::DbError::Migration("Interaction missing".into()))
        })?;
        let context_items = db.get_context_items(&id)?;
        let tool_executions = db.get_tool_executions(&id)?;
        let artifact_links = db.get_artifact_links(&id)?;

        let node = SyncNode {
            interaction,
            context_items,
            tool_executions,
            artifact_links,
        };

        // Serialize and Write Blob
        let json = serde_json::to_string_pretty(&node)?;
        let blob_oid = repo.blob(json.as_bytes())?;

        // Add to Tree
        tree_builder.insert(&filename, blob_oid, FileMode::Blob.into())?;
    }

    // 4. Write Tree
    let new_tree_oid = tree_builder.write()?;

    // 5. Update Ref
    repo.reference(ref_name, new_tree_oid, true, "cvc sync")?;

    Ok(())
}

pub fn pull_from_ref(repo: &Repository, db: &CvcStore, ref_name: &str) -> Result<()> {
    // 1. Resolve Ref
    let reference = match repo.find_reference(ref_name) {
        Ok(r) => r,
        Err(_) => return Ok(()), // Nothing to pull
    };

    let obj = reference.peel(ObjectType::Tree)?;
    let tree = obj
        .as_tree()
        .ok_or_else(|| SyncError::Ref("Ref is not a tree".into()))?;

    // 2. Iterate Tree
    // We want to verify existing IDs in DB to skip reading blobs
    let existing_ids_vec = db.get_all_interaction_ids()?;
    let existing_ids: HashSet<String> = existing_ids_vec
        .into_iter()
        .map(|id| id.as_str().to_string())
        .collect();

    for entry in tree.iter() {
        let name = entry.name().unwrap_or_default();
        if !name.ends_with(".json") {
            continue;
        }

        let id_str = name.trim_end_matches(".json");
        if existing_ids.contains(id_str) {
            continue;
        }

        // 3. Read Blob
        let object = entry.to_object(repo)?;
        let blob = object
            .as_blob()
            .ok_or_else(|| SyncError::Ref("Entry is not a blob".into()))?;
        let content = std::str::from_utf8(blob.content())
            .map_err(|e| SyncError::Serde(serde_json::Error::custom(e.to_string())))?;

        // 4. Provide SyncNode
        let node: SyncNode = serde_json::from_str(content)?;

        // 5. Insert into DB
        let conv_id = &node.interaction.conversation_id;
        if db.get_conversation(conv_id)?.is_none() {
            // Create placeholder
            db.create_conversation(&crate::models::Conversation {
                id: conv_id.clone(),
                title: "Synced Conversation".into(),
                created_at: node.interaction.timestamp,
            })?;
        }

        db.create_interaction(&node.interaction)?;
        for item in &node.context_items {
            db.add_context_item(item)?;
        }
        for exe in &node.tool_executions {
            db.create_tool_execution(exe)?;
        }
        for link in &node.artifact_links {
            db.link_interaction(&link.interaction_id, &link.git_commit_hash, &link.link_type)?;
        }
    }

    Ok(())
}
