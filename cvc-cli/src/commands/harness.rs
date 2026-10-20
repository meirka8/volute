//! `cvc harness`: lifecycle-hook capture for agent harnesses.
use anyhow::{bail, Context, Result};
use cvc_core::claude_code::harness_record;
use cvc_core::claude_code::settings::{self, ExcludeAction, InstallAction, WorktreeState};
use cvc_core::hooks::{self, HookAction};
use cvc_core::privacy;
use cvc_core::repository::RepositoryLayout;
use std::cmp::Ordering;
use std::env;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Directory (below `~/.cvc`) the npm launcher unpacks version-specific
/// binaries into: `~/.cvc/mcp-cache/v<version>-<arch>-<os>/cvc`.
const LAUNCHER_CACHE_DIR: &str = "mcp-cache";

/// How [`resolve_hook_binary`] arrived at the path it returns, so the caller
/// can tell the user what happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BinaryChoice {
    /// The running executable, unchanged: it is neither a launcher-cache copy
    /// nor the stable binary, or the home directory is unknown.
    Running,
    /// The running executable is the stable `~/.cvc/bin/cvc`; its path is
    /// recorded in the `<home>/.cvc/bin/cvc` spelling.
    Stable,
    /// The running executable is a launcher-cache copy and the stable binary
    /// exists and reports a version at least as new as this one, so the stable
    /// path is recorded instead.
    StableInsteadOfCache,
    /// The running executable is a launcher-cache copy but `stable` does not
    /// exist, so the cache copy is recorded: a hook must never point at a
    /// missing file. The recorded command goes stale on the next upgrade.
    CacheCopyOnly { stable: PathBuf },
    /// The running executable is a launcher-cache copy and `stable` exists,
    /// but it reports an older version than this binary (`reported`), or none
    /// that could be read. The launcher runs from its cache precisely when it
    /// declined to publish here, so an older file is the expected failure:
    /// steering the hooks onto it could break them (a release without the
    /// `ingest` subcommand). The cache copy is recorded instead.
    CacheCopyStableOlder {
        stable: PathBuf,
        reported: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedBinary {
    pub(crate) path: PathBuf,
    pub(crate) choice: BinaryChoice,
}

/// `<home>/.cvc/bin/cvc` (`cvc.exe` on Windows): where `install.sh`,
/// `install.ps1` and the npm launcher publish the verified binary.
fn stable_binary(home: &Path) -> PathBuf {
    home.join(".cvc")
        .join("bin")
        .join(format!("cvc{}", env::consts::EXE_SUFFIX))
}

/// The home directory the npm launcher resolves with Node's `os.homedir()`:
/// `HOME` on Unix, `USERPROFILE` on Windows. Unset or empty means unknown.
fn home_dir() -> Option<PathBuf> {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
}

/// Both paths canonicalized, or `None` when either cannot be. Comparisons are
/// only ever made canonical-to-canonical (Windows canonical paths carry a
/// `\\?\` prefix a raw path never matches) or raw-to-raw.
fn canonical_pair(a: &Path, b: &Path) -> Option<(PathBuf, PathBuf)> {
    Some((fs::canonicalize(a).ok()?, fs::canonicalize(b).ok()?))
}

/// Component-wise containment, so `mcp-cache-old` is not under `mcp-cache`,
/// and symlinked homes (`/home` vs `/var/home`) still match.
fn is_under(path: &Path, root: &Path) -> bool {
    match canonical_pair(path, root) {
        Some((path, root)) => path.starts_with(root),
        None => path.starts_with(root),
    }
}

fn is_same_file(a: &Path, b: &Path) -> bool {
    match canonical_pair(a, b) {
        Some((a, b)) => a == b,
        None => a == b,
    }
}

/// A SemVer 2.0.0 version: enough to order the releases `cvc --version`
/// reports. Build metadata is dropped at parse time since it never takes part
/// in precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    core: [u64; 3],
    pre: Vec<PreRelease>,
}

/// One dot-separated pre-release identifier. The declaration order makes the
/// derived ordering match SemVer: numeric identifiers compare numerically and
/// sort before alphanumeric ones, which compare in ASCII order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PreRelease {
    Numeric(u64),
    Alpha(String),
}

impl Version {
    /// Parses `major.minor.patch[-pre][+build]`; anything else is `None`.
    fn parse(text: &str) -> Option<Self> {
        let text = text.split_once('+').map_or(text, |(version, _)| version);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut parts = core.split('.');
        let core = [
            numeric_identifier(parts.next()?)?,
            numeric_identifier(parts.next()?)?,
            numeric_identifier(parts.next()?)?,
        ];
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            Some(pre) => pre
                .split('.')
                .map(pre_release_identifier)
                .collect::<Option<Vec<_>>>()?,
            None => Vec::new(),
        };
        Some(Self { core, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core
            .cmp(&other.core)
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                // A release outranks any of its own pre-releases.
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                // Field by field; with equal prefixes the longer list wins.
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Digits only, no leading zeros, fits in a `u64`.
fn numeric_identifier(text: &str) -> Option<u64> {
    let digits = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits || (text.len() > 1 && text.starts_with('0')) {
        return None;
    }
    text.parse().ok()
}

fn pre_release_identifier(text: &str) -> Option<PreRelease> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        return numeric_identifier(text).map(PreRelease::Numeric);
    }
    Some(PreRelease::Alpha(text.to_owned()))
}

/// The version in one line of `cvc --version` output: `cvc <semver>` with
/// optional trailing text. Build metadata is stripped from the result.
fn parse_version_line(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("cvc")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let token = rest.split_whitespace().next()?;
    let token = token.split_once('+').map_or(token, |(version, _)| version);
    Version::parse(token)?;
    Some(token.to_owned())
}

/// What a finished `--version` probe means. Current releases print to stdout
/// and exit 0; releases up to v0.6.0 print to stderr and exit 1.
fn version_from_output(exit_code: Option<i32>, stdout: &str, stderr: &str) -> Option<String> {
    match exit_code {
        Some(0) => parse_version_line(stdout.lines().next()?),
        Some(1) if stdout.trim().is_empty() => parse_version_line(stderr.lines().next()?),
        _ => None,
    }
}

const PROBE_DEADLINE: Duration = Duration::from_secs(5);
const PROBE_POLL: Duration = Duration::from_millis(25);
/// How long to wait for the pipes to close after the child is gone, in case a
/// background grandchild inherited them.
const PROBE_GRACE: Duration = Duration::from_millis(500);
const PROBE_OUTPUT_CAP: usize = 64 * 1024;

/// A pipe being drained on its own thread. The thread reads until end of
/// file and keeps at most [`PROBE_OUTPUT_CAP`] bytes, but never stops
/// reading: a child writing more than the pipe buffer holds must not block.
struct Drain {
    buffer: Arc<Mutex<Vec<u8>>>,
    /// Disconnects when the reader thread ends.
    finished: mpsc::Receiver<()>,
}

impl Drain {
    fn start(mut pipe: impl Read + Send + 'static) -> Self {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let (done, finished) = mpsc::channel::<()>();
        let sink = Arc::clone(&buffer);
        thread::spawn(move || {
            let _done = done;
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let mut kept = sink.lock().unwrap_or_else(|error| error.into_inner());
                        let room = PROBE_OUTPUT_CAP.saturating_sub(kept.len());
                        kept.extend_from_slice(&chunk[..read.min(room)]);
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
        Self { buffer, finished }
    }

    fn text(&self) -> String {
        let kept = self
            .buffer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        String::from_utf8_lossy(&kept).into_owned()
    }
}

/// Runs `<binary> --version` and returns the version it reports, or `None`
/// when it cannot be run, does not finish in time, or prints nothing usable.
fn probe_version(binary: &Path) -> Option<String> {
    probe_version_within(binary, PROBE_DEADLINE)
}

fn probe_version_within(binary: &Path, deadline: Duration) -> Option<String> {
    let mut child = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let (stdout, stderr) = (Drain::start(stdout), Drain::start(stderr));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < deadline => thread::sleep(PROBE_POLL),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    // Let the readers take what the child wrote before it exited. A
    // background process that inherited the pipes keeps them open, so this
    // wait is bounded and whatever arrived by then is used.
    let grace_ends = Instant::now() + PROBE_GRACE;
    for drain in [&stdout, &stderr] {
        let _ = drain
            .finished
            .recv_timeout(grace_ends.saturating_duration_since(Instant::now()));
    }
    version_from_output(status.code(), &stdout.text(), &stderr.text())
}

/// Picks the binary the hooks should run when `--binary` was not given.
///
/// A `cvc` that came through the npm launcher runs from a version-specific
/// cache (`<home>/.cvc/mcp-cache/v<version>-<arch>-<os>/cvc`); recording that
/// path pins the hooks to one release forever. The launcher also publishes
/// the verified binary to `<home>/.cvc/bin/cvc`, which survives upgrades, so
/// when that file exists and `probe` reports a version at least as new as
/// `own_version` it is recorded instead. The launcher falls back to its cache
/// exactly when it declined to publish, so the stable file may be older or
/// broken; in that case the cache copy is kept. `probe` runs only in this
/// cache case and returns the version a binary reports, if any. The returned
/// stable path is always spelled from `home`, never canonicalized.
pub(crate) fn resolve_hook_binary(
    current_exe: &Path,
    home: Option<&Path>,
    own_version: &str,
    probe: impl Fn(&Path) -> Option<String>,
) -> ResolvedBinary {
    let running = |choice| ResolvedBinary {
        path: current_exe.to_path_buf(),
        choice,
    };
    // A relative home would yield a relative hook path, which the settings
    // writer rejects; treat it (and an empty one) like an unknown home.
    let Some(home) = home.filter(|home| home.is_absolute()) else {
        return running(BinaryChoice::Running);
    };
    let stable = stable_binary(home);
    if is_under(current_exe, &home.join(".cvc").join(LAUNCHER_CACHE_DIR)) {
        if !stable.is_file() {
            return running(BinaryChoice::CacheCopyOnly { stable });
        }
        let own = Version::parse(own_version);
        let reported = probe(&stable).and_then(|text| Version::parse(&text).map(|v| (text, v)));
        return match reported {
            Some((_, version)) if own.is_some_and(|own| version >= own) => ResolvedBinary {
                path: stable,
                choice: BinaryChoice::StableInsteadOfCache,
            },
            reported => running(BinaryChoice::CacheCopyStableOlder {
                stable,
                reported: reported.map(|(text, _)| text),
            }),
        };
    }
    if is_same_file(current_exe, &stable) {
        return ResolvedBinary {
            path: stable,
            choice: BinaryChoice::Stable,
        };
    }
    running(BinaryChoice::Running)
}

pub async fn install_claude_code(binary: Option<PathBuf>) -> Result<()> {
    let layout = discover_initialized()?;
    // Hook-driven ingestion is background observation of an agent session, so
    // it needs the same repository-local acknowledgement as passive VS Code
    // capture, and installing hooks that would refuse to run is not useful.
    require_capture_acknowledged(&layout)?;
    // `--binary` overrides everything; only the default is steered away from
    // the launcher's versioned cache.
    let (binary, choice) = match binary {
        Some(binary) => (binary, None),
        None => {
            let running = env::current_exe().context(
                "cannot resolve the running cvc executable; pass --binary <absolute path>",
            )?;
            let resolved = resolve_hook_binary(
                &running,
                home_dir().as_deref(),
                env!("CARGO_PKG_VERSION"),
                probe_version,
            );
            (resolved.path, Some(resolved.choice))
        }
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
    let cache_fallback = matches!(
        choice,
        Some(BinaryChoice::CacheCopyOnly { .. } | BinaryChoice::CacheCopyStableOlder { .. })
    );
    match choice {
        Some(BinaryChoice::StableInsteadOfCache) => println!(
            "  using {} instead of the launcher's versioned cache, so upgrades reach the hooks without a reinstall",
            binary.display()
        ),
        Some(BinaryChoice::CacheCopyOnly { stable }) => {
            eprintln!(
                "  warning: {} does not exist, so the hooks point at the launcher's versioned cache and will not follow upgrades",
                stable.display()
            );
            eprintln!(
                "  run `cvc harness install claude-code` again once it exists (the launcher retries publishing on each launch)"
            );
        }
        Some(BinaryChoice::CacheCopyStableOlder { stable, reported }) => {
            let own = env!("CARGO_PKG_VERSION");
            match reported {
                Some(reported) => eprintln!(
                    "  warning: {} reports {reported}, older than this cvc {own}, so the hooks point at the launcher's versioned cache and will not follow upgrades",
                    stable.display()
                ),
                None => eprintln!(
                    "  warning: {} reports no usable version (this cvc is {own}), so the hooks point at the launcher's versioned cache and will not follow upgrades",
                    stable.display()
                ),
            }
            eprintln!(
                "  run `cvc harness install claude-code` again after the launcher has published this release to ~/.cvc/bin (it retries on each launch)"
            );
        }
        // The one-time migration from a 0.7.0-era install: the shim now runs
        // the stable binary and the old cache-path entries were rewritten.
        Some(BinaryChoice::Stable) if outcome.action == InstallAction::Updated => println!(
            "  hooks now run {}, which upgrades replace in place",
            binary.display()
        ),
        Some(BinaryChoice::Running | BinaryChoice::Stable) | None => {}
    }
    match outcome.exclude {
        ExcludeAction::AddedToInfoExclude => println!(
            "  added {} to the repository's info/exclude so the machine-specific path is never committed",
            settings::SETTINGS_RELATIVE_PATH
        ),
        ExcludeAction::AlreadyIgnored => {}
    }
    // The per-checkout install above is complete; what follows only makes new
    // worktrees follow it. A failure there is reported, never fatal.
    if cache_fallback {
        // A versioned-cache path goes stale on the next upgrade, and every
        // worktree created from the record would inherit the staleness, so
        // this install is not recorded. What that means depends on whether an
        // earlier install left a usable record.
        let earlier = harness_record::read_record(&layout)
            .ok()
            .flatten()
            .filter(|recorded| settings::binary_problem(recorded).is_none());
        match earlier {
            Some(recorded) => println!(
                "New worktrees keep getting hooks that run {} (recorded earlier); this install was not recorded because it ran from the launcher's versioned cache.",
                printable_path(&recorded)
            ),
            None => println!(
                "New worktrees will not get these hooks until you run `cvc harness install claude-code` again with a current cvc in ~/.cvc/bin."
            ),
        }
    } else if enable_worktree_propagation(&layout, &binary) {
        println!(
            "Claude Code sessions in worktrees created from now on (git worktree add, Claude Code's worktree option) are captured too: they get these hooks automatically while this repository's capture acknowledgement stands. `cvc harness uninstall claude-code` stops this."
        );
    } else {
        println!("This install is per checkout: run it again in each linked worktree you use.");
    }
    report_other_worktrees(
        &layout,
        WorktreeWanted::WithoutHooks,
        "Existing worktrees without these hooks (run `cvc harness install claude-code` there once):",
        Some(WORKTREE_LIST_LIMIT),
    );
    if matches!(
        outcome.action,
        InstallAction::Created | InstallAction::Updated
    ) {
        println!(
            "Claude Code applies these hooks to new sessions: restart Claude Code or start a new session, then run `cvc conversations` to see what was captured."
        );
    }
    Ok(())
}

/// How many worktrees the install listing names before summarizing the rest.
const WORKTREE_LIST_LIMIT: usize = 10;

/// Control characters (a newline, an escape sequence) in a path or a message
/// that came from outside this process must not reach the terminal as such.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

fn printable_path(path: &Path) -> String {
    printable(&path.to_string_lossy())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorktreeWanted {
    /// Worktrees that carry no CVC hook entries (what `install` offers to fix).
    WithoutHooks,
    /// Worktrees that still carry them (what `uninstall` leaves behind).
    WithHooks,
}

/// Lists the other worktrees `install` or `uninstall` left untouched, and says
/// plainly what it could not find out: a worktree whose settings could not be
/// read safely, a main worktree that could not be located, a failed
/// enumeration. `limit` caps the listing (the rest is counted); `None` names
/// every one, which `uninstall` does because each is still capturing.
fn report_other_worktrees(
    layout: &RepositoryLayout,
    wanted: WorktreeWanted,
    header: &str,
    limit: Option<usize>,
) {
    let survey = match settings::other_worktrees(layout) {
        Ok(survey) => survey,
        Err(error) => {
            eprintln!(
                "warning: could not list other worktrees: {}; check `git worktree list`",
                printable(&format!("{error:#}"))
            );
            return;
        }
    };
    let listed: Vec<&Path> = survey
        .worktrees
        .iter()
        .filter(|worktree| match wanted {
            WorktreeWanted::WithoutHooks => worktree.state == WorktreeState::NotInstalled,
            WorktreeWanted::WithHooks => worktree.installed(),
        })
        .map(|worktree| worktree.root.as_path())
        .collect();
    if !listed.is_empty() {
        println!("{header}");
        let shown = limit.unwrap_or(listed.len());
        for root in listed.iter().take(shown) {
            println!("  {}", printable_path(root));
        }
        if listed.len() > shown {
            println!("  and {} more", listed.len() - shown);
        }
    }
    for worktree in &survey.worktrees {
        if let WorktreeState::Unreadable(reason) = &worktree.state {
            eprintln!(
                "warning: could not check: {} ({})",
                printable_path(&worktree.root),
                printable(reason)
            );
        }
    }
    if !survey.main_located {
        eprintln!("warning: the main worktree could not be located; check it yourself");
    }
}

/// Records that the harness is wanted for this repository and makes sure Git's
/// `post-checkout` hook is there to act on the record in a new worktree. Only
/// that hook: this never re-creates a post-commit or pre-push hook the user
/// removed. Returns whether both are in place; otherwise a warning has been
/// printed.
fn enable_worktree_propagation(layout: &RepositoryLayout, binary: &Path) -> bool {
    let mut complete = true;
    if let Err(error) = harness_record::write_record(layout, binary) {
        eprintln!("  warning: could not record this install for new worktrees: {error:#}");
        complete = false;
    }
    match hooks::install_post_checkout(layout) {
        Ok(outcome) => match outcome.action {
            HookAction::Created => println!(
                "  created the post-checkout Git hook at {} so new worktrees get these hooks",
                outcome.hook_path.display()
            ),
            HookAction::Appended => println!(
                "  added CVC to the existing post-checkout Git hook at {} so new worktrees get these hooks",
                outcome.hook_path.display()
            ),
            HookAction::Updated => println!(
                "  updated the post-checkout Git hook at {} so new worktrees get these hooks",
                outcome.hook_path.display()
            ),
            HookAction::AlreadyPresent => {}
            HookAction::SkippedNotShell => {
                eprintln!("{}", super::hook::not_shell_warning(&outcome, Some(binary)));
                complete = false;
            }
        },
        Err(error) => {
            eprintln!("  warning: could not install the post-checkout Git hook: {error:#}");
            complete = false;
        }
    }
    complete
}

pub async fn uninstall_claude_code() -> Result<()> {
    let layout = discover_initialized()?;
    // Two independent halves: this checkout's entries, and the repository-wide
    // record that gives them to new worktrees. A failure in one must not stop
    // the other, or "uninstall" could leave capture running in a way the user
    // was told it no longer is.
    let mut failures: Vec<anyhow::Error> = Vec::new();
    match settings::uninstall(&layout) {
        Ok(outcome) if outcome.removed == 0 => println!(
            "No CVC hook entries found in {}",
            outcome.settings_path.display()
        ),
        Ok(outcome) if outcome.deleted_file => println!(
            "Removed {} CVC hook entr{} and the now-empty {}",
            outcome.removed,
            if outcome.removed == 1 { "y" } else { "ies" },
            outcome.settings_path.display()
        ),
        Ok(outcome) => println!(
            "Removed {} CVC hook entr{} from {}",
            outcome.removed,
            if outcome.removed == 1 { "y" } else { "ies" },
            outcome.settings_path.display()
        ),
        Err(error) => failures.push(error.context("could not remove this checkout's hooks")),
    }
    // The record is repository-wide, so it goes whatever this checkout held:
    // an uninstall means the harness is no longer wanted. It does not withdraw
    // the capture acknowledgement.
    match harness_record::clear_record(&layout) {
        Ok(true) => println!("New worktrees will no longer get the Claude Code hooks."),
        Ok(false) => {}
        Err(error) => failures.push(error.context(format!(
            "the repository-wide record that installs the hooks in new worktrees could not be cleared; remove {} by hand",
            harness_record::record_path(&layout).display()
        ))),
    }
    // Other worktrees' files are theirs to remove; every one that still
    // carries entries is still capturing, so name them all.
    report_other_worktrees(
        &layout,
        WorktreeWanted::WithHooks,
        "Other worktrees still carry these hooks and keep capturing (run `cvc harness uninstall claude-code` there):",
        None,
    );
    let mut failures = failures.into_iter();
    if let Some(first) = failures.next() {
        for other in failures {
            eprintln!("{other:#}");
        }
        return Err(first);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The version of "this binary" in the resolver tests.
    const OWN: &str = "0.7.0";

    /// A stand-in binary. Executable, since a hook entry (and a recorded path)
    /// must name a file that can be run.
    fn touch(path: &Path) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path.to_path_buf()
    }

    fn cache_exe(home: &Path) -> PathBuf {
        home.join(".cvc")
            .join(LAUNCHER_CACHE_DIR)
            .join("v0.7.0-x86_64-linux")
            .join(format!("cvc{}", env::consts::EXE_SUFFIX))
    }

    /// A probe that reports `version` for whatever binary it is asked about.
    fn reporting(version: &'static str) -> impl Fn(&Path) -> Option<String> {
        move |_| Some(version.to_owned())
    }

    /// A probe that must not run: only the cache case consults one.
    fn no_probe(binary: &Path) -> Option<String> {
        panic!("unexpected version probe of {}", binary.display());
    }

    /// Resolves a cache-copy run against a stable binary reporting `stable`.
    fn resolve_against(own: &str, stable: Option<&'static str>) -> ResolvedBinary {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let running = touch(&cache_exe(home));
        touch(&stable_binary(home));
        resolve_hook_binary(&running, Some(home), own, |_| stable.map(str::to_owned))
    }

    #[test]
    fn cache_copy_resolves_to_the_stable_binary_when_it_is_current() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let running = touch(&cache_exe(home));
        let stable = touch(&stable_binary(home));

        for reported in [OWN, "0.7.1", "0.10.0", "1.0.0"] {
            let resolved = resolve_hook_binary(&running, Some(home), OWN, reporting(reported));
            assert_eq!(resolved.path, stable, "{reported}");
            assert_eq!(resolved.choice, BinaryChoice::StableInsteadOfCache);
            assert!(resolved.path.starts_with(home.join(".cvc").join("bin")));
        }
    }

    #[test]
    fn cache_copy_keeps_itself_when_the_stable_binary_is_older() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let running = touch(&cache_exe(home));
        let stable = touch(&stable_binary(home));

        let resolved = resolve_hook_binary(&running, Some(home), OWN, reporting("0.6.0"));
        assert_eq!(resolved.path, running);
        assert_eq!(
            resolved.choice,
            BinaryChoice::CacheCopyStableOlder {
                stable,
                reported: Some("0.6.0".to_owned())
            }
        );
    }

    #[test]
    fn cache_copy_keeps_itself_when_the_stable_version_is_unknown() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let running = touch(&cache_exe(home));
        let stable = touch(&stable_binary(home));
        let expected = |reported| BinaryChoice::CacheCopyStableOlder {
            stable: stable.clone(),
            reported,
        };

        let resolved = resolve_hook_binary(&running, Some(home), OWN, |_| None);
        assert_eq!(resolved.path, running);
        assert_eq!(resolved.choice, expected(None));

        // A probe answer that is not a version counts as no answer.
        let resolved = resolve_hook_binary(&running, Some(home), OWN, reporting("nightly"));
        assert_eq!(resolved.path, running);
        assert_eq!(resolved.choice, expected(None));

        // So does an own version that cannot be compared: never steer blindly.
        let resolved = resolve_hook_binary(&running, Some(home), "dev", reporting("9.9.9"));
        assert_eq!(resolved.path, running);
        assert_eq!(resolved.choice, expected(Some("9.9.9".to_owned())));
    }

    #[test]
    fn pre_releases_order_below_their_release_and_numerically() {
        let steered = |own, stable| {
            resolve_against(own, Some(stable)).choice == BinaryChoice::StableInsteadOfCache
        };
        // A pre-release is older than its release, in both directions.
        assert!(!steered("0.7.0", "0.7.0-rc.1"));
        assert!(steered("0.7.0-rc.1", "0.7.0"));
        // Identifiers compare numerically, then in ASCII order.
        assert!(steered("0.7.0-rc.2", "0.7.0-rc.10"));
        assert!(!steered("0.7.0-rc.10", "0.7.0-rc.2"));
        assert!(steered("0.7.0-alpha", "0.7.0-beta"));
        assert!(steered("0.7.0-rc.1", "0.7.0-rc.1"));
        // A pre-release of a later version still beats an earlier release.
        assert!(steered("0.7.0", "0.8.0-alpha"));
        assert!(!steered("0.7.1", "0.7.0"));
    }

    #[test]
    fn build_metadata_does_not_take_part_in_the_comparison() {
        let steered = |own, stable| {
            resolve_against(own, Some(stable)).choice == BinaryChoice::StableInsteadOfCache
        };
        assert!(steered("0.7.0", "0.7.0+abc123"));
        assert!(steered("0.7.0+x", "0.7.0+y"));
        assert!(steered("0.7.0+z", "0.7.0"));
        assert!(!steered("0.7.1+a", "0.7.0+z"));
    }

    #[test]
    fn cache_copy_without_a_stable_binary_is_kept_and_flagged() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let running = touch(&cache_exe(home));

        // With no file there is nothing to probe.
        let resolved = resolve_hook_binary(&running, Some(home), OWN, no_probe);
        assert_eq!(resolved.path, running);
        assert_eq!(
            resolved.choice,
            BinaryChoice::CacheCopyOnly {
                stable: stable_binary(home)
            }
        );
    }

    #[test]
    fn a_stable_path_that_is_not_a_regular_file_is_not_recorded() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let running = touch(&cache_exe(home));
        fs::create_dir_all(stable_binary(home)).unwrap();

        let resolved = resolve_hook_binary(&running, Some(home), OWN, no_probe);
        assert_eq!(resolved.path, running);
        assert!(matches!(
            resolved.choice,
            BinaryChoice::CacheCopyOnly { .. }
        ));
    }

    #[test]
    fn running_the_stable_binary_records_the_home_spelling_without_a_probe() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let stable = touch(&stable_binary(home));

        let resolved = resolve_hook_binary(&stable, Some(home), OWN, no_probe);
        assert_eq!(resolved.path, stable);
        assert_eq!(resolved.choice, BinaryChoice::Stable);
    }

    #[test]
    fn an_unrelated_executable_is_recorded_as_is() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let running = touch(&temp.path().join("target").join("debug").join("cvc"));
        touch(&stable_binary(&home));

        let resolved = resolve_hook_binary(&running, Some(&home), OWN, no_probe);
        assert_eq!(resolved.path, running);
        assert_eq!(resolved.choice, BinaryChoice::Running);
    }

    #[test]
    fn a_missing_empty_or_relative_home_changes_nothing() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let running = touch(&cache_exe(home));
        touch(&stable_binary(home));

        for unusable in [None, Some(Path::new("")), Some(Path::new("relative/home"))] {
            let resolved = resolve_hook_binary(&running, unusable, OWN, no_probe);
            assert_eq!(resolved.path, running, "{unusable:?}");
            assert_eq!(resolved.choice, BinaryChoice::Running, "{unusable:?}");
        }
    }

    #[test]
    fn a_sibling_sharing_the_cache_name_prefix_is_not_the_cache() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let running = touch(
            &home
                .join(".cvc")
                .join("mcp-cache-old")
                .join("v0.6.0-x86_64-linux")
                .join(format!("cvc{}", env::consts::EXE_SUFFIX)),
        );
        touch(&stable_binary(home));

        let resolved = resolve_hook_binary(&running, Some(home), OWN, no_probe);
        assert_eq!(resolved.path, running);
        assert_eq!(resolved.choice, BinaryChoice::Running);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_home_still_matches_the_cache_and_the_stable_binary() {
        let temp = TempDir::new().unwrap();
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // The process reports the canonical location; the home variable
        // carries the symlinked spelling.
        let running = touch(&cache_exe(&real));
        touch(&stable_binary(&real));

        // The probe is handed the home spelling too.
        let probe = |binary: &Path| {
            assert_eq!(binary, stable_binary(&link));
            Some(OWN.to_owned())
        };
        let resolved = resolve_hook_binary(&running, Some(&link), OWN, probe);
        assert_eq!(resolved.path, stable_binary(&link));
        assert_eq!(resolved.choice, BinaryChoice::StableInsteadOfCache);

        let resolved = resolve_hook_binary(&stable_binary(&real), Some(&link), OWN, no_probe);
        assert_eq!(resolved.path, stable_binary(&link));
        assert_eq!(resolved.choice, BinaryChoice::Stable);
    }

    /// A 0.7.0-era install recorded the launcher's cache path. Re-running
    /// the install after the launcher published the stable binary rewrites
    /// every entry in place, and then stays idempotent.
    #[test]
    fn reinstall_rewrites_a_cache_path_entry_to_the_stable_path() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let repo_dir = temp.path().join("repo");
        fs::create_dir_all(&repo_dir).unwrap();
        git2::Repository::init(&repo_dir).unwrap();
        let layout = RepositoryLayout::discover(&repo_dir).unwrap();
        let running = touch(&cache_exe(&home));

        let first = settings::install(&layout, &running).unwrap();
        assert_eq!(first.action, InstallAction::Created);
        assert!(first.command.contains(LAUNCHER_CACHE_DIR));

        // Before the launcher publishes, the cache copy is all there is.
        let unpublished = resolve_hook_binary(&running, Some(&home), OWN, no_probe);
        assert!(matches!(
            unpublished.choice,
            BinaryChoice::CacheCopyOnly { .. }
        ));
        let same = settings::install(&layout, &unpublished.path).unwrap();
        assert_eq!(same.action, InstallAction::AlreadyPresent);

        let stable = touch(&stable_binary(&home));
        let resolved = resolve_hook_binary(&running, Some(&home), OWN, reporting(OWN));
        assert_eq!(resolved.path, stable);
        let second = settings::install(&layout, &resolved.path).unwrap();
        assert_eq!(second.action, InstallAction::Updated);
        assert!(second.command.contains(stable.to_str().unwrap()));
        let rendered = fs::read_to_string(&second.settings_path).unwrap();
        assert!(rendered.contains(".cvc"), "{rendered}");
        assert!(!rendered.contains(LAUNCHER_CACHE_DIR), "{rendered}");

        let third = settings::install(&layout, &resolved.path).unwrap();
        assert_eq!(third.action, InstallAction::AlreadyPresent);
    }

    #[test]
    fn versions_follow_semver_precedence() {
        // The ordering chain from the SemVer 2.0.0 specification.
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
        ];
        for (index, lower) in chain.iter().enumerate() {
            for higher in &chain[index + 1..] {
                let (lower_v, higher_v) = (
                    Version::parse(lower).unwrap(),
                    Version::parse(higher).unwrap(),
                );
                assert!(lower_v < higher_v, "{lower} < {higher}");
                assert!(higher_v > lower_v, "{higher} > {lower}");
            }
        }
        // Numeric, not textual: 0.10.0 is newer than 0.9.0.
        assert!(Version::parse("0.9.0") < Version::parse("0.10.0"));
        // Build metadata is ignored.
        assert_eq!(Version::parse("1.2.3+a"), Version::parse("1.2.3+b"));
        assert_eq!(Version::parse("1.2.3+a"), Version::parse("1.2.3"));
        assert_eq!(
            Version::parse("1.2.3-rc.1+a")
                .unwrap()
                .cmp(&Version::parse("1.2.3-rc.1").unwrap()),
            Ordering::Equal
        );
    }

    #[test]
    fn malformed_versions_are_rejected() {
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "v1.2.3",
            "1.2.x",
            "-1.2.3",
            "1.2.3-",
            "1.2.3-.",
            "1.2.3-rc..1",
            "1.2.3-01",
            "1.2.3-rc_1",
            "1.2.3 ",
            "99999999999999999999.0.0",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert!(Version::parse("0.0.0").is_some());
        assert!(Version::parse("1.2.3-0").is_some());
        assert!(Version::parse("1.2.3-x-y.0a").is_some());
    }

    #[test]
    fn version_lines_are_parsed_from_cvc_output() {
        let parse = parse_version_line;
        assert_eq!(parse("cvc 0.7.0"), Some("0.7.0".to_owned()));
        assert_eq!(parse("cvc 0.7.0\n"), Some("0.7.0".to_owned()));
        assert_eq!(parse("  cvc   0.6.0  "), Some("0.6.0".to_owned()));
        assert_eq!(
            parse("cvc 0.7.1 (abc123 2026-10-17)"),
            Some("0.7.1".to_owned())
        );
        assert_eq!(parse("cvc 0.8.0-rc.1"), Some("0.8.0-rc.1".to_owned()));
        // Build metadata is stripped from what is returned.
        assert_eq!(parse("cvc 0.8.0+g1a2b3c"), Some("0.8.0".to_owned()));
        assert_eq!(
            parse("cvc 0.8.0-rc.1+build.5 extra"),
            Some("0.8.0-rc.1".to_owned())
        );
        for bad in [
            "",
            "cvc",
            "cvc ",
            "cvc-cli 0.7.0",
            "cvcc 0.7.0",
            "0.7.0",
            "other 0.7.0",
            "cvc 0.7",
            "cvc v0.7.0",
            "cvc unknown",
            "error: unrecognized option '--version'",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn output_is_read_from_the_stream_the_exit_code_implies() {
        let v = version_from_output;
        // Current releases: stdout, exit 0.
        assert_eq!(v(Some(0), "cvc 0.7.0\n", ""), Some("0.7.0".to_owned()));
        assert_eq!(
            v(Some(0), "cvc 0.7.0\nmore\n", "noise"),
            Some("0.7.0".to_owned())
        );
        // Releases up to v0.6.0: stderr, exit 1, nothing on stdout.
        assert_eq!(v(Some(1), "", "cvc 0.6.0\n"), Some("0.6.0".to_owned()));
        assert_eq!(v(Some(1), "  \n", "cvc 0.6.0\n"), Some("0.6.0".to_owned()));
        // Anything else is not a version report.
        assert_eq!(v(Some(1), "cvc 0.6.0\n", ""), None);
        assert_eq!(v(Some(1), "", "error: something else\n"), None);
        assert_eq!(v(Some(2), "cvc 0.7.0\n", "cvc 0.7.0\n"), None);
        assert_eq!(v(None, "cvc 0.7.0\n", ""), None);
        assert_eq!(v(Some(0), "", "cvc 0.7.0\n"), None);
        assert_eq!(v(Some(0), "garbage\n", ""), None);
        assert_eq!(v(Some(0), "", ""), None);
    }

    #[cfg(unix)]
    mod probe {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Mutex;

        /// A script is written and executed by the same process, so no other
        /// test may fork between the write and the exec (`ETXTBSY`).
        static SPAWNING: Mutex<()> = Mutex::new(());

        const SHORT: Duration = Duration::from_millis(400);

        fn run(body: &str, deadline: Duration) -> (Option<String>, Duration) {
            let _serial = SPAWNING.lock().unwrap_or_else(|error| error.into_inner());
            let temp = TempDir::new().unwrap();
            let script = temp.path().join("cvc");
            fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            let started = Instant::now();
            let reported = probe_version_within(&script, deadline);
            (reported, started.elapsed())
        }

        #[test]
        fn a_current_release_reports_on_stdout() {
            let (reported, _) = run("echo 'cvc 0.8.1'", PROBE_DEADLINE);
            assert_eq!(reported, Some("0.8.1".to_owned()));
        }

        #[test]
        fn a_legacy_release_reports_on_stderr_with_exit_one() {
            let (reported, _) = run("echo 'cvc 0.6.0' >&2; exit 1", PROBE_DEADLINE);
            assert_eq!(reported, Some("0.6.0".to_owned()));
        }

        #[test]
        fn failures_and_garbage_are_unknown() {
            for body in [
                "echo 'cvc 0.6.0'; exit 1",
                "echo \"error: unrecognized option '--version'\" >&2; exit 2",
                "echo hello",
                "exit 0",
                "kill -9 $$",
            ] {
                assert_eq!(run(body, PROBE_DEADLINE).0, None, "{body}");
            }
        }

        #[test]
        fn a_file_that_cannot_run_is_unknown() {
            let _serial = SPAWNING.lock().unwrap_or_else(|error| error.into_inner());
            let temp = TempDir::new().unwrap();
            assert_eq!(
                probe_version_within(&temp.path().join("missing"), SHORT),
                None
            );
            let plain = touch(&temp.path().join("not-executable"));
            fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(probe_version_within(&plain, SHORT), None);
        }

        #[test]
        fn a_hanging_binary_is_killed_at_the_deadline() {
            let (reported, elapsed) = run("exec sleep 30", SHORT);
            assert_eq!(reported, None);
            assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
        }

        #[test]
        fn chatty_output_neither_blocks_the_child_nor_the_probe() {
            // Far more than a pipe buffer on both streams, before and after
            // the version line; undrained, the child would block and time out.
            let (reported, elapsed) = run(
                "head -c 1000000 /dev/zero >&2; echo 'cvc 0.7.0'; head -c 1000000 /dev/zero",
                PROBE_DEADLINE,
            );
            assert_eq!(reported, Some("0.7.0".to_owned()));
            assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
        }

        #[test]
        fn a_background_process_holding_the_pipes_does_not_hold_the_probe() {
            let (reported, elapsed) = run("sleep 5 &\necho 'cvc 0.7.0'", PROBE_DEADLINE);
            assert_eq!(reported, Some("0.7.0".to_owned()));
            assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
        }
    }
}
