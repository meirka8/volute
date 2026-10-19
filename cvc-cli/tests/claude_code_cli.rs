#![cfg(unix)]

//! `cvc harness install|uninstall claude-code` and `cvc ingest claude-code`
//! end to end through the binary, including hook mode on stdin.
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

const BASIC: &str = include_str!("../../cvc-core/tests/fixtures/claude-code/v2.1.260/basic.jsonl");
const SESSION: &str = "7c1e9f5a-2b2f-4f4e-9c3b-0f2e7a1d5b60";

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

    fn cvc_with_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .command(env!("CARGO_BIN_EXE_cvc"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// The acknowledgement is a typed TTY challenge in the CLI; tests grant
    /// it through the core API exactly as the challenge would.
    fn acknowledge_capture(&self) {
        let repo = git2::Repository::open(&self.repo).unwrap();
        cvc_core::privacy::acknowledge_capture(&repo).unwrap();
    }

    fn write_transcript(&self, content: &str) -> PathBuf {
        let dir = self.temp.path().join("transcripts");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{SESSION}.jsonl"));
        let root = fs::canonicalize(&self.repo).unwrap();
        fs::write(
            &path,
            content.replace("__WORKTREE__", root.to_str().unwrap()),
        )
        .unwrap();
        path
    }

    fn interaction_count(&self) -> i64 {
        let connection =
            rusqlite::Connection::open(self.repo.join(".git").join("cvc").join("index.db"))
                .unwrap();
        connection
            .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
            .unwrap()
    }

    fn hook_payload(&self, transcript: &std::path::Path, event: &str, extra: &str) -> String {
        format!(
            "{{\"session_id\":\"{SESSION}\",\"transcript_path\":\"{}\",\"cwd\":\"{}\",\"hook_event_name\":\"{event}\"{extra}}}",
            transcript.display(),
            self.repo.display()
        )
    }
}

#[test]
fn harness_install_is_gated_idempotent_and_excluded_from_git() {
    let fixture = Fixture::new();

    let output = fixture.cvc(&["harness", "install", "claude-code"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cvc init"));

    fixture.cvc_ok(&["init"]);
    let output = fixture.cvc(&["harness", "install", "claude-code"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("consent-required"), "{stderr}");
    assert!(stderr.contains("acknowledge-capture"), "{stderr}");
    assert!(!fixture.repo.join(".claude/settings.local.json").exists());

    fixture.acknowledge_capture();
    let stdout = fixture.cvc_ok(&["harness", "install", "claude-code"]);
    assert!(stdout.contains("installed"), "{stdout}");
    assert!(stdout.contains("per checkout"), "{stdout}");
    let settings_path = fixture.repo.join(".claude/settings.local.json");
    let settings = fs::read_to_string(&settings_path).unwrap();
    assert!(settings.contains(env!("CARGO_BIN_EXE_cvc")), "{settings}");
    assert!(settings.contains("ingest claude-code --hook"), "{settings}");
    for event in ["PostToolUse", "Stop", "SessionEnd"] {
        assert!(settings.contains(&format!("\"{event}\"")), "{settings}");
    }
    // Machine-specific path never reaches a commit: the fixture has no global
    // excludes, so the repository-local exclude must carry it.
    let ignored = fixture
        .command("git")
        .args(["check-ignore", "-q", ".claude/settings.local.json"])
        .status()
        .unwrap();
    assert!(ignored.success());
    let status = fixture.cvc_ok(&["status"]);
    let _ = status;
    let porcelain = fixture
        .command("git")
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&porcelain.stdout).contains(".claude"),
        "{}",
        String::from_utf8_lossy(&porcelain.stdout)
    );

    let stdout = fixture.cvc_ok(&["harness", "install", "claude-code"]);
    assert!(stdout.contains("already present"), "{stdout}");

    let stdout = fixture.cvc_ok(&["harness", "uninstall", "claude-code"]);
    assert!(stdout.contains("Removed 3"), "{stdout}");
    assert!(!settings_path.exists());
}

/// A `cvc` that came through the npm launcher runs from its versioned cache.
/// Hooks must point at the stable `~/.cvc/bin/cvc` once the launcher has
/// published it, so an upgrade applies without reinstalling the hooks, and
/// must never point at a file that does not exist.
#[test]
fn harness_install_from_the_launcher_cache_prefers_the_stable_binary() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let cache = fixture.home.join(".cvc/mcp-cache/v0.7.0-test/cvc");
    let stable = fixture.home.join(".cvc/bin/cvc");
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_cvc"), &cache).unwrap();
    let settings_path = fixture.repo.join(".claude/settings.local.json");
    // Runs `harness <args>` as `program`; the stdout must contain `expected`.
    let run = |program: &PathBuf, args: &[&str], expected: &str| {
        let output = fixture.command(program).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(stdout.contains(expected), "{stdout}");
        (stdout, String::from_utf8_lossy(&output.stderr).into_owned())
    };
    let install = ["harness", "install", "claude-code"];
    let migration_line = format!(
        "hooks now run {}, which upgrades replace in place",
        stable.display()
    );
    let using_line = format!(
        "using {} instead of the launcher's versioned cache, so upgrades reach the hooks without a reinstall",
        stable.display()
    );

    // The launcher has not published the stable binary yet (the 0.7.0 state):
    // the cache copy is recorded, with a warning that it will not follow
    // upgrades.
    let (stdout, stderr) = run(&cache, &install, "installed");
    assert!(!stdout.contains("instead of the launcher"), "{stdout}");
    assert!(!stdout.contains("hooks now run"), "{stdout}");
    assert!(
        stderr.contains(&format!(
            "warning: {} does not exist, so the hooks point at the launcher's versioned cache and will not follow upgrades",
            stable.display()
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains("run `cvc harness install claude-code` again once it exists (the launcher retries publishing on each launch)"),
        "{stderr}"
    );
    let settings = fs::read_to_string(&settings_path).unwrap();
    assert!(settings.contains("mcp-cache"), "{settings}");

    // The launcher runs from its cache precisely when it declined to publish,
    // so a stable file may be an older release. v0.6.0 has no `ingest`
    // subcommand and reports its version on stderr with exit 1: hooks steered
    // onto it would fail on every run, so the cache copy is kept.
    fs::create_dir_all(stable.parent().unwrap()).unwrap();
    fs::write(&stable, "#!/bin/sh\necho 'cvc 0.6.0' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&stable, fs::Permissions::from_mode(0o755)).unwrap();
    let (stdout, stderr) = run(&cache, &install, "already present");
    assert!(!stdout.contains("instead of the launcher"), "{stdout}");
    assert!(
        stderr.contains(&format!(
            "warning: {} reports 0.6.0, older than this cvc {}, so the hooks point at the launcher's versioned cache and will not follow upgrades",
            stable.display(),
            env!("CARGO_PKG_VERSION")
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains("run `cvc harness install claude-code` again after the launcher has published this release to ~/.cvc/bin (it retries on each launch)"),
        "{stderr}"
    );
    let settings = fs::read_to_string(&settings_path).unwrap();
    assert!(settings.contains("mcp-cache"), "{settings}");
    assert!(!settings.contains(stable.to_str().unwrap()), "{settings}");
    fs::remove_file(&stable).unwrap();

    // The launcher publishes the binary and the shim now runs it from there.
    // Re-running the install migrates the cache-path entries in place and says
    // so.
    fs::create_dir_all(stable.parent().unwrap()).unwrap();
    if fs::hard_link(&cache, &stable).is_err() {
        fs::copy(&cache, &stable).unwrap();
    }
    let (stdout, stderr) = run(&stable, &install, "updated");
    assert!(stdout.contains(&migration_line), "{stdout}");
    assert!(!stdout.contains("instead of the launcher"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
    let settings = fs::read_to_string(&settings_path).unwrap();
    assert!(settings.contains(stable.to_str().unwrap()), "{settings}");
    assert!(!settings.contains("mcp-cache"), "{settings}");

    // Idempotent, and the migration line is not repeated.
    let (stdout, _) = run(&stable, &install, "already present");
    assert!(!stdout.contains("hooks now run"), "{stdout}");
    assert_eq!(fs::read_to_string(&settings_path).unwrap(), settings);

    // A fresh install from the stable binary is not a migration either.
    run(
        &stable,
        &["harness", "uninstall", "claude-code"],
        "Removed 3",
    );
    let (stdout, _) = run(&stable, &install, "installed");
    assert!(!stdout.contains("hooks now run"), "{stdout}");

    // An explicit --binary overrides the preference, even for the cache copy.
    let explicit = ["harness", "install", "claude-code", "--binary"];
    let cache_arg = cache.to_str().unwrap();
    let (stdout, stderr) = run(&cache, &[&explicit[..], &[cache_arg]].concat(), "updated");
    assert!(!stdout.contains("instead of the launcher"), "{stdout}");
    assert!(!stdout.contains("hooks now run"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
    let settings = fs::read_to_string(&settings_path).unwrap();
    assert!(settings.contains("mcp-cache"), "{settings}");
    assert!(!settings.contains(stable.to_str().unwrap()), "{settings}");

    // Without --binary the cache copy is steered to the stable path again.
    let (stdout, stderr) = run(&cache, &install, "updated");
    assert!(stdout.contains(&using_line), "{stdout}");
    assert!(!stdout.contains("hooks now run"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
    let settings = fs::read_to_string(&settings_path).unwrap();
    assert!(settings.contains(stable.to_str().unwrap()), "{settings}");
    assert!(!settings.contains("mcp-cache"), "{settings}");
}

#[test]
fn ingest_transcript_records_the_session_once() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let transcript = fixture.write_transcript(BASIC);

    let output = fixture.cvc(&[
        "ingest",
        "claude-code",
        "--transcript",
        transcript.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("consent-required"));
    assert_eq!(fixture.interaction_count(), 0);

    fixture.acknowledge_capture();
    let stdout = fixture.cvc_ok(&[
        "ingest",
        "claude-code",
        "--transcript",
        transcript.to_str().unwrap(),
    ]);
    assert!(stdout.contains("7 new thought(s)"), "{stdout}");
    assert_eq!(fixture.interaction_count(), 7);

    let listing = fixture.cvc_ok(&["conversations"]);
    assert!(listing.contains(SESSION), "{listing}");
    assert!(listing.contains("7 thought(s)"), "{listing}");
    assert!(listing.contains("Greeting work"), "{listing}");
    assert!(listing.contains("[private]"), "{listing}");

    let stdout = fixture.cvc_ok(&[
        "ingest",
        "claude-code",
        "--transcript",
        transcript.to_str().unwrap(),
        "--session",
        SESSION,
    ]);
    assert!(stdout.contains("0 new thought(s)"), "{stdout}");
    assert_eq!(fixture.interaction_count(), 7);

    let output = fixture.cvc(&[
        "ingest",
        "claude-code",
        "--transcript",
        transcript.to_str().unwrap(),
        "--session",
        "another-session",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("belongs to session"));
}

#[test]
fn hook_mode_is_quiet_incremental_and_never_blocking() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let full = BASIC.to_owned();
    let cut = full.find("\"uuid\":\"s7\"").unwrap();
    let line_start = full[..cut].rfind('\n').unwrap() + 1;
    let transcript = fixture.write_transcript(&full[..line_start]);

    // PostToolUse mid-session: closed responses only, nothing on stdout.
    let output = fixture.cvc_with_stdin(
        &["ingest", "claude-code", "--hook"],
        &fixture.hook_payload(&transcript, "PostToolUse", ""),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(fixture.interaction_count(), 3);

    // Stop after the session grew: the final text response closes too.
    fixture.write_transcript(&full);
    let output = fixture.cvc_with_stdin(
        &["ingest", "claude-code", "--hook"],
        &fixture.hook_payload(&transcript, "Stop", ""),
    );
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fixture.interaction_count(), 7);

    // Subagent hook invocations are ignored quietly.
    let output = fixture.cvc_with_stdin(
        &["ingest", "claude-code", "--hook"],
        &fixture.hook_payload(&transcript, "PostToolUse", ",\"agent_id\":\"abc\""),
    );
    assert!(output.status.success());
    assert!(output.stdout.is_empty());

    // A transcript this cvc does not understand fails loudly, and with the
    // non-blocking exit status: never 2. The unknown shape sits in the last
    // response, which is where the stored cursor resumes.
    fixture.write_transcript(&full.replace(
        "\"type\":\"text\",\"text\":\"All set.\"",
        "\"kind\":\"text\",\"text\":\"All set.\"",
    ));
    let output = fixture.cvc_with_stdin(
        &["ingest", "claude-code", "--hook"],
        &fixture.hook_payload(&transcript, "PostToolUse", ""),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no type"));
    assert_eq!(fixture.interaction_count(), 7);

    // Garbage on stdin is also a plain failure.
    let output = fixture.cvc_with_stdin(&["ingest", "claude-code", "--hook"], "not json");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

/// Public issue 102: the post-commit hook names the commit it ran for, so an
/// agent's quiet commit still leaves exact evidence in its tool output. With
/// nothing linked and no harness installed it stays silent, as before.
#[test]
fn post_commit_hook_names_the_commit_when_the_harness_is_installed() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let silent = fixture.cvc_ok(&["hook", "post-commit"]);
    assert!(silent.is_empty(), "{silent}");

    fixture.acknowledge_capture();
    fixture.cvc_ok(&["harness", "install", "claude-code"]);
    let named = fixture.cvc_ok(&["hook", "post-commit"]);
    let head = git2::Repository::open(&fixture.repo)
        .unwrap()
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id()
        .to_string();
    assert_eq!(
        named.trim(),
        format!("CVC: Linked 0 thought(s) to commit {}.", &head[..12])
    );
}
