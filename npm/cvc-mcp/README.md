# @volute_cvc/cvc-mcp

Launcher for [Volute CVC](https://github.com/meirka8/volute) (Cognitive Version Control). CVC records AI-assisted development context — prompts, responses, integration-exposed reasoning, and tool metadata — alongside Git history, private by default.

> **Using the `cvc` command line?** Install [`@volute_cvc/cvc`](https://www.npmjs.com/package/@volute_cvc/cvc) instead: `npm install --global @volute_cvc/cvc`. It is this same launcher published under a name for the CLI. This package, `@volute_cvc/cvc-mcp`, is still published with every release, so MCP client configurations that run `cvc-mcp` or `npx @volute_cvc/cvc-mcp` keep working unchanged.

This package downloads the released CVC binaries for your platform and runs them. It provides three commands:

- `cvc-mcp` — the MCP server, for coding agents that speak the Model Context Protocol.
- `cvc` — the CVC command-line interface (init, capture, sharing, and the Claude Code harness).
- `cvc-lsp` — the language server that editor integrations use.

It checks `~/.cvc/bin` first. Unless that already holds this release or a newer one, it fetches the archive matching this package's version from the project's GitHub release, verifies it against the release `SHA256SUMS.txt`, and keeps the verified binaries in a versioned cache under `~/.cvc/mcp-cache/`. It then installs them into `~/.cvc/bin`, the directory the installer scripts use, and runs them from there. Binaries that are already in `~/.cvc/bin` are run as they are: the launcher verifies only what it downloads. The checksum is an integrity check from the same origin as the archive, not an independent signature.

## Install

Run the MCP server without installing:

```bash
npx @volute_cvc/cvc-mcp
```

Or install all three commands on your `PATH`:

```bash
npm install --global @volute_cvc/cvc-mcp
```

`@volute_cvc/cvc` provides the same three commands, so install one of the two globally, not both: npm stops the second install with an `EEXIST` error.

Supported platforms: Linux x86-64, macOS x86-64, macOS ARM64, and Windows x86-64. Linux ARM64 is not published, and the launcher rejects an unsupported platform before downloading.

## Where the binaries run from

The launcher publishes `cvc`, `cvc-mcp`, and `cvc-lsp` to `~/.cvc/bin` (`%USERPROFILE%\.cvc\bin` on Windows), using the same staged copy, rename, and `.cvc-install.lock` as the installer scripts. Every install channel therefore ends up in one place, and the VS Code extension searches it first. `cvc harness install claude-code` records `~/.cvc/bin/cvc` in your Claude Code hooks, so an upgrade reaches the hooks without reinstalling them.

- **It replaces `~/.cvc/bin` only when it is missing, out of date, or unexecutable.** The launcher runs `~/.cvc/bin/cvc --version` before it downloads anything. If that reports the same complete release or a newer one, the launcher runs it as it is, with no download, so this also works offline. The package version is a minimum, not a pin, so an older launcher left in an MCP configuration cannot roll your hooks back. The launcher replaces the binaries only when `cvc` is missing, reports an older release (every release before v0.7.0 is recognised as older), reports this release with a sibling binary missing, or the system reports it as unexecutable (empty, no execute permission, or not an executable format for this system), and then only after the staged new `cvc` has reported the package version. Anything else that fails to report a version (it timed out, crashed, failed, printed something unrecognised, or failed because of the environment, such as a missing shared library or a `noexec` mount) is left in place: the launcher runs its own verified copy from the cache and prints one line to stderr saying why. To have the launcher replace such a file, delete `~/.cvc/bin/cvc`. If a newer `cvc` is installed without the binary being launched, the launcher runs its own, older, cached copy of that binary.
- **Publishing never fails a launch.** If the launcher cannot publish (another installation holds the lock, the directory is unsafe, or a file is in use on Windows), it runs the verified copy from the cache for that run and prints one line to stderr. Concurrent first launches wait briefly for each other instead of printing that line. A lock left behind by an interrupted launcher is usually recovered automatically on the next launch; it is not when the launcher that left it ran in a different PID namespace, such as under Flatpak. A lock held by the installer scripts is never removed by the launcher. When a lock looks stale, the message prints the exact command that removes it; run that command only if no installation is running. All launcher messages go to stderr, because stdout belongs to the MCP protocol.
- **Old caches stay.** The launcher does not remove versioned directories from `~/.cvc/mcp-cache/`, because hooks installed by v0.7.0 may still point into them.

## Use as an MCP server

Point your MCP client at the `cvc-mcp` command. A minimal client configuration:

```json
{
  "mcpServers": {
    "cvc": { "command": "cvc-mcp", "args": [], "env": { "RUST_LOG": "info" } }
  }
}
```

The server binds to the repository or worktree at its working directory and rejects a cross-repository or sibling-worktree target. Common client locations and the exact configuration steps are in [`cvc-mcp/README.md`](https://github.com/meirka8/volute/blob/main/cvc-mcp/README.md).

## Use the CLI

A global install also gives you `cvc`. To capture Claude Code sessions:

```bash
cvc init
cvc privacy acknowledge-capture
cvc harness install claude-code
```

Then start a new Claude Code session and run `cvc conversations`. See the [CLI README](https://github.com/meirka8/volute/blob/main/cvc-cli/README.md) for the full command set.

## Upgrading

Upgrade the package:

```bash
npm install --global @volute_cvc/cvc-mcp@latest
```

The package has no post-install step, so the new release reaches `~/.cvc/bin` only when the launcher next runs. Restart your MCP client, whose configuration runs the npm-installed `cvc-mcp`, or run the npm-installed `cvc` once: `"$(npm prefix -g)/bin/cvc" --version` on Linux and macOS, or `& "$(npm prefix -g)\cvc.cmd" --version` in PowerShell. Either one downloads and verifies the new release and publishes it to `~/.cvc/bin`, and hooks that point at `~/.cvc/bin/cvc` pick it up from their next run. Plain `cvc --version` is not enough if `~/.cvc/bin` comes first on your `PATH`, which is what the PATH setup offered by `install.sh` does: that runs the installed binary, which publishes nothing. Restarting the client also replaces its running `cvc-mcp` server, because a client keeps the old process until it restarts.

**From v0.7.0.** Hooks that v0.7.0 installed through this launcher point at its versioned cache (`~/.cvc/mcp-cache/v0.7.0-<arch>-<os>/cvc`), so they keep running v0.7.0 after an upgrade. After you upgrade the package, run this once in each checkout, and each linked worktree, where you installed the hooks:

```bash
cvc harness install claude-code
```

The install is idempotent. It rewrites the existing entries to `~/.cvc/bin/cvc` and reports them as updated. To check a checkout, look for `mcp-cache` in its `.claude/settings.local.json`; the step is needed only if that text appears.

## Privacy

Everything is private by default and fail-closed. Capture requires a one-time local acknowledgement, and remote sharing and auto-push each require a separate interactive challenge tied to the destination. No MCP client, IDE, or script can grant that consent. See [Privacy.md](https://github.com/meirka8/volute/blob/main/Privacy.md).

## Configuration

- `CVC_RELEASE_REPOSITORY` — override the `owner/repo` the binaries are fetched from.
- `CVC_RELEASE_BASE_URL` — override the download base URL (HTTPS only).

## License

Apache-2.0. Each published archive and this package include `THIRD-PARTY-NOTICES.md`.
