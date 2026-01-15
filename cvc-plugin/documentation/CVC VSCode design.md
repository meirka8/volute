# Project Design: Volute VC VS Code Extension

## 1. Executive Summary

The **Volute VC VS Code Extension** is the primary "Head" of the system for developers using VS Code, Cursor, or Windsurf (where extensions are supported). It serves two critical functions:

1. **Input (The Passive Observer):** Silently observes GitHub Copilot Chat conversations by monitoring VS Code's chat session storage files, capturing prompts, responses, and chain-of-thought without interfering with the user's workflow.

2. **Visualization (The Cognitive Timeline):** Provides a real-time Side Panel view of the "Shadow Graph," showing both **Pending Thoughts** (uncommitted reasoning) and **Historical Context** (thoughts linked to previous commits).


## 2. Architecture & Communication

The extension functions as a **Thick Client** for the `cvc-lsp` server. It does not access the SQLite database directly; all data requests are routed through JSON-RPC over the Language Server Protocol.

### 2.1 The LSP Bridge

- **Client:** The VS Code Extension (`LanguageClient`).

- **Server:** `cvc-lsp` (Rust Binary).

- **Transport:** Stdio (Standard Input/Output).


### 2.2 Protocol Extensions

To support the UI features, we extend the standard LSP with custom methods:

- **`cvc/timeline/get` (Request):**

    - _Client -> Server_: Asks for the interaction history relative to the current Git HEAD.

    - _Params:_ `{ maxItems: 50, includeUnbound: true }`

    - _Returns:_ A structured tree of commits and their linked thoughts, plus a bucket of "Floating" thoughts.

- **`cvc/timeline/refresh` (Notification):**

    - _Server -> Client_: Signals that the DB has changed (e.g., a CLI push happened, or a new thought was recorded).

    - _Action:_ Extension triggers a refresh of the Tree View.


## 3. Feature Specifications

### 3.1 Feature A: The Chat Session Watcher (Primary - Passive Observer)

**The Invisible Stenographer** - Silently monitors GitHub Copilot Chat without any user intervention.

- **Mechanism:**

    1. **Discovery:** On activation, locates the VS Code workspace storage directory containing `chatSessions/*.json` files.
    
    2. **File Watching:** Uses `vscode.workspace.createFileSystemWatcher()` to monitor the `chatSessions` directory for changes.
    
    3. **Parsing:** When a chat session file is modified, parses the JSON to extract new interactions.
    
    4. **Logging:** Sends `$/cvc/turn/start` and `$/cvc/turn/end` notifications to the LSP with extracted data.

- **Data Extracted:**

    - `requests[].message.text` - User prompt
    - `requests[].variableData.variables` - File references and context
    - `requests[].response[].kind: "thinking"` - Chain of thought (when available)
    - `requests[].response[].value` - Model response (markdown content)
    - `inputState.selectedModel` - Model identifier

- **Chat Session File Location:**

    - **Linux:** `~/.config/Code/User/workspaceStorage/<workspace-id>/chatSessions/*.json`
    - **Windows:** `%APPDATA%\Code\User\workspaceStorage\<workspace-id>\chatSessions\*.json`
    - **macOS:** `~/Library/Application Support/Code/User/workspaceStorage/<workspace-id>/chatSessions/*.json`

- **Benefits:**

    - Zero interference with Copilot's capabilities (RAG, tools, workspace indexing)
    - Captures full chain-of-thought from models that expose it
    - Works with any model the user selects
    - Completely invisible to the user


### 3.2 Feature B: The Native Chat Participant (`@volute`) - Alternative/Explicit Mode

Integrates directly into the VS Code GitHub Copilot Chat sidebar as an **explicit logging mode**.

- **Invocation:** User types `@volute` in the chat input.

- **Use Case:** When user wants explicit control over logging, or when passive observation is insufficient.

- **Behavior:**

    1. **Interception:** Captures the user's prompt and attached context.

    2. **Logging (Start):** Sends `$/cvc/turn/start` to LSP.

    3. **Delegation:** Calls `vscode.lm.selectChatModels` to find the most capable model and streams the prompt to it.

    4. **Streaming:** Streams the model's response back to the user UI while buffering it.

    5. **Logging (End):** Sends `$/cvc/turn/end` with the full response to LSP.

- **Trade-offs:**

    - Loses some Copilot intelligence (workspace RAG, advanced tools)
    - Provides explicit user control over what gets logged
    - Useful for sensitive conversations where passive observation should be disabled


### 3.3 Feature C: The Cognitive Timeline (Side Panel)

A dedicated Tree View in the Side Bar (or Explorer Container).

- **Structure:**

    - **Pending Thoughts (Staged/Floating)**

        - _Icon:_ Thought Bubble

        - _Content:_ Thoughts recorded since the last commit. These are currently "Unbound."

        - _Context:_ "Will be linked to next commit."

    - **History (Bound)**

        - **Commit: `feat: add auth` (a1b2c)**

            - _Thought:_ "Refactor to use JWT..."

            - _Thought:_ "Fix race condition..."

        - **Commit: `fix: login bug` (d4e5f)**

            - _Thought:_ "Investigate 401 error..."

- **Interaction:**

    - **Click:** Opens a **Webview Panel** displaying the full detailed view of the interaction (Prompt, Context Files, Chain of Thought, Tool Outputs).

    - **Context Menu:**

        - _Pending Items:_ "Delete Thought" (Garbage collection).

        - _History Items:_ "Copy Prompt", "View Diff".


## 4. UX Doctrine: "The Invisible Scribe"

The extension should feel invisible until needed.

- **Zero Latency:** The Timeline updates optimistically or asynchronously. It must not block the editor.

- **Native Look:** Use VS Code's native Tree View API and Webview UI Toolkit (`@vscode/webview-ui-toolkit`) to look exactly like built-in features.

- **No Configuration:** It automatically finds the `cvc-lsp` binary (or downloads it) and connects.

- **Silent Operation:** The Chat Session Watcher operates entirely in the background with no user prompts or notifications unless errors occur.


## 5. Security Strategy

- **Strict Content Security Policy (CSP):** The Webview used to display thought details must strictly disallow external scripts.

- **Sanitization:** Ensure markdown rendering in the Webview sanitizes HTML to prevent XSS from "poisoned" thoughts.

- **Local Only:** Chat session files are read locally; no data is sent externally except to the local LSP server.


## 6. Implementation Notes

### 6.1 Workspace Storage Discovery

The workspace storage ID is derived from the workspace folder URI. The extension must:

1. Get the current workspace folder URI
2. Hash it to find the corresponding storage directory
3. Or enumerate storage directories and match by checking contained workspace metadata

### 6.2 Deduplication

The Chat Session Watcher must track which interactions have already been logged to avoid duplicates:

- Maintain a set of processed `requestId` values per session
- Only send new interactions to the LSP
- Persist processed IDs across extension restarts (optional, can re-process on restart)

### 6.3 Debouncing

Chat session files are written frequently during streaming responses. The watcher should:

- Debounce file change events (e.g., 500ms-1s delay)
- Only process complete responses (check for response completion markers)
