use anyhow::{Context, Result};
use git2::Repository;
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
    // Check for core.hooksPath in git config
    let git_config = repo_root.join(".git").join("config");
    let mut hooks_dir = repo_root.join(".git").join("hooks");

    if git_config.exists() {
        if let Ok(repo) = Repository::open(repo_root) {
            if let Ok(config) = repo.config() {
                if let Ok(custom_path) = config.get_string("core.hooksPath") {
                    // Determine if custom_path is absolute or relative
                    let path_obj = Path::new(&custom_path);
                    if path_obj.is_absolute() {
                        hooks_dir = path_obj.to_path_buf();
                    } else {
                        hooks_dir = repo_root.join(path_obj);
                    }
                }
            }
        }
    }

    if !hooks_dir.exists() {
        fs::create_dir_all(&hooks_dir).context(format!(
            "Failed to create hooks directory at {:?}",
            hooks_dir
        ))?;
    }

    let hooks = vec![
        ("post-commit", "\n# CVC Hook\ncvc hook post-commit\n"),
        ("pre-push", "\n# CVC Hook\ncvc push\n"),
        ("post-merge", "\n# CVC Hook\ncvc pull\n"),
    ];

    let mut outcomes = Vec::with_capacity(hooks.len());

    for (hook_name, hook_cmd) in hooks {
        let hook_path = hooks_dir.join(hook_name);

        let action = if hook_path.exists() {
            let content = fs::read_to_string(&hook_path)?;
            // Check if hook is already present (simple string match)
            if !content.contains(hook_cmd.trim()) {
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

        outcomes.push(HookInstallOutcome {
            hook_name,
            hook_path,
            action,
        });
    }

    Ok(outcomes)
}
