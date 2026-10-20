#![cfg(unix)]

//! Independent QA additions for public issue 113 (the Claude Code harness
//! reaches new worktrees through Git's `post-checkout` hook).
//!
//! `harness_worktree_cli.rs` covers the main flows. These tests cover what it
//! does not: the ways a worktree can be created (from a subdirectory, from a
//! linked worktree, nested under `<repo>/.claude/worktrees/` as the desktop app
//! does, awkward paths), concurrent creation, a binary path that needs shell
//! quoting, a stale `cvc` earlier on `PATH`, and branches that commit `.claude`
//! content of their own.
//!
//! The two defect reproductions QA added first (D1: a committed `.claude`
//! symlink was followed out of the worktree; D2: `cvc init` appended shell text
//! to a non-shell hook) are fixed and no longer `#[ignore]`d. The review round
//! that fixed them also changed three behaviours these tests had pinned, and
//! they were updated to match: the hook runs the binary named in the
//! repository's record rather than whatever `cvc` is on `PATH` (so the stale-`cvc`
//! test now asserts the stale one is never started); the record is the
//! plain-text `claude-code.path` and settings temporaries have random names;
//! and a settings file that a branch *tracks* is declined and left untouched
//! instead of being merged into.
//!
//! Like the sibling file, everything runs against throwaway repositories with
//! a throwaway `HOME` and an explicit `PATH`; nothing touches the developer's
//! own repositories or `~/.cvc/bin`.
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

const BASIC: &str = include_str!("../../cvc-core/tests/fixtures/claude-code/v2.1.260/basic.jsonl");
const SESSION: &str = "7c1e9f5a-2b2f-4f4e-9c3b-0f2e7a1d5b60";
const INSTALLED_LINE: &str =
    "CVC: installed the Claude Code capture hooks in this worktree (.claude/settings.local.json)";
const SETTINGS: &str = ".claude/settings.local.json";
const EVENTS: [&str; 3] = ["PostToolUse", "Stop", "SessionEnd"];

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

    /// Lines the CVC hook printed.
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
}

fn real_tool(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

impl Fixture {
    /// A repository with one commit and the built `cvc` copied to the stable
    /// `$HOME/.cvc/bin/cvc`, which is the only way the hook can find it.
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        let repo = root.join("repo");
        let git_only = root.join("path-git-only");
        for dir in [&home, &repo, &git_only, &home.join("templates")] {
            fs::create_dir_all(dir).unwrap();
        }
        symlink(real_tool("git").expect("git on PATH"), git_only.join("git")).unwrap();
        let stable = home.join(".cvc/bin/cvc");
        fs::create_dir_all(stable.parent().unwrap()).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_cvc"), &stable).unwrap();
        fs::set_permissions(&stable, fs::Permissions::from_mode(0o755)).unwrap();

        let fixture = Self {
            _temp: temp,
            root,
            home,
            repo,
            git_only,
        };
        let repo = fixture.repo.clone();
        fixture.git_ok(&repo, &["init"]);
        fs::write(repo.join("tracked"), "initial\n").unwrap();
        fixture.git_ok(&repo, &["add", "tracked"]);
        fixture.git_ok(&repo, &["commit", "-m", "initial"]);
        fixture
    }

    /// `cvc init`, capture acknowledged, harness installed from the stable copy.
    fn ready() -> Self {
        let fixture = Self::new();
        fixture.cvc_ok(&fixture.repo, &["init"]);
        fixture.acknowledge();
        fixture.cvc_ok(&fixture.repo, &["harness", "install", "claude-code"]);
        fixture
    }

    fn acknowledge(&self) {
        // The CLI asks for a typed TTY challenge; this is the same core call.
        let repository = git2::Repository::open(&self.repo).unwrap();
        cvc_core::privacy::acknowledge_capture(&repository).unwrap();
    }

    fn stable(&self) -> PathBuf {
        self.home.join(".cvc/bin/cvc")
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>, cwd: &Path, path: &Path) -> Command {
        let mut command = Command::new(program);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_AUTHOR_NAME", "QA")
            .env("GIT_AUTHOR_EMAIL", "qa@example.invalid")
            .env("GIT_COMMITTER_NAME", "QA")
            .env("GIT_COMMITTER_EMAIL", "qa@example.invalid")
            .env("GIT_TEMPLATE_DIR", self.home.join("templates"))
            .stdin(Stdio::null());
        command
    }

    fn finish(mut command: Command) -> Run {
        // Retry while the kernel reports ETXTBSY: this file copies a binary and
        // runs it from parallel test threads, and a child forked by a sibling
        // thread can briefly hold the copy's write descriptor.
        let output = (0..100)
            .find_map(|_| match command.output() {
                Err(error) if error.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    None
                }
                result => Some(result.unwrap()),
            })
            .unwrap_or_else(|| command.output().unwrap());
        Run {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Run {
        self.git_with_path(cwd, &self.git_only, args)
    }

    fn git_with_path(&self, cwd: &Path, path: &Path, args: &[&str]) -> Run {
        let mut command = self.command(real_tool("git").unwrap(), cwd, path);
        command.args(args);
        Self::finish(command)
    }

    fn git_ok(&self, cwd: &Path, args: &[&str]) -> Run {
        let run = self.git(cwd, args);
        assert!(run.success, "git {args:?} failed: {}", run.stderr);
        run
    }

    /// The stable copy of `cvc`, run by absolute path under the GUI `PATH`.
    fn cvc(&self, cwd: &Path, args: &[&str]) -> Run {
        let mut command = self.command(self.stable(), cwd, &self.git_only);
        command.args(args);
        Self::finish(command)
    }

    fn cvc_ok(&self, cwd: &Path, args: &[&str]) -> Run {
        let run = self.cvc(cwd, args);
        assert!(run.success, "cvc {args:?} failed: {}", run.stderr);
        run
    }

    /// `git worktree add <args>`; on success the worktree must have been given
    /// the hooks, announced on Git's stderr, with Git's exit status untouched.
    fn add_installed(&self, cwd: &Path, args: &[&str], worktree: &Path) {
        let mut full = vec!["worktree", "add"];
        full.extend_from_slice(args);
        let run = self.git(cwd, &full);
        assert!(run.success, "git {full:?}: {}", run.stderr);
        assert!(
            run.all().contains(INSTALLED_LINE),
            "{full:?}: {}",
            run.all()
        );
        assert_eq!(run.cvc_lines().len(), 1, "{full:?}: {}", run.all());
        self.assert_hooks(worktree);
    }

    /// Three events, each with exactly one entry running the stable binary.
    fn assert_hooks(&self, worktree: &Path) {
        let settings = settings(worktree);
        let expected = format!("'{}' ingest claude-code --hook", self.stable().display());
        for event in EVENTS {
            let groups = settings["hooks"][event].as_array().unwrap();
            assert_eq!(
                groups.len(),
                1,
                "{}: {event}: {settings}",
                worktree.display()
            );
            assert_eq!(groups[0]["hooks"][0]["command"], expected, "{event}");
        }
    }

    /// Commits `content` at `path` with `mode` on a new branch built with
    /// plumbing, so no working tree (the main checkout's included) changes.
    fn branch_with(&self, branch: &str, mode: &str, path: &str, content: &str) {
        let index = self.root.join(format!("index-{branch}"));
        let git = |args: &[&str], stdin: Option<&str>| -> String {
            let mut command = self.command(real_tool("git").unwrap(), &self.repo, &self.git_only);
            command
                .env("GIT_INDEX_FILE", &index)
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
        let blob = git(&["hash-object", "-w", "--stdin"], Some(content));
        git(&["read-tree", "HEAD"], None);
        git(
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("{mode},{blob},{path}"),
            ],
            None,
        );
        let tree = git(&["write-tree"], None);
        let commit = git(&["commit-tree", &tree, "-p", "HEAD", "-m", branch], None);
        git(&["branch", branch, &commit], None);
        let _ = fs::remove_file(index);
    }

    /// Runs the `Stop` command the worktree's settings hold, as Claude Code
    /// would, over a synthetic transcript recorded in `worktree`, and returns
    /// (all interactions, interactions attributed to `worktree`).
    fn capture_from(&self, worktree: &Path) -> (i64, i64) {
        let transcript = self.root.join(format!("{SESSION}.jsonl"));
        fs::write(
            &transcript,
            BASIC.replace("__WORKTREE__", worktree.to_str().unwrap()),
        )
        .unwrap();
        let payload = format!(
            "{{\"session_id\":\"{SESSION}\",\"transcript_path\":\"{}\",\"cwd\":\"{}\",\"hook_event_name\":\"Stop\"}}",
            transcript.display(),
            worktree.display()
        );
        let command = settings(worktree)["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut shell = self.command("/bin/sh", worktree, &self.git_only);
        shell
            .args(["-c", &command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = shell.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), payload.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let connection = rusqlite::Connection::open(self.repo.join(".git/cvc/index.db")).unwrap();
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
}

fn settings(worktree: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(worktree.join(SETTINGS)).unwrap()).unwrap()
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap()
}

#[test]
fn every_way_of_creating_a_worktree_propagates() {
    let f = Fixture::ready();

    // From a subdirectory of the main checkout.
    let sub = f.repo.join("sub/deep");
    fs::create_dir_all(&sub).unwrap();
    f.add_installed(
        &sub,
        &["../../../from-subdir", "-b", "from-subdir"],
        &f.root.join("from-subdir"),
    );
    assert_eq!(
        fs::read_dir(&sub).unwrap().count(),
        0,
        "nothing leaked into the subdirectory"
    );

    // From inside another linked worktree.
    let first = f.root.join("first");
    f.add_installed(&f.repo, &["../first", "-b", "first"], &first);
    f.add_installed(
        &first,
        &["../from-linked", "-b", "from-linked"],
        &f.root.join("from-linked"),
    );

    // Paths a shell would mangle: the hook must not interpret them.
    for (index, name) in [
        "w with spaces",
        "w it's $x $(true) \u{fc}n\u{ef}",
        "w`bt`;rm",
    ]
    .into_iter()
    .enumerate()
    {
        let branch = format!("special-{index}");
        f.add_installed(
            &f.repo,
            &[&format!("../{name}"), "-b", &branch],
            &f.root.join(name),
        );
    }

    // An existing branch, and a detached HEAD.
    f.git_ok(&f.repo, &["branch", "existing"]);
    f.add_installed(
        &f.repo,
        &["../existing", "existing"],
        &f.root.join("existing"),
    );
    f.add_installed(
        &f.repo,
        &["--detach", "../detached"],
        &f.root.join("detached"),
    );

    // The desktop app keeps its worktrees inside the checkout, under
    // `.claude/worktrees/`. The settings file there is still ignored, and a
    // session in it is attributed to it, not to the main checkout.
    let nested = f.repo.join(".claude/worktrees/nested");
    f.add_installed(
        &f.repo,
        &[".claude/worktrees/nested", "-b", "nested"],
        &nested,
    );
    assert!(f.git(&nested, &["check-ignore", "-q", SETTINGS]).success);
    let nested = canonical(&nested);
    assert_eq!(f.capture_from(&nested), (7, 7));
}

#[test]
fn parallel_worktree_creation_gives_every_worktree_valid_hooks() {
    let f = Fixture::ready();
    let runs: Vec<(PathBuf, Run)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let f = &f;
                scope.spawn(move || {
                    let worktree = f.root.join(format!("par{index}"));
                    let run = f.git(
                        &f.repo,
                        &[
                            "worktree",
                            "add",
                            worktree.to_str().unwrap(),
                            "-b",
                            &format!("par{index}"),
                        ],
                    );
                    (worktree, run)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });
    for (worktree, run) in &runs {
        assert!(run.success, "{}: {}", worktree.display(), run.stderr);
        assert_eq!(
            run.cvc_lines(),
            vec![INSTALLED_LINE.to_owned()],
            "{}",
            run.all()
        );
        f.assert_hooks(worktree);
        // Temporaries have random names and are renamed away.
        let leftovers: Vec<_> = fs::read_dir(worktree.join(".claude"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".cvc-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
    let exclude = fs::read_to_string(f.repo.join(".git/info/exclude")).unwrap();
    assert_eq!(
        exclude
            .lines()
            .filter(|line| *line == "/.claude/settings.local.json")
            .count(),
        1,
        "{exclude}"
    );
    let harness: Vec<_> = fs::read_dir(f.repo.join(".git/cvc/harness"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(harness, vec![std::ffi::OsString::from("claude-code.path")]);
}

#[test]
fn a_binary_path_that_needs_shell_quoting_survives_to_a_real_hook_run() {
    let f = Fixture::ready();
    let custom = f.root.join("bin dir/it's cvc");
    fs::create_dir_all(custom.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_cvc"), &custom).unwrap();
    fs::set_permissions(&custom, fs::Permissions::from_mode(0o755)).unwrap();
    f.cvc_ok(
        &f.repo,
        &[
            "harness",
            "install",
            "claude-code",
            "--binary",
            custom.to_str().unwrap(),
        ],
    );

    let worktree = f.root.join("quoted");
    let run = f.git(
        &f.repo,
        &[
            "worktree",
            "add",
            worktree.to_str().unwrap(),
            "-b",
            "quoted",
        ],
    );
    assert!(run.success, "{}", run.stderr);
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    for event in EVENTS {
        assert_eq!(
            settings(&worktree)["hooks"][event][0]["hooks"][0]["command"],
            format!(
                "'{}/bin dir/it'\\''s cvc' ingest claude-code --hook",
                f.root.display()
            ),
            "{event}"
        );
    }
    // And the quoted command really runs: the session is captured there.
    assert_eq!(f.capture_from(&canonical(&worktree)), (7, 7));
}

#[test]
fn a_stale_cvc_on_path_is_never_started_by_the_hook() {
    // An older `cvc` (or the npm launcher resolving to one) that predates
    // `hook post-checkout` used to shadow the recorded binary because PATH won:
    // Git succeeded but nothing was installed and the stale program's error
    // reached the user. The hook now runs the binary named in the repository's
    // record and never looks `cvc` up, so the stale one is simply not involved.
    let f = Fixture::ready();
    let stale_dir = f.root.join("path-stale");
    fs::create_dir(&stale_dir).unwrap();
    symlink(real_tool("git").unwrap(), stale_dir.join("git")).unwrap();
    let stale_log = f.root.join("stale-was-started");
    executable(
        &stale_dir.join("cvc"),
        &format!(
            "#!/bin/sh\necho started >> \"{}\"\necho \"error: unrecognized subcommand 'post-checkout'\" >&2\nexit 2\n",
            stale_log.display()
        ),
    );
    let worktree = f.root.join("stale");
    let run = f.git_with_path(
        &f.repo,
        &stale_dir,
        &["worktree", "add", worktree.to_str().unwrap(), "-b", "stale"],
    );
    assert!(run.success, "{}", run.stderr);
    assert_eq!(run.cvc_lines(), [INSTALLED_LINE], "{}", run.all());
    assert!(!run.all().contains("unrecognized subcommand"));
    assert!(!stale_log.exists(), "the stale cvc on PATH was started");
    f.assert_hooks(&worktree);
}

#[test]
fn a_committed_settings_file_is_declined_and_left_exactly_as_committed() {
    // This used to merge CVC's entries into the committed file, foreign entries
    // kept. A settings file a commit carries is now never modified: CVC writes
    // a machine-specific binary path there, which would show up as a change to
    // a tracked file (and, committed, would leak the path to everyone).
    let f = Fixture::ready();
    let foreign = r#"{"permissions":{"allow":["Bash(ls:*)"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo foreign-stop"}]}],"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo foreign-pre"}]}]}}"#;
    f.branch_with("foreign", "100644", SETTINGS, foreign);
    let worktree = f.root.join("foreign");
    let run = f.git(
        &f.repo,
        &["worktree", "add", worktree.to_str().unwrap(), "foreign"],
    );
    assert!(run.success, "{}", run.stderr);
    assert_eq!(
        run.cvc_lines(),
        vec![format!(
            "CVC: Claude Code hooks not installed in this worktree: `.claude/settings.local.json` is tracked by Git in this checkout; CVC writes a machine-specific path there and will not modify a tracked file (untrack it with `git rm --cached .claude/settings.local.json`)"
        )],
        "{}",
        run.all()
    );
    assert!(!run.all().contains(INSTALLED_LINE));

    // Byte for byte as committed, and Git agrees nothing changed.
    assert_eq!(
        fs::read_to_string(worktree.join(SETTINGS)).unwrap(),
        foreign
    );
    assert_eq!(f.git_ok(&worktree, &["status", "--porcelain"]).stdout, "");

    // Running the hook again by hand says the same and still changes nothing.
    let again = f.cvc(
        &worktree,
        &[
            "hook",
            "post-checkout",
            "0000000000000000000000000000000000000000",
            "1111111111111111111111111111111111111111",
            "1",
        ],
    );
    assert!(again.success, "{}", again.all());
    assert!(
        again.stderr.contains("is tracked by Git"),
        "{}",
        again.all()
    );
    assert_eq!(
        fs::read_to_string(worktree.join(SETTINGS)).unwrap(),
        foreign
    );
}

#[test]
fn committed_claude_content_cvc_cannot_use_is_reported_and_never_fails_git() {
    let f = Fixture::ready();
    let outside_file = f.root.join("outside-file.json");
    fs::write(&outside_file, "{\"untouched\":true}\n").unwrap();
    let missing = f.root.join("does-not-exist");

    // (branch, mode, path, content, file that must keep exactly this content)
    let cases: Vec<(&str, &str, &str, String, Option<PathBuf>)> = vec![
        (
            "bad-json",
            "100644",
            SETTINGS,
            "{ not json".into(),
            Some(PathBuf::from(SETTINGS)),
        ),
        (
            "hooks-array",
            "100644",
            SETTINGS,
            r#"{"hooks":[1,2]}"#.into(),
            Some(PathBuf::from(SETTINGS)),
        ),
        (
            "claude-is-a-file",
            "100644",
            ".claude",
            "i am a file".into(),
            None,
        ),
        (
            "dangling-dir-link",
            "120000",
            ".claude",
            missing.to_str().unwrap().into(),
            None,
        ),
        (
            "settings-is-a-link",
            "120000",
            SETTINGS,
            outside_file.to_str().unwrap().into(),
            None,
        ),
    ];
    for (branch, mode, path, content, keeps) in cases {
        f.branch_with(branch, mode, path, &content);
        let worktree = f.root.join(branch);
        let run = f.git(
            &f.repo,
            &["worktree", "add", worktree.to_str().unwrap(), branch],
        );
        assert!(run.success, "{branch}: {}", run.stderr);
        assert_eq!(run.cvc_lines().len(), 1, "{branch}: {}", run.all());
        // A settings file the branch tracks is declined (whatever it holds, a
        // link included); a `.claude` that is not a directory is an error that
        // names the path.
        let expected_prefix = if path == SETTINGS {
            "CVC: Claude Code hooks not installed in this worktree: "
        } else {
            "CVC: post-checkout: "
        };
        assert!(
            run.cvc_lines()[0].starts_with(expected_prefix),
            "{branch}: {}",
            run.all()
        );
        assert!(
            !run.all().contains(INSTALLED_LINE),
            "{branch}: {}",
            run.all()
        );
        if let Some(file) = keeps {
            assert_eq!(
                fs::read_to_string(worktree.join(file)).unwrap(),
                content,
                "{branch}"
            );
        }
    }
    assert_eq!(
        fs::read_to_string(&outside_file).unwrap(),
        "{\"untouched\":true}\n"
    );
    assert!(
        !missing.exists(),
        "a dangling link target must not be created"
    );
}

#[test]
fn a_committed_claude_symlink_is_never_followed_out_of_the_worktree() {
    let f = Fixture::ready();
    let outside = f.root.join("outside");
    fs::create_dir(&outside).unwrap();
    f.branch_with("linked", "120000", ".claude", outside.to_str().unwrap());
    let worktree = f.root.join("linked");
    let run = f.git(
        &f.repo,
        &["worktree", "add", worktree.to_str().unwrap(), "linked"],
    );
    assert!(run.success, "{}", run.stderr);
    let written: Vec<_> = fs::read_dir(&outside)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        written.is_empty(),
        "the hook wrote {written:?} through the committed symlink into {}",
        outside.display()
    );
}

#[test]
fn a_non_shell_user_hook_is_not_broken_by_cvc_init() {
    let Some(python) = real_tool("python3") else {
        eprintln!("python3 is not available; skipping");
        return;
    };
    let f = Fixture::new();
    let path = f.root.join("path-git-python");
    fs::create_dir(&path).unwrap();
    symlink(real_tool("git").unwrap(), path.join("git")).unwrap();
    symlink(python, path.join("python3")).unwrap();
    fs::create_dir_all(f.repo.join(".git/hooks")).unwrap();
    executable(
        &f.repo.join(".git/hooks/post-checkout"),
        "#!/usr/bin/env python3\nprint('user python hook')\n",
    );
    assert!(
        f.git_with_path(&f.repo, &path, &["checkout", "-q", "-b", "before"])
            .success
    );

    f.cvc_ok(&f.repo, &["init"]);
    let after = f.git_with_path(&f.repo, &path, &["checkout", "-q", "-b", "after"]);
    assert!(
        after.success,
        "git checkout now exits non-zero because of the hook CVC appended to: {}",
        after.stderr
    );
}

/// QA D7, re-scoped. The shell half of the Git hook starts the program the
/// record names before the Rust half ever looks at the record, so the Rust
/// half's ownership and permission checks (group/world-writable, wrong owner)
/// govern what is written into settings, not which program the hook starts.
/// That is accepted and documented: the shell cannot check owner or mode
/// portably, and the trust boundary is write access to the common Git
/// directory, which also lets anyone edit `hooks/post-checkout` directly. So
/// this test no longer asserts anything about mode bits (the Rust refusal of a
/// group-writable record is covered in `harness_worktree_cli.rs`, with the
/// binary started and the settings not written).
///
/// What the shell half does check is a link: a record that is a symlink, or a
/// `harness` directory that is one, is refused there, with one line, before
/// anything is started.
#[test]
fn a_symlinked_record_or_directory_never_causes_its_binary_to_be_started() {
    let f = Fixture::ready();
    let marker = f.root.join("recorded-binary-was-started");
    let wrapper = f.root.join("bin/logging-cvc");
    fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
    executable(
        &wrapper,
        &format!(
            "#!/bin/sh\necho \"$@\" >> \"{}\"\nexec \"{}\" \"$@\"\n",
            marker.display(),
            env!("CARGO_BIN_EXE_cvc")
        ),
    );
    f.cvc_ok(
        &f.repo,
        &[
            "harness",
            "install",
            "claude-code",
            "--binary",
            wrapper.to_str().unwrap(),
        ],
    );
    let harness = f.repo.join(".git/cvc/harness");
    let record = harness.join("claude-code.path");
    let good = fs::read_to_string(&record).unwrap();
    let unusable = format!(
        "CVC: Claude Code hooks not installed in this worktree: the harness record {} is unusable; run `cvc harness install claude-code` here",
        record.display()
    );
    let add = |name: &str| {
        let worktree = f.root.join(name);
        let run = f.git(
            &f.repo,
            &["worktree", "add", worktree.to_str().unwrap(), "-b", name],
        );
        (worktree, run)
    };
    let assert_refused = |name: &str, worktree: &Path, run: &Run| {
        assert!(run.success, "{name}: {}", run.stderr);
        // One line says why, nothing was installed...
        assert_eq!(
            run.cvc_lines(),
            [unusable.as_str()],
            "{name}: {}",
            run.all()
        );
        assert!(!worktree.join(".claude").exists(), "{name}: {}", run.all());
        // ...and the program the link led to was never started.
        assert!(
            !marker.exists(),
            "{name}: the program named through a link was started: {}",
            fs::read_to_string(&marker).unwrap_or_default()
        );
    };

    // A record that is a symlink to a perfectly good record.
    let elsewhere = f.root.join("elsewhere.path");
    fs::write(&elsewhere, &good).unwrap();
    fs::remove_file(&record).unwrap();
    symlink(&elsewhere, &record).unwrap();
    let (worktree, run) = add("link-to-record");
    assert_refused("record link", &worktree, &run);

    // A dangling link counts as a record that exists, too.
    fs::remove_file(&record).unwrap();
    symlink(f.root.join("nowhere.path"), &record).unwrap();
    let (worktree, run) = add("dangling-record-link");
    assert_refused("dangling record link", &worktree, &run);

    // The `harness` directory itself a link, to a directory with a good record.
    fs::remove_file(&record).unwrap();
    let real = f.root.join("real-harness");
    fs::rename(&harness, &real).unwrap();
    symlink(&real, &harness).unwrap();
    let (worktree, run) = add("link-to-directory");
    assert_refused("directory link", &worktree, &run);

    // Put back as it was, the same record is used.
    fs::remove_file(&harness).unwrap();
    fs::rename(&real, &harness).unwrap();
    fs::write(&record, &good).unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    let (worktree, run) = add("restored");
    assert!(run.success, "{}", run.stderr);
    assert!(run.all().contains(INSTALLED_LINE), "{}", run.all());
    assert!(marker.exists(), "the recorded program should start now");
    assert!(worktree.join(SETTINGS).exists());
}
