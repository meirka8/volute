use anyhow::{Context, Result};
use git2::ErrorCode;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// What happened to a single hook file during `install`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAction {
    Created,
    Appended,
    AlreadyPresent,
}

/// Outcome of installing a single hook, for callers to report as they see fit.
///
/// `install` must not print or log directly: it is shared by cvc-cli (where
/// stdout is a normal terminal), cvc-lsp, and cvc-mcp (both stdio JSON-RPC
/// servers where writing anything but framed protocol messages to stdout
/// corrupts the stream for any client). Callers decide how/whether to surface
/// this.
#[derive(Debug, Clone)]
pub struct HookInstallOutcome {
    pub hook_name: &'static str,
    pub hook_path: PathBuf,
    pub action: HookAction,
}

pub fn install(repo_root: &Path) -> Result<Vec<HookInstallOutcome>> {
    // Discovery gives us both common storage and the actual active worktree.
    // The latter matters because Git resolves a relative hooksPath from it.
    let layout = crate::repository::RepositoryLayout::discover(repo_root)
        .context("Failed to discover git repository")?;
    install_layout(&layout)
}

/// Installs hooks for an already validated layout without rediscovering a
/// caller-controlled pathname.
pub fn install_layout(
    layout: &crate::repository::RepositoryLayout,
) -> Result<Vec<HookInstallOutcome>> {
    let worktree_root = layout.worktree_root()?;
    let repo = layout.repository();
    let mut hooks_dir = layout.common_git_dir().join("hooks");
    let config = repo.config().context("Failed to read Git configuration")?;
    match config.get_path("core.hooksPath") {
        Ok(path) => {
            // libgit2's pathname accessor performs Git's supported `~` and
            // `%(prefix)` expansion. Keep the resulting path byte-safe until
            // this explicit validation, then resolve relative paths from the
            // discovered worktree as Git does.
            if path.to_str().is_none() {
                anyhow::bail!("core.hooksPath is not valid UTF-8");
            }
            hooks_dir = if path.is_absolute() {
                path
            } else {
                worktree_root.join(path)
            };
        }
        Err(error) if error.code() == ErrorCode::NotFound => {}
        Err(error) => return Err(error).context("Failed to read core.hooksPath"),
    }

    if !hooks_dir.exists() {
        fs::create_dir_all(&hooks_dir).context(format!(
            "Failed to create hooks directory at {:?}",
            hooks_dir
        ))?;
    }

    let hooks = vec![
        ("post-commit", "\n# CVC Hook\ncvc hook post-commit || :\n"),
        ("pre-push", "\n# CVC Hook\ncvc hook pre-push \"$@\" || :\n"),
        (
            "post-merge",
            "\n# CVC Hook\ncvc hook post-merge \"$@\" || :\n",
        ),
        (
            "post-rewrite",
            "\n# CVC Hook\ncvc hook post-rewrite \"$@\" || :\n",
        ),
    ];

    let mut outcomes = Vec::with_capacity(hooks.len());

    for (hook_name, hook_cmd) in hooks {
        let hook_path = hooks_dir.join(hook_name);

        if fs::symlink_metadata(&hook_path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            anyhow::bail!("refusing to modify symlinked hook {:?}", hook_path);
        }

        let action = if hook_path.exists() {
            let content = fs::read_to_string(&hook_path)?;
            // Upgrade the old managed `cvc push` line in place.  Keeping it would
            // publish twice and can recurse through git's pre-push hook.
            let old = "# CVC Hook\ncvc push";
            let wanted = hook_cmd.trim();
            if content.contains(old) {
                fs::write(&hook_path, content.replace(old, wanted))?;
                HookAction::Appended
            } else if content.contains("# CVC Hook")
                && [
                    "cvc hook post-commit",
                    "cvc hook pre-push \"$@\"",
                    "cvc hook post-merge",
                    "cvc hook post-rewrite \"$@\"",
                ]
                .iter()
                .any(|line| content.lines().any(|existing| existing == *line))
            {
                let mut upgraded = content;
                for line in [
                    "cvc hook post-commit",
                    "cvc hook pre-push \"$@\"",
                    "cvc hook post-merge",
                    "cvc hook post-rewrite \"$@\"",
                ] {
                    upgraded = upgraded.replace(&format!("{line}\n"), &format!("{line} || :\n"));
                }
                // post-merge now forwards Git's squash flag too.
                upgraded = upgraded.replace(
                    "cvc hook post-merge || :",
                    "cvc hook post-merge \"$@\" || :",
                );
                fs::write(&hook_path, upgraded)?;
                HookAction::Appended
            } else if !content.contains(wanted) {
                let mut file = fs::OpenOptions::new().append(true).open(&hook_path)?;
                file.write_all(hook_cmd.as_bytes())?;
                HookAction::Appended
            } else {
                HookAction::AlreadyPresent
            }
        } else {
            let mut file = fs::File::create(&hook_path)?;
            file.write_all(b"#!/bin/sh\n")?;
            file.write_all(hook_cmd.as_bytes())?;
            HookAction::Created
        };

        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&hook_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms)?;
        }

        // Keep the long-standing public outcome cardinality stable; post-rewrite
        // is an internal companion hook but is installed with the same safety
        // guarantees as the reported hooks.
        if hook_name != "post-rewrite" {
            outcomes.push(HookInstallOutcome {
                hook_name,
                hook_path,
                action,
            });
        }
    }

    Ok(outcomes)
}
