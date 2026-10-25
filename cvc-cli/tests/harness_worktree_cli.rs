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

/// Claude Code exports these to its Bash tool and to its hooks, so a test run
/// from a session inherits them. Each changes what `cvc` and its Git hooks
/// print or do (`CLAUDECODE=1` makes the post-commit hook name the commit;
/// `CLAUDE_PROJECT_DIR` confines hook mode to the repository the session was
/// started in; `CLAUDE_PLUGIN_ROOT` is not read by `cvc`, but is part of a
/// plugin hook's environment), so no command a test spawns may see them unless
/// the test sets them itself.
const CLAUDE_CODE_ENV: [&str; 3] = ["CLAUDECODE", "CLAUDE_PLUGIN_ROOT", "CLAUDE_PROJECT_DIR"];
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
const EXCLUDE_ENTRY: &str = "/.claude/settings.local.json";
/// What the hook says when a `.gitignore` rule of the new worktree overrides the
/// `info/exclude` entry (the text after [`DECLINED`]).
const NOT_IGNORED_LINE: &str = "`.claude/settings.local.json` would not be ignored by Git in this worktree: a `.gitignore` rule overrides the entry CVC adds to `.git/info/exclude`; CVC writes a machine-specific path there and will not create a file that a commit could carry (fix the rule, then run `cvc harness install claude-code` here)";
/// What the hook says when `git` gave no verdict on whether the new worktree
/// ignores the settings file (the text after [`DECLINED`]).
const IGNORE_UNKNOWN_LINE: &str = "could not ask Git whether `.claude/settings.local.json` is ignored in this worktree; CVC writes a machine-specific path there and creates nothing it cannot confirm Git ignores (run `cvc harness install claude-code` here)";
/// What `cvc harness install claude-code` says on stderr in the same case.
const NOT_IGNORED_WARNING: &str = "  warning: Git does not ignore .claude/settings.local.json in this checkout: a .gitignore rule overrides the entry CVC added to .git/info/exclude. The file holds a machine-specific path; do not commit it, or fix the rule.";
/// What `cvc harness install claude-code` says on stdout when it adds the entry.
const ADDED_ENTRY_LINE: &str = "  added .claude/settings.local.json to the repository's info/exclude so the machine-specific path is never committed";
/// A `.gitignore` rule that leaves the settings file visible to Git whatever
/// `info/exclude` says.
const OVERRIDING_RULE: &str = "!/.claude/settings.local.json\n";

/// One finished command.
struct Run {
    success: bool,
    /// The exit status; `None` when a signal ended the command.
    code: Option<i32>,
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
        self.run_exporting(program, cwd, path, args, &[])
    }

    /// [`Fixture::run`] with `exported` added to the command's environment
    /// once this process's own `GIT_*` variables have been taken out of it:
    /// what a command started from inside Git would see.
    fn run_exporting(
        &self,
        program: impl AsRef<OsStr>,
        cwd: &Path,
        path: &Path,
        args: &[&str],
        exported: &[(&str, &Path)],
    ) -> Run {
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
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("CVC_CALL_LOG", &self.calls)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("empty.gitconfig"))
            .env("GIT_TEMPLATE_DIR", self.home.join("templates"))
            .stdin(Stdio::null());
        for (name, value) in exported {
            command.env(name, value);
        }
        let output = output_retrying(&mut command);
        Run {
            success: output.status.success(),
            code: output.status.code(),
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
    /// plumbing, so no working tree (the main checkout's included) changes. For
    /// mode `160000` (a submodule) `content` is the id of the commit the entry
    /// names, which does not have to exist in this repository.
    fn branch_with(&self, branch: &str, files: &[(&str, &str, &str)]) {
        let index = self.root.join(format!("index-{branch}"));
        let git = |args: &[&str], stdin: Option<&str>| -> String {
            let mut command = Command::new(real_git());
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
            let blob = if *mode == "160000" {
                (*content).to_owned()
            } else {
                git(&["hash-object", "-w", "--stdin"], Some(content))
            };
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
    let mut shell = Command::new("/bin/sh");
    for name in CLAUDE_CODE_ENV {
        shell.env_remove(name);
    }
    let mut child = shell
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
    for name in CLAUDE_CODE_ENV {
        command.env_remove(name);
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
        for name in CLAUDE_CODE_ENV {
            command.env_remove(name);
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
            code: output.status.code(),
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

// ---- a `.gitignore` rule that overrides the `info/exclude` entry ----
//
// CVC keeps `.claude/settings.local.json` out of commits with an entry in the
// repository's `info/exclude`, which it adds whenever that exact line is
// missing, whatever else ignores the path. Git reads the `.gitignore` files of a
// checkout in preference to that file, so a rule such as
// `!/.claude/settings.local.json` leaves the settings file visible to Git
// although the entry is there. Once the entry is in, `git check-ignore` is asked
// once: the hook for a new worktree creates the file only when Git says it is
// ignored, and declines when Git says it is not or gives no verdict at all
// (`git` cannot be run, ends with some other status, or refuses to judge the
// path, as it does inside a submodule). These tests use the fixture's empty
// `HOME` and switched-off global Git configuration, and take what is ignored,
// and what a commit could carry, from `git` itself.

impl Fixture {
    /// Commits a `.gitignore` holding `rules` in the main checkout.
    fn commit_gitignore(&self, rules: &str) {
        fs::write(self.repo.join(".gitignore"), rules).unwrap();
        self.git_ok(&self.repo, &["add", ".gitignore"]);
        self.git_ok(&self.repo, &["commit", "-m", "ignore rules"]);
    }

    /// How many times the repository's `info/exclude` lists the settings file.
    fn exclude_entries(&self) -> usize {
        fs::read_to_string(self.repo.join(".git/info/exclude"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.trim() == EXCLUDE_ENTRY)
            .count()
    }

    /// Whether Git ignores the settings file in `checkout`: the exit status of
    /// `git check-ignore -q` (0 ignored, 1 not ignored; a failure says so on
    /// stderr). The index is consulted, as `git add` and `git status` do, so a
    /// sparse checkout's ignore files that only the index holds count.
    fn git_ignores_settings(&self, checkout: &Path) -> bool {
        let run = self.git(checkout, &["check-ignore", "-q", "--", SETTINGS]);
        assert!(
            run.success || run.stderr.is_empty(),
            "git check-ignore failed: {}",
            run.stderr
        );
        run.success
    }

    /// The commit `HEAD` of the main checkout names.
    fn head_oid(&self) -> String {
        self.git_ok(&self.repo, &["rev-parse", "HEAD"])
            .stdout
            .trim()
            .to_owned()
    }

    /// Records the wrapper for new worktrees, as an install would, without
    /// installing anything in the main checkout: no settings file there and
    /// nothing in `info/exclude`, so whatever the hook leaves in the new
    /// worktree and in `info/exclude` is its own doing.
    fn record_without_installing(&self) {
        let layout = cvc_core::repository::RepositoryLayout::discover(&self.repo).unwrap();
        cvc_core::claude_code::harness_record::write_record(&layout, &self.wrapper).unwrap();
    }

    /// A `PATH` directory named `name` holding a stand-in `git` that, when
    /// `FAKE_GIT_LOG` names a file in its environment, appends its arguments
    /// (one `ARGS:` line) and the `GIT_` variables of its environment to that
    /// file, and then runs `body`, a piece of `sh` such as `exit 1`. No other
    /// variable is logged: the rest of an environment can hold secrets, and a
    /// failed assertion prints the log.
    fn fake_git(&self, name: &str, body: &str) -> PathBuf {
        let dir = self.root.join(name);
        let script = dir.join("git");
        executable(
            &script,
            &format!(
                "#!/bin/sh\nif [ -n \"${{FAKE_GIT_LOG:-}}\" ]; then\n  {{ printf 'ARGS: %s\\n' \"$*\"; /usr/bin/env | while IFS= read -r line; do case $line in GIT_*) printf '%s\\n' \"$line\";; esac; done; }} >> \"$FAKE_GIT_LOG\"\nfi\n{body}\n"
            ),
        );
        // A sibling thread's fork can hold the script's write descriptor for a
        // moment, and a start of the finished file then fails with "Text file
        // busy" (see `output_retrying`). Start it once here, where that is
        // retried, so the starts that count do not meet the race.
        let mut start = Command::new(&script);
        start.env_remove("FAKE_GIT_LOG");
        output_retrying(&mut start);
        dir
    }

    /// `git status --porcelain -uall` in `checkout`: what a commit made there
    /// could carry.
    fn status(&self, checkout: &Path) -> String {
        self.git_ok(checkout, &["status", "--porcelain", "-uall"])
            .stdout
    }

    /// `git worktree add <root>/<branch> <branch>` for a branch that exists.
    fn worktree_add_existing(&self, branch: &str) -> (PathBuf, Run) {
        let worktree = self.root.join(branch);
        let run = self.git(
            &self.repo,
            &["worktree", "add", worktree.to_str().unwrap(), branch],
        );
        (fs::canonicalize(&worktree).unwrap_or(worktree), run)
    }

    /// `cvc harness install claude-code --binary <wrapper>` in `cwd` with
    /// `exported` in its environment, whether or not it succeeds.
    fn install_exporting(&self, cwd: &Path, exported: &[(&str, &Path)]) -> Run {
        self.run_exporting(
            env!("CARGO_BIN_EXE_cvc"),
            cwd,
            &self.git_only,
            &[
                "harness",
                "install",
                "claude-code",
                "--binary",
                self.wrapper.to_str().unwrap(),
            ],
            exported,
        )
    }

    /// Whether this Git knows the global option `--no-lazy-fetch` (Git 2.44),
    /// which it hands on to its hooks as `GIT_NO_LAZY_FETCH=1`.
    fn git_knows_no_lazy_fetch(&self) -> bool {
        self.git(&self.repo, &["--no-lazy-fetch", "rev-parse", "--git-dir"])
            .success
    }

    /// A blobless, sparse clone of the main checkout in `<root>/<name>`: Git
    /// fetches an object the clone lacks from the main checkout when it needs
    /// one, and only the files at the top level are checked out, so the ignore
    /// files in directories are in the index and nowhere else. Set up as
    /// [`Fixture::ready`] sets up the main checkout (`cvc init`, capture
    /// acknowledged), with the wrapper recorded for new worktrees and nothing
    /// installed. `None` when this Git cannot make such a clone.
    fn partial_clone(&self, name: &str) -> Option<PathBuf> {
        // A clone over `file://` gets the filter it asks for only when the
        // repository it clones from allows filters, and the objects the clone
        // lacks are later asked for one by one, by id.
        for (key, value) in [
            ("uploadpack.allowFilter", "true"),
            ("uploadpack.allowAnySHA1InWant", "true"),
        ] {
            self.git_ok(&self.repo, &["config", key, value]);
        }
        let clone = self.root.join(name);
        let url = format!("file://{}", self.repo.display());
        let cloned = self.git(
            &self.root,
            &[
                "clone",
                "--quiet",
                "--filter=blob:none",
                "--sparse",
                &url,
                clone.to_str().unwrap(),
            ],
        );
        if !cloned.success {
            eprintln!("this Git could not clone: {}", cloned.stderr);
            return None;
        }
        // A server that ignores the filter leaves a clone with every object.
        let promisor = self.git(&clone, &["config", "--get", "remote.origin.promisor"]);
        if promisor.stdout.trim() != "true" {
            eprintln!("the clone is not a partial clone; its remote gave every object");
            return None;
        }
        self.cvc_ok(&clone, &["init"]);
        let repository = git2::Repository::open(&clone).unwrap();
        cvc_core::privacy::acknowledge_capture(&repository).unwrap();
        let layout = cvc_core::repository::RepositoryLayout::discover(&clone).unwrap();
        cvc_core::claude_code::harness_record::write_record(&layout, &self.wrapper).unwrap();
        Some(fs::canonicalize(&clone).unwrap_or(clone))
    }
}

/// How many lines of `text` are exactly `line`.
fn lines_equal_to(text: &str, line: &str) -> usize {
    text.lines().filter(|candidate| *candidate == line).count()
}

#[test]
fn harness_install_warns_when_a_gitignore_rule_leaves_the_settings_file_visible_to_git() {
    let fixture = Fixture::ready();
    fixture.commit_gitignore(OVERRIDING_RULE);

    // Exit 0 (`install` asserts it), and the warning once on stderr.
    let installed = fixture.install(&fixture.repo);
    assert_eq!(
        lines_equal_to(&installed.stderr, NOT_IGNORED_WARNING),
        1,
        "{}",
        installed.stderr
    );
    // The install went through all the same: the file holds the hook entries...
    assert_hooks_run(&fixture.repo, &fixture.wrapper);
    // ...and Git lists it as a new file, which is what the warning says.
    assert!(!fixture.git_ignores_settings(&fixture.repo));
    assert_eq!(fixture.status(&fixture.repo), format!("?? {SETTINGS}\n"));
    assert_eq!(fixture.exclude_entries(), 1);

    // Run again, it reports the same and adds no second entry.
    let again = fixture.install(&fixture.repo);
    assert!(again.stdout.contains("already present"), "{}", again.stdout);
    assert_eq!(
        lines_equal_to(&again.stderr, NOT_IGNORED_WARNING),
        1,
        "{}",
        again.stderr
    );
    assert_eq!(fixture.exclude_entries(), 1);
}

#[test]
fn a_new_worktree_whose_branch_overrides_the_exclude_entry_gets_no_settings_file() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    fixture.branch_with(
        "overriding-rule",
        &[("100644", ".gitignore", OVERRIDING_RULE)],
    );

    // `git worktree add` exits 0 and the hook says why it installed nothing:
    // one line, on stderr.
    let (wt, run) = fixture.worktree_add_existing("overriding-rule");
    assert!(run.success, "{}", run.stderr);
    let declined = format!("{DECLINED}{NOT_IGNORED_LINE}");
    assert_eq!(lines_equal_to(&run.stderr, &declined), 1, "{}", run.stderr);
    assert_eq!(run.cvc_lines(), [declined.as_str()], "{}", run.all());
    // The worktree has no `.claude` at all, hence nothing a commit could carry.
    assert!(!wt.join(".claude").exists());
    assert_eq!(fixture.status(&wt), "");
    assert_eq!(fixture.exclude_entries(), 1);

    // The entry is added when it is missing, with the rest of the file kept,
    // and a second declined worktree adds no second one.
    let exclude = fixture.repo.join(".git/info/exclude");
    fs::write(&exclude, "# mine\n").unwrap();
    for branch in ["second", "third"] {
        let wt = fixture.root.join(branch);
        let run = fixture.git(
            &fixture.repo,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                wt.to_str().unwrap(),
                "overriding-rule",
            ],
        );
        assert!(run.success, "{branch}: {}", run.stderr);
        assert_eq!(
            run.cvc_lines(),
            [declined.as_str()],
            "{branch}: {}",
            run.all()
        );
        assert!(!wt.join(".claude").exists(), "{branch}");
        assert_eq!(fixture.exclude_entries(), 1, "{branch}");
        assert!(
            fs::read_to_string(&exclude)
                .unwrap()
                .starts_with("# mine\n"),
            "{branch}"
        );
    }
}

#[test]
fn a_new_worktree_is_declined_for_each_way_its_branch_can_override_the_exclude_entry() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    // (branch, file it commits, rules in it)
    let cases = [
        (
            "nested-rule",
            ".claude/.gitignore",
            "!settings.local.json\n",
        ),
        ("wildcard-rule", ".gitignore", "!/.claude/*.json\n"),
        (
            "re-included",
            ".gitignore",
            ".claude/*\n!.claude/settings.local.json\n",
        ),
    ];
    for (branch, file, rules) in cases {
        fixture.branch_with(branch, &[("100644", file, rules)]);

        let (wt, run) = fixture.worktree_add_existing(branch);
        assert!(run.success, "{branch}: {}", run.stderr);
        assert_eq!(
            run.cvc_lines(),
            [format!("{DECLINED}{NOT_IGNORED_LINE}")],
            "{branch}: {}",
            run.all()
        );
        assert!(!wt.join(SETTINGS).exists(), "{branch}");
        assert_eq!(fixture.status(&wt), "", "{branch}");
    }
    assert_eq!(fixture.exclude_entries(), 1);
}

#[test]
fn a_new_worktree_without_an_overriding_rule_gets_the_hooks_and_a_clean_status() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    // Branches with `.gitignore` files of their own, none of whose rules
    // overrides the entry: the new worktree is set up as usual.
    let cases = [
        ("unrelated-rules", ".gitignore", "target/\n*.log\n"),
        // Re-including a neighbour says nothing about the settings file.
        (
            "neighbour-rule",
            ".gitignore",
            ".claude/*\n!.claude/settings.json\n",
        ),
    ];
    for (branch, file, rules) in cases {
        fixture.branch_with(branch, &[("100644", file, rules)]);

        let (wt, run) = fixture.worktree_add_existing(branch);
        assert!(run.success, "{branch}: {}", run.stderr);
        assert_eq!(run.cvc_lines(), [INSTALLED_LINE], "{branch}: {}", run.all());
        assert_hooks_run(&wt, &fixture.wrapper);
        assert!(fixture.git_ignores_settings(&wt), "{branch}");
        assert_eq!(fixture.status(&wt), "", "{branch}");
    }
}

#[test]
fn the_remedy_the_declined_worktree_is_told_about_ends_the_warning() {
    let fixture = Fixture::ready();
    fixture.install(&fixture.repo);
    fixture.branch_with(
        "overriding-rule",
        &[("100644", ".gitignore", OVERRIDING_RULE)],
    );
    let (wt, run) = fixture.worktree_add_existing("overriding-rule");
    assert_eq!(
        run.cvc_lines(),
        [format!("{DECLINED}{NOT_IGNORED_LINE}")],
        "{}",
        run.all()
    );

    // The command the message names, run there with the rule still in place,
    // writes the file and warns that Git lists it...
    let installed = fixture.install(&wt);
    assert_eq!(
        lines_equal_to(&installed.stderr, NOT_IGNORED_WARNING),
        1,
        "{}",
        installed.stderr
    );
    assert_hooks_run(&wt, &fixture.wrapper);
    assert_eq!(fixture.status(&wt), format!("?? {SETTINGS}\n"));

    // ...and once the rule is fixed, the same command has nothing to warn about
    // and Git ignores the file.
    fs::write(wt.join(".gitignore"), "# nothing about .claude here\n").unwrap();
    let installed = fixture.install(&wt);
    assert!(
        installed.stdout.contains("already present"),
        "{}",
        installed.stdout
    );
    assert!(
        !installed.stderr.contains("Git does not ignore"),
        "{}",
        installed.stderr
    );
    assert!(fixture.git_ignores_settings(&wt));
    assert_eq!(fixture.status(&wt), " M .gitignore\n");
    assert_eq!(fixture.exclude_entries(), 1);
}

/// What an install in the main checkout reported and left behind, given
/// whether the checkout's `.gitignore` overrides the entry: the warning exactly
/// when it does, the hook entries written either way, and `git` agreeing that
/// the file is ignored exactly when it does not.
fn assert_install_verdict(fixture: &Fixture, installed: &Run, overridden: bool, case: &str) {
    assert!(installed.success, "{case}: {}", installed.stderr);
    assert_eq!(
        lines_equal_to(&installed.stderr, NOT_IGNORED_WARNING),
        usize::from(overridden),
        "{case}: {}",
        installed.stderr
    );
    assert_hooks_run(&fixture.repo, &fixture.wrapper);
    assert_eq!(
        fixture.git_ignores_settings(&fixture.repo),
        !overridden,
        "{case}"
    );
    let expected = if overridden {
        format!("?? {SETTINGS}\n")
    } else {
        String::new()
    };
    assert_eq!(fixture.status(&fixture.repo), expected, "{case}");
}

#[test]
fn harness_install_gives_the_same_verdicts_with_git_dir_naming_the_checkouts_own_git_directory() {
    for overridden in [false, true] {
        for exported in [false, true] {
            let case = format!("rule: {overridden}, GIT_DIR exported: {exported}");
            let fixture = Fixture::ready();
            if overridden {
                fixture.commit_gitignore(OVERRIDING_RULE);
            }
            let git_dir = fixture.repo.join(".git");
            let installed = if exported {
                fixture.install_exporting(&fixture.repo, &[("GIT_DIR", git_dir.as_path())])
            } else {
                fixture.install_exporting(&fixture.repo, &[])
            };
            assert_install_verdict(&fixture, &installed, overridden, &case);
        }
    }
}

#[test]
fn repository_variables_exported_to_the_install_do_not_change_the_verdict() {
    // (variable, where it points inside a different repository, whether the
    // checkout has the overriding rule). Each pairing is one where that other
    // repository would turn Git's answer around if the variable reached the
    // `git` that cvc asks.
    let cases = [
        ("GIT_DIR", ".git", false),
        ("GIT_COMMON_DIR", ".git", false),
        ("GIT_WORK_TREE", "", true),
    ];
    for (variable, inside, overridden) in cases {
        let case = format!("{variable}, rule: {overridden}");
        let fixture = Fixture::ready();
        if overridden {
            fixture.commit_gitignore(OVERRIDING_RULE);
        }
        let other = fixture.root.join("other-repository");
        fixture.git_ok(&fixture.root, &["init", other.to_str().unwrap()]);
        let value = if inside.is_empty() {
            other.clone()
        } else {
            other.join(inside)
        };

        let installed = fixture.install_exporting(&fixture.repo, &[(variable, value.as_path())]);
        assert_install_verdict(&fixture, &installed, overridden, &case);
    }
}

#[test]
fn harness_install_run_from_a_subdirectory_judges_the_file_at_the_checkout_root() {
    for overridden in [false, true] {
        let case = format!("rule: {overridden}");
        let fixture = Fixture::ready();
        if overridden {
            fixture.commit_gitignore(OVERRIDING_RULE);
        }
        let nested = fixture.repo.join("src/nested");
        fs::create_dir_all(&nested).unwrap();

        let installed = fixture.install_exporting(&nested, &[]);
        assert_install_verdict(&fixture, &installed, overridden, &case);
        assert!(!nested.join(".claude").exists(), "{case}");
    }
}

#[test]
fn harness_install_judges_the_file_without_git_when_git_cannot_be_run() {
    let fixture = Fixture::ready();
    let no_git = fixture.root.join("path-without-git");
    fs::create_dir(&no_git).unwrap();

    let installed = fixture.run(
        env!("CARGO_BIN_EXE_cvc"),
        &fixture.repo,
        &no_git,
        &[
            "harness",
            "install",
            "claude-code",
            "--binary",
            fixture.wrapper.to_str().unwrap(),
        ],
    );
    // With no `git` to ask, libgit2's answer stands: the entry it needs is
    // added, the file counts as ignored, and Git agrees.
    assert_install_verdict(&fixture, &installed, false, "no git on PATH");
}

#[test]
fn harness_install_adds_the_info_exclude_entry_even_when_the_global_excludes_list_the_file() {
    let fixture = Fixture::ready();
    // The global excludes file, as the repository's own configuration names it,
    // so that libgit2 reads it too.
    let global = fixture.home.join("global-ignore");
    fs::write(&global, ".claude/settings.local.json\n").unwrap();
    fixture.git_ok(
        &fixture.repo,
        &["config", "core.excludesFile", global.to_str().unwrap()],
    );
    // The premise: Git ignores the file before CVC has added anything.
    assert!(fixture.git_ignores_settings(&fixture.repo));
    assert_eq!(fixture.exclude_entries(), 0);

    let installed = fixture.install(&fixture.repo);
    assert_eq!(
        lines_equal_to(&installed.stdout, ADDED_ENTRY_LINE),
        1,
        "{}",
        installed.stdout
    );
    assert!(
        !installed.stderr.contains("Git does not ignore"),
        "{}",
        installed.stderr
    );
    assert_eq!(fixture.exclude_entries(), 1);
    assert_hooks_run(&fixture.repo, &fixture.wrapper);

    // Run again, the line is there and nothing is added or said.
    let again = fixture.install(&fixture.repo);
    assert_eq!(
        lines_equal_to(&again.stdout, ADDED_ENTRY_LINE),
        0,
        "{}",
        again.stdout
    );
    assert_eq!(fixture.exclude_entries(), 1);

    // A new worktree is set up as usual and adds no second entry.
    let (wt, run) = fixture.worktree_add("wt");
    assert_eq!(run.cvc_lines(), [INSTALLED_LINE], "{}", run.all());
    assert!(fixture.git_ignores_settings(&wt));
    assert_eq!(fixture.exclude_entries(), 1);
}

#[test]
fn global_options_given_to_git_worktree_add_do_not_change_what_the_new_worktree_gets() {
    let fixture = Fixture::ready();
    fixture.record_without_installing();
    fixture.branch_with(
        "overriding-rule",
        &[("100644", ".gitignore", OVERRIDING_RULE)],
    );
    let one = Path::new("1");
    // (case, options placed before `worktree`, variables exported to the
    // command). Git passes each option on to its hooks as the variable of the
    // same name. `git check-ignore` refuses to run with a pathspec one, and
    // `GIT_NO_LAZY_FETCH` (from `--no-lazy-fetch`) keeps Git from fetching an
    // object a partial clone lacks, which is not what the hook means to ask.
    type Case<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, &'a Path)]);
    let cases: [Case<'_>; 6] = [
        ("--literal-pathspecs", &["--literal-pathspecs"], &[]),
        ("--glob-pathspecs", &["--glob-pathspecs"], &[]),
        ("--noglob-pathspecs", &["--noglob-pathspecs"], &[]),
        ("--icase-pathspecs", &["--icase-pathspecs"], &[]),
        (
            "GIT_ICASE_PATHSPECS exported",
            &[],
            &[("GIT_ICASE_PATHSPECS", one)],
        ),
        ("--no-lazy-fetch", &["--no-lazy-fetch"], &[]),
    ];
    let knows_no_lazy_fetch = fixture.git_knows_no_lazy_fetch();
    for (index, (case, options, exported)) in cases.into_iter().enumerate() {
        if options.contains(&"--no-lazy-fetch") && !knows_no_lazy_fetch {
            eprintln!("this Git has no --no-lazy-fetch; skipping that case");
            continue;
        }
        let add = |branch: &str, start: &[&str]| {
            let worktree = fixture.root.join(branch);
            let mut args: Vec<&str> = options.to_vec();
            args.extend(["worktree", "add", "-b", branch, worktree.to_str().unwrap()]);
            args.extend(start.iter().copied());
            let run = fixture.run_exporting(
                real_git(),
                &fixture.repo,
                &fixture.git_only,
                &args,
                exported,
            );
            (fs::canonicalize(&worktree).unwrap_or(worktree), run)
        };

        // The branch's rule overrides the entry: the hook declines, as it does
        // without the option, and nothing is created.
        let (wt, run) = add(&format!("declined-{index}"), &["overriding-rule"]);
        assert!(run.success, "{case}: {}", run.stderr);
        assert_eq!(
            run.cvc_lines(),
            [format!("{DECLINED}{NOT_IGNORED_LINE}")],
            "{case}: {}",
            run.all()
        );
        assert!(!wt.join(".claude").exists(), "{case}");
        assert_eq!(fixture.status(&wt), "", "{case}");
        assert_eq!(fixture.exclude_entries(), 1, "{case}");

        // No such rule: the worktree is set up as usual.
        let (wt, run) = add(&format!("installed-{index}"), &[]);
        assert!(run.success, "{case}: {}", run.stderr);
        assert_eq!(run.cvc_lines(), [INSTALLED_LINE], "{case}: {}", run.all());
        assert_hooks_run(&wt, &fixture.wrapper);
        assert!(fixture.git_ignores_settings(&wt), "{case}");
        assert_eq!(fixture.status(&wt), "", "{case}");
        assert_eq!(fixture.exclude_entries(), 1, "{case}");
    }
}

#[test]
fn a_new_worktree_is_declined_while_git_cannot_be_run_and_gets_the_hooks_once_it_can() {
    let fixture = Fixture::ready();
    // Created before anything is recorded, so it has no settings of its own.
    let (wt, run) = fixture.worktree_add("wt");
    assert!(run.success && !run.all().contains("CVC"), "{}", run.all());
    fixture.record_without_installing();
    let head = fixture.git_ok(&wt, &["rev-parse", "HEAD"]).stdout;
    let hook = |path: &Path| {
        fixture.run(
            env!("CARGO_BIN_EXE_cvc"),
            &wt,
            path,
            &["hook", "post-checkout", NULL_OID, head.trim(), "1"],
        )
    };

    // A `PATH` with no `git` in it: no verdict, so nothing is created. The
    // entry was added before Git was asked, and stays.
    let no_git = fixture.root.join("path-without-git");
    fs::create_dir(&no_git).unwrap();
    let run = hook(&no_git);
    assert!(run.success, "{}", run.all());
    assert_eq!(
        run.cvc_lines(),
        [format!("{DECLINED}{IGNORE_UNKNOWN_LINE}")],
        "{}",
        run.all()
    );
    assert_eq!(run.stdout, "");
    assert!(!wt.join(".claude").exists());
    assert_eq!(fixture.exclude_entries(), 1);

    // The same command with `git` on `PATH` sets the worktree up.
    let run = hook(&fixture.git_only);
    assert!(run.success, "{}", run.all());
    assert_eq!(run.stdout.trim_end(), INSTALLED_LINE);
    assert_eq!(run.stderr, "");
    assert_hooks_run(&wt, &fixture.wrapper);
    assert!(fixture.git_ignores_settings(&wt));
    assert_eq!(fixture.status(&wt), "");
    assert_eq!(fixture.exclude_entries(), 1);
}

#[test]
fn the_exit_status_of_git_decides_what_a_new_worktree_gets() {
    let fixture = Fixture::ready();
    // (what the stand-in `git` does, the line the hook declines with; `None`
    // when the worktree gets the hooks)
    let cases: [(&str, Option<&str>); 6] = [
        ("exit 0", None),
        ("exit 1", Some(NOT_IGNORED_LINE)),
        ("exit 2", Some(IGNORE_UNKNOWN_LINE)),
        ("exit 128", Some(IGNORE_UNKNOWN_LINE)),
        ("exit 255", Some(IGNORE_UNKNOWN_LINE)),
        ("kill -9 $$", Some(IGNORE_UNKNOWN_LINE)),
    ];
    // Created before anything is recorded, so none has settings of its own; one
    // more than the cases, for the `git` that cannot be started.
    let worktrees: Vec<PathBuf> = (0..=cases.len())
        .map(|index| {
            let (wt, run) = fixture.worktree_add(&format!("wt{index}"));
            assert!(run.success && !run.all().contains("CVC"), "{}", run.all());
            wt
        })
        .collect();
    fixture.record_without_installing();
    let head = fixture.head_oid();
    let git_dir = fixture.repo.join(".git");
    let index_file = fixture.root.join("some-index");
    let one = Path::new("1");
    let forwarded = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
        "GIT_NO_LAZY_FETCH",
    ];

    for (index, ((body, declined), wt)) in cases.iter().zip(&worktrees).enumerate() {
        let case = *body;
        let path = fixture.fake_git(&format!("path-fake-git-{index}"), body);
        let log = fixture.root.join(format!("fake-git-{index}.log"));
        // Every variable a surrounding Git command can export is exported to the
        // hook command, and so is a `GIT_TERMINAL_PROMPT` that lets Git ask
        // questions.
        let run = fixture.run_exporting(
            env!("CARGO_BIN_EXE_cvc"),
            wt,
            &path,
            &["hook", "post-checkout", NULL_OID, head.as_str(), "1"],
            &[
                ("FAKE_GIT_LOG", log.as_path()),
                ("GIT_DIR", git_dir.as_path()),
                ("GIT_WORK_TREE", wt.as_path()),
                ("GIT_COMMON_DIR", git_dir.as_path()),
                ("GIT_INDEX_FILE", index_file.as_path()),
                ("GIT_LITERAL_PATHSPECS", one),
                ("GIT_GLOB_PATHSPECS", one),
                ("GIT_NOGLOB_PATHSPECS", one),
                ("GIT_ICASE_PATHSPECS", one),
                ("GIT_NO_LAZY_FETCH", one),
                ("GIT_TERMINAL_PROMPT", one),
            ],
        );
        assert!(run.success, "{case}: {}", run.all());
        match declined {
            None => {
                assert_eq!(run.stdout.trim_end(), INSTALLED_LINE, "{case}");
                assert_eq!(run.stderr, "", "{case}");
                assert_hooks_run(wt, &fixture.wrapper);
            }
            Some(line) => {
                assert_eq!(
                    run.cvc_lines(),
                    [format!("{DECLINED}{line}")],
                    "{case}: {}",
                    run.all()
                );
                assert_eq!(run.stdout, "", "{case}");
                assert!(!wt.join(".claude").exists(), "{case}");
            }
        }
        assert_eq!(fixture.exclude_entries(), 1, "{case}");

        // Git was asked once, about the settings file of this worktree, none
        // of those variables reached it, and it was told never to prompt.
        let logged = fs::read_to_string(&log).unwrap();
        // The log holds the arguments and the `GIT_` variables, nothing else,
        // so the messages below never print the rest of an environment, which
        // can hold secrets. This check names no line that it finds.
        let others = logged
            .lines()
            .filter(|line| !line.starts_with("ARGS: ") && !line.starts_with("GIT_"))
            .count();
        assert_eq!(
            others, 0,
            "{case}: the stand-in git logged {others} lines that are neither its arguments nor GIT_ variables"
        );
        let asked: Vec<&str> = logged
            .lines()
            .filter(|line| line.starts_with("ARGS: "))
            .collect();
        let expected = format!(
            "ARGS: -C {} check-ignore --quiet -- {SETTINGS}",
            wt.display()
        );
        assert_eq!(asked, [expected.as_str()], "{case}");
        // The log does show the `GIT_` variables: what the fixture sets is in it.
        assert!(
            logged.lines().any(|line| line == "GIT_CONFIG_NOSYSTEM=1"),
            "{case}: {logged}"
        );
        for name in forwarded {
            assert!(
                !logged
                    .lines()
                    .any(|line| line.starts_with(&format!("{name}="))),
                "{case}: {name} reached git: {logged}"
            );
        }
        // Nobody is there to answer a question, so Git is told never to ask
        // one, although the environment of the hook exports `1`.
        let prompts: Vec<&str> = logged
            .lines()
            .filter(|line| line.starts_with("GIT_TERMINAL_PROMPT="))
            .collect();
        assert_eq!(prompts, ["GIT_TERMINAL_PROMPT=0"], "{case}: {logged}");
    }

    // A `git` that is there but cannot be started gives no verdict either.
    let unstartable = fixture.root.join("path-unstartable-git");
    fs::create_dir(&unstartable).unwrap();
    fs::write(unstartable.join("git"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(unstartable.join("git"), fs::Permissions::from_mode(0o644)).unwrap();
    let wt = worktrees.last().unwrap();
    let run = fixture.run(
        env!("CARGO_BIN_EXE_cvc"),
        wt,
        &unstartable,
        &["hook", "post-checkout", NULL_OID, head.as_str(), "1"],
    );
    assert!(run.success, "{}", run.all());
    assert_eq!(
        run.cvc_lines(),
        [format!("{DECLINED}{IGNORE_UNKNOWN_LINE}")],
        "{}",
        run.all()
    );
    assert!(!wt.join(".claude").exists());
}

#[test]
fn a_new_worktree_whose_claude_is_a_submodule_is_declined_and_left_empty() {
    let fixture = Fixture::ready();
    fixture.record_without_installing();
    fixture.branch_with(
        "claude-submodule",
        &[("160000", ".claude", fixture.head_oid().as_str())],
    );

    let (wt, run) = fixture.worktree_add_existing("claude-submodule");
    assert!(run.success, "{}", run.stderr);
    assert_eq!(
        run.cvc_lines(),
        [format!("{DECLINED}{IGNORE_UNKNOWN_LINE}")],
        "{}",
        run.all()
    );
    // Git left the empty directory it leaves for a submodule that was not
    // cloned, and CVC put nothing in it.
    assert!(wt.join(".claude").is_dir());
    assert_eq!(fs::read_dir(wt.join(".claude")).unwrap().count(), 0);
    assert!(!wt.join(SETTINGS).exists());
    assert_eq!(fixture.status(&wt), "");
    assert_eq!(fixture.exclude_entries(), 1);
    // The premise: Git itself refuses to judge a path inside the submodule.
    let asked = fixture.git(&wt, &["check-ignore", "-q", "--", SETTINGS]);
    assert_eq!(asked.code, Some(128), "{}", asked.stderr);
}

#[test]
fn a_sparse_worktree_is_judged_by_the_ignore_files_the_branch_keeps_in_the_index() {
    let fixture = Fixture::ready();
    fixture.record_without_installing();
    // The branch's ignore file sits outside the sparse cone, so a worktree
    // created from the main checkout's sparse state does not have it on disk.
    fixture.branch_with(
        "sparse-rule",
        &[
            ("100644", "src/lib.rs", "pub fn f() {}\n"),
            ("100644", ".claude/.gitignore", "!settings.local.json\n"),
        ],
    );
    let sparse = fixture.git(&fixture.repo, &["sparse-checkout", "set", "--cone", "src"]);
    if !sparse.success {
        eprintln!(
            "this Git has no sparse-checkout: {}; skipping",
            sparse.stderr
        );
        return;
    }
    let (wt, run) = fixture.worktree_add_existing("sparse-rule");
    // Later commands in the main checkout see it whole again.
    fixture.git_ok(&fixture.repo, &["sparse-checkout", "disable"]);
    assert!(run.success, "{}", run.stderr);
    if !wt.join("src/lib.rs").is_file() || wt.join(".claude/.gitignore").exists() {
        eprintln!("this Git gave the new worktree no sparse checkout; skipping");
        return;
    }

    assert_eq!(
        run.cvc_lines(),
        [format!("{DECLINED}{NOT_IGNORED_LINE}")],
        "{}",
        run.all()
    );
    assert!(!wt.join(".claude").exists());
    assert_eq!(fixture.exclude_entries(), 1);
    // The ignore file is in the index only, and Git, which reads it there, does
    // not ignore the settings file.
    let listed = fixture.git_ok(&wt, &["ls-files", "-t"]).stdout;
    assert!(listed.contains("S .claude/.gitignore"), "{listed}");
    assert!(!fixture.git_ignores_settings(&wt));

    // The install the message names writes the file all the same, warns, and
    // Git lists it as a new file.
    let installed = fixture.install(&wt);
    assert_eq!(
        lines_equal_to(&installed.stderr, NOT_IGNORED_WARNING),
        1,
        "{}",
        installed.stderr
    );
    assert_hooks_run(&wt, &fixture.wrapper);
    assert_eq!(fixture.status(&wt), format!("?? {SETTINGS}\n"));
}

#[test]
fn a_partial_clones_new_worktree_is_judged_by_the_fetched_ignore_file_even_under_no_lazy_fetch() {
    let fixture = Fixture::new();
    if !fixture.git_knows_no_lazy_fetch() {
        eprintln!("this Git has no --no-lazy-fetch; skipping");
        return;
    }
    // The branch's ignore file is in a directory that the sparse checkout of a
    // clone leaves out, so a blobless clone has the file's place in the trees
    // it fetched and no copy of the file: Git has to fetch it to read it.
    fixture.branch_with(
        "overriding-rule",
        &[("100644", ".claude/.gitignore", "!settings.local.json\n")],
    );
    let ignore_file = fixture
        .git_ok(
            &fixture.repo,
            &["rev-parse", "overriding-rule:.claude/.gitignore"],
        )
        .stdout
        .trim()
        .to_owned();
    let one = Path::new("1");
    // (case, options placed before `worktree`, variables exported to the
    // command). Git fetches the ignore file when the hook asks, and the hook
    // declines. It must do the same when the command told Git not to fetch,
    // which Git hands on to the hook as `GIT_NO_LAZY_FETCH=1`: an ignore file
    // that has not been fetched yet is not an ignore file that is not there.
    type Case<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, &'a Path)]);
    let cases: [Case<'_>; 3] = [
        ("plain", &[], &[]),
        ("--no-lazy-fetch", &["--no-lazy-fetch"], &[]),
        (
            "GIT_NO_LAZY_FETCH exported",
            &[],
            &[("GIT_NO_LAZY_FETCH", one)],
        ),
    ];
    for (index, (case, options, exported)) in cases.into_iter().enumerate() {
        // A clone for each case: once fetched, the file stays.
        let Some(clone) = fixture.partial_clone(&format!("partial-{index}")) else {
            eprintln!("this Git cannot make a blobless sparse clone; skipping");
            return;
        };
        // Whether the clone holds the ignore file, asked without fetching it.
        let holds_ignore_file = || {
            fixture
                .git(&clone, &["--no-lazy-fetch", "cat-file", "-e", &ignore_file])
                .success
        };
        assert!(!holds_ignore_file(), "{case}: the clone has the file");

        let worktree = fixture.root.join(format!("wt-{index}"));
        let mut args: Vec<&str> = options.to_vec();
        args.extend([
            "worktree",
            "add",
            "-b",
            "new",
            worktree.to_str().unwrap(),
            "origin/overriding-rule",
        ]);
        let run = fixture.run_exporting(real_git(), &clone, &fixture.git_only, &args, exported);
        let wt = fs::canonicalize(&worktree).unwrap_or(worktree);

        assert!(run.success, "{case}: {}", run.stderr);
        assert_eq!(
            run.cvc_lines(),
            [format!("{DECLINED}{NOT_IGNORED_LINE}")],
            "{case}: {}",
            run.all()
        );
        // The `git` that the hook ran fetched the file, whatever the command
        // told Git, and nothing is created in the new worktree.
        assert!(holds_ignore_file(), "{case}: the hook fetched no file");
        assert!(!wt.join(".claude").exists(), "{case}");
        assert_eq!(fixture.status(&wt), "", "{case}");
        let exclude = fs::read_to_string(clone.join(".git/info/exclude")).unwrap();
        assert_eq!(lines_equal_to(&exclude, EXCLUDE_ENTRY), 1, "{case}");
    }
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
