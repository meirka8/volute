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
        ))
        .map(|_| ())
    }
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
