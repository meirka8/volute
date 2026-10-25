# CVC CLI

`cvc` manages a repository-local Cognitive Version Control cache. Captures are **private by default** in `$(git rev-parse --git-common-dir)/cvc/index.db`; privacy/consent state, locks, and rewrite state share that directory across linked worktrees. CVC refs such as `refs/cvc/main` are normal shared Git refs, while hooks use Git's effective hooks path (`<common-dir>/hooks` by default, or `core.hooksPath`; relative paths are active-worktree-relative). `.thoughtignore`, context, `HEAD`, index, and branch stay active-worktree-local. Sync is opt-in per destination and is not a substitute for a secret-management or deletion system.

## Installation

```bash
cargo install --path cvc-cli
```

## Basic commands

```bash
cvc init
cvc status
cvc log
cvc conversations
cvc run -- <command> [args...]
cvc pull
```

`conversations` lists conversations most-recent-first with thought and linked counts and, for the selected remote (`--remote <name>`, default `origin`), their share and publication state, so it answers "what would I be sharing?". In a terminal it opens an [interactive view](#browse-and-share-interactively) for browsing and, on Linux and macOS, for sharing, unsharing and publishing, each only through the same typed phrase as the matching command. With `--plain`, in a pipe, or without a terminal it prints a plain listing instead, which only reads and prints ids and titles with control and format characters replaced by `?`. `--limit <N>` caps how many conversations are listed; the plain listing shows 20 by default.

In the plain listing a conversation reads `[shared, …]` while some of its thoughts are shared for that remote. When nothing is shared but this clone's records show the remote holds some of it, it reads `[published, not shared here]`, and `[unconfirmed, not shared here]` when they show only an unfinished publish with nothing published; a re-ingested session and a conversation imported by `cvc pull` read this way, never as `[private]`. It reads `[shared, future only]` when future sharing is on but nothing is shared and the remote holds nothing of it, and `[private]` otherwise, so `[private]` means private as far as this clone knows: this clone shares nothing in it for that remote and has no record of the remote holding any of it. After `cvc unshare` it is `[private]` again, and the `cvc share` picker offers it again; the picker lists imported conversations too. The `published` figure counts only thoughts that this clone shares for that remote and knows the remote holds, so a conversation imported by `cvc pull` is not counted as shared or published from this clone, however many of its thoughts the remote holds.

`init` creates the local SQLite cache and installs advisory `post-commit`, `pre-push`, `post-merge`, `post-rewrite`, and `post-checkout` hooks (respecting `core.hooksPath`); the MCP and LSP servers install only the first four. The `post-checkout` hook starts a binary only when Git creates a worktree, and only if `cvc harness install` recorded one (see "Hooks in new worktrees" below); ordinary checkouts and switches do not start anything, with one exception: leaving an unborn (orphan) branch looks like a new worktree to Git, so it starts the recorded binary too (which acts only in a linked worktree and does nothing in the main one). A hook file that already exists and is not a shell script is left alone with a warning, for each of these hooks. `run` records its command and output as a private floating interaction. The post-commit linker may associate recent eligible interactions with a commit, but it does not share them. When it links, it prints one line naming the commit, `CVC: Linked N thought(s) to commit <sha>.`. It prints that line on every commit, whether or not anything was linked, in two more cases: in a checkout with the per-checkout Claude Code harness installed, and for any commit made from a Claude Code session (Claude Code sets `CLAUDECODE=1` for its shell commands) in a repository whose capture acknowledgement is granted, whether or not that session is being recorded there, which is how Claude Code plugin users get it. The count says how many thoughts were linked, and `0` is possible. Transcript ingestion reads the commit hash from this line (see below), so a quiet `git commit -q` still names its commit. Configure its conservative window with `git config cvc.linkWindow <seconds>` (`0..=2592000`, default `86400`; `0` disables automatic linking). Automatic linking uses only the documented time/file-context policy—not author, message, or file-set similarity heuristics; `CVC-Session` trailers are not implemented. Captures additionally record a local-only fingerprint of their active worktree, and automatic linking considers only thoughts captured in the committing worktree (legacy rows without a recorded origin remain broadly eligible); parallel linked worktrees cannot claim each other's floating thoughts. Thoughts captured in a worktree that is later removed stay floating rather than being linked elsewhere.

The optional pre-push range observer requires an explicit local target branch; it does not guess from a remote HEAD:

```bash
git config cvc.targetBranch refs/heads/main
```

It observes a single pushed local branch against that branch and may record exact range evidence within its advisory hook budget. The budget is cooperative: bounded traversal checks occur between operations, but one libgit2 call can exceed the nominal five seconds. Current exact evidence is SHA-1-only; unsupported object formats fail closed without creating a link.

## Exact derivation evidence and hooks

Record a range explicitly (Git revisions are resolved locally):

```bash
cvc relink observe-range <BASE> <TIP>
cvc relink observe-range <BASE> <TIP> --remote origin
```

With `--remote`, the command requires the displayed `I AUTHORIZE RANGE <base> <tip> <fingerprint>` TTY acknowledgement; this authorizes that range's source snapshots only for that destination, not sharing. A valid range has a unique merge base equal to `BASE`, `BASE` as a strict ancestor of `TIP`, at most 2,048 ordered members, and a canonical `cvc.changeset/v1` digest of the base-tree→tip-tree transition.

`post-rewrite` accepts only Git's exact `amend` (one old/new pair) and `rebase` pair stream. It validates and writes recoverable input to `$(git rev-parse --git-common-dir)/cvc/rewrite-inbox` before replaying it; permanent malformed entries are quarantined and retryable entries remain for later replay. It derives only from locally observed source provenance—never author/message/file-set heuristics—and does not guarantee a relink if the range, source evidence, or required Git objects are absent. `post-commit` scans its branch cursor; `post-merge` and `cvc pull` pull first and then run a longer pending squash scan. All hook failures are warnings and never fail the Git operation.

## Agent harness capture (Claude Code)

Claude Code writes every session to a local JSONL transcript and runs documented lifecycle hooks. CVC turns that into deterministic capture: the hooks call `cvc`, which reads the transcript incrementally and records each completed assistant response as a private thought, with no cooperation needed from the model. This is first-party local capture; it is unrelated to sync ingestion (`cvc pull`), which imports another machine's already-shared projection.

Two routes put the hooks in place, and both run the same `cvc ingest claude-code --hook`: the per-checkout install below, and a Claude Code plugin (unrelated to the VS Code extension in `cvc-plugin/`) from this repository's Claude Code plugin marketplace, described under "Claude Code plugin" below.

```bash
cvc harness install claude-code
cvc harness uninstall claude-code
```

`install` writes `PostToolUse`, `Stop`, and `SessionEnd` hook entries into this checkout's `.claude/settings.local.json`, using the absolute path of the running `cvc` binary (override with `--binary <absolute path>`), and ensures that file is excluded from Git through the repository's `info/exclude`: a machine-specific path must never be committed. The recorded path is the binary the hooks run, so it should be one that an upgrade replaces in place. The installer scripts and the npm launcher both publish to `~/.cvc/bin`, and `install` steers there: when the running `cvc` is a copy in the launcher's versioned cache (`~/.cvc/mcp-cache/v<version>-<arch>-<os>/`) and `~/.cvc/bin/cvc` exists and reports a version at least as new as the running `cvc`, it records `~/.cvc/bin/cvc` instead and says so. If that file does not exist, reports an older version, or reports none, it keeps the cache path and warns on stderr, because the hooks would then keep running that release after an upgrade; run `install` again once the launcher has published this release to `~/.cvc/bin`. `--binary` overrides all of this. Upgrading from v0.7.0: run `cvc harness install claude-code` once more in the checkout. It rewrites hooks that v0.7.0 recorded through the launcher (the cache path) to `~/.cvc/bin/cvc`, and it turns on the new-worktree behavior described below; one run does both. It requires an initialized repository and the capture acknowledgement described below, and refuses to install hooks that would only report `consent-required`. Claude Code picks the entries up when it next reloads its settings; if a running session does not, start a new session.

### Hooks in new worktrees

The settings file is per checkout, but the install is per repository. Besides this checkout's entries, `install` does two things so that worktrees created later get the same hooks:

- It writes a record of the binary to `$(git rev-parse --git-common-dir)/cvc/harness/claude-code.path`: one absolute path and a newline, the same binary the entries run. Every worktree shares it. It is written atomically with mode `0600`. `install` will not write through a symlink, and fails its record step (with a warning, leaving the rest of the install in place) if the `harness` directory already exists and is owned by someone else or writable by group or others. The Git hook starts nothing from a record that is a symlink, or that sits in a symlinked `harness` directory, and says so on one line. `cvc hook post-checkout` also installs nothing from a record owned by someone else or, on Unix, writable by group or others. Those owner and mode checks govern what is written into settings, not which program the Git hook starts: the shell block cannot check them portably and starts the recorded binary first. The trust boundary is write access to the common Git directory, which also lets anyone edit `hooks/post-checkout` itself.
- It adds CVC's block to Git's `post-checkout` hook, creating the file if needed, and says so when it does. It never recreates any other hook you removed. `cvc init` installs the same block; with no record it does nothing. The block is wrapped in `# CVC Hook: post-checkout begin (v1)` and `# CVC Hook: post-checkout end` lines. Running `install` or `init` again leaves a current block alone and replaces an older version in place, with your own content above and below untouched; it never adds a second copy.

`install` refuses a `--binary` that is not an absolute path to an executable file, and records nothing then. It also refuses, exits 1 and changes nothing in a checkout whose `.claude/settings.local.json` Git tracks, or whose `.claude` is a symlink or a file.

When Git creates a worktree with a checkout, the hook reads the record and runs the binary it names. It never looks `cvc` up on `PATH` or in `~/.cvc/bin`. That run (`cvc hook post-checkout`) installs the same entries into the new worktree and prints `CVC: installed the Claude Code capture hooks in this worktree (.claude/settings.local.json)`.

- It acts for `git worktree add`, which is what Claude Code's worktree option and the desktop app's worktree box run. A `--no-checkout` worktree gets nothing, because Git skips the hook.
- It acts only in a linked worktree, never in the main worktree (where you normally run `install`).
- Ordinary `git checkout` and `git switch` runs start nothing.
- Git takes its own exit status for `git worktree add`, `git checkout` and `git switch` from the hook. The block always succeeds and keeps the status of any hook of yours it was appended to.
- It only ever creates `.claude/settings.local.json` in the new worktree; it never merges into one. If the file already exists there, for any reason, it installs nothing and says so. To add the hooks to such a worktree, run `cvc harness install claude-code` inside it.

It installs nothing, and prints one line to stderr, when:

- the recorded binary is gone or not an executable file (``CVC: Claude Code hooks not installed in this worktree: <binary> does not exist; run `cvc harness install claude-code` here``);
- the record exists but is unusable: a symlink, in a symlinked `harness` directory, unreadable, empty, or not an absolute path (a Windows drive-relative or UNC path counts) (``CVC: Claude Code hooks not installed in this worktree: the harness record <record path> is unusable; run `cvc harness install claude-code` here``). Only a missing record is silent, because then the harness was never installed;
- the capture acknowledgement is not granted;
- Git tracks `.claude/settings.local.json` in that checkout. CVC writes a machine-specific path there and will not edit a tracked file; untrack it with `git rm --cached .claude/settings.local.json`. Other tracked `.claude` files, such as `settings.json` or `agents/`, do not matter;
- `.claude/settings.local.json` already exists in that worktree (``CVC: Claude Code hooks not installed in this worktree: `.claude/settings.local.json` already exists in this worktree; CVC only creates it in a new worktree (run `cvc harness install claude-code` here to add the hooks)``);
- `.claude` in that worktree is a symlink or a file (a branch can commit one). CVC never follows it, and says so.

When the running `cvc` is the launcher's versioned cache copy and no stable `~/.cvc/bin/cvc` can take its place, `install` updates this checkout but does not write the record. Every worktree created from it would inherit a path that goes stale on upgrade. What it prints depends on whether an earlier install left a usable record. With one, new worktrees keep getting hooks that run the earlier binary: ``New worktrees keep getting hooks that run <recorded binary> (recorded earlier); this install was not recorded because it ran from the launcher's versioned cache.`` Without one: ``New worktrees will not get these hooks until you run `cvc harness install claude-code` again with a current cvc in ~/.cvc/bin.`` `--binary` overrides this and is recorded as given. A repository with no record gets no settings file in new worktrees.

Limits:

- Worktrees that already exist are not touched. `install` lists the ones without the hooks (up to ten, then a count), so you can run it in each once.
- A new worktree captures under its own checked-out `.thoughtignore`. A policy that was never committed does not carry over from the checkout you installed in.
- A switch away from an unborn (orphan) branch reaches the hook with the same arguments as a new worktree, so the shell block starts the recorded binary then too. In the main worktree the binary does nothing. Inside a linked worktree that has no settings file it installs the hooks there too, when a record exists; one that already has a settings file gets the "already exists" line and is left alone.
- A hook of yours that is not a shell script (Python, Perl, a compiled program) is left untouched, for every managed hook, and new worktrees are not followed until you make the call yourself. `harness install` knows the binary and prints ``warning: <path> is not a shell script; CVC did not modify it. Call `'<abs binary>' hook post-checkout "$@"` from it yourself (ignore its exit status) to enable Claude Code capture hooks in new worktrees.`` using the absolute path it recorded, and then prints `This install is per checkout: run it again in each linked worktree you use.` Use that absolute path, not a bare `cvc`, which depends on `PATH` and may be a stale release; in a Python hook, run it with `subprocess` and the hook's own arguments (`sys.argv[1:]`) and ignore the result. `cvc init` has no record to take the path from and prints the same warning with `cvc hook post-checkout "$@"` and a pointer to this section.
- A clone runs a `post-checkout` hook only if one is templated into it (`init.templateDir`). CVC does not do that, and a clone has no record to follow.

`uninstall` removes this checkout's entries and clears the record, so new worktrees stop getting the hooks (`New worktrees will no longer get the Claude Code hooks.`). It does not withdraw the capture acknowledgement. Other worktrees keep their own entries and keep capturing until you run `uninstall` in each. It lists every one that still carries them, with no cap, and says so for any worktree it could not check, any it could not list, and a main worktree it could not locate. Hooks that remain without an acknowledgement fail with `consent-required` and record nothing. The `post-checkout` Git hook stays; with no record it starts no `cvc`.

### Claude Code plugin

This repository is also a Claude Code plugin marketplace named `volute` (`.claude-plugin/marketplace.json`) with one Claude Code plugin, `cvc`, whose files are in `plugins/claude-code/`. The marketplace entry pins the plugin to the CVC release tag it names (`ref`, for example `v0.7.1`). The plugin's files come only from the release tag. The catalog entry that names that tag is read from `main`, and CI requires it to name this repository, this path and the tag of the current version, with no other fields. Installing it puts the capture hooks in place for every Claude Code session you start, without running `cvc harness install claude-code` in each checkout or worktree. Inside Claude Code:

```text
/plugin marketplace add meirka8/volute
/plugin install cvc@volute
```

The shell equivalents are `claude plugin marketplace add meirka8/volute` and `claude plugin install cvc@volute`. The default install scope is user, so the hooks run in every Claude Code session you start, in every project and every worktree: in the terminal, the desktop app's Code tab and the IDE extensions, but, per Claude Code's documentation, not in cloud sessions. A session is recorded in the repository it was started in (see "Ingestion" below). Start a new session after installing.

Requirements:

- The `cvc` CLI, 0.7.1 or later, in an absolute directory on the `PATH` Claude Code sees. The installer scripts and the npm launcher put `cvc` in `~/.cvc/bin`. The desktop app and IDE-launched sessions inherit their environment from how they were started, which can differ from your terminal's. The plugin's hooks look for `cvc` only in absolute directories on that `PATH`. Relative entries (empty, `.`, `bin`, a literal `~`) are skipped, so a project cannot supply `cvc` through them. Absolute directories on your `PATH` are trusted, including ones your tools add for a project, such as direnv, an activated virtualenv or `node_modules/.bin`.
- If you installed CVC with npm: the packages `@volute_cvc/cvc` (the name for CLI users) and `@volute_cvc/cvc-mcp` both provide `cvc`, `cvc-mcp` and `cvc-lsp`, and the `cvc` they put on `PATH` is a Node launcher. Each time it runs, it checks `~/.cvc/bin/cvc --version`. If that build is older than the package, it downloads the matching release, verifies it, caches it under `~/.cvc/mcp-cache/` and publishes it to `~/.cvc/bin` (never downgrading). Then it runs the binary. For the plugin, put `~/.cvc/bin` on Claude Code's `PATH` ahead of npm's global bin directory, and run `cvc --version` once after each npm install or upgrade. Otherwise every hook event starts the Node launcher first, which adds latency in every project, and the first event after an install or upgrade may download a release (network access and writes under `~/.cvc`) within the hook's 30 second budget.
- In each repository you want captured, `cvc init` and `cvc privacy acknowledge-capture`, exactly as for the per-checkout install. The plugin's hooks never run the acknowledgement, share or publish commands, and installing it changes no consent state. Those commands accept their phrase only at an interactive terminal, which is a confirmation step, not a security boundary (see [Privacy.md](../Privacy.md)). In a repository where capture was already acknowledged (for the VS Code extension or a per-checkout install), the plugin starts recording Claude Code sessions from the next session, with no new prompt.

`PostToolUse`, `Stop` and `SessionEnd` each run `cvc ingest claude-code --hook` with a 30 second timeout: the same three events and the same command that `cvc harness install claude-code` writes. The plugin runs that command through a small shell wrapper, `/bin/sh "${CLAUDE_PLUGIN_ROOT}/scripts/run-cvc.sh" ingest claude-code --hook` (the script is `plugins/claude-code/scripts/run-cvc.sh`), followed by the guard `; s=$?; [ "$s" -ne 2 ] || s=1; exit "$s"`, which turns exit status 2 into 1, because Claude Code treats 2 as blocking. The wrapper finds `cvc` only in absolute directories on the `PATH` Claude Code runs hooks with. Relative entries (empty, `.`, `bin`, a literal `~`) are skipped, so a project cannot supply `cvc` through them. Absolute directories on your `PATH` are trusted, including ones your tools add for a project, such as direnv, an activated virtualenv or `node_modules/.bin`. Because `/bin/sh` is an absolute path, a project cannot supply the shell either. The per-checkout entries record an absolute path instead.

A `SessionStart` hook, on startup and on resume, runs a check: `/bin/sh "${CLAUDE_PLUGIN_ROOT}/scripts/check-cvc.sh"` (the script is `plugins/claude-code/scripts/check-cvc.sh`). It is designed not to block the session (it always exits 0). It shows you one line, as a message to you that, per Claude Code's documentation, is not added to Claude's context, in four cases. If `cvc` is not on the `PATH`:

```text
CVC: the cvc command is not on the PATH Claude Code runs hooks with, so the cvc plugin cannot capture this session. Install CVC (https://github.com/meirka8/volute#install-a-release) or add its directory (by default ~/.cvc/bin) to that PATH, then start a new session.
```

That one message is not the only sign. While `cvc` is missing from that `PATH`, every `PostToolUse`, `Stop` and `SessionEnd` event also shows a non-blocking hook error, in every project: `cvc: command not found in an absolute PATH directory (the cvc plugin does not use relative PATH entries)`. Fix the `PATH`, or disable the plugin with `/plugin disable cvc@volute`.

If the only `cvc` on the `PATH` is in a relative entry, which the plugin does not use:

```text
CVC: the only cvc on the PATH Claude Code runs hooks with is in a relative PATH entry, which the cvc plugin does not use, so it cannot capture this session. Put cvc in an absolute directory on that PATH (by default ~/.cvc/bin), then start a new session.
```

The hook error above appears in that case too, until `cvc` is in an absolute directory or the plugin is disabled.

If a `cvc` is found on the `PATH` but cannot be run:

```text
CVC: the cvc found on the PATH Claude Code runs hooks with could not be run (for example an npm-installed cvc whose node is not on that PATH), so the cvc plugin cannot capture this session. Check that cvc --version works with that PATH, then start a new session.
```

And if the `cvc` on the `PATH` is older than the plugin needs:

```text
CVC: the cvc on PATH is older than the cvc plugin needs (0.7.1 or later), so its hooks report errors in projects without CVC. Upgrade CVC (https://github.com/meirka8/volute#install-a-release), then start a new session.
```

The check tests for the behavior the hooks depend on rather than comparing version numbers. It pipes a payload whose working directory and project directory do not exist into `cvc ingest claude-code --hook`: 0.7.1 and later exit 0 silently there, and an older `cvc` rejects the input, which names no session or transcript, so it records nothing either. In every other case the check prints nothing.

Which route to choose:

- **Claude Code plugin**: you use Claude Code across many repositories or worktrees (desktop worktree sessions included) and want one install per machine, with updates through `/plugin`. It requires `cvc` in an absolute directory on the `PATH` Claude Code sees.
- **`cvc harness install claude-code`**: you want capture confined to specific checkouts; you cannot or do not want to put `cvc` in an absolute directory on that `PATH`, because the entries it writes pin an absolute binary path; or your organization restricts Claude Code plugins and marketplaces.

Installing both is supported. Per Claude Code's documentation, Claude Code runs a plugin's hooks alongside the hooks in settings files, in parallel and without de-duplicating them. So in a checkout whose `.claude/settings.local.json` carries CVC's entries, both hooks run for each event. CVC serializes ingestion of a session with a per-session lock in the repository's CVC directory. The second run finds nothing new, because ids are deterministic and the cursor has advanced. Ingestion is idempotent: thought ids are derived from the session and response, so a second run over the same event adds no duplicate thoughts. The cost is one more short `cvc` process per event.

Per Claude Code's documentation, Claude Code does not auto-update third-party (non-Anthropic) marketplaces by default. Update with `/plugin marketplace update volute` (or turn on auto-update for the marketplace yourself); the marketplace pins the plugin to the CVC release tag, so you receive plugin changes with each CVC release. Stop with `/plugin disable cvc@volute` or `/plugin uninstall cvc@volute`. Neither withdraws the capture acknowledgement, and the `cvc privacy` controls described below apply to what was already captured.

Windows is untested for the Claude Code path (public issue [#97](https://github.com/meirka8/volute/issues/97)). The plugin's hooks are all POSIX `sh` scripts, which Claude Code runs through Git Bash on Windows. Without Git Bash (Claude Code's PowerShell fallback), the hooks report non-blocking errors and nothing is captured.

### Ingestion

Each hook runs `cvc ingest claude-code --hook`, which reads Claude Code's JSON payload from stdin, finds the session's repository (see below), and ingests the transcript from the session's stored cursor.

A session is recorded in the repository it was started in: the repository that contains Claude Code's project directory (`CLAUDE_PROJECT_DIR`, which Claude Code gives every hook); any of its worktrees counts, and so does any subdirectory. The session is recorded there in full, including work it does in other directories (`/add-dir` folders, other repositories it moves into) and its end, wherever its working directory is by then. The hook's working directory only decides which of that repository's worktrees a response is attributed to. A session started outside an initialized repository (in a parent folder, or in another project) is not recorded, even when it later works inside one. Another repository that a session visits receives nothing. If Claude Code provides no `CLAUDE_PROJECT_DIR` (older Claude Code versions, or a manual run), the hook's working directory decides; if it is set but empty, nothing is recorded. This is the same scope the per-checkout install has, since Claude Code loads a checkout's settings only for sessions started there. Open Claude Code in the repository, or one of its worktrees, rather than in a folder above it.

Hook runs are silent on success. They are also silent when the session was not started in a CVC-initialized repository: Claude Code's project directory is not in a Git repository, is in a repository where `cvc init` was never run, is in a bare repository, no longer exists, or is in a repository owned by another user (Git itself refuses those). There `cvc ingest claude-code --hook` exits 0 and prints nothing, which keeps a user-scoped Claude Code plugin quiet in unrelated projects. Each hook event in such a session still starts a short `cvc` process. Claude Code passes it the hook's JSON input on stdin; for `PostToolUse` that input includes the tool's arguments and output, which can contain file contents and command output from that project. `cvc` reads up to 1 MiB of it into memory to find the working directory, then checks whether the session was started in a CVC-initialized repository. When it was not, it exits 0 and prints nothing: it reads no transcript, opens no CVC database, writes no file and uses no network, and the input is discarded when it exits. A `.git` entry that Git cannot open (for example a dangling gitfile) still reports an error that names the path, without blocking the session. An initialized repository without the capture acknowledgement still fails visibly with `consent-required: …`, a message that ends "…(or stop the hooks: `/plugin disable cvc@volute` for the Claude Code plugin, `cvc harness uninstall claude-code` for this checkout's hooks)". Other failures in an initialized repository still surface; the status is never the blocking status 2.

Hooks from the plugin and from a per-checkout install can both run for the same event, in parallel. CVC serializes ingestion of a session with a per-session lock in the repository's CVC directory, so the second run finds nothing new: ids are deterministic and the cursor has advanced. If a lock wait times out in hook mode, that run exits quietly and the next event catches up. On a filesystem that cannot take the per-session lock (some network filesystems), CVC proceeds without it, since ingestion is idempotent.

A hook run has the hook's 30 second budget, and a run commits nothing unless it finishes. A very large transcript, roughly tens of megabytes on first ingestion (for example when you resume an old, long session after installing the plugin), may not finish within that budget, so the hooks do not record it. Record it afterwards with `cvc ingest claude-code --transcript <file>`.

A transcript this version does not understand (another major version, or an entry whose shape is unknown) fails loudly with a non-zero status that Claude Code shows in the session without blocking it; CVC never returns the blocking status, and a failed run writes nothing. A content block whose type is merely unfamiliar, which Claude Code adds within a major version (its `fallback` model-switch marker, for example), does not fail the session: the block is kept as a bracketed placeholder naming its type, never its content, and `cvc ingest claude-code` reports how many such blocks it kept and their types. `PostToolUse` keeps ingestion near-real-time so a commit made mid-session can still link the reasoning that preceded it; `Stop` and `SessionEnd` close the final response of a turn or session. When a response runs `git commit`, ingest reads the commit's hash from the command's recorded output in four exact shapes: git's own `[branch sha]` summary line, the `CVC: Linked … to commit <sha>.` line the post-commit hook prints into the same output (which is how a quiet `git commit -q` still names its commit), a hash at the start of a line as `git log --oneline` or `git rev-parse --short HEAD` print, and a full 40-character hash. Every candidate must resolve to a commit in this repository whose time is consistent with the response, so output that merely mentions older commits links nothing. The verified commit then links that response and the reasoning before it directly, since the time-window linker cannot reach a response written before its commit existed. Subagent (Agent tool) transcripts stored beside the session are ingested in the same pass, each as its own conversation named `<session>:agent-<id>`; subagent hook invocations themselves are ignored because the parent session's hooks cover them.

Record a finished session, or one that ran before the hooks were installed, explicitly:

```bash
cvc ingest claude-code --transcript ~/.claude/projects/<workspace>/<session>.jsonl
```

Ingestion is idempotent: every assistant response has a deterministic id within its session, so re-running never duplicates or replaces a thought, and never disturbs commit links it has already earned. One thought is recorded per assistant response: the prompt or tool results it reacted to become the prompt, its exposed thinking becomes `model_cot`, its text becomes the response, and its tool calls become tool executions carrying the status of their results. Paths the session read or edited inside the worktree become file context, subject to `.thoughtignore` `path:` rules; tool output is rendered into the following thought's prompt with bounded size. The conversation id is the Claude Code session id, so `cvc conversations` and `cvc share` work on it directly, and its title follows Claude Code's own: a title you set yourself outranks the generated one, and the first prompt stands in until either exists. Captures follow the usual boundaries: private by default, scrubbed on the way in, shared only per conversation and destination (a session's thoughts include the prompts it gave its agents and the results they returned, so sharing a session shares those, while each agent's own conversation is shared separately), and attributed to the worktree the hook ran in, so parallel checkouts cannot claim each other's thoughts.

## Privacy acknowledgement and destination consent

Inspect the local status for the selected remote (or the default remote):

```bash
cvc privacy status --remote origin
```

Passive collection (VS Code chat-session watching and Claude Code transcript ingestion, including its hook-driven form) is disabled until the repository owner completes:

```bash
cvc privacy acknowledge-capture
```

This requires an interactive TTY and typing exactly `I UNDERSTAND LOCAL CAPTURE`. The acknowledgement remains local and is never synced. It is per repository, and neither way of installing the Claude Code hooks grants it: `cvc harness install claude-code` refuses to run without it, and the Claude Code plugin's hooks never run the acknowledgement, share or publish commands, and installing it changes no consent state. Those commands accept their phrase only at an interactive terminal, which is a confirmation step, not a security boundary (see [Privacy.md](../Privacy.md) and the paragraph on non-interactive input below). Hooks in a repository that has not granted it fail visibly with `consent-required` and record nothing.

Before any remote publication, separately acknowledge the *effective push URL* for a remote:

```bash
cvc privacy acknowledge-sharing --remote origin
```

The CLI displays that destination's fingerprint and requires the exact interactive `I AUTHORIZE SHARING <fingerprint>` challenge. Consent is tied to the destination fingerprint; changing a remote/push URL requires a new acknowledgement. Sharing consent does not enable auto-push.

Auto-push is off by default and also needs a TTY challenge for that same destination:

```bash
cvc privacy set-auto-push on --remote origin
cvc privacy set-auto-push off --remote origin
```

Non-interactive input is rejected for acknowledgements. This is meant to stop scripts, IDEs, and MCP clients from granting capture, sharing, or auto-push consent through piped or queued input. It is a confirmation step, not a security boundary: a program that drives a terminal as you, such as an AI agent with terminal access, can still type a phrase. On Linux and macOS the answer is read from the controlling terminal itself, and anything already queued on it (a paste, type-ahead, or a line left over from a preceding challenge in the same command) is discarded when the prompt appears, so a challenge can only be satisfied by input typed after it was displayed. The prompt also waits until input has been quiet for about 250 ms, which is the short pause you may see before it appears, and if input keeps arriving for about 3 seconds no phrase is asked and the challenge is not confirmed (`input kept arriving at the terminal, so no phrase was asked: acknowledgement challenge did not match`). On Windows the prompt reads standard input and cannot discard type-ahead.

## Share and publish

Sharing records an exact private conversation snapshot for one destination. Future turns stay private unless requested explicitly:

```bash
cvc share <conversation-id> --remote origin
cvc share <conversation-id> --remote origin --future
cvc unshare <conversation-id> --remote origin
```

Discover conversation ids with `cvc conversations`, or run `cvc share` with no id from a terminal for an interactive picker over the destination's unshared conversations. The picker is TTY-only and feeds the same typed challenge, so non-interactive callers must always name a conversation explicitly; before the challenge, `share` prints the conversation's title, counts, and activity range so the consent is legible.

`share` requires the displayed TTY challenge, which includes the destination fingerprint and snapshot count. `unshare` makes the whole conversation private again for that remote, or changes nothing: it is refused if any turn of the conversation has been published to that remote, or is pending or of unknown state after an unfinished publish (`cvc privacy reconcile` settles those against the remote). It cannot recall content already published.

A thought can hold material that is not yours to publish: file contents and web pages the agent read, command output, and model output that your AI provider's terms govern. Publishing copies it to everyone who can read the remote; the repository's license does not cover material you have no right to license. CVC's typed phrases record your choice; they are not consent from anyone whose personal data a thought contains.

A conversation you pulled from a remote can be shared and published to another remote, the same way Git lets you push to one remote what you pulled from another. Consent stays per destination: pulling from one remote authorizes nothing for another, so sharing it asks the usual `I SHARE` phrase, and publishing it asks that destination's publishing authorization and the `I PUBLISH` phrase. Publishing it again copies content that someone else captured, which can hold their prompts, file contents, command output and personal data, so review it first, as the paragraph above says. `cvc share` with no id lists these conversations too: their state reads `[published, not shared here]` for the remote they came from and `[private]` for any other remote, until you share them here. A re-shared conversation keeps its imported id, which names the remote it came from by that remote's fingerprint (a hash of its push URL), so readers of the new remote can see where it came from.

`cvc share <conversation-id> --remote origin --push` publishes the snapshot in the same command. Where auto-push was acknowledged for that destination, the share challenge is the only one: that standing grant already covers publication, exactly as it does for hooks and a bare `cvc push`. Without it, the `I PUBLISH` challenge follows, and declining it leaves the turns shared but unpublished.

Publish selected shared content manually:

```bash
cvc push --manual --remote origin
```

Manual publication requires destination sharing consent and a TTY `I PUBLISH <fingerprint> <candidate> <count>` challenge. A bare `cvc push` is treated as an auto-consent-gated path: it will not publish unless auto-push was explicitly enabled for that remote. Hooks use the same destination-specific auto-push gate.

Before the challenge, `push` states what it is about to publish:

```text
Publishing 21 new shared interaction(s) to 'origin' (42 shared in total).
Type exactly 'I PUBLISH <fingerprint> a1b2c3d4e5f6 21' to continue:
```

`<candidate>` is the first 12 characters of the commit that will be pushed. `<count>` is the number of shared interactions (the "thoughts" that `cvc conversations` counts) that the destination's current `refs/cvc/main` does not already hold, checked against a fresh fetch just before the prompt. Interactions it already holds are not counted again: after sharing one more 21-thought conversation, a second push asks for `21`, not `42`. If the remote ref was rewound or recreated, interactions published earlier are sent and counted again. The total in parentheses counts only what this clone shares with that destination, not everything on the remote, since other clones may have published more; it is for orientation only. The `published` figures in `cvc conversations` count the thoughts this clone shares with a destination that it knows the destination already has.

Everything else the push changes is named on the same line rather than counted in the challenge; the candidate id covers it. Added commit links, tombstones, and derivation records follow the count (`, plus 3 commit link(s) and 1 tombstone(s)`). When tombstones also drop an interaction, or records attached to it, from the current `refs/cvc/main` tree, the line names that removal too. That is suppression, not erasure: the ref's earlier commits still hold what was dropped, as can clones and third-party copies (see [Suppress, redact, and local rewrite plans](#suppress-redact-and-local-rewrite-plans)).

A push with no new interactions says so, as in `Publishing 3 commit link(s) and 1 tombstone(s) to 'origin' (no new shared interactions; 42 shared in total).`, and its challenge ends in `0`. A push that changes only the ref's format or index says `a format/index update of refs/cvc/main` instead. When nothing differs from the destination, `push` prints `No new shared interactions to publish to 'origin' (42 shared in total).` and asks for no challenge. Hooks and an acknowledged auto-push print the result line (`Published 21 new shared interaction(s) to 'origin' (42 shared in total).`) without a challenge.

A destination that declines the ref update, through a protected ref, a server hook, or a race lost after the advertisement, fails the push with the server's reason, publishes nothing, and leaves the affected rows in the unknown state until a reconciliation confirms what the destination holds. Reconcile ambiguous transport results before changing publication choices:

```bash
cvc privacy reconcile --remote origin
```

`pull` fetches the CVC ref and imports it into the local cache; receiving content from a remote does not grant local sharing intent for another destination.

### Browse and share interactively

Run `cvc conversations` in a terminal to open an interactive view of your captured conversations, most recent first, with their share and publication state for one remote at a time. Press `?` for help and `q` to quit. The view hides conversations that have no thoughts.

It opens only when standard input and output are both terminals, `TERM` names a terminal that can redraw (not `dumb` or empty, and on Linux and macOS not unset), and the window is at least 60 columns by 12 rows. In a smaller window, or one whose size cannot be read, `cvc conversations` prints the plain listing with one line saying why. `--plain`, a pipe, or no terminal prints the plain listing too. With nothing to list it says so, with a hint for the next step, instead of opening the view; for Claude Code capture the hint names both routes, the Claude Code plugin and `cvc harness install claude-code`.

When the view opens, and again after each share, unshare, publish or authorization, it waits until the terminal's input has been quiet for about 300 ms and discards whatever arrives meanwhile, so nothing typed or pasted before the screen appeared, or sent during the wait, acts in it. If input keeps arriving for about 3 seconds, the view closes without acting (`input kept arriving at the terminal, so the view was closed without acting on any of it`). When a phrase prompt cannot be asked for the same reason, the view says so instead, for example `Not shared: input kept arriving at the terminal, so no phrase was asked. Nothing was changed.` (likewise `Not unshared`, `Not published` and `Not authorized`).

**The screen.** The header shows the selected remote with a shortened location (user information, query and fragment removed), for example `github.com/acme/repo`, and the first 12 characters of its destination fingerprint; `publishing: authorized in this clone` (shortened to `publishing: authorized` on a narrow terminal), `publishing: not authorized yet`, or `publishing: unknown` when CVC's privacy state cannot be read, which turns sharing and publishing in the view off; the auto-push state, shown loudly when it is on; and `N shared thoughts waiting to publish`. These figures come from this clone's own records, and a publish fetches the remote first, so the count you are asked to confirm can differ. Each row shows when the conversation was last active (the column is headed `when (UTC)`), its thought and linked counts, its title, and a state label. Claude Code subagent conversations nest under their session, collapsed behind a badge such as `+2 agents · 1 waiting`. Sharing a session does not share an agent's own conversation, which is shared separately. But the session's own thoughts include the prompt it gave each agent and the result each returned (tool arguments and tool results are captured), and those are shared with the session. Conversations pulled from a remote are dim and read-only in the view; re-share one with `cvc share <conversation-id> --remote <name>`.

| Key | Action |
| --- | --- |
| `↑` `↓`, `j` `k` | Move the selection |
| `PgUp` `PgDn`, `Home` `End` | Move a page, or to the first or last row |
| `→` or `l` | Show a session's subagents |
| `←` or `h` | Hide them; on a subagent, select its session |
| `Enter` | Open details for the selected conversation |
| `s` | Share the selected conversation |
| `u` | Unshare it |
| `p` | Publish shared thoughts to the selected remote |
| `r` | Choose another remote |
| `?` | Help |
| `q` | Quit from the list; in an overlay or the remote list, close it |
| `Esc` | Close a bar, an overlay or the remote list |

Letters are case-insensitive, so Shift and Caps Lock change nothing. Sharing with future thoughts is a choice made in the share bar rather than a separate key, because the typed phrase is the same with and without `--future`. A key that cannot act right now is dimmed in the bottom bar, and pressing it says why.

| State | Meaning |
| --- | --- |
| `○ private` | This clone shares nothing in the conversation for the selected remote, and has no record of the remote holding any of it. |
| `◐ shared · N waiting` | Shared here; N shared thoughts are not yet published. |
| `◐ shared · future only` | Future sharing is on, but nothing in the conversation is shared yet and the remote holds nothing of it. |
| `● published` | Every shared thought is published. |
| `● published · N private` | Published, and N thoughts are not shared and the remote holds none of them (for example thoughts added after the share). The same `· N private` tail can follow `◐ shared`. |
| `● published · not shared here` | This clone's records show the remote holds it, but this clone has no share for it (for example after re-ingesting a transcript). |
| `? unconfirmed · not shared here` | This clone has no share for it, and its records for the remote show only an unfinished publish (pending or unknown), with nothing published. `cvc privacy reconcile` settles it. |
| `imported` | Pulled from a remote; read-only in the view (re-share one with `cvc share <conversation-id> --remote <name>`). |
| `· N not shared here` | When something is shared, a tail after `◐ shared` or `● published`: N thoughts that this clone has not shared but that the remote holds, by this clone's records. |
| `+future` | Added after another label, except `◐ shared · future only`, which already says it: thoughts captured later are shared as they are captured, without a new phrase. |

Every label describes this clone's own records for that remote, not a live check of the remote; `private` means private as far as this clone knows. On the Linux console (`TERM=linux`) or in a locale that is not UTF-8, the glyphs are replaced by ASCII stand-ins and the words stay the same; `NO_COLOR` is honored.

**Share, unshare and publish: a bar, then the phrase.** `s`, `u` and `p` do not act on their own. Each opens a short bar at the bottom of the screen that says what will happen next; nothing has happened yet, and `Esc` cancels. In the share bar, `Enter` means "this snapshot only" (the thoughts recorded so far) and `f` means "also future thoughts": thoughts captured later are marked shared as they are captured, without a new phrase, and reach the remote at the next publish. Where auto-push is on for that remote, every `git push` to that remote publishes them. Re-sharing a conversation whose future sharing is on, with `Enter`, turns future sharing off; the bar says so. `u` is refused, with the reason on screen, when any thought of the conversation has been published to the selected remote or is pending or unknown after an unfinished publish, because unsharing cannot recall content the remote may already hold. Otherwise every shared thought of the conversation becomes private again for that remote, and later thoughts stay private.

In a bar, `Enter` or `f` only continues to the phrase; it is never consent. The view then leaves the full-screen display and prints a short banner on the normal screen (the remote and its fingerprint, what the action will do, and that an empty line and Enter cancels), followed by the same typed phrase that the matching command asks for and whatever summary that command prints. It returns to the list afterwards:

| Key | Same as | Typed phrase |
| --- | --- | --- |
| `s` | `cvc share` | `I SHARE <conversation-id> <fingerprint> <count> <token>` |
| `u` | `cvc unshare` | `I UNSHARE <conversation-id> <fingerprint>` |
| `p` | `cvc push --manual` | `I PUBLISH <fingerprint> <candidate> <count>` |

In the share phrase, `<count>` and `<token>` pin the exact set of thoughts the share covers; the publish phrase is explained above. On Linux and macOS the prompt discards anything typed before it appears, so only text typed after the prompt can answer it. The view does not answer a phrase for you, and no key shares or publishes by itself. Two standing choices act later without a new phrase: `+future` shares new thoughts as they are captured, and auto-push (set with `cvc privacy set-auto-push`, never from this view) publishes shared thoughts at every `git push` to that remote. Scrubbing can miss secrets and personal data, and published thoughts cannot be recalled, so review a conversation's content before you share it.

**Authorization.** Publishing needs a one-time authorization for the remote's exact push URL; CVC checks only the URL, not who owns the repository or who can read it. While it is missing, `s` and `p` offer the `cvc privacy acknowledge-sharing` phrase (`I AUTHORIZE SHARING <fingerprint>`) instead of the share or publish phrase. Authorizing does not continue into the share or the publish, so press `s` or `p` again afterwards. It never turns on auto-push, which is set with `cvc privacy set-auto-push`, never from this view.

**Remotes.** `r` lists the repository's remotes with each one's location, fingerprint prefix, whether publishing is authorized, and the auto-push state. Choosing a remote changes what the view shows and acts on; it never authorizes anything. Authorization belongs to a push URL, not a remote name: it grants nothing for a different URL, and a remote with the same push URL is already covered. The view starts on `--remote <name>` if you pass it, otherwise on `origin`, and does not remember the choice between runs.

**Windows.** In v0.7.1 the view only browses on Windows: `s`, `u` and `p` are disabled and point to `cvc share`, `cvc unshare` and `cvc push --manual` (and `cvc privacy acknowledge-sharing` to authorize publishing). Those commands still work there, but on Windows their typed phrase cannot yet discard keys typed ahead of the prompt, which is why the view does not start one. The type-ahead protection described under [Privacy acknowledgement and destination consent](#privacy-acknowledgement-and-destination-consent) applies on Linux and macOS only.

**Words.** The view counts thoughts. A thought is one recorded step; for Claude Code, one assistant response. `cvc share` calls the same unit a "turn" and `cvc push` calls it an "interaction", so a count can read differently on the screen, in a phrase, and in a command's output.

**Scripts and automation.** Use `cvc conversations --plain` in scripts and in any tool that drives `cvc` through a pseudo-terminal, which looks like a terminal and would otherwise get the view. The view adds no consent path of its own: each phrase is still typed at the prompt, as with the commands.

**In VS Code.** The extension's **Volute: Share with Remote in Terminal** command opens this view for you. On Linux and macOS, when `cvc --version` of the `cvc` the extension runs (`volute.cvcCliPath`, then `~/.cvc/bin`, then `PATH`) reports 0.7.1 or later, the command runs `cvc conversations` in a VS Code terminal and asks for nothing first: you choose the conversation and the remote in the view, and each share, unshare and publish still ends in the typed phrase of the matching command, which neither the view nor the extension answers for you. The view needs a terminal panel of at least 60 columns by 12 rows; in a smaller one the plain listing appears with a line saying why, so enlarge the panel and run the command again. On Windows, where the view only browses, and with a `cvc` older than 0.7.1 or one whose version cannot be read, the command keeps its earlier flow: it asks for a conversation id and a remote name in two input boxes and runs `cvc share <conversation-id> --remote <name>` in a terminal. The extension reads the version each time the command runs, so updating `cvc` in place needs no window reload; after installing `cvc` somewhere else, run **Volute: Check Setup** so the extension finds it.

## Suppress, redact, and local rewrite plans

To suppress an interaction in local CVC projections only:

```bash
cvc delete-local <interaction-uuid>
```

This creates local suppression; it does not erase Git objects, remote history, or third-party copies.

`redact` requires destination share or publication authority. On its first confirmed invocation it creates a **pending destination tombstone**, suppresses the local projection, and tells you the exact next command:

```bash
cvc push --manual --remote <name>
```

That manual, destination-consented push projects the tombstone. Fetching on a later `redact` invocation must observe that tombstone in the v5 baseline before the command can build a protected hard-redaction plan:

```bash
cvc redact <interaction-uuid> --remote origin --rewrite-plan ./redaction-plan.json
cvc redact-verify-plan ./redaction-plan.json --remote origin
cvc redact <interaction-uuid> --remote origin --rewrite-plan ./redaction-plan.json --apply-local
```

The plan file is written with mode `0600` on Unix. `redact-verify-plan` only checks that the remote tip remains current. `--apply-local` changes **only local** `refs/cvc/main`; neither plan command pushes or force-pushes. `cvc delete-local` creates local suppression only and never propagates. A tombstone is suppression, not physical erasure, and a current-ref replacement is not guaranteed deletion.

Redaction plans created today use v2 identity and are portable only between linked
worktrees that share the same common Git directory. Historical v1 plans remain
bound to their original worktree. Neither form implies portability to arbitrary clones.
Verification accepts only regular, non-symlink plan files up to 64 KiB and rejects
unknown, duplicate, or mismatched format/version fields before contacting a remote.

The local SQLite cache enables `secure_delete` and, after deletion, attempts a truncating WAL checkpoint and `VACUUM` compaction. This is best effort only: residual filesystem blocks, SSD wear leveling, snapshots, failed-operation WAL remnants, backups, and Git objects may still retain data.

If credentials may have been exposed, rotate them first. Git-host support/removal is best effort; clones, forks, reflogs, caches, backups, and host object retention may retain content. Remote hard rewrite is **NOT implemented** pending an atomic force-with-lease design. Do not use blind force-push commands.

## Troubleshooting

- Run `cvc init` in a Git repository before using capture or sync commands.
- Ensure the configured remote permits `refs/cvc/main` publication.
- The Claude Code plugin's `SessionStart` message says the `cvc` command is not on the `PATH` Claude Code runs hooks with. The plugin cannot capture that session, and nothing blocked it. While `cvc` is missing from that `PATH`, every `PostToolUse`, `Stop` and `SessionEnd` event also shows a non-blocking hook error, in every project (`cvc: command not found in an absolute PATH directory (the cvc plugin does not use relative PATH entries)`), so fix the `PATH` or disable the plugin (`/plugin disable cvc@volute`). Install CVC (see the README's [Install a release](../README.md#install-a-release)) or add the directory holding `cvc` (by default `~/.cvc/bin`, where the installer scripts and the npm launcher put it) to that `PATH`, then start a new session. A desktop app or IDE session inherits its environment from how that app was started, which can differ from your terminal's, so `cvc` working in a terminal does not prove it works there. You see this message even in a checkout that has the per-checkout hooks, and it is accurate there: those hooks pin an absolute binary path, so they still capture the session. If you cannot or do not want to change that `PATH`, `cvc harness install claude-code` is the route that does not need it.
- The Claude Code plugin's `SessionStart` message says the only `cvc` on the `PATH` Claude Code runs hooks with is in a relative `PATH` entry, which the plugin does not use. Your `PATH` reaches `cvc` only through an entry such as `.` or `bin`, or through a quoted `~/.cvc/bin` (for example `export PATH="~/.cvc/bin:$PATH"`), which leaves a literal `~` in `PATH`; write it as `$HOME/.cvc/bin`, which the shell expands to an absolute path. The plugin's hooks look for `cvc` only in absolute directories, so a project cannot supply `cvc` through a relative entry. Put `cvc` in an absolute directory on that `PATH` (by default `~/.cvc/bin`) and start a new session. Until then the hook error quoted above appears on every event, in every project, and nothing is captured.
- The Claude Code plugin's `SessionStart` message says the `cvc` found on the `PATH` Claude Code runs hooks with could not be run. The `cvc` the hooks found exists but does not start with that `PATH`. A typical case is an npm-installed `cvc`, which is a Node launcher, when `node` is not on that `PATH`. Run `cvc --version` with the same `PATH` Claude Code sees (a desktop app or IDE session can differ from your terminal), fix what fails, and start a new session. Putting `~/.cvc/bin` ahead of npm's global bin directory on that `PATH` avoids the launcher once the binary has been published there.
- The Claude Code plugin's `SessionStart` message says the `cvc` on `PATH` is older than the plugin needs. Upgrade CVC to 0.7.1 or later (see the README's [Install a release](../README.md#install-a-release)) and start a new session. Until then an older `cvc` reports hook errors in every project without CVC, because only 0.7.1 and later stay silent where there is no initialized repository.
- You installed the Claude Code plugin but nothing is captured, and Claude Code showed nothing. Outside a CVC-initialized repository the hooks exit 0 and print nothing by design (unless Git itself cannot open the repository): in a project that is not a Git repository, in a repository where `cvc init` was never run, in a bare repository, in a directory that no longer exists, or in a repository owned by another user (Git refuses those). That silence is expected in a project that does not use CVC. If you want the project captured, run `cvc init` in it and then `cvc privacy acknowledge-capture`. A repository that is initialized but has no capture acknowledgement is not silent: its hooks report `consent-required`. A `.git` entry that Git cannot open, for example a dangling gitfile, reports an error that names the path; fix or remove that entry.
- Nothing is captured although the repository is initialized and capture is acknowledged. Was the session started in a parent folder, or in another project? A session is recorded in the repository it was started in, and one started outside an initialized repository is not recorded, even when it later works inside one. Open Claude Code in the repository, or one of its worktrees. For a finished session, `cvc ingest claude-code --transcript <file>` records it afterwards.
- A long or resumed Claude Code session is missing, for example an old session you resumed right after installing the plugin. A hook run has the hook's 30 second budget and commits nothing unless it finishes, so a very large transcript (roughly tens of megabytes on first ingestion) may not finish and is then not recorded by the hooks. Record it with `cvc ingest claude-code --transcript <file>`.
- Every Claude Code hook event is slow, in every project, or the first event after installing or upgrading CVC takes long. If you installed CVC with npm and the npm launcher is the `cvc` found first on the `PATH`, every event starts the Node launcher first, and the first event after an install or upgrade may download a release within the hook's 30 second budget. Put `~/.cvc/bin` on Claude Code's `PATH` ahead of npm's global bin directory and run `cvc --version` once after each npm install or upgrade.
- A new worktree has no Claude Code hooks. This is about the per-checkout install; the Claude Code plugin needs no per-worktree install. If `git worktree add` printed a `CVC: Claude Code hooks not installed in this worktree: ...` line, it says why. If it printed nothing, there are four likely causes: there is no record (run `cvc harness install claude-code` in the main checkout and read its output); the worktree was made with `--no-checkout`; the client that made it created the worktree without running Git's hooks; or a hook of yours that `post-checkout` was appended to exits before the CVC block is reached. To give a worktree that missed out the hooks, run `cvc harness install claude-code` inside it.
- Secret scrubbing is defense in depth, not a guarantee that all sensitive material is detected or removed.
