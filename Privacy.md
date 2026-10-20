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

Transcript ingestion records more than the other channels: the harness-exposed thinking blocks (as `model_cot`), the text of tool results (size-bounded, as the prompt of the following thought), and tool arguments. All of it stays local, private by default, and scrubbed on the way in. `.thoughtignore` `path:` rules remove file-context items for excluded paths, but they do not search the tool output copied into prompt text; for that text the coverage is the built-in detectors plus `literal:`/`regex:` rules. The hook entries themselves live in the checkout-local `.claude/settings.local.json`, which the installer keeps out of Git.

The database, privacy/consent state, locks, and rewrite state live in `$(git rev-parse --git-common-dir)/cvc`, so linked Git worktrees share them. Normal CVC Git refs, including `refs/cvc/main`, remain in the shared Git refs namespace rather than that directory. Hooks use Git's effective hooks path (`<common-dir>/hooks` by default, or `core.hooksPath`; relative hook paths are active-worktree-relative). `.thoughtignore`, captured context resolution, `HEAD`, index, and branch state are local to the active worktree. CLI, MCP, LSP, VS Code, and Claude Code harness linked-worktree support does not change the private-by-default, sharing, or auto-push boundaries. Claude Code hooks live in each worktree's own `.claude/settings.local.json`, and each worktree's captures are attributed to that worktree. How a new worktree gets those hooks is described under "Claude Code hooks in new worktrees" below.

Destination authority is isolated: a share, authorization, publication observation, or tombstone received from destination A has no authority for destination B. Remote evidence is recorded as an untrusted remote observation unless independently observed locally; it is not a permission grant.

Conversations pulled from a remote can be shared and published to another remote, the same way Git lets you push to one remote what you pulled from another. Consent stays per destination: pulling from one remote authorizes nothing for another, so sharing a pulled conversation asks the usual `I SHARE` phrase, and publishing it asks that destination's publishing authorization and the `I PUBLISH` phrase. Publishing it again copies content that someone else captured, which can hold their prompts, file contents, command output and personal data, so review it first (see the [note on material that is not yours to publish](cvc-cli/README.md#share-and-publish) in the CLI reference). A pulled conversation that is shared again keeps its imported id, which names the remote it came from by that remote's fingerprint (a hash of its push URL), so readers of the new remote can see where it came from.

The scrubber uses bounded, high-confidence built-in detectors for several private-key, token, authorization, URL-credential, and credential-assignment forms, plus credential-bearing JSON keys. It is defense in depth, not proof that all secrets, PII, encodings, or provider-specific formats are found. Built-in protection cannot be disabled by policy.

## Claude Code hooks in new worktrees

`cvc harness install claude-code` writes capture hooks into one checkout's `.claude/settings.local.json`. It also changes what happens when you create a worktree later. It adds two things:

- **A record.** `<common-git-dir>/cvc/harness/claude-code.path` holds the absolute path of the `cvc` binary the hooks run, and nothing else. No captured data is in it.
- **A Git hook.** CVC's block in Git's `post-checkout` hook (`cvc init` installs it too). Git runs that hook after every checkout, but the block starts the recorded binary only when a checkout creates a worktree (`git worktree add`, Claude Code's worktree option). Leaving an unborn (orphan) branch looks the same to Git, so the binary is started then too; in the main worktree it does nothing. With no record it starts nothing.

The recorded binary then runs in the new linked worktree and creates that worktree's `.claude/settings.local.json` with the same hook entries. It only creates the file: if one is already there, whatever it holds, it installs nothing and says so, and it never edits a settings file that Git tracks. To do this it reads the record and the repository's consent state. It opens no database and uses no network.

**Consent.** The capture acknowledgement is per repository, not per worktree. It is stored with the other consent state in the common Git directory, so one acknowledgement covers every worktree, and a new worktree is not asked again. New worktrees get the hooks only while that acknowledgement stands. Without it the hook prints one line and installs nothing, and `harness install` itself refuses to run. Hooks left without an acknowledgement fail with `consent-required` and record nothing.

Captures made through these hooks follow the same boundaries as any other: private by default, scrubbed on the way in, attributed to the worktree they ran in, and shared only per conversation and destination. A new worktree is captured under its own checked-out `.thoughtignore`; a policy that was never committed in another checkout does not carry over.

**Stopping it.** `cvc harness uninstall claude-code` removes this checkout's entries and deletes the record, so later worktrees get nothing. Worktrees that already have the entries keep them and keep capturing. The command lists each one, and you run it in every listed worktree to stop capture there. It does not withdraw the capture acknowledgement. The `post-checkout` block stays in place and does nothing without a record.

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
