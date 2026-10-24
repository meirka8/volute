# CVC Privacy and `.thoughtignore` Reference

This reference documents the privacy behavior implemented in this repository. CVC is local-first, not local-only: local capture and remote publication are separate operations with separate consent boundaries.

## How your data moves

Four verbs, four boundaries — worth internalizing before the detailed reference below:

1. **Capture** writes an interaction into the local SQLite database immediately, as the conversation happens. Content is scrubbed on the way in and is private by default. It exists nowhere in Git: `git push` cannot transport it, and a clone does not receive it.
2. **Link** — the automatic post-commit hook — records which commit a thought belongs to. Linking is provenance bookkeeping inside the local database; it has no effect on visibility or sharing.
3. **Share** records consent marks in the local database: *these turns of this conversation are authorized for this exact destination fingerprint.* Nothing is exported, and nothing touches Git. New conversations are never auto-shared; each is a fresh, deliberate decision. List conversations and their share state with `cvc conversations`; a bare `cvc share` from a terminal offers an interactive picker. The interactive view that `cvc conversations` opens in a terminal adds no consent path: it hands over to the same typed phrases and does not answer them, so no key shares or publishes by itself; where auto-push is on, every `git push` to that remote publishes shared thoughts.
4. **Push** — `cvc push --manual` with its interactive challenge, or a destination whose auto-push was separately acknowledged — is the only step that creates Git objects. It fetches the destination's current CVC ref as a baseline, projects shared-but-unpublished content into `refs/cvc/main`, and pushes that one ref. A thought that is never shared, or shared but never pushed, never becomes a Git object at all.

On another machine, `cvc pull` fetches the ref and imports it into that machine's local cache; receiving content grants no authority for any destination.

The consequence to hold onto: Git objects are the durable, hard-to-erase form of this data, and they are minted only at the moment of authorized publication — behind the last human gate. With auto-push on for a destination, that gate is any `git push` to that remote, including one an editor or tool runs. Everything before that point is locally deletable database state.

## Capture and sharing

Every production aggregate capture is prepared and scrubbed before its SQLite transaction. Captures are private by default. Passive capture channels — VS Code chat-session watching and Claude Code transcript ingestion (`cvc ingest claude-code`, including its hook-driven form) — additionally require the repository-local interactive capture acknowledgement, and refuse to record without it. Sharing, consent, auto-push, publication state, and remote tombstone authority are destination-specific local state; they are not synced as general permission grants. Derived links and FORMAT5 derivation evidence never create, broaden, or imply share consent.

The typed challenges that record consent run only at an interactive terminal, which stops non-interactive callers and stale or queued input from answering them. They are a confirmation step, not a security boundary: a program that drives a terminal as the same user can type a phrase, and the consent and share records are local files that any process running as that user can change.

Transcript ingestion records more than the other channels: the harness-exposed thinking blocks (as `model_cot`), the text of tool results (size-bounded, as the prompt of the following thought), and tool arguments. All of it stays local, private by default, and scrubbed on the way in. `.thoughtignore` `path:` rules remove file-context items for excluded paths, but they do not search the tool output copied into prompt text; for that text the coverage is the built-in detectors plus `literal:`/`regex:` rules. When installed per checkout, the hook entries live in the checkout-local `.claude/settings.local.json`, which the installer keeps out of Git. A Claude Code plugin's hooks come from the plugin instead; see "Claude Code plugin hooks" below.

The database, privacy/consent state, locks, and rewrite state live in `$(git rev-parse --git-common-dir)/cvc`, so linked Git worktrees share them. Normal CVC Git refs, including `refs/cvc/main`, remain in the shared Git refs namespace rather than that directory. Hooks use Git's effective hooks path (`<common-dir>/hooks` by default, or `core.hooksPath`; relative hook paths are active-worktree-relative). `.thoughtignore`, captured context resolution, `HEAD`, index, and branch state are local to the active worktree. CLI, MCP, LSP, VS Code, and Claude Code harness linked-worktree support does not change the private-by-default, sharing, or auto-push boundaries. Claude Code hooks installed per checkout live in each worktree's own `.claude/settings.local.json`, and each worktree's captures are attributed to that worktree whichever way the hooks arrived. How a new worktree gets those hooks is described under "Claude Code hooks in new worktrees" below; the Claude Code plugin needs no per-worktree file.

Destination authority is isolated: a share, authorization, publication observation, or tombstone received from destination A has no authority for destination B. Remote evidence is recorded as an untrusted remote observation unless independently observed locally; it is not a permission grant.

Conversations pulled from a remote can be shared and published to another remote, the same way Git lets you push to one remote what you pulled from another. Consent stays per destination: pulling from one remote authorizes nothing for another, so sharing a pulled conversation asks the usual `I SHARE` phrase, and publishing it asks that destination's publishing authorization and the `I PUBLISH` phrase. Publishing it again copies content that someone else captured, which can hold their prompts, file contents, command output and personal data, so review it first (see the [note on material that is not yours to publish](cvc-cli/README.md#share-and-publish) in the CLI reference). A pulled conversation that is shared again keeps its imported id, which names the remote it came from by that remote's fingerprint (a hash of its push URL), so readers of the new remote can see where it came from.

The scrubber uses bounded, high-confidence built-in detectors for several private-key, token, authorization, URL-credential, and credential-assignment forms, plus credential-bearing JSON keys. It is defense in depth, not proof that all secrets, PII, encodings, or provider-specific formats are found. Built-in protection cannot be disabled by policy.

## Claude Code hooks in new worktrees

This section describes the per-checkout install. The user-scoped Claude Code plugin works differently and is covered in the next section.

`cvc harness install claude-code` writes capture hooks into one checkout's `.claude/settings.local.json`. It also changes what happens when you create a worktree later. It adds two things:

- **A record.** `<common-git-dir>/cvc/harness/claude-code.path` holds the absolute path of the `cvc` binary the hooks run, and nothing else. No captured data is in it.
- **A Git hook.** CVC's block in Git's `post-checkout` hook (`cvc init` installs it too). Git runs that hook after every checkout, but the block starts the recorded binary only when a checkout creates a worktree (`git worktree add`, Claude Code's worktree option). Leaving an unborn (orphan) branch looks the same to Git, so the binary is started then too; in the main worktree it does nothing. With no record it starts nothing.

The recorded binary then runs in the new linked worktree and creates that worktree's `.claude/settings.local.json` with the same hook entries. It only creates the file: if one is already there, whatever it holds, it installs nothing and says so, and it never edits a settings file that Git tracks. To do this it reads the record and the repository's consent state. It opens no database and uses no network.

**Consent.** The capture acknowledgement is per repository, not per worktree. It is stored with the other consent state in the common Git directory, so one acknowledgement covers every worktree, and a new worktree is not asked again. New worktrees get the hooks only while that acknowledgement stands. Without it the hook prints one line and installs nothing, and `harness install` itself refuses to run. Hooks left without an acknowledgement fail with `consent-required` and record nothing.

Captures made through these hooks follow the same boundaries as any other: private by default, scrubbed on the way in, attributed to the worktree they ran in, and shared only per conversation and destination. A new worktree is captured under its own checked-out `.thoughtignore`; a policy that was never committed in another checkout does not carry over.

**Stopping it.** `cvc harness uninstall claude-code` removes this checkout's entries and deletes the record, so later worktrees get nothing. Worktrees that already have the entries keep them and keep capturing. The command lists each one, and you run it in every listed worktree to stop capture there. It does not withdraw the capture acknowledgement. The `post-checkout` block stays in place and does nothing without a record.

## Claude Code plugin hooks

The `volute` Claude Code plugin marketplace in this repository offers a Claude Code plugin named `cvc` (`/plugin install cvc@volute`). It reaches the same capture by a different route, and it differs from the per-checkout install in scope, not in consent. The scope matters, though: installing the plugin turns on Claude Code capture, for sessions started there, in every repository on the machine that already holds a capture acknowledgement (for example one granted for the VS Code extension or a per-checkout install). In a repository where capture was already acknowledged, the plugin starts recording Claude Code sessions from the next session, with no new prompt. Where no acknowledgement is held, nothing is recorded.

**Scope.** It installs at user scope by default, so Claude Code runs its hooks in every session you start on the machine, in every project and worktree (terminal, the desktop app's Code tab and IDE extensions; per Claude Code's documentation, not cloud sessions), including projects that have nothing to do with CVC. It adds hooks only. Their definitions are in `plugins/claude-code/hooks/hooks.json` in this repository, with the two `sh` scripts they run (`scripts/run-cvc.sh` and `scripts/check-cvc.sh`) beside it. Like any Claude Code plugin's hooks they run commands on your machine, so read them before you install, and again after each `/plugin marketplace update`: the hooks come from the CVC release tag the marketplace names, and run as you. The marketplace entry pins the plugin to that tag. The plugin's files come only from the release tag. The catalog entry that names that tag is read from `main`, and CI requires it to name this repository, this path and the tag of the current version, with no other fields. Each hook runs `cvc ingest claude-code --hook` through `scripts/run-cvc.sh`, started as `/bin/sh` by its absolute path (with a guard that turns exit status 2, which Claude Code treats as blocking, into 1), so a project cannot supply the shell. The script looks for `cvc` only in absolute directories on the `PATH` that Claude Code runs hooks with. Relative entries (empty, `.`, `bin`, a literal `~`) are skipped, so a project cannot supply `cvc` through them. Absolute directories on your `PATH` are trusted, including ones your tools add for a project, such as direnv, an activated virtualenv or `node_modules/.bin`. The per-checkout install pins an absolute path instead. When a session starts or resumes, a small `sh` script (`plugins/claude-code/scripts/check-cvc.sh`, also started as `/bin/sh`) checks that `cvc` is present and recent enough. To do so it runs `cvc ingest claude-code --hook` once with a made-up payload whose working directory and project directory do not exist, so no repository is found, no transcript is read, and nothing is recorded. An older `cvc` rejects the input, which names no session or transcript, so it records nothing either. The script shows you a message, which per Claude Code's documentation is not added to Claude's context, only when `cvc` is missing, is reachable only through a relative `PATH` entry, could not be run, or is too old, and it is designed not to block the session (it always exits 0).

**Which sessions are recorded.** A session is recorded in the repository it was started in: the repository that contains Claude Code's project directory (`CLAUDE_PROJECT_DIR`, which Claude Code gives every hook); any of its worktrees counts, and so does any subdirectory. The session is recorded there in full, including work it does in other directories (`/add-dir` folders, other repositories it moves into) and its end, wherever its working directory is by then. The hook's working directory only decides which of that repository's worktrees a response is attributed to. Another repository that the session visits receives nothing. Put plainly, the start repository's store receives the whole session, including prompts, tool arguments and tool output from other directories the session worked in, even from projects that have nothing to do with CVC. Only the start repository's `.thoughtignore` and acknowledgement apply to that content: another repository's own `.thoughtignore` is not consulted for what the session read there, so keep secrets you work with across repositories covered by the start repository's policy too, or start a separate session in that repository. Sharing stays a separate typed choice per conversation and destination, and the sharing review in `cvc conversations` is where to check what a session holds before you share it. A session started outside an initialized repository (in a parent folder, or in another project) is not recorded, even when it later works inside one. If Claude Code provides no `CLAUDE_PROJECT_DIR` (older Claude Code versions, or a manual run), the hook's working directory decides; if it is set but empty, nothing is recorded. This is the same scope the per-checkout install has, since Claude Code loads a checkout's settings only for sessions started there.

**In unrelated projects.** When a session was not started in a CVC-initialized repository (Claude Code's project directory is not in a Git repository, is in a repository where `cvc init` was never run, is in a bare repository, no longer exists, or is in a repository owned by another user, which Git itself refuses), the hooks still run. Each hook event runs the small `sh` wrapper, which starts a short `cvc` process:

- Claude Code passes it the hook's JSON input on stdin. For `PostToolUse` that input includes the tool's arguments and output, which can contain file contents and command output from that project.
- `cvc` reads up to 1 MiB of it into memory to find the working directory, then checks whether the session was started in a CVC-initialized repository.
- When it was not, it exits 0 and prints nothing. It reads no transcript, opens no CVC database, writes no file and uses no network, and the input is discarded when it exits. Nothing from those sessions is recorded.

Three exceptions:

- A `.git` entry that Git cannot open (for example a dangling gitfile) reports an error that names the path, without blocking the session.
- If `cvc` is missing from the `PATH`, or reachable only through a relative `PATH` entry, then besides the one `SessionStart` message, every `PostToolUse`, `Stop` and `SessionEnd` event shows a non-blocking hook error, in every project: `cvc: command not found in an absolute PATH directory (the cvc plugin does not use relative PATH entries)`.
- If the `cvc` found first on the `PATH` is the npm launcher, a Node program, it runs before the binary and, when the build in `~/.cvc/bin` is older than the package, downloads a release (network access and writes under `~/.cvc`). See the Requirements in the [plugin README](plugins/claude-code/README.md#requirements).

**In a CVC repository.** Consent is unchanged. Capture needs the same per-repository capture acknowledgement, a typed challenge at an interactive terminal, and refuses to record without it: an initialized repository that has not granted it gets a visible `consent-required` and records nothing, without blocking the session. The plugin's hooks never run the acknowledgement, share or publish commands, and installing it changes no consent state. Those commands accept their phrase only at an interactive terminal, which is a confirmation step, not a security boundary (see "Capture and sharing" above); sharing and publication remain the CLI's typed challenges. Where the session was started in the repository and capture is acknowledged, each hook run reads the session's transcript from its stored cursor and writes the new scrubbed thoughts, the cursor and a per-session lock under `$(git rev-parse --git-common-dir)/cvc`. Captures follow the same boundaries as any other: private by default, scrubbed on the way in, attributed to the worktree they ran in, and shared only per conversation and destination.

**With both routes installed.** Per Claude Code's documentation, a plugin's hooks are not de-duplicated against settings-file hooks, so in a checkout that also has the per-checkout entries both hooks run for each event, in parallel. Installing both is supported. CVC serializes ingestion of a session with a per-session lock in the repository's CVC directory, and the second run finds nothing new, because ids are deterministic and the cursor has advanced. Ingestion is idempotent: thought ids are derived from the session and response, so a second run over the same event adds no duplicate thoughts. The cost is one more short `cvc` process per event.

**Stopping it.** `/plugin disable cvc@volute` stops the hooks and keeps the plugin installed; `/plugin uninstall cvc@volute` removes it. Neither withdraws the capture acknowledgement or removes what was already captured. Those thoughts stay in the local database, private unless you share them, and the `cvc privacy` controls and the deletion commands under "Redaction and retention" below still apply to them.

## `.thoughtignore` location and syntax

Place a regular, non-symlink file named `.thoughtignore` at the repository root. It is itself excluded from captured file context. The current policy format has no header; the directives below are the complete versioned syntax for the current implementation:

```text
# comments and blank lines are ignored
path:relative/directory
path:relative/file.ext
literal:an exact string to mask
regex:an RE2-style Rust regex to mask
```

`path:` excludes matching context paths, including descendants when the configured path is a directory. It removes that context item; it does **not** search arbitrary prompt/response text.

`literal:` replaces exact occurrences in captured text with a generated redaction marker. `regex:` replaces each regex match with a generated marker. Both add to, rather than replace, built-in secret masking. Do not place a real secret in a literal or regex policy: the policy is repository content and is not a secret store.

## Bounds and validation

- Policy file: at most 64 KiB.
- Directive: at most 4,096 bytes.
- Literal directives: at most 128.
- Regex directives: at most 64.
- Combined built-in/policy replacements: at most 256 per processed text value.
- Captured text value: at most 1 MiB; aggregate capture: at most 16 MiB.

Paths must be non-empty, relative, slash-separated paths with no `..`, absolute prefix, backslash, doubled slash, or `./` component. Regexes must compile and may not use `(?` constructs. Unknown directives, empty literals, unsafe/oversized policies, malformed regexes, excessive rules or matches, symlinks, and detected policy-file races are errors.

Policy loading is fail-closed. A capture uses one immutable `PreparedPolicy` snapshot, so path filtering and persistence use the same policy even if the file changes concurrently. On platforms where policy loading cannot be secured, an existing policy is rejected instead of silently ignored. A missing policy uses built-ins only.

## Redaction and retention

Format v5 tombstones suppress future CVC projections for their local or destination scope; they also suppress the tombstoned interaction's FORMAT5 derivation-event and range-source closure. They do not physically erase immutable Git objects. `cvc delete-local <interaction-uuid>` creates only local suppression and never propagates. `cvc redact <interaction-uuid> --remote <name> --rewrite-plan <path>` confirms and creates a pending tombstone for that destination. Its exact next command is `cvc push --manual --remote <name>`; after the tombstone is visible in the fetched v5 baseline, rerun `redact` to create a local-only `RedactionPlan` (and optionally apply it locally). `RedactionPlan` never rewrites a remote.

SQLite enables `secure_delete` and, after logical deletion, attempts `wal_checkpoint(TRUNCATE)` and `VACUUM` compaction. These are best-effort database/file cleanup measures, not a guaranteed erase: the filesystem, SSD wear leveling, snapshots, WAL/journal remnants after failures, backups, and already-created Git objects can retain bytes.

If a credential may be exposed, rotate it first. Git host removal/support is best effort; clones, forks, reflogs, caches, backups, and host retention may still contain data. Remote hard rewrite is **NOT implemented** pending an atomic force-with-lease design. Do not use blind force-push commands.
