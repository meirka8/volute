# CVC Privacy and `.thoughtignore` Reference

This reference documents the privacy behavior implemented in this repository. CVC is local-first, not local-only: local capture and remote publication are separate operations with separate consent boundaries.

## How your data moves

Four verbs, four boundaries — worth internalizing before the detailed reference below:

1. **Capture** writes an interaction into the local SQLite database immediately, as the conversation happens. Content is scrubbed on the way in and is private by default. It exists nowhere in Git: `git push` cannot transport it, and a clone does not receive it.
2. **Link** — the automatic post-commit hook — records which commit a thought belongs to. Linking is provenance bookkeeping inside the local database; it has no effect on visibility or sharing.
3. **Share** records consent marks in the local database: *these turns of this conversation are authorized for this exact destination fingerprint.* Nothing is exported, and nothing touches Git. New conversations are never auto-shared; each is a fresh, deliberate decision.
4. **Push** — `cvc push --manual` with its interactive challenge, or a destination whose auto-push was separately acknowledged — is the only step that creates Git objects. It fetches the destination's current CVC ref as a baseline, projects shared-but-unpublished content into `refs/cvc/main`, and pushes that one ref. A thought that is never shared, or shared but never pushed, never becomes a Git object at all.

On another machine, `cvc pull` fetches the ref and imports it into that machine's local cache; receiving content grants no authority for any destination.

The consequence to hold onto: Git objects are the durable, hard-to-erase form of this data, and they are minted only at the moment of authorized publication — behind the last human gate. Everything before that point is locally deletable database state.

## Capture and sharing

Every production aggregate capture is prepared and scrubbed before its SQLite transaction. Captures are private by default. Passive VS Code capture additionally requires the repository-local interactive capture acknowledgement. Sharing, consent, auto-push, publication state, and remote tombstone authority are destination-specific local state; they are not synced as general permission grants. Derived links and FORMAT5 derivation evidence never create, broaden, or imply share consent.

The database, privacy/consent state, locks, and rewrite state live in `$(git rev-parse --git-common-dir)/cvc`, so linked Git worktrees share them. Normal CVC Git refs, including `refs/cvc/main`, remain in the shared Git refs namespace rather than that directory. Hooks use Git's effective hooks path (`<common-dir>/hooks` by default, or `core.hooksPath`; relative hook paths are active-worktree-relative). `.thoughtignore`, captured context resolution, `HEAD`, index, and branch state are local to the active worktree. CLI, MCP, LSP, and VS Code linked-worktree support does not change the private-by-default, sharing, or auto-push boundaries.

Destination authority is isolated: a share, authorization, publication observation, or tombstone received from destination A has no authority for destination B. Remote evidence is recorded as an untrusted remote observation unless independently observed locally; it is not a permission grant.

The scrubber uses bounded, high-confidence built-in detectors for several private-key, token, authorization, URL-credential, and credential-assignment forms, plus credential-bearing JSON keys. It is defense in depth, not proof that all secrets, PII, encodings, or provider-specific formats are found. Built-in protection cannot be disabled by policy.

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
