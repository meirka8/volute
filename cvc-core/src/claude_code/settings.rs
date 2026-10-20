//! Checkout-local Claude Code hook installation.
//!
//! Hooks are written to `.claude/settings.local.json` in the active worktree,
//! never to the shared `.claude/settings.json`: the entry carries the absolute
//! path of the `cvc` binary that Claude Code's hook environment must run, and a
//! machine-specific path must not be committable. Because that file is per
//! checkout, a fresh linked worktree starts without it; the `post-checkout`
//! propagation (see [`super::harness_record`]) installs it there.
//!
//! A worktree's content is whatever its branch committed, so nothing here
//! trusts it: `.claude` must be a real directory inside the worktree (never a
//! symlink or a file), the settings file is never written through a link, and
//! a settings file that Git tracks is left alone.
//!
//! Like the Git hooks installer, this module never prints: it returns what it
//! did so callers can report it in whichever way suits their stdout.
use crate::repository::RepositoryLayout;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Settings file relative to the worktree root.
pub const SETTINGS_RELATIVE_PATH: &str = ".claude/settings.local.json";
/// The directory the settings file lives in, relative to the worktree root.
const CLAUDE_DIR: &str = ".claude";
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
/// A settings file is a few kilobytes; this only bounds what is read.
const MAX_SETTINGS_BYTES: u64 = 4 * 1024 * 1024;

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

/// What another worktree's settings look like from outside: whether it carries
/// CVC's entries, or why that could not be told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeState {
    Installed,
    NotInstalled,
    /// `.claude` or the settings file could not be read safely (a symlink, an
    /// unreadable or malformed file); the reason is a short sentence.
    Unreadable(String),
}

/// Another worktree of the same repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeHooks {
    pub root: PathBuf,
    pub state: WorktreeState,
}

impl WorktreeHooks {
    pub fn installed(&self) -> bool {
        self.state == WorktreeState::Installed
    }
}

/// The other worktrees of a repository, as far as they can be found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSurvey {
    pub worktrees: Vec<WorktreeHooks>,
    /// False when the active worktree is a linked one and the main worktree
    /// could not be located (its Git directory is not its own `.git`), so it
    /// is missing from `worktrees` although it exists.
    pub main_located: bool,
}

/// Returned (inside an [`anyhow::Error`]) by [`install`] when Git tracks the
/// settings file. CVC writes a machine-specific path into it and will not edit
/// a file a commit carries.
#[derive(Debug, thiserror::Error)]
#[error("`.claude/settings.local.json` is tracked by Git in this checkout; CVC writes a machine-specific path there and will not modify a tracked file (untrack it with `git rm --cached .claude/settings.local.json`)")]
pub struct TrackedSettings;

/// Returned (inside an [`anyhow::Error`]) by [`create`] when the settings file
/// is already there, for whatever reason: a new worktree is supposed to have
/// none, so anything found was put there by something else and is not merged
/// into.
#[derive(Debug, thiserror::Error)]
#[error("`.claude/settings.local.json` already exists in this worktree; CVC only creates it in a new worktree (run `cvc harness install claude-code` here to add the hooks)")]
pub struct SettingsExist;

/// Why `binary` cannot be the program a hook runs, if it cannot. The text
/// starts with the path, so callers can put their own words before it.
pub fn binary_problem(binary: &Path) -> Option<String> {
    if !binary.is_absolute() {
        return Some(format!("{} is not an absolute path", binary.display()));
    }
    match fs::metadata(binary) {
        Ok(metadata) if !metadata.is_file() => Some(format!("{} is not a file", binary.display())),
        #[cfg(unix)]
        Ok(metadata) if is_not_executable(&metadata) => {
            Some(format!("{} is not executable", binary.display()))
        }
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Some(format!("{} does not exist", binary.display()))
        }
        Err(_) => Some(format!("{} is not accessible", binary.display())),
    }
}

#[cfg(unix)]
fn is_not_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 == 0
}

/// The shell command a hook entry runs. Shell-string form is used rather than
/// an exec-form entry so that every Claude Code version runs the same thing;
/// a client that ignored an `args` list would otherwise run a bare `cvc`.
pub fn hook_command(binary: &Path) -> Result<String> {
    if let Some(problem) = binary_problem(binary) {
        bail!("hook binary {problem}");
    }
    let text = binary
        .to_str()
        .ok_or_else(|| anyhow!("hook binary path is not valid UTF-8"))?;
    Ok(format!("{} {HOOK_SIGNATURE}", shell_quote(text)))
}

/// Quotes `text` as one word for the shell that runs a hook entry (single
/// quotes on Unix, double quotes on Windows).
pub fn shell_quote(text: &str) -> String {
    if cfg!(windows) {
        format!("\"{text}\"")
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

pub fn settings_path(layout: &RepositoryLayout) -> Result<PathBuf> {
    Ok(layout.worktree_root()?.join(SETTINGS_RELATIVE_PATH))
}

/// Why `<worktree>/.claude` cannot be used.
#[derive(Debug)]
enum DirProblem {
    Symlink,
    NotDirectory,
    Outside,
    Io(std::io::Error),
}

impl DirProblem {
    /// One sentence, without the path (callers add it where it helps).
    fn reason(&self) -> String {
        match self {
            Self::Symlink => "`.claude` in this worktree is a symlink; not following it".into(),
            Self::NotDirectory => "`.claude` in this worktree is not a directory".into(),
            Self::Outside => {
                "`.claude` in this worktree resolves outside the worktree; not using it".into()
            }
            Self::Io(error) => format!("`.claude` in this worktree cannot be inspected: {error}"),
        }
    }
}

/// Looks at `<root>/.claude` without following it: `Ok(None)` when absent,
/// `Ok(Some(canonical path))` for a real directory inside the worktree, and a
/// reason when it is a symlink (a branch can commit one pointing anywhere), a
/// file, or resolves elsewhere. `root` is a canonical worktree root.
fn claude_dir_state(root: &Path) -> std::result::Result<Option<PathBuf>, DirProblem> {
    let dir = root.join(CLAUDE_DIR);
    let metadata = match fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(DirProblem::Io(error)),
    };
    if metadata.file_type().is_symlink() {
        return Err(DirProblem::Symlink);
    }
    if !metadata.is_dir() {
        return Err(DirProblem::NotDirectory);
    }
    let canonical = fs::canonicalize(&dir).map_err(DirProblem::Io)?;
    let canonical_root = fs::canonicalize(root).map_err(DirProblem::Io)?;
    if !canonical.starts_with(&canonical_root) {
        return Err(DirProblem::Outside);
    }
    Ok(Some(canonical))
}

/// [`claude_dir_state`] with the problem turned into an error naming the path.
fn inspect_claude_dir(root: &Path) -> Result<Option<PathBuf>> {
    claude_dir_state(root)
        .map_err(|problem| anyhow!("{} ({})", problem.reason(), root.join(CLAUDE_DIR).display()))
}

/// The verified `.claude` directory, created (plain `mkdir`, never `-p`) when
/// it is absent.
fn ensure_claude_dir(root: &Path) -> Result<PathBuf> {
    if let Some(dir) = inspect_claude_dir(root)? {
        return Ok(dir);
    }
    let path = root.join(CLAUDE_DIR);
    match fs::create_dir(&path) {
        Ok(()) => {}
        // Created meanwhile; the inspection below judges whatever is there.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to create {}", path.display()))
        }
    }
    inspect_claude_dir(root)?.ok_or_else(|| anyhow!("{} vanished", path.display()))
}

/// Refuses a settings file that Git tracks in this checkout's index. Only that
/// exact path: repositories commonly track `.claude/settings.json` and
/// `.claude/agents/`, which have nothing to do with this file.
fn refuse_if_tracked(layout: &RepositoryLayout) -> Result<()> {
    let index = layout
        .repository()
        .index()
        .context("failed to read the Git index")?;
    if index
        .get_path(Path::new(SETTINGS_RELATIVE_PATH), 0)
        .is_some()
    {
        return Err(TrackedSettings.into());
    }
    Ok(())
}

/// Installs or refreshes the hook entries. Existing unrelated settings and
/// hooks are preserved byte-for-byte in meaning; only entries carrying the CVC
/// signature are touched. Idempotent. Refuses a `.claude` that is not a real
/// directory, and a tracked settings file ([`TrackedSettings`]).
pub fn install(layout: &RepositoryLayout, binary: &Path) -> Result<InstallOutcome> {
    install_with(layout, binary, false)
}

/// Like [`install`], for a worktree that was just created: it only ever
/// creates the settings file. If anything is already at that path -- a file, a
/// link, a directory, whatever its content -- it refuses ([`SettingsExist`])
/// instead of merging into it. Merging belongs to the person running
/// `cvc harness install` in a checkout they chose; the `post-checkout`
/// propagation runs unattended, inside Git, on whatever a branch put there.
pub fn create(layout: &RepositoryLayout, binary: &Path) -> Result<InstallOutcome> {
    install_with(layout, binary, true)
}

fn install_with(
    layout: &RepositoryLayout,
    binary: &Path,
    create_only: bool,
) -> Result<InstallOutcome> {
    let command = hook_command(binary)?;
    let root = layout.worktree_root()?;
    let settings_path = settings_path(layout)?;
    inspect_claude_dir(root)?;
    refuse_if_tracked(layout)?;
    if create_only {
        match fs::symlink_metadata(&settings_path) {
            Ok(_) => return Err(SettingsExist.into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", settings_path.display()))
            }
        }
    }
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
        let dir = ensure_claude_dir(root)?;
        write_settings(&dir, &settings_path, &settings)?;
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
    let root = layout.worktree_root()?;
    let settings_path = settings_path(layout)?;
    let nothing = |settings_path: PathBuf| UninstallOutcome {
        settings_path,
        removed: 0,
        deleted_file: false,
    };
    let Some(dir) = inspect_claude_dir(root)? else {
        return Ok(nothing(settings_path));
    };
    match fs::symlink_metadata(&settings_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(nothing(settings_path))
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
        return Ok(nothing(settings_path));
    }
    let deleted_file = settings.is_empty();
    if deleted_file {
        fs::remove_file(&settings_path)?;
    } else {
        write_settings(&dir, &settings_path, &settings)?;
    }
    Ok(UninstallOutcome {
        settings_path,
        removed,
        deleted_file,
    })
}

/// Whether this checkout's settings file carries CVC's hook entries, which is
/// to say whether Claude Code sessions here are being ingested. Read-only; a
/// missing, unreadable, or refused settings file reads as not installed.
pub fn installed(layout: &RepositoryLayout) -> bool {
    let Ok(root) = layout.worktree_root() else {
        return false;
    };
    check_worktree(root) == WorktreeState::Installed
}

fn check_worktree(root: &Path) -> WorktreeState {
    match claude_dir_state(root) {
        Ok(None) => return WorktreeState::NotInstalled,
        Ok(Some(_)) => {}
        Err(problem) => return WorktreeState::Unreadable(problem.reason()),
    }
    match read_settings(&root.join(SETTINGS_RELATIVE_PATH)) {
        Ok(settings) if carries_hooks(&settings) => WorktreeState::Installed,
        Ok(_) => WorktreeState::NotInstalled,
        Err(error) => WorktreeState::Unreadable(format!("{error:#}")),
    }
}

fn carries_hooks(settings: &Map<String, Value>) -> bool {
    settings
        .get("hooks")
        .and_then(Value::as_object)
        .is_some_and(|hooks| {
            hooks
                .values()
                .filter_map(Value::as_array)
                .flatten()
                .filter_map(|group| group.get("hooks").and_then(Value::as_array))
                .flatten()
                .any(|entry| {
                    entry
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|command| command.contains(HOOK_SIGNATURE))
                })
        })
}

/// Every worktree of this repository other than the active one that still
/// exists on disk -- the main worktree first, then the linked ones by path --
/// each with whether it carries CVC's hook entries. Read-only. Nothing is
/// followed out of a worktree: a `.claude` symlink, or a settings file that
/// cannot be read safely, comes back as [`WorktreeState::Unreadable`] rather
/// than as "not installed".
///
/// The main worktree is found as the parent of the common Git directory when
/// that directory is named `.git`. A separately placed Git directory carries
/// no such back-reference, so it is not guessed at; the survey says so in
/// [`WorktreeSurvey::main_located`] instead of omitting it silently. (A bare
/// repository has no main worktree to find.)
pub fn other_worktrees(layout: &RepositoryLayout) -> Result<WorktreeSurvey> {
    let active = layout.worktree_root()?;
    let common = layout.common_git_dir();
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut main_located = true;

    if layout.is_linked_worktree() {
        if common.file_name().is_some_and(|name| name == ".git") {
            if let Some(main) = common.parent() {
                candidates.push(main.to_path_buf());
            }
        } else {
            let bare = git2::Repository::open(common)
                .map(|repository| repository.is_bare())
                .unwrap_or(false);
            main_located = bare;
        }
    }

    let repository = layout.repository();
    let mut linked: Vec<PathBuf> = Vec::new();
    for name in repository.worktrees()?.iter().flatten() {
        if let Ok(worktree) = repository.find_worktree(name) {
            linked.push(worktree.path().to_path_buf());
        }
    }
    linked.sort();
    candidates.extend(linked);

    let mut seen: Vec<PathBuf> = vec![active.to_path_buf()];
    let mut worktrees = Vec::new();
    for candidate in candidates {
        // A removed worktree whose administrative entry is not pruned yet
        // cannot be canonicalized.
        let Ok(root) = fs::canonicalize(&candidate) else {
            continue;
        };
        if !root.is_dir() || seen.contains(&root) {
            continue;
        }
        let state = check_worktree(&root);
        seen.push(root.clone());
        worktrees.push(WorktreeHooks { root, state });
    }
    Ok(WorktreeSurvey {
        worktrees,
        main_located,
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
    if metadata.len() > MAX_SETTINGS_BYTES {
        bail!("{} is too large to be a settings file", path.display());
    }
    let mut text = String::new();
    open_settings(path)
        .and_then(|file| file.take(MAX_SETTINGS_BYTES + 1).read_to_string(&mut text))
        .with_context(|| format!("failed to read {}", path.display()))?;
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

/// Opens without following a symlink swapped in after the metadata check.
#[cfg(unix)]
fn open_settings(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_settings(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

/// A fresh, private (0600 on Unix) file that is never a symlink to somewhere
/// else: `create_new` refuses anything already there.
#[cfg(unix)]
fn create_temporary(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn create_temporary(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Writes `settings` to `path` inside the already verified `.claude` directory
/// `dir`. The temporary file has a random name and is created exclusively, so
/// nothing a branch committed can be written through; a settings file that
/// already existed keeps its permissions, a new one is private.
fn write_settings(dir: &Path, path: &Path, settings: &Map<String, Value>) -> Result<()> {
    let mut rendered = serde_json::to_string_pretty(&Value::Object(settings.clone()))?;
    rendered.push('\n');
    let existing_permissions = fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.permissions());
    // Write-then-rename so a crash never leaves Claude Code a half-written
    // settings file to reject.
    let temporary = dir.join(format!(
        ".settings.local.json.{}.cvc-tmp",
        uuid::Uuid::new_v4()
    ));
    let written = (|| -> std::io::Result<()> {
        let mut file = create_temporary(&temporary)?;
        file.write_all(rendered.as_bytes())?;
        if let Some(permissions) = existing_permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("failed to write {}", path.display()));
    }
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
