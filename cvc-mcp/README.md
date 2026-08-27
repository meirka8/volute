# CVC MCP Server Setup

`cvc-mcp` lets an MCP-compatible client store concise interaction records in the current repository's local CVC cache. MCP captures are **private by default** and are sanitized before persistence. The server cannot grant capture acknowledgement, remote sharing consent, conversation sharing, or auto-push.

## Build and configure

```bash
cargo build --release -p cvc-mcp
```

Configure the built binary, or the `cvc-mcp` executable installed from npm, in the MCP client:

```json
{
  "mcpServers": {
    "cvc": {
      "command": "cvc-mcp",
      "args": [],
      "env": { "RUST_LOG": "info" }
    }
  }
}
```

Common client locations include workspace `.vscode/mcp.json`, client settings, and Claude Desktop's configuration file. Use the client documentation for its exact location.

## Tools and privacy boundary

- `commit_thought` saves a concise task record, reasoning supplied by the client, and an optional result locally.
- `read_history` reads local saved history.
- `get_context` inspects Git-backed context for one file.
- `setup_cvc` initializes CVC storage and hooks.

`commit_thought` is not a promise to capture hidden model chain-of-thought; it only records fields the MCP client explicitly supplies. Repository `.thoughtignore` and built-in scrubbing apply before the SQLite transaction. Scrubbing is defense in depth, not a guarantee that every secret or sensitive value is recognized.

The MCP server does not publish CVC data. Sync import may fetch an already-shared `refs/cvc/main` projection, but importing does not make local captures shared and does not authorize a destination. Use the interactive CLI for acknowledgement, `cvc share ... --remote ...`, and manual or separately enabled auto-push.

## Repository binding and linked worktrees

CVC stores its database, privacy/consent state, locks, and rewrite state under `$(git rev-parse --git-common-dir)/cvc`; linked worktrees therefore share that storage. Normal CVC refs such as `refs/cvc/main` are shared Git refs, not files under `cvc`. Hooks use Git's effective hooks path (`<common-dir>/hooks` by default, or `core.hooksPath`; relative paths are active-worktree-relative). `.thoughtignore`, context, `HEAD`, index, and branch remain local to the active worktree. One MCP server process binds to its active repository/worktree and rejects cross-repository or sibling-worktree cwd targets. This is not multi-repository support and does not change private-by-default capture, sharing consent, or auto-push behavior. CLI, MCP, LSP, and VS Code support linked worktrees; the VS Code extension remains single-folder rather than multi-root.

## Troubleshooting

Ensure the binary path is executable and that the MCP process starts in the intended Git repository. Run the binary in a terminal and inspect logs if the client cannot start it.
