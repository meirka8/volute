# CVC MCP Server Setup

This guide explains how to build the `cvc-mcp` server and configure it for MCP-compatible clients such as VS Code, Cursor, and Claude Desktop.

## 1. Building the Server

First, build the release binary. Run this command from the project root (`/path/to/project/root`):

```bash
cargo build --release -p cvc-mcp
```

The binary will be located at:
`/path/to/project/root/target/release/cvc-mcp`

## 2. Configuration for IDEs and Claude Desktop

You need to add the server configuration to your MCP client config file. If you want to launch `cvc-mcp` from your PATH, install it globally via npm first: `npm install -g @volute_cvc/cvc-mcp`. If you prefer to use the binary you built above, point your client config at its absolute path instead.

**Config File Locations:**
- **VS Code / Cursor:** Typically in workspace `.vscode/mcp.json` or in client settings. Check your specific client documentation.
- **Claude Desktop:** `~/config/Claude/claude_desktop_config.json` (Linux) or `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS).

**Configuration JSON:**

Add the following entry to the `mcpServers` object:

```json
{
  "mcpServers": {
    "cvc": {
      "command": "cvc-mcp",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

## 3. Verification

After configuring, restart your IDE or Claude Desktop. Confirm these CVC tools appear:
- `commit_thought` — Save a concise task record, key reasoning, and optional result.
- `read_history` — Read recent saved CVC history for prior context or decisions.
- `get_context` — Inspect git-backed context for one file before summarizing or recording it.
- `setup_cvc` — Initialize CVC storage and hooks for the current repository.

## 4. Troubleshooting

If the server fails to start:
1.  Check the logs.
2.  Ensure the path to the binary is correct and absolute.
3.  Ensure the binary is executable (`chmod +x ...`).
4.  Try running the command manually in a terminal to check for startup errors.
