//! Migrations of the shared index (`<git-common-dir>/cvc/index.db`) from every
//! schema released versions create, checked against schema-only dumps of
//! those versions (`fixtures/db/<version>.sql`, generated from the release
//! tags; no captured rows).
//!
//! Each test builds a database from a dump, fills it with synthetic rows of
//! the kinds that version writes, and opens it with this version. The
//! migration must reach exactly the schema of a new database, keep every row
//! of every existing table byte for byte, copy the 0.7.x Claude Code cursors
//! losslessly, be idempotent and safe with concurrent openers, and leave a
//! database that cvc 0.7.x keeps using without rebuilding anything.
use cvc_core::db::{CvcStore, HarnessCursorUpdate};
use cvc_core::harness::HarnessId;
use cvc_core::models::{Author, Conversation, Interaction, InteractionId};
use cvc_core::privacy::{ClaudeCodeCapture, PreparedPolicy};
use rusqlite::types::Value;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// The released schemas, oldest first.
const RELEASED: [(&str, &str); 5] = [
    ("0.6.0", include_str!("fixtures/db/0.6.0.sql")),
    (
        "0.6.0-then-0.7.2",
        include_str!("fixtures/db/0.6.0-then-0.7.2.sql"),
    ),
    ("0.7.0", include_str!("fixtures/db/0.7.0.sql")),
    ("0.7.1", include_str!("fixtures/db/0.7.1.sql")),
    ("0.7.2", include_str!("fixtures/db/0.7.2.sql")),
];

/// The oldest form of all: the packaged initial schema alone, before any
/// convergence ran.
const ORIGINAL: &str = include_str!("../migrations/0001_initial_schema.sql");

fn every_earlier_schema() -> Vec<(&'static str, &'static str)> {
    let mut all = vec![("0001", ORIGINAL)];
    all.extend(RELEASED);
    all
}

/// The values of `capture_source` this version accepts.
const KNOWN_SOURCES: [&str; 11] = [
    "mcp",
    "vscode_passive",
    "vscode_explicit",
    "cli_run",
    "sync_import",
    "legacy",
    "claude_code",
    "codex",
    "opencode",
    "cursor",
    "antigravity",
];

/// A database at `<temp>/cvc/index.db` made from `schema`, in WAL mode as every
/// released version leaves it.
fn database_from(schema: &str) -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("cvc");
    std::fs::create_dir_all(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = dir.join("index.db");
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch(schema).unwrap();
    (temp, path)
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    conn.prepare(&format!("PRAGMA table_info({table})"))
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        params![table],
        |row| row.get(0),
    )
    .unwrap()
}

fn interaction_id(n: usize) -> String {
    format!("6b1f0c0e-1111-4aaa-8bbb-{n:012}")
}

/// Synthetic rows of every kind the schema can hold: conversations, a chain of
/// interactions from every source the schema accepts, context items, tool
/// executions, links, shares, publications and 0.7.x harness cursors.
fn populate(conn: &Connection) {
    let interaction_columns = columns(conn, "interactions");
    let table_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='interactions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let sources: &[&str] = if !interaction_columns.iter().any(|c| c == "capture_source") {
        &[]
    } else if table_sql.contains("'claude_code'") {
        &[
            "mcp",
            "vscode_passive",
            "vscode_explicit",
            "cli_run",
            "sync_import",
            "legacy",
            "claude_code",
        ]
    } else {
        &[
            "mcp",
            "vscode_passive",
            "vscode_explicit",
            "cli_run",
            "sync_import",
            "legacy",
        ]
    };
    conn.execute(
        "INSERT INTO conversations(id,title,created_at) VALUES ('conv-1','A title with \"quotes\" and ünïcode',1700000000), ('s1:agent-a1','',NULL)",
        [],
    )
    .unwrap();
    let rows = sources.len().max(3);
    for n in 0..rows {
        let id = interaction_id(n);
        let parent = (n > 0).then(|| interaction_id(n - 1));
        let conversation = if n % 2 == 0 { "conv-1" } else { "s1:agent-a1" };
        conn.execute(
            "INSERT INTO interactions(id,conversation_id,parent_id,timestamp,author,user_prompt,model_name,model_cot,model_response) VALUES (?1,?2,?3,?4,'human',?5,'model',?6,NULL)",
            params![
                id,
                conversation,
                parent,
                1_700_000_000 + n as i64,
                format!("[prompt ~cvc-0123456789abcdef] prompt {n}\n\nwith\0nul and \u{2028}"),
                (n % 3 == 0).then(|| format!("reasoning {n}")),
            ],
        )
        .unwrap();
        if let Some(source) = sources.get(n) {
            conn.execute(
                "UPDATE interactions SET source_request_id=?2, visibility=?3, capture_source=?4, scrubber_version=?5, capture_worktree=?6 WHERE id=?1",
                params![
                    id,
                    format!("req-{n}"),
                    if n % 2 == 0 { "private" } else { "shared" },
                    source,
                    (n % 2) as i64,
                    (n % 2 == 0).then(|| "ab".repeat(32)),
                ],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO context_items(interaction_id,file_path,git_blob_sha,dirty_patch,start_line,end_line) VALUES (?1,?2,?3,?4,1,-1)",
            params![id, format!("src/{n}.rs"), "c".repeat(40), (n % 2 == 1).then_some("@@ -1 +1 @@")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tool_executions(interaction_id,tool_protocol,tool_name,arguments,status) VALUES (?1,'native','Bash',?2,'success')",
            params![id, format!("{{\"command\":\"ls {n}\"}}")],
        )
        .unwrap();
    }
    let link_columns = columns(conn, "artifact_links");
    if link_columns.iter().any(|c| c == "linked_by") {
        conn.execute(
            "INSERT INTO artifact_links(interaction_id,git_commit_hash,link_type,linked_by) VALUES (?1,?2,'generated','dev@example.test')",
            params![interaction_id(0), "d".repeat(40)],
        )
        .unwrap();
    }
    if table_exists(conn, "interaction_shares") {
        conn.execute_batch(&format!(
            "INSERT INTO conversation_shares VALUES ('conv-1','{fp}',1);
             INSERT INTO interaction_shares VALUES ('{id}','{fp}');
             INSERT INTO publications VALUES ('{id}','{fp}','published',1700000001);",
            fp = "e".repeat(64),
            id = interaction_id(0)
        ))
        .unwrap();
    }
    if table_exists(conn, "harness_ingest_cursors") {
        conn.execute_batch(
            "INSERT INTO harness_ingest_cursors VALUES ('claude-code','s1','/home/x/.claude/projects/p/s1.jsonl',123456,1700000002);
             INSERT INTO harness_ingest_cursors VALUES ('claude-code','s1:agent-a1','/p/it''s \"quoted\" ünï.jsonl',9223372036854775807,1700000003);
             INSERT INTO harness_ingest_cursors VALUES ('claude-code','s0','/p/s0.jsonl',0,1700000004);",
        )
        .unwrap();
    }
}

type Schema = Vec<(String, String, String, Option<String>)>;
/// A table's column names and its rows, in a stable order.
type Rows = (Vec<String>, Vec<Vec<Value>>);

fn schema(conn: &Connection) -> Schema {
    conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn tables(conn: &Connection) -> Vec<String> {
    conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Every row of `table`, with its column names, in a stable order.
fn rows(conn: &Connection, table: &str) -> Rows {
    let names = columns(conn, table);
    let mut rows: Vec<Vec<Value>> = conn
        .prepare(&format!("SELECT * FROM {table}"))
        .unwrap()
        .query_map([], |row| {
            (0..names.len())
                .map(|i| row.get::<_, Value>(i))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    rows.sort_by_key(|row| format!("{row:?}"));
    (names, rows)
}

/// Every row of every table but the migration markers.
fn contents(conn: &Connection) -> Vec<(String, Rows)> {
    tables(conn)
        .into_iter()
        .filter(|table| table != "cvc_internal_migrations")
        .map(|table| {
            let rows = rows(conn, &table);
            (table, rows)
        })
        .collect()
}

fn markers(conn: &Connection) -> Vec<String> {
    conn.prepare("SELECT name FROM cvc_internal_migrations ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn rootpage(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT rootpage FROM sqlite_master WHERE type='table' AND name='interactions'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

/// The open of an operator command: every migration, the one-time
/// `capture_source` rebuild included.
fn open(path: &Path) {
    CvcStore::open_initialized_upgrading(path).unwrap();
}

/// The open of a hook, the editor and MCP: never the rebuild of a populated
/// table that cvc 0.7.x already widened.
fn open_default(path: &Path) {
    CvcStore::open_initialized(path).unwrap();
}

/// The schema this version gives a new database, with either open: the
/// rebuild of an empty table costs nothing, so the default open does it too.
fn current_schema() -> Schema {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("cvc").join("index.db");
    open_default(&path);
    let fresh = schema(&Connection::open(&path).unwrap());
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("cvc").join("index.db");
    open(&path);
    assert_eq!(schema(&Connection::open(&path).unwrap()), fresh);
    fresh
}

/// Whether the populated schema `dump` can hold `claude_code` once the cheap
/// migrations ran: cvc 0.7.x widened it, or it has no `capture_source` column
/// yet, which the convergence adds with the 0.7.x constraint. The default
/// open then leaves the rebuild to an operator command.
fn widened_by_07x(dump: &str) -> bool {
    dump.contains("'claude_code'") || !dump.contains("capture_source")
}

const CURRENT_MARKERS: [&str; 4] = [
    "capture-source-claude-code/v1",
    "capture-source/v2",
    "harness-cursors/v1",
    "legacy-link-observations/v1",
];

#[test]
fn a_new_database_has_the_current_schema_and_every_marker() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("cvc").join("index.db");
    // A hook can be the first to open a store; it still gets the current
    // schema, and the store needs no upgrade.
    open_default(&path);
    assert!(!CvcStore::open_initialized(&path)
        .unwrap()
        .needs_capture_source_upgrade()
        .unwrap());
    let conn = Connection::open(&path).unwrap();
    assert_eq!(markers(&conn), CURRENT_MARKERS);
    let triggers: Vec<String> = schema(&conn)
        .into_iter()
        .filter(|(kind, ..)| kind == "trigger")
        .map(|(_, name, table, _)| format!("{table}.{name}"))
        .collect();
    assert_eq!(
        triggers,
        [
            "interactions.interactions_capture_source_insert",
            "interactions.interactions_capture_source_update"
        ]
    );
    for table in [
        "harness_cursors",
        "conversation_relations",
        "harness_commit_evidence",
        "harness_ingest_cursors",
    ] {
        assert!(table_exists(&conn, table), "{table}");
    }
    let table_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='interactions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !table_sql.contains("CHECK(capture_source IN"),
        "{table_sql}"
    );
    // cvc 0.7.x looks for this value in the table's SQL when its marker is
    // missing; finding it, it rebuilds nothing.
    assert!(table_sql.contains("'claude_code'"), "{table_sql}");
}

#[test]
fn every_earlier_schema_migrates_to_the_current_one_without_changing_a_row() {
    let current = current_schema();
    for (version, dump) in every_earlier_schema() {
        let (_temp, path) = database_from(dump);
        let conn = Connection::open(&path).unwrap();
        populate(&conn);
        let before = contents(&conn);
        let legacy_cursors = if table_exists(&conn, "harness_ingest_cursors") {
            rows(&conn, "harness_ingest_cursors").1
        } else {
            Vec::new()
        };
        drop(conn);

        open(&path);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(schema(&conn), current, "{version}: schema");
        for (table, (names, rows)) in &before {
            // Columns an older convergence adds (to the oldest schema) are
            // allowed; every column that was there keeps every value.
            let projected: Vec<Vec<Value>> = {
                let mut projected: Vec<Vec<Value>> = conn
                    .prepare(&format!("SELECT {} FROM {table}", names.join(",")))
                    .unwrap()
                    .query_map([], |row| {
                        (0..names.len())
                            .map(|i| row.get::<_, Value>(i))
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap();
                projected.sort_by_key(|row| format!("{row:?}"));
                projected
            };
            assert_eq!(&projected, rows, "{version}: rows of {table}");
            if version != "0001" {
                assert_eq!(
                    &columns(&conn, table),
                    names,
                    "{version}: columns of {table}"
                );
            }
        }
        assert_eq!(markers(&conn), CURRENT_MARKERS, "{version}: markers");

        // Every 0.7.x Claude Code cursor has its typed copy, losslessly.
        let typed = rows(&conn, "harness_cursors").1;
        assert_eq!(typed.len(), legacy_cursors.len(), "{version}");
        let store = CvcStore::open(&path).unwrap();
        for legacy in &legacy_cursors {
            let [Value::Text(harness), Value::Text(session), Value::Text(path), Value::Integer(offset), Value::Integer(updated)] =
                legacy.as_slice()
            else {
                panic!("{version}: unexpected legacy row {legacy:?}");
            };
            assert_eq!(harness, "claude-code");
            let copy = store
                .harness_cursor(HarnessId::ClaudeCode, session)
                .unwrap()
                .unwrap_or_else(|| panic!("{version}: no typed cursor for {session}"));
            assert_eq!(
                (
                    copy.kind.as_str(),
                    copy.version,
                    copy.epoch,
                    copy.updated_at
                ),
                ("jsonl", 1, 0, *updated),
                "{version}: {session}"
            );
            assert_eq!(
                copy.state,
                serde_json::json!({ "path": path, "byte_offset": offset }),
                "{version}: {session}"
            );
            // The legacy reader still reads the same values.
            let read = store
                .harness_ingest_cursor("claude-code", session)
                .unwrap()
                .unwrap();
            assert_eq!(
                (read.transcript_path.as_str(), read.byte_offset as i64),
                (path.as_str(), *offset)
            );
        }
    }
}

fn claude_code_capture(session: &str, n: usize) -> ClaudeCodeCapture {
    let at = chrono::DateTime::from_timestamp(1_700_000_100, 0).unwrap();
    ClaudeCodeCapture::new(
        Conversation {
            id: session.into(),
            title: "a session".into(),
            created_at: at,
        },
        Interaction {
            id: interaction_id(500 + n).parse::<InteractionId>().unwrap(),
            conversation_id: session.into(),
            parent_id: None,
            timestamp: at,
            author: Author::Human,
            user_prompt: "prompt".into(),
            model_name: None,
            model_cot: None,
            model_response: Some("response".into()),
            source_request_id: Some(format!("req_{n}")),
        },
        Vec::new(),
        Vec::new(),
        PreparedPolicy::built_ins_only(),
        "ab".repeat(32),
    )
}

/// What a hook (the default open) does to a store an older version made: a
/// populated table that cvc 0.7.x widened is not rebuilt, the cheap
/// migrations still run, Claude Code keeps capturing, and the old constraint
/// keeps refusing the new harnesses' values until an operator command
/// finishes the upgrade. A table that cannot hold `claude_code` yet is
/// rebuilt in the same open, as cvc 0.7.x would.
#[test]
fn a_hook_style_open_leaves_a_populated_07x_store_for_an_operator_command() {
    for (version, dump) in every_earlier_schema() {
        let (_temp, path) = database_from(dump);
        populate(&Connection::open(&path).unwrap());
        let root = rootpage(&Connection::open(&path).unwrap());
        let store = CvcStore::open_initialized(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        if !widened_by_07x(dump) {
            assert!(!store.needs_capture_source_upgrade().unwrap(), "{version}");
            assert_eq!(markers(&conn), CURRENT_MARKERS, "{version}");
            continue;
        }
        assert!(store.needs_capture_source_upgrade().unwrap(), "{version}");
        assert_eq!(
            rootpage(&conn),
            root,
            "{version}: rebuilt by a default open"
        );
        assert_eq!(
            markers(&conn),
            [
                "capture-source-claude-code/v1",
                "harness-cursors/v1",
                "legacy-link-observations/v1"
            ],
            "{version}"
        );
        for table in [
            "harness_cursors",
            "conversation_relations",
            "harness_commit_evidence",
        ] {
            assert!(table_exists(&conn, table), "{version}: {table}");
        }
        // Claude Code capture works on the old constraint.
        let outcome = store
            .capture_claude_code_batch(
                vec![claude_code_capture("s1", 1)],
                HarnessCursorUpdate {
                    harness: "claude-code",
                    session_id: "s1",
                    transcript_path: "/p/s1.jsonl",
                    byte_offset: 200_000,
                },
            )
            .unwrap();
        assert_eq!(outcome.inserted, 1, "{version}");
        let source: String = conn
            .query_row(
                "SELECT capture_source FROM interactions WHERE id=?1",
                params![interaction_id(501)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source, "claude_code");
        assert_eq!(
            store
                .harness_cursor(HarnessId::ClaudeCode, "s1")
                .unwrap()
                .unwrap()
                .state["byte_offset"],
            200_000
        );
        // The new harnesses' values are still refused.
        assert!(conn
            .execute(
                "INSERT INTO interactions(id,conversation_id,timestamp,author,user_prompt,capture_source) VALUES (?1,'conv-1',1,'agent','x','codex')",
                params![interaction_id(600)],
            )
            .is_err());
        drop((store, conn));

        // An operator command finishes it, once; cvc 0.7.x would not undo it.
        open(&path);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(markers(&conn), CURRENT_MARKERS, "{version}");
        assert_ne!(rootpage(&conn), root, "{version}");
        assert_eq!(what_07x_would_do(&conn), "nothing", "{version}");
        assert!(!CvcStore::open_initialized(&path)
            .unwrap()
            .needs_capture_source_upgrade()
            .unwrap());
    }
}

#[test]
fn reopening_changes_nothing_and_rebuilds_nothing() {
    for (version, dump) in every_earlier_schema() {
        let (_temp, path) = database_from(dump);
        populate(&Connection::open(&path).unwrap());
        open(&path);
        let conn = Connection::open(&path).unwrap();
        let (schema_once, contents_once, root_once) =
            (schema(&conn), contents(&conn), rootpage(&conn));
        drop(conn);
        open(&path);
        open_default(&path);
        open(&path);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(schema(&conn), schema_once, "{version}");
        assert_eq!(contents(&conn), contents_once, "{version}");
        assert_eq!(rootpage(&conn), root_once, "{version}: rebuilt again");
        assert_eq!(markers(&conn), CURRENT_MARKERS, "{version}");
    }
}

/// Upgrading openers and hook-style default openers at once: every one
/// opens, the rebuild happens once, and no row changes.
#[test]
fn concurrent_openers_migrate_once() {
    for (version, dump) in RELEASED {
        let (_temp, path) = database_from(dump);
        populate(&Connection::open(&path).unwrap());
        let before = contents(&Connection::open(&path).unwrap());
        let openers: Vec<_> = (0..8)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    if n % 2 == 0 {
                        CvcStore::open_initialized_upgrading(&path).map(|_| ())
                    } else {
                        CvcStore::open_initialized(&path).map(|_| ())
                    }
                })
            })
            .collect();
        for opener in openers {
            opener.join().unwrap().unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        assert_eq!(schema(&conn), current_schema(), "{version}");
        assert_eq!(markers(&conn), CURRENT_MARKERS, "{version}");
        let after = contents(&conn);
        for (table, rows) in &before {
            let (_, migrated) = after.iter().find(|(name, _)| name == table).unwrap();
            assert_eq!(migrated, rows, "{version}: {table}");
        }
    }
}

/// A process of an older version keeps its connection, with statements
/// prepared against the old schema, while this version migrates: SQLite
/// prepares them again, and they keep working under the new constraints.
#[test]
fn a_connection_prepared_against_the_old_schema_keeps_working() {
    let (_temp, path) = database_from(RELEASED[4].1);
    let old = Connection::open(&path).unwrap();
    old.busy_timeout(std::time::Duration::from_secs(2)).unwrap();
    populate(&old);
    let mut insert = old
        .prepare("INSERT INTO interactions (id,conversation_id,parent_id,timestamp,author,user_prompt,model_name,model_cot,model_response,source_request_id,visibility,capture_source,scrubber_version,capture_worktree) VALUES (?1,'conv-1',NULL,1,'human','p',NULL,NULL,NULL,NULL,'private',?2,1,NULL)")
        .unwrap();
    let mut cursor = old
        .prepare("SELECT byte_offset FROM harness_ingest_cursors WHERE harness='claude-code' AND session_id='s1'")
        .unwrap();
    let mut upsert = old
        .prepare("INSERT INTO harness_ingest_cursors(harness,session_id,transcript_path,byte_offset,updated_at) VALUES ('claude-code','s9','/p',5,1) ON CONFLICT(harness,session_id) DO UPDATE SET byte_offset=excluded.byte_offset")
        .unwrap();
    // A reader in the middle of a transaction does not stop the migration.
    old.execute_batch("BEGIN; SELECT COUNT(*) FROM interactions;")
        .unwrap();
    open(&path);
    old.execute_batch("COMMIT").unwrap();

    insert
        .execute(params![interaction_id(100), "claude_code"])
        .unwrap();
    insert.execute(params![interaction_id(101), "mcp"]).unwrap();
    let refused = insert
        .execute(params![interaction_id(102), "not_a_source"])
        .unwrap_err();
    assert!(
        refused.to_string().contains("unknown capture_source"),
        "{refused}"
    );
    let offset: i64 = cursor.query_row([], |row| row.get(0)).unwrap();
    assert_eq!(offset, 123456);
    upsert.execute([]).unwrap();
    drop((insert, cursor, upsert));
    let count: i64 = old
        .query_row(
            "SELECT COUNT(*) FROM interactions WHERE id IN (?1,?2,?3)",
            params![
                interaction_id(100),
                interaction_id(101),
                interaction_id(102)
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn the_triggers_accept_every_known_source_and_refuse_any_other() {
    for (version, dump) in [
        ("new", ""),
        ("0.6.0", RELEASED[0].1),
        ("0.7.2", RELEASED[4].1),
    ] {
        let (_temp, path) = database_from(dump);
        open(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO conversations(id,title,created_at) VALUES('c','t',1)",
            [],
        )
        .unwrap();
        let insert = |n: usize, source: &str| {
            conn.execute(
                "INSERT INTO interactions(id,conversation_id,timestamp,author,user_prompt,capture_source,scrubber_version) VALUES (?1,'c',1,'agent','x',?2,1)",
                params![interaction_id(n), source],
            )
        };
        for (n, source) in KNOWN_SOURCES.iter().enumerate() {
            insert(n, source).unwrap_or_else(|e| panic!("{version}: {source}: {e}"));
        }
        for (n, source) in ["", "MCP", "claude-code", "codex ", "other", "a\u{0}b"]
            .iter()
            .enumerate()
        {
            assert!(insert(100 + n, source).is_err(), "{version}: {source:?}");
        }
        // The default stays 'legacy'.
        conn.execute(
            "INSERT INTO interactions(id,conversation_id,timestamp,author,user_prompt) VALUES (?1,'c',1,'agent','x')",
            params![interaction_id(200)],
        )
        .unwrap();
        let updated = conn.execute(
            "UPDATE interactions SET capture_source='other' WHERE id=?1",
            params![interaction_id(0)],
        );
        assert!(updated.is_err(), "{version}");
        conn.execute(
            "UPDATE interactions SET capture_source='sync_import' WHERE id=?1",
            params![interaction_id(0)],
        )
        .unwrap();
        // Updating another column never trips the trigger.
        conn.execute("UPDATE interactions SET visibility='shared'", [])
            .unwrap();
    }
}

/// cvc 0.7.x's `widen_capture_source_constraint`, step for step: what it would
/// do to this database when it opens it.
fn what_07x_would_do(conn: &Connection) -> &'static str {
    let migrated: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM cvc_internal_migrations WHERE name='capture-source-claude-code/v1')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if migrated {
        return "nothing";
    }
    let table_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='interactions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if table_sql.contains("'claude_code'") {
        conn.execute(
            "INSERT OR IGNORE INTO cvc_internal_migrations(name) VALUES('capture-source-claude-code/v1')",
            [],
        )
        .unwrap();
        return "marker";
    }
    "rebuild"
}

#[test]
fn cvc_07x_reopening_a_migrated_store_rebuilds_nothing() {
    for (version, dump) in every_earlier_schema() {
        let (_temp, path) = database_from(dump);
        open(&path);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(what_07x_would_do(&conn), "nothing", "{version}");
        // Even without its marker it finds the table already widened.
        conn.execute(
            "DELETE FROM cvc_internal_migrations WHERE name='capture-source-claude-code/v1'",
            [],
        )
        .unwrap();
        assert_eq!(what_07x_would_do(&conn), "marker", "{version}");
        drop(conn);
        // And this version, opening it again, restores its own state.
        open(&path);
        assert_eq!(markers(&Connection::open(&path).unwrap()), CURRENT_MARKERS);
    }
}

/// A table this version does not know column for column is not rebuilt: its
/// rows cannot be copied without losing what it does not know. The old CHECK
/// stays, so new sources fail closed there, and the store still opens.
#[test]
fn an_interactions_table_with_unknown_columns_is_left_alone() {
    let (_temp, path) = database_from(RELEASED[4].1);
    let conn = Connection::open(&path).unwrap();
    populate(&conn);
    conn.execute_batch("ALTER TABLE interactions ADD COLUMN future_column TEXT DEFAULT 'kept'")
        .unwrap();
    let before = rows(&conn, "interactions");
    let root = rootpage(&conn);
    drop(conn);
    open(&path);
    open(&path);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(rows(&conn, "interactions"), before);
    assert_eq!(rootpage(&conn), root);
    assert!(!markers(&conn).contains(&"capture-source/v2".to_owned()));
    let codex = conn.execute(
        "INSERT INTO interactions(id,conversation_id,timestamp,author,user_prompt,capture_source) VALUES (?1,'conv-1',1,'agent','x','codex')",
        params![interaction_id(300)],
    );
    assert!(codex.is_err());
}

/// The one-time rebuild of `interactions` on a large synthetic store, timed.
/// Run on purpose: `CVC_MIGRATION_ROWS=<n> CVC_MIGRATION_DIR=<dir> cargo test
/// -p cvc-core --test test_db_migrations -- --ignored --nocapture`.
#[test]
#[ignore]
fn the_one_time_rebuild_on_a_large_store_is_timed() {
    let rows: usize = std::env::var("CVC_MIGRATION_ROWS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(50_000);
    let base = std::env::var_os("CVC_MIGRATION_DIR").map(PathBuf::from);
    let temp = match &base {
        Some(dir) => TempDir::new_in(dir).unwrap(),
        None => TempDir::new().unwrap(),
    };
    let dir = temp.path().join("cvc");
    std::fs::create_dir_all(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = dir.join("index.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(RELEASED[4].1).unwrap();
        conn.execute_batch("BEGIN").unwrap();
        conn.execute("INSERT INTO conversations VALUES ('c','t',1)", [])
            .unwrap();
        let prompt = "p".repeat(4096);
        let response = "r".repeat(2048);
        let reasoning = "c".repeat(1024);
        for n in 0..rows {
            conn.execute(
                "INSERT INTO interactions(id,conversation_id,parent_id,timestamp,author,user_prompt,model_name,model_cot,model_response,source_request_id,visibility,capture_source,scrubber_version,capture_worktree) VALUES (?1,'c',?2,?3,'human',?4,'m',?5,?6,?7,'private','claude_code',1,?8)",
                params![
                    interaction_id(n),
                    (n > 0).then(|| interaction_id(n - 1)),
                    n as i64,
                    prompt,
                    reasoning,
                    response,
                    format!("req-{n}"),
                    "ab".repeat(32)
                ],
            )
            .unwrap();
            for k in 0..2 {
                conn.execute(
                    "INSERT INTO context_items(interaction_id,file_path,git_blob_sha) VALUES (?1,?2,?3)",
                    params![interaction_id(n), format!("src/{k}.rs"), "c".repeat(40)],
                )
                .unwrap();
            }
        }
        conn.execute_batch("COMMIT; PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
    }
    let bytes = std::fs::metadata(&path).unwrap().len();
    let started = std::time::Instant::now();
    open_default(&path);
    println!(
        "a hook-style open of the same store before the upgrade took {:?}",
        started.elapsed()
    );
    let started = std::time::Instant::now();
    open(&path);
    let took = started.elapsed();
    let started = std::time::Instant::now();
    open(&path);
    let again = started.elapsed();
    println!(
        "rebuild of {rows} interactions ({:.1} MiB database) took {took:?}; reopening took {again:?}",
        bytes as f64 / (1024.0 * 1024.0)
    );
    let conn = Connection::open(&path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count as usize, rows);
    assert_eq!(markers(&conn), CURRENT_MARKERS);
}
