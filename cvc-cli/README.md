# CVC CLI

`cvc` manages a repository-local Cognitive Version Control cache. Captures are **private by default** in `.git/cvc/index.db`. Sync to `refs/cvc/main` is opt-in per destination and is not a substitute for a secret-management or deletion system.

## Installation

```bash
cargo install --path cvc-cli
```

## Basic commands

```bash
cvc init
cvc status
cvc log
cvc run -- <command> [args...]
cvc pull
```

`init` creates the local SQLite cache and hooks. `run` records its command and output as a private floating interaction. The post-commit linker may associate recent eligible interactions with a commit, but it does not share them. Configure its conservative window with `git config cvc.linkWindow <seconds>` (`0..=2592000`, default `86400`; `0` disables automatic linking).

## Privacy acknowledgement and destination consent

Inspect the local status for the selected remote (or the default remote):

```bash
cvc privacy status --remote origin
```

Passive VS Code collection is disabled until the repository owner completes:

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

That manual, destination-consented push projects the tombstone. Fetching on a later `redact` invocation must observe that tombstone in the v4 baseline before the command can build a protected hard-redaction plan:

```bash
cvc redact <interaction-uuid> --remote origin --rewrite-plan ./redaction-plan.json
cvc redact-verify-plan ./redaction-plan.json --remote origin
cvc redact <interaction-uuid> --remote origin --rewrite-plan ./redaction-plan.json --apply-local
```

The plan file is written with mode `0600` on Unix. `redact-verify-plan` only checks that the remote tip remains current. `--apply-local` changes **only local** `refs/cvc/main`; neither plan command pushes or force-pushes. `cvc delete-local` creates local suppression only and never propagates. A tombstone is suppression, not physical erasure, and a current-ref replacement is not guaranteed deletion.

The local SQLite cache enables `secure_delete` and, after deletion, attempts a truncating WAL checkpoint and `VACUUM` compaction. This is best effort only: residual filesystem blocks, SSD wear leveling, snapshots, failed-operation WAL remnants, backups, and Git objects may still retain data.

If credentials may have been exposed, rotate them first. Git-host support/removal is best effort; clones, forks, reflogs, caches, backups, and host object retention may retain content. Remote hard rewrite is **NOT implemented** pending an atomic force-with-lease design. Do not use blind force-push commands.

## Troubleshooting

- Run `cvc init` in a Git repository before using capture or sync commands.
- Ensure the configured remote permits `refs/cvc/main` publication.
- Secret scrubbing is defense in depth, not a guarantee that all sensitive material is detected or removed.
