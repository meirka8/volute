use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::{
    Author, CommitSha, Conversation, FutureSharePolicy, Interaction, InteractionId,
};
use cvc_core::privacy::{McpCapture, PreparedPolicy};
use cvc_core::rewrite;
use cvc_core::sync;
use git2::{Repository, Signature};
use std::fs;
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

fn commit(repo: &Repository, message: &str) -> anyhow::Result<git2::Oid> {
    let tree = repo.find_tree(repo.treebuilder(None)?.write()?)?;
    let signature = Signature::now("rewrite test", "rewrite@example.test")?;
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    Ok(repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents,
    )?)
}

fn replacement_root(repo: &Repository) -> anyhow::Result<git2::Oid> {
    let tree = repo.find_tree(repo.treebuilder(None)?.write()?)?;
    let signature = Signature::now("rewrite test", "rewrite@example.test")?;
    let oid = repo.commit(None, &signature, &signature, "replacement", &tree, &[])?;
    repo.set_head_detached(oid)?;
    Ok(oid)
}

fn interaction() -> Interaction {
    Interaction {
        id: InteractionId::new(),
        conversation_id: "rewrite-test".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "preserve this provenance".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    }
}

fn persist(store: &CvcStore, interaction: &Interaction) -> anyhow::Result<()> {
    store.capture_mcp(McpCapture::new(
        Conversation {
            id: interaction.conversation_id.clone(),
            title: "rewrite test".into(),
            created_at: interaction.timestamp,
        },
        interaction.clone(),
        Vec::new(),
        Vec::new(),
        PreparedPolicy::built_ins_only(),
    ))?;
    Ok(())
}

fn trust_legacy_source(
    db_path: &std::path::Path,
    interaction: &InteractionId,
    commit: git2::Oid,
) -> anyhow::Result<()> {
    let key = format!("legacy:{interaction}:{commit}");
    rusqlite::Connection::open(db_path)?.execute(
        "INSERT INTO derivation_observations(event_id,source_fingerprint,source_key,origin,trusted_local) VALUES(?1,NULL,?1,'local_api',1)",
        [key],
    )?;
    Ok(())
}

#[test]
fn amend_and_rebase_append_exact_events_without_replacing_old_links() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let mut store = CvcStore::open(temp.path().join("index.db"))?;
    let first_old = commit(&repo, "old one")?;
    let second_old = commit(&repo, "old two")?;
    let third_old = commit(&repo, "old three")?;
    let first = interaction();
    let second = interaction();
    persist(&store, &first)?;
    persist(&store, &second)?;
    store.link_interaction(
        &first.id,
        &CommitSha::new(first_old.to_string()),
        "generated",
    )?;
    trust_legacy_source(&temp.path().join("index.db"), &first.id, first_old)?;
    store.link_interaction(
        &first.id,
        &CommitSha::new(second_old.to_string()),
        "generated",
    )?;
    trust_legacy_source(&temp.path().join("index.db"), &first.id, second_old)?;
    store.link_interaction(
        &first.id,
        &CommitSha::new(third_old.to_string()),
        "generated",
    )?;
    trust_legacy_source(&temp.path().join("index.db"), &first.id, third_old)?;
    store.link_interaction(
        &second.id,
        &CommitSha::new(second_old.to_string()),
        "generated",
    )?;
    trust_legacy_source(&temp.path().join("index.db"), &second.id, second_old)?;

    // A replacement root is reachable from HEAD while both old commits are no
    // longer ancestors, matching the relevant post-rewrite observation.
    let replacement = replacement_root(&repo)?;
    let amend = format!("{} {}\n", first_old, replacement);
    assert_eq!(
        rewrite::apply(&repo, &mut store, "amend", amend.as_bytes())?,
        1
    );
    assert_eq!(
        rewrite::apply(&repo, &mut store, "amend", amend.as_bytes())?,
        1
    );

    // A rebase/fixup-style squash can map three old commits to one new one.
    let rebase = format!(
        "{} {}\n{} {}\n{} {}\n",
        first_old, replacement, second_old, replacement, third_old, replacement
    );
    assert_eq!(
        rewrite::apply(&repo, &mut store, "rebase", rebase.as_bytes())?,
        4
    );
    assert_eq!(
        rewrite::apply(&repo, &mut store, "rebase", rebase.as_bytes())?,
        4
    );

    let first_links = store.get_artifact_links(&first.id)?;
    assert!(first_links
        .iter()
        .any(|l| l.git_commit_hash.as_str() == first_old.to_string()));
    assert!(first_links
        .iter()
        .any(|l| l.git_commit_hash.as_str() == second_old.to_string()));
    assert!(first_links
        .iter()
        .any(|l| l.git_commit_hash.as_str() == third_old.to_string()));
    assert!(first_links.iter().any(|l| {
        l.git_commit_hash.as_str() == replacement.to_string() && l.link_type == "rewrite_exact"
    }));
    let db = rusqlite::Connection::open(temp.path().join("index.db"))?;
    let event_count: u64 = db.query_row(
        "SELECT COUNT(*) FROM derivation_events WHERE interaction_id=?1 AND target_commit=?2 AND relation='rewrite_exact'",
        [&first.id.to_string(), &replacement.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(
        event_count, 3,
        "one immutable rewrite event per old endpoint"
    );
    assert_eq!(
        store
            .get_artifact_links(&second.id)?
            .iter()
            .filter(|l| l.git_commit_hash.as_str() == replacement.to_string()
                && l.link_type == "rewrite_exact")
            .count(),
        1
    );
    Ok(())
}

#[test]
fn inbox_is_replayable_but_never_accepts_world_readable_history() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let inbox = temp.path().join("rewrite-inbox");
    let raw =
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
    let path = rewrite::persist_inbox(&inbox, "amend", raw)?;
    #[cfg(unix)]
    {
        assert_eq!(fs::metadata(&inbox)?.permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(rewrite::persist_inbox(&inbox, "amend", raw)?, path);

    #[cfg(unix)]
    {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
        assert!(rewrite::persist_inbox(&inbox, "amend", raw).is_err());
    }
    Ok(())
}

#[test]
fn v5_rewrite_events_project_import_and_remain_remote_assertions() -> anyhow::Result<()> {
    const DESTINATION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let mut source = CvcStore::open(temp.path().join("source.db"))?;
    let old = commit(&repo, "old")?;
    let item = interaction();
    persist(&source, &item)?;
    source.link_interaction(&item.id, &CommitSha::new(old.to_string()), "generated")?;
    trust_legacy_source(&temp.path().join("source.db"), &item.id, old)?;
    source.share_conversation_for_remote(
        &item.conversation_id,
        DESTINATION,
        FutureSharePolicy::Private,
    )?;
    let new = replacement_root(&repo)?;
    rewrite::apply(
        &repo,
        &mut source,
        "amend",
        format!("{old} {new}\n").as_bytes(),
    )?;

    let projection = sync::push_projection_to_ref(&repo, &source, "", DESTINATION)?;
    let oid = match projection {
        sync::ProjectionResult::Candidate { oid, .. } => oid,
        sync::ProjectionResult::NoChanges => unreachable!("new source must project"),
    };
    repo.reference("refs/cvc/rewrite-v5", oid, true, "test remote")?;
    let tree = repo.find_commit(oid)?.tree()?;
    assert_eq!(
        repo.find_blob(tree.get_name("FORMAT").unwrap().id())?
            .content(),
        b"5"
    );
    assert!(tree.get_name("events").is_some());

    let imported = CvcStore::open(temp.path().join("imported.db"))?;
    sync::pull_from_ref_with_limits(
        &repo,
        &imported,
        "refs/cvc/rewrite-v5",
        Some(DESTINATION),
        sync::SyncReadLimits::default(),
    )?;
    let imported_events: u64 = rusqlite::Connection::open(temp.path().join("imported.db"))?
        .query_row("SELECT COUNT(*) FROM derivation_events", [], |row| {
            row.get(0)
        })?;
    assert_eq!(imported_events, 1);
    // Imported proof is destination-scoped evidence, never authority to publish
    // it onward from this clone.
    assert!(matches!(
        sync::push_projection_to_ref(&repo, &imported, "refs/cvc/rewrite-v5", DESTINATION)?,
        sync::ProjectionResult::NoChanges
    ));
    Ok(())
}

#[test]
fn rewrite_of_a_rewrite_cites_the_actual_source_event() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let mut store = CvcStore::open(temp.path().join("index.db"))?;
    let old = commit(&repo, "old")?;
    let item = interaction();
    persist(&store, &item)?;
    store.link_interaction(&item.id, &CommitSha::new(old.to_string()), "generated")?;
    trust_legacy_source(&temp.path().join("index.db"), &item.id, old)?;
    let middle = replacement_root(&repo)?;
    rewrite::apply(
        &repo,
        &mut store,
        "amend",
        format!("{old} {middle}\n").as_bytes(),
    )?;
    let first_id: String = rusqlite::Connection::open(temp.path().join("index.db"))?.query_row(
        "SELECT event_id FROM derivation_events WHERE target_commit=?1",
        [middle.to_string()],
        |r| r.get(0),
    )?;
    let newest = commit(&repo, "new")?;
    rewrite::apply(
        &repo,
        &mut store,
        "amend",
        format!("{middle} {newest}\n").as_bytes(),
    )?;
    let source_json: String = rusqlite::Connection::open(temp.path().join("index.db"))?.query_row(
        "SELECT source_event_ids FROM derivation_events WHERE target_commit=?1",
        [newest.to_string()],
        |r| r.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&source_json)?,
        vec![first_id]
    );
    assert!(!source_json.contains("endpoint:"));
    Ok(())
}

#[test]
fn inbox_quota_counts_only_active_json_and_exact_replay_wins_when_full() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let inbox = temp.path().join("inbox");
    let raw =
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
    let canonical = rewrite::persist_inbox(&inbox, "amend", raw)?;
    for index in 0..63 {
        let path = inbox.join(format!("filler-{index}.json"));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        drop(file);
    }
    fs::create_dir(inbox.join("quarantine"))?;
    for index in 0..80 {
        fs::write(inbox.join("quarantine").join(format!("{index}.bad")), b"x")?;
    }
    assert_eq!(rewrite::persist_inbox(&inbox, "amend", raw)?, canonical);
    let other =
        b"cccccccccccccccccccccccccccccccccccccccc dddddddddddddddddddddddddddddddddddddddd\n";
    assert!(rewrite::persist_inbox(&inbox, "amend", other).is_err());

    let byte_inbox = temp.path().join("byte-inbox");
    fs::create_dir(&byte_inbox)?;
    #[cfg(unix)]
    fs::set_permissions(&byte_inbox, fs::Permissions::from_mode(0o700))?;
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(byte_inbox.join("large.json"))?
        .set_len(32 * 1024 * 1024)?;
    assert!(rewrite::persist_inbox(&byte_inbox, "amend", raw).is_err());

    let bounded = temp.path().join("bounded-quarantine");
    for index in 1..=40 {
        let record = format!("{:040x} bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n", index);
        let path = rewrite::persist_inbox(&bounded, "amend", record.as_bytes())?;
        rewrite::quarantine_inbox(&path)?;
    }
    assert!(fs::read_dir(bounded.join("quarantine"))?.count() <= 32);
    assert!(rewrite::persist_inbox(&bounded, "amend", raw)?.exists());
    Ok(())
}

#[test]
fn transient_database_busy_keeps_entry_and_branch_switch_replay_succeeds() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let repo = Repository::init(temp.path())?;
    let db_path = temp.path().join("index.db");
    let mut store = CvcStore::open(&db_path)?;
    let old = commit(&repo, "old")?;
    let item = interaction();
    persist(&store, &item)?;
    store.link_interaction(&item.id, &CommitSha::new(old.to_string()), "generated")?;
    trust_legacy_source(&db_path, &item.id, old)?;
    let new = replacement_root(&repo)?;
    let raw = format!("{old} {new}\n");
    rewrite::validate_initial_delivery(&repo, "amend", raw.as_bytes())?;
    let path = rewrite::persist_inbox(&temp.path().join("inbox"), "amend", raw.as_bytes())?;

    let lock = rusqlite::Connection::open(&db_path)?;
    lock.execute_batch("BEGIN IMMEDIATE")?;
    assert!(rewrite::apply_inbox(&repo, &mut store, &path).is_err());
    assert!(path.exists(), "transient failure must remain retryable");
    lock.execute_batch("ROLLBACK")?;

    repo.set_head_detached(old)?;
    assert_eq!(rewrite::apply_inbox(&repo, &mut store, &path)?, 1);
    assert!(!path.exists());
    assert!(store.get_artifact_links(&item.id)?.iter().any(|link| {
        link.git_commit_hash.as_str() == new.to_string() && link.link_type == "rewrite_exact"
    }));
    Ok(())
}
