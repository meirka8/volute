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

`conversations` lists conversations most-recent-first with thought/linked counts and, for the selected (or `origin`) remote, their share and publication state — it is the read-only view that answers "what would I be sharing?".

`init` creates the local SQLite cache and installs advisory `post-commit`, `pre-push`, `post-merge`, `post-rewrite`, and `post-checkout` hooks (respecting `core.hooksPath`); the MCP and LSP servers install only the first four. The `post-checkout` hook starts a binary only when Git creates a worktree, and only if `cvc harness install` recorded one (see "Hooks in new worktrees" below); ordinary checkouts and switches do not start anything, with one exception: leaving an unborn (orphan) branch looks like a new worktree to Git, so it starts the recorded binary too (which acts only in a linked worktree and does nothing in the main one). A hook file that already exists and is not a shell script is left alone with a warning, for each of these hooks. `run` records its command and output as a private floating interaction. The post-commit linker may associate recent eligible interactions with a commit, but it does not share them. When it links, it prints one line naming the commit, `CVC: Linked N thought(s) to commit <sha>.`; in a checkout with the Claude Code harness installed it prints that line on every commit, because transcript ingestion reads the commit hash from it (see below). Configure its conservative window with `git config cvc.linkWindow <seconds>` (`0..=2592000`, default `86400`; `0` disables automatic linking). Automatic linking uses only the documented time/file-context policy—not author, message, or file-set similarity heuristics; `CVC-Session` trailers are not implemented. Captures additionally record a local-only fingerprint of their active worktree, and automatic linking considers only thoughts captured in the committing worktree (legacy rows without a recorded origin remain broadly eligible); parallel linked worktrees cannot claim each other's floating thoughts. Thoughts captured in a worktree that is later removed stay floating rather than being linked elsewhere.

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

### Ingestion

Each hook runs `cvc ingest claude-code --hook`, which reads Claude Code's JSON payload from stdin, discovers the repository from the payload's working directory, and ingests the transcript from the session's stored cursor. Hook runs are silent on success. A transcript this version does not understand (another major version, or an entry whose shape is unknown) fails loudly with a non-zero status that Claude Code shows in the session without blocking it; CVC never returns the blocking status, and a failed run writes nothing. A content block whose type is merely unfamiliar, which Claude Code adds within a major version (its `fallback` model-switch marker, for example), does not fail the session: the block is kept as a bracketed placeholder naming its type, never its content, and `cvc ingest claude-code` reports how many such blocks it kept and their types. `PostToolUse` keeps ingestion near-real-time so a commit made mid-session can still link the reasoning that preceded it; `Stop` and `SessionEnd` close the final response of a turn or session. When a response runs `git commit`, ingest reads the commit's hash from the command's recorded output in four exact shapes: git's own `[branch sha]` summary line, the `CVC: Linked … to commit <sha>.` line the post-commit hook prints into the same output (which is how a quiet `git commit -q` still names its commit), a hash at the start of a line as `git log --oneline` or `git rev-parse --short HEAD` print, and a full 40-character hash. Every candidate must resolve to a commit in this repository whose time is consistent with the response, so output that merely mentions older commits links nothing. The verified commit then links that response and the reasoning before it directly, since the time-window linker cannot reach a response written before its commit existed. Subagent (Agent tool) transcripts stored beside the session are ingested in the same pass, each as its own conversation named `<session>:agent-<id>`; subagent hook invocations themselves are ignored because the parent session's hooks cover them.

Record a finished session, or one that ran before the hooks were installed, explicitly:

```bash
cvc ingest claude-code --transcript ~/.claude/projects/<workspace>/<session>.jsonl
```

Ingestion is idempotent: every assistant response has a deterministic id within its session, so re-running never duplicates or replaces a thought, and never disturbs commit links it has already earned. One thought is recorded per assistant response: the prompt or tool results it reacted to become the prompt, its exposed thinking becomes `model_cot`, its text becomes the response, and its tool calls become tool executions carrying the status of their results. Paths the session read or edited inside the worktree become file context, subject to `.thoughtignore` `path:` rules; tool output is rendered into the following thought's prompt with bounded size. The conversation id is the Claude Code session id, so `cvc conversations` and `cvc share` work on it directly, and its title follows Claude Code's own: a title you set yourself outranks the generated one, and the first prompt stands in until either exists. Captures follow the usual boundaries: private by default, scrubbed on the way in, shared only per conversation and destination, and attributed to the worktree the hook ran in, so parallel checkouts cannot claim each other's thoughts.

## Privacy acknowledgement and destination consent

Inspect the local status for the selected remote (or the default remote):

```bash
cvc privacy status --remote origin
```

Passive collection (VS Code chat-session watching and Claude Code transcript ingestion, including its hook-driven form) is disabled until the repository owner completes:

```bash
cvc privacy acknowledge-capture
```

This requires an interactive TTY and typing exactly `I UNDERSTAND LOCAL CAPTURE`. The acknowledgement remains local and is never synced.

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

Non-interactive input is rejected for acknowledgements. This intentionally prevents scripts, IDEs, and MCP clients from silently granting capture, sharing, or auto-push consent. The answer is read from the controlling terminal itself, and anything already queued on it (a paste, type-ahead, or a line left over from a preceding challenge in the same command) is discarded when the prompt appears, so a challenge can only be satisfied by input typed after it was displayed.

## Share and publish

Sharing records an exact private conversation snapshot for one destination. Future turns stay private unless requested explicitly:

```bash
cvc share <conversation-id> --remote origin
cvc share <conversation-id> --remote origin --future
cvc unshare <conversation-id> --remote origin
```

Discover conversation ids with `cvc conversations`, or run `cvc share` with no id from a terminal for an interactive picker over the destination's unshared conversations. The picker is TTY-only and feeds the same typed challenge, so non-interactive callers must always name a conversation explicitly; before the challenge, `share` prints the conversation's title, counts, and activity range so the consent is legible.

`share` requires the displayed TTY challenge, which includes the destination fingerprint and snapshot count. `unshare` makes only unpublished turns private; it cannot recall content already published.

`cvc share <conversation-id> --remote origin --push` publishes the snapshot in the same command. Where auto-push was acknowledged for that destination, the share challenge is the only one: that standing grant already covers publication, exactly as it does for hooks and a bare `cvc push`. Without it, the `I PUBLISH` challenge follows, and declining it leaves the turns shared but unpublished.

Publish selected shared content manually:

```bash
cvc push --manual --remote origin
```

Manual publication requires destination sharing consent and a TTY `I PUBLISH ...` challenge. A bare `cvc push` is treated as an auto-consent-gated path: it will not publish unless auto-push was explicitly enabled for that remote. Hooks use the same destination-specific auto-push gate. Reconcile ambiguous transport results before changing publication choices:

```bash
cvc privacy reconcile --remote origin
```

`pull` fetches the CVC ref and imports it into the local cache; receiving content from a remote does not grant local sharing intent for another destination.

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
- A new worktree has no Claude Code hooks. If `git worktree add` printed a `CVC: Claude Code hooks not installed in this worktree: ...` line, it says why. If it printed nothing, there are four likely causes: there is no record (run `cvc harness install claude-code` in the main checkout and read its output); the worktree was made with `--no-checkout`; the client that made it created the worktree without running Git's hooks; or a hook of yours that `post-checkout` was appended to exits before the CVC block is reached. To give a worktree that missed out the hooks, run `cvc harness install claude-code` inside it.
- Secret scrubbing is defense in depth, not a guarantee that all sensitive material is detected or removed.
