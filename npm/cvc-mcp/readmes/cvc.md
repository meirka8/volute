# @volute_cvc/cvc

The `cvc` command-line tool for [Volute CVC](https://github.com/meirka8/volute) (Cognitive Version Control). CVC records AI-assisted development context — prompts, responses, integration-exposed reasoning, and tool metadata — alongside Git history, private by default.

This package is a launcher: it downloads the released CVC binaries for your platform, verifies them, and runs them. It is the same launcher that is published as [`@volute_cvc/cvc-mcp`](https://www.npmjs.com/package/@volute_cvc/cvc-mcp), under a name for command-line users.

## Install

```bash
npm install --global @volute_cvc/cvc
cvc --version
```

Or run it once without installing: `npx @volute_cvc/cvc --version`.

The first run downloads the release that matches this package's version, so it needs network access once. Supported platforms: Linux x86-64, macOS x86-64, macOS ARM64, and Windows x86-64. Linux ARM64 is not published, and the launcher rejects an unsupported platform before downloading.

Install this package or `@volute_cvc/cvc-mcp` globally, not both. They provide the same three commands, so npm stops the second install with an `EEXIST` error.

## Commands

- `cvc` — the CVC command-line interface (init, capture, sharing, and the Claude Code harness).
- `cvc-mcp` — the MCP server, for coding agents that speak the Model Context Protocol.
- `cvc-lsp` — the language server that editor integrations use.

## Get started

From a Git working tree:

```bash
cvc init
cvc privacy acknowledge-capture
cvc harness install claude-code
```

Then start a new Claude Code session and run `cvc conversations`. The [repository README](https://github.com/meirka8/volute#capture-claude-code-sessions) explains each step, including `cvc init`, and the [CLI README](https://github.com/meirka8/volute/blob/main/cvc-cli/README.md) covers the full command set.

## Where the binaries run from

The launcher keeps `cvc`, `cvc-mcp`, and `cvc-lsp` in `~/.cvc/bin` (`%USERPROFILE%\.cvc\bin` on Windows), the directory the `install.sh` and `install.ps1` installer scripts use, and runs them from there. It downloads and replaces them only when that directory is missing, out of date, or unexecutable, and it never downgrades. `cvc harness install claude-code` records `~/.cvc/bin/cvc` in your Claude Code hooks, so an upgrade reaches the hooks without reinstalling them. What the launcher does when it cannot publish, how it verifies a download, and its configuration variables are in the [launcher README](https://github.com/meirka8/volute/blob/main/npm/cvc-mcp/README.md).

## Upgrading

```bash
npm install --global @volute_cvc/cvc@latest
```

The package has no post-install step, so the new release reaches `~/.cvc/bin` only when the npm-installed `cvc` next runs: `"$(npm prefix -g)/bin/cvc" --version` on Linux and macOS, or `& "$(npm prefix -g)\cvc.cmd" --version` in PowerShell. Plain `cvc --version` is not enough if `~/.cvc/bin` comes first on your `PATH`, which is what the PATH setup offered by `install.sh` does: that runs the installed binary, which publishes nothing.

## Already using `@volute_cvc/cvc-mcp`?

MCP client configurations that run `cvc-mcp` or `npx @volute_cvc/cvc-mcp` keep working, and that package is still published with every release. To move a global install over for the command line:

```bash
npm uninstall --global @volute_cvc/cvc-mcp
npm install --global @volute_cvc/cvc
```

## Privacy

Everything is private by default and fail-closed. Capture requires a one-time local acknowledgement, and remote sharing and auto-push each require a separate interactive challenge tied to the destination. See [Privacy.md](https://github.com/meirka8/volute/blob/main/Privacy.md).

## License

Apache-2.0. Each published archive and this package include `THIRD-PARTY-NOTICES.md`.
