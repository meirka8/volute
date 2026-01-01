# CVC MCP Server Setup

This guide provides instructions on how to build the `cvc-mcp` server and configure it for use with IDEs like VS Code, Cursor, or Antigravity, as well as Claude Desktop.

## 1. Building the Server

First, build the release binary. Run this command from the project root (`/path/to/project/root`):

```bash
cargo build --release -p cvc-mcp
```

The binary will be located at:
`/path/to/project/root/target/release/cvc-mcp`

## 2. Configuration for IDEs and Claude Desktop

You need to add the server configuration to your MCP config file.

**Config File Locations:**
- **VS Code (Antigravity/Forks):** Typically inside `.vscode/mcp.json` or user settings. Check your specific extension documentation.
- **Claude Desktop:** `~/config/Claude/claude_desktop_config.json` (Linux) or `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS).

**Configuration JSON:**

Add the following entry to the `mcpServers` object:

```json
{
  "mcpServers": {
    "cvc": {
      "command": "/path/to/project/root/target/release/cvc-mcp",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

## 3. Verification

After configuring, restart your IDE or Claude Desktop. You should see the CVC tools available:
- `commit_thought`
- `read_history`
- `get_context`
- `setup_cvc`

## 4. Troubleshooting

If the server fails to start:
1.  Check the logs.
2.  Ensure the path to the binary is correct and absolute.
3.  Ensure the binary is executable (`chmod +x ...`).
4.  Try running the command manually in a terminal to check for startup errors.
