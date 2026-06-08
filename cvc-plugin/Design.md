# Project Design Specification: Cognitive Version Control (CVC)

## 1. Executive Summary

**Cognitive Version Control (CVC)** is a system designed to augment standard Git version control by tracking the "trajectory of intent" alongside the "history of artifacts." While Git captures **what** changed (diffs) and **when**, CVC captures **why** (prompts, context, and AI Chain-of-Thought).

The system operates as a "Shadow DAG" (Directed Acyclic Graph), creating a parallel history of reasoning that is tightly coupled with, but distinct from, the Git commit graph.

## 2. Core Philosophy

- **The Artifact vs. The Process:** Code is the artifact; Intelligence is the process. We treat the _process_ as a first-class citizen in version control.
    
- **The Missing Layer:** Modern development is a dialogue between human intent and AI generation. Losing this dialogue is akin to losing the mathematical proof while keeping only the theorem.
    
- **Native Integration:** The system must feel like a natural extension of Git, utilizing similar CLI patterns and storage locations.
    

## 3. Architecture: The "Double Helix" Model

The system maintains two intertwined graphs:

1. **The Git Graph:** The standard history of file snapshots (Blobs/Trees/Commits).
    
2. **The Cognitive Graph:** A history of interaction nodes (Prompts/Reasoning/Context).
    

### 3.1 The Unit of Change: The "Interaction Node"

Unlike Git, where the atomic unit is a snapshot of files, the atomic unit in CVC is an **Interaction Node**.

A Node consists of:

- **User Intent (The Stimulus):** The raw prompt, instruction, or signal that triggered the thought.
    
    - _Human:_ "Refactor this."
        
    - _System/Agent:_ Ticket description, Tool output, or Compiler error.
        
- **System State (The Context):** The precise scope of the codebase exposed to the AI.
    
    - _Explicit Context:_ Files or code regions manually attached by the user.
        
    - _Dynamic Context:_ Files read or discovered by the agent during execution (e.g., via tool use).
        
- **The Derivation:** The AI's hidden "Chain of Thought" (CoT) and final response.
    
- **The Outcome:** A linkage to the resulting Git Commit SHA.
    
    - _Cardinality:_ This is a **Many-to-One** relationship. A lengthy dialogue (multiple interaction nodes) often distills down into a single Git commit.
        

### 3.2 Storage Strategy: Hybrid Relational/CAS

To balance performance with query capability, we utilize a hybrid approach:

- **Database:** SQLite (`.git/cvc/index.db`) for the graph structure, metadata, and relational queries.
    
- **Context Deduplication (The Git Link):** To avoid data bloat, we do **not** duplicate file content in SQLite if it already exists in Git.
    
    - _Clean State:_ If the context file matches a known Git Blob, we store the Blob SHA.
        
    - _Dirty State:_ If the user prompts while the file has uncommitted changes, we store a **diff/patch** in SQLite against the nearest Blob SHA.
        

## 4. Technical Stack

|   |   |   |
|---|---|---|
|**Component**|**Technology**|**Rationale**|
|**Core Logic & CLI**|**Rust**|System-level performance, memory safety, and excellent bindings for git internals (`git2` crate).|
|**Data Storage**|**SQLite**|Single-file portability, relational capabilities for complex querying ("Find all prompts regarding `auth.rs`"), and ACID compliance.|
|**Git Interface**|**libgit2**|Direct, high-performance interaction with the Git repository without shelling out to binaries.|
|**IDE Integration**|**LSP (Language Server Protocol)**|Allows the system to be editor-agnostic. The "LSP" will serve as the bridge between the IDE's chat window and the CVC backend.|

## 5. User Stories

### Story A: Prompt Recovery

**Scenario:** A developer asks the AI to refactor a class. The AI hallucinates a method that doesn't exist. The developer spends 20 minutes trying to fix the generated code before realizing the approach was fundamentally flawed.

- **Without CVC:** The developer does `git checkout .` to revert the code. They then have to re-type or re-paste the context into the AI chat to try again.
    
- **With CVC:** The developer runs `git cvc restore <node-id>`. The IDE chat window reverts to the state _exactly before_ the bad prompt was sent. The developer modifies the prompt slightly ("Don't use Method X") and branches the conversation.
    

### Story B: The "Archaeologist"

**Scenario:** A new developer joins the team and sees a complex regex function committed 6 months ago. `git blame` shows who committed it, but not _how_ they derived it.

- **With CVC:** The developer runs `git cvc blame utils.ts`. The system reveals the interaction history linked to that file, showing the specific conversation where the previous developer worked with the AI to construct the regex, including the edge cases they discussed.
    

### Story C: The Headless Agent (Agentic Flow)

**Scenario:** An autonomous agent picks up a GitHub Issue ("Fix Memory Leak"). The agent spends 30 minutes reading files, running profilers, and attempting fixes.

- **With CVC:** The entire agent loop is recorded as a **Conversation**.
    
    - _Node 1:_ Author: `IssueTracker`, Prompt: "Fix issue #42..."
        
    - _Node 2:_ Author: `System`, Prompt: `profiler_output.txt`
        
    - _Node 3:_ Author: `Compiler`, Prompt: `Error: borrow checker failed`
        
- **Value:** The human developer can review the _agent's debugging strategy_, not just the final code, ensuring the agent didn't just suppress the error without fixing the root cause.
    

### Story D: The Insightful Review (PR Workflow)

**Scenario:** A reviewer looks at a PR that refactors a critical authentication module. The code looks correct but complex. The reviewer wonders, "Did they consider the race condition in the token refresh?"

- **With CVC:** The reviewer opens the **CVC Reviewer Webapp** (Graphite-style UI). Alongside the code diff, they see the "Cognitive Timeline." They expand the "Thinking" node linked to the auth commit and see the AI explicitly discussing the race condition and the user verifying the fix with a specific test case.
    
- **Value:** Faster reviews, fewer "Why did you do this?" round-trips, and higher confidence in the merged code.
    

## 6. CLI Command Menu (Draft)

The interface is designed as a custom git command (`git-cvc`), allowing it to be invoked as `git cvc <command>`.

### Initialization & Configuration

- `git cvc init`: Initializes the SQLite database in `.git/cvc/`.
    
- `git cvc status`: Shows the current divergence between the Git HEAD and the Cognitive HEAD.
    

### The "Thought" Workflow

- `git cvc log`: Displays the conversation history (DAG) in a pager.
    
- `git cvc show <node-id>`: Displays the full details of a specific interaction (Prompt + CoT + Diff).
    
- `git cvc commit -m "Intent"`: Manually records a cognitive snapshot (though this should ideally be automated via IDE hooks).
    

### Navigation & Restoration

- `git cvc checkout <node-id>`: Reverts the "Cognitive Head" to a previous state.
    
- `git cvc branch <name>`: Creates a new "Thought Branch" (experimenting with a different prompt strategy).
    

### Integration

- `git cvc link <commit-sha> <node-id>`: Manually associates a specific git commit with a specific interaction node (used for retro-fitting history).
    

## 7. Data Schema (SQLite Draft)

```
-- The Container: Grouping interaction trees into logical sessions
CREATE TABLE conversations (
    id TEXT PRIMARY KEY, -- UUID
    title TEXT,          -- Auto-generated summary (e.g. "Refactoring Auth")
    created_at INTEGER
);

-- The primary nodes in the graph
CREATE TABLE interactions (
    id TEXT PRIMARY KEY, -- SHA-256 of content
    conversation_id TEXT, -- The logic container
    parent_id TEXT,      -- Pointer to previous thought (DAG structure)
    timestamp INTEGER,
    
    -- "Author" tracks the source of the stimulus
    author TEXT,         -- 'human', 'agent', 'system' (tools/compiler), 'external' (issues)
    
    -- "User Prompt" acts as the generic Input/Stimulus field
    user_prompt TEXT,    -- The chat message, ticket body, or tool output
    
    model_name TEXT,
    model_cot TEXT,      -- Chain of Thought (Hidden reasoning)
    model_response TEXT, -- Final visible response
    
    FOREIGN KEY(conversation_id) REFERENCES conversations(id),
    FOREIGN KEY(parent_id) REFERENCES interactions(id)
);

-- Efficient Context Storage (The "Git Link")
CREATE TABLE context_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    interaction_id TEXT,
    file_path TEXT,
    
    -- If file was clean:
    git_blob_sha TEXT,   -- Reference to Git's object store
    
    -- If file was dirty (uncommitted changes):
    dirty_patch TEXT,    -- The diff applied to the git_blob_sha
    
    -- Region specificity (optional)
    start_line INTEGER,
    end_line INTEGER,
    
    FOREIGN KEY(interaction_id) REFERENCES interactions(id)
);

-- Explicit Tool Usage Tracking (MCP, Function Calling)
-- Tracks the specific "actions" the model attempted during an interaction
CREATE TABLE tool_executions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    interaction_id TEXT,
    
    -- Protocol/Type: 'mcp', 'openai_function', 'native_exec'
    tool_protocol TEXT,
    
    -- Name: 'filesystem/read_file', 'git/status', 'postgres/query'
    tool_name TEXT,
    
    -- The raw arguments passed to the tool (JSON)
    arguments TEXT,
    
    -- Status: 'success', 'failure' (Output content is usually the Prompt for the *next* node)
    status TEXT,
    
    FOREIGN KEY(interaction_id) REFERENCES interactions(id)
);

-- Mapping thoughts to code
-- Supports Many-to-One: Multiple interaction nodes can link to a single Git commit.
CREATE TABLE artifact_links (
    interaction_id TEXT,
    git_commit_hash TEXT,
    link_type TEXT, -- e.g., 'generated', 'verified', 'refactored'
    PRIMARY KEY (interaction_id, git_commit_hash)
);
```

## 8. Software Architecture

We adhere to a strict separation between the "Brain" (Library) and the "Mouth" (LSP/CLI/MCP).

### 8.1 `cvc-core` (Rust Crate)

The foundational library containing no network or UI logic.

- **Modules:**
    
    - `db`: SQLite connection, schema migrations, query builders.
        
    - `git`: Bindings to `libgit2` for hashing, tree traversal, and diff generation.
        
    - `models`: Serde structs representing the Data Schema (Interactions, Tools, Context).
        
    - `graph`: Logic for traversing the Shadow DAG.
        

### 8.2 `cvc-lsp` (Rust Binary)

The integration server that runs alongside the IDE.

- **Role:** Wraps `cvc-core` and exposes it via JSON-RPC.
    
- **Concurrency:** Handles async event loops to ensure that recording a heavy "Thought" doesn't block the IDE's UI.
    

### 8.3 `cvc-cli` (Rust Binary)

The user-facing command line interface.

- **Role:** User-driven maintenance and querying (`init`, `status`, `restore`).
    
- **Dependency:** Imports `cvc-core` directly.
    

### 8.4 `cvc-mcp` (Rust Binary)

The Model Context Protocol Server for Agents.

- **Role:** Exposes CVC functionality as tools (`commit_thought`, `read_history`) for AI agents.
    
- **Dependency:** Imports `cvc-core` directly.
    
- **Architecture:** Runs as a standalone process spawned by the Client (Claude Desktop, Zed, Cursor), interacting directly with the SQLite database via `cvc-core` mechanisms.
    

## 9. Integration Protocols (Custom LSP Methods)

To avoid parsing fragile logs, CVC defines a set of **Custom LSP Notifications** (starting with `$/`) that the IDE or Agent MUST push. This shifts the burden of structure to the entity that actually knows what is happening.

### 9.1 Session Management

- **`$/cvc/session/start`**: Signal the beginning of a logical task (e.g., opening a chat window).
    
    - _Payload:_ `{ "title": "Refactor Auth", "timestamp": ... }`
        
- **`$/cvc/session/end`**: Explicit closure (optional).
    

### 9.2 The Interaction Loop

This is the heartbeat of the system.

1. **`$/cvc/turn/start`** (User Intent)
    
    - _Trigger:_ User hits "Send" in chat.
        
    - _Payload:_ `{ "prompt": "Fix this bug", "author": "human", "context_files": [...] }`
        
    - _Action:_ `cvc-core` snapshots the context (Git SHA or Dirty Diff).
        
2. **`$/cvc/tool/exec`** (Agent Action)
    
    - _Trigger:_ Model calls a tool (MCP or native).
        
    - _Payload:_ `{ "tool": "grep", "args": "...", "status": "success" }`
        
    - _Note:_ Sent _after_ execution to capture success/failure status.
        
3. **`$/cvc/turn/end`** (Model Response)
    
    - _Trigger:_ Model finishes streaming response.
        
    - _Payload:_ `{ "response": "I fixed it.", "chain_of_thought": "...", "model": "gpt-4" }`
        

### 9.3 Artifact Linking

- **`$/cvc/link/commit`**
    
    - _Trigger:_ User clicks "Commit" in the IDE.
        
    - _Payload:_ `{ "commit_sha": "a1b2...", "interaction_ids": ["..."] }`
        

## 10. Client-Side Implementation Strategy

To overcome the "Walled Garden" limitations of proprietary AI tools (like GitHub Copilot) and sandboxed IDEs, we employ distinct strategies depending on the environment.

### 10.1 The "Native Delegate" (Primary Strategy - VS Code)

- **Concept:** A lightweight VS Code Extension that registers a Chat Participant (e.g., `@cvc`).
    
- **Mechanism:**
    
    - **User Action:** The user invokes `@cvc` in the existing GitHub Copilot Chat sidebar.
        
    - **Interception:** The extension receives the prompt _before_ it is sent to the model.
        
    - **Logging:** It sends a `$/cvc/turn/start` notification to the `cvc-lsp`.
        
    - **Delegation:** It uses the `vscode.lm` API to invoke the available Language Model (e.g., GPT-4 via the user's existing Copilot subscription).
        
    - **Streaming:** The response is streamed back to the UI and simultaneously logged via `$/cvc/turn/end`.
        
- **Benefit:** Zero API key friction (reuses Copilot subscription), native UI feel, and guaranteed visibility of the conversation.
    

### 10.2 The "Trojan Proxy" (Secondary Strategy - Cursor/Zed)

- **Concept:** A local proxy server (`cvc-proxy`) acting as an OpenAI/Anthropic-compatible endpoint.
    
- **Mechanism:**
    
    - **Configuration:** User sets the Tool's Base URL to `http://localhost:9000/v1` (or equivalent).
        
    - **Interception:** The proxy captures the full request body (Prompt + Context) and response stream.
        
    - **Hashing:** It computes the interaction hash and writes to SQLite.
        
    - **Forwarding:** It transparently forwards traffic to the real provider.
        
- **Benefit:** Critical for AI-First Editors (Cursor, Windsurf) where the native AI features (Composer, Flow, Cmd+K) are proprietary and do not expose standard Extension APIs.
    

### 10.3 The "MCP Logger" (Agentic Memory - The Agent Standard)

- **Concept:** An MCP Server (`cvc-mcp`) exposing tools for cognitive logging.
    
- **Mechanism:**
    
    - **Direction:** "Reverse" Tool Use. The model uses the tool to push data out, rather than pull data in.
        
    - **Setup:** The Agent is configured with the `cvc-mcp` server.
        
    - **Prompting:** The System Prompt instructs the Agent: _"After every reasoning step, you MUST call the `commit_thought` tool."_
        
    - **Action:** The Agent calls `commit_thought(reasoning="...", context_summary="...")`.
        
- **Benefit:** Native integration for autonomous agents (Claude Code, Devin-like loops) that creates structured, high-fidelity logs of the internal reasoning process. Also serves as the primary integration point for modern CLIs (Claude CLI, Copilot CLI).
    

### 10.4 The "Process Shim" (Legacy/Closed Box Fallback)

- **Concept:** A wrapper command for tools that do not support API Configuration (e.g., hardcoded binaries).
    
- **Mechanism:**
    
    - **User Action:** `cvc wrap -- gemini "Fix this"`
        
    - **Interception:** CVC spawns the child process and captures `stdin` (User Prompt) and `stdout` (Model Response).
        
    - **Context Snapshot:** CVC snapshots the `git status` immediately before execution to approximate the context available to the tool.
        
- **Benefit:** Ensures coverage for "dumb" or proprietary tools that lack MCP or API config, at the cost of lower data fidelity.
    

### 10.5 Strategy Selection Matrix

|   |   |   |   |   |
|---|---|---|---|---|
|**Strategy**|**Ideal Use Case**|**Data Fidelity**|**Setup Friction**|**Notes**|
|**Native Delegate**|**VS Code / Copilot Sidebar**|High (Structured)|Medium (Plugin)|Uses `vscode.lm` API. Best for standard chat workflow.|
|**Trojan Proxy**|**Cursor (Composer), Windsurf**|Medium (Parsed)|Low (Config URL)|Essential because Cursor's native AI does not use the extension API.|
|**MCP Logger**|**Agents (Claude, Devin, CLI)**|**Very High** (Semantic)|Low (Standard)|The future standard. Relies on model obedience.|
|**Process Shim**|**Legacy / Closed Binaries**|Low (Raw I/O)|Low (Wrapper)|Fallback only.|

## 11. Workflow & Lifecycle Scenarios

To ensure data integrity without disrupting the developer's flow, CVC employs an "Immediate Persistence, Late Binding" model.

### 11.1 The "Immediate Persistence" Rule

- **Logic:** Interactions are committed to the SQLite database immediately upon the completion of a turn (`$/cvc/turn/end`).
    
- **Rationale:** We cannot rely on memory buffers. IDE crashes, window reloads, or power failures would result in the loss of the "Chain of Thought."
    
- **State:** At this stage, interactions are considered **"Floating"**. They exist, they are searchable, but they are not yet attached to a permanent Git artifact (Commit SHA).
    

### 11.2 The "Late Binding" of Artifacts

The connection between the Cognitive Graph and the Git Graph is solidified only when the code is actually committed.

- **Trigger:** A standard `git commit` event.
    
    - _Mechanism:_ A `post-commit` hook or an IDE event listener.
        
- **The Binding Logic:**
    
    1. CVC queries the `interactions` table for all **Floating Nodes** (unlinked).
        
    2. It filters for nodes created by the **current user** within the **current repository**.
        
    3. It creates entries in the `artifact_links` table, associating these interaction IDs with the new Git Commit SHA.
        
- **Edge Case (The "Abandoned Thought"):** If a user has a conversation but never commits code (e.g., "How do I center a div?"), those nodes remain Floating forever (or until garbage collection). This is a feature, not a bug; it preserves the research history even if no artifact resulted.
    

### 11.3 VS Code Context persistence

- **Invocation:** The user enters the CVC context by invoking the participant (e.g., `@cvc`).
    
- **Thread Stickiness:** In VS Code, subsequent messages in the same chat thread typically retain the context of the participant.
    
- **Session Boundary:** A "Conversation" (DB table) maps 1:1 with a VS Code Chat Session ID. If the user clears the chat or starts a new thread, a new Conversation ID is generated.
    

## 12. Collaboration & Remote Sync (The Shadow Ref)

To enable collaboration without requiring a dedicated database server, CVC utilizes the existing Git Remote as the transport layer for the cognitive graph, utilizing a "Serverless" architecture.

### 12.1 Philosophy: Git as Transport, SQLite as Cache

- **The Problem:** Binary SQLite files cannot be merged. Storing them in Git causes conflicts and repo bloat.
    
- **The Solution:** The local SQLite DB acts as a high-speed **Cache/Index**. The "Truth" is stored as **Immutable JSON Blobs** inside Git's Object Database, referenced by a custom, hidden ref (`refs/cvc/main`).
    

### 12.2 The Sync Mechanism

**1. The Push Flow (Hydration):**

- **Trigger:** User runs `git cvc push` (or hooked into `git push`).
    
- **Scan:** CVC identifies new, un-synced Interaction Nodes in the local SQLite DB.
    
- **Serialize:** Each node is serialized into a deterministic JSON format.
    
- **Write:** These JSONs are written as Git Blobs (Objects) into a custom Tree.
    
- **Update Ref:** The `refs/cvc/main` ref is updated to point to this new Tree.
    
- **Transport:** The custom ref is pushed to the remote (`git push origin refs/cvc/main`).
    

**2. The Pull Flow (Ingestion):**

- **Trigger:** User runs `git cvc pull`.
    
- **Fetch:** CVC fetches the remote `refs/cvc/main`.
    
- **Diff:** It detects new Blobs in the remote tree that are missing locally.
    
- **Ingest:** It reads the JSON blobs and inserts them into the local SQLite `interactions` table.
    
- **Result:** The local "Cache" is now consistent with the distributed "Truth."
    

### 12.3 Conflict Resolution

- **Architecture:** Because Interaction Nodes are **Content-Addressable** (ID = Hash) and **Immutable** (Append-Only), true merge conflicts are impossible.
    
- **Scenario:** Alice and Bob both push new thoughts.
    
- **Result:** The `refs/cvc/main` tree simply becomes the union of Alice's blobs and Bob's blobs. Git handles the deduplication of identical objects automatically.
    

## 13. Future Considerations & Open Questions

While the Core Architecture is defined, the following areas represent edge cases and complex features deferred for future refinement.

### 13.1 Security & Secret Sanitization

- **The Risk:** Since interactions are synced to the remote, accidental pasting of API keys in chat could lead to permanent leaks in the Git Object DB.
    
- **Potential Solution:** An active scrubber/filter running in the Proxy/LSP layer that masks patterns matching common keys (`sk-...`, `AWS...`) before writing to SQLite.
    
- **Mechanism:** Implementation of a `.thoughtignore` file for pattern matching.
    

### 13.2 Garbage Collection (The "Floating Node" Buildup)

- **The Issue:** "Abandoned Thoughts" (chats that never resulted in a commit) accumulate indefinitely in SQLite.
    
- **Potential Solution:** A configurable `gc.ttl` (Time To Live). If a node is unlinked and older than 30 days, `git cvc gc` prunes it.
    

### 13.3 Advanced Git Rewrites (Rebase/Squash)

- **The Complexity:** When a user squashes 5 commits into 1, the `artifact_links` table points to 5 non-existent SHAs and misses the new SHA.
    
- **The Strategy:** The "One Big Branch" storage model makes the _thoughts_ safe. The challenge is UI. We likely need a `post-rewrite` hook to heuristically re-link the old thought-chains to the new squashed SHA, creating a "many-to-one" history visualization.
    

### 13.4 Selective Synchronization (Private Thoughts)

- **The Need:** A user may want to commit the code publicly but keep the conversation private (e.g., "Explain this basic concept to me").
    
- **Potential Solution:** A `private` flag in the `interactions` table. The "Push" mechanism filters these out, ensuring they remain local-only.
    

### 13.5 Large Context Blobs

- **The Constraint:** If a user pastes a 50MB log file into the chat, storing it in SQLite/Git might degrade performance.
    
- **Potential Solution:** Threshold-based external storage (LFS-like behavior) or aggressive summarization before storage.
    

## 14. The Pull Request Workflow & Visualization

To integrate the Cognitive Graph into the code review process (PRs), we treat the CVC data as an overlay that augments standard Git platforms.

### 14.1 The "CVC Reviewer" (Web App / UI)

A visualization layer that sits alongside GitHub/GitLab. It can be a local web view (`cvc ui`) or a hosted service (OAuth app).

- **Authentication:** Authenticates with the Git Host (e.g., GitHub).
    
- **Data Fetching:**
    
    - Fetches the PR's `base_sha` and `head_sha` to determine the Commit Range.
        
    - Fetches the blobs from `refs/cvc/main` (The "Bag of Thoughts") via the GitHub API.
        
- **Logic:**
    
    - It filters the "Bag of Thoughts" for interactions linked to commits within the PR range.
        
    - It reconstructs the timeline: Code Change -> Prompt -> Reasoning -> Code Change.
        
- **Value:** Reviewers see the thought process without needing to clone the repo or run special CLI commands.
    

### 14.2 The "CVC Bot" (Automated Summarization)

A CI/CD integration that posts summaries to the PR conversation.

- **Trigger:** On `pull_request` creation.
    
- **Action:**
    
    - Extracts the linked conversations from the commits.
        
    - Uses an LLM to generate a high-level summary of _why_ changes were made (e.g., "The Agent explored 3 patterns for the auth middleware and settled on JWT due to statelessness requirements").
        
    - Posts this summary as a comment on the PR.
