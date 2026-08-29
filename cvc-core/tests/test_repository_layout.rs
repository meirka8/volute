use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::{Author, Conversation, Interaction, InteractionId};
use cvc_core::privacy::{McpCapture, PreparedPolicy};
use cvc_core::repository::{RepositoryLayout, RepositoryLayoutError};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;

fn git(home: &Path, dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("empty.gitconfig"))
        .env("GIT_TEMPLATE_DIR", home.join("templates"))
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed: {status}");
}

fn repository() -> (TempDir, PathBuf, PathBuf) {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let main = temp.path().join("main");
    fs::create_dir(&home).unwrap();
    fs::create_dir(home.join("templates")).unwrap();
    fs::create_dir(&main).unwrap();
    git(&home, &main, &["init"]);
    git(
        &home,
        &main,
        &["config", "user.email", "test@example.invalid"],
    );
    git(&home, &main, &["config", "user.name", "CVC test"]);
    fs::write(main.join("tracked"), "one\n").unwrap();
    git(&home, &main, &["add", "tracked"]);
    git(&home, &main, &["commit", "-m", "initial"]);
    (temp, home, main)
}

fn capture_fixture_interaction(
    store: &CvcStore,
    interaction: Interaction,
) -> cvc_core::db::Result<()> {
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: interaction.conversation_id.clone(),
                title: "fixture".into(),
                created_at: interaction.timestamp,
            },
            interaction,
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
        ))
        .map(|_| ())
}

#[test]
fn linked_worktree_nested_path_shares_storage_but_not_policy_root() {
    let (_temp, home, main) = repository();
    let linked = main.parent().unwrap().join("linked");
    git(
        &home,
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "linked-branch",
            linked.to_str().unwrap(),
        ],
    );
    let linked_gitfile = linked.join(".git");
    let linked_gitfile_metadata = fs::symlink_metadata(&linked_gitfile).unwrap();
    assert!(linked_gitfile_metadata.is_file());
    assert!(!linked_gitfile_metadata.file_type().is_symlink());
    fs::create_dir_all(linked.join("nested/path")).unwrap();

    let primary = RepositoryLayout::discover(&main).unwrap();
    let secondary = RepositoryLayout::discover(linked.join("nested/path")).unwrap();
    assert_eq!(primary.common_git_dir(), secondary.common_git_dir());
    assert_eq!(primary.cvc_dir(), secondary.cvc_dir());
    assert_eq!(primary.db_path(), secondary.db_path());
    assert_ne!(primary.git_dir(), secondary.git_dir());
    assert_ne!(
        primary.worktree_root().unwrap(),
        secondary.worktree_root().unwrap()
    );
    assert_eq!(
        secondary.policy_root().unwrap(),
        secondary.worktree_root().unwrap()
    );

    // Git writes this marker relative to the per-worktree administrative dir.
    let marker = fs::read_to_string(secondary.git_dir().join("commondir")).unwrap();
    assert!(!Path::new(marker.trim()).is_absolute());
}

#[test]
fn linked_worktree_store_handles_write_concurrently_to_shared_database() {
    let (_temp, home, main) = repository();
    let linked = main.parent().unwrap().join("linked");
    git(
        &home,
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "concurrent-branch",
            linked.to_str().unwrap(),
        ],
    );
    let main_layout = RepositoryLayout::discover(&main).unwrap();
    let linked_layout = RepositoryLayout::discover(&linked).unwrap();
    assert_eq!(main_layout.db_path(), linked_layout.db_path());
    assert_ne!(
        main_layout.policy_root().unwrap(),
        linked_layout.policy_root().unwrap()
    );

    // Each worker owns an independent SQLite connection, as separate servers in
    // the primary and linked checkouts would.  The barrier deliberately aligns
    // writes without timing-dependent sleeps.  Open/migrate both connections
    // first: concurrent first-use migration is outside the behavior under test.
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    let stores = [
        ("main", CvcStore::open(main_layout.db_path()).unwrap()),
        ("linked", CvcStore::open(linked_layout.db_path()).unwrap()),
    ];
    for (name, store) in stores {
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let conversation = Conversation {
                id: format!("concurrent-{name}"),
                title: format!("{name} conversation"),
                created_at: Utc::now(),
            };
            barrier.wait();
            let interaction = Interaction {
                id: InteractionId::new(),
                conversation_id: conversation.id,
                parent_id: None,
                timestamp: Utc::now(),
                author: Author::Human,
                user_prompt: format!("write from {name}"),
                model_name: None,
                model_cot: None,
                model_response: None,
                source_request_id: None,
            };
            for attempt in 0..20 {
                match capture_fixture_interaction(&store, interaction.clone()) {
                    Ok(()) => return interaction.id,
                    Err(_error) if attempt < 19 => {
                        // Independent SQLite writers may lose a lock race. Yield
                        // rather than sleeping so the test remains synchronized,
                        // then prove that both successful writes are durable.
                        std::thread::yield_now();
                    }
                    Err(error) => panic!("concurrent write did not succeed: {error}"),
                }
            }
            unreachable!("the retry loop either returns or panics");
        }));
    }
    let ids: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().expect("concurrent store worker panicked"))
        .collect();

    let store = CvcStore::open(main_layout.db_path()).unwrap();
    for id in ids {
        assert!(store.get_interaction(&id).unwrap().is_some());
    }
    let integrity: String = rusqlite::Connection::open(main_layout.db_path())
        .unwrap()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}

#[test]
fn normal_nested_and_detached_head_are_discovered() {
    let (_temp, home, main) = repository();
    fs::create_dir_all(main.join("a/b")).unwrap();
    let normal = RepositoryLayout::discover(main.join("a/b")).unwrap();
    assert_eq!(normal.git_dir(), normal.common_git_dir());
    assert_eq!(
        normal.worktree_root().unwrap(),
        fs::canonicalize(&main).unwrap()
    );
    git(&home, &main, &["checkout", "--detach"]);
    assert_eq!(
        RepositoryLayout::discover(&main)
            .unwrap()
            .worktree_root()
            .unwrap(),
        fs::canonicalize(&main).unwrap()
    );
}

#[test]
fn bare_repository_has_no_policy_root() {
    let temp = TempDir::new().unwrap();
    let bare = temp.path().join("bare.git");
    fs::create_dir(temp.path().join("templates")).unwrap();
    fs::create_dir(&bare).unwrap();
    git(temp.path(), &bare, &["init", "--bare"]);
    let layout = RepositoryLayout::discover(&bare).unwrap();
    assert_eq!(layout.common_git_dir(), layout.git_dir());
    assert!(matches!(
        layout.policy_root(),
        Err(RepositoryLayoutError::BareRepository)
    ));
}

#[test]
fn invalid_commondir_fails_closed() {
    let (_temp, home, main) = repository();
    let linked = main.parent().unwrap().join("linked");
    git(
        &home,
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "bad-marker",
            linked.to_str().unwrap(),
        ],
    );
    let repo = git2::Repository::open(&linked).unwrap();
    fs::write(repo.path().join("commondir"), "../does-not-exist\n").unwrap();
    assert!(cvc_core::repository::common_git_dir(&repo).is_err());
}

#[test]
fn missing_linked_worktree_commondir_fails_closed() {
    let (_temp, home, main) = repository();
    let linked = main.parent().unwrap().join("linked");
    git(
        &home,
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "missing-marker",
            linked.to_str().unwrap(),
        ],
    );
    let repo = git2::Repository::open(&linked).unwrap();
    let admin_dir = repo.path().to_owned();
    fs::remove_file(admin_dir.join("commondir")).unwrap();
    assert!(RepositoryLayout::from_repository(repo).is_err());
    assert!(!admin_dir.join("cvc").exists());
}

#[test]
fn malformed_commondir_contents_and_types_fail_closed() {
    let (_temp, home, main) = repository();
    let linked = main.parent().unwrap().join("linked");
    git(
        &home,
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "invalid-markers",
            linked.to_str().unwrap(),
        ],
    );
    let repo = git2::Repository::open(&linked).unwrap();
    let marker = repo.path().join("commondir");
    for contents in [b"\n".as_slice(), b"a\nb\n", b"a\0b\n", b"\xff\n"] {
        fs::write(&marker, contents).unwrap();
        assert!(cvc_core::repository::common_git_dir(&repo).is_err());
    }
    fs::write(&marker, vec![b'a'; 4097]).unwrap();
    assert!(cvc_core::repository::common_git_dir(&repo).is_err());
    let target_file = repo.path().join("not-a-directory");
    fs::write(&target_file, "x").unwrap();
    fs::write(&marker, "not-a-directory\n").unwrap();
    assert!(cvc_core::repository::common_git_dir(&repo).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        fs::remove_file(&marker).unwrap();
        symlink("../..", &marker).unwrap();
        assert!(cvc_core::repository::common_git_dir(&repo).is_err());
    }
}
