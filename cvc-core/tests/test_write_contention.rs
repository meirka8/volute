//! Writers that meet another process's write lock must wait for it, not fail
//! on the spot. Two SQLite rules make the difference: a transaction that reads
//! before it writes is refused the busy handler when it upgrades (deadlock
//! avoidance), so it fails instantly under contention; and the busy timeout
//! bounds how long an IMMEDIATE transaction waits at `BEGIN`. Every writing
//! store transaction is therefore IMMEDIATE, and the timeout is long enough
//! to outlast a sibling process's startup work.
//!
//! A second connection in this process stands in for the other process: SQLite
//! applies the same lock and busy-handler decisions across connections.
use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::{Author, CommitSha, Conversation, Interaction, InteractionId};
use cvc_core::privacy::{McpCapture, PreparedPolicy};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    db_path: PathBuf,
    store: CvcStore,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let db_path = temp.path().join("cvc").join("index.db");
        let store = CvcStore::open(&db_path).unwrap();
        Self {
            _temp: temp,
            db_path,
            store,
        }
    }

    fn capture(&self) -> InteractionId {
        let id = InteractionId::new();
        let interaction = Interaction {
            id: id.clone(),
            conversation_id: "contention".into(),
            parent_id: None,
            timestamp: Utc::now(),
            author: Author::Human,
            user_prompt: "hold the lock".into(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        };
        self.store
            .capture_mcp(McpCapture::new(
                Conversation {
                    id: "contention".into(),
                    title: "contention".into(),
                    created_at: interaction.timestamp,
                },
                interaction,
                Vec::new(),
                Vec::new(),
                PreparedPolicy::built_ins_only(),
                "0".repeat(64),
            ))
            .unwrap();
        id
    }
}

/// Holds the database write lock from another connection for `hold`, the
/// way a sibling process's startup migration or batch ingest would.
fn hold_write_lock(db_path: &Path, hold: Duration) -> thread::JoinHandle<()> {
    let path = db_path.to_path_buf();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let handle = thread::spawn(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        ready_tx.send(()).unwrap();
        thread::sleep(hold);
        connection.execute_batch("COMMIT").unwrap();
    });
    ready_rx.recv().unwrap();
    handle
}

#[test]
fn linking_waits_for_a_competing_writer_instead_of_failing() {
    let fixture = Fixture::new();
    let id = fixture.capture();
    // Longer than the old 250 ms patience, shorter than the current one.
    let holder = hold_write_lock(&fixture.db_path, Duration::from_millis(600));
    let started = Instant::now();
    fixture
        .store
        .link_interaction_with_metadata(
            &id,
            &CommitSha::new("a".repeat(40)),
            "temporal",
            Some("linker@example.invalid"),
        )
        .expect("the linker must wait for the lock, not fail");
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "the write went through without waiting, so the lock was never contended"
    );
    holder.join().unwrap();
    assert_eq!(fixture.store.get_artifact_links(&id).unwrap().len(), 1);
}

#[test]
fn read_then_write_transactions_wait_instead_of_failing_instantly() {
    let fixture = Fixture::new();
    // update_scan_cursor reads the cursor before writing it: as a deferred
    // transaction it was refused the busy handler and failed immediately.
    fixture
        .store
        .update_scan_cursor("worktree", "refs/heads/main", None, &"b".repeat(40))
        .unwrap();
    let holder = hold_write_lock(&fixture.db_path, Duration::from_millis(300));
    let started = Instant::now();
    let advanced = fixture
        .store
        .update_scan_cursor(
            "worktree",
            "refs/heads/main",
            Some(&"b".repeat(40)),
            &"c".repeat(40),
        )
        .expect("a read-then-write transaction must wait for the lock");
    assert!(advanced);
    assert!(started.elapsed() >= Duration::from_millis(100));
    holder.join().unwrap();
}

#[test]
fn a_writer_held_beyond_the_patience_still_fails_closed() {
    let fixture = Fixture::new();
    let id = fixture.capture();
    let holder = hold_write_lock(&fixture.db_path, Duration::from_millis(3200));
    let error = fixture
        .store
        .link_interaction_with_metadata(&id, &CommitSha::new("d".repeat(40)), "temporal", None)
        .expect_err("a lock held past the patience must surface as an error");
    assert!(error.to_string().contains("locked"), "{error}");
    holder.join().unwrap();
    assert!(fixture.store.get_artifact_links(&id).unwrap().is_empty());
}
