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
//! The file is kept out of commits by an entry in the repository's
//! `info/exclude`, which [`install`] and [`create`] both add (when that exact
//! line is missing) before they write anything. A `.gitignore` rule in the
//! checkout overrides that file, so with the entry in place `git check-ignore`
//! is asked, once, whether the path is now ignored. The unattended [`create`]
//! goes on only when Git says it is: when Git says it is not
//! ([`SettingsNotIgnored`]) or gives no verdict ([`SettingsIgnoreUnknown`]) it
//! creates nothing, neither the `.claude` directory nor the file, and the entry
//! stays. [`install`], which a person runs in a checkout they chose, writes the
//! file all the same and reports [`ExcludeAction::NotIgnored`] so they can be
//! told; when `git` gives no verdict it falls back on libgit2's answer, which is
//! not used unattended.
//!
//! Like the Git hooks installer, this module never prints: it returns what it
//! did so callers can report it in whichever way suits their stdout.
use crate::repository::RepositoryLayout;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::borrow::Cow;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
/// How installed entries are recognized regardless of the binary path. Public
/// so that what else runs the same command (the Claude Code plugin's
/// `hooks/hooks.json`) can be checked against it.
pub const HOOK_SIGNATURE: &str = "ingest claude-code --hook";
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
    /// The entry was already in `info/exclude`, so nothing was added. It says
    /// nothing about what Git ignores; see [`ExcludeAction::NotIgnored`].
    AlreadyIgnored,
    /// The entry was added to `info/exclude`.
    AddedToInfoExclude,
    /// The entry is in `info/exclude` (added now or earlier), but the settings
    /// file is still not ignored: Git says so, usually because a `.gitignore`
    /// rule in the checkout overrides `info/exclude`, or libgit2 does when
    /// `git` could not be asked.
    NotIgnored,
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

/// Returned (inside an [`anyhow::Error`]) by [`create`] when Git would not
/// ignore the settings file even with CVC's entry in `info/exclude`, because a
/// `.gitignore` rule in the checkout overrides that file. The file holds a
/// machine-specific path, so nothing is created: no `.claude` directory and no
/// settings file. The entry stays in `info/exclude`.
#[derive(Debug, thiserror::Error)]
#[error("`.claude/settings.local.json` would not be ignored by Git in this worktree: a `.gitignore` rule overrides the entry CVC adds to `.git/info/exclude`; CVC writes a machine-specific path there and will not create a file that a commit could carry (fix the rule, then run `cvc harness install claude-code` here)")]
pub struct SettingsNotIgnored;

/// Returned (inside an [`anyhow::Error`]) by [`create`] when Git gave no
/// verdict on the settings file: `git` could not be run in the worktree, or
/// ran and declined to answer (a `.claude` that is a submodule, a directory it
/// refused). The file holds a machine-specific path and nothing is created
/// that Git is not known to ignore, so no `.claude` directory and no settings
/// file exist afterwards. The entry stays in `info/exclude`.
#[derive(Debug, thiserror::Error)]
#[error("could not ask Git whether `.claude/settings.local.json` is ignored in this worktree; CVC writes a machine-specific path there and creates nothing it cannot confirm Git ignores (run `cvc harness install claude-code` here)")]
pub struct SettingsIgnoreUnknown;

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
///
/// The `info/exclude` entry goes in first and Git is then asked, once, whether
/// the file is ignored. When it is not (a `.gitignore` rule overrides the
/// entry) the file is written all the same and the outcome says so
/// ([`ExcludeAction::NotIgnored`]), so the person who ran this can be told.
/// When `git` cannot be run or gives no verdict, libgit2's answer stands as a
/// last resort: it drops a lone `!` rule of a `.gitignore`, so it can call a
/// path ignored that Git lists.
pub fn install(layout: &RepositoryLayout, binary: &Path) -> Result<InstallOutcome> {
    install_with(layout, binary, false)
}

/// Like [`install`], for a worktree that was just created: it only ever
/// creates the settings file. If anything is already at that path -- a file, a
/// link, a directory, whatever its content -- it refuses ([`SettingsExist`])
/// instead of merging into it. Merging belongs to the person running
/// `cvc harness install` in a checkout they chose; the `post-checkout`
/// propagation runs unattended, inside Git, on whatever a branch put there.
///
/// For the same reason it creates the file only when Git is known to ignore
/// it. The `info/exclude` entry goes in first and Git is then asked, once; if
/// Git says the file is not ignored (a `.gitignore` rule overrides the entry,
/// [`SettingsNotIgnored`]) or gives no verdict ([`SettingsIgnoreUnknown`];
/// libgit2 is not consulted in its place), it refuses before the `.claude`
/// directory or the file exists. Nobody is there to be warned, and the file
/// holds a machine-specific path that must not be committed.
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
    // The entry goes into `info/exclude` before the `.claude` directory or the
    // file is touched, and Git is then asked, once, whether it ignores the
    // file: a `.gitignore` rule in the checkout overrides `info/exclude`, so
    // the entry alone does not say. An unattended create goes on only when Git
    // says yes, and stops here, before `.claude` or the file exist, when it
    // says no or gives no verdict. An install the person chose to run writes
    // the file in any case and reports a "no"; when `git` gave no verdict,
    // libgit2's answer stands in for it.
    let mut exclude = ensure_excluded(layout)?;
    let verdict = ask_git_if_ignored(layout);
    if create_only {
        match verdict {
            Some(true) => {}
            Some(false) => return Err(SettingsNotIgnored.into()),
            None => return Err(SettingsIgnoreUnknown.into()),
        }
    } else if !verdict.unwrap_or_else(|| libgit2_ignores_settings(layout)) {
        exclude = ExcludeAction::NotIgnored;
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

/// Whether Git ignores the settings file in this checkout, by every source it
/// reads: `.gitignore` files (including those a sparse checkout keeps only in
/// the index), `info/exclude`, the user's global excludes. `Some` is the answer
/// of `git check-ignore` run in this checkout (exit status 0 is ignored, 1 is
/// not ignored); `None` is no answer at all: `git` not found, killed, or ending
/// with any other status (128 when it refuses the directory or cannot judge the
/// path, as when `.claude` is a submodule).
///
/// `git` itself is asked, since it decides what a commit carries. libgit2 is
/// not consulted here: it drops a lone `!` rule from a `.gitignore`, although
/// that rule still overrides `info/exclude` and the global excludes, so it can
/// report "ignored" where `git` reports "not ignored" (see
/// [`libgit2_ignores_settings`]).
///
/// In a partial clone the check may fetch an ignore file the sparse checkout
/// left out, as `git status` would. Terminal prompts are off, so a fetch that
/// needs credentials fails rather than waiting for them: Git exits 128, which
/// is no answer.
///
/// Nothing is printed, and the variables a surrounding Git command exports to
/// point at a repository, to change how pathspecs are read or to turn off lazy
/// fetching are removed from the environment, so the answer is about this
/// checkout.
fn ask_git_if_ignored(layout: &RepositoryLayout) -> Option<bool> {
    let root = layout.worktree_root().ok()?;
    // `fs::canonicalize` gives a Windows path the `\\?\` prefix; `git -C` is
    // handed the plain spelling (`C:\x`, `\\server\share\x`).
    let root = match root.to_str() {
        Some(text) if cfg!(windows) => PathBuf::from(without_verbatim_prefix(text).into_owned()),
        _ => root.to_path_buf(),
    };
    let mut command = Command::new("git");
    // No `--no-index`: the index is what `git add` and `git status` consult,
    // including ignore files that a sparse checkout keeps only in the index. A
    // tracked path answers "not ignored", which `refuse_if_tracked` has handled
    // before this point; a `.claude` that is a gitlink makes Git give no
    // verdict (exit 128), which the unattended path declines.
    command
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--quiet", "--", SETTINGS_RELATIVE_PATH]);
    // These name a repository, a work tree or an index for a Git command; the
    // ones of a surrounding command would make the answer about that
    // repository, not this checkout.
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(name);
    }
    // Git exports these to hooks when the caller passed `--literal-pathspecs`
    // and friends, and `check-ignore` refuses to run with them (exit 128); the
    // path here is a constant with no glob characters, so they cannot change
    // the answer.
    for name in [
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
    ] {
        command.env_remove(name);
    }
    // Git exports this to hooks when the caller passed `--no-lazy-fetch`. In a
    // partial clone whose sparse checkout leaves `.claude/` out, an ignore
    // file kept only in the index may have no local blob; with lazy fetching
    // off, Git treats that file as absent and answers "ignored" from the other
    // sources, though a rule in the file may say otherwise. With the variable
    // gone, Git either fetches the blob, as `git status` would, or exits 128,
    // which the unattended path declines.
    command.env_remove("GIT_NO_LAZY_FETCH");
    // With terminal prompts off, a fetch that needs credentials fails, and the
    // unattended path declines, instead of stopping at a prompt in the middle
    // of `git worktree add`.
    command.env("GIT_TERMINAL_PROMPT", "0");
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    match status.code()? {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// libgit2's answer for the settings file, for an install a person runs when
/// `git` gave no verdict. A last resort and for that interactive path only:
/// libgit2 drops a lone `!` rule from a `.gitignore` (one that undoes no
/// earlier rule of the same file), although that rule still overrides
/// `info/exclude` and the global excludes, so it can call a path ignored that
/// `git` lists. A lookup that fails counts as not ignored.
fn libgit2_ignores_settings(layout: &RepositoryLayout) -> bool {
    layout
        .repository()
        .is_path_ignored(Path::new(SETTINGS_RELATIVE_PATH))
        .unwrap_or(false)
}

/// `path` without the `\\?\` prefix that `fs::canonicalize` gives a Windows
/// path: `\\?\C:\x` is `C:\x` and `\\?\UNC\server\share\x` is
/// `\\server\share\x`. Any other spelling comes back unchanged, including a
/// verbatim one that has no plain equivalent here (`\\?\Volume{...}\x`).
fn without_verbatim_prefix(path: &str) -> Cow<'_, str> {
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        if !unc.is_empty() && !unc.starts_with('\\') {
            return Cow::Owned(format!(r"\\{unc}"));
        }
    } else if let Some(drive) = path.strip_prefix(r"\\?\") {
        let bytes = drive.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
        {
            return Cow::Borrowed(drive);
        }
    }
    Cow::Borrowed(path)
}

/// Keeps the machine-specific settings file out of commits. Claude Code adds
/// a global exclude only when it creates the file itself, so an installer that
/// creates it must do the equivalent. The repository-local `info/exclude` is
/// shared by every linked worktree and is not a tracked file.
///
/// Appends the entry whenever that exact line is missing, whatever else
/// currently ignores the path: an entry that is to last must not depend on a
/// `.gitignore` rule or a global excludes file that may change later.
/// [`ExcludeAction::AlreadyIgnored`] means only that the line was already
/// there. The entry being there does not mean Git ignores the file -- a
/// `.gitignore` rule in the checkout takes precedence over `info/exclude` --
/// so the caller asks Git afterwards ([`ask_git_if_ignored`]), once.
fn ensure_excluded(layout: &RepositoryLayout) -> Result<ExcludeAction> {
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

#[cfg(test)]
mod tests {
    use super::without_verbatim_prefix;
    use std::borrow::Cow;

    #[test]
    fn a_verbatim_drive_path_loses_its_prefix() {
        assert_eq!(without_verbatim_prefix(r"\\?\C:\x"), r"C:\x");
        assert_eq!(
            without_verbatim_prefix(r"\\?\d:\work\tree\x"),
            r"d:\work\tree\x"
        );
        assert_eq!(without_verbatim_prefix(r"\\?\C:\"), r"C:\");
    }

    #[test]
    fn a_verbatim_unc_path_becomes_a_plain_unc_path() {
        assert_eq!(
            without_verbatim_prefix(r"\\?\UNC\server\share\x"),
            r"\\server\share\x"
        );
        assert_eq!(
            without_verbatim_prefix(r"\\?\UNC\server\share"),
            r"\\server\share"
        );
    }

    #[test]
    fn any_other_spelling_is_returned_unchanged() {
        for path in [
            r"C:\x",
            r"\\server\share\x",
            "/home/user/tree",
            "relative/tree",
            "",
            // Verbatim spellings with no plain equivalent here.
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\x",
            r"\\?\C:",
            r"\\?\C:x",
            r"\\?\1:\x",
            r"\\?\UNC\",
            r"\\?\UNC\\x",
            r"\\?\unc\server\share\x",
            // Device paths are not verbatim paths.
            r"\\.\C:\x",
        ] {
            let spelled = without_verbatim_prefix(path);
            assert_eq!(spelled, path);
            assert!(matches!(spelled, Cow::Borrowed(_)), "{path}");
        }
    }
}
