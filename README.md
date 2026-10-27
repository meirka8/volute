# CVC

CVC (Cognitive Version Control) stores explicit AI-assisted development context alongside Git history. It maintains a repository-local SQLite cache and can project content that a user explicitly authorizes into the Git object database under `refs/cvc/main`.

CVC records only data exposed or supplied by its integrations. It does not retrieve hidden model chain-of-thought, and it does not promise a verbatim record of a black-box model's internal reasoning.

## What is in this repository

- `cvc-core`: synchronous Rust library for local storage, Git integration, policy enforcement, linking, and sync data structures.
- `cvc-cli`: the `cvc` command-line interface.
- `cvc-mcp`: an MCP server for explicitly submitted interaction records and local history access.
- `cvc-lsp`: the language server used by editor integrations.
- `cvc-plugin`: VS Code extension for the supported local VS Code chat-session workflow.
- `.claude-plugin/` and `plugins/claude-code`: the `volute` Claude Code plugin marketplace and its `cvc` Claude Code plugin, which installs the Claude Code capture hooks (a different thing from the VS Code extension above; see [Capture Claude Code sessions](#capture-claude-code-sessions)).
- `cvc-reviewer`: client-side React reviewer for GitHub-hosted CVC data.
- `npm/cvc-mcp`: npm launcher package for released CVC binaries, published as `@volute_cvc/cvc` and `@volute_cvc/cvc-mcp`.

## Install a release

Install the launcher from npm. On first run it downloads the release for your platform, verifies it, and keeps `cvc`, `cvc-mcp`, and `cvc-lsp` in `~/.cvc/bin`:

```bash
npm install --global @volute_cvc/cvc
cvc --version
```

If you already installed `@volute_cvc/cvc-mcp` globally, run `npm uninstall --global @volute_cvc/cvc-mcp` first: the two packages provide the same commands, so npm reports `EEXIST`. MCP configurations that run `npx @volute_cvc/cvc-mcp` keep working.

Alternatively, download the archive for your platform directly from [GitHub Releases](https://github.com/meirka8/volute/releases/latest):

| Platform | Release asset |
| --- | --- |
| Linux x86-64 | `cvc-x86_64-unknown-linux-gnu.tar.gz` |
| macOS x86-64 | `cvc-x86_64-apple-darwin.tar.gz` |
| macOS ARM64 | `cvc-aarch64-apple-darwin.tar.gz` |
| Windows x86-64 | `cvc-x86_64-pc-windows-msvc.zip` |

Linux ARM64 is not currently released. The repository's `install.sh`, `install.ps1`, and npm launcher support the same platform matrix and reject unsupported platforms before downloading an asset. The `install.sh` and `install.ps1` scripts also copy the `LICENSE` and `THIRD-PARTY-NOTICES.md` that each archive carries into a `share/cvc` directory beside the install directory (`~/.cvc/share/cvc` by default, `%USERPROFILE%\.cvc\share\cvc` on Windows), print where they put them, and still install an older archive that lacks them.

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

CVC can record Claude Code sessions from the transcript Claude Code writes, through its own lifecycle hooks, with no cooperation from the model. Two routes put those hooks in place: a Claude Code plugin, installed once per machine from the `volute` Claude Code plugin marketplace in this repository, or `cvc harness install claude-code`, run in each checkout. Steps 1 and 2 are the same for both. From a Git working tree with `cvc` installed:

1. `cvc init` — create the local cache and the advisory Git hooks.
2. `cvc privacy acknowledge-capture` — the one-time local capture acknowledgement. It is an interactive typed challenge, and passive capture stays off until it is granted. Neither route's hooks run it, and installing either route changes no consent state: the command accepts its phrase only at an interactive terminal, which is a confirmation step, not a security boundary (see [Privacy.md](Privacy.md)).
3. Install the hooks, by one of:
   - **Claude Code plugin**, once per machine: in Claude Code, run `/plugin marketplace add meirka8/volute`, then `/plugin install cvc@volute`. The shell equivalents are `claude plugin marketplace add meirka8/volute` and `claude plugin install cvc@volute`.
   - **Per checkout**: `cvc harness install claude-code` — write the capture hooks into this checkout's `.claude/settings.local.json`, using the absolute path of the `cvc` you just ran. By default the installer scripts and the npm launcher put `cvc` in `~/.cvc/bin` and replace it in place on upgrade (the launcher on its next run), so hooks that point there follow upgrades.
4. Start a *new* Claude Code session, in the repository or one of its worktrees. A session already running does not pick up the new hooks, and a session is recorded in the repository it was started in.
5. Work as usual, then `cvc conversations` to see the captured sessions (in a terminal it opens an interactive view: `q` quits, and `--plain` prints the plain listing; see [Browse and share interactively](cvc-cli/README.md#browse-and-share-interactively)) and `cvc log` for detail.

**Which route.** Choose the plugin if you use Claude Code across many repositories or worktrees (the desktop app's worktree sessions included) and want one install per machine, with updates through `/plugin`. It needs `cvc` 0.7.1 or later in an absolute directory on the `PATH` Claude Code sees; `~/.cvc/bin` is where the installers and the npm launcher put it, and sessions started from the desktop app or an IDE inherit their environment from how that app was started. The plugin's hooks look for `cvc` only in absolute `PATH` directories: relative entries (empty, `.`, `bin`, a literal `~`) are skipped, so a project cannot supply `cvc` through them, while absolute directories on your `PATH` are trusted, including ones your tools add for a project, such as direnv, an activated virtualenv or `node_modules/.bin`. Choose `cvc harness install claude-code` if you want capture confined to specific checkouts, cannot or do not want to put `cvc` in an absolute directory on that `PATH` (its hooks pin an absolute binary path), or your organization restricts Claude Code plugins and marketplaces. Installing both is supported: both sets of hooks run for each event, in parallel. CVC serializes ingestion of a session with a per-session lock, and ingestion is idempotent: thought ids are derived from the session and response, so a second run over the same event adds no duplicate thoughts. The cost is one more short `cvc` process per event.

**Requirements, if you installed CVC with npm.** The npm packages `@volute_cvc/cvc` and `@volute_cvc/cvc-mcp` both provide `cvc`, `cvc-mcp` and `cvc-lsp`, and the `cvc` they put on `PATH` is a Node launcher. Each time it runs, it checks `~/.cvc/bin/cvc --version`. If that build is older than the package, it downloads the matching release, verifies it, caches it under `~/.cvc/mcp-cache/` and publishes it to `~/.cvc/bin` (never downgrading). Then it runs the binary. For the plugin, put `~/.cvc/bin` on Claude Code's `PATH` ahead of npm's global bin directory, and run `cvc --version` once after each npm install or upgrade. Otherwise every hook event starts the Node launcher first, which adds latency in every project, and the first event after an install or upgrade may download a release (network access and writes under `~/.cvc`) within the hook's 30 second budget.

**The Claude Code plugin.** At its default user scope the plugin's hooks run in every Claude Code session you start (terminal, the desktop app's Code tab, IDE extensions; per Claude Code's documentation, not cloud sessions), in every project and worktree. A session is recorded in the repository it was started in: the one that contains Claude Code's project directory (any of its worktrees counts, and so does any subdirectory). It is recorded there in full, including work the session does in other directories (`/add-dir` folders, other repositories it moves into), and the hook's working directory only decides which of that repository's worktrees a response is attributed to. A session started outside an initialized repository (in a parent folder, or in another project) is not recorded, even when it later works inside one, and another repository the session visits receives nothing. This is the scope the per-checkout install has too. So open Claude Code in the repository, or in one of its worktrees, rather than in a folder above it. For sessions started elsewhere the hooks record nothing and exit quietly, and the short `cvc` process that each hook event starts reads the hook's input only to find the working directory; [Privacy.md](Privacy.md) says exactly what it reads. In an initialized repository without the step 2 acknowledgement they report `consent-required` and record nothing, without blocking the session. In a repository where capture was already acknowledged (for the VS Code extension or a per-checkout install), the plugin starts recording Claude Code sessions from the next session, with no new prompt.

At session start a check can show you one line if `cvc` is not on the `PATH` Claude Code runs hooks with, if the only `cvc` there is in a relative `PATH` entry (which the plugin does not use), if the `cvc` found there could not be run, or if it is older than 0.7.1; each message says what to fix (the first and last point back to [Install a release](#install-a-release)), and the check is designed not to block the session (it always exits 0). If `cvc` is missing from that `PATH`, every `PostToolUse`, `Stop` and `SessionEnd` event also shows a non-blocking hook error, in every project (`cvc: command not found in an absolute PATH directory (the cvc plugin does not use relative PATH entries)`): fix the `PATH` or disable the plugin. The plugin's hooks never run the acknowledgement, share or publish commands, and installing it changes no consent state. Those commands accept their phrase only at an interactive terminal, which is a confirmation step, not a security boundary (see [Privacy.md](Privacy.md)). To update, run `/plugin marketplace update volute`: Claude Code does not auto-update third-party (non-Anthropic) marketplaces by default (per Claude Code's documentation). The marketplace pins the plugin to the CVC release tag, so you receive plugin changes with each CVC release: the plugin's files come only from the release tag, while the catalog entry that names it is read from `main`. To stop, run `/plugin disable cvc@volute` or `/plugin uninstall cvc@volute`; neither withdraws the step 2 acknowledgement. The plugin on one page: [`plugins/claude-code/README.md`](plugins/claude-code/README.md).

The plugin and the `volute` marketplace are independent of Anthropic: not made, reviewed, or endorsed by Anthropic, and not listed in Anthropic's plugin directory.

**The per-checkout install.** Step 3 also covers worktrees you create later. It records which `cvc` to run and adds CVC to Git's `post-checkout` hook. After that, `git worktree add` (and Claude Code's worktree option) gives the new worktree the same hooks, so its sessions are captured too. This holds only while the capture acknowledgement from step 2 stands. Worktrees that already exist are not changed: run `cvc harness install claude-code` in each one (the command lists them). If you set up hooks with v0.7.0, run it once more in your main checkout to turn this on.

To stop, run `cvc harness uninstall claude-code`. It removes this checkout's hooks and stops new worktrees from getting them. Other worktrees keep their hooks and keep capturing: the command lists them, and you run it in each one. It does not withdraw the step 2 acknowledgement.

Windows is untested for the Claude Code path ([#97](https://github.com/meirka8/volute/issues/97)); the plugin's README says what to expect there.

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

The CLI, MCP server, LSP, VS Code extension, and Claude Code harness adapter support linked worktrees. The VS Code extension intentionally supports one opened repository folder at a time; multi-root workspaces are not supported. MCP binds to one active repository/worktree and rejects cross-repository or sibling-worktree cwd targets. Claude Code hooks reach a session in one of two ways. `cvc harness install claude-code` writes them per checkout, and also makes a Git hook add them to each new worktree while the capture acknowledgement stands; existing worktrees need their own install. The Claude Code plugin is user-scoped instead: it needs no per-worktree install, its hooks run in every project, and it records nothing for a session started outside a CVC-initialized repository, while a session started inside one is recorded in full there, including work in other directories (see [Privacy.md](Privacy.md) for what each hook run does). Captures are attributed to the worktree they ran in. These boundaries do not change privacy, sharing, or auto-push semantics. Neither route's hooks run the acknowledgement, share or publish commands, and installing either changes no consent state. Those commands accept their phrase only at an interactive terminal, which is a confirmation step, not a security boundary (see [Privacy.md](Privacy.md)).

Secret scrubbing and `.thoughtignore` are defense in depth, not guarantees. They can miss credentials, personal data, encoded values, or provider-specific formats. Git publication can be difficult or impossible to erase completely from objects, clones, forks, reflogs, caches, and backups. Rotate exposed credentials first; tombstones and local deletion are suppression mechanisms, not guaranteed physical erasure.

Read the detailed [privacy reference](Privacy.md). Report vulnerabilities privately according to the [security policy](SECURITY.md).

## Contributing and license

Contributions require DCO sign-off. See [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md).

Licensed under the [Apache License 2.0](LICENSE).
