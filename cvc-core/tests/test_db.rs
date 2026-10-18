use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::*;
use cvc_core::privacy::{McpCapture, PreparedPolicy};
use rusqlite::Connection;
use std::str::FromStr;
use tempfile::TempDir;

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
        .map(|_| ())
    }
}

#[test]
fn public_link_api_cannot_create_trusted_source_even_after_reopen() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("index.db");
    let id = InteractionId::new();
    let interaction = Interaction {
        id: id.clone(),
        conversation_id: "sealed-link".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "untrusted public fixture".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    let sha = CommitSha::new("a".repeat(40));
    {
        let store = CvcStore::open(&path)?;
        store.create_interaction(&interaction)?;
        store.link_interaction(&id, &sha, "generated")?;
        assert!(!store.has_locally_observed_source(&id, &sha)?);
    }
    let reopened = CvcStore::open(&path)?;
    assert!(!reopened.has_locally_observed_source(&id, &sha)?);
    Ok(())
}

#[test]
fn test_db_workflow() -> anyhow::Result<()> {
    // 1. Setup in-memory DB
    let store = CvcStore::open(":memory:")?;
    store.init()?;

    // 2. Create Conversation
    let conv_id = "test-conv-1";
    let conv = Conversation {
        id: conv_id.to_string(),
        title: "Test Conversation".to_string(),
        created_at: Utc::now(),
    };
    store.create_conversation(&conv)?;

    // 3. Create Interaction
    let inter_id = InteractionId::from_str("550e8400-e29b-41d4-a716-446655440000")?;
    let interaction = Interaction {
        id: inter_id.clone(),
        conversation_id: conv_id.to_string(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "Hello World".to_string(),
        model_name: Some("gpt-4".to_string()),
        model_cot: None,
        model_response: Some("Hi there".to_string()),
        source_request_id: None,
    };
    store.create_interaction(&interaction)?;

    let fetched_conv = store.get_conversation(conv_id)?;
    assert!(fetched_conv.is_some());

    let fetched_inter = store.get_interaction(&inter_id)?;
    assert!(fetched_inter.is_some());
    assert_eq!(fetched_inter.unwrap().user_prompt, "Hello World");

    // 4. Test Floating Nodes
    let floating = store.get_floating_interactions()?;
    assert_eq!(floating.len(), 1);
    assert_eq!(floating[0].id, inter_id);

    // 5. Link Artifact
    let commit_sha = CommitSha::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    store.link_interaction(&inter_id, &commit_sha, "generated")?;

    let links = store.get_artifact_links(&inter_id)?;
    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0].git_commit_hash.as_str(),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );

    // 6. Test Floating Nodes Empty
    let floating_after = store.get_floating_interactions()?;
    assert_eq!(floating_after.len(), 0);

    Ok(())
}

#[test]
fn remote_redaction_requires_exact_authority_and_is_idempotent() -> anyhow::Result<()> {
    let store = CvcStore::open(":memory:")?;
    let id = InteractionId::new();
    let interaction = Interaction {
        id: id.clone(),
        conversation_id: "redact-authority".into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "secret".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    store.create_interaction(&interaction)?;
    assert!(store
        .authorize_and_tombstone_remote(&id, "destination-a", TombstoneReasonCode::Security)
        .is_err());
    assert!(store.get_interaction(&id)?.is_some());
    store.share_conversation_for_remote(
        "redact-authority",
        "destination-a",
        FutureSharePolicy::Private,
    )?;
    let first = store.authorize_and_tombstone_remote(
        &id,
        "destination-a",
        TombstoneReasonCode::Security,
    )?;
    assert!(store.get_interaction(&id)?.is_none());
    let retry = store.authorize_and_tombstone_remote(
        &id,
        "destination-a",
        TombstoneReasonCode::Security,
    )?;
    assert_eq!(first, retry);
    assert_eq!(store.tombstones_for_projection("destination-a")?.len(), 1);
    assert!(store
        .authorize_and_tombstone_remote(&id, "destination-a", TombstoneReasonCode::Retention)
        .is_err());
    Ok(())
}

#[test]
fn init_migrates_a_pre_0003_artifact_links_table_without_losing_links() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let path = temp_dir.path().join("legacy.db");
    let legacy = Connection::open(&path)?;
    legacy.execute_batch(
        "CREATE TABLE artifact_links (
            interaction_id TEXT,
            git_commit_hash TEXT,
            link_type TEXT,
            PRIMARY KEY (interaction_id, git_commit_hash)
        );
        INSERT INTO artifact_links VALUES ('old-interaction', 'old-commit', NULL);",
    )?;
    drop(legacy);

    // `open` is the runtime hook/CLI path and must migrate before any query.
    let _store = CvcStore::open(&path)?;

    let conn = Connection::open(&path)?;
    let linked_by: Option<String> = conn.query_row(
        "SELECT linked_by FROM artifact_links WHERE interaction_id = 'old-interaction'",
        [],
        |row| row.get(0),
    )?;
    let link_type: String = conn.query_row(
        "SELECT link_type FROM artifact_links WHERE interaction_id = 'old-interaction'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(link_type, "generated");
    assert_eq!(linked_by, None);
    Ok(())
}

#[test]
fn init_converges_partially_applied_artifact_link_schema() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let path = temp_dir.path().join("partial.db");
    let legacy = Connection::open(&path)?;
    legacy.execute_batch(
        "CREATE TABLE artifact_links (
            interaction_id TEXT,
            git_commit_hash TEXT,
            link_type TEXT,
            linked_by TEXT,
            PRIMARY KEY (interaction_id, git_commit_hash)
        );
        INSERT INTO artifact_links VALUES ('null-type', 'one', NULL, 'old@example.com');
        INSERT INTO artifact_links VALUES ('historical', 'two', 'verified', NULL);",
    )?;
    drop(legacy);

    let _store = CvcStore::open(&path)?;
    let conn = Connection::open(&path)?;
    let rows: Vec<(String, String, Option<String>)> = conn
        .prepare("SELECT interaction_id, link_type, linked_by FROM artifact_links ORDER BY interaction_id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        rows,
        vec![
            ("historical".into(), "verified".into(), None),
            (
                "null-type".into(),
                "generated".into(),
                Some("old@example.com".into())
            ),
        ]
    );
    let link_type_info: (i64, Option<String>) = conn.query_row(
        "SELECT \"notnull\", dflt_value FROM pragma_table_info('artifact_links') WHERE name = 'link_type'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(link_type_info.0, 1);
    assert!(link_type_info
        .1
        .as_deref()
        .is_some_and(|value| value.contains("generated")));
    Ok(())
}

#[test]
fn automatic_link_conflicts_converge_only_when_compatible() -> anyhow::Result<()> {
    let store = CvcStore::open(":memory:")?;
    let id = InteractionId::new();
    let sha = CommitSha::new("b".repeat(40));
    assert_eq!(
        store.link_automatic_interactions(
            std::slice::from_ref(&id),
            &sha,
            "generated",
            Some("a@example.com")
        )?,
        1
    );
    assert_eq!(
        store.link_automatic_interactions(
            std::slice::from_ref(&id),
            &sha,
            "generated",
            Some("a@example.com")
        )?,
        0
    );

    let null_id = InteractionId::new();
    store.link_interaction(&null_id, &sha, "generated")?;
    assert_eq!(
        store.link_automatic_interactions(
            std::slice::from_ref(&null_id),
            &sha,
            "generated",
            Some("upgraded@example.com")
        )?,
        0
    );
    assert_eq!(
        store.get_artifact_links(&null_id)?[0].linked_by.as_deref(),
        Some("upgraded@example.com")
    );

    assert!(store
        .link_automatic_interactions(
            std::slice::from_ref(&id),
            &sha,
            "temporal",
            Some("a@example.com")
        )
        .is_err());
    assert!(store
        .link_automatic_interactions(
            std::slice::from_ref(&id),
            &sha,
            "generated",
            Some("b@example.com")
        )
        .is_err());
    assert_eq!(store.get_artifact_links(&id)?.len(), 1);

    // A conflict later in a batch must roll back links selected earlier in the
    // same linker decision; otherwise a conversation could be half-bound.
    let would_be_inserted = InteractionId::new();
    let conflicting = InteractionId::new();
    store.link_automatic_interactions(
        std::slice::from_ref(&conflicting),
        &sha,
        "generated",
        Some("a@example.com"),
    )?;
    assert!(store
        .link_automatic_interaction_batch(
            &[
                (&would_be_inserted, "generated",),
                (&conflicting, "temporal")
            ],
            &sha,
            Some("a@example.com"),
        )
        .is_err());
    assert!(store.get_artifact_links(&would_be_inserted)?.is_empty());
    Ok(())
}

fn capture_with_worktree(
    store: &CvcStore,
    conversation: &str,
    capture_worktree: String,
) -> anyhow::Result<InteractionId> {
    let id = InteractionId::new();
    store.capture_mcp(McpCapture::new(
        Conversation {
            id: conversation.into(),
            title: "worktree fixture".into(),
            created_at: Utc::now(),
        },
        Interaction {
            id: id.clone(),
            conversation_id: conversation.into(),
            parent_id: None,
            timestamp: Utc::now(),
            author: Author::Agent,
            user_prompt: "scoped".into(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        },
        Vec::new(),
        Vec::new(),
        PreparedPolicy::built_ins_only(),
        capture_worktree,
    ))?;
    Ok(id)
}

#[test]
fn floating_query_scopes_to_capture_worktree_and_keeps_legacy_rows() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("index.db");
    let store = CvcStore::open(&path)?;
    store.init()?;
    let mine = capture_with_worktree(&store, "mine", "a".repeat(64))?;
    let sibling = capture_with_worktree(&store, "sibling", "b".repeat(64))?;
    let legacy = capture_with_worktree(&store, "legacy", "c".repeat(64))?;
    Connection::open(&path)?.execute(
        "UPDATE interactions SET capture_worktree=NULL WHERE id=?1",
        [legacy.as_str()],
    )?;

    let scoped: Vec<_> = store
        .get_floating_interactions_for_worktree(&"a".repeat(64))?
        .into_iter()
        .map(|interaction| interaction.id)
        .collect();
    assert!(scoped.contains(&mine));
    assert!(scoped.contains(&legacy), "legacy rows stay eligible");
    assert!(
        !scoped.contains(&sibling),
        "another worktree's rows are excluded"
    );
    // The unscoped query keeps returning every floating node for status views.
    assert_eq!(store.get_floating_interactions()?.len(), 3);

    // The stored value is exactly the fingerprint the capture supplied, and
    // malformed origins are rejected by the schema itself.
    let connection = Connection::open(&path)?;
    let stored: String = connection.query_row(
        "SELECT capture_worktree FROM interactions WHERE id=?1",
        [mine.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(stored, "a".repeat(64));
    assert!(connection
        .execute(
            "UPDATE interactions SET capture_worktree='not-hex' WHERE id=?1",
            [mine.as_str()],
        )
        .is_err());
    Ok(())
}

#[test]
fn conversation_summaries_report_activity_and_destination_state() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("index.db");
    let store = CvcStore::open(&path)?;
    store.init()?;
    let fingerprint = "7".repeat(64);

    // Older conversation: two thoughts, one linked, shared and published.
    let older_first = capture_with_worktree(&store, "older", "a".repeat(64))?;
    let older_second = capture_with_worktree(&store, "older", "a".repeat(64))?;
    store.link_interaction(&older_first, &CommitSha::new("c".repeat(40)), "generated")?;
    let snapshot = store.share_snapshot("older")?;
    store.share_exact_snapshot("older", &fingerprint, &snapshot, FutureSharePolicy::Private)?;
    let connection = Connection::open(&path)?;
    connection.execute(
        "INSERT INTO publications(interaction_id, remote_fingerprint, state, updated_at) VALUES(?1, ?2, 'published', 0)",
        rusqlite::params![older_first.as_str(), fingerprint.as_str()],
    )?;
    // Newer conversation: one thought, unshared.
    let newer = capture_with_worktree(&store, "newer", "b".repeat(64))?;
    connection.execute(
        "UPDATE interactions SET timestamp = timestamp + 100 WHERE id = ?1",
        [newer.as_str()],
    )?;

    let summaries = store.list_conversation_summaries(Some(&fingerprint), 10)?;
    assert_eq!(
        summaries.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["newer", "older"],
        "most recent activity first"
    );
    let older = &summaries[1];
    assert_eq!((older.thoughts, older.linked, older.published), (2, 1, 1));
    assert!(older.shared);
    assert!(older.first_activity <= older.last_activity);
    let newer_summary = &summaries[0];
    assert_eq!((newer_summary.thoughts, newer_summary.linked), (1, 0));
    assert!(!newer_summary.shared);
    assert_eq!(newer_summary.published, 0);

    // Without a destination, share state is uniformly absent.
    let neutral = store.list_conversation_summaries(None, 10)?;
    assert!(neutral.iter().all(|s| !s.shared && s.published == 0));
    // The limit caps the newest slice.
    assert_eq!(store.list_conversation_summaries(None, 1)?.len(), 1);
    let _ = older_second;
    Ok(())
}

fn summary_of(
    store: &CvcStore,
    destination: Option<&str>,
    conversation: &str,
) -> anyhow::Result<cvc_core::db::ConversationSummary> {
    store
        .list_conversation_summaries(destination, 100)?
        .into_iter()
        .find(|summary| summary.id == conversation)
        .ok_or_else(|| anyhow::anyhow!("no summary for {conversation}"))
}

#[test]
fn an_unshared_conversation_is_private_again_although_its_policy_row_remains() -> anyhow::Result<()>
{
    let store = CvcStore::open(":memory:")?;
    let fingerprint = "7".repeat(64);
    capture_with_worktree(&store, "round-trip", "a".repeat(64))?;
    capture_with_worktree(&store, "round-trip", "a".repeat(64))?;

    let before = summary_of(&store, Some(&fingerprint), "round-trip")?;
    assert!(!before.shared);
    assert_eq!(
        (before.shared_count, before.published, before.unconfirmed),
        (0, 0, 0)
    );
    assert_eq!(before.share_future, None);

    let snapshot = store.share_snapshot("round-trip")?;
    store.share_exact_snapshot(
        "round-trip",
        &fingerprint,
        &snapshot,
        FutureSharePolicy::Shared,
    )?;
    let shared = summary_of(&store, Some(&fingerprint), "round-trip")?;
    assert!(shared.shared);
    assert_eq!(shared.shared_count, 2);
    assert_eq!(shared.share_future, Some(true));

    assert_eq!(
        store.unshare_conversation_for_remote("round-trip", &fingerprint)?,
        2
    );
    // The conversation-level policy row survives an unshare, with future
    // turns private. It is a policy, not a share: nothing is shared now.
    let after = summary_of(&store, Some(&fingerprint), "round-trip")?;
    assert!(!after.shared, "{after:?}");
    assert_eq!(
        (after.shared_count, after.published, after.unconfirmed),
        (0, 0, 0)
    );
    assert_eq!(after.share_future, Some(false));

    // It can be shared again, and shows as shared again.
    let snapshot = store.share_snapshot("round-trip")?;
    store.share_exact_snapshot(
        "round-trip",
        &fingerprint,
        &snapshot,
        FutureSharePolicy::Private,
    )?;
    let again = summary_of(&store, Some(&fingerprint), "round-trip")?;
    assert!(again.shared);
    assert_eq!(again.shared_count, 2);
    Ok(())
}

#[test]
fn share_state_counts_shared_interactions_only_and_honours_the_wildcard() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("index.db");
    let store = CvcStore::open(&path)?;
    let fingerprint = "7".repeat(64);
    let other = "8".repeat(64);
    let worktree = "a".repeat(64);
    let ids: Vec<_> = (0..6)
        .map(|_| capture_with_worktree(&store, "mixed", worktree.clone()))
        .collect::<anyhow::Result<_>>()?;
    let connection = Connection::open(&path)?;
    let share = |id: &InteractionId, destination: &str| {
        connection.execute(
            "INSERT INTO interaction_shares(interaction_id, remote_fingerprint) VALUES(?1, ?2)",
            rusqlite::params![id.as_str(), destination],
        )
    };
    let publish = |id: &InteractionId, destination: &str, state: &str| {
        connection.execute(
            "INSERT INTO publications(interaction_id, remote_fingerprint, state, updated_at) VALUES(?1, ?2, ?3, 0)",
            rusqlite::params![id.as_str(), destination, state],
        )
    };
    // ids[0]: shared and published. ids[1]: shared, pending. ids[2]: shared
    // through the wildcard only, unknown. ids[3]: shared both ways (counted
    // once), never attempted. ids[4]: shared for another destination only.
    // ids[5]: not shared, but published (as a pull records for what it imports).
    share(&ids[0], &fingerprint)?;
    publish(&ids[0], &fingerprint, "published")?;
    share(&ids[1], &fingerprint)?;
    publish(&ids[1], &fingerprint, "pending")?;
    share(&ids[2], "*")?;
    publish(&ids[2], &fingerprint, "unknown")?;
    share(&ids[3], &fingerprint)?;
    share(&ids[3], "*")?;
    share(&ids[4], &other)?;
    publish(&ids[4], &other, "published")?;
    publish(&ids[5], &fingerprint, "published")?;

    let summary = summary_of(&store, Some(&fingerprint), "mixed")?;
    assert_eq!(summary.thoughts, 6);
    assert!(summary.shared);
    assert_eq!(
        summary.shared_count, 4,
        "ids 0-3, the wildcard counted once"
    );
    assert_eq!(
        summary.published, 1,
        "ids[5] is published but not shared, so it is not counted"
    );
    assert_eq!(summary.unconfirmed, 2, "pending and unknown");

    // Another destination sees only its own share and publication.
    let elsewhere = summary_of(&store, Some(&other), "mixed")?;
    assert_eq!(
        (
            elsewhere.shared_count,
            elsewhere.published,
            elsewhere.unconfirmed
        ),
        (3, 1, 0),
        "ids 2 and 3 through the wildcard, plus its own ids[4]"
    );

    // Without a destination nothing is shared, the wildcard included.
    let neutral = summary_of(&store, None, "mixed")?;
    assert!(!neutral.shared);
    assert_eq!(
        (neutral.shared_count, neutral.published, neutral.unconfirmed),
        (0, 0, 0)
    );
    assert_eq!(neutral.share_future, None);
    Ok(())
}

fn capture_with_context(
    store: &CvcStore,
    conversation: &str,
    paths: &[&str],
) -> anyhow::Result<InteractionId> {
    let id = InteractionId::new();
    store.capture_mcp(McpCapture::new(
        Conversation {
            id: conversation.into(),
            title: format!("{conversation} title"),
            created_at: Utc::now(),
        },
        Interaction {
            id: id.clone(),
            conversation_id: conversation.into(),
            parent_id: None,
            timestamp: Utc::now(),
            author: Author::Agent,
            user_prompt: "context".into(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        },
        paths
            .iter()
            .map(|path| ContextItem {
                id: None,
                interaction_id: id.clone(),
                file_path: (*path).into(),
                git_blob_sha: None,
                dirty_patch: None,
                start_line: None,
                end_line: None,
            })
            .collect(),
        Vec::new(),
        PreparedPolicy::built_ins_only(),
        "a".repeat(64),
    ))?;
    Ok(id)
}

#[test]
fn a_conversation_is_imported_only_when_every_interaction_was() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("index.db");
    let store = CvcStore::open(&path)?;
    let worktree = "a".repeat(64);
    let fingerprint = "7".repeat(64);
    let all = [
        capture_with_worktree(&store, "all-imported", worktree.clone())?,
        capture_with_worktree(&store, "all-imported", worktree.clone())?,
    ];
    let mixed = [
        capture_with_worktree(&store, "mixed", worktree.clone())?,
        capture_with_worktree(&store, "mixed", worktree.clone())?,
    ];
    capture_with_worktree(&store, "local-only", worktree.clone())?;
    let connection = Connection::open(&path)?;
    // What a pull does to the rows it imports.
    for id in all.iter().chain(&mixed[..1]) {
        connection.execute(
            "UPDATE interactions SET capture_source='sync_import' WHERE id=?1",
            [id.as_str()],
        )?;
    }
    connection.execute(
        "INSERT INTO conversations(id, title, created_at) VALUES('empty', 'no interactions', 0)",
        [],
    )?;

    for destination in [None, Some(fingerprint.as_str())] {
        let imported: Vec<_> = store
            .list_conversation_summaries(destination, 100)?
            .into_iter()
            .map(|summary| (summary.id, summary.imported))
            .collect();
        for (id, expected) in [
            ("all-imported", true),
            ("mixed", false),
            ("local-only", false),
            ("empty", false),
        ] {
            assert!(
                imported.contains(&(id.to_string(), expected)),
                "{id} should be imported={expected}: {imported:?}"
            );
        }
        assert_eq!(imported.len(), 4);
    }
    // The detail view reports the same thing.
    for (id, expected) in [("all-imported", true), ("mixed", false), ("empty", false)] {
        let details = store.conversation_details(id, None)?.unwrap();
        assert_eq!(details.summary.imported, expected, "{id}");
    }
    Ok(())
}

#[test]
fn details_list_the_most_used_paths_and_the_exact_share_snapshot() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("index.db");
    let store = CvcStore::open(&path)?;
    let fingerprint = "7".repeat(64);
    let extras: Vec<String> = (0..14).map(|n| format!("extra/f{n:02}.rs")).collect();
    let extra_refs: Vec<&str> = extras.iter().map(String::as_str).collect();

    // `src/a.rs` is in three interactions (twice in the first, which counts
    // once), `src/b.rs` in two, the rest in one each.
    let first = capture_with_context(
        &store,
        "details",
        &["src/a.rs", "src/a.rs", "src/b.rs", "README.md"],
    )?;
    let second = capture_with_context(&store, "details", &["src/a.rs", "src/b.rs"])?;
    let third = capture_with_context(&store, "details", &["src/a.rs", "docs/x.md", "docs/y.md"])?;
    let fourth = capture_with_context(&store, "details", &extra_refs)?;
    capture_with_context(&store, "elsewhere", &["other/only.rs", "src/a.rs"])?;
    let connection = Connection::open(&path)?;
    // Rows that must never be listed or counted.
    connection.execute(
        "INSERT INTO context_items(interaction_id, file_path) VALUES(?1, '')",
        [first.as_str()],
    )?;
    connection.execute(
        "INSERT INTO context_items(interaction_id, file_path) VALUES(?1, NULL)",
        [first.as_str()],
    )?;

    let details = store
        .conversation_details("details", Some(&fingerprint))?
        .expect("the conversation exists");
    let listed: Vec<_> = details
        .context_paths
        .iter()
        .map(|used| (used.path.as_str(), used.interactions))
        .collect();
    // Most used first, then byte order: uppercase sorts before lowercase.
    assert_eq!(
        listed,
        vec![
            ("src/a.rs", 3),
            ("src/b.rs", 2),
            ("README.md", 1),
            ("docs/x.md", 1),
            ("docs/y.md", 1),
            ("extra/f00.rs", 1),
            ("extra/f01.rs", 1),
            ("extra/f02.rs", 1),
            ("extra/f03.rs", 1),
            ("extra/f04.rs", 1),
        ]
    );
    assert_eq!(
        details.context_paths.len(),
        cvc_core::db::CONVERSATION_DETAIL_PATH_LIMIT
    );
    assert_eq!(details.context_path_total, 5 + 14, "all distinct paths");
    assert!(!listed.iter().any(|(path, _)| path.starts_with("other/")));

    // The snapshot is exactly what the share challenge will carry.
    assert_eq!(
        details.snapshot_count,
        store.share_snapshot("details")?.len()
    );
    assert_eq!(details.snapshot_count, 4);
    assert_eq!(details.summary.thoughts, 4);
    assert_eq!((details.snapshot_shared, details.snapshot_new), (0, 4));

    // Two interactions shared, one of them through the wildcard.
    connection.execute(
        "INSERT INTO interaction_shares(interaction_id, remote_fingerprint) VALUES(?1, ?2)",
        rusqlite::params![second.as_str(), fingerprint.as_str()],
    )?;
    connection.execute(
        "INSERT INTO interaction_shares(interaction_id, remote_fingerprint) VALUES(?1, '*')",
        [third.as_str()],
    )?;
    let details = store
        .conversation_details("details", Some(&fingerprint))?
        .unwrap();
    assert_eq!(
        details.snapshot_count,
        store.share_snapshot("details")?.len()
    );
    assert_eq!((details.snapshot_shared, details.snapshot_new), (2, 2));
    // Without a destination nothing is shared.
    let neutral = store.conversation_details("details", None)?.unwrap();
    assert_eq!((neutral.snapshot_shared, neutral.snapshot_new), (0, 4));
    assert_eq!(neutral.summary.share_future, None);
    let _ = fourth;

    // The summary inside is the row the listing shows.
    let listed = summary_of(&store, Some(&fingerprint), "details")?;
    assert_eq!(details.summary, listed);
    Ok(())
}

#[test]
fn details_report_share_future_per_destination_and_handle_missing_and_empty_conversations(
) -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("index.db");
    let store = CvcStore::open(&path)?;
    let fingerprint = "7".repeat(64);
    let other = "8".repeat(64);
    capture_with_worktree(&store, "future", "a".repeat(64))?;
    let share_future = |destination: Option<&str>| -> anyhow::Result<Option<bool>> {
        Ok(store
            .conversation_details("future", destination)?
            .unwrap()
            .summary
            .share_future)
    };

    assert_eq!(share_future(Some(&fingerprint))?, None);
    let snapshot = store.share_snapshot("future")?;
    store.share_exact_snapshot(
        "future",
        &fingerprint,
        &snapshot,
        FutureSharePolicy::Private,
    )?;
    assert_eq!(share_future(Some(&fingerprint))?, Some(false));
    store.share_exact_snapshot("future", &fingerprint, &snapshot, FutureSharePolicy::Shared)?;
    assert_eq!(share_future(Some(&fingerprint))?, Some(true));
    // Another destination and no destination decide nothing for this one.
    assert_eq!(share_future(Some(&other))?, None);
    assert_eq!(share_future(None)?, None);
    store.unshare_conversation_for_remote("future", &fingerprint)?;
    assert_eq!(share_future(Some(&fingerprint))?, Some(false));

    assert!(store.conversation_details("no-such", None)?.is_none());

    Connection::open(&path)?.execute(
        "INSERT INTO conversations(id, title, created_at) VALUES('empty', 'nothing yet', 42)",
        [],
    )?;
    let empty = store
        .conversation_details("empty", Some(&fingerprint))?
        .unwrap();
    assert_eq!(empty.snapshot_count, store.share_snapshot("empty")?.len());
    assert_eq!(
        (
            empty.snapshot_count,
            empty.snapshot_shared,
            empty.snapshot_new
        ),
        (0, 0, 0)
    );
    assert_eq!(
        (empty.summary.first_activity, empty.summary.last_activity),
        (42, 42)
    );
    assert!(empty.context_paths.is_empty());
    assert_eq!(empty.context_path_total, 0);
    Ok(())
}

#[test]
fn destination_counts_follow_the_projection_sharing_rule_minus_its_state_filter(
) -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("index.db");
    let store = CvcStore::open(&path)?;
    let fingerprint = "7".repeat(64);
    let other = "8".repeat(64);
    let worktree = "a".repeat(64);

    let none = store.destination_publication_counts(&fingerprint)?;
    assert_eq!(none, Default::default());
    assert_eq!(none.unpublished(), 0);

    let ids: Vec<_> = (0..8)
        .map(|n| capture_with_worktree(&store, &format!("c{}", n % 2), worktree.clone()))
        .collect::<anyhow::Result<_>>()?;
    let connection = Connection::open(&path)?;
    let share = |id: &InteractionId, destination: &str| {
        connection.execute(
            "INSERT INTO interaction_shares(interaction_id, remote_fingerprint) VALUES(?1, ?2)",
            rusqlite::params![id.as_str(), destination],
        )
    };
    // ids[0] published; ids[1] pending; ids[2] unknown; ids[3] never attempted;
    // ids[4] shared through the wildcard and also directly (counted once) and
    // published; ids[5] shared for another destination only; ids[6] not shared
    // but published (what a pull records); ids[7] not shared at all.
    for id in &ids[..4] {
        share(id, &fingerprint)?;
    }
    share(&ids[4], "*")?;
    share(&ids[4], &fingerprint)?;
    share(&ids[5], &other)?;
    store.mark_publication(&ids[..1], &fingerprint, PublicationState::Published)?;
    store.mark_publication(&ids[1..2], &fingerprint, PublicationState::Pending)?;
    store.mark_publication(&ids[2..3], &fingerprint, PublicationState::Unknown)?;
    store.mark_publication(&ids[4..5], &fingerprint, PublicationState::Published)?;
    store.mark_publication(&ids[5..6], &other, PublicationState::Published)?;
    store.mark_publication(&ids[6..7], &fingerprint, PublicationState::Published)?;

    let counts = store.destination_publication_counts(&fingerprint)?;
    assert_eq!(counts.shared, 5, "ids 0-4, ids[4] counted once");
    assert_eq!(counts.published, 2, "ids[6] is published but not shared");
    assert_eq!(counts.unconfirmed, 2, "pending and unknown");
    assert_eq!(
        counts.unpublished(),
        3,
        "pending, unknown and never attempted"
    );

    // It is the projection's sharing set, with the state filter taken off:
    // the projection leaves out only what is `unknown`.
    let projected = store.projection_interaction_ids(&fingerprint)?.len() as i64;
    assert_eq!(counts.shared, projected + 1);

    // The summaries add up to the same totals.
    let summaries = store.list_conversation_summaries(Some(&fingerprint), 10)?;
    assert_eq!(
        summaries.iter().map(|s| s.shared_count).sum::<i64>(),
        counts.shared
    );
    assert_eq!(
        summaries.iter().map(|s| s.published).sum::<i64>(),
        counts.published
    );
    assert_eq!(
        summaries.iter().map(|s| s.unconfirmed).sum::<i64>(),
        counts.unconfirmed
    );

    // A destination sees its own shares and the wildcard, nobody else's.
    let elsewhere = store.destination_publication_counts(&other)?;
    assert_eq!((elsewhere.shared, elsewhere.published), (2, 1));
    // Everything published: nothing is left to publish.
    store.mark_publication(&ids[..4], &fingerprint, PublicationState::Published)?;
    let done = store.destination_publication_counts(&fingerprint)?;
    assert_eq!((done.shared, done.published, done.unconfirmed), (5, 5, 0));
    assert_eq!(done.unpublished(), 0);
    Ok(())
}

#[test]
fn can_unshare_applies_exactly_the_rule_unshare_enforces() -> anyhow::Result<()> {
    let store = CvcStore::open(":memory:")?;
    let fingerprint = "7".repeat(64);
    let other = "8".repeat(64);
    let worktree = "a".repeat(64);

    // For each publication state: the check must predict the refusal, and a
    // refused unshare must change nothing.
    for (conversation, state) in [
        ("none", None),
        ("pending", Some(PublicationState::Pending)),
        ("published", Some(PublicationState::Published)),
        ("unknown", Some(PublicationState::Unknown)),
    ] {
        let first = capture_with_worktree(&store, conversation, worktree.clone())?;
        capture_with_worktree(&store, conversation, worktree.clone())?;
        let snapshot = store.share_snapshot(conversation)?;
        store.share_exact_snapshot(
            conversation,
            &fingerprint,
            &snapshot,
            FutureSharePolicy::Private,
        )?;
        if let Some(state) = state {
            store.mark_publication(std::slice::from_ref(&first), &fingerprint, state)?;
        }
        // Another destination's publication never blocks this one.
        store.mark_publication(
            std::slice::from_ref(&first),
            &other,
            PublicationState::Published,
        )?;

        let check = store.can_unshare(conversation, &fingerprint)?;
        let expected = match state {
            None => (0, 0, 0),
            Some(PublicationState::Published) => (1, 0, 0),
            Some(PublicationState::Pending) => (0, 1, 0),
            Some(PublicationState::Unknown) => (0, 0, 1),
        };
        assert_eq!(
            (check.published, check.pending, check.unknown),
            expected,
            "{conversation}"
        );
        assert_eq!(check.is_allowed(), state.is_none(), "{conversation}");

        let shared_before = summary_of(&store, Some(&fingerprint), conversation)?.shared_count;
        let outcome = store.unshare_conversation_for_remote(conversation, &fingerprint);
        assert_eq!(outcome.is_ok(), check.is_allowed(), "{conversation}");
        let shared_after = summary_of(&store, Some(&fingerprint), conversation)?.shared_count;
        if check.is_allowed() {
            assert_eq!(outcome?, 2);
            assert_eq!(shared_after, 0);
        } else {
            assert_eq!(shared_after, shared_before, "a refusal changes nothing");
        }
        // The check itself never changes anything either.
        store.can_unshare(conversation, &fingerprint)?;
    }

    // Counts are per state across the conversation's interactions.
    let mixed: Vec<_> = (0..4)
        .map(|_| capture_with_worktree(&store, "mixed", worktree.clone()))
        .collect::<anyhow::Result<_>>()?;
    store.mark_publication(&mixed[..2], &fingerprint, PublicationState::Published)?;
    store.mark_publication(&mixed[2..3], &fingerprint, PublicationState::Pending)?;
    store.mark_publication(&mixed[3..], &fingerprint, PublicationState::Unknown)?;
    let check = store.can_unshare("mixed", &fingerprint)?;
    assert_eq!((check.published, check.pending, check.unknown), (2, 1, 1));
    assert_eq!(check.blocked(), 4);
    // An unknown conversation has nothing to block it.
    assert!(store.can_unshare("no-such", &fingerprint)?.is_allowed());
    Ok(())
}
