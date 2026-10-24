#![cfg(unix)]

//! `cvc harness install|uninstall claude-code` and `cvc ingest claude-code`
//! end to end through the binary, including hook mode on stdin.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::SystemTime;
use tempfile::TempDir;

const BASIC: &str = include_str!("../../cvc-core/tests/fixtures/claude-code/v2.1.260/basic.jsonl");
const SESSION: &str = "7c1e9f5a-2b2f-4f4e-9c3b-0f2e7a1d5b60";

/// Claude Code exports these to its Bash tool and to its hooks, so a test run
/// from a session inherits them. Each changes what `cvc` and its Git hooks
/// print or do (`CLAUDECODE=1` makes the post-commit hook name the commit;
/// `CLAUDE_PROJECT_DIR` confines hook mode to the repository the session was
/// started in; `CLAUDE_PLUGIN_ROOT` is not read by `cvc`, but is part of a
/// plugin hook's environment), so no command a test spawns may see them unless
/// the test sets them itself.
const CLAUDE_CODE_ENV: [&str; 3] = ["CLAUDECODE", "CLAUDE_PLUGIN_ROOT", "CLAUDE_PROJECT_DIR"];

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
        for name in CLAUDE_CODE_ENV {
            command.env_remove(name);
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
        self.git_in(&self.repo, args);
    }

    fn git_in(&self, dir: &Path, args: &[&str]) {
        let output = self
            .command("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
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
        self.write_transcript_for(&self.repo, content)
    }

    /// The transcript of a session that worked in `worktree`.
    fn write_transcript_for(&self, worktree: &Path, content: &str) -> PathBuf {
        let dir = self.temp.path().join("transcripts");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{SESSION}.jsonl"));
        let root = fs::canonicalize(worktree).unwrap();
        fs::write(
            &path,
            content.replace("__WORKTREE__", root.to_str().unwrap()),
        )
        .unwrap();
        path
    }

    fn interaction_count(&self) -> i64 {
        self.interactions_in(&self.repo)
            .expect("the fixture repository has a store")
    }

    /// The interactions stored in the repository at `repo`; `None` when it has
    /// no store (and none is created by asking).
    fn interactions_in(&self, repo: &Path) -> Option<i64> {
        let database = repo.join(".git").join("cvc").join("index.db");
        database.exists().then(|| {
            rusqlite::Connection::open(database)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
                .unwrap()
        })
    }

    /// Where ingestion of the test session resumes in the fixture repository.
    fn cursor(&self) -> Option<i64> {
        use rusqlite::OptionalExtension;
        rusqlite::Connection::open(self.repo.join(".git").join("cvc").join("index.db"))
            .unwrap()
            .query_row(
                "SELECT byte_offset FROM harness_ingest_cursors WHERE harness='claude-code' AND session_id=?1",
                [SESSION],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
    }

    /// The worktrees (as the fingerprints they are stored under) that the
    /// interactions stored in the repository at `repo` were captured in.
    fn capture_worktrees(&self, repo: &Path) -> Vec<String> {
        let connection =
            rusqlite::Connection::open(repo.join(".git").join("cvc").join("index.db")).unwrap();
        let mut statement = connection
            .prepare("SELECT DISTINCT capture_worktree FROM interactions ORDER BY 1")
            .unwrap();
        let rows = statement.query_map([], |row| row.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    }

    fn hook_payload(&self, transcript: &Path, event: &str, extra: &str) -> String {
        self.hook_payload_in(&self.repo, transcript, event, extra)
    }

    /// The payload Claude Code delivers for a session whose directory is `cwd`.
    fn hook_payload_in(&self, cwd: &Path, transcript: &Path, event: &str, extra: &str) -> String {
        format!(
            "{{\"session_id\":\"{SESSION}\",\"transcript_path\":\"{}\",\"cwd\":\"{}\",\"hook_event_name\":\"{event}\"{extra}}}",
            transcript.display(),
            cwd.display()
        )
    }

    /// Starts `cvc ingest claude-code --hook` in `cwd` with `env` set on top of
    /// the isolated environment, its stdin open for the payload.
    fn spawn_hook(&self, cwd: &Path, env: &[(&str, &str)]) -> Child {
        let mut command = self.command(env!("CARGO_BIN_EXE_cvc"));
        command
            .args(["ingest", "claude-code", "--hook"])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in env {
            command.env(name, value);
        }
        command.spawn().unwrap()
    }

    /// Runs `cvc ingest claude-code --hook` from `cwd` with `input` on stdin,
    /// as Claude Code (or its plugin) does, and `env` set on top of the
    /// isolated environment.
    fn hook(&self, cwd: &Path, input: &[u8], env: &[(&str, &str)]) -> Output {
        let mut child = self.spawn_hook(cwd, env);
        // The command may stop reading before a very large payload is
        // written; that is the command's answer, not a test failure.
        let _ = child.stdin.take().unwrap().write_all(input);
        child.wait_with_output().unwrap()
    }

    /// A directory with a `cvc` in it, for Git's hooks to find. A link, so no
    /// copy of the binary is made.
    fn bin_dir(&self) -> PathBuf {
        let bin = self.temp.path().join("bin");
        if !bin.exists() {
            fs::create_dir(&bin).unwrap();
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_cvc"), bin.join("cvc")).unwrap();
        }
        bin
    }

    /// `git` in `dir`, successful, with `cvc` on its `PATH` so the hooks `cvc
    /// init` installed run, and `env` set. Returns its stdout and stderr
    /// together: Git gives a hook's stdout to its own stderr, and what a Claude
    /// Code session sees of a commit is both.
    fn git_with_hooks(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let mut paths = vec![self.bin_dir()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let mut command = self.command("git");
        command
            .args(args)
            .current_dir(dir)
            .env("PATH", std::env::join_paths(paths).unwrap());
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    /// A new Git repository named `name` beside the fixture's, with one
    /// commit and no CVC.
    fn git_repository(&self, name: &str) -> PathBuf {
        let dir = self.temp.path().join(name);
        fs::create_dir(&dir).unwrap();
        self.git_in(&dir, &["init"]);
        self.git_in(&dir, &["config", "user.email", "test@example.invalid"]);
        self.git_in(&dir, &["config", "user.name", "CVC test"]);
        fs::write(dir.join("tracked"), "initial\n").unwrap();
        self.git_in(&dir, &["add", "tracked"]);
        self.git_in(&dir, &["commit", "-m", "initial"]);
        dir
    }

    /// [`Fixture::git_repository`], initialized for CVC and with capture
    /// acknowledged: one in which hook mode would ingest whatever it is sent.
    fn capturing_repository(&self, name: &str) -> PathBuf {
        let dir = self.git_repository(name);
        let output = self
            .command(env!("CARGO_BIN_EXE_cvc"))
            .arg("init")
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "cvc init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let repository = git2::Repository::open(&dir).unwrap();
        cvc_core::privacy::acknowledge_capture(&repository).unwrap();
        dir
    }
}

/// What hook mode must be in a project that is not CVC's: it ran, did nothing,
/// and said nothing on either stream (Claude Code shows stderr of a failing
/// hook, and takes stdout as hook output).
fn assert_silent_success(output: &Output, what: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{what}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "{what}: stdout {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "{what}: stderr {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A failure Claude Code shows the user: exit 1, never the blocking status 2,
/// nothing on stdout, and a message naming `needle`.
fn assert_visible_failure(output: &Output, needle: &str, what: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{what}: {stderr}");
    assert!(output.stdout.is_empty(), "{what}");
    assert!(stderr.contains(needle), "{what}: {stderr}");
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
    // Since public issue 113 new worktrees follow the install (see
    // harness_worktree_cli.rs), so the old "per checkout" advice is gone.
    assert!(stdout.contains("get these hooks automatically"), "{stdout}");
    assert!(!stdout.contains("per checkout"), "{stdout}");
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
        // A binary written a moment ago may still be open for writing in a
        // process that another test thread is in the middle of forking (the
        // descriptor is closed on exec, which that child has not reached), and
        // then cannot be executed: "text file busy". It clears in milliseconds.
        let mut attempts = 0;
        let output = loop {
            match fixture.command(program).args(args).output() {
                Err(error)
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 100 =>
                {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                other => break other.unwrap(),
            }
        };
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

/// Public issue 116: the Claude Code plugin is installed for the user and so
/// runs this command in every project they open. Outside a CVC-initialized
/// repository it must do nothing at all, whatever it was handed.
#[test]
fn hook_mode_is_silent_outside_a_cvc_initialized_repository() {
    // `fixture.repo` is a Git repository on which `cvc init` never ran.
    let fixture = Fixture::new();
    let transcript = fixture.write_transcript(BASIC);
    let outside = fixture.temp.path().join("not-a-repository");
    fs::create_dir(&outside).unwrap();
    let bare = fixture.temp.path().join("bare.git");
    fs::create_dir(&bare).unwrap();
    fixture.git_in(&bare, &["init", "--bare"]);
    let a_file = fixture.temp.path().join("a-file");
    fs::write(&a_file, "not a directory").unwrap();
    let missing = fixture.temp.path().join("never-created");

    let subdirectory = fixture.repo.join("src");
    fs::create_dir(&subdirectory).unwrap();

    let directories = [
        ("not a Git repository", outside.clone()),
        ("a Git repository without CVC", fixture.repo.clone()),
        ("a subdirectory of it", subdirectory),
        ("a bare repository", bare.clone()),
        ("a path that does not exist", missing),
        ("a path that is a file", a_file),
    ];
    for (label, directory) in &directories {
        // The directory is named by the payload and the process runs
        // elsewhere, as Claude Code does it.
        let payload = fixture.hook_payload_in(directory, &transcript, "PostToolUse", "");
        let output = fixture.hook(&outside, payload.as_bytes(), &[]);
        assert_silent_success(&output, label);
        // And the other way round: no `cwd` in the payload, so the process's
        // own directory is the one in question.
        let payload = format!(
            "{{\"session_id\":\"{SESSION}\",\"transcript_path\":\"{}\",\"hook_event_name\":\"Stop\"}}",
            transcript.display()
        );
        if directory.is_dir() {
            let output = fixture.hook(directory, payload.as_bytes(), &[]);
            assert_silent_success(&output, &format!("{label} (process directory)"));
        }
    }

    // The payload is not looked at before that is settled, so one that could
    // not be used anywhere is just as silent.
    let oversize = format!("{{\"cwd\":\"x\"}}{}", " ".repeat(2 * 1024 * 1024));
    let unusable: [(&str, &[u8]); 7] = [
        ("garbage", b"not json"),
        ("an empty payload", b""),
        ("a JSON array", b"[1,2]"),
        ("a cwd that is not text", b"{\"cwd\":5}"),
        ("no session id", b"{\"transcript_path\":\"/x\"}"),
        ("an empty object", b"{}"),
        ("an oversize payload", oversize.as_bytes()),
    ];
    for (label, input) in unusable {
        assert_silent_success(&fixture.hook(&outside, input, &[]), label);
        assert_silent_success(
            &fixture.hook(&fixture.repo, input, &[]),
            &format!("{label}, in a repository without CVC"),
        );
        assert_silent_success(
            &fixture.hook(&bare, input, &[]),
            &format!("{label}, in a bare repository"),
        );
    }
    // Nothing was created anywhere along the way.
    assert!(!fixture.repo.join(".git").join("cvc").exists());
}

/// Inside a CVC-initialized repository the same payloads are the failures they
/// always were: visible, and never the blocking exit status.
#[test]
fn hook_mode_reports_an_unusable_payload_inside_a_cvc_initialized_repository() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let transcript = fixture.write_transcript(BASIC);
    let outside = fixture.temp.path().join("not-a-repository");
    fs::create_dir(&outside).unwrap();

    let oversize = " ".repeat(2 * 1024 * 1024);
    let unusable: [(&str, &[u8], &str); 6] = [
        ("garbage", b"not json", "not the JSON"),
        ("an empty payload", b"", "not the JSON"),
        ("a JSON array", b"[1,2]", "not the JSON"),
        (
            "no session id",
            b"{\"transcript_path\":\"/x\"}",
            "no session_id",
        ),
        (
            "no transcript path",
            b"{\"session_id\":\"s\"}",
            "no transcript_path",
        ),
        ("an oversize payload", oversize.as_bytes(), "exceeds 1 MiB"),
    ];
    for (label, input, message) in unusable {
        let output = fixture.hook(&fixture.repo, input, &[]);
        assert_visible_failure(&output, message, label);
    }
    // Which repository counts is the payload's `cwd` and nothing else: one
    // that points away from CVC's is silent, though the process runs inside it.
    let payload = fixture.hook_payload_in(&outside, &transcript, "Stop", "");
    let output = fixture.hook(&fixture.repo, payload.as_bytes(), &[]);
    assert_silent_success(&output, "a payload directory outside any repository");
    assert_eq!(fixture.interaction_count(), 0);
    // And a subdirectory of the repository is the repository.
    let subdirectory = fixture.repo.join("src");
    fs::create_dir(&subdirectory).unwrap();
    let payload = fixture.hook_payload_in(&subdirectory, &transcript, "Stop", "");
    let output = fixture.hook(&outside, payload.as_bytes(), &[]);
    assert_silent_success(&output, "a payload directory inside the repository");
    assert_eq!(fixture.interaction_count(), 7);
}

/// The plugin's SessionStart check (`plugins/claude-code/scripts/check-cvc.sh`)
/// tells a cvc that has the silent no-op from one that lacks it by feeding
/// hook mode this payload and a project directory that does not exist either:
/// a working directory that does not exist, and no session. The newer cvc exits
/// 0 and says nothing wherever it runs; an older one rejects the missing
/// session. Pinned here with the exact bytes and variable the script sends.
#[test]
fn hook_mode_answers_the_plugins_capability_probe_silently_from_any_directory() {
    const PROBE: &str = r#"{"cwd":"/nonexistent/cvc-plugin-probe"}"#;
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    fixture.cvc_ok(&["harness", "install", "claude-code"]);
    let outside = fixture.temp.path().join("not-a-repository");
    fs::create_dir(&outside).unwrap();
    let uninitialized = fixture.temp.path().join("uninitialized");
    fs::create_dir(&uninitialized).unwrap();
    fixture.git_in(&uninitialized, &["init"]);
    let the_scripts = [("CLAUDE_PROJECT_DIR", "/nonexistent/cvc-plugin-probe")];
    let unset = [];
    let empty = [("CLAUDE_PROJECT_DIR", "")];

    // Inside an initialized, acknowledged repository, where any other payload
    // without a session is an error.
    for (label, directory) in [
        ("inside a CVC repository", fixture.repo.clone()),
        ("a repository without CVC", uninitialized),
        ("outside any repository", outside),
    ] {
        for (which, env) in [
            ("as the script sends it", &the_scripts[..]),
            ("without a project directory", &unset[..]),
            ("with an empty one", &empty[..]),
        ] {
            let output = fixture.hook(&directory, PROBE.as_bytes(), env);
            assert_silent_success(&output, &format!("{label}, {which}"));
        }
    }
    assert_eq!(fixture.interaction_count(), 0);

    // Without the directory that does not exist the same payload is what an
    // older cvc made of the probe, and so what the check looks for.
    let output = fixture.hook(&fixture.repo, br#"{"cwd":""}"#, &[]);
    assert_visible_failure(&output, "no session_id", "a payload with no session");
}

/// Silence is for a directory that is not in a repository, or that Git itself
/// would not use. A repository that cannot be read could be an initialized one
/// whose capture is being lost, so that stays a visible failure, and says where.
#[test]
fn hook_mode_does_not_take_a_broken_repository_for_no_repository() {
    let fixture = Fixture::new();
    let transcript = fixture.write_transcript(BASIC);
    let empty_git_dir = fixture.temp.path().join("empty-git-dir");
    fs::create_dir_all(empty_git_dir.join(".git")).unwrap();
    let dangling = fixture.temp.path().join("dangling-gitfile");
    fs::create_dir(&dangling).unwrap();
    fs::write(dangling.join(".git"), "gitdir: /nonexistent/elsewhere\n").unwrap();

    for (label, directory) in [
        ("a .git directory that is not a repository", empty_git_dir),
        ("a .git file that points nowhere", dangling),
    ] {
        let payload = fixture.hook_payload_in(&directory, &transcript, "Stop", "");
        let output = fixture.hook(&fixture.temp.path().join("home"), payload.as_bytes(), &[]);
        assert_visible_failure(&output, "Failed to discover Git repository", label);
        // Actionable: the entry Git could not open, and that it is Git that
        // could not.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let marker = directory.join(".git");
        assert!(
            stderr.contains(&format!(
                "Git cannot open the repository at {}",
                marker.display()
            )),
            "{label}: {stderr}"
        );
    }
}

/// A `cvc` entry that is not a directory is not "CVC is not set up here": it
/// is something where CVC's directory belongs, in a repository that may well
/// be capturing. It is a visible failure, not silence.
#[test]
fn hook_mode_reports_a_cvc_entry_that_is_not_a_directory() {
    let fixture = Fixture::new();
    let transcript = fixture.write_transcript(BASIC);
    let entry = fixture.repo.join(".git").join("cvc");
    let elsewhere = fixture.temp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();

    for label in [
        "a file",
        "a link that leads nowhere",
        "a link to a directory",
    ] {
        match label {
            "a file" => fs::write(&entry, "not a directory").unwrap(),
            "a link that leads nowhere" => {
                std::os::unix::fs::symlink("/nonexistent/cvc", &entry).unwrap()
            }
            _ => std::os::unix::fs::symlink(&elsewhere, &entry).unwrap(),
        }
        let payload = fixture.hook_payload(&transcript, "Stop", "");
        let output = fixture.hook(&fixture.temp.path().join("home"), payload.as_bytes(), &[]);
        assert_visible_failure(&output, "exists but is not a directory", label);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(entry.to_str().unwrap()),
            "{label}: {stderr}"
        );
        // The process's own directory, with no `cwd` in the payload, is judged
        // the same way.
        let output = fixture.hook(&fixture.repo, b"{}", &[]);
        assert_visible_failure(&output, "exists but is not a directory", label);
        // So is the project directory's repository, whatever the working
        // directory.
        let payload = fixture.hook_payload_in(&elsewhere, &transcript, "Stop", "");
        let project = [("CLAUDE_PROJECT_DIR", fixture.repo.to_str().unwrap())];
        let output = fixture.hook(&elsewhere, payload.as_bytes(), &project);
        assert_visible_failure(&output, "exists but is not a directory", label);
        fs::remove_file(&entry).unwrap();
    }
    // Nothing is where it belongs: CVC is not set up, and that is silence.
    let payload = fixture.hook_payload(&transcript, "Stop", "");
    assert_silent_success(
        &fixture.hook(&fixture.repo, payload.as_bytes(), &[]),
        "no cvc entry at all",
    );
}

/// Capture needs the repository's acknowledgement. An initialized repository
/// without it is the one case besides a bad payload where hook mode speaks, and
/// it names how to stop both kinds of hooks.
#[test]
fn hook_mode_refuses_visibly_in_a_cvc_repository_that_has_not_acknowledged_capture() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let transcript = fixture.write_transcript(BASIC);
    let payload = fixture.hook_payload(&transcript, "Stop", "");
    let project = fixture.repo.to_str().unwrap();
    for env in [&[][..], &[("CLAUDE_PROJECT_DIR", project)][..]] {
        let output = fixture.hook(&fixture.repo, payload.as_bytes(), env);
        assert_visible_failure(&output, "consent-required", "capture not acknowledged");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "consent-required: Claude Code transcript capture is passive collection and needs the repository-local capture acknowledgement; run `cvc privacy acknowledge-capture` first (or stop the hooks: `/plugin disable cvc@volute` for the Claude Code plugin, `cvc harness uninstall claude-code` for this checkout's hooks)\n"
        );
    }
    assert_eq!(fixture.interaction_count(), 0);
}

/// The plugin's main gain over the checkout's hooks: it needs nothing in the
/// checkout, so a linked worktree that has no `.claude/settings.local.json`
/// is captured all the same.
#[test]
fn hook_mode_ingests_in_a_linked_worktree_that_has_no_checkout_settings() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let worktree = fixture.temp.path().join("linked");
    fixture.git_with_hooks(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            worktree.to_str().unwrap(),
        ],
        &[],
    );
    assert!(!worktree.join(".claude/settings.local.json").exists());
    assert!(!fixture.repo.join(".claude/settings.local.json").exists());

    let transcript = fixture.write_transcript_for(&worktree, BASIC);
    let payload = fixture.hook_payload_in(&worktree, &transcript, "Stop", "");
    let output = fixture.hook(
        &worktree,
        payload.as_bytes(),
        &[("CLAUDE_PROJECT_DIR", worktree.to_str().unwrap())],
    );
    assert_silent_success(&output, "a linked worktree");
    assert_eq!(fixture.interaction_count(), 7);
}

/// Where nothing else is installed, hook mode ingests, whoever runs it: the
/// plugin's hook, a checkout's own, or a person's shell.
#[test]
fn hook_mode_ingests_where_the_checkout_has_no_entries_of_its_own() {
    let shipped_plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins/claude-code");
    for (project_dir, plugin_root) in [(false, false), (true, false), (true, true)] {
        let fixture = Fixture::new();
        fixture.cvc_ok(&["init"]);
        fixture.acknowledge_capture();
        assert!(!fixture.repo.join(".claude/settings.local.json").exists());
        let transcript = fixture.write_transcript(BASIC);
        let payload = fixture.hook_payload(&transcript, "Stop", "");
        let mut env = Vec::new();
        if project_dir {
            env.push(("CLAUDE_PROJECT_DIR", fixture.repo.to_str().unwrap()));
        }
        if plugin_root {
            env.push(("CLAUDE_PLUGIN_ROOT", shipped_plugin.to_str().unwrap()));
        }
        let output = fixture.hook(&fixture.repo, payload.as_bytes(), &env);
        assert_silent_success(&output, &format!("{env:?}"));
        assert_eq!(fixture.interaction_count(), 7, "{env:?}");
    }
}

/// Every file and directory under `root`, with its size and modification time:
/// equal before and after means nothing was created, removed or written.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, Option<SystemTime>)> {
    fn walk(directory: &Path, found: &mut BTreeMap<PathBuf, (u64, Option<SystemTime>)>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            found.insert(path.clone(), (metadata.len(), metadata.modified().ok()));
            if metadata.is_dir() {
                walk(&path, found);
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, &mut found);
    found
}

/// Claude Code loads a checkout's own settings only for sessions started in
/// it, but the user-scoped plugin's hooks fire in every session. A session is
/// recorded in the repository it was started in (`CLAUDE_PROJECT_DIR`), so one
/// that was started where there is no CVC repository records nothing, even when
/// its working directory has since moved into a capturing repository: its
/// transcript, from the first byte, is not taken into a repository it was never
/// started in.
#[test]
fn hook_mode_records_nothing_for_a_session_started_where_no_cvc_repository_is() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let sibling = fixture.git_repository("sibling-without-cvc");
    let sibling_subdirectory = sibling.join("src");
    fs::create_dir(&sibling_subdirectory).unwrap();
    let a_file = fixture.temp.path().join("a-file");
    fs::write(&a_file, "not a directory").unwrap();
    let missing = fixture.temp.path().join("never-created");
    let transcript = fixture.write_transcript(BASIC);
    // The session's working directory has moved into the repository.
    let payload = fixture.hook_payload(&transcript, "Stop", "");

    let started_elsewhere = [
        (
            "a folder above the repository, in no repository",
            fixture.temp.path().to_path_buf(),
        ),
        ("a sibling repository without CVC", sibling.clone()),
        ("a subdirectory of that repository", sibling_subdirectory),
        ("a directory that does not exist", missing),
        ("a file", a_file),
        // Claude Code did not say where: nowhere that can be named.
        ("an empty project directory", PathBuf::new()),
    ];
    let before = snapshot(fixture.temp.path());
    for (label, project) in &started_elsewhere {
        let env = [("CLAUDE_PROJECT_DIR", project.to_str().unwrap())];
        // Run in the repository, as a hook runs in the session's directory.
        let output = fixture.hook(&fixture.repo, payload.as_bytes(), &env);
        assert_silent_success(&output, label);
    }
    assert_eq!(
        snapshot(fixture.temp.path()),
        before,
        "nothing was created, removed or written anywhere"
    );
    assert_eq!(fixture.interaction_count(), 0);
    assert_eq!(fixture.interactions_in(&sibling), None);
}

/// A project directory that is something Git cannot open, or a repository whose
/// `cvc` entry is wrong, is judged as a working directory is: visibly, since
/// the session's capture may be what is being lost.
#[test]
fn hook_mode_judges_the_project_directory_as_it_judges_a_working_directory() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let transcript = fixture.write_transcript(BASIC);
    let payload = fixture.hook_payload(&transcript, "Stop", "");
    let broken = fixture.temp.path().join("broken");
    fs::create_dir_all(broken.join(".git")).unwrap();
    let odd = fixture.git_repository("odd-cvc-entry");
    fs::write(odd.join(".git").join("cvc"), "not a directory").unwrap();

    let output = fixture.hook(
        &fixture.repo,
        payload.as_bytes(),
        &[("CLAUDE_PROJECT_DIR", broken.to_str().unwrap())],
    );
    assert_visible_failure(
        &output,
        "Failed to discover Git repository",
        "a project directory Git cannot open",
    );
    let output = fixture.hook(
        &fixture.repo,
        payload.as_bytes(),
        &[("CLAUDE_PROJECT_DIR", odd.to_str().unwrap())],
    );
    assert_visible_failure(
        &output,
        "exists but is not a directory",
        "a project with a cvc entry that is not a directory",
    );
    assert_eq!(fixture.interaction_count(), 0);
}

/// The session is recorded in the repository it was started in, whole, wherever
/// it ends. A session started in R that moved into another capturing
/// repository, an `/add-dir` folder, a nested repository or anywhere else, and
/// ended there, still has all of it in R, and the place it ended gets nothing.
/// Which worktree of R it is attributed to follows the working directory only
/// while that is in R.
#[test]
fn hook_mode_records_a_session_in_the_repository_it_was_started_in_wherever_it_ends() {
    struct World {
        fixture: Fixture,
        linked: PathBuf,
        other: PathBuf,
        add_dir: PathBuf,
        nested: PathBuf,
        unrelated: PathBuf,
        missing: PathBuf,
    }
    let world = || {
        let fixture = Fixture::new();
        fixture.cvc_ok(&["init"]);
        fixture.acknowledge_capture();
        let linked = fixture.temp.path().join("linked");
        fixture.git_with_hooks(
            &fixture.repo,
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
            &[],
        );
        let other = fixture.capturing_repository("other-capturing");
        let add_dir = fixture.temp.path().join("added-with-add-dir");
        let nested = fixture.repo.join("nested");
        let unrelated = fixture.temp.path().join("unrelated");
        fs::create_dir(&add_dir).unwrap();
        fs::create_dir(&nested).unwrap();
        fs::create_dir(&unrelated).unwrap();
        fixture.git_in(&nested, &["init"]);
        let missing = fixture.temp.path().join("removed-since");
        World {
            fixture,
            linked,
            other,
            add_dir,
            nested,
            unrelated,
            missing,
        }
    };

    // Where the session started is R in every case; this is where it ends, and
    // the worktree of R that its captures are attributed to.
    type Ends = Box<dyn Fn(&World) -> (PathBuf, PathBuf)>;
    let cases: Vec<(&str, Ends)> = vec![
        (
            "in R, as before",
            Box::new(|w| (w.fixture.repo.clone(), w.fixture.repo.clone())),
        ),
        (
            "in a linked worktree of R",
            Box::new(|w| (w.linked.clone(), w.linked.clone())),
        ),
        (
            "in another initialized repository with capture acknowledged",
            Box::new(|w| (w.other.clone(), w.fixture.repo.clone())),
        ),
        (
            "in a folder added with /add-dir, in no repository",
            Box::new(|w| (w.add_dir.clone(), w.fixture.repo.clone())),
        ),
        (
            "in a nested repository",
            Box::new(|w| (w.nested.clone(), w.fixture.repo.clone())),
        ),
        (
            "in an unrelated folder",
            Box::new(|w| (w.unrelated.clone(), w.fixture.repo.clone())),
        ),
        (
            "in a directory that has since been removed",
            Box::new(|w| (w.missing.clone(), w.fixture.repo.clone())),
        ),
    ];

    let full = BASIC.to_owned();
    let cut = full.find("\"uuid\":\"s7\"").unwrap();
    let first_part = full[..full[..cut].rfind('\n').unwrap() + 1].to_owned();
    for (label, ends) in cases {
        let world = world();
        let fixture = &world.fixture;
        let (cwd, attributed_to) = ends(&world);
        let project = fixture.repo.to_str().unwrap();
        let env = [("CLAUDE_PROJECT_DIR", project)];
        let before_other = fixture.interactions_in(&world.other);

        // Mid-session, then the Stop that closes the final response; the same
        // counts as a session that stayed in R (3, then 7).
        let mut counts = Vec::new();
        for (event, content) in [("PostToolUse", &first_part), ("Stop", &full)] {
            let transcript = fixture.write_transcript(content);
            let payload = fixture.hook_payload_in(&cwd, &transcript, event, "");
            let output = fixture.hook(&cwd_or_repo(&cwd, fixture), payload.as_bytes(), &env);
            assert_silent_success(&output, &format!("{label}: {event}"));
            counts.push(fixture.interaction_count());
        }
        assert_eq!(counts, [3, 7], "{label}");
        assert_eq!(
            fixture.capture_worktrees(&fixture.repo),
            vec![cvc_core::repository::worktree_origin_fingerprint(&attributed_to).unwrap()],
            "{label}: attributed to the worktree of R it belongs to"
        );
        assert_eq!(
            fixture.interactions_in(&world.other),
            before_other,
            "{label}: the other repository received nothing"
        );
    }
}

/// The directory to start the hook process in: the session's working directory
/// when it exists, and otherwise the repository (a removed directory cannot be
/// one's working directory).
fn cwd_or_repo(cwd: &Path, fixture: &Fixture) -> PathBuf {
    if cwd.is_dir() {
        cwd.to_path_buf()
    } else {
        fixture.repo.clone()
    }
}

/// Consent is the project's repository's to give: a session started in an
/// unacknowledged repository is refused there, though it has moved into one
/// that has acknowledged capture, and that one gets nothing.
#[test]
fn hook_mode_asks_the_repository_the_session_started_in_for_consent() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let acknowledged = fixture.capturing_repository("acknowledged");
    let transcript = fixture.write_transcript(BASIC);
    let payload = fixture.hook_payload_in(&acknowledged, &transcript, "Stop", "");
    let project = fixture.repo.to_str().unwrap();

    let output = fixture.hook(
        &acknowledged,
        payload.as_bytes(),
        &[("CLAUDE_PROJECT_DIR", project)],
    );
    assert_visible_failure(&output, "consent-required", "the project is unacknowledged");
    assert_eq!(fixture.interaction_count(), 0);
    assert_eq!(fixture.interactions_in(&acknowledged), Some(0));
}

/// Without `CLAUDE_PROJECT_DIR` (an older Claude Code, a person's shell) there
/// is no start to go by, and the working directory chooses the repository, as it
/// always did.
#[test]
fn without_a_project_directory_the_working_directory_chooses_the_repository() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let other = fixture.capturing_repository("other-capturing");
    let transcript = fixture.write_transcript_for(&other, BASIC);
    let payload = fixture.hook_payload_in(&other, &transcript, "Stop", "");

    let output = fixture.hook(&fixture.repo, payload.as_bytes(), &[]);
    assert_silent_success(&output, "no project directory");
    assert_eq!(fixture.interactions_in(&other), Some(7));
    assert_eq!(fixture.interaction_count(), 0);
}

/// The session counts as started in the repository when its project directory
/// is in it: the same checkout or one of its subdirectories, any linked
/// worktree, however the path is spelled. Where the hook's working directory is
/// in the same repository, its worktree is the one captures are attributed to.
#[test]
fn hook_mode_records_a_session_started_in_the_same_repository() {
    // Where the session started (`None`: `CLAUDE_PROJECT_DIR` unset), and where
    // its working directory is when the hook runs, relative to the fixture.
    struct Layout {
        main: PathBuf,
        linked: PathBuf,
    }
    type Where = Box<dyn Fn(&Layout) -> (Option<PathBuf>, PathBuf)>;
    let cases: Vec<(&str, Where)> = vec![
        (
            "the repository itself",
            Box::new(|l| (Some(l.main.clone()), l.main.clone())),
        ),
        (
            "a subdirectory of the repository, working at its root",
            Box::new(|l| (Some(l.main.join("src")), l.main.clone())),
        ),
        (
            "the repository, working in a subdirectory",
            Box::new(|l| (Some(l.main.clone()), l.main.join("src"))),
        ),
        (
            "a linked worktree, working in the main checkout",
            Box::new(|l| (Some(l.linked.clone()), l.main.clone())),
        ),
        (
            "the main checkout, working in a linked worktree",
            Box::new(|l| (Some(l.main.clone()), l.linked.clone())),
        ),
        (
            "a subdirectory of a linked worktree, working in the main checkout",
            Box::new(|l| (Some(l.linked.join("src")), l.main.clone())),
        ),
        (
            "a link to the repository",
            Box::new(|l| (Some(l.main.with_file_name("alias")), l.main.clone())),
        ),
        (
            "no project directory (an older Claude Code, a manual run)",
            Box::new(|l| (None, l.main.clone())),
        ),
        (
            "no project directory, working in a linked worktree",
            Box::new(|l| (None, l.linked.clone())),
        ),
    ];
    for (label, place) in cases {
        let fixture = Fixture::new();
        fixture.cvc_ok(&["init"]);
        fixture.acknowledge_capture();
        let linked = fixture.temp.path().join("linked");
        fixture.git_with_hooks(
            &fixture.repo,
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
            &[],
        );
        fs::create_dir(fixture.repo.join("src")).unwrap();
        fs::create_dir(linked.join("src")).unwrap();
        std::os::unix::fs::symlink(&fixture.repo, fixture.temp.path().join("alias")).unwrap();
        let layout = Layout {
            main: fixture.repo.clone(),
            linked,
        };

        let (project, cwd) = place(&layout);
        let transcript = fixture.write_transcript_for(&cwd, BASIC);
        let payload = fixture.hook_payload_in(&cwd, &transcript, "Stop", "");
        let env: Vec<(&str, &str)> = project
            .iter()
            .map(|project| ("CLAUDE_PROJECT_DIR", project.to_str().unwrap()))
            .collect();
        let output = fixture.hook(&cwd, payload.as_bytes(), &env);
        assert_silent_success(&output, label);
        assert_eq!(fixture.interaction_count(), 7, "{label}");
        let worktree = if cwd.starts_with(&layout.linked) {
            &layout.linked
        } else {
            &layout.main
        };
        assert_eq!(
            fixture.capture_worktrees(&fixture.repo),
            vec![cvc_core::repository::worktree_origin_fingerprint(worktree).unwrap()],
            "{label}: attributed to the worktree the hook ran in"
        );
    }
}

/// The plugin's SessionStart check sends hook mode a project directory that
/// does not exist, as it does the working directory, because its own session's
/// `CLAUDE_PROJECT_DIR` may be an initialized repository. Without that the
/// payload's missing session would be reported there, as any real one's is.
/// This is why the script overrides the variable.
#[test]
fn the_capability_probe_needs_a_project_directory_that_is_not_a_capturing_repository() {
    const PROBE: &[u8] = br#"{"cwd":"/nonexistent/cvc-plugin-probe"}"#;
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let before = snapshot(fixture.temp.path());
    let project = fixture.repo.to_str().unwrap();
    let env = [("CLAUDE_PROJECT_DIR", project)];
    for (label, cwd) in [
        ("from the project", fixture.repo.clone()),
        ("from elsewhere", fixture.temp.path().join("home")),
    ] {
        assert_visible_failure(&fixture.hook(&cwd, PROBE, &env), "no session_id", label);
    }
    assert_eq!(snapshot(fixture.temp.path()), before);
    assert_eq!(fixture.interaction_count(), 0);
}

/// Claude Code writes the whole payload to the hook's stdin. A hook that
/// exits with part of an oversize one unread leaves the writer a broken pipe.
#[test]
fn hook_mode_reads_an_oversize_payload_to_the_end_before_it_answers() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let outside = fixture.temp.path().join("not-a-repository");
    fs::create_dir(&outside).unwrap();
    let uninitialized = fixture.git_repository("uninitialized");
    let oversize = format!("{{\"cwd\":\"x\"}}{}", " ".repeat(8 * 1024 * 1024));

    for (label, cwd, message) in [
        ("outside any repository", &outside, None),
        ("in a repository without CVC", &uninitialized, None),
        ("in a CVC repository", &fixture.repo, Some("exceeds 1 MiB")),
    ] {
        let mut child = fixture.spawn_hook(cwd, &[]);
        let written = child.stdin.take().unwrap().write_all(oversize.as_bytes());
        let output = child.wait_with_output().unwrap();
        assert!(written.is_ok(), "{label}: the writer got {written:?}");
        match message {
            None => assert_silent_success(&output, label),
            Some(message) => assert_visible_failure(&output, message, label),
        }
    }
}

/// The digest `cvc` names a session's ingest lock after, written out from its
/// description (see `cvc-core/src/claude_code/session_lock.rs`) so that this
/// test can hold the lock as another `cvc` process would.
fn session_lock_file(repo: &Path, session: &str) -> PathBuf {
    let mut hash = Sha256::new();
    hash.update(b"cvc.ingest-lock/v1\0claude-code\0");
    hash.update(session.as_bytes());
    repo.join(".git")
        .join("cvc")
        .join(format!("ingest-{}.lock", hex::encode(hash.finalize())))
}

fn ingest_lock_files(repo: &Path) -> Vec<String> {
    fs::read_dir(repo.join(".git").join("cvc"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("ingest-"))
        .collect()
}

/// The checkout's own entries and the plugin both run on every event, in
/// parallel and without knowing of each other, and a person may run `--transcript`
/// at any moment. Runs on one session take turns, and the result is what one
/// run makes of it.
#[test]
fn concurrent_hook_runs_on_one_session_take_turns_and_record_it_once() {
    let full = BASIC.to_owned();
    let cut = full.find("\"uuid\":\"s7\"").unwrap();
    let first_part = full[..full[..cut].rfind('\n').unwrap() + 1].to_owned();
    let shipped_plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins/claude-code");

    // What `rounds` leave behind: the interactions and the cursor of the
    // session, after each of two events. `parallel` runs per event.
    let outcome = |parallel: usize| {
        let fixture = Fixture::new();
        fixture.cvc_ok(&["init"]);
        fixture.acknowledge_capture();
        let project = fixture.repo.to_str().unwrap();
        let mut seen = Vec::new();
        for (event, content) in [("PostToolUse", &first_part), ("Stop", &full)] {
            let transcript = fixture.write_transcript(content);
            let payload = fixture.hook_payload(&transcript, event, "");
            // Even runs stand in for the plugin's hook, odd ones for the
            // checkout's own; both are given the project directory.
            let children: Vec<_> = (0..parallel)
                .map(|n| {
                    let mut env = vec![("CLAUDE_PROJECT_DIR", project)];
                    if n % 2 == 0 {
                        env.push(("CLAUDE_PLUGIN_ROOT", shipped_plugin.to_str().unwrap()));
                    }
                    fixture.spawn_hook(&fixture.repo, &env)
                })
                .collect();
            for (n, mut child) in children.into_iter().enumerate() {
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(payload.as_bytes())
                    .unwrap();
                let output = child.wait_with_output().unwrap();
                assert_silent_success(&output, &format!("{event}, run {n} of {parallel}"));
            }
            seen.push((fixture.interaction_count(), fixture.cursor()));
        }
        assert_eq!(
            ingest_lock_files(&fixture.repo),
            Vec::<String>::new(),
            "no lock file is left behind"
        );
        seen
    };

    let alone = outcome(1);
    assert_eq!(alone[0].0, 3);
    assert_eq!(alone[1].0, 7);
    assert_eq!(outcome(8), alone, "eight at once, as one at a time");
}

/// A run that cannot get the lock in time has not failed: the run that holds it
/// is making progress and the next event catches up from the cursor. A hook
/// says nothing, so a delay is not reported as an error; a person who asked for
/// a transcript by hand is told it was not ingested. (Takes the 20 s wait twice
/// over, in parallel.)
#[test]
fn a_run_that_waits_in_vain_for_the_sessions_lock_is_silent_as_a_hook_and_an_error_by_hand() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let transcript = fixture.write_transcript(BASIC);
    let payload = fixture.hook_payload(&transcript, "Stop", "");

    // Another cvc process, mid-ingest.
    let lock_file = session_lock_file(&fixture.repo, SESSION);
    let holder = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_file)
        .unwrap();
    fs::set_permissions(&lock_file, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX) }, 0);

    let started = std::time::Instant::now();
    let mut hook = fixture.spawn_hook(&fixture.repo, &[]);
    hook.stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let by_hand = fixture
        .command(env!("CARGO_BIN_EXE_cvc"))
        .args(["ingest", "claude-code", "--transcript"])
        .arg(&transcript)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let hook = hook.wait_with_output().unwrap();
    let by_hand = by_hand.wait_with_output().unwrap();
    let waited = started.elapsed();

    assert_silent_success(&hook, "the hook");
    assert_eq!(by_hand.status.code(), Some(1));
    assert!(by_hand.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&by_hand.stderr);
    assert!(
        stderr.contains("another cvc process is still ingesting Claude Code session")
            && stderr.contains(SESSION)
            && stderr.contains("did not finish within 20 s"),
        "{stderr}"
    );
    assert!(
        waited >= std::time::Duration::from_secs(19),
        "both waited for the lock: {waited:?}"
    );
    assert_eq!(fixture.interaction_count(), 0);

    // Once the holder is done, the next event ingests: nothing was lost.
    drop(holder);
    let output = fixture.hook(&fixture.repo, payload.as_bytes(), &[]);
    assert_silent_success(&output, "the next event");
    assert_eq!(fixture.interaction_count(), 7);
}

/// Public issue 102 for plugin users: the post-commit hook names the commit
/// when the commit was made from a Claude Code session that may be capturing
/// the repository, though nothing in the checkout says so.
#[test]
fn post_commit_hook_names_the_commit_for_a_claude_code_session_that_may_be_capturing() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let commit = |message: &str, env: &[(&str, &str)]| {
        fixture.git_with_hooks(
            &fixture.repo,
            &["commit", "-q", "--allow-empty", "-m", message],
            env,
        )
    };
    let head = || {
        git2::Repository::open(&fixture.repo)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string()
    };

    // Capture has not been acknowledged, so no session is capturing.
    let output = commit("unacknowledged", &[("CLAUDECODE", "1")]);
    assert!(!output.contains("CVC: Linked"), "{output}");

    fixture.acknowledge_capture();
    // Not from a Claude Code session.
    let output = commit("by hand", &[]);
    assert!(!output.contains("CVC: Linked"), "{output}");
    // The variable is a flag, set to 1.
    let output = commit("not a session", &[("CLAUDECODE", "0")]);
    assert!(!output.contains("CVC: Linked"), "{output}");
    // From a session, even with `-q`.
    let output = commit("from a session", &[("CLAUDECODE", "1")]);
    assert!(
        output.contains(&format!(
            "CVC: Linked 0 thought(s) to commit {}.",
            &head()[..12]
        )),
        "{output}"
    );
}
