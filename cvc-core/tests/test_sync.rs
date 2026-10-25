use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::*;
use cvc_core::privacy::{McpCapture, PreparedPolicy};
use cvc_core::sync::{self, SyncNode};
use git2::{FileMode, ObjectType, Repository, Signature};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::sync::mpsc;
use std::time::Duration;
use tempfile::TempDir;

const TEST_DESTINATION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Pins what `baseline` currently resolves to; `""` means "no remote baseline"
/// (a tracking ref that does not exist, so the projection starts empty and
/// verifies that nothing appeared).
fn pinned(repo: &Repository, baseline: &str) -> sync::ImportedBaseline {
    let name = if baseline.is_empty() {
        "refs/remotes/unpublished/cvc/main"
    } else {
        baseline
    };
    sync::ImportedBaseline::observe(repo, name).unwrap()
}

#[test]
fn historical_redaction_plan_v1_fixture_round_trips_exactly() -> anyhow::Result<()> {
    let fixture = include_str!("fixtures/redaction-plan-v1.json");
    let plan: RedactionPlan = serde_json::from_str(fixture)?;
    let encoded = serde_json::to_string_pretty(&plan)?;
    assert_eq!(encoded, fixture.trim_end());
    assert!(!encoded.contains("common_git_dir_fingerprint"));
    assert_eq!(plan.format, "cvc.redaction-plan/v1");
    assert_eq!(plan.version, 1);
    Ok(())
}

#[test]
fn pull_rejects_non_utf8_sync_paths() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let blob = repo.blob(b"{}")?;
    let mut raw = b"100644 bad\xff\0".to_vec();
    raw.extend_from_slice(blob.as_bytes());
    let tree_oid = repo.odb()?.write(ObjectType::Tree, &raw)?;
    let tree = repo.find_tree(tree_oid)?;
    let signature = Signature::now("test", "test@example.test")?;
    repo.commit(
        Some("refs/cvc/non-utf8"),
        &signature,
        &signature,
        "non utf8",
        &tree,
        &[],
    )?;
    let store = CvcStore::open(temp.path().join("index.db"))?;
    assert!(sync::pull_from_ref(&repo, &store, "refs/cvc/non-utf8").is_err());
    Ok(())
}

trait CaptureFixture {
    fn create_conversation(&self, _: &Conversation) -> cvc_core::db::Result<()>;
    fn create_interaction(&self, interaction: &Interaction) -> cvc_core::db::Result<()>;
}
impl CaptureFixture for CvcStore {
    fn create_conversation(&self, _: &Conversation) -> cvc_core::db::Result<()> {
        Ok(())
    }
    fn create_interaction(&self, interaction: &Interaction) -> cvc_core::db::Result<()> {
        self.capture_mcp(McpCapture::new(
            Conversation {
                id: interaction.conversation_id.clone(),
                title: "fixture".into(),
                created_at: interaction.timestamp,
            },
            interaction.clone(),
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .and_then(|_| {
            self.share_conversation_for_remote(
                &interaction.conversation_id,
                TEST_DESTINATION,
                FutureSharePolicy::Private,
            )
            .map(|_| ())
        })
    }
}

/// Test-only transport shim: make the current destination ref an explicit remote
/// baseline, then accept the exact candidate produced by the production projection.
/// It deliberately never lets the candidate seed itself from a mutable local ref.
fn project_fixture_to_ref(
    repo: &Repository,
    store: &CvcStore,
    ref_name: &str,
) -> anyhow::Result<()> {
    const BASELINE: &str = "refs/remotes/fixture/cvc/main";
    let baseline = if let Ok(reference) = repo.find_reference(ref_name) {
        repo.reference(
            BASELINE,
            reference.target().expect("fixture ref has direct target"),
            true,
            "fixture: verified remote baseline",
        )?;
        BASELINE
    } else {
        ""
    };
    let projection =
        sync::push_projection_to_ref(repo, store, &pinned(repo, baseline), TEST_DESTINATION)?;
    match projection {
        sync::ProjectionResult::Candidate { oid, candidate, .. } => {
            repo.reference(ref_name, oid, true, "fixture: remote accepted candidate")?;
            drop(candidate);
        }
        sync::ProjectionResult::NoChanges { .. } => {}
    }
    Ok(())
}

fn derivation(
    interaction_id: &InteractionId,
    commit: char,
    source_event_ids: Vec<String>,
) -> DerivationEvent {
    let mut event = DerivationEvent {
        event_id: String::new(),
        interaction_id: interaction_id.clone(),
        target_commit: CommitSha::new(commit.to_string().repeat(40)),
        relation: DerivationRelation::Generated,
        evidence: Evidence {
            version: 1,
            kind: EvidenceKind::LocallyObserved,
        },
        origin: DerivationOrigin::LocalLinker,
        source_event_ids,
        old_oid: None,
        new_oid: None,
        range_id: None,
        linked_by: None,
    };
    event.event_id = event.canonical_id();
    event
}

fn trust_and_authorize_event(
    db_path: &std::path::Path,
    event: &DerivationEvent,
) -> anyhow::Result<()> {
    let db = Connection::open(db_path)?;
    db.execute("INSERT INTO derivation_observations(event_id,source_fingerprint,source_key,origin,trusted_local) VALUES(?1,NULL,'fixture-local','local_api',1)", [&event.event_id])?;
    db.execute("INSERT INTO derivation_authorizations(event_id,remote_fingerprint,source_event_id) VALUES(?1,?2,?1)", rusqlite::params![event.event_id, TEST_DESTINATION])?;
    Ok(())
}

fn insert_event_fixture(db_path: &std::path::Path, event: &DerivationEvent) -> anyhow::Result<()> {
    let db = Connection::open(db_path)?;
    db.execute(
        "INSERT INTO derivation_events(event_id,interaction_id,target_commit,relation,evidence_version,evidence_kind,origin,source_event_ids,old_oid,new_oid,range_id,linked_by,payload) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        rusqlite::params![
            event.event_id,
            event.interaction_id.to_string(),
            event.target_commit.as_str(),
            match event.relation { DerivationRelation::Generated => "generated", DerivationRelation::Temporal => "temporal", DerivationRelation::Verified => "verified", DerivationRelation::RewriteExact => "rewrite_exact", DerivationRelation::SquashExact => "squash_exact" },
            event.evidence.version,
            match event.evidence.kind { EvidenceKind::LocallyObserved => "locally_observed", EvidenceKind::ImportedLegacy => "imported_legacy", EvidenceKind::RemoteAssertion => "remote_assertion" },
            match event.origin { DerivationOrigin::LocalHook => "local_hook", DerivationOrigin::LocalLinker => "local_linker", DerivationOrigin::RemoteAssertion => "remote_assertion", DerivationOrigin::LegacyImport => "legacy_import" },
            serde_json::to_string(&event.source_event_ids)?,
            event.old_oid.as_ref().map(CommitSha::as_str),
            event.new_oid.as_ref().map(CommitSha::as_str),
            event.range_id,
            event.linked_by,
            serde_json::to_string(event)?,
        ],
    )?;
    Ok(())
}

fn wire_event_ids(repo: &Repository, tree: &git2::Tree<'_>) -> anyhow::Result<Vec<String>> {
    let Some(events) = tree.get_name("events") else {
        return Ok(Vec::new());
    };
    let mut ids = Vec::new();
    for shard in events.to_object(repo)?.peel_to_tree()?.iter() {
        for entry in shard.to_object(repo)?.peel_to_tree()?.iter() {
            let event: DerivationEvent =
                serde_json::from_slice(entry.to_object(repo)?.peel_to_blob()?.content())?;
            ids.push(event.event_id);
        }
    }
    ids.sort();
    Ok(ids)
}

#[test]
fn tombstone_prunes_transitive_event_dag_and_rebuilds_index_from_one_tree() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let db_path = temp.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    let target = Interaction {
        id: InteractionId::new(),
        conversation_id: "target".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "remove".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    let retained = Interaction {
        id: InteractionId::new(),
        conversation_id: "retained".into(),
        user_prompt: "keep".into(),
        ..target.clone()
    };
    store.create_interaction(&target)?;
    store.create_interaction(&retained)?;
    let first = derivation(&target.id, 'a', Vec::new());
    let second = derivation(&target.id, 'b', vec![first.event_id.clone()]);
    let third = derivation(&target.id, 'c', vec![second.event_id.clone()]);
    let unrelated = derivation(&retained.id, 'd', Vec::new());
    for event in [&first, &second, &third, &unrelated] {
        insert_event_fixture(&db_path, event)?;
        trust_and_authorize_event(&db_path, event)?;
    }
    project_fixture_to_ref(&repo, &store, "refs/cvc/dag-prune")?;
    assert_eq!(
        wire_event_ids(
            &repo,
            &repo
                .find_reference("refs/cvc/dag-prune")?
                .peel_to_commit()?
                .tree()?
        )?
        .len(),
        4
    );

    store.tombstone_remote(
        &target.id,
        TEST_DESTINATION,
        TombstoneReasonCode::Security,
        None,
    )?;
    let later = derivation(&retained.id, 'e', vec![unrelated.event_id.clone()]);
    insert_event_fixture(&db_path, &later)?;
    trust_and_authorize_event(&db_path, &later)?;
    project_fixture_to_ref(&repo, &store, "refs/cvc/dag-prune")?;
    let tree = repo
        .find_reference("refs/cvc/dag-prune")?
        .peel_to_commit()?
        .tree()?;
    assert_eq!(
        wire_event_ids(&repo, &tree)?,
        vec![later.event_id.clone(), unrelated.event_id.clone()]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    let index = tree
        .get_name("by-commit")
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    for removed in ['a', 'b', 'c'] {
        assert!(index.get_name(&removed.to_string().repeat(40)).is_none());
    }
    for kept in ['d', 'e'] {
        assert!(index.get_name(&kept.to_string().repeat(40)).is_some());
    }
    Ok(())
}

#[test]
fn outbound_events_require_trusted_observation_and_exact_destination_authority(
) -> anyhow::Result<()> {
    const OTHER: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let db_path = temp.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    let item = Interaction {
        id: InteractionId::new(),
        conversation_id: "trust".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "authority".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&item)?;
    store.share_conversation_for_remote(
        &item.conversation_id,
        OTHER,
        FutureSharePolicy::Private,
    )?;

    let allowed = derivation(&item.id, 'a', Vec::new());
    insert_event_fixture(&db_path, &allowed)?;
    trust_and_authorize_event(&db_path, &allowed)?;
    let mut assertion = derivation(&item.id, 'b', Vec::new());
    assertion.evidence.kind = EvidenceKind::RemoteAssertion;
    assertion.origin = DerivationOrigin::RemoteAssertion;
    assertion.event_id = assertion.canonical_id();
    insert_event_fixture(&db_path, &assertion)?;
    {
        let db = Connection::open(&db_path)?;
        db.execute("INSERT INTO derivation_observations(event_id,source_fingerprint,source_key,origin,trusted_local) VALUES(?1,NULL,'fingerprintless','remote_sync',0)", [&assertion.event_id])?;
        db.execute("INSERT INTO derivation_authorizations(event_id,remote_fingerprint,source_event_id) VALUES(?1,?2,?1)", rusqlite::params![assertion.event_id, TEST_DESTINATION])?;
    }
    let projection =
        sync::push_projection_to_ref(&repo, &store, &pinned(&repo, ""), TEST_DESTINATION)?;
    let oid = match projection {
        sync::ProjectionResult::Candidate { oid, .. } => oid,
        _ => anyhow::bail!("expected projection"),
    };
    assert_eq!(
        wire_event_ids(&repo, &repo.find_commit(oid)?.tree()?)?,
        vec![allowed.event_id.clone()]
    );

    let other = sync::push_projection_to_ref(&repo, &store, &pinned(&repo, ""), OTHER)?;
    let oid = match other {
        sync::ProjectionResult::Candidate { oid, .. } => oid,
        _ => anyhow::bail!("node should project"),
    };
    let tree = repo.find_commit(oid)?.tree()?;
    assert!(
        wire_event_ids(&repo, &tree)?.is_empty(),
        "destination A authority must not cross to B"
    );
    assert!(
        tree.get_name("by-commit").is_none(),
        "assertions must not leak index pointers"
    );
    Ok(())
}

#[test]
fn format5_always_emits_and_requires_empty_reserved_trees() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let store = CvcStore::open(temp.path().join("source.db"))?;
    let item = Interaction {
        id: InteractionId::new(),
        conversation_id: "empty-v5".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "empty namespaces".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&item)?;
    project_fixture_to_ref(&repo, &store, "refs/cvc/empty-v5")?;
    let tree = repo
        .find_reference("refs/cvc/empty-v5")?
        .peel_to_commit()?
        .tree()?;
    for namespace in ["events", "ranges"] {
        let subtree = tree
            .get_name(namespace)
            .expect("FORMAT5 namespace")
            .to_object(&repo)?
            .peel_to_tree()?;
        assert!(subtree.is_empty());
    }
    let mut malformed = repo.treebuilder(Some(&tree))?;
    malformed.remove("ranges")?;
    let malformed = repo.find_tree(malformed.write()?)?;
    let signature = Signature::now("test", "test@example.test")?;
    repo.commit(
        Some("refs/cvc/missing-v5-namespace"),
        &signature,
        &signature,
        "missing namespace",
        &malformed,
        &[],
    )?;
    let imported = CvcStore::open(temp.path().join("imported.db"))?;
    assert!(sync::pull_from_ref(&repo, &imported, "refs/cvc/missing-v5-namespace").is_err());
    Ok(())
}

#[test]
fn projection_refuses_dangling_exact_event_instead_of_pruning_it() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let db_path = temp.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    let item = Interaction {
        id: InteractionId::new(),
        conversation_id: "dangling-event".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "reject dangling".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&item)?;
    let mut event = derivation(&item.id, 'b', vec!["f".repeat(64)]);
    event.relation = DerivationRelation::RewriteExact;
    event.old_oid = Some(CommitSha::new("a".repeat(40)));
    event.new_oid = Some(CommitSha::new("b".repeat(40)));
    event.event_id = event.canonical_id();
    insert_event_fixture(&db_path, &event)?;
    trust_and_authorize_event(&db_path, &event)?;
    assert!(
        sync::push_projection_to_ref(&repo, &store, &pinned(&repo, ""), TEST_DESTINATION).is_err()
    );
    Ok(())
}

#[test]
fn pull_rejects_dangling_event_before_tombstone_pruning_with_duplicate_legacy_endpoint(
) -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let source = CvcStore::open(temp.path().join("source.db"))?;
    let item = Interaction {
        id: InteractionId::new(),
        conversation_id: "pre-prune-validation".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "must reject malformed wire graph".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&item)?;
    source.link_interaction(&item.id, &CommitSha::new("a".repeat(40)), "generated")?;
    project_fixture_to_ref(&repo, &source, "refs/cvc/pre-prune-valid")?;
    let baseline = repo
        .find_reference("refs/cvc/pre-prune-valid")?
        .peel_to_commit()?
        .tree()?;
    assert!(baseline.get_name("links").is_some());

    let tombstone = Tombstone::new(item.id.clone(), TombstoneReasonCode::Security, None);
    let tombstone_file = format!("{}.json", item.id);
    let tombstone_shard_name = &item.id.as_str()[..2];
    let mut tombstone_shard = repo.treebuilder(None)?;
    tombstone_shard.insert(
        &tombstone_file,
        repo.blob(&serde_json::to_vec(&tombstone)?)?,
        FileMode::Blob.into(),
    )?;
    let mut tombstones = repo.treebuilder(None)?;
    tombstones.insert(
        tombstone_shard_name,
        tombstone_shard.write()?,
        FileMode::Tree.into(),
    )?;

    let mut dangling = derivation(&item.id, 'b', vec!["f".repeat(64)]);
    dangling.relation = DerivationRelation::RewriteExact;
    dangling.old_oid = Some(CommitSha::new("a".repeat(40)));
    dangling.new_oid = Some(CommitSha::new("b".repeat(40)));
    dangling.event_id = dangling.canonical_id();
    let event_file = format!("{}.json", dangling.event_id);
    let mut event_shard = repo.treebuilder(None)?;
    event_shard.insert(
        &event_file,
        repo.blob(&serde_json::to_vec(&dangling)?)?,
        FileMode::Blob.into(),
    )?;
    let mut events = repo.treebuilder(Some(
        &baseline
            .get_name("events")
            .expect("events")
            .to_object(&repo)?
            .peel_to_tree()?,
    ))?;
    events.insert(
        &dangling.event_id[..2],
        event_shard.write()?,
        FileMode::Tree.into(),
    )?;
    let mut root = repo.treebuilder(Some(&baseline))?;
    root.insert("tombstones", tombstones.write()?, FileMode::Tree.into())?;
    root.insert("events", events.write()?, FileMode::Tree.into())?;
    let malformed = repo.find_tree(root.write()?)?;
    let signature = Signature::now("test", "test@example.test")?;
    repo.commit(
        Some("refs/cvc/pre-prune-malformed"),
        &signature,
        &signature,
        "malformed before pruning",
        &malformed,
        &[],
    )?;
    let imported = CvcStore::open(temp.path().join("imported.db"))?;
    assert!(sync::pull_from_ref(&repo, &imported, "refs/cvc/pre-prune-malformed").is_err());
    assert!(imported.get_all_interaction_ids()?.is_empty());
    Ok(())
}

#[test]
fn format5_rejects_malformed_ranges_and_by_commit_shapes_or_targets() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let source = CvcStore::open(temp.path().join("source.db"))?;
    let item = Interaction {
        id: InteractionId::new(),
        conversation_id: "index-validation".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "index".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&item)?;
    source.link_interaction(&item.id, &CommitSha::new("a".repeat(40)), "generated")?;
    project_fixture_to_ref(&repo, &source, "refs/cvc/valid-index")?;
    let baseline = repo
        .find_reference("refs/cvc/valid-index")?
        .peel_to_commit()?
        .tree()?;
    let signature = Signature::now("test", "test@example.test")?;

    let publish = |name: &str, tree: git2::Oid| -> anyhow::Result<()> {
        let tree = repo.find_tree(tree)?;
        repo.commit(
            Some(name),
            &signature,
            &signature,
            "malformed fixture",
            &tree,
            &[],
        )?;
        Ok(())
    };
    // Non-empty ranges are not accepted until their bounded canonical schema is
    // implemented; silently retaining arbitrary evidence would be a trust bug.
    let mut ranges = repo.treebuilder(None)?;
    ranges.insert("unexpected.json", repo.blob(b"{}")?, FileMode::Blob.into())?;
    let mut root = repo.treebuilder(Some(&baseline))?;
    root.insert("ranges", ranges.write()?, FileMode::Tree.into())?;
    publish("refs/cvc/bad-ranges", root.write()?)?;

    let empty = repo.blob(b"")?;
    let bad_body = repo.blob(b"not empty")?;
    for (case, commit_name, pointer_name, pointer_oid, pointer_mode) in [
        (
            "uppercase",
            "A".repeat(40),
            item.id.to_string(),
            empty,
            FileMode::Blob,
        ),
        (
            "commit",
            "short".into(),
            item.id.to_string(),
            empty,
            FileMode::Blob,
        ),
        (
            "uuid",
            "a".repeat(40),
            "not-a-uuid".into(),
            empty,
            FileMode::Blob,
        ),
        (
            "body",
            "a".repeat(40),
            item.id.to_string(),
            bad_body,
            FileMode::Blob,
        ),
        (
            "nesting",
            "a".repeat(40),
            item.id.to_string(),
            repo.treebuilder(None)?.write()?,
            FileMode::Tree,
        ),
        (
            "stale",
            "b".repeat(40),
            item.id.to_string(),
            empty,
            FileMode::Blob,
        ),
    ] {
        let mut pointers = repo.treebuilder(None)?;
        pointers.insert(&pointer_name, pointer_oid, pointer_mode.into())?;
        let mut index = repo.treebuilder(None)?;
        index.insert(&commit_name, pointers.write()?, FileMode::Tree.into())?;
        let mut root = repo.treebuilder(Some(&baseline))?;
        root.insert("by-commit", index.write()?, FileMode::Tree.into())?;
        publish(&format!("refs/cvc/bad-index-{case}"), root.write()?)?;
    }
    let mut root = repo.treebuilder(Some(&baseline))?;
    root.insert(
        "by-commit",
        repo.blob(b"wrong root type")?,
        FileMode::Blob.into(),
    )?;
    publish("refs/cvc/bad-index-root", root.write()?)?;

    for name in [
        "bad-ranges",
        "bad-index-root",
        "bad-index-uppercase",
        "bad-index-commit",
        "bad-index-uuid",
        "bad-index-body",
        "bad-index-nesting",
        "bad-index-stale",
    ] {
        let store = CvcStore::open(temp.path().join(format!("{name}.db")))?;
        assert!(
            sync::pull_from_ref(&repo, &store, &format!("refs/cvc/{name}")).is_err(),
            "accepted {name}"
        );
    }
    Ok(())
}

#[test]
fn hard_redaction_fixture_removes_multihop_derivation_closure() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let db_path = temp.path().join("hard-dag.db");
    let store = CvcStore::open(&db_path)?;
    let target = Interaction {
        id: InteractionId::new(),
        conversation_id: "hard-target".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "remove".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    let retained = Interaction {
        id: InteractionId::new(),
        conversation_id: "hard-retained".into(),
        user_prompt: "retain".into(),
        ..target.clone()
    };
    store.create_interaction(&target)?;
    store.create_interaction(&retained)?;
    let first = derivation(&target.id, 'a', Vec::new());
    let second = derivation(&target.id, 'b', vec![first.event_id.clone()]);
    let third = derivation(&target.id, 'c', vec![second.event_id.clone()]);
    let unrelated = derivation(&retained.id, 'd', Vec::new());
    for event in [&first, &second, &third, &unrelated] {
        insert_event_fixture(&db_path, event)?;
        trust_and_authorize_event(&db_path, event)?;
    }
    project_fixture_to_ref(&repo, &store, "refs/cvc/hard-dag")?;
    let unredacted = repo
        .find_reference("refs/cvc/hard-dag")?
        .peel_to_commit()?
        .tree()?;
    let stale_events = unredacted.get_name("events").unwrap().id();
    let stale_index = unredacted.get_name("by-commit").unwrap().id();

    store.tombstone_remote(
        &target.id,
        TEST_DESTINATION,
        TombstoneReasonCode::Security,
        None,
    )?;
    project_fixture_to_ref(&repo, &store, "refs/cvc/hard-dag")?;
    let suppressed = repo
        .find_reference("refs/cvc/hard-dag")?
        .peel_to_commit()?
        .tree()?;
    let mut stale = repo.treebuilder(Some(&suppressed))?;
    stale.insert("events", stale_events, FileMode::Tree.into())?;
    stale.insert("by-commit", stale_index, FileMode::Tree.into())?;
    let stale_tree = repo.find_tree(stale.write()?)?;
    let signature = Signature::now("test", "test@example.test")?;
    let stale_tip = repo.commit(
        None,
        &signature,
        &signature,
        "stale redaction evidence",
        &stale_tree,
        &[],
    )?;
    repo.reference(
        "refs/remotes/fixture/cvc/main",
        stale_tip,
        true,
        "stale fixture",
    )?;

    let candidate = sync::build_hard_redaction_plan(
        &repo,
        Some("refs/remotes/fixture/cvc/main"),
        TEST_DESTINATION,
        &target.id,
    )?;
    let replacement = repo
        .find_commit(git2::Oid::from_str(&candidate.plan.replacement_commit)?)?
        .tree()?;
    assert_eq!(
        wire_event_ids(&repo, &replacement)?,
        vec![unrelated.event_id]
    );
    let index = replacement
        .get_name("by-commit")
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    for removed in ['a', 'b', 'c'] {
        assert!(index.get_name(&removed.to_string().repeat(40)).is_none());
    }
    assert!(index.get_name(&"d".repeat(40)).is_some());
    assert_eq!(
        repo.find_reference("refs/remotes/fixture/cvc/main")?
            .target(),
        Some(stale_tip)
    );
    Ok(())
}

#[test]
fn post_node_link_records_reach_fresh_and_existing_pulls() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let source = CvcStore::open(temp_dir.path().join("source.db"))?;
    let conversation = Conversation {
        id: "late-link".into(),
        title: "late-link".into(),
        created_at: Utc::now(),
    };
    source.create_conversation(&conversation)?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: conversation.id.clone(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "floating first".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&interaction)?;
    let ref_name = "refs/cvc/late-link";
    project_fixture_to_ref(&repo, &source, ref_name)?;

    let existing = CvcStore::open(temp_dir.path().join("existing.db"))?;
    sync::pull_from_ref(&repo, &existing, ref_name)?;
    assert!(existing.get_artifact_links(&interaction.id)?.is_empty());

    let sha = CommitSha::new("a".repeat(40));
    source.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &sha,
        "generated",
        Some("linker@example.com"),
    )?;
    project_fixture_to_ref(&repo, &source, ref_name)?;

    let fresh = CvcStore::open(temp_dir.path().join("fresh.db"))?;
    sync::pull_from_ref(&repo, &fresh, ref_name)?;
    sync::pull_from_ref(&repo, &existing, ref_name)?;
    for store in [&fresh, &existing] {
        let links = store.get_artifact_links(&interaction.id)?;
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].link_type, "generated");
        assert_eq!(links[0].linked_by.as_deref(), Some("linker@example.com"));
    }
    Ok(())
}

#[test]
fn v4_tombstone_precedes_nodes_links_and_stale_clone_data() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let source = CvcStore::open(temp_dir.path().join("source.db"))?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "redact-me".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "must disappear".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&interaction)?;
    source.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &CommitSha::new("a".repeat(40)),
        "generated",
        None,
    )?;
    let ref_name = "refs/cvc/tombstone";
    project_fixture_to_ref(&repo, &source, ref_name)?;
    let stale_oid = repo.find_reference(ref_name)?.target().expect("direct ref");
    repo.reference(
        "refs/cvc/stale-before-redaction",
        stale_oid,
        true,
        "fixture stale clone",
    )?;

    // A second clone first sees the published node and link.
    let second_clone = CvcStore::open(temp_dir.path().join("second.db"))?;
    sync::pull_from_ref(&repo, &second_clone, ref_name)?;
    assert!(second_clone.get_interaction(&interaction.id)?.is_some());
    assert_eq!(second_clone.get_artifact_links(&interaction.id)?.len(), 1);

    source.apply_tombstones(
        &[cvc_core::models::Tombstone::new(
            interaction.id.clone(),
            TombstoneReasonCode::Security,
            None,
        )],
        "fixture",
        Some(TEST_DESTINATION),
    )?;
    project_fixture_to_ref(&repo, &source, ref_name)?;
    sync::pull_from_ref(&repo, &second_clone, ref_name)?;
    assert!(second_clone.is_tombstoned(&interaction.id)?);
    assert!(second_clone.get_interaction(&interaction.id)?.is_none());
    assert!(second_clone.get_artifact_links(&interaction.id)?.is_empty());

    // Replaying a stale clone's old node/link tree must not resurrect it.
    sync::pull_from_ref(&repo, &second_clone, "refs/cvc/stale-before-redaction")?;
    assert!(second_clone.get_interaction(&interaction.id)?.is_none());

    let tree = repo.find_reference(ref_name)?.peel_to_commit()?.tree()?;
    assert!(tree.get_name(&format!("{}.json", interaction.id)).is_none());
    let nodes = tree
        .get_name("nodes")
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    let shard = nodes
        .get_name(&interaction.id.as_str()[..2])
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    assert!(shard
        .get_name(&format!("{}.json", interaction.id))
        .is_none());
    let links = tree
        .get_name("links")
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    assert!(links.get_name(&interaction.id.to_string()).is_none());
    Ok(())
}

#[test]
fn hard_redaction_plan_verifies_applies_locally_and_retains_tombstone() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let signature = Signature::now("test", "test@example.test")?;
    let empty_tree = repo.treebuilder(None)?.write()?;
    repo.commit(
        Some("refs/heads/main"),
        &signature,
        &signature,
        "initial",
        &repo.find_tree(empty_tree)?,
        &[],
    )?;
    repo.set_head("refs/heads/main")?;
    let sibling_path = temp_dir.path().join("sibling");
    assert!(std::process::Command::new("git")
        .args([
            "-C",
            temp_dir.path().to_str().unwrap(),
            "worktree",
            "add",
            "-b",
            "redaction-sibling",
            sibling_path.to_str().unwrap(),
        ])
        .status()?
        .success());
    let sibling = Repository::open(&sibling_path)?;
    assert!(repo.remotes()?.is_empty());
    assert!(sibling.remotes()?.is_empty());
    let store = CvcStore::open(temp_dir.path().join("store.db"))?;
    let target = Interaction {
        id: InteractionId::new(),
        conversation_id: "redaction-plan".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "remove me".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    let unrelated = Interaction {
        id: InteractionId::new(),
        user_prompt: "retain me".into(),
        ..target.clone()
    };
    store.create_interaction(&target)?;
    store.create_interaction(&unrelated)?;
    store.link_automatic_interactions(
        std::slice::from_ref(&target.id),
        &CommitSha::new("d".repeat(40)),
        "generated",
        None,
    )?;
    project_fixture_to_ref(&repo, &store, "refs/cvc/redaction-plan")?;
    store.tombstone_remote(
        &target.id,
        TEST_DESTINATION,
        TombstoneReasonCode::Security,
        None,
    )?;
    project_fixture_to_ref(&repo, &store, "refs/cvc/redaction-plan")?;
    let tip = repo
        .find_reference("refs/cvc/redaction-plan")?
        .target()
        .unwrap();
    repo.reference(
        "refs/remotes/fixture/cvc/main",
        tip,
        true,
        "fixture baseline",
    )?;

    let candidate = sync::build_hard_redaction_plan(
        &repo,
        Some("refs/remotes/fixture/cvc/main"),
        TEST_DESTINATION,
        &target.id,
    )?;
    let plan = candidate.plan.clone();
    assert_eq!(plan.format, "cvc.redaction-plan/v2");
    assert_eq!(
        serde_json::to_string_pretty(&plan)?,
        serde_json::to_string_pretty(&serde_json::from_str::<RedactionPlan>(
            &serde_json::to_string_pretty(&plan)?
        )?)?
    );
    let encoded = serde_json::to_string(&plan)?;
    let duplicate_format = encoded.replacen('{', r#"{"format":"cvc.redaction-plan/v1","#, 1);
    assert!(
        serde_json::from_str::<RedactionPlan>(&duplicate_format).is_err(),
        "duplicate discriminators must not be collapsed by an intermediate map"
    );
    let duplicate_version = encoded.replacen("\"version\":2", "\"version\":1,\"version\":2", 1);
    assert!(serde_json::from_str::<RedactionPlan>(&duplicate_version).is_err());
    let duplicate_fingerprint = encoded.replacen(
        "\"repository_fingerprint\":",
        "\"repository_fingerprint\":\"wrong\",\"repository_fingerprint\":",
        1,
    );
    assert!(serde_json::from_str::<RedactionPlan>(&duplicate_fingerprint).is_err());
    let mut missing_fingerprint = serde_json::to_value(&plan)?;
    missing_fingerprint
        .as_object_mut()
        .unwrap()
        .remove("repository_fingerprint");
    assert!(serde_json::from_value::<RedactionPlan>(missing_fingerprint).is_err());
    // The historical flat v1 wire shape remains readable and uses exactly its
    // original lossy active-worktree identity algorithm.
    let mut legacy_wire = serde_json::to_value(&plan)?;
    legacy_wire["format"] = serde_json::Value::String("cvc.redaction-plan/v1".into());
    legacy_wire["version"] = serde_json::Value::from(1);
    legacy_wire["repository_fingerprint"] = serde_json::Value::String(hex::encode(Sha256::digest(
        repo.path().to_string_lossy().as_bytes(),
    )));
    let legacy: RedactionPlan = serde_json::from_value(legacy_wire.clone())?;
    assert!(sync::verify_redaction_plan(
        &repo,
        &legacy,
        "refs/remotes/fixture/cvc/main"
    )?);
    assert!(
        sync::verify_redaction_plan(&sibling, &legacy, "refs/remotes/fixture/cvc/main").is_err()
    );
    assert!(sync::apply_hard_redaction_locally(&sibling, &legacy).is_err());
    for (key, value) in [
        (
            "unknown_identity_field",
            serde_json::Value::String("mixed".into()),
        ),
        ("version", serde_json::Value::from(3)),
    ] {
        let mut invalid = legacy_wire.clone();
        invalid[key] = value;
        assert!(serde_json::from_value::<RedactionPlan>(invalid).is_err());
    }
    assert!(sync::verify_redaction_plan(
        &repo,
        &plan,
        "refs/remotes/fixture/cvc/main"
    )?);
    assert!(sync::verify_redaction_plan(
        &sibling,
        &plan,
        "refs/remotes/fixture/cvc/main"
    )?);
    sync::apply_hard_redaction_locally(&sibling, &plan)?;
    let local = repo.find_reference("refs/cvc/main")?.peel_to_commit()?;
    assert_eq!(local.parent_count(), 0);
    let tree = local.tree()?;
    let target_file = format!("{}.json", target.id);
    assert!(tree.get_name(&target_file).is_none());
    let nodes = tree
        .get_name("nodes")
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    let target_shard = nodes
        .get_name(&target.id.as_str()[..2])
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    assert!(target_shard.get_name(&target_file).is_none());
    let unrelated_shard = nodes
        .get_name(&unrelated.id.as_str()[..2])
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    assert!(unrelated_shard
        .get_name(&format!("{}.json", unrelated.id))
        .is_some());
    let tombstones = tree
        .get_name("tombstones")
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    let tombstone_shard = tombstones
        .get_name(&target.id.as_str()[..2])
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    assert!(tombstone_shard.get_name(&target_file).is_some());
    assert_eq!(
        repo.find_reference("refs/cvc/redaction-plan")?.target(),
        Some(tip),
        "local apply must not mutate remote baseline"
    );
    repo.reference(
        "refs/remotes/fixture/cvc/main",
        local.id(),
        true,
        "stale plan",
    )?;
    assert!(!sync::verify_redaction_plan(
        &repo,
        &plan,
        "refs/remotes/fixture/cvc/main"
    )?);
    for mut tampered in [
        {
            let mut wire = serde_json::to_value(&plan)?;
            wire["repository_fingerprint"] = serde_json::Value::String("wrong".into());
            serde_json::from_value(wire)?
        },
        {
            let mut p = plan.clone();
            p.tombstone_oid = "0".repeat(40);
            p
        },
        {
            let mut p = plan.clone();
            p.unrelated_entries_retained += 1;
            p
        },
    ] {
        assert!(sync::apply_hard_redaction_locally(&repo, &tampered).is_err());
        // Avoid retaining the mutable binding in the assertion loop.
        tampered.warning.clear();
    }
    let temporary_ref = plan.temporary_ref.clone();
    drop(candidate);
    assert!(repo.find_reference(&temporary_ref).is_err());
    Ok(())
}

#[test]
fn remote_tombstones_only_suppress_stale_data_from_the_matching_source() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let source = CvcStore::open(temp_dir.path().join("source.db"))?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "remote-scope".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "stale remote node".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&interaction)?;
    source.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &CommitSha::new("b".repeat(40)),
        "generated",
        None,
    )?;
    project_fixture_to_ref(&repo, &source, "refs/cvc/stale-remote")?;
    let stale = repo
        .find_reference("refs/cvc/stale-remote")?
        .target()
        .expect("fixture ref has a direct target");

    // The later remote state contains the tombstone, while a stale clone retains
    // the old immutable node and link blobs.
    let tombstone = source.tombstone_remote(
        &interaction.id,
        TEST_DESTINATION,
        TombstoneReasonCode::Security,
        None,
    )?;
    project_fixture_to_ref(&repo, &source, "refs/cvc/redacted-remote")?;
    repo.reference(
        "refs/cvc/stale-remote",
        stale,
        true,
        "fixture: stale clone before redaction",
    )?;

    let target = CvcStore::open(temp_dir.path().join("target.db"))?;
    // Exercise the production remote-aware pull API: the received tombstone is
    // authority only for this exact source fingerprint.
    sync::pull_from_ref_for_remote(
        &repo,
        &target,
        "refs/cvc/redacted-remote",
        Some(TEST_DESTINATION),
    )?;
    sync::pull_from_ref_for_remote(
        &repo,
        &target,
        "refs/cvc/stale-remote",
        Some(TEST_DESTINATION),
    )?;
    assert!(target.get_interaction(&interaction.id)?.is_none());
    assert!(target.get_artifact_links(&interaction.id)?.is_empty());

    // A tombstone from this destination must not suppress an independently
    // authorized source, even where the immutable interaction ID is identical.
    sync::pull_from_ref_for_remote(
        &repo,
        &target,
        "refs/cvc/stale-remote",
        Some(&"f".repeat(64)),
    )?;
    assert!(target.get_interaction(&interaction.id)?.is_some());
    assert_eq!(target.get_artifact_links(&interaction.id)?.len(), 1);
    assert!(!target.is_tombstoned(&interaction.id)?);
    // The received record is remote-scoped, not a local/global tombstone.
    assert_eq!(tombstone.interaction_id, interaction.id);
    Ok(())
}

#[test]
fn matching_wire_tombstone_reconciles_pending_redaction_on_no_change_projection(
) -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let source = CvcStore::open(temp_dir.path().join("source.db"))?;
    let target_path = temp_dir.path().join("target.db");
    let target = CvcStore::open(&target_path)?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "reconcile-redaction".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "redaction transport was ambiguous".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    for store in [&source, &target] {
        store.create_interaction(&interaction)?;
        store.share_conversation_for_remote(
            &interaction.conversation_id,
            TEST_DESTINATION,
            FutureSharePolicy::Private,
        )?;
    }
    source.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &CommitSha::new("c".repeat(40)),
        "generated",
        None,
    )?;
    project_fixture_to_ref(&repo, &source, "refs/cvc/reconcile-stale")?;
    let wire_tombstone = source.tombstone_remote(
        &interaction.id,
        TEST_DESTINATION,
        TombstoneReasonCode::Security,
        None,
    )?;
    // Model a timeout after the local redaction write: it is pending until the
    // exact matching wire record proves that this destination received it.
    target.apply_tombstones(
        std::slice::from_ref(&wire_tombstone),
        "local",
        Some(TEST_DESTINATION),
    )?;
    let connection = Connection::open(&target_path)?;
    let pending_state: String = connection.query_row(
        "SELECT state FROM tombstones WHERE interaction_id=?1 AND scope_kind='remote' AND remote_fingerprint=?2",
        [&interaction.id.to_string(), TEST_DESTINATION],
        |row| row.get(0),
    )?;
    assert_eq!(pending_state, "pending");
    // The pending local record already suppresses a stale clone from this exact
    // destination; neither its node nor link may reappear before reconciliation.
    sync::pull_from_ref_for_remote(
        &repo,
        &target,
        "refs/cvc/reconcile-stale",
        Some(TEST_DESTINATION),
    )?;
    assert!(target.get_interaction(&interaction.id)?.is_none());
    assert!(target.get_artifact_links(&interaction.id)?.is_empty());
    project_fixture_to_ref(&repo, &source, "refs/cvc/reconcile-redaction")?;
    let wire_oid = repo
        .find_reference("refs/cvc/reconcile-redaction")?
        .target()
        .expect("fixture ref has a direct target");

    sync::pull_from_ref_for_remote(
        &repo,
        &target,
        "refs/cvc/reconcile-redaction",
        Some(TEST_DESTINATION),
    )?;
    let state: String = connection.query_row(
        "SELECT state FROM tombstones WHERE interaction_id=?1 AND scope_kind='remote' AND remote_fingerprint=?2",
        [&interaction.id.to_string(), TEST_DESTINATION],
        |row| row.get(0),
    )?;
    assert_eq!(state, "published");
    assert!(target.get_interaction(&interaction.id)?.is_none());

    repo.reference(
        "refs/remotes/fixture/cvc/main",
        wire_oid,
        true,
        "fixture: reconciliation baseline",
    )?;
    assert!(matches!(
        sync::push_projection_to_ref(
            &repo,
            &target,
            &pinned(&repo, "refs/remotes/fixture/cvc/main"),
            TEST_DESTINATION,
        )?,
        sync::ProjectionResult::NoChanges { .. }
    ));
    Ok(())
}

#[test]
fn pull_rejects_a_tombstones_root_with_the_wrong_object_type() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("store.db"))?;
    let tombstones_blob = repo.blob(b"not a tree")?;
    let mut root = repo.treebuilder(None)?;
    root.insert("tombstones", tombstones_blob, FileMode::Blob.into())?;
    let tree = repo.find_tree(root.write()?)?;
    let signature = Signature::now("CVC Test", "test@example.com")?;
    repo.commit(
        Some("refs/cvc/bad-tombstones-root"),
        &signature,
        &signature,
        "bad tombstones root",
        &tree,
        &[],
    )?;

    let error = sync::pull_from_ref(&repo, &store, "refs/cvc/bad-tombstones-root")
        .expect_err("reserved tombstones root must be a tree");
    assert!(error.to_string().contains("tombstones must be a tree"));
    assert!(store.get_all_interaction_ids()?.is_empty());
    Ok(())
}

fn commit_tombstone_tree(
    repo: &Repository,
    reference: &str,
    tombstones: &[Tombstone],
) -> anyhow::Result<()> {
    let mut shards: std::collections::HashMap<String, git2::TreeBuilder<'_>> =
        std::collections::HashMap::new();
    for tombstone in tombstones {
        let id = tombstone.interaction_id.to_string();
        let shard = id[..2].to_owned();
        let blob = repo.blob(&serde_json::to_vec(tombstone)?)?;
        let builder = match shards.entry(shard.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(repo.treebuilder(None)?)
            }
        };
        builder.insert(format!("{id}.json"), blob, FileMode::Blob.into())?;
    }
    let mut tombstone_root = repo.treebuilder(None)?;
    for (shard, builder) in shards {
        tombstone_root.insert(&shard, builder.write()?, FileMode::Tree.into())?;
    }
    let mut root = repo.treebuilder(None)?;
    root.insert("tombstones", tombstone_root.write()?, FileMode::Tree.into())?;
    let tree = repo.find_tree(root.write()?)?;
    let signature = Signature::now("CVC Test", "test@example.com")?;
    repo.commit(
        Some(reference),
        &signature,
        &signature,
        "tombstones",
        &tree,
        &[],
    )?;
    Ok(())
}

#[test]
fn pull_tombstone_budgets_reject_before_database_mutation() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let store = CvcStore::open(temp.path().join("store.db"))?;
    let planted = Interaction {
        id: InteractionId::new(),
        conversation_id: "planted".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "retain".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&planted)?;
    let tombstones: Vec<_> = (0..3)
        .map(|_| Tombstone::new(InteractionId::new(), TombstoneReasonCode::Security, None))
        .collect();
    commit_tombstone_tree(&repo, "refs/cvc/budget-count", &tombstones)?;
    let limits = sync::SyncReadLimits {
        max_tombstones: 2,
        max_tombstone_bytes: 16 * 1024,
        max_total_bytes: 1024 * 1024,
    };
    assert!(
        sync::pull_from_ref_with_limits(&repo, &store, "refs/cvc/budget-count", None, limits)
            .is_err()
    );
    assert!(store.get_interaction(&planted.id)?.is_some());

    commit_tombstone_tree(&repo, "refs/cvc/budget-bytes", &tombstones[..2])?;
    let limits = sync::SyncReadLimits {
        max_tombstones: 3,
        max_tombstone_bytes: 16 * 1024,
        max_total_bytes: 1,
    };
    assert!(
        sync::pull_from_ref_with_limits(&repo, &store, "refs/cvc/budget-bytes", None, limits)
            .is_err()
    );
    assert!(store.get_interaction(&planted.id)?.is_some());
    Ok(())
}

#[test]
fn pull_duplicate_node_and_tombstone_representations_are_atomic() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let store = CvcStore::open(temp.path().join("store.db"))?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "duplicate".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "same".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    let node = SyncNode {
        interaction: interaction.clone(),
        context_items: vec![],
        tool_executions: vec![],
        artifact_links: vec![],
    };
    let bytes = serde_json::to_vec(&node)?;
    let file = format!("{}.json", interaction.id);
    let blob = repo.blob(&bytes)?;
    let mut shard = repo.treebuilder(None)?;
    shard.insert(&file, blob, FileMode::Blob.into())?;
    let mut nodes = repo.treebuilder(None)?;
    nodes.insert(
        &interaction.id.as_str()[..2],
        shard.write()?,
        FileMode::Tree.into(),
    )?;
    let mut root = repo.treebuilder(None)?;
    root.insert(&file, blob, FileMode::Blob.into())?;
    root.insert("nodes", nodes.write()?, FileMode::Tree.into())?;
    let tree = repo.find_tree(root.write()?)?;
    let sig = Signature::now("CVC Test", "test@example.com")?;
    repo.commit(
        Some("refs/cvc/duplicates-ok"),
        &sig,
        &sig,
        "duplicate",
        &tree,
        &[],
    )?;
    sync::pull_from_ref(&repo, &store, "refs/cvc/duplicates-ok")?;
    assert_eq!(store.get_all_interaction_ids()?.len(), 1);

    let mut altered = node;
    altered.interaction.user_prompt = "different".into();
    let mut bad_shard = repo.treebuilder(None)?;
    bad_shard.insert(
        &file,
        repo.blob(&serde_json::to_vec(&altered)?)?,
        FileMode::Blob.into(),
    )?;
    let mut bad_nodes = repo.treebuilder(None)?;
    bad_nodes.insert(
        &interaction.id.as_str()[..2],
        bad_shard.write()?,
        FileMode::Tree.into(),
    )?;
    let mut bad_root = repo.treebuilder(None)?;
    bad_root.insert(&file, blob, FileMode::Blob.into())?;
    bad_root.insert("nodes", bad_nodes.write()?, FileMode::Tree.into())?;
    let bad_tree = repo.find_tree(bad_root.write()?)?;
    repo.commit(
        Some("refs/cvc/duplicates-bad"),
        &sig,
        &sig,
        "bad duplicate",
        &bad_tree,
        &[],
    )?;
    let empty = CvcStore::open(temp.path().join("empty.db"))?;
    assert!(sync::pull_from_ref(&repo, &empty, "refs/cvc/duplicates-bad").is_err());
    assert!(empty.get_all_interaction_ids()?.is_empty());
    // A valid Git tree cannot encode the same canonical shard/name twice. The
    // transport-neutral canonicalizer is the seam for pack/alternate readers.
    let tombstone = Tombstone::new(interaction.id.clone(), TombstoneReasonCode::Security, None);
    let mut conflicting = tombstone.clone();
    conflicting.reason_code = TombstoneReasonCode::Retention;
    assert!(sync::validate_tombstone_records(&[tombstone.clone(), tombstone]).is_ok());
    assert!(sync::validate_tombstone_records(&[conflicting.clone(), conflicting.clone()]).is_ok());
    let original = Tombstone::new(interaction.id, TombstoneReasonCode::Security, None);
    assert!(sync::validate_tombstone_records(&[original, conflicting]).is_err());
    Ok(())
}

#[test]
fn tombstone_deletion_reports_logical_cleanup_for_local_and_imported_records() -> anyhow::Result<()>
{
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let store = CvcStore::open(temp.path().join("store.db"))?;
    let local = Interaction {
        id: InteractionId::new(),
        conversation_id: "local-cleanup".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "delete".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&local)?;
    store.tombstone_local(&local.id, TombstoneReasonCode::Security, None)?;
    assert!(store.get_interaction(&local.id)?.is_none());
    let report = store.compact_after_deletion()?;
    assert_eq!(report.busy, 0);
    assert_eq!(report.log_frames, 0);
    assert_eq!(report.checkpointed_frames, 0);
    assert_eq!(report.wal_bytes, 0);

    let imported = Interaction {
        id: InteractionId::new(),
        conversation_id: "remote-cleanup".into(),
        ..local.clone()
    };
    store.create_interaction(&imported)?;
    let tombstone = Tombstone::new(imported.id.clone(), TombstoneReasonCode::Security, None);
    commit_tombstone_tree(
        &repo,
        "refs/cvc/import-cleanup",
        std::slice::from_ref(&tombstone),
    )?;
    sync::pull_from_ref_for_remote(
        &repo,
        &store,
        "refs/cvc/import-cleanup",
        Some(TEST_DESTINATION),
    )?;
    assert!(store.get_interaction(&imported.id)?.is_none());
    let report = store.compact_after_deletion()?;
    assert_eq!(
        (
            report.busy,
            report.log_frames,
            report.checkpointed_frames,
            report.wal_bytes
        ),
        (0, 0, 0, 0)
    );
    Ok(())
}

#[test]
fn projection_reports_no_changes_against_verified_baseline() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("source.db"))?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "no-change".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "only once".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&interaction)?;

    let first = sync::push_projection_to_ref(&repo, &store, &pinned(&repo, ""), TEST_DESTINATION)?;
    let sync::ProjectionResult::Candidate { oid, candidate, .. } = first else {
        anyhow::bail!("initial projection must create a candidate");
    };
    repo.reference(
        "refs/remotes/fixture/cvc/main",
        oid,
        true,
        "verified fixture baseline",
    )?;
    drop(candidate);

    assert!(matches!(
        sync::push_projection_to_ref(
            &repo,
            &store,
            &pinned(&repo, "refs/remotes/fixture/cvc/main"),
            TEST_DESTINATION,
        )?,
        sync::ProjectionResult::NoChanges { .. }
    ));
    Ok(())
}

#[test]
fn failed_transport_cleanup_removes_projection_candidate() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("source.db"))?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "cleanup".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "cleanup candidate".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&interaction)?;
    let projection =
        sync::push_projection_to_ref(&repo, &store, &pinned(&repo, ""), TEST_DESTINATION)?;
    let sync::ProjectionResult::Candidate { candidate, .. } = projection else {
        anyhow::bail!("fixture expected projection candidate");
    };
    let temp_ref = candidate.ref_name().to_owned();
    let destination = cvc_core::privacy::RemoteDestination {
        name: "unreachable".into(),
        effective_url: temp_dir.path().join("missing-remote").display().to_string(),
        fingerprint: "a".repeat(64),
        ref_name: "refs/cvc/main".into(),
    };
    assert!(sync::push_temp_ref(&repo, &destination, &temp_ref).is_err());
    drop(candidate);
    assert!(repo.find_reference(&temp_ref).is_err());
    Ok(())
}

#[test]
fn candidate_guard_cleans_up_on_early_error() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("source.db"))?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "cancelled".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "cancel before publish".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&interaction)?;
    let projection =
        sync::push_projection_to_ref(&repo, &store, &pinned(&repo, ""), TEST_DESTINATION)?;
    let sync::ProjectionResult::Candidate { candidate, .. } = projection else {
        anyhow::bail!("fixture expected projection candidate");
    };
    let temp_ref = candidate.ref_name().to_owned();
    let cancelled = (|| -> anyhow::Result<()> {
        let _candidate = candidate;
        anyhow::bail!("simulated TTY cancellation")
    })();
    assert!(cancelled.is_err());
    assert!(repo.find_reference(&temp_ref).is_err());
    Ok(())
}

#[test]
fn destination_operation_lock_serializes_same_destination() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let fingerprint = "b".repeat(64);
    let held = cvc_core::privacy::destination_operation_lock(&repo, &fingerprint)?;
    let path = temp_dir.path().to_owned();
    let fingerprint_for_thread = fingerprint.clone();
    let (acquired, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || -> anyhow::Result<()> {
        let repo = Repository::open(path)?;
        let _lock = cvc_core::privacy::destination_operation_lock(&repo, &fingerprint_for_thread)?;
        acquired.send(()).unwrap();
        Ok(())
    });
    assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
    drop(held);
    receiver.recv_timeout(Duration::from_secs(2))?;
    worker.join().expect("lock worker panicked")?;
    Ok(())
}

#[test]
fn push_upgrades_v2_format_without_downgrading_data() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let source = CvcStore::open(temp_dir.path().join("source.db"))?;
    source.create_conversation(&Conversation {
        id: "upgrade".into(),
        title: "upgrade".into(),
        created_at: Utc::now(),
    })?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "upgrade".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "preserve me".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&interaction)?;
    let ref_name = "refs/cvc/upgrade";
    project_fixture_to_ref(&repo, &source, ref_name)?;

    let previous = repo.find_reference(ref_name)?.peel_to_commit()?;
    let mut builder = repo.treebuilder(Some(&previous.tree()?))?;
    builder.insert("FORMAT", repo.blob(b"2")?, FileMode::Blob.into())?;
    builder.remove("events")?;
    builder.remove("ranges")?;
    let tree = repo.find_tree(builder.write()?)?;
    let signature = Signature::now("Test User", "test@example.com")?;
    repo.commit(
        Some(ref_name),
        &signature,
        &signature,
        "v2 marker",
        &tree,
        &[&previous],
    )?;

    let existing = CvcStore::open(temp_dir.path().join("existing.db"))?;
    sync::pull_from_ref(&repo, &existing, ref_name)?;
    project_fixture_to_ref(&repo, &source, ref_name)?;
    let upgraded_tree = repo.find_reference(ref_name)?.peel_to_commit()?.tree()?;
    let marker = repo.find_blob(upgraded_tree.get_name("FORMAT").unwrap().id())?;
    assert_eq!(marker.content(), b"5");

    let fresh = CvcStore::open(temp_dir.path().join("fresh.db"))?;
    sync::pull_from_ref(&repo, &fresh, ref_name)?;
    sync::pull_from_ref(&repo, &existing, ref_name)?;
    assert!(fresh.get_interaction(&interaction.id)?.is_some());
    assert!(existing.get_interaction(&interaction.id)?.is_some());
    Ok(())
}

#[test]
fn v3_pull_surfaces_conflicting_link_attribution() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let source = CvcStore::open(temp_dir.path().join("source.db"))?;
    source.create_conversation(&Conversation {
        id: "conflict".into(),
        title: "conflict".into(),
        created_at: Utc::now(),
    })?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "conflict".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "conflict".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&interaction)?;
    let ref_name = "refs/cvc/conflict";
    project_fixture_to_ref(&repo, &source, ref_name)?;
    let target = CvcStore::open(temp_dir.path().join("target.db"))?;
    sync::pull_from_ref(&repo, &target, ref_name)?;
    let sha = CommitSha::new("c".repeat(40));
    target.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &sha,
        "generated",
        Some("local@example.com"),
    )?;
    source.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &sha,
        "generated",
        Some("remote@example.com"),
    )?;
    project_fixture_to_ref(&repo, &source, ref_name)?;
    assert!(sync::pull_from_ref(&repo, &target, ref_name).is_err());
    Ok(())
}

#[test]
fn push_rejects_existing_immutable_link_record_mismatch() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let db_path = temp_dir.path().join("source.db");
    let source = CvcStore::open(&db_path)?;
    source.create_conversation(&Conversation {
        id: "push-conflict".into(),
        title: "push-conflict".into(),
        created_at: Utc::now(),
    })?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "push-conflict".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "push".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    source.create_interaction(&interaction)?;
    let sha = CommitSha::new("d".repeat(40));
    source.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &sha,
        "generated",
        Some("first@example.com"),
    )?;
    let ref_name = "refs/cvc/push-conflict";
    project_fixture_to_ref(&repo, &source, ref_name)?;
    // Exact replay is the immutable-record no-op case.
    project_fixture_to_ref(&repo, &source, ref_name)?;

    let raw = Connection::open(&db_path)?;
    raw.execute(
        "UPDATE artifact_links SET linked_by = 'second@example.com'",
        [],
    )?;
    drop(raw);
    assert!(project_fixture_to_ref(&repo, &source, ref_name).is_err());
    Ok(())
}

#[test]
fn pull_rejects_malformed_legacy_embedded_link() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let signature = Signature::now("Test User", "test@example.com")?;
    let node_id = InteractionId::new();
    let node = SyncNode {
        interaction: Interaction {
            id: node_id.clone(),
            conversation_id: "bad".into(),
            parent_id: None,
            timestamp: Utc::now(),
            author: Author::Human,
            user_prompt: "bad".into(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        },
        context_items: vec![],
        tool_executions: vec![],
        artifact_links: vec![ArtifactLink {
            interaction_id: InteractionId::new(),
            git_commit_hash: CommitSha::new("not-an-oid"),
            link_type: "verified".into(),
            linked_by: None,
        }],
    };
    let mut builder = repo.treebuilder(None)?;
    builder.insert(
        format!("{}.json", node_id),
        repo.blob(serde_json::to_vec(&node)?.as_slice())?,
        FileMode::Blob.into(),
    )?;
    let tree = repo.find_tree(builder.write()?)?;
    repo.commit(
        Some("refs/cvc/bad"),
        &signature,
        &signature,
        "bad",
        &tree,
        &[],
    )?;
    let store = CvcStore::open(temp_dir.path().join("target.db"))?;
    assert!(sync::pull_from_ref(&repo, &store, "refs/cvc/bad").is_err());
    assert!(store.get_all_interaction_ids()?.is_empty());
    Ok(())
}

#[test]
fn test_sync_push_pull() -> anyhow::Result<()> {
    // 1. Setup Repo and DB
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;

    // Create initial commit so we have a HEAD (optional but good practice)
    let signature = Signature::now("Test User", "test@example.com")?;
    let tree_oid = repo.index()?.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    repo.commit(Some("HEAD"), &signature, &signature, "Init", &tree, &[])?;

    let db_path = temp_dir.path().join("cvc.db");
    let store = CvcStore::open(&db_path)?;
    store.init()?;

    // 2. Insert Data
    let conv = Conversation {
        id: "conv-1".to_string(),
        title: "Sync Test".to_string(),
        created_at: Utc::now(),
    };
    store.create_conversation(&conv)?;

    let inter = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "To be synced".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&inter)?;

    // 3. Push to Ref
    let ref_name = "refs/cvc/test";
    project_fixture_to_ref(&repo, &store, ref_name)?;

    // Verify ref exists
    let _ref_obj = repo.find_reference(ref_name)?;

    // 4. Simulate Fresh DB (Pull)
    let db_path_2 = temp_dir.path().join("cvc_2.db");
    let store_2 = CvcStore::open(&db_path_2)?;
    store_2.init()?;

    sync::pull_from_ref(&repo, &store_2, ref_name)?;

    // 5. Verify Data in store_2
    let fetched_inter = store_2.get_interaction(&inter.id)?;
    assert!(fetched_inter.is_some());
    assert_eq!(fetched_inter.unwrap().user_prompt, "To be synced");

    Ok(())
}

#[test]
fn test_sync_creates_commits() -> anyhow::Result<()> {
    // 1. Setup Repo and DB
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;

    // Create initial commit
    let signature = Signature::now("Test User", "test@example.com")?;
    let tree_oid = repo.index()?.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    repo.commit(Some("HEAD"), &signature, &signature, "Init", &tree, &[])?;

    let db_path = temp_dir.path().join("cvc.db");
    let store = CvcStore::open(&db_path)?;
    store.init()?;

    // 2. Insert Data
    let conv = Conversation {
        id: "conv-1".to_string(),
        title: "Sync Test".to_string(),
        created_at: Utc::now(),
    };
    store.create_conversation(&conv)?;

    let inter = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "To be synced".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&inter)?;

    // 3. Push to Ref
    let ref_name = "refs/cvc/test";
    project_fixture_to_ref(&repo, &store, ref_name)?;

    // 4. Verify ref points to a Commit
    let reference = repo.find_reference(ref_name)?;
    let commit = reference.peel_to_commit();
    assert!(commit.is_ok(), "Ref should point to a commit");

    // 5. Push again (should add another commit or fast-forward if no changes)
    // Add another interaction to force a new commit
    let inter2 = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: Some(inter.id.clone()),
        timestamp: Utc::now(),
        author: Author::Agent,
        user_prompt: "Response".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&inter2)?;
    project_fixture_to_ref(&repo, &store, ref_name)?;

    let reference = repo.find_reference(ref_name)?;
    let head_commit = reference.peel_to_commit()?;
    let parents: Vec<_> = head_commit.parents().collect();
    assert_eq!(parents.len(), 1, "Should have one parent");

    Ok(())
}

#[test]
fn test_sync_idempotency() -> anyhow::Result<()> {
    // 1. Setup Repo and DB
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;

    // Create initial commit
    let signature = Signature::now("Test User", "test@example.com")?;
    let tree_oid = repo.index()?.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    repo.commit(Some("HEAD"), &signature, &signature, "Init", &tree, &[])?;

    let db_path = temp_dir.path().join("cvc.db");
    let store = CvcStore::open(&db_path)?;
    store.init()?;

    // 2. Insert Data
    let conv = Conversation {
        id: "conv-1".to_string(),
        title: "Sync Test".to_string(),
        created_at: Utc::now(),
    };
    store.create_conversation(&conv)?;
    let inter = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "To be synced".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&inter)?;

    let ref_name = "refs/cvc/test";

    // 3. First Push
    project_fixture_to_ref(&repo, &store, ref_name)?;
    let initial_commit_oid = repo.find_reference(ref_name)?.peel_to_commit()?.id();

    // 4. Second Push (No new data)
    project_fixture_to_ref(&repo, &store, ref_name)?;
    let second_commit_oid = repo.find_reference(ref_name)?.peel_to_commit()?.id();

    // 5. Verify OID hasn't changed
    assert_eq!(
        initial_commit_oid, second_commit_oid,
        "Should not create new commit if no changes"
    );

    // 6. Add new data and Push
    let inter2 = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: Some(inter.id.clone()),
        timestamp: Utc::now(),
        author: Author::Agent,
        user_prompt: "Response".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&inter2)?;
    project_fixture_to_ref(&repo, &store, ref_name)?;
    let third_commit_oid = repo.find_reference(ref_name)?.peel_to_commit()?.id();

    assert_ne!(
        second_commit_oid, third_commit_oid,
        "Should create new commit when data changes"
    );

    Ok(())
}

#[test]
fn test_sync_divergence_recovery() -> anyhow::Result<()> {
    // 1. Setup Remote Repo (Bare)
    let temp_dir = TempDir::new()?;
    let remote_path = temp_dir.path().join("remote");
    let _remote_repo = Repository::init_bare(&remote_path)?;

    // 2. Setup Local Repo A (User A)
    let local_a_path = temp_dir.path().join("local_a");
    let repo_a = Repository::clone(remote_path.to_str().expect("Path not UTF-8"), &local_a_path)?;

    // Config signature
    let mut config_a = repo_a.config()?;
    config_a.set_str("user.name", "User A")?;
    config_a.set_str("user.email", "a@example.com")?;

    let db_path_a = local_a_path.join(".git/cvc/index.db");
    let store_a = CvcStore::open(&db_path_a)?;
    store_a.init()?;

    // Create conversation for A
    let conv_a = Conversation {
        id: "conv-1".to_string(),
        title: "Conv A".to_string(),
        created_at: Utc::now(),
    };
    store_a.create_conversation(&conv_a)?;

    // 3. Setup Local Repo B (User B)
    let local_b_path = temp_dir.path().join("local_b");
    let repo_b = Repository::clone(remote_path.to_str().expect("Path not UTF-8"), &local_b_path)?;

    // Config signature
    let mut config_b = repo_b.config()?;
    config_b.set_str("user.name", "User B")?;
    config_b.set_str("user.email", "b@example.com")?;

    let db_path_b = local_b_path.join(".git/cvc/index.db");
    let store_b = CvcStore::open(&db_path_b)?;
    store_b.init()?;

    // Create conversation for B
    let conv_b = Conversation {
        id: "conv-2".to_string(),
        title: "Conv B".to_string(),
        created_at: Utc::now(),
    };
    store_b.create_conversation(&conv_b)?;

    // 4. User A creates thought and pushes
    let inter_a = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Thought A".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store_a.create_interaction(&inter_a)?;

    let ref_name = "refs/cvc/main";
    project_fixture_to_ref(&repo_a, &store_a, ref_name)?;

    // Push ref to remote
    let mut remote_callbacks = git2::RemoteCallbacks::new();
    remote_callbacks.credentials(|_, _, _| git2::Cred::default());
    let mut push_opts = git2::PushOptions::new();
    push_opts.remote_callbacks(remote_callbacks);

    let mut origin_a = repo_a.find_remote("origin")?;
    origin_a.push(
        &[format!("{}:{}", ref_name, ref_name)],
        Some(&mut push_opts),
    )?;

    // 5. User B creates thought (Divergence!)
    let inter_b = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-2".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Thought B".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store_b.create_interaction(&inter_b)?;

    // B pushes locally
    project_fixture_to_ref(&repo_b, &store_b, ref_name)?;

    // 6. User B tries to push and fails (Simulation)
    // We expect this to fail because remote has A's work, which is not in B's history.
    let mut origin_b = repo_b.find_remote("origin")?;

    let mut callbacks_b = git2::RemoteCallbacks::new();
    callbacks_b.credentials(|_, _, _| git2::Cred::default());
    let mut push_opts_b = git2::PushOptions::new();
    push_opts_b.remote_callbacks(callbacks_b);

    let result = origin_b.push(
        &[format!("{}:{}", ref_name, ref_name)],
        Some(&mut push_opts_b),
    );
    assert!(result.is_err(), "Push should fail due to non-fast-forward");

    // 7. Recovery Logic (Simulating what cvc pull will do)
    let mut origin_b = repo_b.find_remote("origin")?;
    let remote_tracking_ref = "refs/remotes/origin/cvc/main";

    // Fetch specifically the cvc ref
    origin_b.fetch(
        &[format!("{}:{}", ref_name, remote_tracking_ref)],
        None,
        None,
    )?;

    // Pull/Ingest from remote ref
    sync::pull_from_ref(&repo_b, &store_b, remote_tracking_ref)?;

    // Verify we have A's thought
    assert!(store_b.get_interaction(&inter_a.id)?.is_some());

    // Reset local ref to remote ref (The Fix)
    let remote_ref = repo_b.find_reference(remote_tracking_ref)?;
    let remote_oid = remote_ref.target().unwrap();
    repo_b.reference(ref_name, remote_oid, true, "Reset to remote")?;

    // Push local again (Should now be a merge/union on top of A)
    project_fixture_to_ref(&repo_b, &store_b, ref_name)?;

    // Push to remote (Should succeed)
    origin_b.push(
        &[format!("{}:{}", ref_name, ref_name)],
        Some(&mut push_opts),
    )?;

    // 8. Verify Remote has both via A
    origin_a.fetch(
        &[format!("{}:{}", ref_name, remote_tracking_ref)],
        None,
        None,
    )?;
    sync::pull_from_ref(&repo_a, &store_a, remote_tracking_ref)?;

    assert!(store_a.get_interaction(&inter_b.id)?.is_some());

    Ok(())
}

#[test]
fn test_fetch_and_pull_from_fresh_clone() -> anyhow::Result<()> {
    // Simulates the continuation-of-work scenario: work started on one machine
    // (repo_a) is pushed to a shared remote, then a second machine (repo_b, a fresh
    // clone with an empty CVC cache) catches up via `fetch_and_pull` alone -- the
    // same primitive the cvc-mcp `sync_history` tool calls.
    let temp_dir = TempDir::new()?;
    let remote_path = temp_dir.path().join("remote");
    let _remote_repo = Repository::init_bare(&remote_path)?;

    // Machine A: records a thought and pushes it to the shared remote.
    let local_a_path = temp_dir.path().join("local_a");
    let repo_a = Repository::clone(remote_path.to_str().expect("Path not UTF-8"), &local_a_path)?;
    let mut config_a = repo_a.config()?;
    config_a.set_str("user.name", "User A")?;
    config_a.set_str("user.email", "a@example.com")?;

    let store_a = CvcStore::open(local_a_path.join(".git/cvc/index.db"))?;
    store_a.init()?;
    store_a.create_conversation(&Conversation {
        id: "conv-a".to_string(),
        title: "Conv A".to_string(),
        created_at: Utc::now(),
    })?;
    let inter_a = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-a".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Thought from machine A".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store_a.create_interaction(&inter_a)?;
    project_fixture_to_ref(&repo_a, &store_a, "refs/cvc/main")?;

    let mut push_callbacks = git2::RemoteCallbacks::new();
    push_callbacks.credentials(|_, _, _| git2::Cred::default());
    let mut push_opts = git2::PushOptions::new();
    push_opts.remote_callbacks(push_callbacks);
    repo_a
        .find_remote("origin")?
        .push(&["refs/cvc/main:refs/cvc/main"], Some(&mut push_opts))?;

    // Machine B: a fresh clone that never ran `commit_thought` locally.
    let local_b_path = temp_dir.path().join("local_b");
    let repo_b = Repository::clone(remote_path.to_str().expect("Path not UTF-8"), &local_b_path)?;
    let store_b = CvcStore::open(local_b_path.join(".git/cvc/index.db"))?;
    store_b.init()?;

    assert!(
        store_b.get_interaction(&inter_a.id)?.is_none(),
        "sanity check: machine B shouldn't have A's thought yet"
    );

    // `git fetch` needs a resolvable remote URL; the bare repo is a local path here,
    // which the system `git` CLI (fetch_and_pull's first attempt) handles fine.
    let destination_b = cvc_core::privacy::remote_destination(&repo_b, "origin")?;
    let new_count = sync::fetch_and_pull_destination(&repo_b, &store_b, &destination_b)?;

    assert_eq!(
        new_count, 1,
        "should report exactly one newly ingested interaction"
    );
    let fetched = store_b.get_interaction(&inter_a.id)?;
    assert!(fetched.is_some(), "machine B should now have A's thought");
    assert_eq!(fetched.unwrap().user_prompt, "Thought from machine A");

    // The destination-scoped tracking ref is the only fetched baseline.
    let remote_ref = repo_b.find_reference("refs/remotes/origin/cvc/main")?;
    assert!(remote_ref.target().is_some());

    // Calling again with nothing new on the remote should be a no-op.
    let second_count = sync::fetch_and_pull_destination(&repo_b, &store_b, &destination_b)?;
    assert_eq!(second_count, 0, "should report zero on a repeat sync");

    Ok(())
}

#[test]
fn a_pulled_conversation_is_imported_and_not_shared_by_the_pull() -> anyhow::Result<()> {
    // Machine A projects one conversation to a shared remote; machine B, a
    // fresh clone, pulls it. What B holds is imported, and receiving content
    // grants B no sharing authority.
    let temp_dir = TempDir::new()?;
    let remote_path = temp_dir.path().join("remote");
    let _remote_repo = Repository::init_bare(&remote_path)?;
    let local_a_path = temp_dir.path().join("local_a");
    let repo_a = Repository::clone(remote_path.to_str().expect("Path not UTF-8"), &local_a_path)?;
    let mut config_a = repo_a.config()?;
    config_a.set_str("user.name", "User A")?;
    config_a.set_str("user.email", "a@example.com")?;
    let store_a = CvcStore::open(local_a_path.join(".git/cvc/index.db"))?;
    store_a.init()?;
    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-a".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Thought from machine A".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store_a.create_interaction(&interaction)?;
    project_fixture_to_ref(&repo_a, &store_a, "refs/cvc/main")?;
    let mut push_callbacks = git2::RemoteCallbacks::new();
    push_callbacks.credentials(|_, _, _| git2::Cred::default());
    let mut push_opts = git2::PushOptions::new();
    push_opts.remote_callbacks(push_callbacks);
    repo_a
        .find_remote("origin")?
        .push(&["refs/cvc/main:refs/cvc/main"], Some(&mut push_opts))?;

    let local_b_path = temp_dir.path().join("local_b");
    let repo_b = Repository::clone(remote_path.to_str().expect("Path not UTF-8"), &local_b_path)?;
    let store_b = CvcStore::open(local_b_path.join(".git/cvc/index.db"))?;
    let destination = cvc_core::privacy::remote_destination(&repo_b, "origin")?;
    assert_eq!(
        sync::fetch_and_pull_destination(&repo_b, &store_b, &destination)?,
        1
    );

    let summaries = store_b.list_conversation_summaries(Some(&destination.fingerprint), 10)?;
    assert_eq!(summaries.len(), 1);
    let imported = &summaries[0];
    assert!(
        imported.id.starts_with("remote:") && imported.id.ends_with(":conv-a"),
        "{}",
        imported.id
    );
    assert!(imported.imported, "{imported:?}");
    assert_eq!(imported.thoughts, 1);
    // The pull recorded the interaction as published at its source, but did
    // not share it from this clone.
    assert!(!imported.shared);
    assert_eq!(
        (
            imported.shared_count,
            imported.published,
            imported.unconfirmed
        ),
        (0, 0, 0)
    );
    assert_eq!(imported.share_future, None);
    let counts = store_b.destination_publication_counts(&destination.fingerprint)?;
    assert_eq!(
        (counts.shared, counts.published, counts.unconfirmed),
        (0, 0, 0)
    );

    let details = store_b
        .conversation_details(&imported.id, Some(&destination.fingerprint))?
        .expect("the imported conversation exists");
    assert_eq!(&details.summary, imported);
    assert_eq!(details.snapshot_count, 1);
    assert_eq!((details.snapshot_shared, details.snapshot_new), (0, 1));
    // The publication the pull recorded is what the unshare rule looks at.
    let check = store_b.can_unshare(&imported.id, &destination.fingerprint)?;
    assert_eq!((check.published, check.pending, check.unknown), (1, 0, 0));

    // The local capture on machine A is not imported.
    let local = store_a.list_conversation_summaries(Some(&destination.fingerprint), 10)?;
    assert_eq!(local.len(), 1);
    assert!(!local[0].imported);
    Ok(())
}

#[test]
fn test_sync_v2_round_trip() -> anyhow::Result<()> {
    // Round-trip requirement for the sharded v2 layout: push it, pull into a fresh clone, DBs equal.
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let signature = Signature::now("Test User", "test@example.com")?;
    let tree_oid = repo.index()?.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    let commit_oid = repo.commit(Some("HEAD"), &signature, &signature, "Init", &tree, &[])?;
    let commit_sha = CommitSha::new(commit_oid.to_string());

    let store = CvcStore::open(temp_dir.path().join("cvc.db"))?;
    store.init()?;
    store.create_conversation(&Conversation {
        id: "conv-1".to_string(),
        title: "Round Trip".to_string(),
        created_at: Utc::now(),
    })?;

    // A linked interaction (will end up indexed under by-commit/) ...
    let linked = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Linked thought".to_string(),
        model_name: None,
        model_cot: Some("reasoning".to_string()),
        model_response: Some("response".to_string()),
        source_request_id: None,
    };
    store.capture_mcp(McpCapture::new(
        Conversation {
            id: linked.conversation_id.clone(),
            title: "fixture".into(),
            created_at: linked.timestamp,
        },
        linked.clone(),
        vec![ContextItem {
            id: None,
            interaction_id: linked.id.clone(),
            file_path: "src/lib.rs".to_string(),
            git_blob_sha: Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string()),
            dirty_patch: None,
            start_line: None,
            end_line: None,
        }],
        Vec::new(),
        PreparedPolicy::built_ins_only(),
        "0".repeat(64),
    ))?;
    store.link_interaction_with_metadata(
        &linked.id,
        &commit_sha,
        "generated",
        Some("author@example.com"),
    )?;

    // ... and a floating one (nodes/ only, no by-commit/ entry).
    let floating = Interaction {
        id: InteractionId::new(),
        conversation_id: "conv-1".to_string(),
        parent_id: Some(linked.id.clone()),
        timestamp: Utc::now(),
        author: Author::Agent,
        user_prompt: "Floating thought".to_string(),
        model_name: Some("agent".to_string()),
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&floating)?;

    let ref_name = "refs/cvc/main";
    project_fixture_to_ref(&repo, &store, ref_name)?;

    // Verify the v2 layout actually landed: FORMAT marker, sharded nodes/, by-commit/ index.
    let pushed_tree = repo.find_reference(ref_name)?.peel_to_commit()?.tree()?;
    let format_entry = pushed_tree
        .get_name("FORMAT")
        .expect("FORMAT marker should be written");
    let format_blob = repo.find_blob(format_entry.id())?;
    assert_eq!(format_blob.content(), b"5");

    let nodes_tree = pushed_tree
        .get_name("nodes")
        .expect("nodes/ should exist")
        .to_object(&repo)?
        .peel_to_tree()?;
    let linked_prefix = &linked.id.as_str()[0..2];
    let shard_tree = nodes_tree
        .get_name(linked_prefix)
        .unwrap_or_else(|| panic!("nodes/{} shard should exist", linked_prefix))
        .to_object(&repo)?
        .peel_to_tree()?;
    assert!(shard_tree
        .get_name(&format!("{}.json", linked.id))
        .is_some());

    let by_commit_tree = pushed_tree
        .get_name("by-commit")
        .expect("by-commit/ should exist")
        .to_object(&repo)?
        .peel_to_tree()?;
    let commit_index = by_commit_tree
        .get_name(commit_sha.as_str())
        .expect("by-commit/<sha> should exist for the linked commit")
        .to_object(&repo)?
        .peel_to_tree()?;
    assert!(commit_index.get_name(&linked.id.to_string()).is_some());
    // The floating interaction was never linked, so it must not appear in the index.
    assert!(commit_index.get_name(&floating.id.to_string()).is_none());

    // Pull into a fresh clone/DB and compare.
    let store_2 = CvcStore::open(temp_dir.path().join("cvc_2.db"))?;
    store_2.init()?;
    sync::pull_from_ref(&repo, &store_2, ref_name)?;

    for original in [&linked, &floating] {
        let fetched = store_2
            .get_interaction(&original.id)?
            .unwrap_or_else(|| panic!("interaction {} missing after pull", original.id));
        assert_eq!(fetched.user_prompt, original.user_prompt);
        assert_eq!(fetched.conversation_id, original.conversation_id);
        assert_eq!(fetched.parent_id, original.parent_id);
        assert_eq!(fetched.author, original.author);
        assert_eq!(fetched.model_cot, original.model_cot);
        assert_eq!(fetched.model_response, original.model_response);
    }

    let fetched_links = store_2.get_artifact_links(&linked.id)?;
    assert_eq!(fetched_links.len(), 1);
    assert_eq!(fetched_links[0].git_commit_hash, commit_sha);
    assert_eq!(
        fetched_links[0].linked_by.as_deref(),
        Some("author@example.com")
    );

    let fetched_context = store_2.get_context_items(&linked.id)?;
    assert_eq!(fetched_context.len(), 1);
    assert_eq!(fetched_context[0].file_path, "src/lib.rs");

    assert!(store_2.get_artifact_links(&floating.id)?.is_empty());

    Ok(())
}

#[test]
fn test_pull_from_ref_reads_legacy_v1_layout() -> anyhow::Result<()> {
    // Repos last synced with the legacy v1 format have a flat `<id>.json` tree with no nodes/,
    // by-commit/, or FORMAT entries at all. pull_from_ref must still read them.
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let signature = Signature::now("Test User", "test@example.com")?;

    let id = InteractionId::new();
    let node = SyncNode {
        interaction: Interaction {
            id: id.clone(),
            conversation_id: "conv-legacy".to_string(),
            parent_id: None,
            timestamp: Utc::now(),
            author: Author::Human,
            user_prompt: "Legacy thought".to_string(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        },
        context_items: vec![],
        tool_executions: vec![],
        artifact_links: vec![],
    };

    let mut tree_builder = repo.treebuilder(None)?;
    let json = serde_json::to_string_pretty(&node)?;
    let blob_oid = repo.blob(json.as_bytes())?;
    tree_builder.insert(format!("{}.json", id), blob_oid, FileMode::Blob.into())?;
    let tree_oid = tree_builder.write()?;
    let tree = repo.find_tree(tree_oid)?;
    repo.commit(
        Some("refs/cvc/main"),
        &signature,
        &signature,
        "Legacy v1 sync",
        &tree,
        &[],
    )?;

    let store = CvcStore::open(temp_dir.path().join("cvc.db"))?;
    store.init()?;
    sync::pull_from_ref(&repo, &store, "refs/cvc/main")?;

    let fetched = store.get_interaction(&id)?;
    assert!(
        fetched.is_some(),
        "legacy v1 interaction should be ingested"
    );
    assert_eq!(fetched.unwrap().user_prompt, "Legacy thought");

    // A push against this legacy tree should leave the old entry alone and start
    // writing new interactions under the v2 layout -- no disruptive migration.
    // (conv-legacy already exists: pull_from_ref auto-created it while ingesting the
    // legacy interaction above.)
    let new_id = InteractionId::new();
    store.create_interaction(&Interaction {
        id: new_id.clone(),
        conversation_id: "conv-legacy".to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Agent,
        user_prompt: "Post-upgrade thought".to_string(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    })?;
    project_fixture_to_ref(&repo, &store, "refs/cvc/main")?;

    let updated_tree = repo
        .find_reference("refs/cvc/main")?
        .peel_to_commit()?
        .tree()?;
    // The old flat entry is untouched (immutable blobs rule).
    assert!(updated_tree.get_name(&format!("{}.json", id)).is_some());
    // The new interaction lands in the sharded layout instead.
    let prefix = &new_id.as_str()[0..2];
    let nodes_tree = updated_tree
        .get_name("nodes")
        .expect("nodes/ should now exist")
        .to_object(&repo)?
        .peel_to_tree()?;
    let shard = nodes_tree
        .get_name(prefix)
        .expect("shard should exist")
        .to_object(&repo)?
        .peel_to_tree()?;
    assert!(shard.get_name(&format!("{}.json", new_id)).is_some());

    // And a pull against this mixed tree picks up both.
    let store_2 = CvcStore::open(temp_dir.path().join("cvc_2.db"))?;
    store_2.init()?;
    sync::pull_from_ref(&repo, &store_2, "refs/cvc/main")?;
    assert!(store_2.get_interaction(&id)?.is_some());
    assert!(store_2.get_interaction(&new_id)?.is_some());

    Ok(())
}

#[test]
fn test_sync_cycle_detection() -> anyhow::Result<()> {
    // 1. Setup Repo and DB
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;

    // Create initial commit
    let signature = Signature::now("Test User", "test@example.com")?;
    let tree_oid = repo.index()?.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    repo.commit(Some("HEAD"), &signature, &signature, "Init", &tree, &[])?;

    let db_path = temp_dir.path().join("cvc.db");
    let store = CvcStore::open(&db_path)?;
    store.init()?;

    // 2. Insert Data manually to force a cycle (A -> B -> A)
    let id_a = InteractionId::new();
    let id_b = InteractionId::new();

    // We also need conversations to avoid FK error on conversation_id if we didn't create them.
    // The sync logic creates conversation placeholders if missing, so that's fine.

    let node_a = SyncNode {
        interaction: Interaction {
            id: id_a.clone(),
            conversation_id: "conv-cycle".to_string(),
            parent_id: Some(id_b.clone()), // A depends on B
            timestamp: Utc::now(),
            author: Author::Human,
            user_prompt: "A".to_string(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        },
        context_items: vec![],
        tool_executions: vec![],
        artifact_links: vec![],
    };

    let node_b = SyncNode {
        interaction: Interaction {
            id: id_b.clone(),
            conversation_id: "conv-cycle".to_string(),
            parent_id: Some(id_a.clone()), // B depends on A
            timestamp: Utc::now(),
            author: Author::Human,
            user_prompt: "B".to_string(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        },
        context_items: vec![],
        tool_executions: vec![],
        artifact_links: vec![],
    };

    // Write these to the git ref manually
    let mut tree_builder = repo.treebuilder(None)?;

    let json_a = serde_json::to_string_pretty(&node_a)?;
    let oid_a = repo.blob(json_a.as_bytes())?;
    tree_builder.insert(
        format!("{}.json", id_a).as_str(),
        oid_a,
        FileMode::Blob.into(),
    )?;

    let json_b = serde_json::to_string_pretty(&node_b)?;
    let oid_b = repo.blob(json_b.as_bytes())?;
    tree_builder.insert(
        format!("{}.json", id_b).as_str(),
        oid_b,
        FileMode::Blob.into(),
    )?;

    let tree_oid = tree_builder.write()?;
    let new_tree = repo.find_tree(tree_oid)?;

    repo.commit(
        Some("refs/cvc/cycle"),
        &signature,
        &signature,
        "Cyclic Sync",
        &new_tree,
        &[],
    )?;

    // 3. Try to Pull
    // This should FAIL with Cycle detected error.
    let result = sync::pull_from_ref(&repo, &store, "refs/cvc/cycle");
    assert!(result.is_err(), "Pull should fail on cycle");

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Cycle detected"),
        "Error should be about cycle, got: {}",
        err
    );

    // 4. Verify NOTHING was ingested (Atomic failure would be nice, but here we fail before insert loop starts sort of?)
    // Actually, `pull_from_ref` sorts ALL nodes before inserting ANY.
    // So if sort fails, NOTHING is inserted.

    let fetched_a = store.get_interaction(&id_a)?;
    assert!(
        fetched_a.is_none(),
        "Node A should NOT be ingested if cycle detected"
    );

    Ok(())
}

// --- Publication summary (#106): what a candidate newly writes -----------------

const SUMMARY_BASELINE: &str = "refs/remotes/fixture/cvc/main";

fn summary_interaction(conversation: &str, prompt: &str) -> Interaction {
    Interaction {
        id: InteractionId::new(),
        conversation_id: conversation.into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: prompt.into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    }
}

/// Projects against the fixture baseline (if one was accepted before), accepts a
/// candidate as the new verified baseline, and returns its publication summary.
/// `None` means the projection equals the baseline.
fn project_and_accept(
    repo: &Repository,
    store: &CvcStore,
) -> anyhow::Result<Option<sync::ProjectionPublication>> {
    let baseline = if repo.find_reference(SUMMARY_BASELINE).is_ok() {
        SUMMARY_BASELINE
    } else {
        ""
    };
    match sync::push_projection_to_ref(repo, store, &pinned(repo, baseline), TEST_DESTINATION)? {
        sync::ProjectionResult::NoChanges { .. } => Ok(None),
        sync::ProjectionResult::Candidate {
            oid,
            candidate,
            publication,
        } => {
            repo.reference(SUMMARY_BASELINE, oid, true, "fixture: accepted candidate")?;
            drop(candidate);
            Ok(Some(publication))
        }
    }
}

fn sorted_ids<'a>(ids: impl IntoIterator<Item = &'a InteractionId>) -> Vec<String> {
    let mut ids: Vec<String> = ids.into_iter().map(|id| id.to_string()).collect();
    ids.sort();
    ids
}

#[test]
fn publication_summary_counts_only_interactions_new_to_the_baseline() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("source.db"))?;

    let published: Vec<Interaction> = (0..3)
        .map(|n| summary_interaction(&format!("published-{n}"), "already there"))
        .collect();
    for interaction in &published {
        store.create_interaction(interaction)?;
    }
    let first = project_and_accept(&repo, &store)?.expect("first projection is a candidate");
    assert_eq!(
        sorted_ids(&first.new_ids),
        sorted_ids(published.iter().map(|i| &i.id))
    );
    assert_eq!(first.shared_total, 3);
    assert_eq!(
        (
            first.new_link_records,
            first.new_tombstones,
            first.new_derivation_records
        ),
        (0, 0, 0)
    );

    let fresh: Vec<Interaction> = (0..2)
        .map(|n| summary_interaction(&format!("fresh-{n}"), "newly shared"))
        .collect();
    for interaction in &fresh {
        store.create_interaction(interaction)?;
    }
    let second = project_and_accept(&repo, &store)?.expect("new shares need a candidate");
    assert_eq!(
        sorted_ids(&second.new_ids),
        sorted_ids(fresh.iter().map(|i| &i.id)),
        "only the newly shared interactions are new"
    );
    assert_eq!(second.shared_total, 5, "K published + M new");
    assert!(!second.is_structural_only());

    // Identical projection: nothing to write, so no candidate at all.
    match sync::push_projection_to_ref(
        &repo,
        &store,
        &pinned(&repo, SUMMARY_BASELINE),
        TEST_DESTINATION,
    )? {
        sync::ProjectionResult::NoChanges { shared_total } => assert_eq!(shared_total, 5),
        sync::ProjectionResult::Candidate { .. } => anyhow::bail!("identical projection"),
    }
    assert!(project_and_accept(&repo, &store)?.is_none());
    Ok(())
}

#[test]
fn publication_summary_reports_a_new_link_on_an_already_published_interaction() -> anyhow::Result<()>
{
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("source.db"))?;
    let interaction = summary_interaction("linked-later", "published floating");
    store.create_interaction(&interaction)?;
    let first = project_and_accept(&repo, &store)?.expect("first candidate");
    assert_eq!(first.new_ids.len(), 1);

    store.link_automatic_interactions(
        std::slice::from_ref(&interaction.id),
        &CommitSha::new("c".repeat(40)),
        "generated",
        None,
    )?;
    let linked = project_and_accept(&repo, &store)?.expect("a new link record is a candidate");
    assert!(linked.new_ids.is_empty(), "the node is already published");
    assert_eq!(linked.new_link_records, 1);
    assert_eq!(linked.new_tombstones, 0);
    assert_eq!(linked.shared_total, 1);
    assert!(!linked.is_structural_only());
    assert!(project_and_accept(&repo, &store)?.is_none());
    Ok(())
}

#[test]
fn publication_summary_counts_a_new_derivation_event_without_a_new_node() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let db_path = temp_dir.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    let interaction = summary_interaction("derived", "published first");
    store.create_interaction(&interaction)?;
    project_and_accept(&repo, &store)?.expect("first candidate");

    let event = derivation(&interaction.id, 'a', Vec::new());
    insert_event_fixture(&db_path, &event)?;
    trust_and_authorize_event(&db_path, &event)?;
    let summary = project_and_accept(&repo, &store)?.expect("a new event is a candidate");
    assert!(summary.new_ids.is_empty());
    assert_eq!(summary.new_derivation_records, 1);
    assert_eq!(summary.shared_total, 1);
    Ok(())
}

#[test]
fn publication_summary_counts_a_pending_tombstone_but_not_the_suppressed_node() -> anyhow::Result<()>
{
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("source.db"))?;
    let kept = summary_interaction("kept", "stays published");
    let redacted = summary_interaction("redacted", "to be suppressed");
    store.create_interaction(&kept)?;
    store.create_interaction(&redacted)?;
    let first = project_and_accept(&repo, &store)?.expect("first candidate");
    assert_eq!(first.shared_total, 2);

    store.tombstone_remote(
        &redacted.id,
        TEST_DESTINATION,
        TombstoneReasonCode::UserRequested,
        None,
    )?;
    let after = project_and_accept(&repo, &store)?.expect("a tombstone is a candidate");
    assert_eq!(after.new_tombstones, 1);
    assert!(
        after.new_ids.is_empty(),
        "the tombstoned id is never published as a node: {:?}",
        after.new_ids
    );
    assert!(!after.new_ids.iter().any(|id| id == &redacted.id));
    assert_eq!(
        after.shared_total, 1,
        "the suppressed interaction is excluded"
    );
    assert_eq!(
        (after.removed_interactions, after.removed_records),
        (1, 0),
        "the baseline's node for the tombstoned interaction leaves the tip tree"
    );
    assert!(!after.is_structural_only());

    // The suppressed node is really gone from the accepted tree.
    let tree = repo
        .find_reference(SUMMARY_BASELINE)?
        .peel_to_commit()?
        .tree()?;
    let nodes = tree
        .get_name("nodes")
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    let still_there = nodes.iter().any(|shard| {
        shard
            .to_object(&repo)
            .and_then(|o| o.peel_to_tree())
            .map(|t| t.get_name(&format!("{}.json", redacted.id)).is_some())
            .unwrap_or(true)
    });
    assert!(!still_there);
    assert!(project_and_accept(&repo, &store)?.is_none());
    Ok(())
}

#[test]
fn publication_summary_does_not_count_legacy_flat_baseline_nodes() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let signature = Signature::now("Test User", "test@example.com")?;

    let legacy_id = InteractionId::new();
    let node = SyncNode {
        interaction: Interaction {
            id: legacy_id.clone(),
            ..summary_interaction("conv-legacy", "Legacy thought")
        },
        context_items: vec![],
        tool_executions: vec![],
        artifact_links: vec![],
    };
    let mut flat = repo.treebuilder(None)?;
    flat.insert(
        format!("{legacy_id}.json"),
        repo.blob(serde_json::to_string_pretty(&node)?.as_bytes())?,
        FileMode::Blob.into(),
    )?;
    let legacy_tree = repo.find_tree(flat.write()?)?;
    let legacy_commit = repo.commit(
        Some("refs/cvc/legacy-source"),
        &signature,
        &signature,
        "Legacy v1 sync",
        &legacy_tree,
        &[],
    )?;
    repo.reference(SUMMARY_BASELINE, legacy_commit, true, "legacy baseline")?;

    let store = CvcStore::open(temp_dir.path().join("cvc.db"))?;
    sync::pull_from_ref(&repo, &store, "refs/cvc/legacy-source")?;
    // Sharing the (already imported) conversation puts the legacy node in this
    // destination's projection next to the genuinely new one.
    let fresh = summary_interaction("conv-legacy", "Post-upgrade thought");
    store.create_interaction(&fresh)?;

    let summary = project_and_accept(&repo, &store)?.expect("the fresh node needs a candidate");
    assert_eq!(sorted_ids(&summary.new_ids), vec![fresh.id.to_string()]);
    assert_eq!(
        summary.shared_total, 2,
        "the legacy node is projected and present, but not new"
    );
    Ok(())
}

#[test]
fn publication_marks_preserve_the_timestamp_of_already_published_rows() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let source = CvcStore::open(temp_dir.path().join("source.db"))?;
    let target_path = temp_dir.path().join("target.db");
    let target = CvcStore::open(&target_path)?;
    let interaction = summary_interaction("stamped", "published once");
    source.create_interaction(&interaction)?;
    project_fixture_to_ref(&repo, &source, "refs/cvc/stamped")?;

    let raw = Connection::open(&target_path)?;
    let row = |raw: &Connection| -> rusqlite::Result<(String, i64)> {
        raw.query_row(
            "SELECT state, updated_at FROM publications WHERE interaction_id=?1 AND remote_fingerprint=?2",
            rusqlite::params![interaction.id.to_string(), TEST_DESTINATION],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    };
    const SENTINEL: i64 = 1_000;

    // Fetch/import records the node as published for this exact destination.
    sync::pull_from_ref_for_remote(&repo, &target, "refs/cvc/stamped", Some(TEST_DESTINATION))?;
    let (state, stamped) = row(&raw)?;
    assert_eq!(state, "published");
    assert!(stamped > SENTINEL);

    // Observing the same node at a later fetch must not re-stamp the row.
    raw.execute("UPDATE publications SET updated_at=?1", [SENTINEL])?;
    sync::pull_from_ref_for_remote(&repo, &target, "refs/cvc/stamped", Some(TEST_DESTINATION))?;
    assert_eq!(row(&raw)?, ("published".to_owned(), SENTINEL));

    // Re-asserting `published` through the publication mark is equally inert...
    target.mark_publication(
        std::slice::from_ref(&interaction.id),
        TEST_DESTINATION,
        PublicationState::Published,
    )?;
    assert_eq!(row(&raw)?, ("published".to_owned(), SENTINEL));

    // ...while a row that was not published is stamped when it becomes published:
    // by a later fetch (the row is forced to `unknown`, as a failed first send
    // would have left it) and by the publication mark itself (forced to `pending`).
    // Demotion is covered by `published_rows_are_never_demoted_by_pending_or_unknown_marks`.
    for not_published in ["unknown", "pending"] {
        raw.execute(
            "UPDATE publications SET state=?1, updated_at=?2",
            rusqlite::params![not_published, SENTINEL],
        )?;
        if not_published == "unknown" {
            sync::pull_from_ref_for_remote(
                &repo,
                &target,
                "refs/cvc/stamped",
                Some(TEST_DESTINATION),
            )?;
        } else {
            target.mark_publication(
                std::slice::from_ref(&interaction.id),
                TEST_DESTINATION,
                PublicationState::Published,
            )?;
        }
        let (state, stamped_again) = row(&raw)?;
        assert_eq!(state, "published", "from {not_published}");
        assert!(
            stamped_again > SENTINEL,
            "a row that was {not_published} is stamped when it becomes published"
        );
    }
    Ok(())
}

#[test]
fn publication_summary_names_a_stale_node_removed_next_to_its_tombstone() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("source.db"))?;
    let kept = summary_interaction("kept", "stays");
    let redacted = summary_interaction("redacted", "suppressed");
    store.create_interaction(&kept)?;
    store.create_interaction(&redacted)?;
    project_and_accept(&repo, &store)?.expect("first candidate");
    let with_node = repo.find_reference(SUMMARY_BASELINE)?.peel_to_commit()?;

    store.tombstone_remote(
        &redacted.id,
        TEST_DESTINATION,
        TombstoneReasonCode::UserRequested,
        None,
    )?;
    project_and_accept(&repo, &store)?.expect("tombstone candidate");
    let tombstoned = repo.find_reference(SUMMARY_BASELINE)?.peel_to_commit()?;

    // An older client that ignores tombstones/ pushes the suppressed node back
    // next to its tombstone: take the tombstoned tree and re-insert that blob.
    let file = format!("{}.json", redacted.id);
    let prefix = &redacted.id.to_string()[0..2];
    let old_nodes = with_node
        .tree()?
        .get_name("nodes")
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    let old_shard = old_nodes
        .get_name(prefix)
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    let blob = old_shard.get_name(&file).unwrap().id();
    let tree = tombstoned.tree()?;
    let nodes = tree
        .get_name("nodes")
        .unwrap()
        .to_object(&repo)?
        .peel_to_tree()?;
    let mut shard_builder = match nodes.get_name(prefix) {
        Some(entry) => repo.treebuilder(Some(&entry.to_object(&repo)?.peel_to_tree()?))?,
        None => repo.treebuilder(None)?,
    };
    shard_builder.insert(&file, blob, FileMode::Blob.into())?;
    let mut nodes_builder = repo.treebuilder(Some(&nodes))?;
    nodes_builder.insert(prefix, shard_builder.write()?, FileMode::Tree.into())?;
    let mut root_builder = repo.treebuilder(Some(&tree))?;
    root_builder.insert("nodes", nodes_builder.write()?, FileMode::Tree.into())?;
    let signature = Signature::now("Old Client", "old@example.com")?;
    let resurrected = repo.commit(
        None,
        &signature,
        &signature,
        "old client re-adds a node",
        &repo.find_tree(root_builder.write()?)?,
        &[&tombstoned],
    )?;
    repo.reference(SUMMARY_BASELINE, resurrected, true, "stale baseline")?;

    let cleanup = project_and_accept(&repo, &store)?.expect("pruning the stale node is a change");
    assert!(cleanup.new_ids.is_empty());
    assert_eq!(cleanup.new_tombstones, 0, "the tombstone is already there");
    assert_eq!(cleanup.removed_interactions, 1);
    assert!(
        !cleanup.is_structural_only(),
        "a content removal is not a format or index update"
    );
    assert_eq!(cleanup.shared_total, 1);
    Ok(())
}

#[test]
fn publication_summary_is_structural_only_for_a_format_upgrade() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("source.db"))?;
    store.create_interaction(&summary_interaction("upgrade", "preserve me"))?;
    project_and_accept(&repo, &store)?.expect("first candidate");

    // Re-mark the accepted baseline as the older v2 layout (as in
    // `push_upgrades_v2_format_without_downgrading_data`).
    let previous = repo.find_reference(SUMMARY_BASELINE)?.peel_to_commit()?;
    let mut builder = repo.treebuilder(Some(&previous.tree()?))?;
    builder.insert("FORMAT", repo.blob(b"2")?, FileMode::Blob.into())?;
    builder.remove("events")?;
    builder.remove("ranges")?;
    let signature = Signature::now("Test User", "test@example.com")?;
    let v2 = repo.commit(
        None,
        &signature,
        &signature,
        "v2 marker",
        &repo.find_tree(builder.write()?)?,
        &[&previous],
    )?;
    repo.reference(SUMMARY_BASELINE, v2, true, "v2 baseline")?;

    let upgrade = project_and_accept(&repo, &store)?.expect("the format upgrade is a candidate");
    assert!(upgrade.is_structural_only(), "{upgrade:?}");
    assert_eq!(upgrade.shared_total, 1);
    Ok(())
}

// --- Tombstoned pushes keep what other clones published (#125) -----------------

/// A well-formed range body with its canonical ID. `seed` is a lowercase hex
/// digit; it varies every hash-shaped field, so each seed has its own ID.
fn range_fixture(seed: char) -> RangeEvidence {
    assert!(seed.is_ascii_hexdigit() && !seed.is_ascii_uppercase());
    let oid = |c: char| c.to_string().repeat(40);
    let mut range = RangeEvidence {
        range_id: String::new(),
        format: "cvc.range-evidence/v2".into(),
        version: 2,
        repository_identity: seed.to_string().repeat(64),
        object_format: "sha1".into(),
        base_oid: CommitSha::new(oid('a')),
        tip_oid: CommitSha::new(oid(seed)),
        base_tree_oid: oid('b'),
        result_tree_oid: oid(seed),
        commits: vec![RangeMember {
            commit_oid: CommitSha::new(oid(seed)),
        }],
        changeset_algorithm: "cvc.changeset/v1".into(),
        changeset_digest: seed.to_string().repeat(64),
    };
    range.range_id = range.canonical_id();
    assert!(range.verify_id());
    range
}

/// Records `range` as an explicit local observation, so the projection
/// publishes it once an authorized, locally trusted event references it.
fn observe_range_locally(db_path: &std::path::Path, range: &RangeEvidence) -> anyhow::Result<()> {
    let payload = serde_json::to_string(range)?;
    let digest = hex::encode(Sha256::digest(payload.as_bytes()));
    let db = Connection::open(db_path)?;
    db.execute(
        "INSERT INTO range_evidence(range_id,payload,payload_digest,changeset_algorithm,changeset_digest) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![
            range.range_id,
            payload,
            digest,
            range.changeset_algorithm,
            range.changeset_digest
        ],
    )?;
    db.execute(
        "INSERT INTO range_observations(range_id,source_key,origin,trusted_local) VALUES(?1,'fixture-local','explicit',1)",
        [&range.range_id],
    )?;
    Ok(())
}

/// A squash event of `interaction_id` over `range`.
fn squash_derivation(
    interaction_id: &InteractionId,
    commit: char,
    source_event_ids: Vec<String>,
    range: &RangeEvidence,
) -> DerivationEvent {
    let mut event = derivation(interaction_id, commit, source_event_ids);
    event.relation = DerivationRelation::SquashExact;
    event.origin = DerivationOrigin::LocalHook;
    event.range_id = Some(range.range_id.clone());
    event.event_id = event.canonical_id();
    event
}

/// What another clone's push adds to the tip of `ref_name`: a new commit whose
/// tree also holds `ranges/<id[0..2]>/<id>.json`. Returns the blob it wrote.
fn publish_foreign_range(
    repo: &Repository,
    ref_name: &str,
    range: &RangeEvidence,
) -> anyhow::Result<git2::Oid> {
    let tip = repo.find_reference(ref_name)?.peel_to_commit()?;
    let tree = tip.tree()?;
    let ranges = tree
        .get_name("ranges")
        .expect("a FORMAT5 tree always holds ranges/")
        .to_object(repo)?
        .peel_to_tree()?;
    let shard = &range.range_id[..2];
    let mut shard_builder = match ranges.get_name(shard) {
        Some(entry) => repo.treebuilder(Some(&entry.to_object(repo)?.peel_to_tree()?))?,
        None => repo.treebuilder(None)?,
    };
    let blob = repo.blob(&serde_json::to_vec(range)?)?;
    shard_builder.insert(
        format!("{}.json", range.range_id),
        blob,
        FileMode::Blob.into(),
    )?;
    let mut ranges_builder = repo.treebuilder(Some(&ranges))?;
    ranges_builder.insert(shard, shard_builder.write()?, FileMode::Tree.into())?;
    let mut root = repo.treebuilder(Some(&tree))?;
    root.insert("ranges", ranges_builder.write()?, FileMode::Tree.into())?;
    let signature = Signature::now("Other Clone", "other@example.com")?;
    let commit = repo.commit(
        None,
        &signature,
        &signature,
        "another clone publishes a range",
        &repo.find_tree(root.write()?)?,
        &[&tip],
    )?;
    repo.reference(ref_name, commit, true, "fixture: another clone's range")?;
    Ok(blob)
}

fn wire_range_ids(repo: &Repository, tree: &git2::Tree<'_>) -> anyhow::Result<Vec<String>> {
    let Some(ranges) = tree.get_name("ranges") else {
        return Ok(Vec::new());
    };
    let mut ids = Vec::new();
    for shard in ranges.to_object(repo)?.peel_to_tree()?.iter() {
        for entry in shard.to_object(repo)?.peel_to_tree()?.iter() {
            let range: RangeEvidence =
                serde_json::from_slice(entry.to_object(repo)?.peel_to_blob()?.content())?;
            ids.push(range.range_id);
        }
    }
    ids.sort();
    Ok(ids)
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

/// A clone that holds a tombstone pushes over a tip carrying a range that no
/// event references, which its own database never published. With
/// `import_first` the database has also imported that tip, as the CLI does
/// before every projection; the range is then known to it only as untrusted
/// remote evidence, which is never projected.
fn tombstoned_push_over_an_unreferenced_range(import_first: bool) -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("source.db"))?;
    let kept = summary_interaction("kept", "stays");
    let redacted = summary_interaction("redacted", "suppressed");
    store.create_interaction(&kept)?;
    store.create_interaction(&redacted)?;
    project_and_accept(&repo, &store)?.expect("first candidate");

    let foreign = range_fixture('1');
    let foreign_blob = publish_foreign_range(&repo, SUMMARY_BASELINE, &foreign)?;
    if import_first {
        sync::pull_from_ref_for_remote(&repo, &store, SUMMARY_BASELINE, Some(TEST_DESTINATION))?;
    }
    store.tombstone_remote(
        &redacted.id,
        TEST_DESTINATION,
        TombstoneReasonCode::UserRequested,
        None,
    )?;

    let publication = project_and_accept(&repo, &store)?.expect("the tombstone is a change");
    let tip = accepted_tip(&repo)?;
    assert_eq!(
        wire_range_ids(&repo, &tip)?,
        vec![foreign.range_id.clone()],
        "a range no event references is not about the tombstoned interaction, \
         so a tombstoned push must keep it"
    );
    let path = format!(
        "ranges/{}/{}.json",
        &foreign.range_id[..2],
        foreign.range_id
    );
    assert_eq!(
        tip.get_path(std::path::Path::new(&path))?.id(),
        foreign_blob,
        "the kept range is the very blob the other clone published"
    );
    assert!(has_path(&tip, &tombstone_path(&redacted.id)));
    assert!(!has_path(&tip, &node_path(&redacted.id)));
    assert_eq!(publication.new_tombstones, 1);
    assert_eq!(publication.removed_interactions, 1);
    assert_eq!(
        publication.removed_records, 0,
        "the summary names removals as records of tombstoned interactions; \
         the range is not one"
    );
    Ok(())
}

#[test]
fn a_tombstoned_push_keeps_a_range_no_event_references() -> anyhow::Result<()> {
    tombstoned_push_over_an_unreferenced_range(false)
}

#[test]
fn a_tombstoned_push_keeps_an_unreferenced_range_the_database_imported_as_remote_evidence(
) -> anyhow::Result<()> {
    tombstoned_push_over_an_unreferenced_range(true)
}

/// The tree a destination's accepted candidate holds.
fn accepted_tip(repo: &Repository) -> anyhow::Result<git2::Tree<'_>> {
    Ok(repo
        .find_reference(SUMMARY_BASELINE)?
        .peel_to_commit()?
        .tree()?)
}

fn has_path(tree: &git2::Tree<'_>, path: &str) -> bool {
    tree.get_path(std::path::Path::new(path)).is_ok()
}

fn node_path(id: &InteractionId) -> String {
    format!("nodes/{}/{}.json", &id.to_string()[..2], id)
}

fn tombstone_path(id: &InteractionId) -> String {
    format!("tombstones/{}/{}.json", &id.to_string()[..2], id)
}

/// What `publish_squash_scenario` put on the destination.
struct SquashScenario {
    redacted: Interaction,
    kept: Interaction,
    /// Referenced only by a squash event of `redacted`.
    tied: RangeEvidence,
    /// Referenced by a squash event of `redacted` and one of `kept`.
    common: RangeEvidence,
    /// Event IDs of `kept`, which must survive the tombstone of `redacted`.
    kept_events: Vec<String>,
}

/// Publishes two interactions with squash evidence. A squash range is shared by
/// every interaction whose commits it covers, so `common` belongs to both while
/// `tied` belongs to `redacted` alone.
fn publish_squash_scenario(
    repo: &Repository,
    store: &CvcStore,
    db_path: &std::path::Path,
) -> anyhow::Result<SquashScenario> {
    let kept = summary_interaction("kept", "stays");
    let redacted = summary_interaction("redacted", "suppressed");
    store.create_interaction(&kept)?;
    store.create_interaction(&redacted)?;
    let tied = range_fixture('1');
    let common = range_fixture('2');
    for range in [&tied, &common] {
        observe_range_locally(db_path, range)?;
    }
    let redacted_source = derivation(&redacted.id, 'a', Vec::new());
    let redacted_tied = squash_derivation(
        &redacted.id,
        'b',
        vec![redacted_source.event_id.clone()],
        &tied,
    );
    let redacted_common = squash_derivation(
        &redacted.id,
        'c',
        vec![redacted_source.event_id.clone()],
        &common,
    );
    let kept_source = derivation(&kept.id, 'd', Vec::new());
    let kept_common = squash_derivation(&kept.id, 'e', vec![kept_source.event_id.clone()], &common);
    let kept_events = [&kept_source, &kept_common];
    for event in [&redacted_source, &redacted_tied, &redacted_common]
        .into_iter()
        .chain(kept_events)
    {
        insert_event_fixture(db_path, event)?;
        trust_and_authorize_event(db_path, event)?;
    }
    project_and_accept(repo, store)?.expect("first candidate");
    let tip = accepted_tip(repo)?;
    assert_eq!(
        wire_range_ids(repo, &tip)?,
        sorted(vec![tied.range_id.clone(), common.range_id.clone()]),
        "the projection publishes both ranges"
    );
    assert_eq!(wire_event_ids(repo, &tip)?.len(), 5);
    Ok(SquashScenario {
        redacted,
        kept,
        tied,
        common,
        kept_events: kept_events
            .iter()
            .map(|event| event.event_id.clone())
            .collect(),
    })
}

#[test]
fn a_tombstone_prunes_the_ranges_only_its_events_referenced() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let db_path = temp_dir.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    let scenario = publish_squash_scenario(&repo, &store, &db_path)?;

    store.tombstone_remote(
        &scenario.redacted.id,
        TEST_DESTINATION,
        TombstoneReasonCode::Security,
        None,
    )?;
    let publication = project_and_accept(&repo, &store)?.expect("the tombstone is a change");
    let tip = accepted_tip(&repo)?;

    let ranges = wire_range_ids(&repo, &tip)?;
    assert!(
        !ranges.contains(&scenario.tied.range_id),
        "the range only the tombstoned interaction's events referenced is pruned"
    );
    assert_eq!(
        ranges,
        vec![scenario.common.range_id.clone()],
        "the range a surviving event also references stays"
    );
    assert_eq!(
        wire_event_ids(&repo, &tip)?,
        sorted(scenario.kept_events.clone()),
        "no event of the tombstoned interaction survives"
    );
    assert!(has_path(&tip, &node_path(&scenario.kept.id)));
    assert!(!has_path(&tip, &node_path(&scenario.redacted.id)));
    assert_eq!(publication.removed_interactions, 1);
    assert_eq!(
        publication.removed_records, 4,
        "three events and the one range they alone referenced"
    );
    Ok(())
}

#[test]
fn a_tombstoned_push_keeps_every_kind_of_entry_another_clone_published() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("this-clone.db"))?;
    let kept = summary_interaction("kept", "stays");
    let redacted = summary_interaction("redacted", "suppressed");
    store.create_interaction(&kept)?;
    store.create_interaction(&redacted)?;
    project_and_accept(&repo, &store)?.expect("this clone publishes first");

    // Another clone adds a node, an automatic link, a derivation event (and so
    // `by-commit/` pointers), and a range. This clone's database never saw any
    // of it.
    let other_db = temp_dir.path().join("other-clone.db");
    let other = CvcStore::open(&other_db)?;
    let foreign = summary_interaction("foreign", "published elsewhere");
    other.create_interaction(&foreign)?;
    let linked_commit = "c".repeat(40);
    other.link_interaction(
        &foreign.id,
        &CommitSha::new(linked_commit.clone()),
        "generated",
    )?;
    let foreign_event = derivation(&foreign.id, 'f', Vec::new());
    insert_event_fixture(&other_db, &foreign_event)?;
    trust_and_authorize_event(&other_db, &foreign_event)?;
    project_and_accept(&repo, &other)?.expect("the other clone publishes");
    let foreign_range = range_fixture('1');
    publish_foreign_range(&repo, SUMMARY_BASELINE, &foreign_range)?;

    store.tombstone_remote(
        &redacted.id,
        TEST_DESTINATION,
        TombstoneReasonCode::UserRequested,
        None,
    )?;
    project_and_accept(&repo, &store)?.expect("the tombstone is a change");
    let tip = accepted_tip(&repo)?;

    assert!(
        !has_path(&tip, &node_path(&redacted.id)),
        "the tombstone still removes its node"
    );
    assert!(has_path(&tip, &tombstone_path(&redacted.id)));
    assert!(has_path(&tip, &node_path(&kept.id)));

    let mut dropped = Vec::new();
    if !has_path(&tip, &node_path(&foreign.id)) {
        dropped.push("nodes/");
    }
    if !has_path(
        &tip,
        &format!("links/{}/{}.json", foreign.id, linked_commit),
    ) {
        dropped.push("links/");
    }
    if !wire_event_ids(&repo, &tip)?.contains(&foreign_event.event_id) {
        dropped.push("events/");
    }
    if !has_path(&tip, &format!("by-commit/{}/{}", linked_commit, foreign.id))
        || !has_path(
            &tip,
            &format!("by-commit/{}/{}", "f".repeat(40), foreign.id),
        )
    {
        dropped.push("by-commit/");
    }
    if !wire_range_ids(&repo, &tip)?.contains(&foreign_range.range_id) {
        dropped.push("ranges/");
    }
    assert!(
        dropped.is_empty(),
        "a tombstoned push dropped what another clone published under {dropped:?}"
    );

    // A reader still accepts the result: the kept entries agree with the
    // rebuilt `by-commit/` index.
    let reader = CvcStore::open(temp_dir.path().join("reader.db"))?;
    sync::pull_from_ref_for_remote(&repo, &reader, SUMMARY_BASELINE, Some(TEST_DESTINATION))?;
    assert!(reader.get_interaction(&foreign.id)?.is_some());
    assert!(reader.get_interaction(&kept.id)?.is_some());
    assert!(reader.get_interaction(&redacted.id)?.is_none());
    Ok(())
}

#[test]
fn hard_redaction_prunes_the_ranges_only_the_target_referenced_and_keeps_the_rest(
) -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let db_path = temp_dir.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    let scenario = publish_squash_scenario(&repo, &store, &db_path)?;
    let unredacted = accepted_tip(&repo)?;
    let stale_events = unredacted.get_name("events").unwrap().id();
    let stale_ranges = unredacted.get_name("ranges").unwrap().id();

    store.tombstone_remote(
        &scenario.redacted.id,
        TEST_DESTINATION,
        TombstoneReasonCode::Security,
        None,
    )?;
    project_and_accept(&repo, &store)?.expect("the tombstone is a change");

    // The destination tip a redaction plan starts from: it holds the tombstone
    // but an older client left the target's derivation evidence next to it, and
    // another clone has added a range of its own.
    let suppressed = accepted_tip(&repo)?;
    let mut stale = repo.treebuilder(Some(&suppressed))?;
    stale.insert("events", stale_events, FileMode::Tree.into())?;
    stale.insert("ranges", stale_ranges, FileMode::Tree.into())?;
    let signature = Signature::now("Old Client", "old@example.com")?;
    let tip = repo.find_reference(SUMMARY_BASELINE)?.peel_to_commit()?;
    let stale_tip = repo.commit(
        None,
        &signature,
        &signature,
        "an older client leaves the derivation evidence",
        &repo.find_tree(stale.write()?)?,
        &[&tip],
    )?;
    repo.reference(SUMMARY_BASELINE, stale_tip, true, "stale fixture")?;
    let foreign = range_fixture('3');
    publish_foreign_range(&repo, SUMMARY_BASELINE, &foreign)?;

    let candidate = sync::build_hard_redaction_plan(
        &repo,
        Some(SUMMARY_BASELINE),
        TEST_DESTINATION,
        &scenario.redacted.id,
    )?;
    let replacement = repo
        .find_commit(git2::Oid::from_str(&candidate.plan.replacement_commit)?)?
        .tree()?;
    assert_eq!(
        wire_event_ids(&repo, &replacement)?,
        sorted(scenario.kept_events.clone()),
        "no event of the target survives the replacement"
    );
    assert_eq!(
        wire_range_ids(&repo, &replacement)?,
        sorted(vec![
            scenario.common.range_id.clone(),
            foreign.range_id.clone()
        ]),
        "the replacement drops the range only the target referenced, keeps the one \
         a surviving event references, and keeps the one nothing references"
    );
    Ok(())
}

// --- Baseline pinning and the publication-state guard (#106 round 2) -----------

const PIN_TRACKING_REF: &str = "refs/remotes/fixture/cvc/main";

fn pin_destination() -> cvc_core::privacy::RemoteDestination {
    cvc_core::privacy::RemoteDestination {
        name: "fixture".into(),
        effective_url: "unused-in-these-tests".into(),
        fingerprint: TEST_DESTINATION.into(),
        ref_name: "refs/cvc/main".into(),
    }
}

fn publication_rows(db_path: &std::path::Path) -> anyhow::Result<Vec<(String, String, i64)>> {
    let raw = Connection::open(db_path)?;
    let mut statement = raw.prepare(
        "SELECT interaction_id, state, updated_at FROM publications \
         ORDER BY interaction_id, remote_fingerprint",
    )?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn candidate_refs(repo: &Repository) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    for reference in repo.references_glob("refs/cvc/candidate-*")? {
        names.push(reference?.name().unwrap_or_default().to_owned());
    }
    Ok(names)
}

/// Asserts `result` is the fail-closed "baseline moved" error and nothing else.
fn assert_baseline_moved(
    result: Result<sync::ProjectionResult<'_>, sync::SyncError>,
) -> anyhow::Result<()> {
    match result {
        Err(error @ sync::SyncError::BaselineMoved(_)) => {
            assert_eq!(
                error.to_string(),
                "the remote CVC ref for 'fixture' changed while publishing; nothing was \
                 published. Run the command again."
            );
            Ok(())
        }
        Err(other) => anyhow::bail!("unexpected error: {other}"),
        Ok(_) => anyhow::bail!("a moved baseline must not produce a projection"),
    }
}

#[test]
fn import_pins_the_commit_it_read_and_projection_accepts_it() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("source.db"))?;
    let destination = pin_destination();

    // Absent: the import records that it saw no ref, and the projection starts empty.
    let (imported, baseline) = sync::pull_destination_pinned(&repo, &store, &destination)?;
    assert_eq!(imported, 0);
    assert_eq!(baseline.tip(), None);
    assert_eq!(baseline.ref_name(), PIN_TRACKING_REF);

    store.create_interaction(&summary_interaction("pinned-a", "already there"))?;
    project_fixture_to_ref(&repo, &store, PIN_TRACKING_REF)?;
    let tip = repo
        .find_reference(PIN_TRACKING_REF)?
        .peel_to_commit()?
        .id();
    let (_, baseline) = sync::pull_destination_pinned(&repo, &store, &destination)?;
    assert_eq!(baseline.tip(), Some(tip));

    store.create_interaction(&summary_interaction("pinned-b", "newly shared"))?;
    let sync::ProjectionResult::Candidate { publication, .. } =
        sync::push_projection_to_ref(&repo, &store, &baseline, TEST_DESTINATION)?
    else {
        anyhow::bail!("expected a candidate on the pinned baseline");
    };
    assert_eq!(publication.new_ids.len(), 1);
    assert_eq!(publication.shared_total, 2);
    Ok(())
}

#[test]
fn projection_fails_closed_when_the_tracking_ref_moves_after_the_import() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let db_path = temp_dir.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    let destination = pin_destination();

    store.create_interaction(&summary_interaction("moved-a", "on the destination"))?;
    project_fixture_to_ref(&repo, &store, PIN_TRACKING_REF)?;
    let (_, baseline) = sync::pull_destination_pinned(&repo, &store, &destination)?;
    let imported_tip = baseline.tip();
    assert!(imported_tip.is_some());

    // Another writer (a second process after a URL change, a plain `git fetch`)
    // moves the tracking ref to a commit holding more than the import read.
    store.create_interaction(&summary_interaction("moved-b", "on the destination too"))?;
    project_fixture_to_ref(&repo, &store, PIN_TRACKING_REF)?;
    assert_ne!(
        repo.find_reference(PIN_TRACKING_REF)?
            .peel_to_commit()?
            .id(),
        imported_tip.unwrap()
    );

    let rows_before = publication_rows(&db_path)?;
    assert_baseline_moved(sync::push_projection_to_ref(
        &repo,
        &store,
        &baseline,
        TEST_DESTINATION,
    ))?;
    assert_eq!(
        publication_rows(&db_path)?,
        rows_before,
        "no publication state may change"
    );
    assert!(
        candidate_refs(&repo)?.is_empty(),
        "no candidate may be left behind"
    );
    Ok(())
}

#[test]
fn projection_fails_closed_when_a_ref_the_import_saw_absent_appears() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let db_path = temp_dir.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    store.create_interaction(&summary_interaction("appears-a", "local"))?;

    let (_, baseline) = sync::pull_destination_pinned(&repo, &store, &pin_destination())?;
    assert_eq!(baseline.tip(), None);

    // The ref appears after the import saw it absent.
    project_fixture_to_ref(&repo, &store, PIN_TRACKING_REF)?;
    let rows_before = publication_rows(&db_path)?;
    assert_baseline_moved(sync::push_projection_to_ref(
        &repo,
        &store,
        &baseline,
        TEST_DESTINATION,
    ))?;
    assert_eq!(publication_rows(&db_path)?, rows_before);
    assert!(candidate_refs(&repo)?.is_empty());
    Ok(())
}

#[test]
fn projection_fails_closed_when_the_ref_the_import_read_disappears() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let db_path = temp_dir.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    store.create_interaction(&summary_interaction("vanishes-a", "local"))?;
    project_fixture_to_ref(&repo, &store, PIN_TRACKING_REF)?;
    let (_, baseline) = sync::pull_destination_pinned(&repo, &store, &pin_destination())?;
    assert!(baseline.tip().is_some());

    repo.find_reference(PIN_TRACKING_REF)?.delete()?;
    let rows_before = publication_rows(&db_path)?;
    assert_baseline_moved(sync::push_projection_to_ref(
        &repo,
        &store,
        &baseline,
        TEST_DESTINATION,
    ))?;
    assert_eq!(publication_rows(&db_path)?, rows_before);
    Ok(())
}

#[test]
fn published_rows_are_never_demoted_by_pending_or_unknown_marks() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let db_path = temp_dir.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    let interaction = summary_interaction("never-demoted", "published once");
    store.create_interaction(&interaction)?;
    let ids = std::slice::from_ref(&interaction.id);

    store.mark_publication(ids, TEST_DESTINATION, PublicationState::Published)?;
    let raw = Connection::open(&db_path)?;
    raw.execute("UPDATE publications SET updated_at = 1000", [])?;
    let expected = vec![(interaction.id.to_string(), "published".to_owned(), 1000)];

    store.mark_publication(ids, TEST_DESTINATION, PublicationState::Pending)?;
    assert_eq!(publication_rows(&db_path)?, expected);
    store.mark_publication(ids, TEST_DESTINATION, PublicationState::Unknown)?;
    assert_eq!(publication_rows(&db_path)?, expected);
    store.mark_publication(ids, TEST_DESTINATION, PublicationState::Published)?;
    assert_eq!(publication_rows(&db_path)?, expected);

    // The guard is per destination, and the other transitions still work.
    let other = "f".repeat(64);
    store.mark_publication(ids, &other, PublicationState::Pending)?;
    store.mark_publication(ids, &other, PublicationState::Unknown)?;
    let states: Vec<String> = publication_rows(&db_path)?
        .into_iter()
        .filter(|row| row.1 != "published")
        .map(|row| row.1)
        .collect();
    assert_eq!(states, vec!["unknown".to_owned()]);
    Ok(())
}

#[test]
fn a_locally_published_id_missing_from_the_baseline_is_resent_and_stays_published(
) -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let db_path = temp_dir.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    let interaction = summary_interaction("rewound", "the remote lost this");
    store.create_interaction(&interaction)?;
    store.mark_publication(
        std::slice::from_ref(&interaction.id),
        TEST_DESTINATION,
        PublicationState::Published,
    )?;
    Connection::open(&db_path)?.execute("UPDATE publications SET updated_at = 1000", [])?;

    // The destination was rewound or recreated: the import sees no ref at all.
    let (_, baseline) = sync::pull_destination_pinned(&repo, &store, &pin_destination())?;
    assert_eq!(baseline.tip(), None);
    let sync::ProjectionResult::Candidate { publication, .. } =
        sync::push_projection_to_ref(&repo, &store, &baseline, TEST_DESTINATION)?
    else {
        anyhow::bail!("the missing node must be re-sent");
    };
    assert_eq!(publication.new_ids, vec![interaction.id.clone()]);

    // The marks `push_destination` applies around a re-send, including a failure.
    let ids = publication.new_ids.as_slice();
    store.mark_publication(ids, TEST_DESTINATION, PublicationState::Pending)?;
    store.mark_publication(ids, TEST_DESTINATION, PublicationState::Unknown)?;
    assert_eq!(
        publication_rows(&db_path)?,
        vec![(interaction.id.to_string(), "published".to_owned(), 1000)],
        "unshare and redact must keep treating this content as published"
    );
    Ok(())
}

#[test]
fn a_failed_transport_leaves_baseline_held_rows_published_and_marks_only_new_ones_unknown(
) -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let db_path = temp_dir.path().join("source.db");
    let store = CvcStore::open(&db_path)?;
    let destination = pin_destination();
    let held = summary_interaction("held", "already on the destination");
    store.create_interaction(&held)?;
    project_fixture_to_ref(&repo, &store, PIN_TRACKING_REF)?;
    let (_, baseline) = sync::pull_destination_pinned(&repo, &store, &destination)?;
    Connection::open(&db_path)?.execute("UPDATE publications SET updated_at = 1000", [])?;
    let fresh = summary_interaction("fresh", "being published now");
    store.create_interaction(&fresh)?;

    let sync::ProjectionResult::Candidate {
        candidate,
        publication,
        ..
    } = sync::push_projection_to_ref(&repo, &store, &baseline, TEST_DESTINATION)?
    else {
        anyhow::bail!("expected a candidate");
    };
    assert_eq!(publication.new_ids, vec![fresh.id.clone()]);

    // What `push_destination` does around the transport, with a real failure.
    let ids = publication.new_ids.as_slice();
    store.mark_publication(ids, TEST_DESTINATION, PublicationState::Pending)?;
    let unreachable = cvc_core::privacy::RemoteDestination {
        name: "unreachable".into(),
        effective_url: temp_dir.path().join("missing-remote").display().to_string(),
        fingerprint: TEST_DESTINATION.into(),
        ref_name: "refs/cvc/main".into(),
    };
    assert!(sync::push_temp_ref(&repo, &unreachable, candidate.ref_name()).is_err());
    store.mark_publication(ids, TEST_DESTINATION, PublicationState::Unknown)?;
    drop(candidate);

    let rows = publication_rows(&db_path)?;
    let state_of = |id: &InteractionId| {
        rows.iter()
            .find(|row| row.0 == id.to_string())
            .map(|row| (row.1.clone(), row.2))
    };
    assert_eq!(state_of(&held.id), Some(("published".to_owned(), 1000)));
    assert_eq!(
        state_of(&fresh.id).map(|row| row.0),
        Some("unknown".to_owned())
    );
    assert_eq!(rows.len(), 2);
    Ok(())
}
