use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::db::DbError;
use cvc_core::models::{Author, CommitSha, Conversation, Interaction, InteractionId};
use cvc_core::privacy::{McpCapture, PreparedPolicy};
use cvc_core::{changeset, squash};
use git2::{FileMode, ObjectType, Oid, Repository, Signature, Tree};
use sha2::Digest;
use tempfile::TempDir;

fn tree<'a>(repo: &'a Repository, entries: &[(&str, &[u8], i32)]) -> anyhow::Result<Tree<'a>> {
    let mut builder = repo.treebuilder(None)?;
    for (name, body, mode) in entries {
        builder.insert(name, repo.blob(body)?, *mode)?;
    }
    Ok(repo.find_tree(builder.write()?)?)
}
fn commit(
    repo: &Repository,
    update: Option<&str>,
    parent: Option<Oid>,
    tree: &Tree<'_>,
    message: &str,
) -> anyhow::Result<Oid> {
    let sig = Signature::now("squash", "squash@test")?;
    let p = parent.map(|x| repo.find_commit(x)).transpose()?;
    let parents: Vec<_> = p.iter().collect();
    Ok(repo.commit(update, &sig, &sig, message, tree, &parents)?)
}
fn interaction(
    store: &CvcStore,
    db_path: &std::path::Path,
    commit_oid: Oid,
) -> anyhow::Result<InteractionId> {
    interaction_named(store, db_path, commit_oid, "squash-test")
}
fn interaction_named(
    store: &CvcStore,
    db_path: &std::path::Path,
    commit_oid: Oid,
    conversation: &str,
) -> anyhow::Result<InteractionId> {
    let id = InteractionId::new();
    let now = Utc::now();
    let i = Interaction {
        id: id.clone(),
        conversation_id: conversation.into(),
        parent_id: None,
        timestamp: now,
        author: Author::Human,
        user_prompt: "exact source".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.capture_mcp(McpCapture::new(
        Conversation {
            id: conversation.into(),
            title: "test".into(),
            created_at: now,
        },
        i,
        vec![],
        vec![],
        PreparedPolicy::built_ins_only(),
        "0".repeat(64),
    ))?;
    store.link_interaction(&id, &CommitSha::new(commit_oid.to_string()), "generated")?;
    let key = format!("legacy:{id}:{commit_oid}");
    rusqlite::Connection::open(db_path)?.execute(
        "INSERT INTO derivation_observations(event_id,source_fingerprint,source_key,origin,trusted_local) VALUES(?1,NULL,?1,'local_api',1)",
        [key],
    )?;
    Ok(id)
}

#[test]
fn exact_squash_attaches_source_and_ambiguity_or_change_is_noop() -> anyhow::Result<()> {
    for case in ["exact", "duplicate_observation", "changed", "delayed"] {
        let temp = TempDir::new()?;
        let repo = Repository::init(temp.path())?;
        let mut store = CvcStore::open(temp.path().join("index.db"))?;
        let base_tree = tree(&repo, &[("a", b"base", FileMode::Blob.into())])?;
        let base = commit(&repo, Some("refs/heads/main"), None, &base_tree, "base")?;
        repo.set_head("refs/heads/main")?;
        let feature_tree = tree(
            &repo,
            &[
                ("a", b"feature", FileMode::Blob.into()),
                ("bin", b"\0\xff", FileMode::BlobExecutable.into()),
            ],
        )?;
        let feature = commit(
            &repo,
            Some("refs/heads/feature"),
            Some(base),
            &feature_tree,
            "feature",
        )?;
        let id = interaction(&store, &temp.path().join("index.db"), feature)?;
        let first_range = if case != "delayed" {
            Some(squash::observe_explicit_range(
                &repo,
                &store,
                base,
                feature,
                Some("refs/heads/feature"),
                None,
                None,
            )?)
        } else {
            None
        };
        if case == "duplicate_observation" {
            let duplicate = squash::observe_pre_push_range_with_abort(
                &repo,
                &store,
                base,
                feature,
                Some("refs/heads/feature"),
                None,
                None,
                || false,
            )?;
            assert_eq!(first_range.as_ref().unwrap().range_id, duplicate.range_id);
        }
        if let Some(range) = &first_range {
            let wire = serde_json::to_string(range)?;
            assert!(!wire.contains(&id.to_string()));
            assert!(!wire.contains("source_event_ids"));
            assert!(!wire.contains("observation_origin"));
        }
        let target_tree = if case == "changed" {
            tree(
                &repo,
                &[("a", b"resolved differently", FileMode::Blob.into())],
            )?
        } else {
            repo.find_tree(feature_tree.id())?
        };
        let target = commit(
            &repo,
            Some("refs/heads/main"),
            Some(base),
            &target_tree,
            "squash",
        )?;
        repo.set_head("refs/heads/main")?;
        let count = squash::scan(&repo, &mut store, true)?;
        if case == "delayed" {
            assert_eq!(count, 0);
            let pending: i64 = rusqlite::Connection::open(temp.path().join("index.db"))?
                .query_row(
                    "SELECT COUNT(*) FROM pending_squash_targets WHERE status='pending'",
                    [],
                    |r| r.get(0),
                )?;
            assert_eq!(pending, 1);
            let worktree = hex::encode(sha2::Sha256::digest(
                repo.path().as_os_str().as_encoded_bytes(),
            ));
            let newer: Vec<_> = (1..=129)
                .map(|number| (format!("f{number:039x}"), format!("e{number:039x}"), true))
                .collect();
            assert!(store.discover_squash_targets(
                &worktree,
                "refs/heads/main",
                Some(&target.to_string()),
                &target.to_string(),
                &newer
            )?);
            squash::observe_explicit_range(&repo, &store, base, feature, None, None, None)?;
            assert_eq!(
                squash::scan(&repo, &mut store, false)?,
                0,
                "first fair batch services older unattempted arrivals"
            );
            assert_eq!(
                squash::scan(&repo, &mut store, false)?,
                1,
                "delayed original rotates back after >128 newer candidates"
            );
            continue;
        }
        if case != "changed" {
            assert_eq!(count, 1);
            assert!(store
                .get_artifact_links(&id)?
                .iter()
                .any(|l| l.git_commit_hash.as_str() == target.to_string()
                    && l.link_type == "squash_exact"));
        } else {
            assert_eq!(count, 0);
            let pending: i64 = rusqlite::Connection::open(temp.path().join("index.db"))?
                .query_row(
                    "SELECT COUNT(*) FROM pending_squash_targets WHERE status='pending'",
                    [],
                    |r| r.get(0),
                )?;
            assert_eq!(pending, 1);
            assert!(!store
                .get_artifact_links(&id)?
                .iter()
                .any(|l| l.git_commit_hash.as_str() == target.to_string()));
        }
    }
    Ok(())
}

#[test]
fn reset_sibling_head_is_bounded_candidate_and_deadline_preserves_pending() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let mut store = CvcStore::open(temp.path().join("index.db"))?;
    let base_tree = tree(&repo, &[("a", b"base", FileMode::Blob.into())])?;
    let base = commit(&repo, Some("refs/heads/main"), None, &base_tree, "base")?;
    repo.set_head("refs/heads/main")?;
    let feature_tree = tree(&repo, &[("a", b"feature", FileMode::Blob.into())])?;
    let feature = commit(
        &repo,
        Some("refs/heads/feature"),
        Some(base),
        &feature_tree,
        "feature",
    )?;
    let id = interaction(&store, &temp.path().join("index.db"), feature)?;
    squash::observe_explicit_range(
        &repo,
        &store,
        base,
        feature,
        Some("refs/heads/main"),
        None,
        None,
    )?;
    let other_tree = tree(&repo, &[("a", b"other", FileMode::Blob.into())])?;
    let other = commit(&repo, None, Some(base), &other_tree, "other")?;
    repo.set_head("refs/heads/main")?;
    assert_eq!(squash::scan(&repo, &mut store, false)?, 0);
    let target = commit(
        &repo,
        None,
        Some(base),
        &feature_tree,
        "reset sibling squash",
    )?;
    repo.reference("refs/heads/main", target, true, "test reset")?;
    assert_ne!(other, target);
    assert!(matches!(
        squash::scan_with_abort(&repo, &mut store, false, || true),
        Err(squash::SquashError::Deadline)
    ));
    assert_eq!(squash::scan(&repo, &mut store, false)?, 1);
    assert!(store
        .get_artifact_links(&id)?
        .iter()
        .any(|link| link.git_commit_hash.as_str() == target.to_string()
            && link.link_type == "squash_exact"));
    Ok(())
}

#[test]
fn changeset_is_net_tree_exact_binary_mode_root_and_raw_path() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let empty = repo.find_tree(repo.treebuilder(None)?.write()?)?;
    let normal = tree(&repo, &[("x", b"a\0b", FileMode::Blob.into())])?;
    let executable = tree(&repo, &[("x", b"a\0b", FileMode::BlobExecutable.into())])?;
    let root = changeset::identify(&repo, None, &normal)?;
    assert_eq!(root.deltas, 1);
    assert!(matches!(
        changeset::identify_with_abort(&repo, None, &normal, || true),
        Err(changeset::ChangesetError::Deadline)
    ));
    assert_ne!(
        root.digest,
        changeset::identify(&repo, Some(&empty), &executable)?.digest
    );
    // A cancelled intermediate change has the same identity as direct base->result.
    assert_eq!(
        changeset::identify(&repo, Some(&normal), &normal)?.digest,
        changeset::identify(&repo, Some(&normal), &normal)?.digest
    );
    // Construct a legal non-UTF8 Git tree through libgit2's ODB; no filesystem conversion occurs.
    let blob = repo.blob(b"raw")?;
    let mut raw = b"100644 bad\xff\0".to_vec();
    raw.extend_from_slice(blob.as_bytes());
    let oid = repo.odb()?.write(ObjectType::Tree, &raw)?;
    let raw_tree = repo.find_tree(oid)?;
    assert_eq!(changeset::identify(&repo, None, &raw_tree)?.deltas, 1);
    Ok(())
}

#[test]
fn changeset_covers_symlink_gitlink_typechange_rename_and_path_bound() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let empty = repo.find_tree(repo.treebuilder(None)?.write()?)?;
    let blob = repo.blob(b"shared object")?;
    let mut base_builder = repo.treebuilder(None)?;
    base_builder.insert("old-name", blob, FileMode::Blob.into())?;
    base_builder.insert("kind", blob, FileMode::Blob.into())?;
    let base = repo.find_tree(base_builder.write()?)?;

    // A rename remains the canonical delete+add tree transition: no similarity
    // heuristic is allowed to make the digest configuration-dependent.
    let mut renamed_builder = repo.treebuilder(None)?;
    renamed_builder.insert("new-name", blob, FileMode::Blob.into())?;
    renamed_builder.insert("kind", blob, FileMode::Blob.into())?;
    let renamed = repo.find_tree(renamed_builder.write()?)?;
    let rename = changeset::identify(&repo, Some(&base), &renamed)?;
    assert_eq!(rename.deltas, 2);

    let symlink = tree(&repo, &[("kind", b"target/path", FileMode::Link.into())])?;
    let gitlink = tree(
        &repo,
        &[("kind", b"not inspected", FileMode::Commit.into())],
    )?;
    let typechange = changeset::identify(&repo, Some(&base), &symlink)?;
    assert_ne!(
        typechange.digest,
        changeset::identify(&repo, Some(&base), &gitlink)?.digest
    );
    assert_ne!(
        typechange.digest,
        changeset::identify(&repo, Some(&empty), &symlink)?.digest
    );

    let long_name = "p".repeat(changeset::MAX_PATH_BYTES + 1);
    let mut oversized = repo.treebuilder(None)?;
    oversized.insert(&long_name, blob, FileMode::Blob.into())?;
    let oversized = repo.find_tree(oversized.write()?)?;
    assert!(matches!(
        changeset::identify(&repo, None, &oversized),
        Err(changeset::ChangesetError::Bound("path"))
    ));
    Ok(())
}

#[test]
fn mixed_private_shared_sources_project_code_only_range_to_exact_destination() -> anyhow::Result<()>
{
    use cvc_core::FutureSharePolicy;
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let mut store = CvcStore::open(temp.path().join("index.db"))?;
    let base_tree = tree(&repo, &[("a", b"base", FileMode::Blob.into())])?;
    let base = commit(&repo, Some("refs/heads/main"), None, &base_tree, "base")?;
    let feature_tree = tree(&repo, &[("a", b"feature", FileMode::Blob.into())])?;
    let feature = commit(
        &repo,
        Some("refs/heads/feature"),
        Some(base),
        &feature_tree,
        "feature",
    )?;
    let shared = interaction_named(&store, &temp.path().join("index.db"), feature, "shared")?;
    let private = interaction_named(&store, &temp.path().join("index.db"), feature, "private")?;
    store.share_conversation_for_remote("shared", "remote-a", FutureSharePolicy::Private)?;
    let range = squash::observe_explicit_range(&repo, &store, base, feature, None, None, None)?;
    let target = commit(&repo, None, Some(base), &feature_tree, "squash")?;
    repo.reference("refs/heads/main", target, true, "test")?;
    repo.set_head("refs/heads/main")?;
    assert_eq!(squash::scan(&repo, &mut store, true)?, 2);
    let projected = store.projection_ranges("remote-a")?;
    assert_eq!(projected, vec![range]);
    let wire = serde_json::to_string(&projected)?;
    assert!(!wire.contains(&shared.to_string()));
    assert!(!wire.contains(&private.to_string()));
    assert!(store.projection_ranges("remote-b")?.is_empty());
    let events = store.projection_derivation_events(std::slice::from_ref(&shared), "remote-a")?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].interaction_id, shared);
    assert_ne!(events[0].interaction_id, private);
    Ok(())
}

#[test]
fn pending_queue_is_fair_beyond_one_batch() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let store = CvcStore::open(temp.path().join("index.db"))?;
    let worktree = "worktree";
    let branch = "refs/heads/main";
    let original = format!("{:040x}", 1);
    assert!(store.discover_squash_targets(
        worktree,
        branch,
        None,
        &original,
        &[(original.clone(), format!("{:040x}", 0), true)]
    )?);
    let newer: Vec<_> = (2..=130)
        .map(|number| {
            (
                format!("{number:040x}"),
                format!("{:040x}", number - 1),
                true,
            )
        })
        .collect();
    assert!(store.discover_squash_targets(
        worktree,
        branch,
        Some(&original),
        &format!("{:040x}", 130),
        &newer
    )?);
    let first = store.pending_squash_targets(worktree, branch)?;
    assert_eq!(first.len(), 128);
    assert_eq!(first[0].0, original);
    for (target, _) in &first {
        store.mark_squash_attempt(worktree, branch, target)?;
    }
    let second = store.pending_squash_targets(worktree, branch)?;
    assert_eq!(second[0].0, format!("{:040x}", 129));
    assert_eq!(second[1].0, format!("{:040x}", 130));
    for (target, _) in second.iter().take(2) {
        store.mark_squash_attempt(worktree, branch, target)?;
    }
    let third = store.pending_squash_targets(worktree, branch)?;
    assert_eq!(
        third[0].0, original,
        "delayed oldest candidate must rotate back after newer candidates receive attempts"
    );
    Ok(())
}

#[test]
fn pending_queue_cap_refuses_discovery_without_dropping_unresolved() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let store = CvcStore::open(temp.path().join("index.db"))?;
    let targets: Vec<_> = (1..=4096)
        .map(|number| {
            (
                format!("{number:040x}"),
                format!("{:040x}", number - 1),
                true,
            )
        })
        .collect();
    assert!(store.discover_squash_targets(
        "w",
        "refs/heads/main",
        None,
        &format!("{:040x}", 4096),
        &targets
    )?);
    assert!(matches!(
        store.discover_squash_targets(
            "w",
            "refs/heads/main",
            Some(&format!("{:040x}", 4096)),
            &format!("{:040x}", 4097),
            &[(format!("{:040x}", 4097), format!("{:040x}", 4096), true)]
        ),
        Err(DbError::SquashQueueCapacity { limit: 4096 })
    ));
    let db = rusqlite::Connection::open(temp.path().join("index.db"))?;
    let pending: i64 = db.query_row(
        "SELECT COUNT(*) FROM pending_squash_targets WHERE status='pending'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(pending, 4096);
    Ok(())
}

#[test]
fn cursor_reachable_only_through_merge_side_parent_resets_without_wedging() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let mut store = CvcStore::open(temp.path().join("index.db"))?;
    let base_tree = tree(&repo, &[("a", b"base", FileMode::Blob.into())])?;
    let base = commit(&repo, Some("refs/heads/main"), None, &base_tree, "base")?;
    let main_tree = tree(&repo, &[("a", b"main", FileMode::Blob.into())])?;
    let main = commit(&repo, None, Some(base), &main_tree, "main")?;
    let side_tree = tree(&repo, &[("a", b"side", FileMode::Blob.into())])?;
    let side = commit(
        &repo,
        Some("refs/heads/main"),
        Some(base),
        &side_tree,
        "side",
    )?;
    repo.set_head("refs/heads/main")?;
    assert_eq!(squash::scan(&repo, &mut store, false)?, 0);

    let merge_tree = tree(&repo, &[("a", b"merge", FileMode::Blob.into())])?;
    let sig = Signature::now("squash", "squash@test")?;
    let main_commit = repo.find_commit(main)?;
    let side_commit = repo.find_commit(side)?;
    let merge = repo.commit(
        None,
        &sig,
        &sig,
        "merge",
        &merge_tree,
        &[&main_commit, &side_commit],
    )?;
    repo.reference("refs/heads/main", merge, true, "merge fixture")?;
    assert_eq!(squash::scan(&repo, &mut store, false)?, 0);

    let child_tree = tree(&repo, &[("a", b"child", FileMode::Blob.into())])?;
    let child = commit(
        &repo,
        Some("refs/heads/main"),
        Some(merge),
        &child_tree,
        "child",
    )?;
    assert_eq!(squash::scan(&repo, &mut store, false)?, 0);
    let db = rusqlite::Connection::open(temp.path().join("index.db"))?;
    let cursor: String = db.query_row(
        "SELECT last_tip FROM branch_scan_cursors WHERE symbolic_ref='refs/heads/main'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(cursor, child.to_string());
    let targets: Vec<String> = db
        .prepare("SELECT target_commit FROM pending_squash_targets WHERE status='pending'")?
        .query_map([], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(targets.contains(&child.to_string()));
    assert!(!targets.contains(&merge.to_string()));
    Ok(())
}
