use crate::db::CvcStore;
use crate::models::{
    ArtifactLink, ContextItem, DerivationEvent, Interaction, RangeEvidence, RedactionPlan,
    Tombstone, ToolExecution,
};
use chrono::{Timelike, Utc};
use git2::{Direction, FileMode, ObjectType, Repository, Tree, TreeBuilder};
use serde::de::Error as SerdeError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use thiserror::Error;

/// Tree layout version written by outbound projection. Stored at the ref tree's root as a
/// blob named `FORMAT`.
const SYNC_FORMAT_VERSION: &[u8] = b"5";
const MAX_SYNC_NODES: usize = 10_000;
const MAX_SYNC_BLOB_BYTES: usize = 4 * 1024 * 1024;
const MAX_SYNC_TOTAL_BYTES: usize = 128 * 1024 * 1024;
const MAX_SYNC_LINK_EVENTS: usize = 20_000;
const MAX_SYNC_DERIVATION_EVENTS: usize = 20_000;
const MAX_SYNC_RANGES: usize = 10_000;
const MAX_SYNC_TOMBSTONES: usize = 4_096;
const MAX_SYNC_TOMBSTONE_BYTES: usize = 16 * 1024;
const MAX_SYNC_LINKS_PER_NODE: usize = 256;
const MAX_SYNC_CONTEXT_ITEMS: usize = 100_000;
const MAX_SYNC_TOOL_EXECUTIONS: usize = 100_000;
const MAX_SYNC_EMBEDDED_LINKS: usize = 100_000;

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

/// Bounds used while decoding an untrusted sync tree.  The default is the
/// production policy; the explicit form also lets callers exercise rejection
/// paths without constructing multi-gigabyte Git fixtures.
#[derive(Clone, Copy, Debug)]
pub struct SyncReadLimits {
    pub max_tombstones: usize,
    pub max_tombstone_bytes: usize,
    pub max_total_bytes: usize,
}

impl Default for SyncReadLimits {
    fn default() -> Self {
        Self {
            max_tombstones: MAX_SYNC_TOMBSTONES,
            max_tombstone_bytes: MAX_SYNC_TOMBSTONE_BYTES,
            max_total_bytes: MAX_SYNC_TOTAL_BYTES,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct SyncNode {
    pub interaction: Interaction,
    pub context_items: Vec<ContextItem>,
    pub tool_executions: Vec<ToolExecution>,
    pub artifact_links: Vec<ArtifactLink>,
}

fn validate_tombstone(t: &Tombstone) -> Result<()> {
    if t.format != "cvc.tombstone/v1"
        || t.version != 1
        || t.deleted_at.timestamp() < 0
        || t.previous_node_oid
            .as_deref()
            .is_some_and(|oid| !is_safe_commit_sha(oid))
    {
        return Err(SyncError::Ref("invalid tombstone".into()));
    }
    Ok(())
}
pub enum ProjectionResult<'repo> {
    NoChanges,
    Candidate {
        oid: git2::Oid,
        candidate: CandidateRef<'repo>,
        ids: Vec<crate::models::InteractionId>,
    },
}

/// Owns a local projection staging ref for its entire lifetime.  Candidate refs
/// have no durable meaning, so every path that abandons a candidate removes it.
pub struct CandidateRef<'repo> {
    repo: &'repo Repository,
    ref_name: Option<String>,
}

impl<'repo> CandidateRef<'repo> {
    fn new(repo: &'repo Repository, ref_name: String) -> Self {
        Self {
            repo,
            ref_name: Some(ref_name),
        }
    }

    pub fn ref_name(&self) -> &str {
        self.ref_name
            .as_deref()
            .expect("candidate reference was disarmed")
    }

    /// Transfer ownership only when a caller deliberately retains a staging ref.
    /// Normal publication must not disarm candidates.
    pub fn disarm(mut self) -> String {
        self.ref_name
            .take()
            .expect("candidate reference was already disarmed")
    }
}

impl Drop for CandidateRef<'_> {
    fn drop(&mut self) {
        if let Some(ref_name) = self.ref_name.take() {
            let _ = cleanup_projection_ref(self.repo, &ref_name);
        }
    }
}

/// A local-only hard-redaction candidate. Dropping it deletes `temporary_ref`;
/// callers must explicitly disarm it if they intentionally retain the staging ref.
pub struct RedactionCandidate<'repo> {
    pub plan: RedactionPlan,
    candidate: CandidateRef<'repo>,
}

impl<'repo> RedactionCandidate<'repo> {
    pub fn temporary_ref(&self) -> &str {
        self.candidate.ref_name()
    }
    pub fn disarm(self) -> String {
        self.candidate.disarm()
    }
}

/// Immutable post-node link event. Node blobs remain immutable, so links made
/// after a floating node was first pushed are represented here instead.
#[derive(Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct SyncLinkRecord {
    interaction_id: String,
    git_commit_hash: String,
    link_type: String,
    #[serde(default)]
    linked_by: Option<String>,
}

fn is_safe_commit_sha(value: &str) -> bool {
    // This libgit2 build is SHA-1 only; reject SHA-256-shaped values rather
    // than claiming repository-algorithm validation we cannot perform.
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
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

/// Rebuild only affected indexes; blobs remain in the object database but become
/// unreachable from the current v4 tree. This is suppression, not object erasure.
fn remove_tombstoned_projection_paths(
    repo: &Repository,
    root: &mut TreeBuilder,
    parent: Option<&Tree>,
    ids: &HashSet<String>,
) -> Result<()> {
    for id in ids {
        // A v4 projection normally has only sharded nodes. `TreeBuilder::remove`
        // errors for an absent legacy flat entry, which previously made every
        // tombstone fail before its suppression record could be published.
        let legacy = format!("{id}.json");
        if root.get(&legacy)?.is_some() {
            root.remove(&legacy)?;
        }
    }
    if let Some(nodes) = child_tree(repo, parent, "nodes") {
        let mut nb = repo.treebuilder(Some(&nodes))?;
        let mut changed = false;
        for id in ids {
            let p = shard_prefix(id);
            if let Some(sub) = child_tree(repo, Some(&nodes), p) {
                let mut sb = repo.treebuilder(Some(&sub))?;
                if sb.remove(format!("{id}.json")).is_ok() {
                    nb.insert(p, sb.write()?, FileMode::Tree.into())?;
                    changed = true;
                }
            }
        }
        if changed {
            root.insert("nodes", nb.write()?, FileMode::Tree.into())?;
        }
    }
    if let Some(links) = child_tree(repo, parent, "links") {
        let mut b = repo.treebuilder(Some(&links))?;
        let mut changed = false;
        for id in ids {
            if b.remove(id).is_ok() {
                changed = true;
            }
        }
        if changed {
            root.insert("links", b.write()?, FileMode::Tree.into())?;
        }
    }
    if let Some(index) = child_tree(repo, parent, "by-commit") {
        let mut ib = repo.treebuilder(Some(&index))?;
        let mut changed = false;
        for entry in index.iter() {
            if entry.kind() != Some(ObjectType::Tree) {
                continue;
            }
            let name = entry.name().unwrap_or_default();
            let sub = repo.find_tree(entry.id())?;
            let mut sb = repo.treebuilder(Some(&sub))?;
            let mut dirty = false;
            for id in ids {
                if sb.remove(id).is_ok() {
                    dirty = true;
                }
            }
            if dirty {
                ib.insert(name, sb.write()?, FileMode::Tree.into())?;
                changed = true;
            }
        }
        if changed {
            root.insert("by-commit", ib.write()?, FileMode::Tree.into())?;
        }
    }
    Ok(())
}

fn remove_if_present(builder: &mut TreeBuilder<'_>, name: &str) -> Result<bool> {
    if builder.get(name)?.is_some() {
        builder.remove(name)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn count_tree_entries(repo: &Repository, tree: &Tree) -> Result<u64> {
    let mut total = 0;
    for entry in tree {
        total += 1;
        if entry.kind() == Some(ObjectType::Tree) {
            total += count_tree_entries(repo, &repo.find_tree(entry.id())?)?;
        }
    }
    Ok(total)
}

/// Remove the complete derivation closure for suppressed interactions.  Event
/// identity, not iteration order, drives the fixed point, so multi-hop DAGs are
/// removed even when their blobs sort before their sources.
fn prune_event_closure(
    repo: &Repository,
    events: &Tree<'_>,
    suppressed: &HashSet<String>,
) -> Result<git2::Oid> {
    let mut parsed = Vec::new();
    for shard in events.iter() {
        if shard.kind() != Some(ObjectType::Tree) {
            return Err(SyncError::Ref("invalid events tree".into()));
        }
        let shard_name = shard.name().unwrap_or_default().to_owned();
        for entry in repo.find_tree(shard.id())?.iter() {
            if entry.kind() != Some(ObjectType::Blob) {
                return Err(SyncError::Ref("invalid event blob".into()));
            }
            let event: DerivationEvent =
                serde_json::from_slice(repo.find_blob(entry.id())?.content())?;
            if !validate_derivation_event(&event) {
                return Err(SyncError::Ref("invalid event in redaction baseline".into()));
            }
            parsed.push((
                shard_name.clone(),
                entry.name().unwrap_or_default().to_owned(),
                event,
            ));
        }
    }
    let mut removed: HashSet<String> = parsed
        .iter()
        .filter(|(_, _, event)| {
            suppressed.contains(&event.interaction_id.to_string())
                || event.source_event_ids.iter().any(|source| {
                    suppressed.iter().any(|id| {
                        source.starts_with(&format!("legacy:{id}:"))
                            || (source.starts_with("endpoint:")
                                && source.ends_with(&format!(":{id}")))
                    })
                })
        })
        .map(|(_, _, event)| event.event_id.clone())
        .collect();
    loop {
        let before = removed.len();
        for (_, _, event) in &parsed {
            if event.source_event_ids.iter().any(|id| removed.contains(id)) {
                removed.insert(event.event_id.clone());
            }
        }
        if removed.len() == before {
            break;
        }
    }
    let mut outer = repo.treebuilder(Some(events))?;
    for shard in events.iter() {
        let shard_name = shard.name().unwrap_or_default();
        let subtree = repo.find_tree(shard.id())?;
        let mut builder = repo.treebuilder(Some(&subtree))?;
        for (_, file, event) in parsed.iter().filter(|(name, _, _)| name == shard_name) {
            if removed.contains(&event.event_id) {
                builder.remove(file)?;
            }
        }
        outer.insert(shard_name, builder.write()?, FileMode::Tree.into())?;
    }
    outer.write().map_err(Into::into)
}

/// Immutable range payloads carry no interaction identity. Redaction follows
/// event ownership/closure and removes a range only when no surviving event
/// references it.
fn prune_range_sources(
    repo: &Repository,
    root: &Tree<'_>,
    suppressed: &HashSet<String>,
) -> Result<Option<git2::Oid>> {
    let Some(ranges) = child_tree(repo, Some(root), "ranges") else {
        return Ok(None);
    };
    let mut bytes = 0;
    let surviving = retain_event_closure(
        collect_derivation_events(repo, root, &mut bytes, MAX_SYNC_TOTAL_BYTES)?,
        suppressed,
        &HashSet::new(),
    );
    let referenced: HashSet<_> = surviving
        .iter()
        .filter_map(|event| event.range_id.as_deref())
        .collect();
    let mut outer = repo.treebuilder(Some(&ranges))?;
    for shard in ranges.iter() {
        let name = shard.name().unwrap_or_default();
        let subtree = repo.find_tree(shard.id())?;
        let mut builder = repo.treebuilder(Some(&subtree))?;
        for entry in subtree.iter() {
            let range: RangeEvidence =
                serde_json::from_slice(repo.find_blob(entry.id())?.content())?;
            if !referenced.contains(range.range_id.as_str()) {
                builder.remove(entry.name().unwrap_or_default())?;
            }
        }
        outer.insert(name, builder.write()?, FileMode::Tree.into())?;
    }
    Ok(Some(outer.write()?))
}

fn retain_event_closure(
    events: Vec<DerivationEvent>,
    suppressed: &HashSet<String>,
    _baseline_event_ids: &HashSet<String>,
) -> Vec<DerivationEvent> {
    let mut removed: HashSet<String> = events
        .iter()
        .filter(|event| {
            suppressed.contains(&event.interaction_id.to_string())
                || event.source_event_ids.iter().any(|source| {
                    suppressed
                        .iter()
                        .any(|id| source.starts_with(&format!("legacy:{id}:")))
                })
        })
        .map(|event| event.event_id.clone())
        .collect();
    loop {
        let before = removed.len();
        for event in &events {
            if event.source_event_ids.iter().any(|id| removed.contains(id)) {
                removed.insert(event.event_id.clone());
            }
        }
        if before == removed.len() {
            break;
        }
    }
    events
        .into_iter()
        .filter(|event| !removed.contains(&event.event_id))
        .collect()
}

/// Build a parentless local replacement from an exact fetched v4 baseline.
/// It never contacts a remote and leaves the candidate ref owned by RAII.
pub fn build_hard_redaction_plan<'repo>(
    repo: &'repo Repository,
    baseline_ref: Option<&str>,
    destination_fingerprint: &str,
    target: &crate::models::InteractionId,
) -> Result<RedactionCandidate<'repo>> {
    let name = match baseline_ref {
        Some(name) => name,
        None => {
            return Err(SyncError::Ref(
                "hard redaction requires an existing v4 baseline".into(),
            ))
        }
    };
    if !validate_ref_name(name) {
        return Err(SyncError::Ref("invalid redaction baseline ref".into()));
    }
    let commit = repo.find_reference(name)?.peel_to_commit()?;
    let expected_remote_tip = Some(commit.id().to_string());
    let tree = commit.tree()?;
    {
        let format = tree
            .get_name("FORMAT")
            .ok_or_else(|| SyncError::Ref("hard redaction requires v5 FORMAT".into()))?;
        if repo.find_blob(format.id())?.content().trim_ascii() != b"5" {
            return Err(SyncError::Ref(
                "hard redaction requires exact v5 baseline".into(),
            ));
        }
    }
    let id = target.as_str();
    let file = format!("{id}.json");
    let shard = shard_prefix(&id);
    let tombstones = child_tree(repo, Some(&tree), "tombstones");
    let tombstone_oid = child_tree(repo, tombstones.as_ref(), shard)
        .and_then(|s| s.get_name(&file).map(|e| e.id()))
        .ok_or_else(|| {
            SyncError::Ref("destination-scoped tombstone is required before hard redaction".into())
        })?;
    let tombstone: Tombstone = serde_json::from_slice(repo.find_blob(tombstone_oid)?.content())?;
    validate_tombstone(&tombstone)?;
    if tombstone.interaction_id != *target {
        return Err(SyncError::Ref("tombstone target mismatch".into()));
    }

    let before = count_tree_entries(repo, &tree)?;
    let mut root = repo.treebuilder(Some(&tree))?;
    let mut removed_nodes = u64::from(remove_if_present(&mut root, &file)?);
    if let Some(nodes) = child_tree(repo, Some(&tree), "nodes") {
        if let Some(sub) = child_tree(repo, Some(&nodes), shard) {
            let mut sb = repo.treebuilder(Some(&sub))?;
            if remove_if_present(&mut sb, &file)? {
                removed_nodes += 1;
            }
            let mut nb = repo.treebuilder(Some(&nodes))?;
            nb.insert(shard, sb.write()?, FileMode::Tree.into())?;
            root.insert("nodes", nb.write()?, FileMode::Tree.into())?;
        }
    }
    let mut removed_by_commit_entries = 0;
    if let Some(index) = child_tree(repo, Some(&tree), "by-commit") {
        let mut ib = repo.treebuilder(Some(&index))?;
        for entry in index.iter() {
            if entry.kind() != Some(ObjectType::Tree) {
                continue;
            }
            let sub = repo.find_tree(entry.id())?;
            let mut sb = repo.treebuilder(Some(&sub))?;
            if remove_if_present(&mut sb, &id)? {
                removed_by_commit_entries += 1;
                ib.insert(
                    entry.name().unwrap_or_default(),
                    sb.write()?,
                    FileMode::Tree.into(),
                )?;
            }
        }
        root.insert("by-commit", ib.write()?, FileMode::Tree.into())?;
    }
    let mut removed_link_entries = 0;
    if let Some(links) = child_tree(repo, Some(&tree), "links") {
        let mut lb = repo.treebuilder(Some(&links))?;
        if remove_if_present(&mut lb, &id)? {
            removed_link_entries = 1;
        }
        root.insert("links", lb.write()?, FileMode::Tree.into())?;
    }
    if let Some(events) = child_tree(repo, Some(&tree), "events") {
        root.insert(
            "events",
            prune_event_closure(repo, &events, &HashSet::from([id.clone()]))?,
            FileMode::Tree.into(),
        )?;
    }
    if let Some(ranges) = prune_range_sources(repo, &tree, &HashSet::from([id.clone()]))? {
        root.insert("ranges", ranges, FileMode::Tree.into())?;
    }
    let redacted_without_index = repo.find_tree(root.write()?)?;
    validate_by_commit_structure(repo, &redacted_without_index)?;
    let endpoints = projected_endpoints(repo, &redacted_without_index)?;
    root = repo.treebuilder(Some(&redacted_without_index))?;
    if endpoints.is_empty() {
        let _ = remove_if_present(&mut root, "by-commit")?;
    } else {
        root.insert(
            "by-commit",
            build_by_commit_tree(repo, &endpoints)?,
            FileMode::Tree.into(),
        )?;
    }
    let replacement_tree = root.write()?;
    let replacement = repo.find_tree(replacement_tree)?;
    // Validate every representation that the reader understands is gone; the
    // tombstone is intentionally excluded and retained.
    let absent = replacement.get_name(&file).is_none()
        && child_tree(repo, Some(&replacement), "nodes")
            .and_then(|n| child_tree(repo, Some(&n), shard))
            .and_then(|s| s.get_name(&file).map(|_| ()))
            .is_none()
        && child_tree(repo, Some(&replacement), "links")
            .and_then(|l| l.get_name(&id).map(|_| ()))
            .is_none();
    if !absent {
        return Err(SyncError::Ref(
            "redaction validation found target in replacement tree".into(),
        ));
    }
    if let Some(index) = child_tree(repo, Some(&replacement), "by-commit") {
        for e in index.iter() {
            if e.kind() == Some(ObjectType::Tree) && repo.find_tree(e.id())?.get_name(&id).is_some()
            {
                return Err(SyncError::Ref(
                    "redaction validation found target index entry".into(),
                ));
            }
        }
    }
    assert_no_redacted_event_reference(repo, &replacement, target)?;
    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("cvc", "cvc@local"))?;
    let temp_ref = format!("refs/cvc/candidate-redact-{}", uuid::Uuid::new_v4());
    let oid = repo.commit(
        Some(&temp_ref),
        &sig,
        &sig,
        "CVC local hard-redaction replacement",
        &replacement,
        &[],
    )?;
    let repository_fingerprint =
        hex::encode(Sha256::digest(repo.path().to_string_lossy().as_bytes()));
    let after = count_tree_entries(repo, &replacement)?;
    let plan = RedactionPlan { format: "cvc.redaction-plan/v1".into(), version: 1, repository_fingerprint, destination_fingerprint: destination_fingerprint.into(), target_id: target.clone(), expected_remote_tip, replacement_commit: oid.to_string(), temporary_ref: temp_ref.clone(), removed_nodes, removed_by_commit_entries, removed_link_entries, unrelated_entries_retained: after, tombstone_oid: tombstone_oid.to_string(), created_at: Utc::now().with_nanosecond(0).expect("valid"), warning: "Local planning only. Remote history rewrite is unsupported pending atomic force-with-lease transport.".into() };
    let _ = before;
    Ok(RedactionCandidate {
        plan,
        candidate: CandidateRef::new(repo, temp_ref),
    })
}

fn assert_no_redacted_event_reference(
    repo: &Repository,
    root: &Tree,
    target: &crate::models::InteractionId,
) -> Result<()> {
    let Some(events) = child_tree(repo, Some(root), "events") else {
        return Ok(());
    };
    let mut parsed = Vec::new();
    for shard in events.iter() {
        if shard.kind() != Some(ObjectType::Tree) {
            return Err(SyncError::Ref("invalid events tree".into()));
        }
        for entry in repo.find_tree(shard.id())?.iter() {
            if entry.kind() != Some(ObjectType::Blob) {
                return Err(SyncError::Ref("invalid event blob".into()));
            }
            let event: DerivationEvent =
                serde_json::from_slice(repo.find_blob(entry.id())?.content())?;
            if event.interaction_id == *target
                || event.source_event_ids.iter().any(|source| {
                    source.starts_with(&format!("legacy:{}:", target))
                        || (source.starts_with("endpoint:")
                            && source.ends_with(&format!(":{target}")))
                })
            {
                return Err(SyncError::Ref(
                    "redaction validation found target event reference".into(),
                ));
            }
            parsed.push(event);
        }
    }
    let ids: HashSet<_> = parsed.iter().map(|event| event.event_id.as_str()).collect();
    if parsed.iter().any(|event| {
        event.source_event_ids.iter().any(|source| {
            source.len() == 64
                && source.bytes().all(|byte| byte.is_ascii_hexdigit())
                && !ids.contains(source.as_str())
        })
    }) {
        return Err(SyncError::Ref(
            "redaction validation found dangling event source".into(),
        ));
    }
    Ok(())
}

/// Ensures a plan still matches the freshly advertised/fetched tracking tip.
pub fn verify_redaction_plan(
    repo: &Repository,
    plan: &RedactionPlan,
    tracking_ref: &str,
) -> Result<bool> {
    if plan.format != "cvc.redaction-plan/v1" || plan.version != 1 {
        return Err(SyncError::Ref("invalid redaction plan".into()));
    }
    validate_redaction_replacement(repo, plan)?;
    let current = repo
        .find_reference(tracking_ref)
        .ok()
        .and_then(|r| r.target())
        .map(|o| o.to_string());
    Ok(current == plan.expected_remote_tip)
}

/// Switches only the local CVC ref after validating the replacement is parentless.
pub fn apply_hard_redaction_locally(repo: &Repository, plan: &RedactionPlan) -> Result<()> {
    validate_redaction_replacement(repo, plan)?;
    let commit = repo.find_commit(git2::Oid::from_str(&plan.replacement_commit)?)?;
    if commit.parent_count() != 0 {
        return Err(SyncError::Ref(
            "replacement commit must be parentless".into(),
        ));
    }
    let tree = commit.tree()?;
    let id = plan.target_id.as_str();
    let file = format!("{id}.json");
    let shard = shard_prefix(&id);
    if tree.get_name(&file).is_some()
        || child_tree(repo, Some(&tree), "nodes")
            .and_then(|nodes| child_tree(repo, Some(&nodes), shard))
            .and_then(|entries| entries.get_name(&file).map(|_| ()))
            .is_some()
        || child_tree(repo, Some(&tree), "links")
            .and_then(|links| links.get_name(&id).map(|_| ()))
            .is_some()
    {
        return Err(SyncError::Ref(
            "replacement still contains redaction target".into(),
        ));
    }
    if let Some(index) = child_tree(repo, Some(&tree), "by-commit") {
        for entry in index.iter() {
            if entry.kind() == Some(ObjectType::Tree)
                && repo.find_tree(entry.id())?.get_name(&id).is_some()
            {
                return Err(SyncError::Ref(
                    "replacement still contains target index".into(),
                ));
            }
        }
    }
    assert_no_redacted_event_reference(repo, &tree, &plan.target_id)?;
    repo.reference(
        "refs/cvc/main",
        commit.id(),
        true,
        "local hard-redaction apply",
    )?;
    Ok(())
}

/// Validate all locally-verifiable plan invariants before either reporting a
/// plan current or changing a ref.  This deliberately has no remote transport.
fn validate_redaction_replacement(repo: &Repository, plan: &RedactionPlan) -> Result<()> {
    let fingerprint = hex::encode(Sha256::digest(repo.path().to_string_lossy().as_bytes()));
    if plan.repository_fingerprint != fingerprint
        || plan.destination_fingerprint.len() != 64
        || !plan
            .destination_fingerprint
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(SyncError::Ref(
            "redaction plan repository or destination mismatch".into(),
        ));
    }
    let commit = repo.find_commit(git2::Oid::from_str(&plan.replacement_commit)?)?;
    if commit.parent_count() != 0 {
        return Err(SyncError::Ref(
            "replacement commit must be parentless".into(),
        ));
    }
    let tree = commit.tree()?;
    if count_tree_entries(repo, &tree)? != plan.unrelated_entries_retained {
        return Err(SyncError::Ref(
            "redaction plan replacement entry count mismatch".into(),
        ));
    }
    let id = plan.target_id.as_str();
    let file = format!("{id}.json");
    let shard = shard_prefix(&id);
    let tombstone_oid = child_tree(
        repo,
        child_tree(repo, Some(&tree), "tombstones").as_ref(),
        shard,
    )
    .and_then(|t| t.get_name(&file).map(|e| e.id()))
    .ok_or_else(|| SyncError::Ref("replacement lost required tombstone".into()))?;
    if tombstone_oid.to_string() != plan.tombstone_oid {
        return Err(SyncError::Ref(
            "redaction plan tombstone oid mismatch".into(),
        ));
    }
    let tombstone: Tombstone = serde_json::from_slice(repo.find_blob(tombstone_oid)?.content())?;
    validate_tombstone(&tombstone)?;
    if tombstone.interaction_id != plan.target_id {
        return Err(SyncError::Ref(
            "replacement tombstone target mismatch".into(),
        ));
    }
    if tree.get_name(&file).is_some()
        || child_tree(repo, Some(&tree), "nodes")
            .and_then(|n| child_tree(repo, Some(&n), shard))
            .and_then(|s| s.get_name(&file).map(|_| ()))
            .is_some()
        || child_tree(repo, Some(&tree), "links")
            .and_then(|l| l.get_name(&id).map(|_| ()))
            .is_some()
    {
        return Err(SyncError::Ref(
            "replacement still contains redaction target".into(),
        ));
    }
    if let Some(index) = child_tree(repo, Some(&tree), "by-commit") {
        if index.iter().any(|entry| {
            entry.kind() == Some(ObjectType::Tree)
                && repo
                    .find_tree(entry.id())
                    .ok()
                    .and_then(|t| t.get_name(&id).map(|_| ()))
                    .is_some()
        }) {
            return Err(SyncError::Ref(
                "replacement still contains target index".into(),
            ));
        }
    }
    assert_no_redacted_event_reference(repo, &tree, &plan.target_id)?;
    Ok(())
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
/// Build a privacy-safe outbound tree from an exact fetched remote baseline.  `baseline_ref`
/// must be the tracking ref just fetched for this remote; the local CVC ref is never a seed.
pub fn push_projection_to_ref<'repo>(
    repo: &'repo Repository,
    db: &CvcStore,
    baseline_ref: &str,
    remote_fingerprint: &str,
) -> Result<ProjectionResult<'repo>> {
    let ids = db.projection_interaction_ids(remote_fingerprint)?;
    // A deletion is an authorization scoped to one destination.  Never use a
    // locally-only or another remote's received suppression as outbound input.
    let tombstones = db.tombstones_for_projection(remote_fingerprint)?;
    if ids.is_empty() && tombstones.is_empty() {
        return Ok(ProjectionResult::NoChanges);
    }
    let temp_ref = format!("refs/cvc/candidate-{}", uuid::Uuid::new_v4());
    let oid = push_ids_to_ref(
        repo,
        db,
        &temp_ref,
        (!baseline_ref.is_empty()).then_some(baseline_ref),
        &ids,
        &tombstones,
        remote_fingerprint,
    )?;
    // `push_ids_to_ref` deliberately avoids creating a commit/ref when the
    // verified baseline already contains this exact projection.  Treat that as
    // a true no-op; handing a nonexistent temporary ref to transport would turn
    // a harmless repeat publish into a failure.
    if repo.find_reference(&temp_ref).is_err() {
        return Ok(ProjectionResult::NoChanges);
    }
    // Construct the owner immediately after the staging ref exists. From here
    // cancellation, errors, and panic unwind all clean it up via Drop.
    let candidate = CandidateRef::new(repo, temp_ref);
    Ok(ProjectionResult::Candidate {
        oid,
        candidate,
        ids,
    })
}

fn push_ids_to_ref(
    repo: &Repository,
    db: &CvcStore,
    ref_name: &str,
    baseline_ref: Option<&str>,
    all_ids: &[crate::models::InteractionId],
    tombstones: &[Tombstone],
    remote_fingerprint: &str,
) -> Result<git2::Oid> {
    if !validate_ref_name(ref_name) {
        return Err(SyncError::Ref(format!("Invalid ref name: {}", ref_name)));
    }

    let mut root_builder = repo.treebuilder(None)?;
    let mut parent_commit = None;
    let mut existing_root_tree: Option<Tree> = None;

    if let Some(seed_ref) = baseline_ref {
        if !validate_ref_name(seed_ref) {
            return Err(SyncError::Ref(format!("Invalid baseline ref: {seed_ref}")));
        }
        if let Ok(reference) = repo.find_reference(seed_ref) {
            if let Ok(commit) = reference.peel_to_commit() {
                let tree = commit.tree()?;
                validate_v5_reserved_namespaces(repo, &tree)?;
                root_builder = repo.treebuilder(Some(&tree))?;
                existing_root_tree = Some(tree);
                parent_commit = Some(commit);
            } else if let Ok(obj) = reference.peel(ObjectType::Tree) {
                if let Some(tree) = obj.as_tree() {
                    validate_v5_reserved_namespaces(repo, tree)?;
                    root_builder = repo.treebuilder(Some(tree))?;
                    existing_root_tree = Some(repo.find_tree(tree.id())?);
                }
            }
        }
    } else {
        // An empty baseline is only valid after the caller has proved the remote
        // ref absent. Never seed outbound bytes from a mutable local shadow ref.
    }

    let mut emitted_bytes = 0usize;

    // Tombstones dominate every historical projection. Remove all target paths
    // from the seeded remote tree before adding immutable suppression records.
    let tombstoned: HashSet<String> = tombstones
        .iter()
        .map(|t| t.interaction_id.to_string())
        .collect();
    if !tombstoned.is_empty() {
        remove_tombstoned_projection_paths(
            repo,
            &mut root_builder,
            existing_root_tree.as_ref(),
            &tombstoned,
        )?;
        // Event blobs are immutable, but the current projection must not retain
        // a tombstoned interaction's derivation references.
        if let Some(events) = child_tree(repo, existing_root_tree.as_ref(), "events") {
            root_builder.insert(
                "events",
                prune_event_closure(repo, &events, &tombstoned)?,
                FileMode::Tree.into(),
            )?;
        }
        if let Some(root) = existing_root_tree.as_ref() {
            if let Some(ranges) = prune_range_sources(repo, root, &tombstoned)? {
                root_builder.insert("ranges", ranges, FileMode::Tree.into())?;
            }
        }
    }
    // From this point onward the original seed is deliberately inaccessible.
    // Every namespace builder, including indexes and newly appended events,
    // starts from the materialized privacy-pruned tree.
    if !tombstoned.is_empty() {
        let pruned = repo.find_tree(root_builder.write()?)?;
        root_builder = repo.treebuilder(Some(&pruned))?;
        existing_root_tree = Some(pruned);
    }
    let existing_nodes_tree = child_tree(repo, existing_root_tree.as_ref(), "nodes");
    let existing_links_tree = child_tree(repo, existing_root_tree.as_ref(), "links");
    let existing_events_tree = child_tree(repo, existing_root_tree.as_ref(), "events");
    let existing_ranges_tree = child_tree(repo, existing_root_tree.as_ref(), "ranges");
    // --- nodes/<prefix>/<id>.json ---
    let mut prefix_builders: HashMap<String, TreeBuilder> = HashMap::new();

    for id in all_ids {
        if tombstoned.contains(&id.to_string()) {
            continue;
        }
        let legacy_filename = format!("{}.json", id.as_str());
        if let Some(entry) = root_builder.get(&legacy_filename)? {
            verify_existing_node(repo, entry.id(), &id.as_str())?;
            continue; // already synced under the pre-v2 flat layout; leave it alone
        }

        let id_str = id.as_str();
        let prefix = shard_prefix(&id_str);
        let already_sharded = child_tree(repo, existing_nodes_tree.as_ref(), prefix)
            .map(|sub| sub.get_name(&legacy_filename).is_some())
            .unwrap_or(false);
        if already_sharded {
            let entry = child_tree(repo, existing_nodes_tree.as_ref(), prefix)
                .and_then(|tree| tree.get_name(&legacy_filename).map(|entry| entry.id()))
                .ok_or_else(|| SyncError::Ref("missing immutable node".into()))?;
            verify_existing_node(repo, entry, &id.as_str())?;
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

        let mut interaction = db.get_interaction(id)?.ok_or_else(|| {
            SyncError::Db(crate::db::DbError::Migration("Interaction missing".into()))
        })?;
        let mut context_items = db.get_context_items(id)?;
        let mut tool_executions = db.get_tool_executions(id)?;
        let mut artifact_links = db.get_legacy_artifact_links(id)?;
        crate::privacy::final_scrub_for_sync(
            &mut interaction,
            &mut context_items,
            &mut tool_executions,
        )
        .map_err(|e| SyncError::Ref(format!("unsafe node serialization: {e}")))?;
        crate::privacy::final_validate_links(&mut artifact_links)
            .map_err(|e| SyncError::Ref(format!("unsafe link serialization: {e}")))?;

        let node = SyncNode {
            interaction,
            context_items,
            tool_executions,
            artifact_links,
        };

        let json = serde_json::to_vec_pretty(&node)?;
        add_sync_bytes(&mut emitted_bytes, json.len(), MAX_SYNC_TOTAL_BYTES)?;
        validate_final_node_bytes(&json)?;
        let blob_oid = repo.blob(&json)?;
        builder.insert(&legacy_filename, blob_oid, FileMode::Blob.into())?;
    }

    // --- tombstones/<prefix>/<id>.json ---
    let existing_tombstones = child_tree(repo, existing_root_tree.as_ref(), "tombstones");
    let mut tombstone_prefixes: HashMap<String, TreeBuilder> = HashMap::new();
    for tombstone in tombstones {
        validate_tombstone(tombstone)?;
        let id = tombstone.interaction_id.to_string();
        let file = format!("{id}.json");
        let prefix = shard_prefix(&id);
        let prior = child_tree(repo, existing_tombstones.as_ref(), prefix)
            .and_then(|x| x.get_name(&file).map(|e| e.id()));
        if let Some(oid) = prior {
            let old: Tombstone = serde_json::from_slice(repo.find_blob(oid)?.content())?;
            validate_tombstone(&old)?;
            if old != *tombstone {
                return Err(SyncError::Ref("immutable tombstone conflict".into()));
            }
            continue;
        }
        let builder = match tombstone_prefixes.entry(prefix.to_owned()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(child_treebuilder(
                repo,
                existing_tombstones.as_ref(),
                prefix,
            )?),
        };
        let bytes = serde_json::to_vec(tombstone)?;
        add_sync_bytes(&mut emitted_bytes, bytes.len(), MAX_SYNC_TOTAL_BYTES)?;
        let oid = repo.blob(&bytes)?;
        builder.insert(&file, oid, FileMode::Blob.into())?;
    }
    if !tombstone_prefixes.is_empty() {
        let mut b = child_treebuilder(repo, existing_root_tree.as_ref(), "tombstones")?;
        for (p, v) in tombstone_prefixes {
            b.insert(&p, v.write()?, FileMode::Tree.into())?;
        }
        root_builder.insert("tombstones", b.write()?, FileMode::Tree.into())?;
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

    // --- links/<interaction-id>/<commit-sha>.json ---
    // Only automatic links need this append-only side channel. Historical
    // custom link types remain available in their legacy node blobs.
    let mut link_builders: HashMap<String, TreeBuilder> = HashMap::new();
    for id in all_ids {
        let id_string = id.as_str();
        for link in db.get_legacy_artifact_links(id)? {
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
                let existing_blob = repo.find_blob(entry)?;
                validate_final_link_bytes(existing_blob.content())?;
                let existing: SyncLinkRecord = serde_json::from_slice(existing_blob.content())?;
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
                if !is_safe_commit_sha(&record.git_commit_hash)
                    || !is_automatic_link_type(&record.link_type)
                    || record
                        .interaction_id
                        .parse::<crate::models::InteractionId>()
                        .is_err()
                {
                    return Err(SyncError::Ref("unsafe link event serialization".into()));
                }
                let mut links = vec![ArtifactLink {
                    interaction_id: record
                        .interaction_id
                        .parse()
                        .map_err(|_| SyncError::Ref("invalid link event id".into()))?,
                    git_commit_hash: crate::models::CommitSha::new(record.git_commit_hash.clone()),
                    link_type: record.link_type.clone(),
                    linked_by: record.linked_by.clone(),
                }];
                crate::privacy::final_validate_links(&mut links)
                    .map_err(|e| SyncError::Ref(format!("unsafe link event serialization: {e}")))?;
                let safe = SyncLinkRecord {
                    interaction_id: record.interaction_id,
                    git_commit_hash: record.git_commit_hash,
                    link_type: record.link_type,
                    linked_by: links.remove(0).linked_by,
                };
                let bytes = serde_json::to_vec(&safe)?;
                add_sync_bytes(&mut emitted_bytes, bytes.len(), MAX_SYNC_TOTAL_BYTES)?;
                validate_final_link_bytes(&bytes)?;
                let blob_oid = repo.blob(&bytes)?;
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

    // --- events/<sha256[0..2]>/<sha256>.json ---
    // Names are derived from the canonical body ID, so path/body disagreement is
    // detectable without trusting mutable endpoint paths.
    let mut event_shards: HashMap<String, TreeBuilder> = HashMap::new();
    let mut baseline_event_bytes = 0;
    let baseline_event_ids: HashSet<String> = existing_root_tree
        .as_ref()
        .map(|root| {
            collect_derivation_events(repo, root, &mut baseline_event_bytes, MAX_SYNC_TOTAL_BYTES)
        })
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .map(|event| event.event_id)
        .collect();
    for event in retain_event_closure(
        db.projection_derivation_events(all_ids, remote_fingerprint)?,
        &tombstoned,
        &baseline_event_ids,
    ) {
        if !event.verify_id()
            || event.event_id.len() != 64
            || !event.event_id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(SyncError::Ref("invalid local derivation event".into()));
        }
        let shard = &event.event_id[..2];
        let file = format!("{}.json", event.event_id);
        let exists = child_tree(repo, existing_events_tree.as_ref(), shard)
            .and_then(|t| t.get_name(&file).map(|e| e.id()));
        let bytes = serde_json::to_vec(&event)?;
        if let Some(oid) = exists {
            if repo.find_blob(oid)?.content() != bytes.as_slice() {
                return Err(SyncError::Ref("immutable derivation event conflict".into()));
            }
            continue;
        }
        let b = match event_shards.entry(shard.to_string()) {
            Entry::Occupied(x) => x.into_mut(),
            Entry::Vacant(x) => x.insert(child_treebuilder(
                repo,
                existing_events_tree.as_ref(),
                shard,
            )?),
        };
        add_sync_bytes(&mut emitted_bytes, bytes.len(), MAX_SYNC_TOTAL_BYTES)?;
        b.insert(&file, repo.blob(&bytes)?, FileMode::Blob.into())?;
    }
    if !event_shards.is_empty() {
        let mut outer = child_treebuilder(repo, existing_root_tree.as_ref(), "events")?;
        for (s, b) in event_shards {
            outer.insert(&s, b.write()?, FileMode::Tree.into())?;
        }
        root_builder.insert("events", outer.write()?, FileMode::Tree.into())?;
    }

    // --- ranges/<sha256[0..2]>/<sha256>.json ---
    let mut range_shards: HashMap<String, TreeBuilder> = HashMap::new();
    for range in db.projection_ranges(remote_fingerprint)? {
        if !range.verify_id() {
            return Err(SyncError::Ref("invalid local range".into()));
        }
        let shard = &range.range_id[..2];
        let file = format!("{}.json", range.range_id);
        let bytes = serde_json::to_vec(&range)?;
        if let Some(oid) = child_tree(repo, existing_ranges_tree.as_ref(), shard)
            .and_then(|t| t.get_name(&file).map(|e| e.id()))
        {
            if repo.find_blob(oid)?.content() != bytes {
                return Err(SyncError::Ref("immutable range conflict".into()));
            }
            continue;
        }
        let b = match range_shards.entry(shard.into()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(child_treebuilder(
                repo,
                existing_ranges_tree.as_ref(),
                shard,
            )?),
        };
        add_sync_bytes(&mut emitted_bytes, bytes.len(), MAX_SYNC_TOTAL_BYTES)?;
        b.insert(&file, repo.blob(&bytes)?, FileMode::Blob.into())?;
    }
    if !range_shards.is_empty() {
        let mut outer = child_treebuilder(repo, existing_root_tree.as_ref(), "ranges")?;
        for (s, b) in range_shards {
            outer.insert(&s, b.write()?, FileMode::Tree.into())?;
        }
        root_builder.insert("ranges", outer.write()?, FileMode::Tree.into())?;
    }
    for namespace in ["events", "ranges"] {
        if root_builder.get(namespace)?.is_none() {
            root_builder.insert(
                namespace,
                repo.treebuilder(None)?.write()?,
                FileMode::Tree.into(),
            )?;
        }
    }

    // The index is a derived view, never an append-only authority. Rebuild it
    // from the one final pruned/projected tree so stale baseline pointers cannot
    // survive privacy pruning or point at unprojectable assertions.
    let projected_without_index = repo.find_tree(root_builder.write()?)?;
    validate_by_commit_structure(repo, &projected_without_index)?;
    validate_tree_derivation_graph(repo, &projected_without_index)?;
    let expected = projected_endpoints(repo, &projected_without_index)?;
    if expected.is_empty() {
        let _ = remove_if_present(&mut root_builder, "by-commit")?;
    } else {
        root_builder.insert(
            "by-commit",
            build_by_commit_tree(repo, &expected)?,
            FileMode::Tree.into(),
        )?;
    }

    // --- FORMAT marker: upgrade known numeric predecessors, never downgrade ---
    let existing_format = if let Some(entry) = existing_root_tree
        .as_ref()
        .and_then(|tree| tree.get_name("FORMAT"))
    {
        let blob = repo.find_blob(entry.id())?;
        let text = std::str::from_utf8(blob.content())
            .map_err(|_| SyncError::Ref("FORMAT is not UTF-8".into()))?;
        Some(
            text.trim()
                .parse::<u64>()
                .map_err(|_| SyncError::Ref("FORMAT is not numeric".into()))?,
        )
    } else {
        None
    };
    if existing_format.is_some_and(|version| version > 5) {
        return Err(SyncError::Ref(
            "remote FORMAT is newer than supported".into(),
        ));
    }
    if existing_format.is_none_or(|version| version < 5) {
        let format_oid = repo.blob(SYNC_FORMAT_VERSION)?;
        root_builder.insert("FORMAT", format_oid, FileMode::Blob.into())?;
    }

    // Write Tree
    let new_tree_oid = root_builder.write()?;

    // Optimization: Skip commit if tree hasn't changed
    if let Some(parent) = &parent_commit {
        if parent.tree_id() == new_tree_oid {
            return Ok(parent.id());
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

    let commit_oid = repo.commit(
        Some(ref_name),
        &sig,
        &sig,
        "Sync interactions",
        &new_tree,
        &parents,
    )?;

    Ok(commit_oid)
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
        let name = entry
            .name()
            .ok_or_else(|| SyncError::Ref("non-UTF-8 sync path".into()))?;
        match entry.kind() {
            Some(ObjectType::Blob) => {
                if let Some(id_str) = name.strip_suffix(".json") {
                    out.push((id_str.to_string(), entry.id()));
                }
            }
            Some(ObjectType::Tree) if max_depth > 0 => {
                if matches!(
                    name,
                    "by-commit" | "links" | "tombstones" | "events" | "ranges"
                ) {
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

const MAX_BY_COMMIT_POINTERS: usize = 100_000;

fn validate_by_commit_structure(
    repo: &Repository,
    root: &Tree<'_>,
) -> Result<HashSet<(String, String)>> {
    let Some(entry) = root.get_name("by-commit") else {
        return Ok(HashSet::new());
    };
    if entry.kind() != Some(ObjectType::Tree) {
        return Err(SyncError::Ref("by-commit must be a tree".into()));
    }
    let mut out = HashSet::new();
    for commit_entry in repo.find_tree(entry.id())?.iter() {
        let commit = commit_entry.name().unwrap_or_default();
        if commit_entry.kind() != Some(ObjectType::Tree)
            || !is_safe_commit_sha(commit)
            || commit.bytes().any(|byte| byte.is_ascii_uppercase())
            || git2::Oid::from_str(commit).is_err()
        {
            return Err(SyncError::Ref("invalid by-commit commit entry".into()));
        }
        for pointer in repo.find_tree(commit_entry.id())?.iter() {
            let id = pointer.name().unwrap_or_default();
            let canonical = id
                .parse::<crate::models::InteractionId>()
                .map(|parsed| parsed.to_string())
                .map_err(|_| SyncError::Ref("invalid by-commit interaction name".into()))?;
            if canonical != id || pointer.kind() != Some(ObjectType::Blob) {
                return Err(SyncError::Ref("invalid by-commit pointer".into()));
            }
            if !repo.find_blob(pointer.id())?.content().is_empty() {
                return Err(SyncError::Ref(
                    "by-commit pointer body must be empty".into(),
                ));
            }
            if !out.insert((commit.to_owned(), id.to_owned())) {
                return Err(SyncError::Ref("duplicate by-commit pointer".into()));
            }
            if out.len() > MAX_BY_COMMIT_POINTERS {
                return Err(SyncError::Ref("too many by-commit pointers".into()));
            }
        }
    }
    Ok(out)
}

fn projected_endpoints(repo: &Repository, root: &Tree<'_>) -> Result<HashSet<(String, String)>> {
    let mut refs = Vec::new();
    collect_interaction_blobs(repo, root, 2, &mut refs)?;
    let mut nodes = HashSet::new();
    let mut endpoints = HashSet::new();
    for (path_id, oid) in refs {
        let node: SyncNode = serde_json::from_slice(repo.find_blob(oid)?.content())?;
        let id = node.interaction.id.to_string();
        if id != path_id {
            return Err(SyncError::Ref("projected node identity mismatch".into()));
        }
        nodes.insert(id.clone());
        for link in node.artifact_links {
            if link.interaction_id.to_string() != id
                || !is_safe_commit_sha(link.git_commit_hash.as_str())
            {
                return Err(SyncError::Ref("invalid projected legacy link".into()));
            }
            endpoints.insert((link.git_commit_hash.as_str().to_owned(), id.clone()));
        }
    }
    let mut decoded = 0;
    for record in collect_link_records(repo, root, &mut decoded, MAX_SYNC_TOTAL_BYTES)? {
        if nodes.contains(&record.interaction_id) {
            endpoints.insert((record.git_commit_hash, record.interaction_id));
        }
    }
    for event in collect_derivation_events(repo, root, &mut decoded, MAX_SYNC_TOTAL_BYTES)? {
        let id = event.interaction_id.to_string();
        if !nodes.contains(&id) {
            return Err(SyncError::Ref("event target has no projected node".into()));
        }
        endpoints.insert((event.target_commit.as_str().to_owned(), id));
    }
    if endpoints.len() > MAX_BY_COMMIT_POINTERS {
        return Err(SyncError::Ref("too many projected endpoints".into()));
    }
    Ok(endpoints)
}

fn parse_legacy_source(source: &str) -> Option<(String, String)> {
    let rest = source.strip_prefix("legacy:")?;
    let (interaction, commit) = rest.split_once(':')?;
    let canonical = interaction
        .parse::<crate::models::InteractionId>()
        .ok()?
        .to_string();
    if canonical != interaction
        || !is_safe_commit_sha(commit)
        || commit.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return None;
    }
    Some((interaction.to_owned(), commit.to_owned()))
}

fn is_event_source(source: &str) -> bool {
    source.len() == 64
        && source
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn validate_closed_derivation_graph(
    events: &[DerivationEvent],
    legacy: &HashSet<(String, String)>,
) -> Result<()> {
    let by_id: HashMap<_, _> = events
        .iter()
        .map(|event| (event.event_id.as_str(), event))
        .collect();
    if by_id.len() != events.len() {
        return Err(SyncError::Ref("duplicate derivation event id".into()));
    }
    for event in events {
        if matches!(
            event.relation,
            crate::models::DerivationRelation::RewriteExact
                | crate::models::DerivationRelation::SquashExact
        ) && event.source_event_ids.is_empty()
        {
            return Err(SyncError::Ref("exact derivation has no source".into()));
        }
        for source in &event.source_event_ids {
            if let Some((interaction, commit)) = parse_legacy_source(source) {
                if interaction != event.interaction_id.to_string()
                    || !legacy.contains(&(interaction, commit))
                {
                    return Err(SyncError::Ref(
                        "legacy derivation source does not resolve".into(),
                    ));
                }
            } else if is_event_source(source) {
                let parent = by_id
                    .get(source.as_str())
                    .ok_or_else(|| SyncError::Ref("dangling derivation event source".into()))?;
                if parent.interaction_id != event.interaction_id {
                    return Err(SyncError::Ref("cross-interaction derivation source".into()));
                }
            } else {
                return Err(SyncError::Ref("invalid derivation source grammar".into()));
            }
        }
    }
    fn visit<'a>(
        id: &'a str,
        by_id: &HashMap<&'a str, &'a DerivationEvent>,
        visiting: &mut HashSet<&'a str>,
        visited: &mut HashSet<&'a str>,
    ) -> Result<()> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(SyncError::Ref("cyclic derivation graph".into()));
        }
        let event = by_id
            .get(id)
            .ok_or_else(|| SyncError::Ref("missing derivation event".into()))?;
        for source in &event.source_event_ids {
            if is_event_source(source) {
                visit(source, by_id, visiting, visited)?;
            }
        }
        visiting.remove(id);
        visited.insert(id);
        Ok(())
    }
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for id in by_id.keys() {
        visit(id, &by_id, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn projected_legacy_sources(
    repo: &Repository,
    root: &Tree<'_>,
) -> Result<HashSet<(String, String)>> {
    let mut refs = Vec::new();
    collect_interaction_blobs(repo, root, 2, &mut refs)?;
    let mut result = HashSet::new();
    for (path_id, oid) in refs {
        let node: SyncNode = serde_json::from_slice(repo.find_blob(oid)?.content())?;
        for link in node.artifact_links {
            if link.interaction_id.to_string() == path_id {
                result.insert((path_id.clone(), link.git_commit_hash.as_str().to_owned()));
            }
        }
    }
    let mut decoded = 0;
    for link in collect_link_records(repo, root, &mut decoded, MAX_SYNC_TOTAL_BYTES)? {
        result.insert((link.interaction_id, link.git_commit_hash));
    }
    Ok(result)
}

fn validate_tree_derivation_graph(repo: &Repository, root: &Tree<'_>) -> Result<()> {
    let mut decoded = 0;
    let events = collect_derivation_events(repo, root, &mut decoded, MAX_SYNC_TOTAL_BYTES)?;
    let legacy = projected_legacy_sources(repo, root)?;
    validate_closed_derivation_graph(&events, &legacy)
}

fn build_by_commit_tree(
    repo: &Repository,
    endpoints: &HashSet<(String, String)>,
) -> Result<git2::Oid> {
    let mut grouped: HashMap<&str, Vec<&str>> = HashMap::new();
    for (commit, id) in endpoints {
        grouped.entry(commit).or_default().push(id);
    }
    let empty = repo.blob(b"")?;
    let mut root = repo.treebuilder(None)?;
    for (commit, mut ids) in grouped {
        ids.sort_unstable();
        let mut subtree = repo.treebuilder(None)?;
        for id in ids {
            subtree.insert(id, empty, FileMode::Blob.into())?;
        }
        root.insert(commit, subtree.write()?, FileMode::Tree.into())?;
    }
    root.write().map_err(Into::into)
}

pub fn pull_from_ref(repo: &Repository, db: &CvcStore, ref_name: &str) -> Result<()> {
    pull_from_ref_with_limits(repo, db, ref_name, None, SyncReadLimits::default())
}
/// Import a remote baseline and record proof that every node seen there was published
/// to this exact destination. Remote payloads never participate in local consent.
pub fn pull_from_ref_for_remote(
    repo: &Repository,
    db: &CvcStore,
    ref_name: &str,
    remote_fingerprint: Option<&str>,
) -> Result<()> {
    pull_from_ref_with_limits(
        repo,
        db,
        ref_name,
        remote_fingerprint,
        SyncReadLimits::default(),
    )
}

/// Pull with caller-supplied decoding bounds.  This is primarily useful to
/// embed CVC in resource-constrained processes and for deterministic tests.
pub fn pull_from_ref_with_limits(
    repo: &Repository,
    db: &CvcStore,
    ref_name: &str,
    remote_fingerprint: Option<&str>,
    limits: SyncReadLimits,
) -> Result<()> {
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
    let mut format_version = 1u64;
    if let Some(entry) = tree.get_name("FORMAT") {
        let blob = repo.find_blob(entry.id())?;
        let text = std::str::from_utf8(blob.content())
            .map_err(|_| SyncError::Ref("FORMAT is not UTF-8".into()))?;
        let version = text
            .trim()
            .parse::<u64>()
            .map_err(|_| SyncError::Ref("FORMAT is not numeric".into()))?;
        if !(1..=5).contains(&version) {
            return Err(SyncError::Ref("unsupported FORMAT".into()));
        }
        format_version = version;
    }
    for name in ["events", "ranges"] {
        match tree.get_name(name) {
            Some(e) => {
                if format_version < 5 {
                    return Err(SyncError::Ref("v5 namespace in legacy FORMAT".into()));
                }
                if e.kind() != Some(ObjectType::Tree) {
                    return Err(SyncError::Ref("v5 namespace must be tree".into()));
                }
            }
            None if format_version == 5 => {
                return Err(SyncError::Ref("FORMAT5 namespace missing".into()));
            }
            None => {}
        }
    }
    validate_v5_reserved_namespaces(repo, tree)?;
    let wire_index = validate_by_commit_structure(repo, tree)?;

    // Tombstones are read and structurally validated before any node or link.
    // This makes ordering in the tree irrelevant and prevents resurrection by a
    // stale clone that still carries an immutable node blob.
    let tombstones = collect_tombstones(repo, tree, limits)?;
    let tombstoned: HashSet<String> = tombstones
        .iter()
        .map(|t| t.interaction_id.to_string())
        .collect();

    // 2. Collect (interaction-id, blob-oid) pairs from BOTH layouts this ref might
    // contain: legacy flat `<id>.json` files at the root, and/or the v2 sharded
    // `nodes/<prefix>/<id>.json` layout. `by-commit/` is skipped entirely -- it's a
    // pure index for the Reviewer's PR-scoped fetch path. `links/` is read
    // separately because it contains post-node append-only events.
    let mut blob_refs: Vec<(String, git2::Oid)> = Vec::new();
    collect_interaction_blobs(repo, tree, 2, &mut blob_refs)?;
    if blob_refs.len() > MAX_SYNC_NODES {
        return Err(SyncError::Ref("too many remote nodes".into()));
    }

    // We want to verify existing IDs in DB to skip reading blobs
    let existing_ids_vec = db.get_all_interaction_ids()?;
    let existing_ids: HashSet<String> = existing_ids_vec
        .into_iter()
        .map(|id| id.as_str().to_string())
        .collect();

    let mut nodes_to_insert = Vec::new();
    let mut decoded_bytes = 0usize;
    let mut context_count = 0usize;
    let mut tool_count = 0usize;
    let mut embedded_link_count = 0usize;
    // UUIDs are identities, not permission to overwrite content. A legacy and
    // sharded representation may coexist only when they decode identically.
    let mut wire_nodes: HashMap<String, serde_json::Value> = HashMap::new();

    let published_ids: Vec<crate::models::InteractionId> = blob_refs
        .iter()
        .filter(|(id, _)| !tombstoned.contains(id))
        .map(|(id, _)| {
            id.parse()
                .map_err(|_| SyncError::Ref("invalid interaction id".into()))
        })
        .collect::<Result<_>>()?;
    for (id_str, blob_oid) in blob_refs {
        if tombstoned.contains(&id_str) {
            continue;
        }
        let blob = repo.find_blob(blob_oid)?;
        if blob.size() > MAX_SYNC_BLOB_BYTES {
            return Err(SyncError::Ref("remote node exceeds size limit".into()));
        }
        add_sync_bytes(&mut decoded_bytes, blob.size(), limits.max_total_bytes)?;
        let content = std::str::from_utf8(blob.content())
            .map_err(|e| SyncError::Serde(serde_json::Error::custom(e.to_string())))?;

        let node: SyncNode = serde_json::from_str(content)?;
        if node.context_items.len() > 256
            || node.tool_executions.len() > 256
            || node.artifact_links.len() > MAX_SYNC_LINKS_PER_NODE
        {
            return Err(SyncError::Ref(
                "remote node collection exceeds limit".into(),
            ));
        }
        context_count = context_count.saturating_add(node.context_items.len());
        tool_count = tool_count.saturating_add(node.tool_executions.len());
        embedded_link_count = embedded_link_count.saturating_add(node.artifact_links.len());
        if context_count > MAX_SYNC_CONTEXT_ITEMS
            || tool_count > MAX_SYNC_TOOL_EXECUTIONS
            || embedded_link_count > MAX_SYNC_EMBEDDED_LINKS
        {
            return Err(SyncError::Ref(
                "remote collection total exceeds limit".into(),
            ));
        }
        if node.interaction.id.to_string() != id_str {
            return Err(SyncError::Ref(
                "interaction blob filename/id mismatch".into(),
            ));
        }
        validate_remote_node(&node)?;
        let semantic = serde_json::to_value(&node)?;
        if let Some(previous) = wire_nodes.get(&id_str) {
            if previous != &semantic {
                return Err(SyncError::Ref(
                    "conflicting duplicate interaction representations".into(),
                ));
            }
            continue;
        }
        wire_nodes.insert(id_str.clone(), semantic);
        if !existing_ids.contains(&id_str) {
            nodes_to_insert.push(node);
        }
    }

    let records: Vec<SyncLinkRecord> =
        collect_link_records(repo, tree, &mut decoded_bytes, limits.max_total_bytes)?
            .into_iter()
            .filter(|r| !tombstoned.contains(&r.interaction_id))
            .collect();
    // Validate the complete wire graph before applying any suppression. A
    // tombstone must never turn malformed evidence into an acceptable snapshot.
    let incoming_events =
        collect_derivation_events(repo, tree, &mut decoded_bytes, limits.max_total_bytes)?;
    let incoming_legacy_sources = projected_legacy_sources(repo, tree)?;
    validate_closed_derivation_graph(&incoming_events, &incoming_legacy_sources)?;
    let events = retain_event_closure(incoming_events, &tombstoned, &HashSet::new());
    let ranges = collect_ranges(repo, tree, &mut decoded_bytes, limits.max_total_bytes)?;
    let range_ids: HashSet<_> = ranges.iter().map(|range| range.range_id.as_str()).collect();
    if events.iter().any(|event| {
        event.relation == crate::models::DerivationRelation::SquashExact
            && event
                .range_id
                .as_deref()
                .is_none_or(|id| !range_ids.contains(id))
    }) {
        return Err(SyncError::Ref(
            "squash event references missing range".into(),
        ));
    }
    let mut wire_legacy_sources = HashSet::new();
    for (id, value) in &wire_nodes {
        let node: SyncNode = serde_json::from_value(value.clone())?;
        for link in node.artifact_links {
            wire_legacy_sources.insert((id.clone(), link.git_commit_hash.as_str().to_owned()));
        }
    }
    wire_legacy_sources.extend(records.iter().map(|record| {
        (
            record.interaction_id.clone(),
            record.git_commit_hash.clone(),
        )
    }));
    validate_closed_derivation_graph(&events, &wire_legacy_sources)?;
    let mut expected_index = HashSet::new();
    for (id, value) in &wire_nodes {
        let node: SyncNode = serde_json::from_value(value.clone())?;
        for link in node.artifact_links {
            expected_index.insert((link.git_commit_hash.as_str().to_owned(), id.clone()));
        }
    }
    for record in &records {
        expected_index.insert((
            record.git_commit_hash.clone(),
            record.interaction_id.clone(),
        ));
    }
    for event in &events {
        expected_index.insert((
            event.target_commit.as_str().to_owned(),
            event.interaction_id.to_string(),
        ));
    }
    let effective_wire_index: HashSet<_> = wire_index
        .into_iter()
        .filter(|(_, id)| !tombstoned.contains(id))
        .collect();
    if format_version >= 2 && effective_wire_index != expected_index {
        return Err(SyncError::Ref(
            "by-commit index is inconsistent with projected evidence".into(),
        ));
    }
    let mut known_interactions = existing_ids;
    known_interactions.extend(
        nodes_to_insert
            .iter()
            .map(|node| node.interaction.id.to_string()),
    );
    validate_records_against_store(db, &records, &known_interactions, &nodes_to_insert)?;
    for event in &events {
        if !known_interactions.contains(&event.interaction_id.to_string()) {
            return Err(SyncError::Ref(
                "derivation references missing interaction".into(),
            ));
        }
    }

    // 5. Robust Topological Sort (DFS)
    // We need to ensure that if B depends on A (B.parent_id = A.id), A is inserted first.
    // Duplicate wire IDs were checked for semantic identity before this point.

    let mut node_map: HashMap<String, SyncNode> = HashMap::new();
    for mut node in nodes_to_insert {
        // The wire node is immutable.  Locally detach a child if its parent was
        // suppressed by this source's tombstone, so a fresh clone can retain it.
        if node
            .interaction
            .parent_id
            .as_ref()
            .is_some_and(|parent| tombstoned.contains(&parent.to_string()))
        {
            node.interaction.parent_id = None;
        }
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

    // Scrub every textual field while the batch is still only memory.  IDs,
    // OIDs and enum-like fields were validated above and are never scrubbed.
    let mut captures = Vec::with_capacity(sorted_nodes.len());
    let mut links = Vec::new();
    for mut node in sorted_nodes {
        // Remote history can predate capture sanitization. Scrub it before its
        // first local SQLite/WAL write too.
        crate::privacy::final_scrub_for_sync(
            &mut node.interaction,
            &mut node.context_items,
            &mut node.tool_executions,
        )
        .map_err(|e| SyncError::Ref(format!("unsafe node import: {e}")))?;
        for link in &mut node.artifact_links {
            link.linked_by = link
                .linked_by
                .take()
                .map(|value| crate::privacy::scrub(&value))
                .transpose()
                .map_err(|e| SyncError::Ref(format!("unsafe embedded linked_by: {e}")))?;
            links.push((
                link.interaction_id.clone(),
                link.git_commit_hash.clone(),
                link.link_type.clone(),
                link.linked_by.clone(),
            ));
        }
        let conv_id = &node.interaction.conversation_id;
        captures.push(crate::privacy::sync_import_capture(
            crate::models::Conversation {
                id: conv_id.clone(),
                title: "Synced Conversation".into(),
                created_at: node.interaction.timestamp,
            },
            node.interaction,
            node.context_items,
            node.tool_executions,
        ));
    }

    for mut record in records {
        let interaction_id: crate::models::InteractionId = record
            .interaction_id
            .parse()
            .map_err(|_| SyncError::Ref("invalid link interaction id".into()))?;
        record.linked_by = record
            .linked_by
            .take()
            .map(|value| crate::privacy::scrub(&value))
            .transpose()
            .map_err(|e| SyncError::Ref(format!("unsafe event linked_by: {e}")))?;
        let commit_sha = crate::models::CommitSha::new(record.git_commit_hash);
        links.push((
            interaction_id,
            commit_sha,
            record.link_type,
            record.linked_by,
        ));
    }
    db.import_sync_batch(
        captures,
        links,
        events,
        ranges,
        remote_fingerprint,
        &tombstones,
        &published_ids,
    )?;

    Ok(())
}

fn validate_v5_reserved_namespaces(repo: &Repository, root: &Tree) -> Result<()> {
    let format = root
        .get_name("FORMAT")
        .map(|e| repo.find_blob(e.id()))
        .transpose()?;
    let is_v5 = format
        .as_ref()
        .is_some_and(|b| b.content().trim_ascii() == b"5");
    if is_v5
        && ["events", "ranges"]
            .iter()
            .any(|name| root.get_name(name).is_none())
    {
        return Err(SyncError::Ref("FORMAT5 namespace missing".into()));
    }
    if let Some(ranges) = root.get_name("ranges") {
        if !is_v5 || ranges.kind() != Some(ObjectType::Tree) {
            return Err(SyncError::Ref("invalid reserved ranges namespace".into()));
        }
        let mut bytes = 0;
        let _ = collect_ranges(repo, root, &mut bytes, MAX_SYNC_TOTAL_BYTES)?;
    }
    if let Some(events) = root.get_name("events") {
        if !is_v5 || events.kind() != Some(ObjectType::Tree) {
            return Err(SyncError::Ref("invalid reserved events namespace".into()));
        }
    }
    // Parse the complete event namespace during baseline validation too. Push
    // must never preserve an invalid v5 event merely because it was received
    // before this client was upgraded.
    if root.get_name("events").is_some() {
        let mut bytes = 0;
        let _ = collect_derivation_events(repo, root, &mut bytes, MAX_SYNC_TOTAL_BYTES)?;
    }
    Ok(())
}

fn collect_ranges(
    repo: &Repository,
    root: &Tree,
    decoded: &mut usize,
    max: usize,
) -> Result<Vec<RangeEvidence>> {
    let Some(tree) = child_tree(repo, Some(root), "ranges") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut seen = HashMap::new();
    for shard in tree.iter() {
        let name = shard.name().unwrap_or_default();
        if name.len() != 2
            || !name
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || shard.kind() != Some(ObjectType::Tree)
        {
            return Err(SyncError::Ref("invalid range shard".into()));
        }
        for entry in repo.find_tree(shard.id())?.iter() {
            if out.len() >= MAX_SYNC_RANGES {
                return Err(SyncError::Ref("too many ranges".into()));
            }
            let file = entry.name().unwrap_or_default();
            let id = file
                .strip_suffix(".json")
                .filter(|x| {
                    x.len() == 64
                        && x.bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                })
                .ok_or_else(|| SyncError::Ref("invalid range filename".into()))?;
            if &id[..2] != name || entry.kind() != Some(ObjectType::Blob) {
                return Err(SyncError::Ref("range path mismatch".into()));
            }
            let blob = repo.find_blob(entry.id())?;
            if blob.size() > MAX_SYNC_BLOB_BYTES {
                return Err(SyncError::Ref("range too large".into()));
            }
            add_sync_bytes(decoded, blob.size(), max)?;
            let range: RangeEvidence = serde_json::from_slice(blob.content())?;
            if range.range_id != id || !range.verify_id() {
                return Err(SyncError::Ref("range body/hash mismatch".into()));
            }
            if let Some(old) = seen.insert(range.range_id.clone(), blob.content().to_vec()) {
                if old != blob.content() {
                    return Err(SyncError::Ref("conflicting range".into()));
                }
            } else {
                out.push(range)
            }
        }
    }
    Ok(out)
}

fn collect_derivation_events(
    repo: &Repository,
    root: &Tree,
    decoded: &mut usize,
    max: usize,
) -> Result<Vec<DerivationEvent>> {
    let Some(tree) = child_tree(repo, Some(root), "events") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut ids = HashMap::new();
    for shard in tree.iter() {
        let name = shard.name().unwrap_or_default();
        if name.len() != 2
            || !name.bytes().all(|b| b.is_ascii_hexdigit())
            || shard.kind() != Some(ObjectType::Tree)
        {
            return Err(SyncError::Ref("invalid event shard".into()));
        }
        let sub = repo.find_tree(shard.id())?;
        for entry in sub.iter() {
            if out.len() >= MAX_SYNC_DERIVATION_EVENTS {
                return Err(SyncError::Ref("too many derivation events".into()));
            }
            let file = entry.name().unwrap_or_default();
            let id = file
                .strip_suffix(".json")
                .filter(|x| x.len() == 64 && x.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(|| SyncError::Ref("invalid event filename".into()))?;
            if &id[..2] != name || entry.kind() != Some(ObjectType::Blob) {
                return Err(SyncError::Ref("event path mismatch".into()));
            }
            let blob = repo.find_blob(entry.id())?;
            if blob.size() > MAX_SYNC_BLOB_BYTES {
                return Err(SyncError::Ref("event too large".into()));
            }
            add_sync_bytes(decoded, blob.size(), max)?;
            let event: DerivationEvent = serde_json::from_slice(blob.content())?;
            if event.event_id != id || !validate_derivation_event(&event) {
                return Err(SyncError::Ref("event body/hash mismatch".into()));
            }
            if let Some(old) = ids.insert(event.event_id.clone(), blob.content().to_vec()) {
                if old != blob.content() {
                    return Err(SyncError::Ref("conflicting duplicate event".into()));
                }
            } else {
                out.push(event);
            }
        }
    }
    Ok(out)
}

fn validate_derivation_event(event: &DerivationEvent) -> bool {
    use crate::models::{DerivationOrigin as O, DerivationRelation as R, EvidenceKind as E};
    if !event.verify_id()
        || !event.sources_are_canonical()
        || event.event_id.len() != 64
        || !event.event_id.bytes().all(|b| b.is_ascii_hexdigit())
        || !is_safe_commit_sha(event.target_commit.as_str())
        || event.evidence.version != 1
        || event
            .linked_by
            .as_deref()
            .is_some_and(|x| x.is_empty() || x.len() > 1024 * 1024)
    {
        return false;
    }
    if event.source_event_ids.iter().any(|source| {
        source.len() > 512 || (parse_legacy_source(source).is_none() && !is_event_source(source))
    }) {
        return false;
    }
    match event.relation {
        R::RewriteExact => {
            !event.source_event_ids.is_empty()
                && event
                    .old_oid
                    .as_ref()
                    .is_some_and(|x| is_safe_commit_sha(x.as_str()))
                && event
                    .new_oid
                    .as_ref()
                    .is_some_and(|x| x.as_str() == event.target_commit.as_str())
                && event.range_id.is_none()
                && event.evidence.kind == E::LocallyObserved
                && event.origin == O::LocalHook
        }
        R::SquashExact => {
            !event.source_event_ids.is_empty()
                && event.old_oid.is_none()
                && event.new_oid.is_none()
                && event.range_id.as_ref().is_some_and(|x| {
                    x.len() == 64
                        && x.bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                })
                && event.evidence.kind == E::LocallyObserved
                && event.origin == O::LocalHook
        }
        R::Generated | R::Temporal | R::Verified => {
            event.old_oid.is_none() && event.new_oid.is_none() && event.range_id.is_none()
        }
    }
}

fn collect_tombstones(
    repo: &Repository,
    root: &Tree,
    limits: SyncReadLimits,
) -> Result<Vec<Tombstone>> {
    let Some(root_entry) = root.get_name("tombstones") else {
        return Ok(Vec::new());
    };
    if root_entry.kind() != Some(ObjectType::Tree) {
        return Err(SyncError::Ref("tombstones must be a tree".into()));
    }
    let tree = repo.find_tree(root_entry.id())?;
    let mut out = Vec::new();
    let mut total_bytes = 0usize;
    for shard in tree.iter() {
        let shard_name = shard.name().unwrap_or_default();
        if shard_name.len() != 2
            || !shard_name.bytes().all(|b| b.is_ascii_hexdigit())
            || shard.kind() != Some(ObjectType::Tree)
        {
            return Err(SyncError::Ref("invalid tombstone shard".into()));
        }
        let sub = repo.find_tree(shard.id())?;
        for entry in sub.iter() {
            if out.len() >= limits.max_tombstones {
                return Err(SyncError::Ref("too many tombstones".into()));
            }
            let name = entry.name().unwrap_or_default();
            if entry.kind() != Some(ObjectType::Blob) {
                return Err(SyncError::Ref("invalid tombstone entry type".into()));
            }
            let id = name
                .strip_suffix(".json")
                .ok_or_else(|| SyncError::Ref("invalid tombstone filename".into()))?;
            if shard_prefix(id) != shard_name {
                return Err(SyncError::Ref("tombstone shard mismatch".into()));
            }
            let blob = repo.find_blob(entry.id())?;
            if blob.size() > limits.max_tombstone_bytes {
                return Err(SyncError::Ref("tombstone exceeds size limit".into()));
            }
            add_sync_bytes(&mut total_bytes, blob.size(), limits.max_total_bytes)?;
            let t: Tombstone = serde_json::from_slice(blob.content())?;
            validate_tombstone(&t)?;
            if t.interaction_id.to_string() != id {
                return Err(SyncError::Ref("tombstone filename/id mismatch".into()));
            }
            out.push(t);
        }
    }
    validate_tombstone_records(&out)
}

/// Canonicalize received tombstones by interaction ID.  Git's canonical shard
/// layout normally makes duplicate IDs unrepresentable, but this seam is also
/// used by alternate/importing transports where repeated records are possible.
/// Equal repeats are harmless; conflicting immutable records are rejected.
pub fn validate_tombstone_records(records: &[Tombstone]) -> Result<Vec<Tombstone>> {
    let mut seen = HashMap::new();
    let mut unique = Vec::with_capacity(records.len());
    for tombstone in records {
        validate_tombstone(tombstone)?;
        match seen.entry(tombstone.interaction_id.to_string()) {
            Entry::Occupied(entry) if entry.get() != tombstone => {
                return Err(SyncError::Ref("conflicting tombstones".into()));
            }
            Entry::Occupied(_) => {}
            Entry::Vacant(entry) => {
                entry.insert(tombstone.clone());
                unique.push(tombstone.clone());
            }
        }
    }
    Ok(unique)
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
            || !matches!(
                link.link_type.as_str(),
                "generated" | "temporal" | "verified"
            )
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

/// Validate remote structural values before any privacy transformation.  These
/// fields define relationships or closed protocol states; redacting one would
/// turn a hostile record into a different immutable record.
fn validate_remote_node(node: &SyncNode) -> Result<()> {
    if node.interaction.conversation_id.is_empty()
        || node.interaction.conversation_id.len() > 256
        || node
            .interaction
            .source_request_id
            .as_deref()
            .is_some_and(|x| x.len() > 256)
    {
        return Err(SyncError::Ref("invalid remote identity".into()));
    }
    for item in &node.context_items {
        if item.interaction_id != node.interaction.id
            || item
                .git_blob_sha
                .as_deref()
                .is_some_and(|v| !is_safe_commit_sha(v))
            || item.start_line.is_some_and(|v| v < 0)
            || item.end_line.is_some_and(|v| v < 0)
        {
            return Err(SyncError::Ref("invalid remote context item".into()));
        }
    }
    for tool in &node.tool_executions {
        if tool.interaction_id != node.interaction.id {
            return Err(SyncError::Ref("invalid remote tool item".into()));
        }
    }
    validate_legacy_links(node)
}

fn validate_records_against_store(
    db: &CvcStore,
    records: &[SyncLinkRecord],
    known_interactions: &HashSet<String>,
    incoming_nodes: &[SyncNode],
) -> Result<()> {
    let mut seen: HashMap<(String, String), (&str, Option<&str>)> = HashMap::new();
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
        let key = (
            record.interaction_id.clone(),
            record.git_commit_hash.clone(),
        );
        if let Some((kind, linked_by)) = seen.get(&key) {
            if *kind != record.link_type
                || matches!((linked_by, record.linked_by.as_deref()), (Some(left), Some(right)) if *left != right)
            {
                return Err(SyncError::Ref(
                    "conflicting duplicate remote link event".into(),
                ));
            }
        } else {
            seen.insert(key, (&record.link_type, record.linked_by.as_deref()));
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

fn collect_link_records(
    repo: &Repository,
    root: &Tree,
    decoded_bytes: &mut usize,
    max_total_bytes: usize,
) -> Result<Vec<SyncLinkRecord>> {
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
            if records.len() == MAX_SYNC_LINK_EVENTS {
                return Err(SyncError::Ref("too many remote link events".into()));
            }
            let filename = record_entry.name().unwrap_or_default();
            let commit = filename
                .strip_suffix(".json")
                .filter(|value| is_safe_commit_sha(value))
                .ok_or_else(|| SyncError::Ref("invalid links path commit SHA".into()))?;
            let blob = repo.find_blob(record_entry.id())?;
            if blob.size() > MAX_SYNC_BLOB_BYTES {
                return Err(SyncError::Ref(
                    "remote link event exceeds size limit".into(),
                ));
            }
            add_sync_bytes(decoded_bytes, blob.size(), max_total_bytes)?;
            let record: SyncLinkRecord = serde_json::from_slice(blob.content())?;
            if record.interaction_id != interaction_id
                || record.git_commit_hash != commit
                || !is_automatic_link_type(&record.link_type)
            {
                return Err(SyncError::Ref("inconsistent link record".into()));
            }
            if record
                .linked_by
                .as_deref()
                .is_some_and(|value| value.is_empty() || value.len() > 1024 * 1024)
            {
                return Err(SyncError::Ref("invalid link attribution".into()));
            }
            records.push(record);
        }
    }
    Ok(records)
}

fn add_sync_bytes(total: &mut usize, amount: usize, max_total_bytes: usize) -> Result<()> {
    *total = total
        .checked_add(amount)
        .ok_or_else(|| SyncError::Ref("sync byte count overflow".into()))?;
    if *total > max_total_bytes {
        return Err(SyncError::Ref(
            "sync payload exceeds total size limit".into(),
        ));
    }
    Ok(())
}

fn verify_existing_node(repo: &Repository, oid: git2::Oid, expected_id: &str) -> Result<()> {
    let blob = repo.find_blob(oid)?;
    let node: SyncNode = serde_json::from_slice(blob.content())?;
    if node.interaction.id.to_string() != expected_id {
        return Err(SyncError::Ref("immutable node filename/id mismatch".into()));
    }
    validate_final_node_bytes(blob.content())
}

/// This is intentionally after serialization: it verifies the precise bytes
/// handed to libgit2, not merely the Rust value used to construct them.
fn validate_final_node_bytes(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_SYNC_BLOB_BYTES {
        return Err(SyncError::Ref(
            "node serialization exceeds size limit".into(),
        ));
    }
    let node: SyncNode = serde_json::from_slice(bytes)?;
    validate_remote_node(&node)?;
    for tool in &node.tool_executions {
        crate::privacy::validate_sanitized_json(&tool.arguments)
            .map_err(|e| SyncError::Ref(format!("unsafe final tool arguments: {e}")))?;
    }
    inspect_final_strings(bytes)
}

fn validate_final_link_bytes(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_SYNC_BLOB_BYTES {
        return Err(SyncError::Ref(
            "link serialization exceeds size limit".into(),
        ));
    }
    let record: SyncLinkRecord = serde_json::from_slice(bytes)?;
    if record
        .interaction_id
        .parse::<crate::models::InteractionId>()
        .is_err()
        || !is_safe_commit_sha(&record.git_commit_hash)
        || !is_automatic_link_type(&record.link_type)
        || record
            .linked_by
            .as_deref()
            .is_some_and(|v| v.is_empty() || v.len() > 1024 * 1024)
    {
        return Err(SyncError::Ref("invalid final link event".into()));
    }
    inspect_final_strings(bytes)
}

fn inspect_final_strings(bytes: &[u8]) -> Result<()> {
    fn visit(value: &serde_json::Value) -> Result<()> {
        match value {
            serde_json::Value::String(text) => {
                // scrub is marker-aware.  Thus an unchanged value is either
                // clean or a valid CVC marker, while a changed one is a leak.
                if crate::privacy::scrub(text)
                    .map_err(|e| SyncError::Ref(format!("final string invalid: {e}")))?
                    != *text
                {
                    return Err(SyncError::Ref(
                        "unredacted secret in final Git bytes".into(),
                    ));
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value)?;
                }
            }
            serde_json::Value::Object(values) => {
                for (key, value) in values {
                    visit(&serde_json::Value::String(key.clone()))?;
                    visit(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(&serde_json::from_slice(bytes)?)
}

/// Fetch using the already snapshotted effective destination.  The named remote is
/// intentionally not reopened, preventing a config race from changing consent or
/// transport after approval.
pub fn fetch_and_pull_destination(
    repo: &Repository,
    db: &CvcStore,
    destination: &crate::privacy::RemoteDestination,
) -> Result<usize> {
    fetch_destination(repo, destination)?;
    pull_destination(repo, db, destination)
}

/// Fetches the approved CVC ref without touching the local CVC database.
pub fn fetch_destination(
    repo: &Repository,
    destination: &crate::privacy::RemoteDestination,
) -> Result<()> {
    let ref_name = "refs/cvc/main";
    let remote_tracking_ref = format!("refs/remotes/{}/cvc/main", destination.name);
    let refspec = format!("{}:{}", ref_name, remote_tracking_ref);
    // Delete stale tracking state: absence is established only by a successful
    // fetch/list operation that leaves no advertised CVC ref.
    if let Ok(mut stale) = repo.find_reference(&remote_tracking_ref) {
        stale.delete()?;
    }
    let mut remote = repo.remote_anonymous(&destination.effective_url)?;
    let mut list_callbacks = git2::RemoteCallbacks::new();
    list_callbacks.credentials(|_url, username_from_url, _allowed_types| {
        git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
    });
    // An exact-ref fetch reports absence as an error on some transports. First
    // authenticated advertisement distinguishes proven absence from transport
    // failure and prevents treating an arbitrary fetch error as empty.
    remote.connect_auth(Direction::Fetch, Some(list_callbacks), None)?;
    let advertised = remote
        .list()?
        .iter()
        .any(|head| head.name() == "refs/cvc/main");
    remote.disconnect()?;
    if advertised {
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(|_url, username_from_url, _allowed_types| {
            git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
        });
        let mut fetch_opts = git2::FetchOptions::new();
        fetch_opts.remote_callbacks(callbacks);
        remote.fetch(&[&refspec], Some(&mut fetch_opts), None)?;
    }

    Ok(())
}

/// Imports an already fetched tracking ref into the supplied store.
pub fn pull_destination(
    repo: &Repository,
    db: &CvcStore,
    destination: &crate::privacy::RemoteDestination,
) -> Result<usize> {
    let remote_tracking_ref = format!("refs/remotes/{}/cvc/main", destination.name);
    if repo.find_reference(&remote_tracking_ref).is_err() {
        return Ok(0);
    }
    let before_count = db.get_all_interaction_ids()?.len();
    pull_from_ref_for_remote(
        repo,
        db,
        &remote_tracking_ref,
        Some(&destination.fingerprint),
    )?;
    let new_count = db
        .get_all_interaction_ids()?
        .len()
        .saturating_sub(before_count);

    Ok(new_count)
}

pub fn push_temp_ref(
    repo: &Repository,
    destination: &crate::privacy::RemoteDestination,
    temporary_ref: &str,
) -> Result<()> {
    if !validate_ref_name(temporary_ref) {
        return Err(SyncError::Ref("invalid temporary ref".into()));
    }
    let mut remote = repo.remote_anonymous(&destination.effective_url)?;
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|_url, username_from_url, _allowed_types| {
        git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
    });
    let mut options = git2::PushOptions::new();
    options.remote_callbacks(callbacks);
    remote.push(
        &[&format!("{temporary_ref}:refs/cvc/main")],
        Some(&mut options),
    )?;
    Ok(())
}

/// Remove an untransported projection candidate. Candidates are local staging
/// refs only; accepting any other ref here would make failure cleanup capable
/// of deleting publication state.
pub fn cleanup_projection_ref(repo: &Repository, temporary_ref: &str) -> Result<()> {
    if !temporary_ref.starts_with("refs/cvc/candidate-") || !validate_ref_name(temporary_ref) {
        return Err(SyncError::Ref("invalid projection candidate ref".into()));
    }
    if let Ok(mut reference) = repo.find_reference(temporary_ref) {
        reference.delete()?;
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

#[cfg(test)]
mod derivation_graph_tests {
    use super::*;
    use crate::models::{
        CommitSha, DerivationOrigin, DerivationRelation, Evidence, EvidenceKind, InteractionId,
    };

    fn event(id: &str, interaction: &str, sources: Vec<String>) -> DerivationEvent {
        DerivationEvent {
            event_id: id.into(),
            interaction_id: interaction.parse::<InteractionId>().expect("UUID fixture"),
            target_commit: CommitSha::new("c".repeat(40)),
            relation: DerivationRelation::RewriteExact,
            evidence: Evidence {
                version: 1,
                kind: EvidenceKind::LocallyObserved,
            },
            origin: DerivationOrigin::LocalHook,
            source_event_ids: sources,
            old_oid: Some(CommitSha::new("a".repeat(40))),
            new_oid: Some(CommitSha::new("c".repeat(40))),
            range_id: None,
            linked_by: None,
        }
    }

    #[test]
    fn graph_rejects_empty_dangling_cross_interaction_and_cycle() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let first = "11111111-1111-4111-8111-111111111111";
        let second = "22222222-2222-4222-8222-222222222222";
        let legacy = HashSet::new();
        assert!(validate_closed_derivation_graph(&[event(&a, first, vec![])], &legacy).is_err());
        assert!(
            validate_closed_derivation_graph(&[event(&a, first, vec![b.clone()])], &legacy)
                .is_err()
        );
        assert!(validate_closed_derivation_graph(
            &[
                event(
                    &a,
                    first,
                    vec![format!("legacy:{first}:{}", "d".repeat(40))]
                ),
                event(&b, second, vec![a.clone()]),
            ],
            &HashSet::from([(first.into(), "d".repeat(40))]),
        )
        .is_err());
        assert!(validate_closed_derivation_graph(
            &[
                event(&a, first, vec![b.clone()]),
                event(&b, first, vec![a.clone()]),
            ],
            &legacy,
        )
        .is_err());
    }
}
