//! Checkout-local Claude Code hook installation.
//!
//! Hooks are written to `.claude/settings.local.json` in the active worktree,
//! never to the shared `.claude/settings.json`: the entry carries the absolute
//! path of the `cvc` binary that Claude Code's hook environment must run, and a
//! machine-specific path must not be committable. Because that file is per
//! checkout, a fresh linked worktree needs its own `cvc harness install`.
//!
//! Like the Git hooks installer, this module never prints: it returns what it
//! did so callers can report it in whichever way suits their stdout.
use crate::repository::RepositoryLayout;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Settings file relative to the worktree root.
pub const SETTINGS_RELATIVE_PATH: &str = ".claude/settings.local.json";
/// Lifecycle events that trigger ingestion. `PostToolUse` keeps ingestion
/// near-real-time so a mid-session commit can still claim earlier reasoning;
/// `Stop` and `SessionEnd` close the final response of a turn or session.
pub const HOOK_EVENTS: [&str; 3] = ["PostToolUse", "Stop", "SessionEnd"];
/// Explicit per-hook timeout: `SessionEnd` hooks otherwise share a budget of
/// under two seconds, which a contended database could exceed.
pub const HOOK_TIMEOUT_SECS: u64 = 30;
/// How installed entries are recognized regardless of the binary path.
const HOOK_SIGNATURE: &str = "ingest claude-code --hook";
const EXCLUDE_LINE: &str = "/.claude/settings.local.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallAction {
    Created,
    Updated,
    AlreadyPresent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcludeAction {
    AlreadyIgnored,
    AddedToInfoExclude,
}

#[derive(Debug, Clone)]
pub struct InstallOutcome {
    pub settings_path: PathBuf,
    pub command: String,
    pub action: InstallAction,
    pub exclude: ExcludeAction,
}

#[derive(Debug, Clone)]
pub struct UninstallOutcome {
    pub settings_path: PathBuf,
    pub removed: usize,
    pub deleted_file: bool,
}

/// The shell command a hook entry runs. Shell-string form is used rather than
/// an exec-form entry so that every Claude Code version runs the same thing;
/// a client that ignored an `args` list would otherwise run a bare `cvc`.
pub fn hook_command(binary: &Path) -> Result<String> {
    if !binary.is_absolute() {
        bail!("hook binary path must be absolute: {}", binary.display());
    }
    let metadata = fs::metadata(binary)
        .with_context(|| format!("hook binary {} is not accessible", binary.display()))?;
    if !metadata.is_file() {
        bail!("hook binary {} is not a file", binary.display());
    }
    let text = binary
        .to_str()
        .ok_or_else(|| anyhow!("hook binary path is not valid UTF-8"))?;
    Ok(format!("{} {HOOK_SIGNATURE}", shell_quote(text)))
}

fn shell_quote(text: &str) -> String {
    if cfg!(windows) {
        format!("\"{text}\"")
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

pub fn settings_path(layout: &RepositoryLayout) -> Result<PathBuf> {
    Ok(layout.worktree_root()?.join(SETTINGS_RELATIVE_PATH))
}

/// Installs or refreshes the hook entries. Existing unrelated settings and
/// hooks are preserved byte-for-byte in meaning; only entries carrying the CVC
/// signature are touched. Idempotent.
pub fn install(layout: &RepositoryLayout, binary: &Path) -> Result<InstallOutcome> {
    let command = hook_command(binary)?;
    let settings_path = settings_path(layout)?;
    let mut settings = read_settings(&settings_path)?;
    let hooks = settings
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks.as_object_mut().ok_or_else(|| {
        anyhow!(
            "refusing to modify {}: \"hooks\" is not an object",
            settings_path.display()
        )
    })?;

    let mut existed = false;
    let mut changed = false;
    for event in HOOK_EVENTS {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()));
        let groups = groups.as_array_mut().ok_or_else(|| {
            anyhow!(
                "refusing to modify {}: hooks.{event} is not an array",
                settings_path.display()
            )
        })?;
        let mut found = false;
        for group in groups.iter_mut() {
            let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            for entry in entries.iter_mut() {
                let Some(existing) = entry.get("command").and_then(Value::as_str) else {
                    continue;
                };
                if !existing.contains(HOOK_SIGNATURE) {
                    continue;
                }
                found = true;
                existed = true;
                if existing != command {
                    entry["command"] = Value::String(command.clone());
                    changed = true;
                }
            }
        }
        if !found {
            groups.push(json!({
                "hooks": [{
                    "type": "command",
                    "command": command,
                    "timeout": HOOK_TIMEOUT_SECS,
                }]
            }));
            changed = true;
        }
    }
    let action = if !existed {
        InstallAction::Created
    } else if changed {
        InstallAction::Updated
    } else {
        InstallAction::AlreadyPresent
    };
    if changed {
        write_settings(&settings_path, &settings)?;
    }
    let exclude = ensure_excluded(layout)?;
    Ok(InstallOutcome {
        settings_path,
        command,
        action,
        exclude,
    })
}

/// Removes every entry carrying the CVC signature and nothing else. An empty
/// settings file left behind is deleted; any other content is kept.
pub fn uninstall(layout: &RepositoryLayout) -> Result<UninstallOutcome> {
    let settings_path = settings_path(layout)?;
    match fs::symlink_metadata(&settings_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UninstallOutcome {
                settings_path,
                removed: 0,
                deleted_file: false,
            })
        }
        Err(error) => return Err(error.into()),
    }
    let mut settings = read_settings(&settings_path)?;
    let mut removed = 0usize;
    if let Some(Value::Object(hooks)) = settings.get_mut("hooks") {
        let events: Vec<String> = hooks.keys().cloned().collect();
        for event in events {
            if let Some(Value::Array(groups)) = hooks.get_mut(&event) {
                for group in groups.iter_mut() {
                    if let Some(Value::Array(entries)) = group.get_mut("hooks") {
                        let before = entries.len();
                        entries.retain(|entry| {
                            !entry
                                .get("command")
                                .and_then(Value::as_str)
                                .is_some_and(|command| command.contains(HOOK_SIGNATURE))
                        });
                        removed += before - entries.len();
                    }
                }
                groups.retain(|group| {
                    group
                        .get("hooks")
                        .and_then(Value::as_array)
                        .is_none_or(|entries| !entries.is_empty())
                });
                if groups.is_empty() {
                    hooks.remove(&event);
                }
            }
        }
        if hooks.is_empty() {
            settings.remove("hooks");
        }
    }
    if removed == 0 {
        return Ok(UninstallOutcome {
            settings_path,
            removed,
            deleted_file: false,
        });
    }
    let deleted_file = settings.is_empty();
    if deleted_file {
        fs::remove_file(&settings_path)?;
    } else {
        write_settings(&settings_path, &settings)?;
    }
    Ok(UninstallOutcome {
        settings_path,
        removed,
        deleted_file,
    })
}

fn read_settings(path: &Path) -> Result<Map<String, Value>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "refusing to modify symlinked settings file {}",
            path.display()
        );
    }
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => bail!(
            "refusing to modify {}: top level is not a JSON object",
            path.display()
        ),
        Err(error) => bail!(
            "refusing to modify {}: not valid JSON ({error})",
            path.display()
        ),
    }
}

fn write_settings(path: &Path, settings: &Map<String, Value>) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("settings path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut rendered = serde_json::to_string_pretty(&Value::Object(settings.clone()))?;
    rendered.push('\n');
    // Write-then-rename so a crash never leaves Claude Code a half-written
    // settings file to reject.
    let temporary = parent.join(".settings.local.json.cvc-tmp");
    {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(rendered.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

/// Keeps the machine-specific settings file out of commits. Claude Code adds
/// a global exclude only when it creates the file itself, so an installer that
/// creates it must do the equivalent. The repository-local `info/exclude` is
/// shared by every linked worktree and is not a tracked file.
fn ensure_excluded(layout: &RepositoryLayout) -> Result<ExcludeAction> {
    if layout
        .repository()
        .is_path_ignored(Path::new(SETTINGS_RELATIVE_PATH))
        .unwrap_or(false)
    {
        return Ok(ExcludeAction::AlreadyIgnored);
    }
    let info_dir = layout.common_git_dir().join("info");
    fs::create_dir_all(&info_dir)?;
    let exclude_path = info_dir.join("exclude");
    if fs::symlink_metadata(&exclude_path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        bail!("refusing to modify symlinked {}", exclude_path.display());
    }
    let existing = match fs::read_to_string(&exclude_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    if existing.lines().any(|line| line.trim() == EXCLUDE_LINE) {
        return Ok(ExcludeAction::AlreadyIgnored);
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.write_all(
        format!("# CVC: machine-specific Claude Code hook path, never committed\n{EXCLUDE_LINE}\n")
            .as_bytes(),
    )?;
    Ok(ExcludeAction::AddedToInfoExclude)
}
