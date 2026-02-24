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

pub fn validate_ref_name(ref_name: &str) -> bool {
    // Simple validation: must start with refs/cvc/ OR refs/remotes/ (for pulling)
    if !ref_name.starts_with("refs/cvc/") && !ref_name.starts_with("refs/remotes/") {
        return false;
    }
    // Check for ".." to prevent traversal if file system backend is somehow involved (though git refs are usually safe)
    // and ensuring valid git ref characters is complex, but checking basic constraints is good.
    // Git ref validation is strict: https://git-scm.com/docs/git-check-ref-format
    // For now, we allow alphanumerics, slash, dash, underscore.
    ref_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '-' || c == '_')
}

pub fn push_to_ref(repo: &Repository, db: &CvcStore, ref_name: &str) -> Result<()> {
    if !validate_ref_name(ref_name) {
        return Err(SyncError::Ref(format!("Invalid ref name: {}", ref_name)));
    }

    // 1. Get all interaction IDs from DB
    let all_ids = db.get_all_interaction_ids()?;

    // 2. Load TreeBuilder from current ref
    let mut tree_builder = repo.treebuilder(None)?;
    let mut parent_commit = None;

    // Check if ref exists and points to a tree or commit
    if let Ok(reference) = repo.find_reference(ref_name) {
        // Try peeling to commit first (normal case after migration)
        if let Ok(commit) = reference.peel_to_commit() {
            let tree = commit.tree()?;
            tree_builder = repo.treebuilder(Some(&tree))?;
            parent_commit = Some(commit);
        } else if let Ok(obj) = reference.peel(ObjectType::Tree) {
            // Legacy case: ref points directly to tree
            if let Some(tree) = obj.as_tree() {
                tree_builder = repo.treebuilder(Some(tree))?;
            }
            // No parent commit in this case, we start a fresh history or valid parent is missing
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

    // Optimization: Skip commit if tree hasn't changed
    if let Some(parent) = &parent_commit {
        if parent.tree_id() == new_tree_oid {
            return Ok(());
        }
    }

    let new_tree = repo.find_tree(new_tree_oid)?;

    // 5. Create Commit
    // Try getting user signature, fallback to default if config missing, but propagate errors if critical
    let sig = match repo.signature() {
        Ok(s) => s,
        Err(_) => git2::Signature::now("cvc", "cvc@local")?,
    };

    let parents = if let Some(ref p) = parent_commit {
        vec![p]
    } else {
        vec![]
    };

    let _commit_oid = repo.commit(
        Some(ref_name),
        &sig,
        &sig,
        "Sync interactions",
        &new_tree,
        &parents,
    )?;

    Ok(())
}

pub fn pull_from_ref(repo: &Repository, db: &CvcStore, ref_name: &str) -> Result<()> {
    if !validate_ref_name(ref_name) {
        return Err(SyncError::Ref(format!("Invalid ref name: {}", ref_name)));
    }

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

    let mut nodes_to_insert = Vec::new();

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

        // 4. Parse SyncNode
        let node: SyncNode = serde_json::from_str(content)?;
        nodes_to_insert.push(node);
    }

    // 5. Robust Topological Sort (DFS)
    // We need to ensure that if B depends on A (B.parent_id = A.id), A is inserted first.

    use std::collections::HashMap;
    let mut node_map: HashMap<String, SyncNode> = HashMap::new();
    for node in nodes_to_insert {
        node_map.insert(node.interaction.id.to_string(), node);
    }

    let mut sorted_nodes = Vec::new();
    let mut visited = HashSet::new();
    let mut visiting = HashSet::new(); // Detect cycles if any (shouldn't exist)

    // We need a recursive helper, but Rust closures with recursion are tricky.
    // Iterative logic or defined function is better.
    // Or we can just use a stack for iterative DFS.

    // keys() gives us random order.
    let keys: Vec<String> = node_map.keys().cloned().collect();

    for id in keys {
        topo_visit(
            &id,
            &mut node_map,
            &mut sorted_nodes,
            &mut visited,
            &mut visiting,
        )?;
    }

    // 6. Insert into DB
    for node in sorted_nodes {
        let conv_id = &node.interaction.conversation_id;

        if db.get_conversation(conv_id)?.is_none() {
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

fn topo_visit(
    id: &str,
    node_map: &mut std::collections::HashMap<String, SyncNode>,
    sorted_nodes: &mut Vec<SyncNode>,
    visited: &mut HashSet<String>,
    visiting: &mut HashSet<String>,
) -> Result<()> {
    if visited.contains(id) {
        return Ok(());
    }
    if visiting.contains(id) {
        return Err(SyncError::Ref(format!(
            "Cycle detected in interaction dependencies: {}",
            id
        )));
    }

    visiting.insert(id.to_string());

    // Process dependencies (parent)
    if let Some(node) = node_map.get(id) {
        if let Some(parent_id) = &node.interaction.parent_id {
            // parent_id is InteractionId. We need string string.
            let p_id = parent_id.to_string();
            // If parent is in our batch, visit it first.
            // If it's not in the batch, we assume it's in DB or missing (which will fail FK).
            if node_map.contains_key(&p_id) {
                topo_visit(&p_id, node_map, sorted_nodes, visited, visiting)?;
            }
        }
    }

    visiting.remove(id);
    visited.insert(id.to_string());

    // Move from map to sorted list
    // Note: This removes from map, so if multiple nodes depend on A,
    // the second time we check A it won't be in map.
    // BUT we check `visited` first. If A is visited, we return Ok.
    // So we don't need A in map anymore.
    if let Some(node) = node_map.remove(id) {
        sorted_nodes.push(node);
    }

    Ok(())
}
