//! `cvc harness`: lifecycle-hook capture for agent harnesses.
use anyhow::{bail, Context, Result};
use cvc_core::claude_code::settings::{self, ExcludeAction, InstallAction};
use cvc_core::privacy;
use cvc_core::repository::RepositoryLayout;
use std::env;
use std::path::PathBuf;

pub async fn install_claude_code(binary: Option<PathBuf>) -> Result<()> {
    let layout = discover_initialized()?;
    // Hook-driven ingestion is background observation of an agent session, so
    // it needs the same repository-local acknowledgement as passive VS Code
    // capture, and installing hooks that would refuse to run is not useful.
    require_capture_acknowledged(&layout)?;
    let binary = match binary {
        Some(binary) => binary,
        None => env::current_exe()
            .context("cannot resolve the running cvc executable; pass --binary <absolute path>")?,
    };
    let outcome = settings::install(&layout, &binary)?;
    let action = match outcome.action {
        InstallAction::Created => "installed",
        InstallAction::Updated => "updated",
        InstallAction::AlreadyPresent => "already present",
    };
    println!(
        "Claude Code hooks {action} in {}",
        outcome.settings_path.display()
    );
    println!(
        "  events: {}  command: {}",
        settings::HOOK_EVENTS.join(", "),
        outcome.command
    );
    match outcome.exclude {
        ExcludeAction::AddedToInfoExclude => println!(
            "  added {} to the repository's info/exclude so the machine-specific path is never committed",
            settings::SETTINGS_RELATIVE_PATH
        ),
        ExcludeAction::AlreadyIgnored => {}
    }
    println!("This install is per checkout: run it again in each linked worktree you use.");
    Ok(())
}

pub async fn uninstall_claude_code() -> Result<()> {
    let layout = discover_initialized()?;
    let outcome = settings::uninstall(&layout)?;
    if outcome.removed == 0 {
        println!(
            "No CVC hook entries found in {}",
            outcome.settings_path.display()
        );
    } else if outcome.deleted_file {
        println!(
            "Removed {} CVC hook entr{} and the now-empty {}",
            outcome.removed,
            if outcome.removed == 1 { "y" } else { "ies" },
            outcome.settings_path.display()
        );
    } else {
        println!(
            "Removed {} CVC hook entr{} from {}",
            outcome.removed,
            if outcome.removed == 1 { "y" } else { "ies" },
            outcome.settings_path.display()
        );
    }
    Ok(())
}

pub(crate) fn discover_initialized() -> Result<RepositoryLayout> {
    let current_dir = env::current_dir()?;
    discover_initialized_at(&current_dir)
}

pub(crate) fn discover_initialized_at(path: &std::path::Path) -> Result<RepositoryLayout> {
    let layout = RepositoryLayout::discover(path).context("Failed to discover Git repository")?;
    layout.worktree_root()?;
    if !layout.cvc_dir().is_dir() {
        bail!("CVC is not initialized in this repository; run `cvc init` first");
    }
    Ok(layout)
}

pub(crate) fn require_capture_acknowledged(layout: &RepositoryLayout) -> Result<()> {
    if !privacy::capture_acknowledged(layout.repository())? {
        bail!(
            "consent-required: Claude Code transcript capture is passive collection and needs the repository-local capture acknowledgement; run `cvc privacy acknowledge-capture` first (or `cvc harness uninstall claude-code` to stop the hooks)"
        );
    }
    Ok(())
}
