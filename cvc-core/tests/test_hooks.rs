use cvc_core::hooks::{self, HookAction};
use git2::Repository;
use std::fs;
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
    for name in &["post-commit", "pre-push", "post-merge"] {
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
    }

    // Verify specific commands
    let post_commit = fs::read_to_string(hooks_dir.join("post-commit"))?;
    assert!(post_commit.contains("cvc hook post-commit"));

    let pre_push = fs::read_to_string(hooks_dir.join("pre-push"))?;
    assert!(pre_push.contains("cvc push"));

    let post_merge = fs::read_to_string(hooks_dir.join("post-merge"))?;
    assert!(post_merge.contains("cvc pull"));

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
