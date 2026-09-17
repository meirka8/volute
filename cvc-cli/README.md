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

`init` creates the local SQLite cache and installs advisory `post-commit`, `pre-push`, `post-merge`, and `post-rewrite` hooks (respecting `core.hooksPath`). `run` records its command and output as a private floating interaction. The post-commit linker may associate recent eligible interactions with a commit, but it does not share them. Configure its conservative window with `git config cvc.linkWindow <seconds>` (`0..=2592000`, default `86400`; `0` disables automatic linking). Automatic linking uses only the documented time/file-context policy—not author, message, or file-set similarity heuristics; `CVC-Session` trailers are not implemented. Captures additionally record a local-only fingerprint of their active worktree, and automatic linking considers only thoughts captured in the committing worktree (legacy rows without a recorded origin remain broadly eligible); parallel linked worktrees cannot claim each other's floating thoughts. Thoughts captured in a worktree that is later removed stay floating rather than being linked elsewhere.

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

`install` writes `PostToolUse`, `Stop`, and `SessionEnd` hook entries into this checkout's `.claude/settings.local.json`, using the absolute path of the running `cvc` binary (override with `--binary <absolute path>`), and ensures that file is excluded from Git through the repository's `info/exclude`: a machine-specific path must never be committed. The install is per checkout, so a fresh linked worktree needs its own `cvc harness install`. It requires an initialized repository and the capture acknowledgement described below, and refuses to install hooks that would only report `consent-required`. Claude Code picks the entries up when it next reloads its settings; if a running session does not, start a new session.

Each hook runs `cvc ingest claude-code --hook`, which reads Claude Code's JSON payload from stdin, discovers the repository from the payload's working directory, and ingests the transcript from the session's stored cursor. Hook runs are silent on success. A transcript this version does not understand (another major version, or an unknown message shape) fails loudly with a non-zero status that Claude Code shows in the session without blocking it; CVC never returns the blocking status, and a failed run writes nothing. `PostToolUse` keeps ingestion near-real-time so a commit made mid-session can still link the reasoning that preceded it; `Stop` and `SessionEnd` close the final response of a turn or session. Subagent hook invocations are ignored, and subagent transcripts are not ingested.

Record a finished session, or one that ran before the hooks were installed, explicitly:

```bash
cvc ingest claude-code --transcript ~/.claude/projects/<workspace>/<session>.jsonl
```

Ingestion is idempotent: every assistant response has a deterministic id within its session, so re-running never duplicates or replaces a thought, and never disturbs commit links it has already earned. One thought is recorded per assistant response: the prompt or tool results it reacted to become the prompt, its exposed thinking becomes `model_cot`, its text becomes the response, and its tool calls become tool executions carrying the status of their results. Paths the session read or edited inside the worktree become file context, subject to `.thoughtignore` `path:` rules; tool output is rendered into the following thought's prompt with bounded size. The conversation id is the Claude Code session id, so `cvc conversations` and `cvc share` work on it directly. Captures follow the usual boundaries: private by default, scrubbed on the way in, shared only per conversation and destination, and attributed to the worktree the hook ran in, so parallel checkouts cannot claim each other's thoughts.

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

Non-interactive input is rejected for acknowledgements. This intentionally prevents scripts, IDEs, and MCP clients from silently granting capture, sharing, or auto-push consent.

## Share and publish

Sharing records an exact private conversation snapshot for one destination. Future turns stay private unless requested explicitly:

```bash
cvc share <conversation-id> --remote origin
cvc share <conversation-id> --remote origin --future
cvc unshare <conversation-id> --remote origin
```

Discover conversation ids with `cvc conversations`, or run `cvc share` with no id from a terminal for an interactive picker over the destination's unshared conversations. The picker is TTY-only and feeds the same typed challenge, so non-interactive callers must always name a conversation explicitly; before the challenge, `share` prints the conversation's title, counts, and activity range so the consent is legible.

`share` requires the displayed TTY challenge, which includes the destination fingerprint and snapshot count. `unshare` makes only unpublished turns private; it cannot recall content already published.

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
- Secret scrubbing is defense in depth, not a guarantee that all sensitive material is detected or removed.
