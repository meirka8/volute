use cvc_core::claude_code::harness_record::write_record;
use cvc_core::hooks::{self, HookAction};
use cvc_core::repository::RepositoryLayout;
use git2::Repository;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tempfile::TempDir;

/// Helper: create a bare-minimum git repo for hook tests.
fn setup_repo() -> (TempDir, Repository) {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let repo = Repository::init(tmp.path()).expect("failed to init repo");
    (tmp, repo)
}

#[test]
fn test_install_creates_hooks() -> anyhow::Result<()> {
    let (tmp, _repo) = setup_repo();
    let outcomes = hooks::install(tmp.path())?;

    // install() must report outcomes to the caller rather than print/log
    // directly -- it's shared by cvc-lsp and cvc-mcp, both stdio JSON-RPC
    // servers where unframed stdout output corrupts the protocol stream.
    assert_eq!(outcomes.len(), 3);
    for outcome in &outcomes {
        assert_eq!(outcome.action, HookAction::Created);
    }

    let hooks_dir = tmp.path().join(".git").join("hooks");
    for name in &["post-commit", "pre-push", "post-merge", "post-rewrite"] {
        let hook = hooks_dir.join(name);
        assert!(hook.exists(), "hook {} should exist", name);

        let content = fs::read_to_string(&hook)?;
        assert!(
            content.starts_with("#!/bin/sh"),
            "{} should start with shebang",
            name
        );
        assert!(
            content.contains("# CVC Hook"),
            "{} should contain CVC marker",
            name
        );
        assert!(content.contains("|| :"), "{name} must be advisory");
    }

    // Verify specific commands
    let post_commit = fs::read_to_string(hooks_dir.join("post-commit"))?;
    assert!(post_commit.contains("cvc hook post-commit"));

    let pre_push = fs::read_to_string(hooks_dir.join("pre-push"))?;
    assert!(pre_push.contains("cvc hook pre-push"));

    let post_merge = fs::read_to_string(hooks_dir.join("post-merge"))?;
    assert!(post_merge.contains("cvc hook post-merge"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn installed_hooks_never_block_when_cvc_is_missing_or_nonzero() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let (tmp, _) = setup_repo();
    hooks::install(tmp.path())?;
    let hooks_dir = tmp.path().join(".git/hooks");
    for (name, args) in [
        ("post-commit", vec![]),
        ("pre-push", vec!["origin", "file:///remote"]),
        ("post-merge", vec!["1"]),
        ("post-rewrite", vec!["rebase"]),
    ] {
        let status = Command::new(hooks_dir.join(name))
            .args(&args)
            .env("PATH", "/definitely/missing")
            .stdin(Stdio::null())
            .status()?;
        assert!(status.success(), "missing cvc blocked {name}");
    }

    let bin = tmp.path().join("bin");
    fs::create_dir(&bin)?;
    let stub = bin.join("cvc");
    fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CVC_ARGS\"\nIFS= read -r line || :\nprintf '%s\\n' \"$line\" >> \"$CVC_STDIN\"\nexit 42\n",
    )?;
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755))?;
    let args_log = tmp.path().join("args.log");
    let stdin_log = tmp.path().join("stdin.log");
    for (name, args, input) in [
        (
            "pre-push",
            vec!["origin", "file:///remote"],
            b"push-input\n".as_slice(),
        ),
        ("post-merge", vec!["1"], b"".as_slice()),
        (
            "post-rewrite",
            vec!["rebase"],
            b"rewrite-input\n".as_slice(),
        ),
    ] {
        let mut child = Command::new(hooks_dir.join(name))
            .args(&args)
            .env("PATH", &bin)
            .env("CVC_ARGS", &args_log)
            .env("CVC_STDIN", &stdin_log)
            .stdin(Stdio::piped())
            .spawn()?;
        child.stdin.take().expect("piped stdin").write_all(input)?;
        assert!(child.wait()?.success(), "nonzero cvc blocked {name}");
    }
    let args = fs::read_to_string(args_log)?;
    assert!(args.contains("hook pre-push origin file:///remote"));
    assert!(args.contains("hook post-merge 1"));
    assert!(args.contains("hook post-rewrite rebase"));
    let stdin = fs::read_to_string(stdin_log)?;
    assert!(stdin.contains("push-input"));
    assert!(stdin.contains("rewrite-input"));
    Ok(())
}

#[test]
fn test_install_upgrades_legacy_pre_push_without_duplication() -> anyhow::Result<()> {
    let (tmp, _) = setup_repo();
    let hook = tmp.path().join(".git/hooks/pre-push");
    fs::write(&hook, "#!/bin/sh\n# CVC Hook\ncvc push\n")?;
    hooks::install(tmp.path())?;
    let content = fs::read_to_string(hook)?;
    assert!(content.contains("cvc hook pre-push"));
    assert!(!content.contains("\ncvc push\n"));
    assert_eq!(content.matches("# CVC Hook").count(), 1);
    Ok(())
}

#[test]
fn test_install_idempotency() -> anyhow::Result<()> {
    let (tmp, _repo) = setup_repo();

    // Install twice
    hooks::install(tmp.path())?;
    let outcomes = hooks::install(tmp.path())?;

    for outcome in &outcomes {
        assert_eq!(outcome.action, HookAction::AlreadyPresent);
    }

    let hooks_dir = tmp.path().join(".git").join("hooks");
    for name in &["post-commit", "pre-push", "post-merge"] {
        let content = fs::read_to_string(hooks_dir.join(name))?;
        let count = content.matches("# CVC Hook").count();
        assert_eq!(
            count, 1,
            "{} should contain exactly one CVC Hook marker, found {}",
            name, count
        );
    }

    Ok(())
}

#[test]
fn test_install_appends_to_existing() -> anyhow::Result<()> {
    let (tmp, _repo) = setup_repo();

    let hooks_dir = tmp.path().join(".git").join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    // Create a pre-existing post-commit hook with custom content
    let existing_content = "#!/bin/sh\necho \"existing hook\"\n";
    fs::write(hooks_dir.join("post-commit"), existing_content)?;

    let outcomes = hooks::install(tmp.path())?;

    let post_commit_outcome = outcomes
        .iter()
        .find(|o| o.hook_name == "post-commit")
        .expect("post-commit outcome should be reported");
    assert_eq!(post_commit_outcome.action, HookAction::Appended);

    let content = fs::read_to_string(hooks_dir.join("post-commit"))?;
    assert!(
        content.contains("echo \"existing hook\""),
        "original content should be preserved"
    );
    assert!(
        content.contains("cvc hook post-commit"),
        "CVC command should be appended"
    );

    Ok(())
}

#[test]
fn test_install_respects_hooks_path() -> anyhow::Result<()> {
    let (tmp, _repo) = setup_repo();

    // Set core.hooksPath to a custom directory
    let custom_hooks = tmp.path().join("my-hooks");
    {
        let repo = Repository::open(tmp.path())?;
        let mut config = repo.config()?;
        config.set_str("core.hooksPath", custom_hooks.to_str().unwrap())?;
    }

    hooks::install(tmp.path())?;

    // Hooks should be in the custom directory, not .git/hooks
    for name in &["post-commit", "pre-push", "post-merge"] {
        assert!(
            custom_hooks.join(name).exists(),
            "{} should exist in custom hooks dir",
            name
        );
        assert!(
            !tmp.path().join(".git").join("hooks").join(name).exists(),
            "{} should NOT exist in default .git/hooks",
            name
        );
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn test_install_permissions() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let (tmp, _repo) = setup_repo();
    hooks::install(tmp.path())?;

    let hooks_dir = tmp.path().join(".git").join("hooks");
    for name in &["post-commit", "pre-push", "post-merge"] {
        let perms = fs::metadata(hooks_dir.join(name))?.permissions();
        let mode = perms.mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "{} should have 0755 permissions, got {:o}",
            name, mode
        );
    }

    Ok(())
}

// ---- post-checkout (public issue 113) ----
//
// `install`/`install_layout` keep installing the four long-standing hooks and
// nothing else (cvc-mcp and cvc-lsp call `install_layout` on startup);
// `cvc init` adds post-checkout through `install_all`, and
// `cvc harness install` installs it alone through `install_post_checkout`.

/// Git's null object id (SHA-1 and SHA-256) and an arbitrary non-null one.
const NULL_OID: &str = "0000000000000000000000000000000000000000";
const NULL_OID_256: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const NEW_OID: &str = "1111111111111111111111111111111111111111";

fn layout_of(tmp: &TempDir) -> RepositoryLayout {
    let layout = RepositoryLayout::discover(tmp.path()).unwrap();
    fs::create_dir_all(layout.cvc_dir()).unwrap();
    layout
}

fn reported(outcomes: &[hooks::HookInstallOutcome]) -> Vec<&'static str> {
    outcomes.iter().map(|outcome| outcome.hook_name).collect()
}

#[test]
fn only_init_and_harness_install_add_post_checkout() -> anyhow::Result<()> {
    let (tmp, _) = setup_repo();
    let layout = layout_of(&tmp);
    let hooks_dir = tmp.path().join(".git/hooks");

    // What the MCP and LSP servers call: the four, never post-checkout.
    let outcomes = hooks::install_layout(&layout)?;
    assert_eq!(
        reported(&outcomes),
        ["post-commit", "pre-push", "post-merge"]
    );
    for name in ["post-commit", "pre-push", "post-merge", "post-rewrite"] {
        assert!(hooks_dir.join(name).exists(), "{name}");
    }
    assert!(!hooks_dir.join("post-checkout").exists());
    hooks::install(tmp.path())?;
    assert!(!hooks_dir.join("post-checkout").exists());

    // What `harness install` calls: post-checkout alone. A hook the user
    // removed on purpose stays removed.
    fs::remove_file(hooks_dir.join("post-commit"))?;
    let outcome = hooks::install_post_checkout(&layout)?;
    assert_eq!(outcome.hook_name, "post-checkout");
    assert_eq!(outcome.action, HookAction::Created);
    assert_eq!(outcome.hook_path, hooks_dir.join("post-checkout"));
    assert!(!hooks_dir.join("post-commit").exists());
    assert_eq!(
        hooks::install_post_checkout(&layout)?.action,
        HookAction::AlreadyPresent
    );

    // What `cvc init` calls: everything, post-checkout reported with the rest.
    let (tmp, _) = setup_repo();
    let layout = layout_of(&tmp);
    let outcomes = hooks::install_all(&layout)?;
    assert_eq!(
        reported(&outcomes),
        ["post-commit", "pre-push", "post-merge", "post-checkout"]
    );
    assert!(outcomes
        .iter()
        .all(|outcome| outcome.action == HookAction::Created));
    let outcomes = hooks::install_all(&layout)?;
    assert!(outcomes
        .iter()
        .all(|outcome| outcome.action == HookAction::AlreadyPresent));
    Ok(())
}

#[test]
fn post_checkout_hook_is_appended_to_a_users_hook_once() -> anyhow::Result<()> {
    let (tmp, _) = setup_repo();
    let layout = layout_of(&tmp);
    let hook = tmp.path().join(".git/hooks/post-checkout");

    // Created: a shebang and exactly one managed block.
    hooks::install_post_checkout(&layout)?;
    let created = fs::read_to_string(&hook)?;
    assert!(created.starts_with("#!/bin/sh\n"));
    assert!(created.contains(hooks::POST_CHECKOUT_HOOK.trim()));
    assert_eq!(
        created.matches("# CVC Hook: post-checkout begin").count(),
        1
    );
    assert_eq!(fs::read_to_string(&hook)?, created);

    // A user's own shell hook is kept and ours is appended once, then recognized.
    fs::write(&hook, "#!/bin/sh\necho user-hook\n")?;
    assert_eq!(
        hooks::install_post_checkout(&layout)?.action,
        HookAction::Appended
    );
    let appended = fs::read_to_string(&hook)?;
    assert!(appended.starts_with("#!/bin/sh\necho user-hook\n"));
    assert!(appended.contains(hooks::POST_CHECKOUT_HOOK.trim()));
    assert_eq!(
        hooks::install_post_checkout(&layout)?.action,
        HookAction::AlreadyPresent
    );
    assert_eq!(fs::read_to_string(&hook)?, appended);
    assert_eq!(
        appended.matches("# CVC Hook: post-checkout begin").count(),
        1
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn post_checkout_hook_is_valid_posix_sh_and_refuses_a_symlink() -> anyhow::Result<()> {
    let (tmp, _) = setup_repo();
    let layout = layout_of(&tmp);
    hooks::install_post_checkout(&layout)?;
    let hook = tmp.path().join(".git/hooks/post-checkout");
    for shell in ["sh", "dash", "bash", "ksh", "zsh"] {
        // `-n` parses without running anything. A shell that is not installed
        // here is skipped; `sh` itself always exists.
        let Ok(status) = Command::new(shell).arg("-n").arg(&hook).status() else {
            assert_ne!(shell, "sh");
            continue;
        };
        assert!(status.success(), "{shell} -n rejected the hook");
    }

    // Like every managed hook, a symlinked file is never written through.
    let elsewhere = tmp.path().join("elsewhere");
    fs::write(&elsewhere, "#!/bin/sh\n")?;
    fs::remove_file(&hook)?;
    std::os::unix::fs::symlink(&elsewhere, &hook)?;
    assert!(hooks::install_post_checkout(&layout).is_err());
    assert_eq!(fs::read_to_string(&elsewhere)?, "#!/bin/sh\n");
    Ok(())
}

/// Runs `command`, retrying while the kernel reports `ETXTBSY`. These tests
/// write scripts and run them at once from parallel threads; a child forked by a
/// sibling thread can briefly hold a just-written file's write descriptor, which
/// makes the exec fail with "Text file busy" for a moment.
#[cfg(unix)]
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

/// A repository whose managed `post-checkout` hook can be run directly, with a
/// record that names a logging stub standing in for `cvc`.
#[cfg(unix)]
struct HookRun {
    tmp: TempDir,
    hook: PathBuf,
    log: PathBuf,
}

#[cfg(unix)]
fn git_directory() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .find(|dir| dir.join("git").is_file())
        .expect("git on PATH")
}

#[cfg(unix)]
impl HookRun {
    fn new() -> anyhow::Result<Self> {
        let (tmp, _) = setup_repo();
        let layout = layout_of(&tmp);
        hooks::install_post_checkout(&layout)?;
        Ok(Self {
            hook: tmp.path().join(".git/hooks/post-checkout"),
            log: tmp.path().join("calls.log"),
            tmp,
        })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    /// An executable that appends `<label> <args>` to the log and exits `code`.
    fn stub(&self, at: &Path, label: &str, code: i32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        fs::write(
            at,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"{label} $*\" >> \"{}\"\nexit {code}\n",
                self.log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(at, fs::Permissions::from_mode(0o755)).unwrap();
        at.to_path_buf()
    }

    /// Records `binary` the way `harness install` does.
    fn record(&self, binary: &Path) {
        let layout = RepositoryLayout::discover(self.tmp.path()).unwrap();
        write_record(&layout, binary).unwrap();
    }

    /// Runs the hook as Git would, from the worktree root, with only `git` on
    /// `PATH` and no `HOME`.
    fn run(&self, args: &[&str]) -> std::process::Output {
        self.run_with(&self.hook, args, |_| {})
    }

    fn run_with(
        &self,
        program: &Path,
        args: &[&str],
        configure: impl FnOnce(&mut Command),
    ) -> std::process::Output {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(self.tmp.path())
            .env_clear()
            .env("PATH", git_directory())
            .stdin(Stdio::null());
        configure(&mut command);
        output_retrying(&mut command)
    }

    fn calls(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

#[cfg(unix)]
#[test]
fn post_checkout_hook_runs_the_recorded_binary_only_for_a_new_worktree() -> anyhow::Result<()> {
    let run = HookRun::new()?;
    // A binary that fails must not fail the hook.
    run.record(&run.stub(&run.path("recorded"), "recorded", 42));

    // Ordinary checkouts: a branch switch (real previous ref, flag 1), a file
    // checkout (flag 0), and malformed shapes. None may start the binary.
    for args in [
        vec![NEW_OID, NEW_OID, "1"],
        vec![NEW_OID, "2222222222222222222222222222222222222222", "1"],
        vec![NEW_OID, NEW_OID, "0"],
        vec![NULL_OID, NEW_OID, "0"],
        vec![NULL_OID, NEW_OID, ""],
        vec!["", NEW_OID, "1"],
        vec!["0000000000000000000000000000000000000001", NEW_OID, "1"],
        vec!["000000000000000000000000000000000000000x", NEW_OID, "1"],
        vec![NULL_OID, NEW_OID],
        vec![],
    ] {
        let output = run.run(&args);
        assert!(output.status.success(), "{args:?} must not fail");
        assert!(output.stderr.is_empty(), "{args:?}");
        assert_eq!(run.calls(), "", "{args:?} started the binary");
    }

    // A new worktree or clone: null previous ref (SHA-1 or SHA-256), flag 1.
    assert!(run.run(&[NULL_OID, NEW_OID, "1"]).status.success());
    assert!(run.run(&[NULL_OID_256, NEW_OID, "1"]).status.success());
    assert_eq!(
        run.calls(),
        format!(
            "recorded hook post-checkout {NULL_OID} {NEW_OID} 1\nrecorded hook post-checkout {NULL_OID_256} {NEW_OID} 1\n"
        )
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn post_checkout_hook_ignores_path_and_home() -> anyhow::Result<()> {
    let run = HookRun::new()?;
    run.record(&run.stub(&run.path("recorded"), "recorded", 0));

    // A stale `cvc` first on PATH (an old launcher without the subcommand, one
    // that would download on every run) and one in $HOME/.cvc/bin: both are
    // beside the point now.
    let stale = run.path("stale-path");
    run.stub(&stale.join("cvc"), "stale", 2);
    let home = run.path("home");
    run.stub(&home.join(".cvc/bin/cvc"), "home", 2);
    run.stub(&home.join(".cvc/bin/cvc.exe"), "home-exe", 2);
    let path = std::env::join_paths([stale, git_directory()])?;

    let args = [NULL_OID, NEW_OID, "1"];
    let output = run.run_with(&run.hook, &args, |command| {
        command.env("PATH", &path).env("HOME", &home);
    });
    assert!(output.status.success());
    // With no HOME at all, and with the shell told to treat unset as an error.
    assert!(run.run(&args).status.success());
    let output = run.run_with(
        Path::new("/bin/sh"),
        &["-u", run.hook.to_str().unwrap(), NULL_OID, NEW_OID, "1"],
        |_| {},
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run.run_with(
        Path::new("/bin/sh"),
        &["-u", run.hook.to_str().unwrap()],
        |_| {},
    );
    assert!(output.status.success());

    let line = format!("recorded hook post-checkout {NULL_OID} {NEW_OID} 1\n");
    assert_eq!(run.calls(), line.repeat(3));
    Ok(())
}

/// The script is POSIX `sh`, so every POSIX-family shell on the machine must
/// agree about it -- this runs it under each one that is installed (CI's `sh`
/// is dash, a developer's may be bash or zsh), with the shell's own "unset is
/// an error" option on.
#[cfg(unix)]
#[test]
fn post_checkout_hook_behaves_the_same_in_every_installed_posix_shell() -> anyhow::Result<()> {
    let run = HookRun::new()?;
    let recorded = run.stub(&run.path("recorded"), "recorded", 0);
    run.record(&recorded);
    let find = |name: &str| {
        std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    };
    let managed = hooks::POST_CHECKOUT_HOOK;
    let failing = run.path("failing-user-hook");
    fs::write(&failing, format!("#!/bin/sh\nfalse\n{managed}"))?;
    let passing = run.path("passing-user-hook");
    fs::write(&passing, format!("#!/bin/sh\ntrue\n{managed}"))?;
    let args = [NULL_OID, NEW_OID, "1"];

    let mut tried = Vec::new();
    for (shell, flags) in [
        ("sh", vec!["-u"]),
        ("dash", vec!["-u"]),
        ("bash", vec!["--posix", "-u"]),
        ("ksh", vec!["-u"]),
        ("zsh", vec!["--emulate", "sh", "-u"]),
    ] {
        let Some(program) = find(shell) else {
            assert_ne!(shell, "sh");
            continue;
        };
        tried.push(shell);
        let invoke = |script: &Path, extra: &[&str]| {
            let mut all: Vec<&str> = flags.clone();
            all.push(script.to_str().unwrap());
            all.extend_from_slice(extra);
            run.run_with(&program, &all, |_| {})
        };
        let before = run.calls().lines().count();

        // A new worktree starts the recorded binary; an ordinary checkout does not.
        let output = invoke(&run.hook, &args);
        assert!(output.status.success(), "{shell}");
        assert_eq!(run.calls().lines().count(), before + 1, "{shell}");
        let output = invoke(&run.hook, &[NEW_OID, NEW_OID, "1"]);
        assert!(output.status.success(), "{shell}");
        assert_eq!(run.calls().lines().count(), before + 1, "{shell}");

        // The status of the hook it was appended to survives, either way.
        assert_eq!(invoke(&failing, &args).status.code(), Some(1), "{shell}");
        assert_eq!(invoke(&passing, &args).status.code(), Some(0), "{shell}");
        assert_eq!(run.calls().lines().count(), before + 3, "{shell}");
    }
    assert!(tried.contains(&"sh"));

    // The one-line report for a binary that is gone reads the same everywhere.
    let gone = run.path("gone");
    run.record(&gone);
    for shell in tried {
        let program = find(shell).unwrap();
        let output = run.run_with(
            &program,
            &[run.hook.to_str().unwrap(), NULL_OID, NEW_OID, "1"],
            |_| {},
        );
        assert!(output.status.success(), "{shell}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!(
                "CVC: Claude Code hooks not installed in this worktree: {} does not exist; run `cvc harness install claude-code` here\n",
                gone.display()
            ),
            "{shell}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn post_checkout_hook_reports_an_unusable_recorded_binary_in_one_line() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let run = HookRun::new()?;
    let args = [NULL_OID, NEW_OID, "1"];
    let record = run.path(".git/cvc/harness/claude-code.path");

    // No record at all: the harness was never installed. Silent.
    let output = run.run(&args);
    assert!(output.status.success() && output.stderr.is_empty());

    // A record whose binary is gone, is not a file, or cannot be executed.
    let missing = run.path("deleted");
    let plain = run.path("plain");
    fs::write(&plain, "x")?;
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o644))?;
    let directory = run.path("a-directory");
    fs::create_dir(&directory)?;
    for (binary, reason) in [
        (&missing, "does not exist"),
        (&plain, "is not an executable file"),
        (&directory, "is not an executable file"),
    ] {
        run.record(binary);
        let output = run.run(&args);
        assert!(output.status.success(), "{reason}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!(
                "CVC: Claude Code hooks not installed in this worktree: {} {reason}; run `cvc harness install claude-code` here\n",
                binary.display()
            )
        );
        assert!(output.stdout.is_empty());
    }

    // A record that exists but cannot be used is never silent: one line, the
    // program is not started. (Run from the worktree root, Git hands the script
    // a relative common directory, so that is how the path reads.)
    let shown = ".git/cvc/harness/claude-code.path";
    let unusable = format!(
        "CVC: Claude Code hooks not installed in this worktree: the harness record {shown} is unusable; run `cvc harness install claude-code` here\n"
    );
    for content in [
        "",
        "\n",
        "  \n",
        "relative/cvc\n",
        "cvc\n",
        // Windows: a drive-relative path, and UNC / device paths.
        "C:cvc.exe\n",
        "C:\n",
        "\\\\server\\share\\cvc.exe\n",
        "\\\\?\\C:\\cvc.exe\n",
        "~/cvc\n",
    ] {
        fs::write(&record, content)?;
        let output = run.run(&args);
        assert!(output.status.success(), "{content:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            unusable,
            "{content:?}"
        );
        assert!(output.stdout.is_empty(), "{content:?}");
    }
    assert_eq!(run.calls(), "");

    // A record that cannot be read is unusable too (skipped where the mode is
    // ignored, as for root).
    fs::write(&record, "/opt/cvc\n")?;
    fs::set_permissions(&record, fs::Permissions::from_mode(0o000))?;
    if fs::read(&record).is_err() {
        let output = run.run(&args);
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stderr), unusable);
    }
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600))?;

    // A record that is a directory, or a `harness` directory that is a file.
    fs::remove_file(&record)?;
    fs::create_dir(&record)?;
    let output = run.run(&args);
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), unusable);
    fs::remove_dir(&record)?;
    let harness = run.path(".git/cvc/harness");
    fs::remove_dir_all(&harness)?;
    fs::write(&harness, "i am a file")?;
    let output = run.run(&args);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "CVC: Claude Code hooks not installed in this worktree: the harness record {shown} is unusable; run `cvc harness install claude-code` here\n"
        )
    );
    fs::remove_file(&harness)?;
    fs::create_dir(&harness)?;
    assert_eq!(run.calls(), "");

    // Windows-style absolute paths are recognized (and, here, simply do not
    // exist): backslash or slash after the drive letter, either case.
    for binary in ["C:\\Users\\me\\cvc.exe", "c:/Users/me/cvc.exe"] {
        fs::write(&record, format!("{binary}\n"))?;
        let output = run.run(&args);
        assert!(output.status.success(), "{binary}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!(
                "CVC: Claude Code hooks not installed in this worktree: {binary} does not exist; run `cvc harness install claude-code` here\n"
            ),
            "{binary}"
        );
    }

    // Echoed paths are cut to 200 characters.
    let long = format!("/{}", "a".repeat(500));
    fs::write(&record, format!("{long}\n"))?;
    let output = run.run(&args);
    let line = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        line.contains(&"a".repeat(199)) && !line.contains(&"a".repeat(200)),
        "{line}"
    );
    assert!(line.ends_with(" does not exist; run `cvc harness install claude-code` here\n"));

    // Outside any repository there is nothing to find: still a quiet success.
    let elsewhere = TempDir::new()?;
    let output = run.run_with(&run.hook, &args, |command| {
        command.current_dir(elsewhere.path());
    });
    assert!(output.status.success() && output.stderr.is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn post_checkout_hook_starts_nothing_for_a_symlinked_record_or_directory() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    let run = HookRun::new()?;
    let recorded = run.stub(&run.path("recorded"), "recorded", 0);
    let args = [NULL_OID, NEW_OID, "1"];
    let shown = ".git/cvc/harness/claude-code.path";
    let unusable = format!(
        "CVC: Claude Code hooks not installed in this worktree: the harness record {shown} is unusable; run `cvc harness install claude-code` here\n"
    );
    let harness = run.path(".git/cvc/harness");
    let record = harness.join("claude-code.path");

    // A record that is a link, to a perfectly good record: the program it
    // names is never started, and the refusal is one line.
    let elsewhere = run.path("elsewhere.path");
    fs::write(&elsewhere, format!("{}\n", recorded.display()))?;
    fs::create_dir_all(&harness)?;
    symlink(&elsewhere, &record)?;
    let output = run.run(&args);
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), unusable);
    assert_eq!(run.calls(), "");
    fs::remove_file(&record)?;

    // A dangling link is a record that exists too.
    symlink(run.path("nowhere.path"), &record)?;
    let output = run.run(&args);
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), unusable);
    fs::remove_file(&record)?;

    // A `harness` directory that is a link, to a directory with a good record.
    let real = run.path("real-harness");
    fs::create_dir(&real)?;
    fs::write(
        real.join("claude-code.path"),
        format!("{}\n", recorded.display()),
    )?;
    fs::remove_dir_all(&harness)?;
    symlink(&real, &harness)?;
    let output = run.run(&args);
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), unusable);
    assert_eq!(run.calls(), "");

    // The same record, in place, is used as before.
    fs::remove_file(&harness)?;
    fs::create_dir(&harness)?;
    fs::write(&record, format!("{}\n", recorded.display()))?;
    let output = run.run(&args);
    assert!(output.status.success() && output.stderr.is_empty());
    assert_eq!(
        run.calls(),
        format!("recorded hook post-checkout {NULL_OID} {NEW_OID} 1\n")
    );
    Ok(())
}

#[test]
fn an_older_marked_block_is_replaced_in_place_exactly_once() -> anyhow::Result<()> {
    let (tmp, _) = setup_repo();
    let layout = layout_of(&tmp);
    let hook = tmp.path().join(".git/hooks/post-checkout");
    let current = hooks::POST_CHECKOUT_HOOK.trim();
    assert!(current.starts_with("# CVC Hook: post-checkout begin (v"));
    assert!(current.ends_with("# CVC Hook: post-checkout end"));

    let above = "#!/bin/sh\necho above\nfalse\n\n";
    let below = "echo below\n";
    for old in ["v0", "v2", "v1-experimental"] {
        let older = format!(
            "# CVC Hook: post-checkout begin ({old})\ncvc_old_logic\nmore old logic\n# CVC Hook: post-checkout end\n"
        );
        fs::write(&hook, format!("{above}{older}{below}"))?;
        let outcome = hooks::install_post_checkout(&layout)?;
        assert_eq!(outcome.action, HookAction::Updated, "{old}");
        let expected = format!("{above}{current}\n{below}");
        assert_eq!(fs::read_to_string(&hook)?, expected, "{old}");
        assert_eq!(
            expected.matches("# CVC Hook: post-checkout begin").count(),
            1
        );
        assert!(!expected.contains("cvc_old_logic"));
        // Installing again is a no-op, and the user's content never moved.
        let again = hooks::install_post_checkout(&layout)?;
        assert_eq!(again.action, HookAction::AlreadyPresent, "{old}");
        assert_eq!(fs::read_to_string(&hook)?, expected, "{old}");
    }

    // The current block, with user content on both sides, is left alone.
    let current_file = format!("{above}{current}\n{below}");
    fs::write(&hook, &current_file)?;
    assert_eq!(
        hooks::install_post_checkout(&layout)?.action,
        HookAction::AlreadyPresent
    );
    assert_eq!(fs::read_to_string(&hook)?, current_file);

    // A stray second copy (two older blocks) collapses into one current block.
    let older = "# CVC Hook: post-checkout begin (v0)\nold\n# CVC Hook: post-checkout end\n";
    fs::write(&hook, format!("{above}{older}middle\n{older}{below}"))?;
    assert_eq!(
        hooks::install_post_checkout(&layout)?.action,
        HookAction::Updated
    );
    assert_eq!(
        fs::read_to_string(&hook)?,
        format!("{above}{current}\nmiddle\n{below}")
    );

    // CRLF line endings around the markers are recognized as well.
    fs::write(
        &hook,
        "#!/bin/sh\r\n# CVC Hook: post-checkout begin (v0)\r\nold\r\n# CVC Hook: post-checkout end\r\necho below\r\n",
    )?;
    assert_eq!(
        hooks::install_post_checkout(&layout)?.action,
        HookAction::Updated
    );
    let replaced = fs::read_to_string(&hook)?;
    assert!(replaced.starts_with(&format!("#!/bin/sh\r\n{current}\n")));
    assert!(replaced.ends_with("echo below\r\n"));

    // A begin marker with no end marker is not guessed at: error, file kept.
    let truncated = format!("{above}# CVC Hook: post-checkout begin (v0)\nhalf a block\n{below}");
    fs::write(&hook, &truncated)?;
    let error = hooks::install_post_checkout(&layout).unwrap_err();
    assert!(
        format!("{error:#}").contains("without its end marker"),
        "{error:#}"
    );
    assert_eq!(fs::read_to_string(&hook)?, truncated);

    // The upgrade is reported by `install_all` the same way, mode intact.
    fs::write(
        &hook,
        "#!/bin/sh\n# CVC Hook: post-checkout begin (v0)\nold\n# CVC Hook: post-checkout end\n",
    )?;
    let outcomes = hooks::install_all(&layout)?;
    assert_eq!(
        outcomes
            .iter()
            .find(|outcome| outcome.hook_name == "post-checkout")
            .unwrap()
            .action,
        HookAction::Updated
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&hook)?.permissions().mode() & 0o777, 0o755);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn post_checkout_hook_keeps_the_status_of_the_hook_it_was_appended_to() -> anyhow::Result<()> {
    let run = HookRun::new()?;
    run.record(&run.stub(&run.path("recorded"), "recorded", 0));
    let new_worktree = [NULL_OID, NEW_OID, "1"];
    let branch_switch = [NEW_OID, NEW_OID, "1"];
    let managed = hooks::POST_CHECKOUT_HOOK;
    let code = |output: std::process::Output| output.status.code();

    // (what the user's hook ended with, the status the whole hook must have)
    for (user, expected) in [
        ("#!/bin/sh\ntrue\n", Some(0)),
        ("#!/bin/sh\nfalse\n", Some(1)),
        ("#!/bin/sh\nset -eu\ntrue\n", Some(0)),
        ("#!/bin/sh\nset -u\n(exit 7)\n", Some(7)),
        ("#!/bin/bash\nset -u\nfalse\n", Some(1)),
        ("#!/usr/bin/env sh\n[ -d /nonexistent ]\n", Some(1)),
    ] {
        fs::write(&run.hook, format!("{user}{managed}"))?;
        // Our block runs, or does not, and the status is the user's either way.
        let before = run.calls().lines().count();
        assert_eq!(
            code(run.run(&new_worktree)),
            expected,
            "{user:?} (new worktree)"
        );
        assert_eq!(
            run.calls().lines().count(),
            before + 1,
            "{user:?}: the managed block still ran"
        );
        assert_eq!(code(run.run(&branch_switch)), expected, "{user:?} (switch)");
        assert_eq!(
            code(run.run_with(&run.hook, &new_worktree, |command| {
                command.env("HOME", "/nonexistent");
            })),
            expected,
            "{user:?} (HOME set)"
        );
    }

    // Our part alone, with a failing binary, still succeeds.
    run.record(&run.stub(&run.path("failing"), "failing", 42));
    fs::write(&run.hook, format!("#!/bin/sh\n{managed}"))?;
    assert_eq!(code(run.run(&new_worktree)), Some(0));
    Ok(())
}

/// Non-shell hooks are left byte for byte as they were, for each managed hook;
/// shell-family hooks (and ones without a shebang) are appended to.
#[test]
fn a_hook_that_is_not_a_shell_script_is_never_modified() -> anyhow::Result<()> {
    let not_shell: [(&str, &[u8]); 8] = [
        ("python via env", b"#!/usr/bin/env python3\nprint('hi')\n"),
        ("perl", b"#!/usr/bin/perl\nprint \"hi\\n\";\n"),
        (
            "node via env -S",
            b"#!/usr/bin/env -S node --flag\nconsole.log(1)\n",
        ),
        ("fish", b"#!/usr/bin/fish\necho hi\n"),
        ("ruby with CRLF", b"#!/usr/bin/ruby\r\nputs 1\r\n"),
        ("env with no interpreter", b"#!/usr/bin/env\n"),
        ("bare shebang", b"#!\nwhatever\n"),
        (
            "compiled program",
            &[0x7f, b'E', b'L', b'F', 0xff, 0xfe, 0x00, 0x01],
        ),
    ];
    for hook_name in [
        "post-commit",
        "pre-push",
        "post-merge",
        "post-rewrite",
        "post-checkout",
    ] {
        for (what, bytes) in not_shell {
            let (tmp, _) = setup_repo();
            let layout = layout_of(&tmp);
            let hooks_dir = tmp.path().join(".git/hooks");
            fs::create_dir_all(&hooks_dir)?;
            let hook = hooks_dir.join(hook_name);
            fs::write(&hook, bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&hook, fs::Permissions::from_mode(0o700))?;
            }

            let outcomes = hooks::install_all(&layout)?;
            let skipped: Vec<_> = outcomes
                .iter()
                .filter(|outcome| outcome.action == HookAction::SkippedNotShell)
                .map(|outcome| outcome.hook_name)
                .collect();
            assert_eq!(skipped, [hook_name], "{hook_name}: {what}");
            assert_eq!(
                outcomes
                    .iter()
                    .find(|outcome| outcome.hook_name == hook_name)
                    .unwrap()
                    .hook_path,
                hook
            );
            assert_eq!(fs::read(&hook)?, bytes, "{hook_name}: {what}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&hook)?.permissions().mode() & 0o777,
                    0o700,
                    "{hook_name}: {what}: mode untouched"
                );
            }
            // Every other managed hook is installed regardless.
            for other in [
                "post-commit",
                "pre-push",
                "post-merge",
                "post-rewrite",
                "post-checkout",
            ] {
                if other != hook_name {
                    assert!(hooks_dir.join(other).exists(), "{other}");
                }
            }
            // Re-running changes nothing and still reports it.
            let again = hooks::install_all(&layout)?;
            assert!(again.iter().any(|outcome| outcome.hook_name == hook_name
                && outcome.action == HookAction::SkippedNotShell));
            assert_eq!(fs::read(&hook)?, bytes);
        }
    }
    // Every managed hook has a line to call by hand and a purpose to state.
    for hook_name in [
        "post-commit",
        "pre-push",
        "post-merge",
        "post-rewrite",
        "post-checkout",
    ] {
        let (line, purpose) = hooks::manual_invocation(hook_name).unwrap();
        assert!(line.starts_with(&format!("cvc hook {hook_name}")), "{line}");
        assert!(!purpose.is_empty());
    }
    Ok(())
}

#[test]
fn hooks_run_by_a_posix_shell_get_the_managed_block() -> anyhow::Result<()> {
    for (what, head) in [
        ("sh", "#!/bin/sh\necho user\n"),
        ("sh with a flag", "#!/bin/sh -e\necho user\n"),
        ("bash", "#!/bin/bash\necho user\n"),
        ("dash", "#!/usr/bin/dash\necho user\n"),
        ("zsh", "#!/bin/zsh\necho user\n"),
        ("ksh", "#!/usr/bin/ksh\necho user\n"),
        ("env sh", "#!/usr/bin/env sh\necho user\n"),
        ("env bash", "#!/usr/bin/env bash\necho user\n"),
        ("env -S bash", "#!/usr/bin/env -S bash -e\necho user\n"),
        (
            "env with assignment",
            "#!/usr/bin/env FOO=1 bash\necho user\n",
        ),
        ("CRLF shebang", "#!/bin/sh\r\necho user\n"),
        ("no shebang", "echo user\n"),
        ("empty", ""),
    ] {
        let (tmp, _) = setup_repo();
        let layout = layout_of(&tmp);
        let hooks_dir = tmp.path().join(".git/hooks");
        fs::create_dir_all(&hooks_dir)?;
        let hook = hooks_dir.join("post-checkout");
        fs::write(&hook, head)?;
        let outcome = hooks::install_post_checkout(&layout)?;
        assert_eq!(outcome.action, HookAction::Appended, "{what}");
        let content = fs::read_to_string(&hook)?;
        assert!(content.starts_with(head), "{what}");
        assert!(content.contains(hooks::POST_CHECKOUT_HOOK.trim()), "{what}");
    }
    Ok(())
}
