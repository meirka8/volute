# CVC

CVC (Cognitive Version Control) stores explicit AI-assisted development context alongside Git history. It maintains a repository-local SQLite cache and can project content that a user explicitly authorizes into the Git object database under `refs/cvc/main`.

CVC records only data exposed or supplied by its integrations. It does not retrieve hidden model chain-of-thought, and it does not promise a verbatim record of a black-box model's internal reasoning.

## What is in this repository

- `cvc-core`: synchronous Rust library for local storage, Git integration, policy enforcement, linking, and sync data structures.
- `cvc-cli`: the `cvc` command-line interface.
- `cvc-mcp`: an MCP server for explicitly submitted interaction records and local history access.
- `cvc-lsp`: the language server used by editor integrations.
- `cvc-plugin`: VS Code extension for the supported local VS Code chat-session workflow.
- `cvc-reviewer`: client-side React reviewer for GitHub-hosted CVC data.
- `npm/cvc-mcp`: npm launcher package for released CVC binaries.

## Install a release

Download the archive for your platform directly from [GitHub Releases](https://github.com/meirka8/volute/releases/latest):

| Platform | Release asset |
| --- | --- |
| Linux x86-64 | `cvc-x86_64-unknown-linux-gnu.tar.gz` |
| macOS x86-64 | `cvc-x86_64-apple-darwin.tar.gz` |
| macOS ARM64 | `cvc-aarch64-apple-darwin.tar.gz` |
| Windows x86-64 | `cvc-x86_64-pc-windows-msvc.zip` |

Linux ARM64 is not currently released. The repository's `install.sh`, `install.ps1`, and npm launcher support the same platform matrix and reject unsupported platforms before downloading an asset.

Extract the archive and put `cvc` (or `cvc.exe`) on `PATH`. Each published binary archive contains `cvc`, `cvc-mcp`, and `cvc-lsp`. Verify the archive against the `SHA256SUMS.txt` asset attached to the same release before extracting it. Then initialize CVC from a Git working tree:

```bash
cvc init
cvc status
```

To record one command explicitly:

```bash
cvc run -- <command> [args...]
cvc log
```

### Capture Claude Code sessions

CVC records each Claude Code session from the transcript Claude Code writes, through its own lifecycle hooks, with no cooperation from the model. From a Git working tree with `cvc` installed:

1. `cvc init` — create the local cache and the advisory Git hooks.
2. `cvc privacy acknowledge-capture` — the one-time local capture acknowledgement. It is an interactive typed challenge, and passive capture stays off until it is granted.
3. `cvc harness install claude-code` — write the capture hooks into this checkout's `.claude/settings.local.json`, using the absolute path of the `cvc` you just ran. By default the installer scripts and the npm launcher put `cvc` in `~/.cvc/bin` and replace it in place on upgrade (the launcher on its next run), so hooks that point there follow upgrades.
4. Start a *new* Claude Code session. A session already running does not pick up the new hooks.
5. Work as usual, then `cvc conversations` to see the captured sessions (in a terminal it opens an interactive view: `q` quits, and `--plain` prints the plain listing; see [Browse and share interactively](cvc-cli/README.md#browse-and-share-interactively)) and `cvc log` for detail.

Step 3 also covers worktrees you create later. It records which `cvc` to run and adds CVC to Git's `post-checkout` hook. After that, `git worktree add` (and Claude Code's worktree option) gives the new worktree the same hooks, so its sessions are captured too. This holds only while the capture acknowledgement from step 2 stands. Worktrees that already exist are not changed: run step 3 in each one (the command lists them). If you set up hooks with v0.7.0, run step 3 once more in your main checkout to turn this on.

To stop, run `cvc harness uninstall claude-code`. It removes this checkout's hooks and stops new worktrees from getting them. Other worktrees keep their hooks and keep capturing: the command lists them, and you run it in each one. It does not withdraw the step 2 acknowledgement.

See [`cvc-cli/README.md`](cvc-cli/README.md#agent-harness-capture-claude-code) for the limits and edge cases, and for sharing, consent, linking, and redaction. See [`cvc-mcp/README.md`](cvc-mcp/README.md) for MCP configuration.

## Build and test from source

Install a current stable Rust toolchain, then run from the repository root:

```bash
cargo build --workspace
cargo test --workspace
```

Install the locally built CLI with:

```bash
cargo install --path cvc-cli --locked
```

The JavaScript projects require a supported Node.js/npm installation:

```bash
cd cvc-plugin && npm ci && npm run compile
cd ../cvc-reviewer && npm ci && npm run build && npm test -- --run
```

These are independent project directories; a Rust workspace build does not build the VS Code extension or reviewer.

## Privacy and security

CVC is **local-first**, not local-only. The database, privacy/consent state, locks, and rewrite state are in `$(git rev-parse --git-common-dir)/cvc` and shared by linked worktrees. Normal CVC Git refs such as `refs/cvc/main` are in Git's shared refs namespace, not that directory. Hooks use Git's effective hooks path (`<common-dir>/hooks` by default, or `core.hooksPath`; relative hook paths are active-worktree-relative). `.thoughtignore`, context, `HEAD`, index, and branch state remain local to the active worktree. Captures are private by default, but explicit sharing/sync can publish selected data through Git. Commands that fetch from or push to a Git remote, such as `cvc push` and `cvc pull`, can also use the network, and the installers and npm launcher download release archives from GitHub. Review data before publication and restrict access to the repository and its Git common directory.

The CLI, MCP server, LSP, VS Code extension, and Claude Code harness adapter support linked worktrees. The VS Code extension intentionally supports one opened repository folder at a time; multi-root workspaces are not supported. MCP binds to one active repository/worktree and rejects cross-repository or sibling-worktree cwd targets. Claude Code hooks are written per checkout. `cvc harness install claude-code` also makes a Git hook add them to each new worktree while the capture acknowledgement stands; existing worktrees need their own install. Captures are attributed to the worktree they ran in. These boundaries do not change privacy, sharing, or auto-push semantics.

Secret scrubbing and `.thoughtignore` are defense in depth, not guarantees. They can miss credentials, personal data, encoded values, or provider-specific formats. Git publication can be difficult or impossible to erase completely from objects, clones, forks, reflogs, caches, and backups. Rotate exposed credentials first; tombstones and local deletion are suppression mechanisms, not guaranteed physical erasure.

Read the detailed [privacy reference](Privacy.md). Report vulnerabilities privately according to the [security policy](SECURITY.md).

## Contributing and license

Contributions require DCO sign-off. See [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md).

Licensed under the [Apache License 2.0](LICENSE).
