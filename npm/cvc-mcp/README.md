# @volute_cvc/cvc-mcp

Launcher for [Volute CVC](https://github.com/meirka8/volute) (Cognitive Version Control). CVC records AI-assisted development context — prompts, responses, integration-exposed reasoning, and tool metadata — alongside Git history, private by default.

This package downloads the released CVC binaries for your platform and runs them. It provides two commands:

- `cvc-mcp` — the MCP server, for coding agents that speak the Model Context Protocol.
- `cvc` — the CVC command-line interface (init, capture, sharing, and the Claude Code harness).

On first run it fetches the archive matching this package's version from the project's GitHub release, verifies it against the release `SHA256SUMS.txt`, and caches the binaries under `~/.cvc/mcp-cache/`. The checksum is an integrity check from the same origin as the archive, not an independent signature.

## Install

Run the MCP server without installing:

```bash
npx @volute_cvc/cvc-mcp
```

Or install both commands on your `PATH`:

```bash
npm install --global @volute_cvc/cvc-mcp
```

Supported platforms: Linux x86-64, macOS x86-64, macOS ARM64, and Windows x86-64. Linux ARM64 is not published, and the launcher rejects an unsupported platform before downloading.

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

## Privacy

Everything is private by default and fail-closed. Capture requires a one-time local acknowledgement, and remote sharing and auto-push each require a separate interactive challenge tied to the destination. No MCP client, IDE, or script can grant that consent. See [Privacy.md](https://github.com/meirka8/volute/blob/main/Privacy.md).

## Configuration

- `CVC_RELEASE_REPOSITORY` — override the `owner/repo` the binaries are fetched from.
- `CVC_RELEASE_BASE_URL` — override the download base URL (HTTPS only).

## License

Apache-2.0. Each published archive and this package include `THIRD-PARTY-NOTICES.md`.
