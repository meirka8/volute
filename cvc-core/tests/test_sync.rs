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
    let projection = sync::push_projection_to_ref(repo, store, baseline, TEST_DESTINATION)?;
    match projection {
        sync::ProjectionResult::Candidate { oid, candidate, .. } => {
            repo.reference(ref_name, oid, true, "fixture: remote accepted candidate")?;
            drop(candidate);
        }
        sync::ProjectionResult::NoChanges => {}
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
    let projection = sync::push_projection_to_ref(&repo, &store, "", TEST_DESTINATION)?;
    let oid = match projection {
        sync::ProjectionResult::Candidate { oid, .. } => oid,
        _ => anyhow::bail!("expected projection"),
    };
    assert_eq!(
        wire_event_ids(&repo, &repo.find_commit(oid)?.tree()?)?,
        vec![allowed.event_id.clone()]
    );

    let other = sync::push_projection_to_ref(&repo, &store, "", OTHER)?;
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
    assert!(sync::push_projection_to_ref(&repo, &store, "", TEST_DESTINATION).is_err());
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
            "refs/remotes/fixture/cvc/main",
            TEST_DESTINATION,
        )?,
        sync::ProjectionResult::NoChanges
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

    let first = sync::push_projection_to_ref(&repo, &store, "", TEST_DESTINATION)?;
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
            "refs/remotes/fixture/cvc/main",
            TEST_DESTINATION,
        )?,
        sync::ProjectionResult::NoChanges
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
    let projection = sync::push_projection_to_ref(&repo, &store, "", TEST_DESTINATION)?;
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
    let projection = sync::push_projection_to_ref(&repo, &store, "", TEST_DESTINATION)?;
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
    // Simulates HEL-57's continuation-of-work scenario: work started on one machine
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
fn test_sync_v2_round_trip() -> anyhow::Result<()> {
    // HEL-65 acceptance criterion: push v2 layout, pull into a fresh clone, DBs equal.
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
    // Repos synced before HEL-65 have a flat `<id>.json` tree with no nodes/,
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
