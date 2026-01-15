# Project Design: CVC VS Code Extension

## 1. Executive Summary

The **CVC VS Code Extension** is the primary "Head" of the system for developers using VS Code, Cursor, or Windsurf (where extensions are supported). It serves two critical functions:

1. **Input (The Native Delegate):** Captures high-fidelity intent via a native Chat Participant (`@cvc`), delegating to the user's existing LLM subscription while logging the thought process.
    
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

### 3.1 Feature A: The Native Chat Participant (`@cvc`)

Integrates directly into the VS Code GitHub Copilot Chat sidebar.

- **Invocation:** User types `@cvc` in the chat input.
    
- **Behavior:**
    
    1. **Interception:** Captures the user's prompt and attached context _before_ execution.
        
    2. **Logging (Start):** Sends `$/cvc/turn/start` to LSP.
        
    3. **Delegation:** Calls `vscode.lm.selectChatModels` to find the most capable model (e.g., `copilot-gpt-4`) and streams the prompt to it.
        
    4. **Streaming:** Streams the model's response back to the user UI while buffering it.
        
    5. **Logging (End):** Sends `$/cvc/turn/end` with the full response and CoT to LSP.
        

### 3.2 Feature B: The Cognitive Timeline (Side Panel)

A dedicated Tree View in the Side Bar (or Explorer Container).

- **Structure:**
    
    - **📂 Pending Thoughts (Staged/Floating)**
        
        - _Icon:_ 💭 (Thought Bubble)
            
        - _Content:_ Thoughts recorded since the last commit. These are currently "Unbound."
            
        - _Context:_ "Will be linked to next commit."
            
    - **📂 History (Bound)**
        
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
    

## 5. Security Strategy

- **Strict Content Security Policy (CSP):** The Webview used to display thought details must strictly disallow external scripts.
    
- **Sanitization:** Ensure markdown rendering in the Webview sanitizes HTML to prevent XSS from "poisoned" thoughts.