#![cfg(unix)]

use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    home: PathBuf,
    bin: PathBuf,
    main: PathBuf,
    linked: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let bin = temp.path().join("bin");
        let main = temp.path().join("main");
        fs::create_dir(&home).unwrap();
        fs::create_dir(home.join("templates")).unwrap();
        fs::create_dir(&bin).unwrap();
        let fixture_cvc = bin.join("cvc");
        fs::copy(env!("CARGO_BIN_EXE_cvc"), &fixture_cvc).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fixture_cvc, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::create_dir(&main).unwrap();
        let fixture = Self {
            temp,
            home,
            bin,
            main,
            linked: PathBuf::new(),
        };
        fixture.git(&fixture.main, &["init"]);
        fixture.git(
            &fixture.main,
            &["config", "user.email", "test@example.invalid"],
        );
        fixture.git(&fixture.main, &["config", "user.name", "CVC test"]);
        fs::write(fixture.main.join("tracked"), "initial\n").unwrap();
        fs::write(
            fixture.main.join(".gitignore"),
            "nested/\ndeep/\nlinked-hooks/\n",
        )
        .unwrap();
        fixture.git(&fixture.main, &["add", "tracked", ".gitignore"]);
        fixture.git(&fixture.main, &["commit", "-m", "initial"]);
        let linked = fixture.temp.path().join("linked");
        fixture.git(
            &fixture.main,
            &[
                "worktree",
                "add",
                "-b",
                "linked-branch",
                linked.to_str().unwrap(),
            ],
        );
        Self { linked, ..fixture }
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        // Git's environment namespace controls discovery, indexes, objects,
        // alternates, and injected configuration. Clear it wholesale instead
        // of relying on a fragile list of individual overrides.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("empty.gitconfig"))
            .env("GIT_TEMPLATE_DIR", self.home.join("templates"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        command
    }

    fn git(&self, cwd: &Path, args: &[&str]) {
        let output = self.command("git", cwd).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn cvc(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(self.bin.join("cvc"), cwd)
            .args(args)
            .output()
            .unwrap()
    }

    fn cvc_ok(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.cvc(cwd, args);
        assert!(
            output.status.success(),
            "cvc {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

#[test]
fn cli_uses_common_storage_and_active_worktree_policy() {
    let fixture = Fixture::new();
    let nested = fixture.linked.join("nested/path");
    fs::create_dir_all(&nested).unwrap();

    // Initialization from a linked nested directory uses the main common dir,
    // and repeated initialization from the primary worktree is harmless.
    fixture.cvc_ok(&nested, &["init"]);
    fixture.cvc_ok(&fixture.main, &["init"]);
    let db = fixture.main.join(".git/cvc/index.db");
    assert!(db.is_file());
    assert!(!fixture.linked.join(".git/cvc").exists());

    // Capture the dirty version of a tracked file, commit it in the linked
    // worktree, then run its installed hook.  The hook must link against the
    // common database rather than attempting to write below the gitfile.
    fs::write(fixture.linked.join("tracked"), "linked change\n").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    fixture.cvc_ok(&nested, &["run", "--", "printf", "captured"]);
    let before_hook = fixture.cvc_ok(&nested, &["status"]);
    assert!(
        String::from_utf8_lossy(&before_hook.stdout)
            .contains("Floating Interactions (Unlinked): 1"),
        "{}",
        String::from_utf8_lossy(&before_hook.stdout)
    );
    fixture.git(&fixture.linked, &["add", "tracked"]);
    fixture.git(&fixture.linked, &["commit", "-m", "linked change"]);
    assert!(fs::symlink_metadata(fixture.linked.join(".git"))
        .unwrap()
        .is_file());

    // The child remains in the invocation directory, while capture policy is
    // loaded from the linked worktree root rather than that nested directory.
    fs::write(
        fixture.linked.join(".thoughtignore"),
        "literal:LINKEDSECRET\n",
    )
    .unwrap();
    let pwd = fixture.cvc_ok(&nested, &["run", "--", "pwd"]);
    assert_eq!(
        String::from_utf8_lossy(&pwd.stdout).lines().last().unwrap(),
        nested.to_str().unwrap()
    );
    fixture.cvc_ok(&nested, &["run", "--", "printf", "LINKEDSECRET"]);

    // Main and linked nested paths see the same database and active worktree
    // status root, and expose the captured/link history through log.
    let linked_status = fixture.cvc_ok(&nested, &["status"]);
    assert!(String::from_utf8_lossy(&linked_status.stdout)
        .contains(&format!("CVC Status for {}", fixture.linked.display())));
    let status = fixture.cvc_ok(&fixture.main, &["status"]);
    assert!(
        String::from_utf8_lossy(&status.stdout)
            .contains(&format!("CVC Status for {}", fixture.main.display())),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
    let main_nested = fixture.main.join("deep");
    fs::create_dir(&main_nested).unwrap();
    let log = fixture.cvc_ok(&main_nested, &["log"]);
    let linked_log = fixture.cvc_ok(&nested, &["log"]);
    let main_log = String::from_utf8_lossy(&log.stdout);
    let linked_log = String::from_utf8_lossy(&linked_log.stdout);
    assert!(main_log.contains("Node:"));
    assert!(linked_log.contains("Node:"));

    let connection = Connection::open(db).unwrap();
    let interaction_id: String = connection
        .query_row(
            "SELECT id FROM interactions ORDER BY timestamp LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let linked_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM artifact_links", [], |row| row.get(0))
        .unwrap();
    assert!(linked_count > 0, "linked hook did not use common history");
    assert!(main_log.contains(&interaction_id));
    assert!(linked_log.contains(&interaction_id));
    let stored: String = connection
        .query_row(
            "SELECT model_response FROM interactions ORDER BY timestamp DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!stored.contains("LINKEDSECRET"));
}

#[test]
fn linked_relative_hookspath_is_worktree_relative_and_advisory() {
    let fixture = Fixture::new();
    let nested = fixture.linked.join("nested");
    fs::create_dir(&nested).unwrap();
    fixture.cvc_ok(&nested, &["init"]);
    fixture.git(
        &fixture.linked,
        &["config", "core.hooksPath", "linked-hooks"],
    );
    fixture.cvc_ok(&nested, &["init"]);
    let hook = fixture.linked.join("linked-hooks/post-commit");
    assert!(hook.is_file());
    assert!(!nested.join("linked-hooks/post-commit").exists());
    assert!(!fixture.linked.join(".git/cvc").exists());

    // Commit from the linked worktree, then capture a context-free interaction
    // and invoke its installed hook. This exercises temporal linking without
    // depending on platform-specific dirty-diff rendering.
    fs::write(
        fixture.linked.join(".gitignore"),
        "nested/\nlinked-hooks/\n",
    )
    .unwrap();
    fixture.git(&fixture.linked, &["add", ".gitignore"]);
    fixture.git(&fixture.linked, &["commit", "-m", "ignore hook fixture"]);
    // SQLite stores capture timestamps at second precision; advance beyond the
    // parent commit's second so the temporal linker eligibility bound is met.
    std::thread::sleep(std::time::Duration::from_secs(1));
    fixture.cvc_ok(&nested, &["run", "--", "printf", "captured"]);
    let before_hook = fixture.cvc_ok(&nested, &["status"]);
    assert!(
        String::from_utf8_lossy(&before_hook.stdout)
            .contains("Floating Interactions (Unlinked): 1"),
        "{}",
        String::from_utf8_lossy(&before_hook.stdout)
    );

    // Invoke the installed hook in the linked worktree with an isolated PATH;
    // its advisory wrapper must never block Git even when CVC has no work.
    let output = fixture.command(&hook, &fixture.linked).output().unwrap();
    assert!(output.status.success());
    let connection = Connection::open(fixture.main.join(".git/cvc/index.db")).unwrap();
    let linked: i64 = connection
        .query_row("SELECT COUNT(*) FROM artifact_links", [], |row| row.get(0))
        .unwrap();
    let after_hook = fixture.cvc_ok(&nested, &["status"]);
    assert!(
        linked > 0,
        "linked worktree hook did not record provenance; status={} stdout={} stderr={}",
        String::from_utf8_lossy(&after_hook.stdout),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hookspath_tilde_expands_using_isolated_home() {
    let fixture = Fixture::new();
    fixture.git(
        &fixture.linked,
        &["config", "core.hooksPath", "~/cvc-hooks"],
    );
    fixture.cvc_ok(&fixture.linked, &["init"]);
    assert!(fixture.home.join("cvc-hooks/post-commit").is_file());
}

#[test]
fn run_rejects_malformed_linked_layout_before_child_and_runs_outside_repositories() {
    let fixture = Fixture::new();
    let admin = git2::Repository::open(&fixture.linked)
        .unwrap()
        .path()
        .to_owned();
    fs::write(admin.join("commondir"), "../missing-common-dir\n").unwrap();
    let blocked = fixture.temp.path().join("blocked");
    let output = fixture.cvc(
        &fixture.linked,
        &["run", "--", "touch", blocked.to_str().unwrap()],
    );
    assert!(!output.status.success());
    assert!(!blocked.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Failed to discover Git repository"));

    let outside = fixture.temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let side_effect = outside.join("ran");
    let output = fixture.cvc(
        &outside,
        &["run", "--", "touch", side_effect.to_str().unwrap()],
    );
    assert!(output.status.success());
    assert!(side_effect.is_file());
}

#[test]
fn failed_hook_install_does_not_initialize_storage_and_can_be_retried() {
    let fixture = Fixture::new();
    let blocked_hooks = fixture.main.join("blocked-hooks");
    fs::write(&blocked_hooks, "not a directory\n").unwrap();
    fixture.git(
        &fixture.main,
        &["config", "core.hooksPath", "blocked-hooks"],
    );

    let failed = fixture.cvc(&fixture.main, &["init"]);
    assert!(!failed.status.success());
    assert!(!fixture.main.join(".git/cvc").exists());
    let run = fixture.cvc_ok(&fixture.main, &["run", "--", "printf", "not-captured"]);
    assert!(String::from_utf8_lossy(&run.stdout).contains("not-captured"));
    let status = fixture.cvc_ok(&fixture.main, &["status"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("not initialized"));

    fs::remove_file(blocked_hooks).unwrap();
    fixture.git(&fixture.main, &["config", "core.hooksPath", "fixed-hooks"]);
    fixture.cvc_ok(&fixture.main, &["init"]);
    assert!(fixture.main.join(".git/cvc/index.db").is_file());
    assert!(fixture.main.join("fixed-hooks/post-commit").is_file());
}
