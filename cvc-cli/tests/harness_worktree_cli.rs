#![cfg(unix)]

//! Public issue 113: `cvc harness install claude-code` propagates into new
//! worktrees through Git's `post-checkout` hook.
//!
//! Everything here drives the real `git` binary against throwaway
//! repositories, with a throwaway `HOME` and an explicit `PATH` that holds
//! only `git`. The Git hook runs the binary named in the repository's harness
//! record, so most tests record a logging wrapper around the built `cvc`
//! (`--binary`): that is how a test can tell whether the hook started `cvc` at
//! all. Nothing touches the developer's own repositories or `~/.cvc/bin`.
use serde_json::Value;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

const BASIC: &str = include_str!("../../cvc-core/tests/fixtures/claude-code/v2.1.260/basic.jsonl");
const SESSION: &str = "7c1e9f5a-2b2f-4f4e-9c3b-0f2e7a1d5b60";
const NULL_OID: &str = "0000000000000000000000000000000000000000";
const NEW_OID: &str = "1111111111111111111111111111111111111111";

const INSTALLED_LINE: &str =
    "CVC: installed the Claude Code capture hooks in this worktree (.claude/settings.local.json)";
const PROPAGATES_LINE: &str = "Claude Code sessions in worktrees created from now on (git worktree add, Claude Code's worktree option) are captured too: they get these hooks automatically while this repository's capture acknowledgement stands. `cvc harness uninstall claude-code` stops this.";
const NOT_RECORDED_LINE: &str = "New worktrees will not get these hooks until you run `cvc harness install claude-code` again with a current cvc in ~/.cvc/bin.";
const PER_CHECKOUT_LINE: &str =
    "This install is per checkout: run it again in each linked worktree you use.";
const NO_LONGER_LINE: &str = "New worktrees will no longer get the Claude Code hooks.";
const TRACKED_LINE: &str = "`.claude/settings.local.json` is tracked by Git in this checkout; CVC writes a machine-specific path there and will not modify a tracked file (untrack it with `git rm --cached .claude/settings.local.json`)";
const EXISTS_LINE: &str = "`.claude/settings.local.json` already exists in this worktree; CVC only creates it in a new worktree (run `cvc harness install claude-code` here to add the hooks)";
const DECLINED: &str = "CVC: Claude Code hooks not installed in this worktree: ";
const SETTINGS: &str = ".claude/settings.local.json";

/// One finished command.
struct Run {
    success: bool,
    stdout: String,
    stderr: String,
}

impl Run {
    /// Everything the command wrote; Git forwards hook output to its stderr.
    fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// The lines a CVC hook printed.
    fn cvc_lines(&self) -> Vec<String> {
        self.all()
            .lines()
            .filter(|line| line.starts_with("CVC"))
            .map(str::to_owned)
            .collect()
    }
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    /// A `PATH` directory holding `git` only: what a GUI-started Git sees.
    git_only: PathBuf,
    /// A logging wrapper around the built `cvc`; the binary tests record.
    wrapper: PathBuf,
    calls: PathBuf,
}

fn real_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git is on PATH")
}

fn executable(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Runs `command`, retrying while the kernel reports `ETXTBSY`. Tests copy a
/// binary and run it from parallel threads; a child forked by a sibling thread
/// can briefly hold the copy's write descriptor, which makes an exec of the
/// finished file fail with "Text file busy" for a moment.
fn output_retrying(command: &mut Command) -> std::process::Output {
    for _ in 0..100 {
        match command.output() {
            Err(error) if error.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            result => return result.unwrap(),
        }
    }
    command.output().unwrap()
}

/// Help text with Clap's line wrapping undone.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        let repo = root.join("repo");
        let git_only = root.join("path-git-only");
        let wrapper = root.join("wrapper/cvc");
        let calls = root.join("cvc-calls.log");
        for dir in [&home, &repo, &git_only] {
            fs::create_dir(dir).unwrap();
        }
        fs::create_dir(home.join("templates")).unwrap();
        symlink(real_git(), git_only.join("git")).unwrap();
        executable(
            &wrapper,
            &format!(
                "#!/bin/sh\nif [ -n \"${{CVC_CALL_LOG:-}}\" ]; then printf '%s\\n' \"$*\" >> \"$CVC_CALL_LOG\"; fi\nexec \"{}\" \"$@\"\n",
                env!("CARGO_BIN_EXE_cvc")
            ),
        );

        let fixture = Self {
            _temp: temp,
            root,
            home,
            repo,
            git_only,
            wrapper,
            calls,
        };
        let repo = fixture.repo.clone();
        fixture.git_ok(&repo, &["init"]);
        fixture.git_ok(&repo, &["config", "user.email", "test@example.invalid"]);
        fixture.git_ok(&repo, &["config", "user.name", "CVC test"]);
        fs::write(repo.join("tracked"), "initial\n").unwrap();
        fixture.git_ok(&repo, &["add", "tracked"]);
        fixture.git_ok(&repo, &["commit", "-m", "initial"]);
        fixture
    }

    /// A repository with `cvc init` run and local capture acknowledged.
    fn ready() -> Self {
        let fixture = Self::new();
        fixture.cvc_ok(&fixture.repo, &["init"]);
        fixture.acknowledge();
        fixture
    }

    fn acknowledge(&self) {
        // The acknowledgement is a typed TTY challenge in the CLI; tests grant
        // it through the core API exactly as the challenge would.
        let repository = git2::Repository::open(&self.repo).unwrap();
        cvc_core::privacy::acknowledge_capture(&repository).unwrap();
    }

    fn run(&self, program: impl AsRef<OsStr>, cwd: &Path, path: &Path, args: &[&str]) -> Run {
        let mut command = Command::new(program);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("CVC_CALL_LOG", &self.calls)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("empty.gitconfig"))
            .env("GIT_TEMPLATE_DIR", self.home.join("templates"))
            .stdin(Stdio::null());
        let output = output_retrying(&mut command);
        Run {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Run {
        self.run(real_git(), cwd, &self.git_only, args)
    }

    fn git_ok(&self, cwd: &Path, args: &[&str]) -> Run {
        let run = self.git(cwd, args);
        assert!(run.success, "git {args:?} failed: {}", run.stderr);
        run
    }

    /// The built `cvc`, run directly.
    fn cvc(&self, cwd: &Path, args: &[&str]) -> Run {
        self.run(env!("CARGO_BIN_EXE_cvc"), cwd, &self.git_only, args)
    }

    fn cvc_ok(&self, cwd: &Path, args: &[&str]) -> Run {
        let run = self.cvc(cwd, args);
        assert!(run.success, "cvc {args:?} failed: {}", run.stderr);
        run
    }

    /// `cvc harness install claude-code --binary <wrapper>`.
    fn install(&self, cwd: &Path) -> Run {
        self.cvc_ok(
            cwd,
            &[
                "harness",
                "install",
                "claude-code",
                "--binary",
                self.wrapper.to_str().unwrap(),
            ],
        )
    }

    /// `git worktree add <root>/<name> -b <name>`.
    fn worktree_add(&self, name: &str) -> (PathBuf, Run) {
        let worktree = self.root.join(name);
        let run = self.git(
            &self.repo,
            &["worktree", "add", worktree.to_str().unwrap(), "-b", name],
        );
        (fs::canonicalize(&worktree).unwrap_or(worktree), run)
    }

    fn record_path(&self) -> PathBuf {
        self.repo.join(".git/cvc/harness/claude-code.path")
    }

    /// The post-checkout invocations that started the recorded wrapper.
    fn post_checkout_calls(&self) -> Vec<String> {
        fs::read_to_string(&self.calls)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with("hook post-checkout"))
            .map(str::to_owned)
            .collect()
    }

    fn clear_calls(&self) {
        let _ = fs::remove_file(&self.calls);
    }

    /// Commits `(mode, path, content)` files on a new branch built with
    /// plumbing, so no working tree (the main checkout's included) changes.
    fn branch_with(&self, branch: &str, files: &[(&str, &str, &str)]) {
        let index = self.root.join(format!("index-{branch}"));
        let git = |args: &[&str], stdin: Option<&str>| -> String {
            let mut command = Command::new(real_git());
            for (name, _) in std::env::vars_os() {
                if name.to_string_lossy().starts_with("GIT_") {
                    command.env_remove(name);
                }
            }
            command
                .current_dir(&self.repo)
                .env("HOME", &self.home)
                .env("PATH", &self.git_only)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", self.home.join("empty.gitconfig"))
                .env("GIT_INDEX_FILE", &index)
                .env("GIT_AUTHOR_NAME", "QA")
                .env("GIT_AUTHOR_EMAIL", "qa@example.invalid")
                .env("GIT_COMMITTER_NAME", "QA")
                .env("GIT_COMMITTER_EMAIL", "qa@example.invalid")
                .args(args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::piped());
            let mut child = command.spawn().unwrap();
            {
                use std::io::Write;
                let mut pipe = child.stdin.take().unwrap();
                if let Some(text) = stdin {
                    pipe.write_all(text.as_bytes()).unwrap();
                }
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&["read-tree", "HEAD"], None);
        for (mode, path, content) in files {
            let blob = git(&["hash-object", "-w", "--stdin"], Some(content));
            git(
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("{mode},{blob},{path}"),
                ],
                None,
            );
        }
        let tree = git(&["write-tree"], None);
        let commit = git(&["commit-tree", &tree, "-p", "HEAD", "-m", branch], None);
        git(&["branch", branch, &commit], None);
        let _ = fs::remove_file(index);
    }
}

fn settings(worktree: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(worktree.join(SETTINGS)).unwrap()).unwrap()
}

fn canonical(path: impl AsRef<Path>) -> PathBuf {
    fs::canonicalize(path).unwrap()
}

/// The three Claude Code events, each carrying exactly one CVC entry that runs
/// `binary`.
fn assert_hooks_run(worktree: &Path, binary: &Path) {
    let settings = settings(worktree);
    for event in ["PostToolUse", "Stop", "SessionEnd"] {
        let groups = settings["hooks"][event].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{event}: {settings}");
        let command = groups[0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(
            command,
            format!("'{}' ingest claude-code --hook", binary.display()),
            "{event}"
        );
    }
}

fn interaction_counts(fixture: &Fixture, worktree: &Path) -> (i64, i64) {
    let connection = rusqlite::Connection::open(fixture.repo.join(".git/cvc/index.db")).unwrap();
    let all = connection
        .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
        .unwrap();
    let origin = cvc_core::repository::worktree_origin_fingerprint(worktree).unwrap();
    let there = connection
        .query_row(
            "SELECT COUNT(*) FROM interactions WHERE capture_worktree = ?1",
            [origin],
            |row| row.get(0),
        )
        .unwrap();
    (all, there)
}

#[test]
fn a_new_worktree_gets_the_hooks_that_harness_install_recorded() {
    let fixture = Fixture::ready();
    let installed = fixture.install(&fixture.repo);
    assert!(
        installed.stdout.contains(PROPAGATES_LINE),
        "{}",
        installed.stdout
    );
    assert!(
        !installed.stdout.contains("per checkout"),
        "{}",
        installed.stdout
    );
    // `cvc init` already placed the Git hook, so there is nothing to announce.
    assert!(
        !installed.stdout.contains("post-checkout"),
        "{}",
        installed.stdout
    );

    // The record is the path and a newline, private to its owner.
    assert_eq!(
        fs::read_to_string(fixture.record_path()).unwrap(),
        format!("{}\n", fixture.wrapper.display())
    );
    assert_eq!(
        fs::metadata(fixture.record_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    // `git worktree add`: exit 0, the line is in Git's output, and the new
    // worktree carries the same entries as the main checkout.
    fixture.clear_calls();
    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.success, "{}", run.stderr);
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert_eq!(fixture.post_checkout_calls().len(), 1);
    assert_hooks_run(&wt, &fixture.wrapper);
    assert_eq!(settings(&wt)["hooks"], settings(&fixture.repo)["hooks"]);

    // The file is machine-specific and must never reach a commit.
    assert!(fixture.git(&wt, &["check-ignore", "-q", SETTINGS]).success);
    let status = fixture.git_ok(&wt, &["status", "--porcelain"]);
    assert!(!status.stdout.contains(".claude"), "{}", status.stdout);

    // Detached worktrees are new worktrees too, and each gets its own entries.
    let detached = fixture.root.join("detached");
    let run = fixture.git(
        &fixture.repo,
        &["worktree", "add", "--detach", detached.to_str().unwrap()],
    );
    assert!(run.success, "{}", run.stderr);
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert_hooks_run(&detached, &fixture.wrapper);

    // The session that starts there is captured, and attributed to it: run the
    // very command the new settings file holds.
    let transcript = fixture.root.join(format!("{SESSION}.jsonl"));
    fs::write(
        &transcript,
        BASIC.replace("__WORKTREE__", wt.to_str().unwrap()),
    )
    .unwrap();
    let payload = format!(
        "{{\"session_id\":\"{SESSION}\",\"transcript_path\":\"{}\",\"cwd\":\"{}\",\"hook_event_name\":\"Stop\"}}",
        transcript.display(),
        wt.display()
    );
    let command = settings(&wt)["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut child = Command::new("/bin/sh")
        .args(["-c", &command])
        .current_dir(&wt)
        .env("PATH", &fixture.git_only)
        .env("HOME", &fixture.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), payload.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(interaction_counts(&fixture, &wt), (7, 7));
}

#[test]
fn harness_install_records_the_running_binary_by_default() {
    let fixture = Fixture::ready();
    let installed = fixture.cvc_ok(&fixture.repo, &["harness", "install", "claude-code"]);
    assert!(
        installed.stdout.contains(PROPAGATES_LINE),
        "{}",
        installed.stdout
    );
    let binary = canonical(env!("CARGO_BIN_EXE_cvc"));
    assert_eq!(
        fs::read_to_string(fixture.record_path()).unwrap(),
        format!("{}\n", binary.display())
    );
    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert_hooks_run(&wt, &binary);
}

#[test]
fn without_a_record_new_worktrees_get_no_settings_file() {
    let fixture = Fixture::ready();
    // The harness was never installed in this repository.
    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.success, "{}", run.stderr);
    assert!(!run.all().contains("CVC"), "{}", run.all());
    assert!(!wt.join(".claude").exists());
    assert!(!fixture.record_path().exists());
}

#[test]
fn uninstall_clears_the_record_and_names_worktrees_that_still_carry_hooks() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    let (first, run) = fixture.worktree_add("first");
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert!(fixture.record_path().exists());

    let removed = fixture.cvc_ok(&fixture.repo, &["harness", "uninstall", "claude-code"]);
    assert!(
        removed.stdout.contains("Removed 3 CVC hook entries"),
        "{}",
        removed.stdout
    );
    assert!(
        removed.stdout.contains(NO_LONGER_LINE),
        "{}",
        removed.stdout
    );
    assert!(
        removed.stdout.contains(
            "Other worktrees still carry these hooks and keep capturing (run `cvc harness uninstall claude-code` there):"
        ),
        "{}",
        removed.stdout
    );
    assert!(
        removed.stdout.contains(&format!("  {}", first.display())),
        "{}",
        removed.stdout
    );
    assert!(!fixture.record_path().exists());
    assert!(!fixture.repo.join(SETTINGS).exists());
    // Other worktrees' files are theirs; uninstalling here left them alone.
    assert_hooks_run(&first, &fixture.wrapper);

    // The next new worktree gets nothing.
    let (second, run) = fixture.worktree_add("second");
    assert!(run.success, "{}", run.stderr);
    assert!(!run.all().contains("CVC"), "{}", run.all());
    assert!(!second.join(".claude").exists());

    // Following the advice there finishes the job, and says nothing more
    // about new worktrees (the record is already gone).
    let removed = fixture.cvc_ok(&first, &["harness", "uninstall", "claude-code"]);
    assert!(
        removed.stdout.contains("Removed 3 CVC hook entries"),
        "{}",
        removed.stdout
    );
    assert!(
        !removed.stdout.contains(NO_LONGER_LINE),
        "{}",
        removed.stdout
    );
    assert!(
        !removed.stdout.contains("still carry"),
        "{}",
        removed.stdout
    );
    assert!(!first.join(SETTINGS).exists());
}

#[test]
fn uninstall_clears_the_record_even_when_this_checkout_has_no_entries() {
    let fixture = Fixture::ready();
    // Created before the install, so it carries no entries of its own.
    let (early, _) = fixture.worktree_add("early");
    fixture.install(&fixture.repo);
    assert!(fixture.record_path().exists());

    let removed = fixture.cvc_ok(&early, &["harness", "uninstall", "claude-code"]);
    assert!(
        removed.stdout.contains("No CVC hook entries found in"),
        "{}",
        removed.stdout
    );
    assert!(
        removed.stdout.contains(NO_LONGER_LINE),
        "{}",
        removed.stdout
    );
    // The main worktree still carries its entries and is named.
    assert!(
        removed
            .stdout
            .contains(&format!("  {}", canonical(&fixture.repo).display())),
        "{}",
        removed.stdout
    );
    assert!(!fixture.record_path().exists());
    assert!(fixture.repo.join(SETTINGS).exists());
}

#[test]
fn ordinary_checkouts_never_start_cvc_or_touch_settings() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.success, "{}", run.stderr);
    fs::remove_file(wt.join(SETTINGS)).unwrap();
    fixture.clear_calls();

    for args in [
        vec!["checkout", "-b", "other"],
        vec!["switch", "-"],
        vec!["switch", "-c", "third"],
        vec!["checkout", "--detach", "HEAD"],
        vec!["checkout", "--", "tracked"],
    ] {
        let run = fixture.git(&wt, &args);
        assert!(run.success, "git {args:?}: {}", run.stderr);
        assert!(!run.all().contains("CVC"), "git {args:?}: {}", run.all());
        assert!(!wt.join(SETTINGS).exists(), "git {args:?} wrote settings");
    }
    // The hook's shell filter kept `cvc` from starting at all.
    assert_eq!(fs::read_to_string(&fixture.calls).unwrap_or_default(), "");

    // `--no-checkout` runs no post-checkout hook, so there is nothing to do.
    let bare = fixture.root.join("no-checkout");
    let run = fixture.git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "--no-checkout",
            bare.to_str().unwrap(),
            "-b",
            "no-checkout",
        ],
    );
    assert!(run.success, "{}", run.stderr);
    assert!(!bare.join(".claude").exists());
    assert_eq!(fixture.post_checkout_calls(), Vec::<String>::new());
}

#[test]
fn the_hook_runs_the_recorded_binary_whatever_is_on_path_or_home() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);

    // A stale `cvc` first on PATH -- an old release without the subcommand, or
    // the launcher that downloads on every run -- and one at the stable
    // location. Neither may be consulted.
    let stale_dir = fixture.root.join("path-stale");
    fs::create_dir(&stale_dir).unwrap();
    symlink(real_git(), stale_dir.join("git")).unwrap();
    let stale_log = fixture.root.join("stale.log");
    let stale = format!(
        "#!/bin/sh\necho \"$0 $*\" >> \"{}\"\necho \"error: unrecognized subcommand 'post-checkout'\" >&2\nexit 2\n",
        stale_log.display()
    );
    executable(&stale_dir.join("cvc"), &stale);
    executable(&fixture.home.join(".cvc/bin/cvc"), &stale);

    let worktree = fixture.root.join("wt");
    let run = fixture.run(
        real_git(),
        &fixture.repo,
        &stale_dir,
        &["worktree", "add", worktree.to_str().unwrap(), "-b", "wt"],
    );
    assert!(run.success, "{}", run.stderr);
    assert_eq!(run.cvc_lines(), [INSTALLED_LINE], "{}", run.all());
    assert!(!run.all().contains("unrecognized subcommand"));
    assert!(!stale_log.exists(), "a cvc found by lookup was started");
    assert_hooks_run(&worktree, &fixture.wrapper);

    // No HOME at all, and a PATH that holds nothing but Git, as a GUI app
    // might hand it on.
    let mut command = Command::new(real_git());
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    let nohome = fixture.root.join("nohome");
    let output = command
        .args(["worktree", "add", nohome.to_str().unwrap(), "-b", "nohome"])
        .current_dir(&fixture.repo)
        .env_remove("HOME")
        .env("PATH", &fixture.git_only)
        .env("CVC_CALL_LOG", &fixture.calls)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", fixture.home.join("empty.gitconfig"))
        .env("GIT_TEMPLATE_DIR", fixture.home.join("templates"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains(INSTALLED_LINE));
    assert_hooks_run(&nohome, &fixture.wrapper);
}

#[test]
fn a_deleted_recorded_binary_is_reported_and_never_fails_git() {
    let fixture = Fixture::ready();
    let custom = fixture.root.join("custom/cvc");
    fs::create_dir_all(custom.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_cvc"), &custom).unwrap();
    fixture.cvc_ok(
        &fixture.repo,
        &[
            "harness",
            "install",
            "claude-code",
            "--binary",
            custom.to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read_to_string(fixture.record_path()).unwrap(),
        format!("{}\n", custom.display())
    );
    fs::remove_file(&custom).unwrap();

    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.success, "{}", run.stderr);
    let expected = format!(
        "{DECLINED}{} does not exist; run `cvc harness install claude-code` here",
        custom.display()
    );
    assert_eq!(run.cvc_lines(), [expected.as_str()], "{}", run.stderr);
    assert!(!wt.join(".claude").exists());

    // The same when it is there but has lost its execute bit.
    fs::write(&custom, "").unwrap();
    fs::set_permissions(&custom, fs::Permissions::from_mode(0o644)).unwrap();
    let (wt, run) = fixture.worktree_add("wt2");
    assert!(run.success, "{}", run.stderr);
    let expected = format!(
        "{DECLINED}{} is not an executable file; run `cvc harness install claude-code` here",
        custom.display()
    );
    assert_eq!(run.cvc_lines(), [expected.as_str()], "{}", run.stderr);
    assert!(!wt.join(".claude").exists());
}

#[test]
fn a_missing_acknowledgement_is_reported_and_never_fails_git() {
    // A record without the acknowledgement: written through the core API, as
    // `harness install` itself refuses to run without it.
    let fixture = Fixture::new();
    fixture.cvc_ok(&fixture.repo, &["init"]);
    let layout = cvc_core::repository::RepositoryLayout::discover(&fixture.repo).unwrap();
    cvc_core::claude_code::harness_record::write_record(&layout, &fixture.wrapper).unwrap();

    let expected = format!("{DECLINED}local capture is not acknowledged for this repository");
    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.success, "{}", run.stderr);
    assert_eq!(run.cvc_lines(), [expected.as_str()], "{}", run.stderr);
    assert!(!wt.join(".claude").exists());

    // The same when an installed repository loses its acknowledgement later.
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    let (first, run) = fixture.worktree_add("first");
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert!(first.join(SETTINGS).exists());
    fs::remove_file(fixture.repo.join(".git/cvc/privacy.json")).unwrap();
    let (second, run) = fixture.worktree_add("second");
    assert!(run.success, "{}", run.stderr);
    assert!(run.stderr.contains(&expected), "{}", run.stderr);
    assert!(!second.join(".claude").exists());
}

#[test]
fn a_clone_is_a_no_op_even_when_the_hook_runs() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    // Git copies the template's hooks into a clone, and runs post-checkout
    // there with a null previous ref and flag 1: the same call a new worktree
    // makes. Template the managed hook so that call really happens.
    let template_hooks = fixture.home.join("templates/hooks");
    fs::create_dir_all(&template_hooks).unwrap();
    fs::copy(
        fixture.repo.join(".git/hooks/post-checkout"),
        template_hooks.join("post-checkout"),
    )
    .unwrap();
    fixture.clear_calls();

    let clone = fixture.root.join("clone");
    let run = fixture.git(
        &fixture.repo,
        &[
            "clone",
            fixture.repo.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    assert!(run.success, "{}", run.stderr);
    assert!(!run.all().contains("CVC"), "{}", run.all());
    assert!(!clone.join(".claude").exists());
    // The clone has no record of its own to follow, and the original's is not
    // consulted: the hook found nothing to run.
    assert_eq!(fixture.post_checkout_calls(), Vec::<String>::new());
    assert!(!clone.join(".git/cvc").exists());
}

#[test]
fn harness_install_lists_existing_worktrees_that_lack_the_hooks() {
    let fixture = Fixture::ready();
    let (early, _) = fixture.worktree_add("early");

    // From a linked worktree the main worktree is named; the active one is not.
    let installed = fixture.install(&early);
    assert!(
        installed.stdout.contains(
            "Existing worktrees without these hooks (run `cvc harness install claude-code` there once):"
        ),
        "{}",
        installed.stdout
    );
    let main = canonical(&fixture.repo);
    assert!(
        installed.stdout.contains(&format!("  {}", main.display())),
        "{}",
        installed.stdout
    );
    assert!(
        !installed
            .stdout
            .contains(&format!("  {}\n", early.display())),
        "{}",
        installed.stdout
    );

    // Following the advice empties the list.
    let installed = fixture.install(&fixture.repo);
    assert!(
        !installed.stdout.contains("Existing worktrees"),
        "{}",
        installed.stdout
    );

    // A long list is cut after ten.
    let fixture = Fixture::ready();
    for index in 0..12 {
        let (_, run) = fixture.worktree_add(&format!("wt{index:02}"));
        assert!(run.success, "{}", run.stderr);
    }
    let installed = fixture.install(&fixture.repo);
    let listed = installed
        .stdout
        .lines()
        .filter(|line| line.starts_with("  ") && line.contains("/wt"))
        .count();
    assert_eq!(listed, 10, "{}", installed.stdout);
    assert!(
        installed.stdout.contains("  and 2 more"),
        "{}",
        installed.stdout
    );
}

#[test]
fn uninstall_names_every_worktree_and_what_it_could_not_check() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    // Twelve worktrees that carry the hooks: not one may be summarized away,
    // since each is still capturing.
    let mut carrying = Vec::new();
    for index in 0..12 {
        let (worktree, run) = fixture.worktree_add(&format!("wt{index:02}"));
        assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
        carrying.push(worktree);
    }
    // One whose `.claude` became a symlink, and one with a name that tries to
    // reach the terminal.
    let linked = &carrying[0];
    fs::remove_dir_all(linked.join(".claude")).unwrap();
    symlink(fixture.root.join("home"), linked.join(".claude")).unwrap();
    let rude = fixture.root.join("rude\u{1b}[31m\nname");
    let run = fixture.git(
        &fixture.repo,
        &["worktree", "add", rude.to_str().unwrap(), "-b", "rude"],
    );
    assert!(run.success, "{}", run.stderr);

    let removed = fixture.cvc_ok(&fixture.repo, &["harness", "uninstall", "claude-code"]);
    for worktree in &carrying[1..] {
        assert!(
            removed
                .stdout
                .contains(&format!("  {}\n", worktree.display())),
            "{} missing from:\n{}",
            worktree.display(),
            removed.stdout
        );
    }
    assert!(!removed.stdout.contains("  and "), "{}", removed.stdout);
    assert!(
        removed
            .stderr
            .contains(&format!("warning: could not check: {} (", linked.display())),
        "{}",
        removed.stderr
    );
    assert!(removed.stderr.contains("symlink"), "{}", removed.stderr);
    // The rude name carried the hooks too, and reaches the terminal defanged.
    assert!(
        removed.stdout.contains("rude?[31m?name"),
        "{}",
        removed.stdout
    );
    assert!(!removed.stdout.contains('\u{1b}'), "{}", removed.stdout);
    assert!(!removed.stderr.contains('\u{1b}'), "{}", removed.stderr);
}

#[test]
fn a_failed_enumeration_of_worktrees_is_a_warning_not_silence() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    let (_wt, _) = fixture.worktree_add("wt");
    let administrative = fixture.repo.join(".git/worktrees");
    fs::set_permissions(&administrative, fs::Permissions::from_mode(0o000)).unwrap();
    let removed = fixture.cvc(&fixture.repo, &["harness", "uninstall", "claude-code"]);
    // Restore before asserting, so a failure cannot leave an undeletable tree.
    fs::set_permissions(&administrative, fs::Permissions::from_mode(0o755)).unwrap();
    if fs::read_dir(&administrative).is_err() {
        return; // running with privileges that ignore the mode: nothing to test
    }
    assert!(removed.success, "{}", removed.stderr);
    assert!(
        removed
            .stderr
            .contains("warning: could not list other worktrees: "),
        "{}",
        removed.stderr
    );
    assert!(
        removed.stderr.contains("; check `git worktree list`"),
        "{}",
        removed.stderr
    );
    // The part that could be done was: the record and this checkout's entries.
    assert!(
        removed.stdout.contains(NO_LONGER_LINE),
        "{}",
        removed.stdout
    );
    assert!(!fixture.record_path().exists());
}

#[test]
fn real_git_keeps_the_status_of_a_users_hook_and_runs_under_set_u_without_home() {
    let fixture = Fixture::ready();
    let hook = fixture.repo.join(".git/hooks/post-checkout");
    let without_home = |args: &[&str]| {
        let mut command = Command::new(real_git());
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        let output = command
            .args(args)
            .current_dir(&fixture.repo)
            .env_remove("HOME")
            .env("PATH", &fixture.git_only)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", fixture.home.join("empty.gitconfig"))
            .env("GIT_TEMPLATE_DIR", fixture.home.join("templates"))
            .output()
            .unwrap();
        Run {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    };

    // A user hook that succeeds, under `set -eu`, with no HOME in the
    // environment: the managed block appended to it must not trip over either.
    fs::write(&hook, "#!/bin/sh\nset -eu\necho user-hook-ran\n").unwrap();
    fixture.install(&fixture.repo);
    let wt = fixture.root.join("ok");
    let run = without_home(&["worktree", "add", wt.to_str().unwrap(), "-b", "ok"]);
    assert!(run.success, "{}", run.stderr);
    assert!(run.stderr.contains("user-hook-ran"), "{}", run.stderr);
    assert!(run.stderr.contains(INSTALLED_LINE), "{}", run.stderr);
    assert_hooks_run(&wt, &fixture.wrapper);

    // A user hook whose last command fails keeps failing the Git command, as
    // it did before CVC was appended to it -- and CVC still did its part.
    fs::write(&hook, "#!/bin/sh\necho failing-user-hook\nfalse\n").unwrap();
    fixture.install(&fixture.repo);
    let wt = fixture.root.join("fails");
    let run = without_home(&["worktree", "add", wt.to_str().unwrap(), "-b", "fails"]);
    assert!(!run.success, "the user's failure must still reach Git");
    assert!(run.stderr.contains("failing-user-hook"), "{}", run.stderr);
    assert!(run.stderr.contains(INSTALLED_LINE), "{}", run.stderr);
    assert_hooks_run(&wt, &fixture.wrapper);
    assert!(!without_home(&["switch", "-c", "elsewhere"]).success);

    // And a user hook that succeeds leaves everything succeeding.
    fs::write(&hook, "#!/bin/sh\necho passing-user-hook\ntrue\n").unwrap();
    fixture.install(&fixture.repo);
    assert!(without_home(&["switch", "-c", "elsewhere2"]).success);
    let wt = fixture.root.join("passes");
    let run = without_home(&["worktree", "add", wt.to_str().unwrap(), "-b", "passes"]);
    assert!(run.success, "{}", run.stderr);
    assert_hooks_run(&wt, &fixture.wrapper);
}

#[test]
fn a_main_worktree_that_cannot_be_located_is_said_so() {
    // `git init --separate-git-dir` leaves a linked worktree no way back to its
    // main worktree, so neither install nor uninstall can list it.
    let fixture = Fixture::new();
    let main = fixture.root.join("sep-main");
    let store = fixture.root.join("sep-store");
    let linked = fixture.root.join("sep-linked");
    fs::create_dir(&main).unwrap();
    fixture.git_ok(
        &main,
        &["init", "--separate-git-dir", store.to_str().unwrap(), "."],
    );
    fixture.git_ok(&main, &["config", "user.email", "test@example.invalid"]);
    fixture.git_ok(&main, &["config", "user.name", "CVC test"]);
    fixture.git_ok(&main, &["commit", "--allow-empty", "-m", "initial"]);
    fixture.cvc_ok(&main, &["init"]);
    let repository = git2::Repository::open(&main).unwrap();
    cvc_core::privacy::acknowledge_capture(&repository).unwrap();
    fixture.git_ok(
        &main,
        &["worktree", "add", linked.to_str().unwrap(), "-b", "sep"],
    );

    let expected = "warning: the main worktree could not be located; check it yourself";
    let installed = fixture.install(&linked);
    assert!(installed.stderr.contains(expected), "{}", installed.stderr);
    let removed = fixture.cvc_ok(&linked, &["harness", "uninstall", "claude-code"]);
    assert!(removed.stderr.contains(expected), "{}", removed.stderr);
    // From the main worktree itself there is nothing to locate.
    let installed = fixture.install(&main);
    assert!(
        !installed.stderr.contains("could not be located"),
        "{}",
        installed.stderr
    );
}

#[test]
fn harness_install_adds_only_the_post_checkout_git_hook() {
    let fixture = Fixture::ready();
    let hooks = fixture.repo.join(".git/hooks");
    let hook = hooks.join("post-checkout");

    // A repository initialized by a cvc that predates the hook. A hook the user
    // removed on purpose must not come back as a side effect.
    fs::remove_file(&hook).unwrap();
    fs::remove_file(hooks.join("post-commit")).unwrap();
    let installed = fixture.install(&fixture.repo);
    assert!(
        installed.stdout.contains(&format!(
            "  created the post-checkout Git hook at {} so new worktrees get these hooks",
            canonical(&hook).display()
        )),
        "{}",
        installed.stdout
    );
    assert!(installed.stdout.contains(PROPAGATES_LINE));
    assert!(fs::read_to_string(&hook).unwrap().contains("# CVC Hook"));
    assert_eq!(
        fs::metadata(&hook).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(!hooks.join("post-commit").exists());

    // The user's own shell hook is kept, and ours is added to it.
    fs::write(&hook, "#!/bin/sh\necho user-hook\n").unwrap();
    let installed = fixture.install(&fixture.repo);
    assert!(
        installed.stdout.contains(&format!(
            "  added CVC to the existing post-checkout Git hook at {} so new worktrees get these hooks",
            canonical(&hook).display()
        )),
        "{}",
        installed.stdout
    );
    let content = fs::read_to_string(&hook).unwrap();
    assert!(
        content.starts_with("#!/bin/sh\necho user-hook\n"),
        "{content}"
    );
    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.all().contains("user-hook"), "{}", run.all());
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert!(wt.join(SETTINGS).exists());

    // Running it again changes and announces nothing.
    let installed = fixture.install(&fixture.repo);
    assert!(
        !installed.stdout.contains("post-checkout"),
        "{}",
        installed.stdout
    );
    assert_eq!(fs::read_to_string(&hook).unwrap(), content);
}

#[test]
fn a_hook_that_is_not_a_shell_script_is_left_alone_with_a_warning() {
    // `cvc init`: each managed hook, in turn, already exists as a Python script.
    let cases = [
        (
            "post-commit",
            "`cvc hook post-commit` from it yourself to enable linking recent thoughts to commits.",
        ),
        (
            "pre-push",
            "`cvc hook pre-push \"$@\"` from it yourself to enable push-range observation and automatic push.",
        ),
        (
            "post-merge",
            "`cvc hook post-merge \"$@\"` from it yourself to enable the post-merge pull and squash scan.",
        ),
        (
            "post-checkout",
            "`cvc hook post-checkout \"$@\"` from it yourself to enable Claude Code capture hooks in new worktrees (see \"Hooks in new worktrees\" in cvc-cli/README.md for how to call it).",
        ),
    ];
    for (name, call) in cases {
        let fixture = Fixture::new();
        let hook = fixture.repo.join(".git/hooks").join(name);
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        let python = "#!/usr/bin/env python3\nprint('user hook')\n";
        executable(&hook, python);

        let init = fixture.cvc_ok(&fixture.repo, &["init"]);
        let warning = format!(
            "warning: {} is not a shell script; CVC did not modify it. Call {call}",
            canonical(&hook).display()
        );
        assert!(init.stderr.contains(&warning), "{name}: {}", init.stderr);
        assert_eq!(fs::read_to_string(&hook).unwrap(), python, "{name}");
        assert!(init.stdout.contains("CVC Initialized successfully!"));
        // The other managed hooks went in as usual.
        for other in ["post-commit", "pre-push", "post-merge", "post-checkout"] {
            assert!(fixture.repo.join(".git/hooks").join(other).exists());
        }
    }

    // `harness install` reports the one it needs and falls back to the old
    // per-checkout advice, since new worktrees will not be followed.
    let fixture = Fixture::new();
    fixture.cvc_ok(&fixture.repo, &["init"]);
    fixture.acknowledge();
    let hook = fixture.repo.join(".git/hooks/post-checkout");
    fs::remove_file(&hook).unwrap();
    let perl = "#!/usr/bin/perl\nprint \"user hook\\n\";\n";
    executable(&hook, perl);
    let installed = fixture.install(&fixture.repo);
    // The advice names the recorded binary itself, quoted, not a bare `cvc`
    // that depends on PATH.
    assert!(
        installed.stderr.contains(&format!(
            "warning: {} is not a shell script; CVC did not modify it. Call `'{}' hook post-checkout \"$@\"` from it yourself (ignore its exit status) to enable Claude Code capture hooks in new worktrees.",
            canonical(&hook).display(),
            fixture.wrapper.display()
        )),
        "{}",
        installed.stderr
    );
    assert!(
        !installed.stderr.contains("`cvc hook"),
        "{}",
        installed.stderr
    );
    assert!(
        installed.stdout.contains(PER_CHECKOUT_LINE),
        "{}",
        installed.stdout
    );
    assert!(!installed.stdout.contains(PROPAGATES_LINE));
    assert_eq!(fs::read_to_string(&hook).unwrap(), perl);
    assert!(fixture.repo.join(SETTINGS).exists());
}

#[test]
fn the_per_checkout_install_survives_a_failed_propagation_setup() {
    // A record directory that is a symlink is refused, not followed.
    let fixture = Fixture::ready();
    let outside = fixture.root.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, fixture.repo.join(".git/cvc/harness")).unwrap();
    let installed = fixture.install(&fixture.repo);
    assert!(
        installed
            .stderr
            .contains("warning: could not record this install for new worktrees"),
        "{}",
        installed.stderr
    );
    assert!(
        installed.stdout.contains(PER_CHECKOUT_LINE),
        "{}",
        installed.stdout
    );
    assert!(
        !installed.stdout.contains(PROPAGATES_LINE),
        "{}",
        installed.stdout
    );
    assert!(fixture.repo.join(SETTINGS).exists());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);

    // A post-checkout hook that is a symlink is never written through.
    let fixture = Fixture::ready();
    let hook = fixture.repo.join(".git/hooks/post-checkout");
    let target = fixture.root.join("shared-hook");
    fs::write(&target, "#!/bin/sh\n").unwrap();
    fs::remove_file(&hook).unwrap();
    symlink(&target, &hook).unwrap();
    let installed = fixture.install(&fixture.repo);
    assert!(
        installed
            .stderr
            .contains("warning: could not install the post-checkout Git hook"),
        "{}",
        installed.stderr
    );
    assert!(
        installed.stdout.contains(PER_CHECKOUT_LINE),
        "{}",
        installed.stdout
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "#!/bin/sh\n");
    assert!(fixture.repo.join(SETTINGS).exists());
}

#[test]
fn uninstall_fails_loudly_when_the_record_cannot_be_cleared() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    // Swap the record for a symlink: refused rather than followed.
    let target = fixture.root.join("target.path");
    fs::write(&target, "/bin/sh\n").unwrap();
    fs::remove_file(fixture.record_path()).unwrap();
    symlink(&target, fixture.record_path()).unwrap();

    let removed = fixture.cvc(&fixture.repo, &["harness", "uninstall", "claude-code"]);
    assert!(!removed.success);
    // The per-checkout half is done and reported; the rest is explained.
    assert!(
        removed.stdout.contains("Removed 3 CVC hook entries"),
        "{}",
        removed.stdout
    );
    assert!(!fixture.repo.join(SETTINGS).exists());
    assert!(
        removed.stderr.contains("could not be cleared"),
        "{}",
        removed.stderr
    );
    assert!(
        removed.stderr.contains("claude-code.path"),
        "{}",
        removed.stderr
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "/bin/sh\n");
}

#[test]
fn uninstall_still_clears_the_record_when_this_checkouts_claude_dir_is_unusable() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    let outside = fixture.root.join("outside");
    fs::create_dir(&outside).unwrap();
    let (wt, _) = fixture.worktree_add("wt");
    fs::remove_dir_all(wt.join(".claude")).unwrap();
    symlink(&outside, wt.join(".claude")).unwrap();

    let removed = fixture.cvc(&wt, &["harness", "uninstall", "claude-code"]);
    assert!(!removed.success);
    assert!(
        removed.stderr.contains("is a symlink; not following it"),
        "{}",
        removed.stderr
    );
    // New worktrees stop getting the hooks all the same.
    assert!(
        removed.stdout.contains(NO_LONGER_LINE),
        "{}",
        removed.stdout
    );
    assert!(!fixture.record_path().exists());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn harness_install_refuses_a_binary_it_cannot_run() {
    let fixture = Fixture::ready();
    let plain = fixture.root.join("plain");
    fs::write(&plain, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();
    for (binary, reason) in [
        (plain.to_str().unwrap().to_owned(), "is not executable"),
        ("relative/cvc".to_owned(), "is not an absolute path"),
        (
            fixture.root.join("missing").to_str().unwrap().to_owned(),
            "does not exist",
        ),
        (fixture.root.to_str().unwrap().to_owned(), "is not a file"),
    ] {
        let run = fixture.cvc(
            &fixture.repo,
            &["harness", "install", "claude-code", "--binary", &binary],
        );
        assert!(!run.success, "{binary}");
        assert!(run.stderr.contains(reason), "{binary}: {}", run.stderr);
        // Nothing was installed or recorded.
        assert!(!fixture.repo.join(".claude").exists(), "{binary}");
        assert!(!fixture.record_path().exists(), "{binary}");
    }
}

#[test]
fn a_cache_copy_installs_here_but_is_not_recorded_for_new_worktrees() {
    let fixture = Fixture::ready();
    // The npm launcher's versioned cache, with no published `~/.cvc/bin/cvc`:
    // the state in which the install falls back to the cache path.
    let cache = fixture.home.join(".cvc/mcp-cache/v0.0.0-test/cvc");
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_cvc"), &cache).unwrap();

    let run = fixture.run(
        &cache,
        &fixture.repo,
        &fixture.git_only,
        &["harness", "install", "claude-code"],
    );
    assert!(run.success, "{}", run.stderr);
    assert!(run.stdout.contains("installed"), "{}", run.stdout);
    assert!(run.stdout.contains(NOT_RECORDED_LINE), "{}", run.stdout);
    assert!(!run.stdout.contains(PROPAGATES_LINE), "{}", run.stdout);
    assert!(run.stderr.contains("launcher's versioned cache"));
    assert!(fixture.repo.join(SETTINGS).exists());
    assert!(!fixture.record_path().exists());
    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.success, "{}", run.stderr);
    assert!(!run.all().contains("CVC"), "{}", run.all());
    assert!(!wt.join(".claude").exists());

    // A record that is already there is neither refreshed nor removed, and the
    // message does not claim propagation is off: it says what keeps working.
    fixture.install(&fixture.repo);
    let recorded = fs::read_to_string(fixture.record_path()).unwrap();
    let run = fixture.run(
        &cache,
        &fixture.repo,
        &fixture.git_only,
        &["harness", "install", "claude-code"],
    );
    assert!(!run.stdout.contains(NOT_RECORDED_LINE), "{}", run.stdout);
    assert!(
        run.stdout.contains(&format!(
            "New worktrees keep getting hooks that run {} (recorded earlier); this install was not recorded because it ran from the launcher's versioned cache.",
            fixture.wrapper.display()
        )),
        "{}",
        run.stdout
    );
    assert_eq!(fs::read_to_string(fixture.record_path()).unwrap(), recorded);

    // An earlier record whose binary has since gone is no promise to keep.
    fs::remove_file(&fixture.wrapper).unwrap();
    let run = fixture.run(
        &cache,
        &fixture.repo,
        &fixture.git_only,
        &["harness", "install", "claude-code"],
    );
    assert!(run.stdout.contains(NOT_RECORDED_LINE), "{}", run.stdout);
    assert!(!run.stdout.contains("keep getting hooks"), "{}", run.stdout);
    assert_eq!(fs::read_to_string(fixture.record_path()).unwrap(), recorded);
    executable(
        &fixture.wrapper,
        &format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", env!("CARGO_BIN_EXE_cvc")),
    );

    // `--binary` is taken as given, cache path or not.
    let run = fixture.run(
        &cache,
        &fixture.repo,
        &fixture.git_only,
        &[
            "harness",
            "install",
            "claude-code",
            "--binary",
            cache.to_str().unwrap(),
        ],
    );
    assert!(run.stdout.contains(PROPAGATES_LINE), "{}", run.stdout);
    assert_eq!(
        fs::read_to_string(fixture.record_path()).unwrap(),
        format!("{}\n", cache.display())
    );
}

#[test]
fn a_record_others_can_write_is_declined() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    fs::set_permissions(fixture.record_path(), fs::Permissions::from_mode(0o664)).unwrap();

    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.success, "{}", run.stderr);
    let lines = run.cvc_lines();
    assert_eq!(lines.len(), 1, "{}", run.all());
    assert!(lines[0].starts_with(DECLINED), "{}", lines[0]);
    assert!(
        lines[0].contains("writable by group or others"),
        "{}",
        lines[0]
    );
    assert!(!wt.join(".claude").exists());

    // Back to private, it works again.
    fs::set_permissions(fixture.record_path(), fs::Permissions::from_mode(0o600)).unwrap();
    let (wt, run) = fixture.worktree_add("wt2");
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert!(wt.join(SETTINGS).exists());
}

#[test]
fn a_settings_file_the_branch_tracks_is_declined_and_left_alone() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    let committed = "{\"permissions\":{\"allow\":[\"Bash(ls:*)\"]}}\n";
    fixture.branch_with("tracked-settings", &[("100644", SETTINGS, committed)]);
    // Other `.claude` content that repositories do commit never blocks it.
    fixture.branch_with(
        "tracked-neighbours",
        &[
            ("100644", ".claude/settings.json", "{}\n"),
            ("100644", ".claude/agents/reviewer.md", "# reviewer\n"),
        ],
    );

    let wt = fixture.root.join("tracked-settings");
    let run = fixture.git(
        &fixture.repo,
        &["worktree", "add", wt.to_str().unwrap(), "tracked-settings"],
    );
    assert!(run.success, "{}", run.stderr);
    assert_eq!(
        run.cvc_lines(),
        [format!("{DECLINED}{TRACKED_LINE}")],
        "{}",
        run.all()
    );
    assert_eq!(fs::read_to_string(wt.join(SETTINGS)).unwrap(), committed);
    // Git sees no change in the worktree: nothing was modified.
    let status = fixture.git_ok(&wt, &["status", "--porcelain"]);
    assert_eq!(status.stdout, "");

    let wt = fixture.root.join("tracked-neighbours");
    let run = fixture.git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            wt.to_str().unwrap(),
            "tracked-neighbours",
        ],
    );
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert_hooks_run(&wt, &fixture.wrapper);
    assert_eq!(
        fs::read_to_string(wt.join(".claude/settings.json")).unwrap(),
        "{}\n"
    );
    assert!(wt.join(".claude/agents/reviewer.md").exists());

    // `harness install` in a checkout that tracks the file says the same and
    // installs nothing, the record included.
    let fixture = Fixture::ready();
    fs::create_dir(fixture.repo.join(".claude")).unwrap();
    fs::write(fixture.repo.join(SETTINGS), committed).unwrap();
    fixture.git_ok(&fixture.repo, &["add", "-f", SETTINGS]);
    let run = fixture.cvc(
        &fixture.repo,
        &[
            "harness",
            "install",
            "claude-code",
            "--binary",
            fixture.wrapper.to_str().unwrap(),
        ],
    );
    assert!(!run.success);
    assert!(run.stderr.contains(TRACKED_LINE), "{}", run.stderr);
    assert_eq!(
        fs::read_to_string(fixture.repo.join(SETTINGS)).unwrap(),
        committed
    );
    assert!(!fixture.record_path().exists());
}

#[test]
fn committed_claude_links_are_never_followed_out_of_a_new_worktree() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    let outside = fixture.root.join("outside");
    fs::create_dir(&outside).unwrap();
    let missing = fixture.root.join("does-not-exist");

    // (branch, mode, content committed at `.claude`, expected phrase)
    let cases: Vec<(&str, &str, String, &str)> = vec![
        (
            "absolute-link",
            "120000",
            outside.to_str().unwrap().into(),
            "is a symlink; not following it",
        ),
        (
            "relative-link",
            "120000",
            "../outside".into(),
            "is a symlink; not following it",
        ),
        (
            "dangling-link",
            "120000",
            missing.to_str().unwrap().into(),
            "is a symlink; not following it",
        ),
        (
            "a-file",
            "100644",
            "i am a file".into(),
            "is not a directory",
        ),
    ];
    for (branch, mode, content, expected) in cases {
        fixture.branch_with(branch, &[(mode, ".claude", &content)]);
        let wt = fixture.root.join(branch);
        let run = fixture.git(
            &fixture.repo,
            &["worktree", "add", wt.to_str().unwrap(), branch],
        );
        assert!(run.success, "{branch}: {}", run.stderr);
        let lines = run.cvc_lines();
        assert_eq!(lines.len(), 1, "{branch}: {}", run.all());
        assert!(lines[0].starts_with("CVC: post-checkout: "), "{}", lines[0]);
        assert!(lines[0].contains(expected), "{branch}: {}", lines[0]);
        assert!(
            lines[0].contains(wt.join(".claude").to_str().unwrap()),
            "{branch}: names the path: {}",
            lines[0]
        );
    }
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "something was written through a committed link"
    );
    assert!(!missing.exists(), "a dangling target was created");
}

#[test]
fn a_hook_that_is_not_a_shell_script_never_breaks_a_checkout() {
    // The reproduction of the defect this guards against: a user's Python
    // post-checkout hook, then `cvc init`, then every checkout and worktree add.
    let Some(python3) = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
    else {
        eprintln!("python3 is not available; skipping");
        return;
    };
    let fixture = Fixture::new();
    let hook = fixture.repo.join(".git/hooks/post-checkout");
    let python = "#!/usr/bin/env python3\nprint('user python hook')\n";
    executable(&hook, python);
    let git_and_python = fixture.root.join("path-git-python");
    fs::create_dir(&git_and_python).unwrap();
    symlink(real_git(), git_and_python.join("git")).unwrap();
    symlink(python3, git_and_python.join("python3")).unwrap();
    let with_python = |args: &[&str]| fixture.run(real_git(), &fixture.repo, &git_and_python, args);

    assert!(with_python(&["checkout", "-q", "-b", "before"]).success);
    fixture.cvc_ok(&fixture.repo, &["init"]);
    fixture.acknowledge();
    fixture.install(&fixture.repo);
    assert_eq!(fs::read_to_string(&hook).unwrap(), python);
    assert!(with_python(&["checkout", "-q", "-b", "after"]).success);
    let worktree = fixture.root.join("wt");
    let run = with_python(&["worktree", "add", worktree.to_str().unwrap(), "-b", "wt"]);
    assert!(run.success, "{}", run.stderr);
    assert!(run.all().contains("user python hook"), "{}", run.all());
}

#[test]
fn the_hook_command_never_fails_whatever_it_finds() {
    let fixture = Fixture::ready();
    let hook = |cwd: &Path, args: &[&str]| {
        let mut full = vec!["hook", "post-checkout"];
        full.extend_from_slice(args);
        fixture.cvc(cwd, &full)
    };
    let new_worktree = [NULL_OID, NEW_OID, "1"];

    // Outside any repository, in a bare repository, in a repository CVC is
    // not set up in: silent success.
    let outside = fixture.root.join("outside");
    fs::create_dir(&outside).unwrap();
    let bare = fixture.root.join("bare.git");
    fixture.git_ok(&fixture.root, &["init", "--bare", bare.to_str().unwrap()]);
    let plain = fixture.root.join("plain");
    fs::create_dir(&plain).unwrap();
    fixture.git_ok(&plain, &["init"]);
    for cwd in [&outside, &bare, &plain] {
        let run = hook(cwd, &new_worktree);
        assert!(
            run.success && run.all().is_empty(),
            "{}: {}",
            cwd.display(),
            run.all()
        );
    }

    // Wrong shapes: clap rejects them, and an advisory hook still succeeds.
    for args in [vec![], vec![NULL_OID], vec![NULL_OID, NEW_OID]] {
        assert!(hook(&fixture.repo, &args).success);
    }

    // The record cases below run in a linked worktree that existed before any
    // record did, since only a linked worktree is ever acted on.
    let (lw, run) = fixture.worktree_add("lw");
    assert!(run.success && !run.all().contains("CVC"), "{}", run.all());

    // Anything but a new worktree is ignored even with a record present.
    fixture.install(&fixture.repo);
    fs::remove_file(fixture.repo.join(SETTINGS)).unwrap();
    for args in [
        [NEW_OID, NEW_OID, "1"],
        [NULL_OID, NEW_OID, "0"],
        ["", NEW_OID, "1"],
        ["00000", NEW_OID, "1"],
    ] {
        let run = hook(&lw, &args);
        assert!(
            run.success && run.all().is_empty(),
            "{args:?}: {}",
            run.all()
        );
        assert!(!lw.join(SETTINGS).exists());
    }

    // Records that cannot be acted on are named on one line, and not obeyed.
    let plain_file = fixture.root.join("not-executable");
    fs::write(&plain_file, "").unwrap();
    fs::set_permissions(&plain_file, fs::Permissions::from_mode(0o644)).unwrap();
    let missing = fixture.root.join("deleted");
    let record = fixture.record_path();
    let cases: Vec<(String, String)> = vec![
        (
            "garbage\n".into(),
            format!(
                "harness record {} is unusable: garbage is not an absolute path",
                record.display()
            ),
        ),
        (
            "relative/cvc\n".into(),
            format!(
                "harness record {} is unusable: relative/cvc is not an absolute path",
                record.display()
            ),
        ),
        (
            "/\n".into(),
            "/ is not a file; run `cvc harness install claude-code` here".into(),
        ),
        (
            format!("{}\n", plain_file.display()),
            format!(
                "{} is not executable; run `cvc harness install claude-code` here",
                plain_file.display()
            ),
        ),
        (
            format!("{}\n", missing.display()),
            format!(
                "{} does not exist; run `cvc harness install claude-code` here",
                missing.display()
            ),
        ),
    ];
    for (content, reason) in cases {
        fs::write(&record, &content).unwrap();
        let run = hook(&lw, &new_worktree);
        assert!(run.success, "{content:?}");
        assert_eq!(
            run.stderr.trim_end(),
            format!("{DECLINED}{reason}"),
            "{content:?}"
        );
        assert!(run.stdout.is_empty(), "{content:?}");
        assert!(!lw.join(SETTINGS).exists());
    }
    fs::write(&record, "\0\0\n").unwrap();
    let run = hook(&lw, &new_worktree);
    assert!(run.success);
    assert_eq!(run.stderr.lines().count(), 1, "{}", run.stderr);
    assert!(run.stderr.starts_with(DECLINED), "{}", run.stderr);

    // With a sound record it creates the settings file in the linked worktree,
    // once. A second call finds the file there and, because propagation only
    // ever creates it, says so and changes nothing.
    fixture.install(&fixture.repo);
    let run = hook(&lw, &new_worktree);
    assert!(run.success);
    assert_eq!(run.stdout.trim_end(), INSTALLED_LINE);
    assert_eq!(run.stderr, "");
    assert_hooks_run(&lw, &fixture.wrapper);
    let before = fs::read_to_string(lw.join(SETTINGS)).unwrap();
    let run = hook(&lw, &new_worktree);
    assert!(run.success);
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr.trim_end(), format!("{DECLINED}{EXISTS_LINE}"));
    assert_eq!(fs::read_to_string(lw.join(SETTINGS)).unwrap(), before);

    // The main worktree is never acted on, with a sound record and everything
    // else in order: the same call there is silent and writes nothing.
    fs::remove_file(fixture.repo.join(SETTINGS)).unwrap();
    let run = hook(&fixture.repo, &new_worktree);
    assert!(run.success && run.all().is_empty(), "{}", run.all());
    assert!(!fixture.repo.join(SETTINGS).exists());
}

#[test]
fn the_main_worktree_never_gets_hooks_from_the_hook_but_new_worktrees_still_do() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    // Make a wrongful install in the main worktree visible.
    fs::remove_file(fixture.repo.join(SETTINGS)).unwrap();
    let branch = fixture
        .git_ok(&fixture.repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .stdout
        .trim()
        .to_owned();

    // An orphan round trip in the main worktree: leaving the unborn branch
    // passes a null previous ref and flag 1, exactly like a new worktree. The
    // shell hook does start the recorded binary (it cannot tell), and the
    // binary declines because this is not a linked worktree.
    fixture.clear_calls();
    let orphan = fixture.git_ok(&fixture.repo, &["switch", "--orphan", "o"]);
    assert!(!orphan.all().contains("CVC"), "{}", orphan.all());
    let back = fixture.git_ok(&fixture.repo, &["switch", &branch]);
    assert!(!back.all().contains("CVC"), "{}", back.all());
    assert_eq!(
        fixture.post_checkout_calls().len(),
        1,
        "the orphan switch did reach the recorded binary"
    );
    assert!(!fixture.repo.join(SETTINGS).exists());
    let again = fixture.git_ok(&fixture.repo, &["checkout", "--orphan", "o2"]);
    assert!(!again.all().contains("CVC"), "{}", again.all());
    let back = fixture.git_ok(&fixture.repo, &["checkout", &branch]);
    assert!(!back.all().contains("CVC"), "{}", back.all());
    assert!(!fixture.repo.join(SETTINGS).exists());

    // After all that, new linked worktrees are served as before, branch or
    // detached.
    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.success, "{}", run.stderr);
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert_hooks_run(&wt, &fixture.wrapper);
    let detached = fixture.root.join("detached");
    let run = fixture.git(
        &fixture.repo,
        &["worktree", "add", "--detach", detached.to_str().unwrap()],
    );
    assert!(run.success, "{}", run.stderr);
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert_hooks_run(&detached, &fixture.wrapper);
    // ...and the main worktree is still untouched.
    assert!(!fixture.repo.join(SETTINGS).exists());
}

#[test]
fn help_text_says_what_the_commands_do_to_new_worktrees() {
    let fixture = Fixture::new();
    let help = flat(&fixture.cvc_ok(&fixture.repo, &["--help"]).stdout);
    assert!(
        help.contains("in new worktrees while capture is acknowledged"),
        "{help}"
    );
    // The subcommand list shows one summary line each; the detail is behind
    // `--help` of the subcommand itself.
    let harness = flat(&fixture.cvc_ok(&fixture.repo, &["harness", "--help"]).stdout);
    assert!(
        harness.contains("Install Claude Code capture hooks here and in new worktrees"),
        "{harness}"
    );
    assert!(
        harness.contains("Remove this checkout's hooks and stop new worktrees from getting them"),
        "{harness}"
    );
    assert!(
        !harness.contains("existing worktrees do not"),
        "the summary stays short: {harness}"
    );
    let install = flat(
        &fixture
            .cvc_ok(&fixture.repo, &["harness", "install", "--help"])
            .stdout,
    );
    assert!(
        install.contains("Worktrees created afterwards (git worktree add) get them too, while this repository's capture acknowledgement stands; existing worktrees do not"),
        "{install}"
    );
    let uninstall = flat(
        &fixture
            .cvc_ok(&fixture.repo, &["harness", "uninstall", "--help"])
            .stdout,
    );
    assert!(
        uninstall.contains("Other worktrees keep theirs"),
        "{uninstall}"
    );
    assert!(
        uninstall.contains("the capture acknowledgement is not withdrawn"),
        "{uninstall}"
    );
    let hook = flat(
        &fixture
            .cvc_ok(&fixture.repo, &["hook", "post-checkout", "--help"])
            .stdout,
    );
    assert!(
        hook.contains("Does nothing for other `git checkout` or `git switch` runs"),
        "{hook}"
    );
}

#[test]
fn leaving_an_orphan_branch_in_a_worktree_that_has_settings_never_modifies_them() {
    // The orphan edge reaches the recorded binary in a linked worktree. Since
    // propagation only ever creates the file, a worktree that already has one --
    // here with a setting of the user's own added -- keeps it byte for byte and
    // is told why, instead of being merged into.
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    let edited = settings(&wt);
    let mut edited = edited.as_object().unwrap().clone();
    edited.insert(
        "permissions".into(),
        serde_json::json!({"allow": ["Bash(ls:*)"]}),
    );
    let edited = serde_json::to_string_pretty(&Value::Object(edited)).unwrap() + "\n";
    fs::write(wt.join(SETTINGS), &edited).unwrap();

    fixture.git_ok(&wt, &["switch", "--orphan", "o"]);
    let run = fixture.git_ok(&wt, &["switch", "wt"]);
    assert_eq!(
        run.cvc_lines(),
        [format!("{DECLINED}{EXISTS_LINE}")],
        "{}",
        run.all()
    );
    assert_eq!(fs::read_to_string(wt.join(SETTINGS)).unwrap(), edited);
}

#[test]
fn a_symlinked_claude_directory_makes_a_manual_install_fail_and_change_nothing() {
    let fixture = Fixture::ready();
    let outside = fixture.root.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, fixture.repo.join(".claude")).unwrap();

    let run = fixture.cvc(
        &fixture.repo,
        &[
            "harness",
            "install",
            "claude-code",
            "--binary",
            fixture.wrapper.to_str().unwrap(),
        ],
    );
    assert!(!run.success);
    assert!(
        run.stderr.contains(&format!(
            "`.claude` in this worktree is a symlink; not following it ({})",
            fixture.repo.join(".claude").display()
        )),
        "{}",
        run.stderr
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert!(!fixture.record_path().exists());
}

#[test]
fn a_harness_directory_others_can_write_fails_the_record_step_but_not_the_install() {
    let fixture = Fixture::ready();
    let harness = fixture.repo.join(".git/cvc/harness");
    fs::create_dir(&harness).unwrap();
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o777)).unwrap();

    let installed = fixture.install(&fixture.repo);
    assert!(
        installed
            .stderr
            .contains("warning: could not record this install for new worktrees: "),
        "{}",
        installed.stderr
    );
    assert!(
        installed.stderr.contains("writable by group or others"),
        "{}",
        installed.stderr
    );
    assert!(
        installed.stdout.contains(PER_CHECKOUT_LINE),
        "{}",
        installed.stdout
    );
    assert!(!installed.stdout.contains(PROPAGATES_LINE));
    assert!(fixture.repo.join(SETTINGS).exists());
    assert!(!fixture.record_path().exists());

    // Tightened, the same command records and says so.
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o700)).unwrap();
    let installed = fixture.install(&fixture.repo);
    assert!(installed.stdout.contains(PROPAGATES_LINE));
    assert!(fixture.record_path().exists());
}

#[test]
fn harness_install_upgrades_an_older_post_checkout_block_in_place() {
    let fixture = Fixture::ready();
    let hook = fixture.repo.join(".git/hooks/post-checkout");
    let older = "# CVC Hook: post-checkout begin (v0)\nold logic\n# CVC Hook: post-checkout end\n";
    fs::write(
        &hook,
        format!("#!/bin/sh\necho user-above\n{older}echo user-below\n"),
    )
    .unwrap();

    let installed = fixture.install(&fixture.repo);
    assert!(
        installed.stdout.contains(&format!(
            "  updated the post-checkout Git hook at {} so new worktrees get these hooks",
            canonical(&hook).display()
        )),
        "{}",
        installed.stdout
    );
    let content = fs::read_to_string(&hook).unwrap();
    assert_eq!(
        content.matches("# CVC Hook: post-checkout begin").count(),
        1,
        "{content}"
    );
    assert!(content.contains("begin (v1)") && !content.contains("old logic"));
    assert!(content.starts_with("#!/bin/sh\necho user-above\n# CVC Hook: post-checkout begin (v1)"));
    assert!(content.ends_with("# CVC Hook: post-checkout end\necho user-below\n"));
    // And the hook still works, user parts included.
    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.success, "{}", run.stderr);
    assert!(run.all().contains("user-above"), "{}", run.all());
    assert!(run.all().contains("user-below"), "{}", run.all());
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert!(wt.join(SETTINGS).exists());
}

/// QA D4, narrowed and left open on purpose. Git passes a null previous ref when
/// a switch leaves an unborn (orphan) branch, which looks to the hook exactly
/// like a new worktree. In the main worktree that is closed: the recorded
/// binary only ever acts in a linked worktree (see
/// `the_main_worktree_never_gets_hooks_from_the_hook_but_new_worktrees_still_do`).
/// Inside an existing LINKED worktree that has no settings it remains. The
/// first guard tried -- "the worktree's HEAD reflog has at most one entry" --
/// does not hold: on Git 2.55 `git worktree add -b` and
/// `git worktree add <path> <branch>` leave TWO entries in the new worktree's
/// HEAD reflog at hook time (an unnamed creation entry and
/// `reset: moving to HEAD`), `--detach` leaves one, and leaving an orphan
/// branch adds `checkout: moving from <orphan> to <branch>`. A count cannot tell
/// a detached new worktree (1) from a branch worktree after one orphan round
/// trip (2 or 3). Un-ignore when a heuristic is agreed.
#[test]
#[ignore = "QA-113 D4 (linked worktrees only): leaving an unborn branch inside an existing linked worktree looks like a new worktree; a HEAD-reflog-count guard does not hold (new worktrees have 1 or 2 entries)"]
fn leaving_an_orphan_branch_does_not_install_hooks_in_an_existing_linked_worktree() {
    let fixture = Fixture::ready();
    // Created before the install, so it has no settings of its own.
    let (wt, _) = fixture.worktree_add("wt");
    fixture.install(&fixture.repo);
    assert!(!wt.join(SETTINGS).exists());

    fixture.git_ok(&wt, &["switch", "--orphan", "o"]);
    let run = fixture.git_ok(&wt, &["switch", "wt"]);
    assert!(!run.all().contains("CVC"), "{}", run.all());
    assert!(!wt.join(SETTINGS).exists());
}
