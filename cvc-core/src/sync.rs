use crate::db::CvcStore;
use crate::models::{ArtifactLink, ContextItem, Interaction, ToolExecution};
use git2::{FileMode, ObjectType, Repository, Tree, TreeBuilder};
use serde::de::Error as SerdeError;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use thiserror::Error;

/// Tree layout version written by `push_to_ref`. Stored at the ref tree's root as a
/// blob named `FORMAT`. See `push_to_ref` for the layout this describes.
const SYNC_FORMAT_VERSION: &[u8] = b"3";

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

/// Immutable post-node link event. Node blobs remain immutable, so links made
/// after a floating node was first pushed are represented here instead.
#[derive(Serialize, Deserialize, Debug)]
struct SyncLinkRecord {
    interaction_id: String,
    git_commit_hash: String,
    link_type: String,
    #[serde(default)]
    linked_by: Option<String>,
}

fn is_safe_commit_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_automatic_link_type(value: &str) -> bool {
    matches!(value, "generated" | "temporal")
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

/// First two hex characters of an interaction ID, used as its `nodes/` shard.
/// Mirrors `.git/objects`' own fan-out scheme so individual tree listings stay small
/// as history grows, instead of one giant flat directory.
fn shard_prefix(id: &str) -> &str {
    &id[0..2]
}

/// Resolves `name` as a child tree of `parent` (if `parent` is set and has such a
/// child), swallowing lookup errors as "not found" -- a ref that's supposed to be
/// append-only should never have a corrupt child, but if it somehow did, treating it
/// as absent and rebuilding that shard fresh is a safe, self-healing fallback.
fn child_tree<'repo>(
    repo: &'repo Repository,
    parent: Option<&Tree>,
    name: &str,
) -> Option<Tree<'repo>> {
    parent
        .and_then(|t| t.get_name(name))
        .filter(|e| e.kind() == Some(ObjectType::Tree))
        .and_then(|e| repo.find_tree(e.id()).ok())
}

/// A `TreeBuilder` seeded from `parent`'s child tree named `name`, or empty if there
/// isn't one yet.
fn child_treebuilder<'repo>(
    repo: &'repo Repository,
    parent: Option<&Tree>,
    name: &str,
) -> Result<TreeBuilder<'repo>> {
    Ok(repo.treebuilder(child_tree(repo, parent, name).as_ref())?)
}

/// Writes the tree layout for `refs/cvc/main` (or any other CVC shadow ref):
///
/// ```text
/// FORMAT                              # version marker, currently "3"
/// nodes/<id[0..2]>/<interaction-id>.json   # sharded interaction blobs (immutable once written)
/// by-commit/<commit-sha>/<interaction-id>  # zero-byte pointer entries, a pure index
/// links/<interaction-id>/<commit-sha>.json # immutable post-node automatic link event
/// ```
///
/// Legacy repos may still have interactions stored as flat `<id>.json` files at the
/// root from before this layout existed -- those are left untouched (never rewritten,
/// per the "immutable blobs" rule) and `pull_from_ref` keeps reading them. New pushes
/// always write the new layout, so a repo naturally converges on it over time without
/// a disruptive one-time migration.
pub fn push_to_ref(repo: &Repository, db: &CvcStore, ref_name: &str) -> Result<()> {
    if !validate_ref_name(ref_name) {
        return Err(SyncError::Ref(format!("Invalid ref name: {}", ref_name)));
    }

    let all_ids = db.get_all_interaction_ids()?;

    let mut root_builder = repo.treebuilder(None)?;
    let mut parent_commit = None;
    let mut existing_root_tree: Option<Tree> = None;

    if let Ok(reference) = repo.find_reference(ref_name) {
        if let Ok(commit) = reference.peel_to_commit() {
            let tree = commit.tree()?;
            root_builder = repo.treebuilder(Some(&tree))?;
            existing_root_tree = Some(tree);
            parent_commit = Some(commit);
        } else if let Ok(obj) = reference.peel(ObjectType::Tree) {
            if let Some(tree) = obj.as_tree() {
                root_builder = repo.treebuilder(Some(tree))?;
                existing_root_tree = Some(repo.find_tree(tree.id())?);
            }
        }
    }

    let existing_nodes_tree = child_tree(repo, existing_root_tree.as_ref(), "nodes");
    let existing_by_commit_tree = child_tree(repo, existing_root_tree.as_ref(), "by-commit");
    let existing_links_tree = child_tree(repo, existing_root_tree.as_ref(), "links");

    // --- nodes/<prefix>/<id>.json ---
    let mut prefix_builders: HashMap<String, TreeBuilder> = HashMap::new();

    for id in &all_ids {
        let legacy_filename = format!("{}.json", id.as_str());
        if root_builder.get(&legacy_filename)?.is_some() {
            continue; // already synced under the pre-v2 flat layout; leave it alone
        }

        let id_str = id.as_str();
        let prefix = shard_prefix(&id_str);
        let already_sharded = child_tree(repo, existing_nodes_tree.as_ref(), prefix)
            .map(|sub| sub.get_name(&legacy_filename).is_some())
            .unwrap_or(false);
        if already_sharded {
            continue;
        }

        let builder = match prefix_builders.entry(prefix.to_string()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let b = child_treebuilder(repo, existing_nodes_tree.as_ref(), prefix)?;
                e.insert(b)
            }
        };
        if builder.get(&legacy_filename)?.is_some() {
            continue;
        }

        let interaction = db.get_interaction(id)?.ok_or_else(|| {
            SyncError::Db(crate::db::DbError::Migration("Interaction missing".into()))
        })?;
        let context_items = db.get_context_items(id)?;
        let tool_executions = db.get_tool_executions(id)?;
        let artifact_links = db.get_artifact_links(id)?;

        let node = SyncNode {
            interaction,
            context_items,
            tool_executions,
            artifact_links,
        };

        let json = serde_json::to_string_pretty(&node)?;
        let blob_oid = repo.blob(json.as_bytes())?;
        builder.insert(&legacy_filename, blob_oid, FileMode::Blob.into())?;
    }

    if !prefix_builders.is_empty() {
        let mut nodes_builder = child_treebuilder(repo, existing_root_tree.as_ref(), "nodes")?;
        for (prefix, builder) in &prefix_builders {
            let oid = builder.write()?;
            nodes_builder.insert(prefix, oid, FileMode::Tree.into())?;
        }
        let nodes_oid = nodes_builder.write()?;
        root_builder.insert("nodes", nodes_oid, FileMode::Tree.into())?;
    }

    // --- by-commit/<sha>/<id>: written/updated whenever artifact links exist ---
    let mut commit_builders: HashMap<String, TreeBuilder> = HashMap::new();

    for id in &all_ids {
        for link in db.get_artifact_links(id)? {
            let commit_sha = link.git_commit_hash.as_str().to_string();
            let pointer_name = id.as_str();

            let already_indexed = child_tree(repo, existing_by_commit_tree.as_ref(), &commit_sha)
                .map(|sub| sub.get_name(&pointer_name).is_some())
                .unwrap_or(false);
            if already_indexed {
                continue;
            }

            let builder = match commit_builders.entry(commit_sha.clone()) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    let b = child_treebuilder(repo, existing_by_commit_tree.as_ref(), &commit_sha)?;
                    e.insert(b)
                }
            };
            if builder.get(&pointer_name)?.is_none() {
                // Zero-byte pointer: by-commit/ is a pure index, the content lives in nodes/.
                let empty_oid = repo.blob(b"")?;
                builder.insert(&pointer_name, empty_oid, FileMode::Blob.into())?;
            }
        }
    }

    if !commit_builders.is_empty() {
        let mut by_commit_builder =
            child_treebuilder(repo, existing_root_tree.as_ref(), "by-commit")?;
        for (commit_sha, builder) in &commit_builders {
            let oid = builder.write()?;
            by_commit_builder.insert(commit_sha, oid, FileMode::Tree.into())?;
        }
        let by_commit_oid = by_commit_builder.write()?;
        root_builder.insert("by-commit", by_commit_oid, FileMode::Tree.into())?;
    }

    // --- links/<interaction-id>/<commit-sha>.json ---
    // Only automatic links need this append-only side channel. Historical
    // custom link types remain available in their legacy node blobs.
    let mut link_builders: HashMap<String, TreeBuilder> = HashMap::new();
    for id in &all_ids {
        let id_string = id.as_str();
        for link in db.get_artifact_links(id)? {
            if !is_automatic_link_type(&link.link_type)
                || !is_safe_commit_sha(link.git_commit_hash.as_str())
            {
                continue;
            }
            let filename = format!("{}.json", link.git_commit_hash.as_str());
            let exists = child_tree(repo, existing_links_tree.as_ref(), &id_string)
                .map(|tree| tree.get_name(&filename).is_some())
                .unwrap_or(false);
            if exists {
                let entry = child_tree(repo, existing_links_tree.as_ref(), &id_string)
                    .and_then(|tree| tree.get_name(&filename).map(|entry| entry.id()))
                    .ok_or_else(|| SyncError::Ref("missing existing link record".into()))?;
                let existing: SyncLinkRecord =
                    serde_json::from_slice(repo.find_blob(entry)?.content())?;
                let intended = SyncLinkRecord {
                    interaction_id: id_string.clone(),
                    git_commit_hash: link.git_commit_hash.as_str().to_owned(),
                    link_type: link.link_type.clone(),
                    linked_by: link.linked_by.clone(),
                };
                if !link_record_equal(&existing, &intended) {
                    return Err(SyncError::Ref(
                        "immutable link record conflicts with local link".into(),
                    ));
                }
                continue;
            }
            let builder = match link_builders.entry(id_string.clone()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => entry.insert(child_treebuilder(
                    repo,
                    existing_links_tree.as_ref(),
                    &id_string,
                )?),
            };
            if builder.get(&filename)?.is_none() {
                let record = SyncLinkRecord {
                    interaction_id: id_string.clone(),
                    git_commit_hash: link.git_commit_hash.as_str().to_owned(),
                    link_type: link.link_type,
                    linked_by: link.linked_by,
                };
                let blob_oid = repo.blob(serde_json::to_vec(&record)?.as_slice())?;
                builder.insert(&filename, blob_oid, FileMode::Blob.into())?;
            }
        }
    }
    if !link_builders.is_empty() {
        let mut links_builder = child_treebuilder(repo, existing_root_tree.as_ref(), "links")?;
        for (interaction_id, builder) in &link_builders {
            links_builder.insert(interaction_id, builder.write()?, FileMode::Tree.into())?;
        }
        root_builder.insert("links", links_builder.write()?, FileMode::Tree.into())?;
    }

    // --- FORMAT marker: upgrade known numeric predecessors, never downgrade ---
    let existing_format = existing_root_tree
        .as_ref()
        .and_then(|tree| tree.get_name("FORMAT"))
        .and_then(|entry| repo.find_blob(entry.id()).ok())
        .and_then(|blob| {
            std::str::from_utf8(blob.content())
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
        });
    if existing_format.is_none_or(|version| version < 3) {
        let format_oid = repo.blob(SYNC_FORMAT_VERSION)?;
        root_builder.insert("FORMAT", format_oid, FileMode::Blob.into())?;
    }

    // Write Tree
    let new_tree_oid = root_builder.write()?;

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

/// Recursively collects `(interaction_id, blob_oid)` pairs for every `<id>.json` blob
/// in `tree`, up to `max_depth` levels below it. Used to read both the legacy flat
/// layout (depth 0) and the v2 `nodes/<prefix>/<id>.json` layout (depth 2) in one pass.
/// Explicitly skips `by-commit/` -- its entries never end in `.json` and it can be
/// large, so there's no reason to even open it here.
fn collect_interaction_blobs(
    repo: &Repository,
    tree: &Tree,
    max_depth: usize,
    out: &mut Vec<(String, git2::Oid)>,
) -> Result<()> {
    for entry in tree.iter() {
        let name = entry.name().unwrap_or_default();
        match entry.kind() {
            Some(ObjectType::Blob) => {
                if let Some(id_str) = name.strip_suffix(".json") {
                    out.push((id_str.to_string(), entry.id()));
                }
            }
            Some(ObjectType::Tree) if max_depth > 0 => {
                if name == "by-commit" || name == "links" {
                    continue;
                }
                let sub = repo.find_tree(entry.id())?;
                collect_interaction_blobs(repo, &sub, max_depth - 1, out)?;
            }
            _ => {}
        }
    }
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

    // 2. Collect (interaction-id, blob-oid) pairs from BOTH layouts this ref might
    // contain: legacy flat `<id>.json` files at the root, and/or the v2 sharded
    // `nodes/<prefix>/<id>.json` layout. `by-commit/` is skipped entirely -- it's a
    // pure index for the Reviewer's PR-scoped fetch path. `links/` is read
    // separately because it contains post-node append-only events.
    let mut blob_refs: Vec<(String, git2::Oid)> = Vec::new();
    collect_interaction_blobs(repo, tree, 2, &mut blob_refs)?;

    // We want to verify existing IDs in DB to skip reading blobs
    let existing_ids_vec = db.get_all_interaction_ids()?;
    let existing_ids: HashSet<String> = existing_ids_vec
        .into_iter()
        .map(|id| id.as_str().to_string())
        .collect();

    let mut nodes_to_insert = Vec::new();

    for (id_str, blob_oid) in blob_refs {
        let blob = repo.find_blob(blob_oid)?;
        let content = std::str::from_utf8(blob.content())
            .map_err(|e| SyncError::Serde(serde_json::Error::custom(e.to_string())))?;

        let node: SyncNode = serde_json::from_str(content)?;
        if node.interaction.id.to_string() != id_str {
            return Err(SyncError::Ref(
                "interaction blob filename/id mismatch".into(),
            ));
        }
        validate_legacy_links(&node)?;
        if !existing_ids.contains(&id_str) {
            nodes_to_insert.push(node);
        }
    }

    let records = collect_link_records(repo, tree)?;
    let mut known_interactions = existing_ids;
    known_interactions.extend(
        nodes_to_insert
            .iter()
            .map(|node| node.interaction.id.to_string()),
    );
    validate_records_against_store(db, &records, &known_interactions, &nodes_to_insert)?;

    // 5. Robust Topological Sort (DFS)
    // We need to ensure that if B depends on A (B.parent_id = A.id), A is inserted first.
    // (Duplicate ids across the two layouts -- e.g. a pre-upgrade repo re-pushed after
    // upgrading -- resolve harmlessly here: last write wins into node_map, and content
    // is identical either way since interaction ids are content-addressed.)

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
            db.import_artifact_link(
                &link.interaction_id,
                &link.git_commit_hash,
                &link.link_type,
                link.linked_by.as_deref(),
            )?;
        }
    }

    for record in records {
        let interaction_id: crate::models::InteractionId = record
            .interaction_id
            .parse()
            .map_err(|_| SyncError::Ref("invalid link interaction id".into()))?;
        if db.get_interaction(&interaction_id)?.is_none() {
            return Err(SyncError::Ref(
                "link record references missing interaction".into(),
            ));
        }
        let commit_sha = crate::models::CommitSha::new(record.git_commit_hash);
        db.link_automatic_interactions(
            &[interaction_id],
            &commit_sha,
            &record.link_type,
            record.linked_by.as_deref(),
        )?;
    }

    Ok(())
}

fn link_record_equal(left: &SyncLinkRecord, right: &SyncLinkRecord) -> bool {
    left.interaction_id == right.interaction_id
        && left.git_commit_hash == right.git_commit_hash
        && left.link_type == right.link_type
        && left.linked_by == right.linked_by
}

fn validate_legacy_links(node: &SyncNode) -> Result<()> {
    for link in &node.artifact_links {
        if link.interaction_id != node.interaction.id
            || git2::Oid::from_str(link.git_commit_hash.as_str()).is_err()
            || link.link_type.trim().is_empty()
            || link
                .linked_by
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
        {
            return Err(SyncError::Ref("invalid embedded artifact link".into()));
        }
    }
    Ok(())
}

fn validate_records_against_store(
    db: &CvcStore,
    records: &[SyncLinkRecord],
    known_interactions: &HashSet<String>,
    incoming_nodes: &[SyncNode],
) -> Result<()> {
    for record in records {
        let interaction_id: crate::models::InteractionId = record
            .interaction_id
            .parse()
            .map_err(|_| SyncError::Ref("invalid link interaction id".into()))?;
        if !known_interactions.contains(&record.interaction_id) {
            return Err(SyncError::Ref(
                "link record references missing interaction".into(),
            ));
        }
        let existing = db.get_artifact_links(&interaction_id)?;
        for link in existing
            .into_iter()
            .filter(|link| link.git_commit_hash.as_str() == record.git_commit_hash)
        {
            if link.link_type != record.link_type
                || matches!((&link.linked_by, &record.linked_by), (Some(left), Some(right)) if left != right)
            {
                return Err(SyncError::Ref(
                    "link record conflicts with local provenance".into(),
                ));
            }
        }
        if let Some(node) = incoming_nodes
            .iter()
            .find(|node| node.interaction.id == interaction_id)
        {
            for link in node
                .artifact_links
                .iter()
                .filter(|link| link.git_commit_hash.as_str() == record.git_commit_hash)
            {
                if link.link_type != record.link_type
                    || matches!((&link.linked_by, &record.linked_by), (Some(left), Some(right)) if left != right)
                {
                    return Err(SyncError::Ref(
                        "link record conflicts with embedded provenance".into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn collect_link_records(repo: &Repository, root: &Tree) -> Result<Vec<SyncLinkRecord>> {
    let Some(links_tree) = child_tree(repo, Some(root), "links") else {
        return Ok(Vec::new());
    };
    let mut records = Vec::new();
    for interaction_entry in links_tree.iter() {
        let interaction_id = interaction_entry.name().unwrap_or_default();
        interaction_id
            .parse::<crate::models::InteractionId>()
            .map_err(|_| SyncError::Ref("invalid links path interaction id".into()))?;
        let interaction_tree = repo.find_tree(interaction_entry.id())?;
        for record_entry in interaction_tree.iter() {
            let filename = record_entry.name().unwrap_or_default();
            let commit = filename
                .strip_suffix(".json")
                .filter(|value| is_safe_commit_sha(value))
                .ok_or_else(|| SyncError::Ref("invalid links path commit SHA".into()))?;
            let blob = repo.find_blob(record_entry.id())?;
            let record: SyncLinkRecord = serde_json::from_slice(blob.content())?;
            if record.interaction_id != interaction_id
                || record.git_commit_hash != commit
                || !is_automatic_link_type(&record.link_type)
            {
                return Err(SyncError::Ref("inconsistent link record".into()));
            }
            records.push(record);
        }
    }
    Ok(records)
}

/// Fetches `refs/cvc/main` from `remote_name` (via the system `git` CLI, falling back
/// to libgit2 with SSH-agent auth if that fails) and ingests any new interactions into
/// `db`, then fast-forwards the local shadow ref to match what was fetched.
///
/// This is the piece that lets a fresh checkout -- a new clone, a different machine, a
/// different agentic harness -- catch up on history pushed from elsewhere before doing
/// its own work. Returns the number of newly ingested interactions.
pub fn fetch_and_pull(repo: &Repository, db: &CvcStore, remote_name: &str) -> Result<usize> {
    let ref_name = "refs/cvc/main";
    let remote_tracking_ref = format!("refs/remotes/{}/cvc/main", remote_name);
    let refspec = format!("{}:{}", ref_name, remote_tracking_ref);

    // Bare repos have no workdir; run `git fetch` from the repo path itself in that
    // case. Without pinning this explicitly, the subprocess inherits *our* cwd, which
    // may well be a different repository entirely.
    let git_cli_success = std::process::Command::new("git")
        .arg("fetch")
        .arg(remote_name)
        .arg(&refspec)
        .current_dir(repo.workdir().unwrap_or_else(|| repo.path()))
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !git_cli_success {
        let mut remote = repo.find_remote(remote_name)?;
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(|_url, username_from_url, _allowed_types| {
            git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
        });
        let mut fetch_opts = git2::FetchOptions::new();
        fetch_opts.remote_callbacks(callbacks);
        remote.fetch(&[&refspec], Some(&mut fetch_opts), None)?;
    }

    if repo.find_reference(&remote_tracking_ref).is_err() {
        return Ok(0);
    }

    let before_count = db.get_all_interaction_ids()?.len();
    pull_from_ref(repo, db, &remote_tracking_ref)?;
    let new_count = db
        .get_all_interaction_ids()?
        .len()
        .saturating_sub(before_count);

    if let Some(oid) = repo.find_reference(&remote_tracking_ref)?.target() {
        repo.reference(ref_name, oid, true, "cvc sync: pull")?;
    }

    Ok(new_count)
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
