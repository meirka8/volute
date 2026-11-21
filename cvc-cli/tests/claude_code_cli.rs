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

/// Cursor exports this to the hooks it runs, and the processes of its
/// integrated terminal may inherit it (not verified), so a test run from there
/// may too. Hook mode reads
/// it (public issue 189: it silences a payload without a session, and a
/// transcript with no complete entry yet), so, as with Claude Code's, no
/// command a test spawns may see it unless the test sets it itself. It is the
/// only `CURSOR_*` variable `cvc` reads.
const CURSOR_ENV: [&str; 1] = ["CURSOR_VERSION"];

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
        for name in CLAUDE_CODE_ENV.into_iter().chain(CURSOR_ENV) {
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

/// Public issue 183: `--rescan` re-reads the session from the start. Thoughts
/// already recorded are kept, so it records nothing and leaves the cursor where
/// a normal run left it; and it is an operator's option, never the hook's.
#[test]
fn ingest_rescan_rereads_the_session_and_changes_nothing_already_recorded() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let transcript = fixture.write_transcript(BASIC);
    let path = transcript.to_str().unwrap();

    let stdout = fixture.cvc_ok(&["ingest", "claude-code", "--transcript", path]);
    assert!(stdout.contains("7 new thought(s)"), "{stdout}");
    let cursor = fixture.cursor();
    assert!(cursor.is_some());

    for _ in 0..2 {
        let stdout = fixture.cvc_ok(&["ingest", "claude-code", "--transcript", path, "--rescan"]);
        assert!(stdout.contains("0 new thought(s)"), "{stdout}");
        assert!(stdout.contains("7 already recorded"), "{stdout}");
        assert_eq!(fixture.interaction_count(), 7);
        assert_eq!(fixture.cursor(), cursor);
    }

    // It names a transcript to re-read, and the hook never rescans.
    for args in [
        vec!["ingest", "claude-code", "--rescan"],
        vec!["ingest", "claude-code", "--hook", "--rescan"],
    ] {
        let output = fixture.cvc(&args);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("--rescan"),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fixture.interaction_count(), 7);
    }
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

/// `cvc log` shows what a person said, or a label of what the stimulus held,
/// never the boundary markers a harness capture is written with.
#[test]
fn log_shows_the_persons_first_line_or_a_label_not_the_stimulus_markers() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let transcript = fixture.write_transcript(BASIC);
    fixture.cvc_ok(&[
        "ingest",
        "claude-code",
        "--transcript",
        transcript.to_str().unwrap(),
    ]);

    let log = fixture.cvc_ok(&["log"]);
    assert!(log.contains("CVC Log (7 thought(s))"), "{log}");
    assert!(!log.contains("~cvc-"), "{log}");
    let shown: Vec<&str> = log
        .lines()
        .filter_map(|line| line.strip_prefix("Prom: "))
        .collect();
    assert_eq!(shown.len(), 7, "{log}");
    // The person's prompt (its meta text is not shown), the two results of one
    // response, the compaction summary, and results alone.
    assert_eq!(
        shown
            .iter()
            .filter(|line| **line == "Add a greeting to lib.rs...")
            .count(),
        1,
        "{shown:?}"
    );
    assert_eq!(
        shown
            .iter()
            .filter(|line| **line == "tool results (2)")
            .count(),
        1,
        "{shown:?}"
    );
    assert_eq!(
        shown
            .iter()
            .filter(|line| **line == "compaction summary")
            .count(),
        1,
        "{shown:?}"
    );
    assert_eq!(
        shown
            .iter()
            .filter(|line| **line == "tool results (1)")
            .count(),
        4,
        "{shown:?}"
    );
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

/// Copies the tree at `from` to `to`, file and directory permissions included,
/// as an archive, `cp -a` or a backup restore at another path does.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else if kind.is_file() {
            fs::copy(entry.path(), &target).unwrap();
        } else {
            std::os::unix::fs::symlink(fs::read_link(entry.path()).unwrap(), &target).unwrap();
        }
    }
    fs::set_permissions(to, fs::metadata(from).unwrap().permissions()).unwrap();
}

/// What capture refuses with in a repository whose acknowledgement on record
/// was given for another checkout or user (or by an older cvc).
const ACKNOWLEDGED_ELSEWHERE: &str = "consent-required: Claude Code transcript capture is passive collection and needs the repository-local capture acknowledgement; capture was acknowledged for another checkout or user, or by an older cvc; run `cvc privacy acknowledge-capture` here (or stop the hooks: `/plugin disable cvc@volute` for the Claude Code plugin, `cvc harness uninstall claude-code` for this checkout's hooks)";

/// A repository copied with its `.git` (an archive, `cp -a`, a backup
/// restore) carries the privacy state of the original, acknowledgement
/// included, but that acknowledgement was given for the original's path. The
/// copy captures nothing until capture is acknowledged in it: hook mode and a
/// manual ingest refuse visibly and say why, `harness install` refuses, and
/// `cvc privacy status` says why. The original still captures.
#[test]
fn a_copy_of_an_acknowledged_repository_captures_nothing_and_says_why() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let copy = fixture.temp.path().join("copy");
    copy_tree(&fixture.repo, &copy);
    let cvc_in = |dir: &Path, args: &[&str]| {
        fixture
            .command(env!("CARGO_BIN_EXE_cvc"))
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
    };

    let transcript = fixture.write_transcript_for(&copy, BASIC);
    let payload = fixture.hook_payload_in(&copy, &transcript, "Stop", "");
    let project = copy.to_str().unwrap();
    for env in [&[][..], &[("CLAUDE_PROJECT_DIR", project)][..]] {
        let output = fixture.hook(&copy, payload.as_bytes(), env);
        assert_visible_failure(&output, "consent-required", "a copied acknowledgement");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("{ACKNOWLEDGED_ELSEWHERE}\n")
        );
    }
    let output = cvc_in(
        &copy,
        &[
            "ingest",
            "claude-code",
            "--transcript",
            transcript.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!("{ACKNOWLEDGED_ELSEWHERE}\n")
    );
    assert_eq!(fixture.interactions_in(&copy), Some(0));

    let output = cvc_in(&copy, &["harness", "install", "claude-code"]);
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!("{ACKNOWLEDGED_ELSEWHERE}\n")
    );
    assert!(!copy.join(".claude").exists());

    let output = cvc_in(&copy, &["privacy", "status"]);
    assert!(output.status.success());
    let status = String::from_utf8_lossy(&output.stdout);
    assert!(
        status.starts_with("capture_acknowledged: false\nnote: capture was acknowledged for another checkout or user, or by an older cvc; run `cvc privacy acknowledge-capture` here\nsharing_consented: false\n"),
        "{status}"
    );

    // The original, where capture was acknowledged, records the session it
    // started, and its status has nothing to add.
    let transcript = fixture.write_transcript(BASIC);
    let payload = fixture.hook_payload(&transcript, "Stop", "");
    let project = fixture.repo.to_str().unwrap();
    let output = fixture.hook(
        &fixture.repo,
        payload.as_bytes(),
        &[("CLAUDE_PROJECT_DIR", project)],
    );
    assert_silent_success(&output, "the original");
    assert_eq!(fixture.interaction_count(), 7);
    let status = fixture.cvc_ok(&["privacy", "status"]);
    assert!(
        status.starts_with("capture_acknowledged: true\nsharing_consented: false\n"),
        "{status}"
    );
    assert_eq!(fixture.interactions_in(&copy), Some(0));
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
/// description (see `cvc-core/src/harness/lock.rs`) so that this
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

/// One transcript entry as Claude Code writes it, for the worktree placeholder
/// `write_transcript` fills in.
fn entry(
    n: usize,
    kind: &str,
    parent: Option<usize>,
    message: serde_json::Value,
    request: Option<&str>,
) -> String {
    let mut line = serde_json::json!({
        "parentUuid": parent.map(|p| format!("e{p}")),
        "isSidechain": false,
        "type": kind,
        "message": message,
        "uuid": format!("e{n}"),
        "timestamp": format!("2026-10-01T10:00:{n:02}.000Z"),
        "sessionId": SESSION,
        "version": "2.1.300",
        "cwd": "__WORKTREE__",
    });
    if let Some(request) = request {
        line["requestId"] = request.into();
    }
    line.to_string()
}

fn reply(text: &str) -> serde_json::Value {
    serde_json::json!({
        "model": "claude-fixture-1", "id": "msg_x", "type": "message", "role": "assistant",
        "content": [{"type": "text", "text": text}], "stop_reason": "end_turn",
        "stop_sequence": null, "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

/// A session whose first response reads a path the model chose: inside the
/// worktree, 5.4 KB, and with 300 matches the scrubber refuses to count.
fn hostile_read_session(extra_turn: bool) -> String {
    let hostile = format!(
        "__WORKTREE__/notes/{}",
        (0..300)
            .map(|n| format!("token=hunter2hunter{n} "))
            .collect::<String>()
    );
    let mut lines = vec![
        entry(
            1,
            "user",
            None,
            serde_json::json!({"role": "user", "content": "Summarise the notes"}),
            None,
        ),
        entry(
            2,
            "assistant",
            Some(1),
            serde_json::json!({
                "model": "claude-fixture-1", "id": "msg_a", "type": "message", "role": "assistant",
                "content": [{"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": hostile}}],
                "stop_reason": "tool_use", "stop_sequence": null,
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }),
            Some("req_A"),
        ),
        entry(
            3,
            "user",
            Some(2),
            serde_json::json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "the notes", "is_error": false}
            ]}),
            None,
        ),
        entry(4, "assistant", Some(3), reply("Summary."), Some("req_B")),
        entry(
            5,
            "user",
            Some(4),
            serde_json::json!({"role": "user", "content": "Thanks"}),
            None,
        ),
        entry(
            6,
            "assistant",
            Some(5),
            reply("You are welcome."),
            Some("req_C"),
        ),
    ];
    if extra_turn {
        lines.push(entry(
            7,
            "user",
            Some(6),
            serde_json::json!({"role": "user", "content": "One more thing"}),
            None,
        ));
        lines.push(entry(
            8,
            "assistant",
            Some(7),
            reply("Done."),
            Some("req_D"),
        ));
    }
    lines.join("\n") + "\n"
}

/// A response whose path cannot be scrubbed used to fail every ingest of its
/// session, because the cursor does not move when a batch fails. It is left out
/// now, the operator is told so, and hook mode (which prints nothing) goes on.
#[test]
fn a_path_the_scrubber_cannot_take_does_not_stop_the_session_from_being_recorded() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let transcript = fixture.write_transcript(&hostile_read_session(false));

    let stdout = fixture.cvc_ok(&[
        "ingest",
        "claude-code",
        "--transcript",
        transcript.to_str().unwrap(),
    ]);
    assert!(stdout.contains("3 new thought(s)"), "{stdout}");
    assert!(
        stdout.contains("left out of the responses recorded now: 0 tool call(s)"),
        "{stdout}"
    );
    assert!(stdout.contains("1 context item(s)"), "{stdout}");
    assert!(!stdout.contains("hunter2"), "{stdout}");
    assert_eq!(fixture.interaction_count(), 3);

    // The session grows; the next run records the new turn.
    fixture.write_transcript(&hostile_read_session(true));
    let stdout = fixture.cvc_ok(&[
        "ingest",
        "claude-code",
        "--transcript",
        transcript.to_str().unwrap(),
    ]);
    assert!(stdout.contains("1 new thought(s)"), "{stdout}");
    assert_eq!(fixture.interaction_count(), 4);
}

#[test]
fn hook_mode_goes_on_past_a_path_the_scrubber_cannot_take() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let transcript = fixture.write_transcript(&hostile_read_session(false));
    let output = fixture.cvc_with_stdin(
        &["ingest", "claude-code", "--hook"],
        &fixture.hook_payload(&transcript, "Stop", ""),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(fixture.interaction_count(), 3);

    fixture.write_transcript(&hostile_read_session(true));
    let output = fixture.cvc_with_stdin(
        &["ingest", "claude-code", "--hook"],
        &fixture.hook_payload(&transcript, "Stop", ""),
    );
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fixture.interaction_count(), 4);
}

// ---- Public issue 189: other agents run Claude Code's hooks -----------------
//
// Cursor loads Claude Code's hook settings by default (and sets
// `CLAUDE_PROJECT_DIR` itself), and Antigravity's CLI can import Claude Code
// plugins, so their sessions run `cvc ingest claude-code --hook` with their own
// payloads and transcripts. Those runs exit 0 and say nothing, and a payload
// with a top-level key that Claude Code does not send is set aside before any
// repository is looked for.

/// What a Cursor agent transcript's lines look like: a role and a message, and
/// none of the fields of a Claude Code entry. The text is made up.
const CURSOR_TRANSCRIPT: &str = concat!(
    r#"{"role":"user","message":{"content":[{"type":"text","text":"Say hello"}]}}"#,
    "\n",
    r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}"#,
    "\n",
);

/// A made-up value for Cursor's `CURSOR_VERSION` and `cursor_version`.
const CURSOR_VERSION: &str = "0.0.0-test";

/// A made-up conversation id for the other agents' payloads.
const OTHER_CONVERSATION: &str = "00000000-0000-4000-8000-0000000000c1";

/// Where hook mode fails once it opens the store of a repository that
/// [`Fixture::make_store_unopenable`] was run on.
const STORE_UNOPENABLE: &str = "CVC database is not an owned regular file";

/// A directory's own modification time, and [`snapshot`] of what is in it.
type DirectoryState = (
    Option<SystemTime>,
    BTreeMap<PathBuf, (u64, Option<SystemTime>)>,
);

impl Fixture {
    /// A transcript shaped like Cursor's, where Cursor keeps them.
    fn write_cursor_transcript(&self) -> PathBuf {
        let dir = self.home.join(".cursor/projects/example/agent-transcripts");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{OTHER_CONVERSATION}.jsonl"));
        fs::write(&path, CURSOR_TRANSCRIPT).unwrap();
        path
    }

    /// A transcript named `name` beside the session's own, with `content`
    /// as it is: one whose name does not name the session, say.
    fn write_transcript_named(&self, name: &str, content: &str) -> PathBuf {
        let dir = self.temp.path().join("transcripts");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    /// The repository's CVC directory, and everything in it with its size and
    /// modification time: equal before and after means no run created,
    /// removed or wrote anything there (the store, its journal, a session lock
    /// or the consent state). Only CVC's directory: Git's own background
    /// maintenance may still be writing under `.git/objects` after a commit.
    fn cvc_state(&self) -> DirectoryState {
        let cvc = self.repo.join(".git").join("cvc");
        let own = fs::symlink_metadata(&cvc).unwrap().modified().ok();
        (own, snapshot(&cvc))
    }

    /// Puts a directory where the repository's store belongs, so that a run
    /// that opens the store fails visibly ([`STORE_UNOPENABLE`]): a run that
    /// is silent in this repository never opened it. The consent state is a
    /// file of its own and is not touched.
    fn make_store_unopenable(&self) {
        let cvc = self.repo.join(".git").join("cvc");
        for name in ["index.db", "index.db-wal", "index.db-shm"] {
            match fs::remove_file(cvc.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("cannot remove {name}: {error}"),
            }
        }
        fs::create_dir(cvc.join("index.db")).unwrap();
    }
}

/// Hook payloads as Cursor sends them when it runs Claude Code's hooks, for a
/// session in `workspace` whose transcript is `transcript`: its own keys,
/// snake_case like Claude Code's, and no `session_id` or `cwd`. Under each of
/// the event names Cursor might give, and with either of its own keys alone.
fn cursor_payloads(workspace: &Path, transcript: &Path) -> Vec<(String, String)> {
    let full = serde_json::json!({
        "conversation_id": OTHER_CONVERSATION,
        "generation_id": "00000000-0000-4000-8000-0000000000c2",
        "model": "example-model",
        "hook_event_name": "stop",
        "cursor_version": CURSOR_VERSION,
        "workspace_roots": [workspace.to_str().unwrap()],
        "user_email": null,
        "transcript_path": transcript.to_str().unwrap(),
    });
    let mut payloads = Vec::new();
    for event in [
        "postToolUse",
        "stop",
        "sessionEnd",
        "PostToolUse",
        "Stop",
        "SessionEnd",
    ] {
        let mut payload = full.clone();
        payload["hook_event_name"] = event.into();
        payloads.push((format!("Cursor's {event}"), payload.to_string()));
    }
    for (only, left_out) in [
        ("cursor_version", "conversation_id"),
        ("conversation_id", "cursor_version"),
    ] {
        let mut payload = full.clone();
        payload.as_object_mut().unwrap().remove(left_out);
        payloads.push((format!("Cursor's, with {only} only"), payload.to_string()));
    }
    // Present is enough, whatever the value.
    let mut payload = full.clone();
    payload["conversation_id"] = serde_json::Value::Null;
    payload.as_object_mut().unwrap().remove("cursor_version");
    payloads.push((
        "Cursor's, with a null conversation_id only".into(),
        payload.to_string(),
    ));
    payloads
}

/// Hook payloads shaped as Antigravity's: camelCase keys, which Claude Code's
/// payloads never have at the top level. The keys its changelog documents
/// (`hookEventName`, `toolCall`, `lastUserInput`) as it would send them, then
/// every key of Antigravity's that hook mode knows, all together and each
/// alone.
fn antigravity_payloads(workspace: &Path, transcript: &Path) -> Vec<(String, String)> {
    let mut payloads = Vec::new();
    for event in ["PostToolUse", "Stop", "SessionStart"] {
        let payload = serde_json::json!({
            "hookEventName": event,
            "cwd": workspace.to_str().unwrap(),
            "toolCall": {"name": "run_command", "args": {"command": "true"}},
        });
        payloads.push((
            format!("Antigravity's documented {event}"),
            payload.to_string(),
        ));
    }
    let full = serde_json::json!({
        "hookEventName": "postToolUse",
        "toolCall": {"name": "run_command"},
        "lastUserInput": "Say hello",
        "conversationId": OTHER_CONVERSATION,
        "transcriptPath": transcript.to_str().unwrap(),
        "workspacePaths": [workspace.to_str().unwrap()],
    });
    payloads.push(("Antigravity's, every key".into(), full.to_string()));
    for (key, value) in full.as_object().unwrap() {
        let payload = serde_json::json!({ key: value });
        payloads.push((
            format!("Antigravity's, with {key} only"),
            payload.to_string(),
        ));
    }
    payloads
}

/// Runs each of another agent's `payloads` through hook mode in three places,
/// each of which makes a run fail visibly once it gets past a given step: a
/// CVC repository without the capture acknowledgement (the consent check), an
/// acknowledged one whose store cannot be opened (the store), and a project
/// directory Git cannot open (looking for the repository). A Claude Code
/// payload is run in each first, to show that it does fail there. Every one of
/// the other agent's runs must exit 0 in silence and leave CVC's directory as
/// it was, with and without the variables Cursor sets.
fn assert_ignored_before_any_repository_is_looked_for(
    payloads: fn(&Path, &Path) -> Vec<(String, String)>,
) {
    let unacknowledged = Fixture::new();
    unacknowledged.cvc_ok(&["init"]);
    let unopenable = Fixture::new();
    unopenable.cvc_ok(&["init"]);
    unopenable.acknowledge_capture();
    unopenable.make_store_unopenable();
    let broken = unacknowledged.temp.path().join("broken");
    fs::create_dir_all(broken.join(".git")).unwrap();

    // Each place speaks once a Claude Code run gets that far.
    for (fixture, needle) in [
        (&unacknowledged, "consent-required"),
        (&unopenable, STORE_UNOPENABLE),
    ] {
        let transcript = fixture.write_transcript(BASIC);
        let payload = fixture.hook_payload(&transcript, "Stop", "");
        let project = [("CLAUDE_PROJECT_DIR", fixture.repo.to_str().unwrap())];
        let output = fixture.hook(&fixture.repo, payload.as_bytes(), &project);
        assert_visible_failure(&output, needle, &format!("a Claude Code run ({needle})"));
    }
    let transcript = unacknowledged.write_transcript(BASIC);
    let payload = unacknowledged.hook_payload_in(&broken, &transcript, "Stop", "");
    let project = [("CLAUDE_PROJECT_DIR", broken.to_str().unwrap())];
    assert_visible_failure(
        &unacknowledged.hook(&broken, payload.as_bytes(), &project),
        "Failed to discover Git repository",
        "a Claude Code run in a project Git cannot open",
    );

    // The other agent's runs get past none of them.
    for fixture in [&unacknowledged, &unopenable] {
        let transcript = fixture.write_cursor_transcript();
        let repo = fixture.repo.to_str().unwrap();
        let before = fixture.cvc_state();
        for env in [
            &[][..],
            &[("CLAUDE_PROJECT_DIR", repo)][..],
            &[
                ("CLAUDE_PROJECT_DIR", repo),
                ("CURSOR_VERSION", CURSOR_VERSION),
            ][..],
        ] {
            for (label, payload) in payloads(&fixture.repo, &transcript) {
                let output = fixture.hook(&fixture.repo, payload.as_bytes(), env);
                assert_silent_success(&output, &format!("{label}, {env:?}"));
            }
        }
        assert_eq!(
            fixture.cvc_state(),
            before,
            "nothing was created, removed or written in CVC's directory"
        );
    }
    assert_eq!(unacknowledged.interaction_count(), 0);

    let transcript = unacknowledged.write_cursor_transcript();
    let broken_project = broken.to_str().unwrap();
    let before = snapshot(&broken);
    for (cwd, env) in [
        (&broken, &[][..]),
        (
            &unacknowledged.repo,
            &[("CLAUDE_PROJECT_DIR", broken_project)][..],
        ),
        (
            &unacknowledged.repo,
            &[
                ("CLAUDE_PROJECT_DIR", broken_project),
                ("CURSOR_VERSION", CURSOR_VERSION),
            ][..],
        ),
    ] {
        for (label, payload) in payloads(&broken, &transcript) {
            let output = unacknowledged.hook(cwd, payload.as_bytes(), env);
            assert_silent_success(
                &output,
                &format!("{label}, a project Git cannot open, {env:?}"),
            );
        }
    }
    assert_eq!(snapshot(&broken), before);
}

/// Cursor's payload (`cursor_version`, `conversation_id` without `session_id`,
/// `workspace_roots`)
/// naming Cursor's transcript: exit 0 in silence before any repository is
/// looked for, so before the consent check and without opening the store.
#[test]
fn hook_mode_ignores_a_cursor_run_before_it_looks_for_a_repository() {
    assert_ignored_before_any_repository_is_looked_for(cursor_payloads);
}

/// The same for Antigravity's camelCase payload (`hookEventName`, `toolCall`,
/// `lastUserInput`, `conversationId`, `transcriptPath`, `workspacePaths`).
#[test]
fn hook_mode_ignores_an_antigravity_run_before_it_looks_for_a_repository() {
    assert_ignored_before_any_repository_is_looked_for(antigravity_payloads);
}

/// Transcripts that begin with a line of another agent's, positively told:
/// Cursor's (a role, no type), Antigravity's steps (`step_index`) and Codex's
/// rollout envelope (`session_meta` with an object payload). The text and ids
/// are made up.
fn other_agents_transcripts() -> Vec<(&'static str, String)> {
    vec![
        ("Cursor's lines", CURSOR_TRANSCRIPT.to_owned()),
        ("after blank lines", format!("\n \r\n{CURSOR_TRANSCRIPT}")),
        (
            "with CRLF line ends",
            CURSOR_TRANSCRIPT.replace('\n', "\r\n"),
        ),
        (
            "followed by Claude Code's entries",
            format!("{CURSOR_TRANSCRIPT}{BASIC}"),
        ),
        (
            "Antigravity's steps",
            "{\"step_index\":0,\"source\":\"USER\",\"content\":\"Say hello\"}\n".to_owned(),
        ),
        (
            "Codex's rollout",
            "{\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"00000000-0000-4000-8000-0000000000d1\"}}\n".to_owned(),
        ),
    ]
}

/// A payload shaped as Claude Code's that names a transcript that begins with
/// another agent's line: after the consent check, the hook reads the
/// transcript's first complete line. When something else corroborates that
/// the run is not Claude Code's own (the file is not named for the payload's
/// session, as Claude Code's `<session>.jsonl` is, or `CURSOR_VERSION` is
/// set), it exits 0 in silence without opening the store, so nothing is
/// recorded and no cursor or lock is written. When nothing does, so that a
/// foreign line put at the top of a session's own transcript cannot silence
/// it, the hook fails visibly, `not a Claude Code transcript`, again without
/// opening the store. Without the acknowledgement it is the visible
/// `consent-required` any run there is.
#[test]
fn hook_mode_passes_over_a_transcript_that_does_not_begin_with_a_claude_code_entry() {
    let transcripts = other_agents_transcripts();
    let run_all = |fixture: &Fixture| {
        let repo = fixture.repo.to_str().unwrap();
        let without = [&[][..], &[("CLAUDE_PROJECT_DIR", repo)][..]];
        let under_cursor = [
            ("CLAUDE_PROJECT_DIR", repo),
            ("CURSOR_VERSION", CURSOR_VERSION),
        ];
        for (label, content) in &transcripts {
            for event in ["PostToolUse", "Stop", "SessionEnd"] {
                // Not named for the session: silent, with or without Cursor.
                let elsewhere =
                    fixture.write_transcript_named(&format!("{OTHER_CONVERSATION}.jsonl"), content);
                let payload = fixture.hook_payload(&elsewhere, event, "");
                for env in without.iter().copied().chain([&under_cursor[..]]) {
                    let output = fixture.hook(&fixture.repo, payload.as_bytes(), env);
                    assert_silent_success(
                        &output,
                        &format!("{label}, {event}, not the session's file, {env:?}"),
                    );
                }
                // The session's own file: silent only under Cursor.
                let own = fixture.write_transcript_named(&format!("{SESSION}.jsonl"), content);
                let payload = fixture.hook_payload(&own, event, "");
                let output = fixture.hook(&fixture.repo, payload.as_bytes(), &under_cursor);
                assert_silent_success(
                    &output,
                    &format!("{label}, {event}, the session's file, under Cursor"),
                );
                for env in without {
                    let output = fixture.hook(&fixture.repo, payload.as_bytes(), env);
                    let what = format!("{label}, {event}, the session's file, {env:?}");
                    assert_visible_failure(&output, "not a Claude Code transcript", &what);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    assert!(stderr.contains(own.to_str().unwrap()), "{what}: {stderr}");
                }
            }
        }
    };

    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let before = fixture.cvc_state();
    run_all(&fixture);
    assert_eq!(
        fixture.cvc_state(),
        before,
        "nothing was created, removed or written in CVC's directory"
    );
    assert_eq!(fixture.interaction_count(), 0);
    assert_eq!(fixture.cursor(), None);
    assert_eq!(ingest_lock_files(&fixture.repo), Vec::<String>::new());

    // The store is not opened for any of them, silent or not.
    let unopenable = Fixture::new();
    unopenable.cvc_ok(&["init"]);
    unopenable.acknowledge_capture();
    unopenable.make_store_unopenable();
    let transcript = unopenable.write_transcript(BASIC);
    let payload = unopenable.hook_payload(&transcript, "Stop", "");
    assert_visible_failure(
        &unopenable.hook(&unopenable.repo, payload.as_bytes(), &[]),
        STORE_UNOPENABLE,
        "a Claude Code transcript opens the store",
    );
    let before = unopenable.cvc_state();
    run_all(&unopenable);
    assert_eq!(unopenable.cvc_state(), before);

    // The consent check comes first, whoever's transcript it is.
    let unacknowledged = Fixture::new();
    unacknowledged.cvc_ok(&["init"]);
    let repo = unacknowledged.repo.to_str().unwrap();
    for transcript in [
        unacknowledged.write_transcript_named(&format!("{SESSION}.jsonl"), CURSOR_TRANSCRIPT),
        unacknowledged
            .write_transcript_named(&format!("{OTHER_CONVERSATION}.jsonl"), CURSOR_TRANSCRIPT),
    ] {
        for env in [
            &[][..],
            &[("CLAUDE_PROJECT_DIR", repo)][..],
            &[
                ("CLAUDE_PROJECT_DIR", repo),
                ("CURSOR_VERSION", CURSOR_VERSION),
            ][..],
        ] {
            let payload = unacknowledged.hook_payload(&transcript, "Stop", "");
            let output = unacknowledged.hook(&unacknowledged.repo, payload.as_bytes(), env);
            assert_visible_failure(
                &output,
                "consent-required",
                &format!("{transcript:?}, {env:?}"),
            );
        }
    }
    assert_eq!(unacknowledged.interaction_count(), 0);

    // A first line that does not end within 1 MiB is not judged: the parser
    // is, and it refuses it visibly, never with the blocking status, and
    // records nothing.
    let long = Fixture::new();
    long.cvc_ok(&["init"]);
    long.acknowledge_capture();
    let line = format!(
        "{{\"role\":\"user\",\"message\":{{\"content\":\"{}\"}}}}\n",
        "x".repeat(1024 * 1024)
    );
    let transcript = long.write_transcript(&line);
    let payload = long.hook_payload(&transcript, "Stop", "");
    assert_visible_failure(
        &long.hook(&long.repo, payload.as_bytes(), &[]),
        "entry has no type",
        "a first line longer than the bound",
    );
    assert_eq!(long.interaction_count(), 0);
}

/// A first line that is neither a Claude Code entry nor another known agent's
/// (not JSON, cut short, after a byte order mark, not an object, an empty
/// `type`) is never passed over, even with every corroboration there is (a file
/// not named for the session, `CURSOR_VERSION` set): it goes on to the store
/// and the parser, which report it. A damaged Claude Code transcript therefore
/// stays loud.
#[test]
fn hook_mode_leaves_a_first_line_of_nobody_known_to_the_parser() {
    let cut_short = format!("{}\n{BASIC}", &BASIC[..20]);
    let after_a_bom = format!("\u{feff}{BASIC}");
    let nobodys = [
        ("a first line cut short, then continued", cut_short.clone()),
        ("a byte order mark", after_a_bom.clone()),
        ("a JSON array", "[{\"type\":\"user\"}]\n".to_owned()),
        ("an empty type", "{\"type\":\"\"}\n".to_owned()),
        (
            "a null type beside a role",
            "{\"type\":null,\"role\":\"user\"}\n".to_owned(),
        ),
        ("not JSON", "not json at all\n".to_owned()),
    ];
    let unopenable = Fixture::new();
    unopenable.cvc_ok(&["init"]);
    unopenable.acknowledge_capture();
    unopenable.make_store_unopenable();
    let repo = unopenable.repo.to_str().unwrap();
    let under_cursor = [
        ("CLAUDE_PROJECT_DIR", repo),
        ("CURSOR_VERSION", CURSOR_VERSION),
    ];
    for (label, content) in &nobodys {
        let transcript =
            unopenable.write_transcript_named(&format!("{OTHER_CONVERSATION}.jsonl"), content);
        let payload = unopenable.hook_payload(&transcript, "Stop", "");
        assert_visible_failure(
            &unopenable.hook(&unopenable.repo, payload.as_bytes(), &under_cursor),
            STORE_UNOPENABLE,
            label,
        );
    }

    // Where the store opens, a damaged Claude Code transcript is the parser's
    // visible error, and nothing is recorded.
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let repo = fixture.repo.to_str().unwrap();
    for (label, content) in [("cut short", &cut_short), ("a BOM", &after_a_bom)] {
        let transcript = fixture.write_transcript(content);
        for env in [
            &[("CLAUDE_PROJECT_DIR", repo)][..],
            &[
                ("CLAUDE_PROJECT_DIR", repo),
                ("CURSOR_VERSION", CURSOR_VERSION),
            ][..],
        ] {
            for event in ["PostToolUse", "Stop"] {
                let payload = fixture.hook_payload(&transcript, event, "");
                assert_visible_failure(
                    &fixture.hook(&fixture.repo, payload.as_bytes(), env),
                    "not a JSON transcript entry",
                    &format!("{label}, {event}, {env:?}"),
                );
            }
        }
    }
    assert_eq!(fixture.interaction_count(), 0);
}

/// Waits up to 20 s for `child` and returns its output, or kills it and fails.
fn wait_with_timeout(mut child: std::process::Child, what: &str) -> Output {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what}: the hook did not finish within 20 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The first-line check opens the transcript once, and neither follows a link
/// (one to another agent's transcript, from a file not named for the session
/// and under Cursor, would otherwise be passed over in silence) nor waits for
/// a writer at a FIFO. What is not a regular file (a link, a FIFO, a directory)
/// is refused there, visibly, with the error ingestion gives it, and before the
/// store is opened: in a repository whose store cannot be opened the refusal is
/// the same, and CVC's directory is left as it was.
#[test]
fn hook_mode_neither_follows_a_link_nor_waits_on_a_fifo_at_the_transcript_path() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let unopenable = Fixture::new();
    unopenable.cvc_ok(&["init"]);
    unopenable.acknowledge_capture();
    unopenable.make_store_unopenable();
    // The trap works: a Claude Code transcript opens the store.
    let transcript = unopenable.write_transcript(BASIC);
    let payload = unopenable.hook_payload(&transcript, "Stop", "");
    assert_visible_failure(
        &unopenable.hook(&unopenable.repo, payload.as_bytes(), &[]),
        STORE_UNOPENABLE,
        "a Claude Code transcript opens the store",
    );

    for fixture in [&fixture, &unopenable] {
        let repo = fixture.repo.to_str().unwrap();
        let under_cursor = [
            ("CLAUDE_PROJECT_DIR", repo),
            ("CURSOR_VERSION", CURSOR_VERSION),
        ];
        let without = [("CLAUDE_PROJECT_DIR", repo)];
        let dir = fixture.temp.path().join("transcripts");
        fs::create_dir_all(&dir).unwrap();
        let link = dir.join("linked.jsonl");
        std::os::unix::fs::symlink(fixture.write_cursor_transcript(), &link).unwrap();
        let fifo = dir.join("fifo.jsonl");
        let status = Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(status.success(), "mkfifo");
        let directory = dir.join("directory.jsonl");
        fs::create_dir(&directory).unwrap();

        let before = fixture.cvc_state();
        for (label, path) in [
            ("a link to another agent's transcript", &link),
            ("a FIFO", &fifo),
            ("a directory", &directory),
        ] {
            for env in [&under_cursor[..], &without[..]] {
                for event in ["PostToolUse", "Stop"] {
                    let what = format!("{label}, {event}, {env:?}");
                    let payload = fixture.hook_payload(path, event, "");
                    let mut child = fixture.spawn_hook(&fixture.repo, env);
                    child
                        .stdin
                        .take()
                        .unwrap()
                        .write_all(payload.as_bytes())
                        .unwrap();
                    let output = wait_with_timeout(child, &what);
                    assert_visible_failure(&output, "is not a regular file", &what);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    assert!(stderr.contains(path.to_str().unwrap()), "{what}: {stderr}");
                    assert!(!stderr.contains(STORE_UNOPENABLE), "{what}: {stderr}");
                }
            }
        }
        assert_eq!(
            fixture.cvc_state(),
            before,
            "nothing was created, removed or written in CVC's directory"
        );
    }
    assert_eq!(fixture.interaction_count(), 0);
    assert_eq!(fixture.cursor(), None);
}

/// Under Cursor, a payload that could not be read whole within the 1 MiB
/// bound (a `PostToolUse` whose tool output is large, say) or that is not the
/// JSON Claude Code sends is taken for Cursor's own, and the hook exits 0 in
/// silence, without opening the store; nothing could be ingested from it in
/// any case. Without `CURSOR_VERSION` each is the visible failure it always
/// was.
#[test]
fn under_cursor_an_oversize_or_unparsable_payload_is_silent() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let transcript = fixture.write_transcript(BASIC);
    let oversize = fixture.hook_payload(
        &transcript,
        "PostToolUse",
        &format!(
            ",\"tool_name\":\"Bash\",\"tool_input\":{{\"command\":\"true\"}},\"tool_response\":{{\"stdout\":\"{}\"}}",
            "x".repeat(1024 * 1024 + 512 * 1024)
        ),
    );
    assert!(oversize.len() > 1024 * 1024);
    let payloads = [
        (
            "an oversize payload",
            oversize,
            "hook payload exceeds 1 MiB",
        ),
        (
            "a JSON array",
            "[1,2,3]".to_owned(),
            "hook payload is not the JSON Claude Code delivers on stdin",
        ),
        (
            "a JSON string",
            "\"stop\"".to_owned(),
            "hook payload is not the JSON Claude Code delivers on stdin",
        ),
        (
            "not JSON",
            "not json at all".to_owned(),
            "hook payload is not the JSON Claude Code delivers on stdin",
        ),
        (
            "an object of the wrong shape",
            format!(
                "{{\"session_id\":7,\"cwd\":\"{}\"}}",
                fixture.repo.display()
            ),
            "hook payload is not the JSON Claude Code delivers on stdin",
        ),
    ];

    let unopenable = Fixture::new();
    unopenable.cvc_ok(&["init"]);
    unopenable.acknowledge_capture();
    unopenable.make_store_unopenable();
    for fixture in [&fixture, &unopenable] {
        let repo = fixture.repo.to_str().unwrap();
        let under_cursor = [
            ("CLAUDE_PROJECT_DIR", repo),
            ("CURSOR_VERSION", CURSOR_VERSION),
        ];
        let without = [("CLAUDE_PROJECT_DIR", repo)];
        let empty_version = [("CLAUDE_PROJECT_DIR", repo), ("CURSOR_VERSION", "")];
        let before = fixture.cvc_state();
        for (label, payload, message) in &payloads {
            assert_silent_success(
                &fixture.hook(&fixture.repo, payload.as_bytes(), &under_cursor),
                &format!("{label}, under Cursor"),
            );
            for env in [&without[..], &empty_version[..]] {
                assert_visible_failure(
                    &fixture.hook(&fixture.repo, payload.as_bytes(), env),
                    message,
                    &format!("{label}, {env:?}"),
                );
            }
        }
        assert_eq!(
            fixture.cvc_state(),
            before,
            "nothing was created, removed or written in CVC's directory"
        );
    }
    assert_eq!(fixture.interaction_count(), 0);
    assert_eq!(fixture.cursor(), None);
}

/// Claude Code run in Cursor's integrated terminal may inherit `CURSOR_VERSION`
/// (not verified), and so may its hooks, along with the `CLAUDE_PROJECT_DIR`
/// Claude Code sets.
/// That never stops a well-formed Claude Code run from being recorded, nor a
/// transcript of a Claude Code version this cvc does not support from being
/// reported.
#[test]
fn cursor_version_in_the_environment_never_silences_a_claude_code_run() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let full = BASIC.to_owned();
    let cut = full.find("\"uuid\":\"s7\"").unwrap();
    let first_part = full[..full[..cut].rfind('\n').unwrap() + 1].to_owned();
    let repo = fixture.repo.to_str().unwrap();
    let under_cursor = [
        ("CLAUDE_PROJECT_DIR", repo),
        ("CURSOR_VERSION", CURSOR_VERSION),
    ];
    for (event, content, recorded) in [
        ("PostToolUse", &first_part, 3),
        ("Stop", &full, 7),
        // Once more: nothing new, nothing twice.
        ("SessionEnd", &full, 7),
    ] {
        let transcript = fixture.write_transcript(content);
        let payload = fixture.hook_payload(&transcript, event, "");
        let output = fixture.hook(&fixture.repo, payload.as_bytes(), &under_cursor);
        assert_silent_success(&output, event);
        assert_eq!(fixture.interaction_count(), recorded, "{event}");
    }
    assert!(fixture.cursor().is_some());

    // A Claude Code transcript of another major version is Claude Code's, and
    // its version is what is reported.
    let future = Fixture::new();
    future.cvc_ok(&["init"]);
    future.acknowledge_capture();
    let content = BASIC.replace("\"2.1.260\"", "\"3.0.0\"");
    assert_ne!(content, BASIC);
    let transcript = future.write_transcript(&content);
    let repo = future.repo.to_str().unwrap();
    for env in [
        &[("CLAUDE_PROJECT_DIR", repo)][..],
        &[
            ("CLAUDE_PROJECT_DIR", repo),
            ("CURSOR_VERSION", CURSOR_VERSION),
        ][..],
    ] {
        for event in ["PostToolUse", "Stop"] {
            let payload = future.hook_payload(&transcript, event, "");
            let output = future.hook(&future.repo, payload.as_bytes(), env);
            assert_visible_failure(
                &output,
                "this cvc supports transcript major version 2 only",
                &format!("{event}, {env:?}"),
            );
        }
    }
    assert_eq!(future.interaction_count(), 0);
}

/// What `CURSOR_VERSION` does silence: a payload without a session or a
/// transcript, which Claude Code never sends, and a transcript with no complete
/// line yet, which the next event reads. Without the variable, or with it
/// empty, the first is the failure it always was and the second goes on to the
/// store.
#[test]
fn under_cursor_a_payload_without_a_session_and_a_transcript_not_begun_are_silent() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let transcript = fixture.write_transcript(BASIC);
    let transcript = transcript.to_str().unwrap();
    let repo = fixture.repo.to_str().unwrap();
    let under_cursor = [
        ("CLAUDE_PROJECT_DIR", repo),
        ("CURSOR_VERSION", CURSOR_VERSION),
    ];
    let empty_version = [("CLAUDE_PROJECT_DIR", repo), ("CURSOR_VERSION", "")];
    let without = [("CLAUDE_PROJECT_DIR", repo)];

    let incomplete = [
        (
            "no session_id",
            serde_json::json!({"transcript_path": transcript, "cwd": repo, "hook_event_name": "Stop"}),
            "hook payload has no session_id",
        ),
        (
            "an empty session_id",
            serde_json::json!({"session_id": "", "transcript_path": transcript, "cwd": repo, "hook_event_name": "Stop"}),
            "hook payload has no session_id",
        ),
        (
            "no transcript_path",
            serde_json::json!({"session_id": SESSION, "cwd": repo, "hook_event_name": "Stop"}),
            "hook payload has no transcript_path",
        ),
        (
            "an empty transcript_path",
            serde_json::json!({"session_id": SESSION, "transcript_path": "", "cwd": repo, "hook_event_name": "Stop"}),
            "hook payload has no transcript_path",
        ),
        (
            "neither, as a payload of Cursor's without its own keys might be",
            serde_json::json!({"hook_event_name": "stop", "workspace_roots": [repo]}),
            "hook payload has no session_id",
        ),
    ];
    for (label, payload, message) in &incomplete {
        let payload = payload.to_string();
        assert_silent_success(
            &fixture.hook(&fixture.repo, payload.as_bytes(), &under_cursor),
            &format!("{label}, under Cursor"),
        );
        assert_visible_failure(
            &fixture.hook(&fixture.repo, payload.as_bytes(), &without),
            message,
            &format!("{label}, without CURSOR_VERSION"),
        );
        assert_visible_failure(
            &fixture.hook(&fixture.repo, payload.as_bytes(), &empty_version),
            message,
            &format!("{label}, with CURSOR_VERSION empty"),
        );
    }
    assert_eq!(fixture.interaction_count(), 0);

    // A transcript with no complete line yet is passed over without opening
    // the store; without the variable the run opens it, which says so here.
    let cursor_first_line = CURSOR_TRANSCRIPT.lines().next().unwrap().to_owned();
    let claude_code_first_line = BASIC.lines().next().unwrap().to_owned();
    let not_begun = [
        ("an empty transcript", String::new()),
        ("only blank lines", "\n \r\n".to_owned()),
        (
            "Cursor's first line, still being written",
            cursor_first_line,
        ),
        (
            "Claude Code's first line, still being written",
            claude_code_first_line,
        ),
    ];
    let unopenable = Fixture::new();
    unopenable.cvc_ok(&["init"]);
    unopenable.acknowledge_capture();
    unopenable.make_store_unopenable();
    let repo = unopenable.repo.to_str().unwrap();
    let under_cursor = [
        ("CLAUDE_PROJECT_DIR", repo),
        ("CURSOR_VERSION", CURSOR_VERSION),
    ];
    let without = [("CLAUDE_PROJECT_DIR", repo)];
    for (label, content) in &not_begun {
        let transcript = unopenable.write_transcript(content);
        for event in ["PostToolUse", "Stop"] {
            let payload = unopenable.hook_payload(&transcript, event, "");
            assert_silent_success(
                &unopenable.hook(&unopenable.repo, payload.as_bytes(), &under_cursor),
                &format!("{label}, {event}, under Cursor"),
            );
            assert_visible_failure(
                &unopenable.hook(&unopenable.repo, payload.as_bytes(), &without),
                STORE_UNOPENABLE,
                &format!("{label}, {event}, without CURSOR_VERSION"),
            );
        }
    }

    // Where the store opens, such a run records nothing and writes no cursor.
    let begun = Fixture::new();
    begun.cvc_ok(&["init"]);
    begun.acknowledge_capture();
    let transcript = begun.write_transcript("");
    let payload = begun.hook_payload(&transcript, "PostToolUse", "");
    let repo = begun.repo.to_str().unwrap();
    let before = begun.cvc_state();
    assert_silent_success(
        &begun.hook(
            &begun.repo,
            payload.as_bytes(),
            &[
                ("CLAUDE_PROJECT_DIR", repo),
                ("CURSOR_VERSION", CURSOR_VERSION),
            ],
        ),
        "an empty transcript, under Cursor",
    );
    assert_eq!(begun.cvc_state(), before);
    assert_eq!(begun.interaction_count(), 0);
    assert_eq!(begun.cursor(), None);
}

/// Only top-level keys tell another agent's payload. A tool's input and output
/// are the model's and the tool's to fill, and may carry any key, the other
/// agents' own among them: a Claude Code payload with such keys below the top
/// level is recorded as any other.
#[test]
fn hook_mode_records_a_claude_code_payload_whose_tool_input_carries_another_agents_keys() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let full = BASIC.to_owned();
    let cut = full.find("\"uuid\":\"s7\"").unwrap();
    let first_part = full[..full[..cut].rfind('\n').unwrap() + 1].to_owned();
    let others = serde_json::json!({
        "hookEventName": "PostToolUse",
        "toolCall": {"name": "run_command"},
        "lastUserInput": "Say hello",
        "transcriptPath": "/example.invalid/transcript.jsonl",
        "conversationId": OTHER_CONVERSATION,
        "workspacePaths": ["/example.invalid/workspace"],
        "cursor_version": CURSOR_VERSION,
        "conversation_id": OTHER_CONVERSATION,
        "nested": {"conversationId": OTHER_CONVERSATION, "transcriptPath": "/x"},
    });
    let mut tool_input = others.clone();
    tool_input["command"] = "true".into();
    let tool = format!(
        ",\"tool_name\":\"Bash\",\"tool_input\":{tool_input},\"tool_response\":[{others}],\"tool_use_id\":\"toolu_1\""
    );
    let repo = fixture.repo.to_str().unwrap();
    for (event, content, recorded) in [("PostToolUse", &first_part, 3), ("Stop", &full, 7)] {
        let transcript = fixture.write_transcript(content);
        let payload = fixture.hook_payload(&transcript, event, &tool);
        assert!(payload.contains("\"transcriptPath\""), "{payload}");
        serde_json::from_str::<serde_json::Value>(&payload).expect("the payload is JSON");
        let output = fixture.hook(
            &fixture.repo,
            payload.as_bytes(),
            &[("CLAUDE_PROJECT_DIR", repo)],
        );
        assert_silent_success(&output, event);
        assert_eq!(fixture.interaction_count(), recorded, "{event}");
    }
}

/// By hand, a file that is not a Claude Code transcript is an error that says
/// so (exit 1), raised after the consent check and before the session lock or
/// the store is touched, and told apart from a Claude Code transcript of a
/// version this cvc does not support.
#[test]
fn ingest_transcript_refuses_a_file_that_is_not_a_claude_code_transcript() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.acknowledge_capture();
    let transcript = fixture.write_transcript(CURSOR_TRANSCRIPT);
    let path = transcript.to_str().unwrap();
    let before = fixture.cvc_state();
    for args in [
        vec!["ingest", "claude-code", "--transcript", path],
        vec![
            "ingest",
            "claude-code",
            "--transcript",
            path,
            "--session",
            SESSION,
        ],
        vec!["ingest", "claude-code", "--transcript", path, "--rescan"],
    ] {
        let output = fixture.cvc(&args);
        assert_visible_failure(
            &output,
            "not a Claude Code transcript",
            &format!("{args:?}"),
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(path), "{args:?}: {stderr}");
    }
    assert_eq!(
        fixture.cvc_state(),
        before,
        "nothing was created, removed or written in CVC's directory"
    );
    assert_eq!(fixture.interaction_count(), 0);
    assert_eq!(fixture.cursor(), None);
    assert_eq!(ingest_lock_files(&fixture.repo), Vec::<String>::new());

    // A Claude Code transcript of another major version is reported as that.
    fixture.write_transcript(&BASIC.replace("\"2.1.260\"", "\"3.0.0\""));
    let output = fixture.cvc(&["ingest", "claude-code", "--transcript", path]);
    assert_visible_failure(
        &output,
        "this cvc supports transcript major version 2 only",
        "another major version",
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("not a Claude Code transcript"));
    assert_eq!(fixture.interaction_count(), 0);

    // The store is not opened for it.
    let unopenable = Fixture::new();
    unopenable.cvc_ok(&["init"]);
    unopenable.acknowledge_capture();
    unopenable.make_store_unopenable();
    let transcript = unopenable.write_transcript(BASIC);
    let path = transcript.to_str().unwrap();
    assert_visible_failure(
        &unopenable.cvc(&["ingest", "claude-code", "--transcript", path]),
        STORE_UNOPENABLE,
        "a Claude Code transcript opens the store",
    );
    unopenable.write_transcript(CURSOR_TRANSCRIPT);
    let output = unopenable.cvc(&["ingest", "claude-code", "--transcript", path]);
    assert_visible_failure(
        &output,
        "not a Claude Code transcript",
        "with a store that cannot be opened",
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains(STORE_UNOPENABLE));

    // Consent comes first.
    let unacknowledged = Fixture::new();
    unacknowledged.cvc_ok(&["init"]);
    let transcript = unacknowledged.write_transcript(CURSOR_TRANSCRIPT);
    assert_visible_failure(
        &unacknowledged.cvc(&[
            "ingest",
            "claude-code",
            "--transcript",
            transcript.to_str().unwrap(),
        ]),
        "consent-required",
        "capture not acknowledged",
    );
    assert_eq!(unacknowledged.interaction_count(), 0);
}
