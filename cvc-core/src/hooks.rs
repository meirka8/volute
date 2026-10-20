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
    /// An older version of the managed block was replaced in place by the
    /// current one (everything around it untouched).
    Updated,
    AlreadyPresent,
    /// The hook file already exists and is not a shell script (a Python or Perl
    /// hook, a compiled program), so appending shell text to it would break it
    /// and, with it, the Git command that ran it. The file was not touched.
    SkippedNotShell,
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

/// The managed `post-checkout` hook, appended to (or written as) the hook file.
///
/// It exists to propagate the Claude Code harness into a new worktree, so it
/// stays out of the way of every other checkout: Git runs `post-checkout` as
/// `<previous HEAD> <new HEAD> <branch flag>`, and only a checkout that starts
/// from the null ref with the flag set to 1 (`git worktree add`, `git clone`)
/// is relevant. The `case` filters on exactly that without starting `cvc`, so
/// `git checkout`/`git switch` and file checkouts pay for a shell pattern
/// match and nothing else. The binary it runs narrows this further: it acts
/// only inside a linked worktree.
///
/// It runs the binary named in the repository's harness record
/// (`<common dir>/cvc/harness/claude-code.path`, see
/// [`crate::claude_code::harness_record`]) and nothing else: no `PATH` lookup,
/// which can find a stale `cvc` or a launcher that downloads on every run, and
/// no `$HOME`, which a GUI-started Git may not have. A record that exists but
/// cannot be used (a symlink or a symlinked directory, unreadable, empty, not
/// an absolute path), and a binary that is missing or not an executable file,
/// are each reported on exactly one line, with echoed paths cut to 200
/// characters; only the absence of a record is silent. It uses only shell
/// builtins besides `git` itself, works under `set -u`, and ends by restoring
/// the status the script had when it started, because the hook's exit status
/// becomes the exit status of `git checkout`, `git switch` and
/// `git worktree add`: it must not turn a user hook's success into failure,
/// nor hide its failure.
///
/// # What the shell block does not check
///
/// It cannot check the record's owner or mode portably, and does not try. The
/// ownership and permission checks live in the Rust half
/// ([`crate::claude_code::harness_record::read_record`]) and govern what is
/// *written into settings*, not which program this block starts: it starts the
/// recorded binary first. The trust boundary is write access to the common Git
/// directory, which also lets anyone edit `hooks/post-checkout` directly.
///
/// # Markers and versions
///
/// The block is wrapped in a begin line that carries its version and an end
/// line. Installing is idempotent on the markers, and an installed block with a
/// different version is replaced in place (never appended a second time), so a
/// later change to the block upgrades it where it stands, with the user's
/// content above and below untouched. Bump the version when the block changes.
pub const POST_CHECKOUT_HOOK: &str = r#"
# CVC Hook: post-checkout begin (v1)
cvc_status=$?
case "${3:-}:${1:-}" in
1:*[!0]*|1:) ;;
1:*)
    cvc_common=$(git rev-parse --git-common-dir 2>/dev/null) || cvc_common=
    cvc_dir="$cvc_common/cvc/harness"
    cvc_record="$cvc_dir/claude-code.path"
    cvc_bin=
    cvc_state=none
    if [ -n "$cvc_common" ]; then
        if [ -L "$cvc_dir" ] || [ -L "$cvc_record" ]; then
            cvc_state=bad
        elif [ -e "$cvc_dir" ] && [ ! -d "$cvc_dir" ]; then
            cvc_state=bad
        elif [ -e "$cvc_record" ]; then
            cvc_state=bad
            if [ -f "$cvc_record" ] && [ -r "$cvc_record" ]; then
                IFS= read -r cvc_bin 2>/dev/null < "$cvc_record" || :
                case "$cvc_bin" in
                /*|[A-Za-z]:/*|[A-Za-z]:\\*) cvc_state=run ;;
                esac
            fi
        fi
    fi
    case "$cvc_state" in
    bad)
        printf 'CVC: Claude Code hooks not installed in this worktree: the harness record %.200s is unusable; run `cvc harness install claude-code` here\n' "$cvc_record" >&2 || :
        ;;
    run)
        if [ ! -e "$cvc_bin" ]; then
            printf 'CVC: Claude Code hooks not installed in this worktree: %.200s does not exist; run `cvc harness install claude-code` here\n' "$cvc_bin" >&2 || :
        elif [ -f "$cvc_bin" ] && [ -x "$cvc_bin" ]; then
            "$cvc_bin" hook post-checkout "$@" || :
        else
            printf 'CVC: Claude Code hooks not installed in this worktree: %.200s is not an executable file; run `cvc harness install claude-code` here\n' "$cvc_bin" >&2 || :
        fi
        ;;
    esac
    ;;
esac
(exit "$cvc_status")
# CVC Hook: post-checkout end
"#;

/// The line that opens the managed `post-checkout` block, up to its version.
const POST_CHECKOUT_BEGIN: &str = "# CVC Hook: post-checkout begin";
/// The line that closes it.
const POST_CHECKOUT_END: &str = "# CVC Hook: post-checkout end";

const POST_COMMIT_HOOK: &str = "\n# CVC Hook\ncvc hook post-commit || :\n";
const PRE_PUSH_HOOK: &str = "\n# CVC Hook\ncvc hook pre-push \"$@\" || :\n";
const POST_MERGE_HOOK: &str = "\n# CVC Hook\ncvc hook post-merge \"$@\" || :\n";
const POST_REWRITE_HOOK: &str = "\n# CVC Hook\ncvc hook post-rewrite \"$@\" || :\n";

/// The hooks CVC has always managed: they observe commits, pushes, merges and
/// rewrites. `post-rewrite` is an internal companion that is not reported
/// unless it had to be skipped.
const CORE_HOOKS: [(&str, &str); 4] = [
    ("post-commit", POST_COMMIT_HOOK),
    ("pre-push", PRE_PUSH_HOOK),
    ("post-merge", POST_MERGE_HOOK),
    ("post-rewrite", POST_REWRITE_HOOK),
];

/// The line a hand-written, non-shell hook has to call for a managed hook, and
/// what that call enables; for callers that report [`HookAction::SkippedNotShell`].
pub fn manual_invocation(hook_name: &str) -> Option<(&'static str, &'static str)> {
    match hook_name {
        "post-commit" => Some(("cvc hook post-commit", "linking recent thoughts to commits")),
        "pre-push" => Some((
            "cvc hook pre-push \"$@\"",
            "push-range observation and automatic push",
        )),
        "post-merge" => Some((
            "cvc hook post-merge \"$@\"",
            "the post-merge pull and squash scan",
        )),
        "post-rewrite" => Some((
            "cvc hook post-rewrite \"$@\"",
            "relinking after amend and rebase",
        )),
        "post-checkout" => Some((
            "cvc hook post-checkout \"$@\"",
            "Claude Code capture hooks in new worktrees",
        )),
        _ => None,
    }
}

/// Installs the four long-standing hooks (`post-commit`, `pre-push`,
/// `post-merge`, `post-rewrite`). `post-checkout` is deliberately not among
/// them; see [`install_post_checkout`].
pub fn install(repo_root: &Path) -> Result<Vec<HookInstallOutcome>> {
    // Discovery gives us both common storage and the actual active worktree.
    // The latter matters because Git resolves a relative hooksPath from it.
    let layout = crate::repository::RepositoryLayout::discover(repo_root)
        .context("Failed to discover git repository")?;
    install_layout(&layout)
}

/// Installs the four long-standing hooks for an already validated layout
/// without rediscovering a caller-controlled pathname. This is what the MCP
/// and LSP servers call on startup, so it must keep doing exactly this:
/// `post-checkout` changes what happens when a worktree is created and is
/// installed only by the commands that mean it ([`install_all`],
/// [`install_post_checkout`]).
pub fn install_layout(
    layout: &crate::repository::RepositoryLayout,
) -> Result<Vec<HookInstallOutcome>> {
    let hooks_dir = hooks_dir(layout)?;
    CORE_HOOKS
        .iter()
        .filter_map(|(name, block)| install_hook(&hooks_dir, name, block).transpose())
        .collect()
}

/// What `cvc init` installs: the four long-standing hooks and `post-checkout`.
pub fn install_all(
    layout: &crate::repository::RepositoryLayout,
) -> Result<Vec<HookInstallOutcome>> {
    let hooks_dir = hooks_dir(layout)?;
    let mut outcomes: Vec<HookInstallOutcome> = CORE_HOOKS
        .iter()
        .filter_map(|(name, block)| install_hook(&hooks_dir, name, block).transpose())
        .collect::<Result<_>>()?;
    outcomes.push(install_post_checkout_in(&hooks_dir)?);
    Ok(outcomes)
}

/// Installs only the managed `post-checkout` hook, which `cvc harness install`
/// needs and which must not re-create any other hook the user removed.
pub fn install_post_checkout(
    layout: &crate::repository::RepositoryLayout,
) -> Result<HookInstallOutcome> {
    install_post_checkout_in(&hooks_dir(layout)?)
}

fn install_post_checkout_in(hooks_dir: &Path) -> Result<HookInstallOutcome> {
    Ok(
        install_hook(hooks_dir, "post-checkout", POST_CHECKOUT_HOOK)?
            .expect("post-checkout is always reported"),
    )
}

/// Git's effective hooks directory for this layout, created when missing.
fn hooks_dir(layout: &crate::repository::RepositoryLayout) -> Result<PathBuf> {
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
    Ok(hooks_dir)
}

/// Whether text appended to this hook file will be run by a POSIX-family
/// shell: it has no `#!` line (Git then runs it with `sh`), or its interpreter
/// is `sh`, `bash`, `dash`, `zsh`, `ksh`, `ash` or `mksh`, directly or through
/// `env`. Anything else (`python`, `perl`, `node`, `fish`, ...) would choke on
/// shell text, and Git returns the hook's status from `git checkout` and
/// `git worktree add`.
fn runs_under_a_posix_shell(content: &str) -> bool {
    let first = content.lines().next().unwrap_or("");
    let first = first.trim_start_matches('\u{feff}').trim_end_matches('\r');
    let Some(rest) = first.strip_prefix("#!") else {
        return true;
    };
    fn base(program: &str) -> &str {
        program.rsplit(['/', '\\']).next().unwrap_or(program)
    }
    let mut words = rest.split_whitespace();
    let Some(program) = words.next() else {
        return false;
    };
    let mut interpreter = base(program);
    if interpreter == "env" {
        // `env [-S] [NAME=value ...] interpreter [args]`
        match words.find(|word| !word.starts_with('-') && !word.contains('=')) {
            Some(word) => interpreter = base(word),
            None => return false,
        }
    }
    matches!(
        interpreter,
        "sh" | "bash" | "dash" | "zsh" | "ksh" | "ash" | "mksh"
    )
}

fn is_begin_marker(line: &str) -> bool {
    line.trim_end_matches(['\r', '\n'])
        .starts_with(POST_CHECKOUT_BEGIN)
}

/// Replaces the marked `post-checkout` block in `content` with `block` (which
/// starts with its begin marker and ends with its end marker): the first block
/// in place, any further ones dropped, everything else byte for byte. `None`
/// when `content` has no begin marker at all. A begin marker without its end
/// marker is an error rather than a guess about how much of the file is ours.
fn replace_marked_blocks(content: &str, block: &str) -> Result<Option<String>> {
    if !content.split_inclusive('\n').any(is_begin_marker) {
        return Ok(None);
    }
    let mut replaced = String::with_capacity(content.len() + block.len());
    let mut lines = content.split_inclusive('\n');
    let mut placed = false;
    while let Some(line) = lines.next() {
        if !is_begin_marker(line) {
            replaced.push_str(line);
            continue;
        }
        let closed = lines
            .by_ref()
            .any(|inner| inner.trim_end_matches(['\r', '\n']) == POST_CHECKOUT_END);
        if !closed {
            anyhow::bail!(
                "the post-checkout hook has a CVC begin marker without its end marker; repair or remove that block by hand"
            );
        }
        if !placed {
            replaced.push_str(block);
            replaced.push('\n');
            placed = true;
        }
    }
    Ok(Some(replaced))
}

/// Installs one managed hook into `hooks_dir`. `None` means the hook is not
/// reported (`post-rewrite` that went in without incident).
fn install_hook(
    hooks_dir: &Path,
    hook_name: &'static str,
    hook_cmd: &str,
) -> Result<Option<HookInstallOutcome>> {
    let hook_path = hooks_dir.join(hook_name);

    if fs::symlink_metadata(&hook_path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        anyhow::bail!("refusing to modify symlinked hook {:?}", hook_path);
    }

    let skipped = |hook_path: PathBuf| HookInstallOutcome {
        hook_name,
        hook_path,
        action: HookAction::SkippedNotShell,
    };

    let action = if hook_path.exists() {
        // A hook that is not text at all (a compiled program) is not a shell
        // script either.
        let Ok(content) = String::from_utf8(fs::read(&hook_path)?) else {
            return Ok(Some(skipped(hook_path)));
        };
        // Upgrade the old managed `cvc push` line in place.  Keeping it would
        // publish twice and can recurse through git's pre-push hook.
        let old = "# CVC Hook\ncvc push";
        let wanted = hook_cmd.trim();
        let legacy = hook_name != "post-checkout";
        // The marked block is recognized by its markers, whatever version it
        // is, and replaced in place when it is not the current one.
        let marked = if legacy {
            None
        } else {
            replace_marked_blocks(&content, wanted)?
        };
        if let Some(replaced) = marked {
            if replaced == content {
                HookAction::AlreadyPresent
            } else {
                fs::write(&hook_path, replaced)?;
                HookAction::Updated
            }
        } else if legacy && content.contains(old) {
            fs::write(&hook_path, content.replace(old, wanted))?;
            HookAction::Appended
        } else if legacy
            && content.contains("# CVC Hook")
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
            if !runs_under_a_posix_shell(&content) {
                // Leave it exactly as it is, mode included.
                return Ok(Some(skipped(hook_path)));
            }
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

    // post-rewrite is an internal companion hook, installed with the same
    // safety guarantees as the reported hooks but not reported.
    if hook_name == "post-rewrite" {
        return Ok(None);
    }
    Ok(Some(HookInstallOutcome {
        hook_name,
        hook_path,
        action,
    }))
}
