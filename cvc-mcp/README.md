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

## Troubleshooting

Ensure the binary path is executable and that the MCP process starts in the intended Git repository. Run the binary in a terminal and inspect logs if the client cannot start it.
