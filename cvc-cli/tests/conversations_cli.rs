#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir(&home).unwrap();
        fs::create_dir(home.join("templates")).unwrap();
        fs::create_dir(&repo).unwrap();
        let fixture = Self { temp, home, repo };
        fixture.git(&["init"]);
        fixture.git(&["config", "user.email", "test@example.invalid"]);
        fixture.git(&["config", "user.name", "CVC test"]);
        fixture.git(&[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ]);
        fs::write(fixture.repo.join("tracked"), "initial\n").unwrap();
        fixture.git(&["add", "tracked"]);
        fixture.git(&["commit", "-m", "initial"]);
        fixture
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("empty.gitconfig"))
            .env("GIT_TEMPLATE_DIR", self.home.join("templates"))
            .stdin(Stdio::null());
        command
    }

    fn git(&self, args: &[&str]) {
        let output = self.command("git").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn cvc(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_cvc"))
            .args(args)
            .output()
            .unwrap()
    }

    fn cvc_ok(&self, args: &[&str]) -> String {
        let output = self.cvc(args);
        assert!(
            output.status.success(),
            "cvc {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}

#[test]
fn conversations_lists_activity_newest_first_with_share_state() {
    let fixture = Fixture::new();
    let _ = &fixture.temp;
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "first"]);
    // Interaction timestamps are second-granular; keep the ordering strict.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fixture.cvc_ok(&["run", "--", "printf", "second"]);

    let listing = fixture.cvc_ok(&["conversations"]);
    assert!(
        listing.contains("share state for remote 'origin'"),
        "{listing}"
    );
    let runs: Vec<_> = listing
        .lines()
        .filter(|line| line.contains("run-"))
        .collect();
    assert_eq!(runs.len(), 2, "{listing}");
    assert!(
        runs.iter().all(|line| line.contains("[private]")),
        "{listing}"
    );
    assert!(
        runs.iter().all(|line| line.contains("1 thought(s)")),
        "{listing}"
    );
    // Newest first: the "second" run's conversation title appears before "first".
    let second_position = listing.find("Run: printf").unwrap();
    assert!(
        listing[..second_position].contains("Conversations"),
        "{listing}"
    );
    // The limit caps output.
    let limited = fixture.cvc_ok(&["conversations", "--limit", "1"]);
    assert_eq!(
        limited.lines().filter(|line| line.contains("run-")).count(),
        1,
        "{limited}"
    );
}

#[test]
fn bare_share_refuses_without_a_terminal_and_mutates_nothing() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);

    let output = fixture.cvc(&["share"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cvc conversations"), "{stderr}");

    // An explicit id still requires the interactive challenge; nothing was
    // marked shared by either refusal.
    let output = fixture.cvc(&["share", "some-conversation"]);
    assert!(!output.status.success());
    let listing = fixture.cvc_ok(&["conversations"]);
    assert!(!listing.contains("[shared"), "{listing}");
}
