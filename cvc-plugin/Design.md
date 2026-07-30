# Project Design Specification: Cognitive Version Control (CVC)

## 1. Executive Summary

**Cognitive Version Control (CVC)** augments Git with a private-by-default record of development intent. Git captures **what** changed and **when**; CVC may retain prompts, responses, tool metadata, and integration-exposed or explicitly supplied reasoning/context. CVC does not access or claim to record a model's hidden internal reasoning.

The system operates as a "Shadow DAG" (Directed Acyclic Graph), creating a parallel history of captured interaction data that is tightly coupled with, but distinct from, the Git commit graph.

## 2. Core Philosophy

- **The Artifact vs. The Process:** Code is the artifact; Intelligence is the process. We treat the _process_ as a first-class citizen in version control.
    
- **The Missing Layer:** Modern development is a dialogue between human intent and AI generation. Retaining the interaction data an integration exposes or a participant supplies can preserve useful decision context alongside the resulting code.
    
- **Native Integration:** The system must feel like a natural extension of Git, utilizing similar CLI patterns and storage locations.
    

## 3. Architecture: The "Double Helix" Model

The system maintains two intertwined graphs:

1. **The Git Graph:** The standard history of file snapshots (Blobs/Trees/Commits).
    
2. **The Cognitive Graph:** A history of interaction nodes (prompts, responses, and available reasoning/context fields).
    

### 3.1 The Unit of Change: The "Interaction Node"

Unlike Git, where the atomic unit is a snapshot of files, the atomic unit in CVC is an **Interaction Node**.

A Node consists of:

- **User Intent (The Stimulus):** The raw prompt, instruction, or signal that triggered the thought.
    
    - _Human:_ "Refactor this."
        
    - _System/Agent:_ Ticket description, Tool output, or Compiler error.
        
- **System State (The Context):** Context associated with the interaction and made available by the integration. It is not necessarily a complete inventory of everything the model received or used.
    
    - _Explicit Context:_ Files or code regions manually attached by the user.
        
    - _Dynamic Context:_ Files read or discovered by the agent during execution (e.g., via tool use).
        
- **The Derivation:** A model response and any reasoning content the integration exposes or a participant explicitly supplies. Hidden internal reasoning is not available to CVC.
    
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

- **With CVC:** Steps that the agent or integration explicitly supplies or exposes can be recorded as a **Conversation**.
    
    - _Node 1:_ Author: `IssueTracker`, Prompt: "Fix issue #42..."
        
    - _Node 2:_ Author: `System`, Prompt: `profiler_output.txt`
        
    - _Node 3:_ Author: `Compiler`, Prompt: `Error: borrow checker failed`
        
- **Value:** The human developer can review the available submitted or exposed debugging context, not just the final code. This record can support review, but it is not a complete account of the agent's internal process.
    

### Story D: The Insightful Review (PR Workflow)

**Scenario:** A reviewer looks at a PR that refactors a critical authentication module. The code looks correct but complex. The reviewer wonders, "Did they consider the race condition in the token refresh?"

- **With CVC:** The reviewer opens the **CVC Reviewer Webapp** (Graphite-style UI). Alongside the code diff, they see the "Cognitive Timeline." If a stored response or supplied reasoning field linked to the auth commit discusses the race condition and test case, the reviewer can inspect that available context.
    
- **Value:** Faster reviews, fewer "Why did you do this?" round-trips, and higher confidence in the merged code.
    

## 6. CLI Command Menu (Draft)

The interface is designed as a custom git command (`git-cvc`), allowing it to be invoked as `git cvc <command>`.

### Initialization & Configuration

- `git cvc init`: Initializes the SQLite database in `.git/cvc/`.
    
- `git cvc status`: Shows the current divergence between the Git HEAD and the Cognitive HEAD.
    

### The "Thought" Workflow

- `git cvc log`: Displays the conversation history (DAG) in a pager.
    
- `git cvc show <node-id>`: Displays the stored fields of a specific interaction (prompt + integration-exposed or explicitly supplied reasoning + diff, when present).
    
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
    id TEXT PRIMARY KEY, -- UUID interaction ID (not a Git content address)
    conversation_id TEXT, -- The logic container
    parent_id TEXT,      -- Pointer to previous thought (DAG structure)
    timestamp INTEGER,
    
    -- "Author" tracks the source of the stimulus
    author TEXT,         -- 'human', 'agent', 'system' (tools/compiler), 'external' (issues)
    
    -- "User Prompt" acts as the generic Input/Stimulus field
    user_prompt TEXT,    -- The chat message, ticket body, or tool output
    
    model_name TEXT,
    model_cot TEXT,      -- optional integration-exposed or explicitly supplied reasoning
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
    interaction_id TEXT, -- UUID referring to interactions.id
    git_commit_hash TEXT,
    link_type TEXT NOT NULL DEFAULT 'generated', -- automatic: 'generated' or low-confidence 'temporal'
    linked_by TEXT, -- configured repository signature email; linking-identity attribution
    PRIMARY KEY (interaction_id, git_commit_hash)
);
```

### 7.1 Current privacy and publication schema

The implemented schema adds `interactions.visibility` (`private` by default), `capture_source`, and `scrubber_version`. Conversation and interaction share records are keyed by a destination fingerprint; publication is independently tracked as `pending`, `published`, or `unknown` for that exact destination. This prevents a consent, share, or observed publication at one URL from authorizing another URL, including a changed push URL.

`tombstones` are local-only or destination-scoped for remote suppression. Their immutable wire form is `cvc.tombstone/v1` and contains an interaction UUID, timestamp, small reason code, and optional prior node object ID—never prompt or path text. Tombstones take precedence over every node, link, index, derivation event, and range-source representation during import and projection. Destination authority is not transitive: a share, publication observation, authorization, or tombstone from destination A grants nothing at destination B.

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
        
    - _Payload:_ `{ "response": "I fixed it.", "model": "gpt-4" }` plus optional reasoning only when the provider exposes it.
        

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
        
- **Benefit:** Zero additional API-key friction when the existing Copilot subscription is usable, a native UI feel, and visibility into the fields returned through this integration.
    

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
        
    - **Prompting:** The System Prompt asks the Agent to call `commit_thought` after a task step with a concise reasoning/context summary it can explicitly provide.
        
    - **Action:** The Agent calls `commit_thought(reasoning="...", context_summary="...")`.
        
- **Benefit:** Native integration for autonomous agents that records structured, integration-exposed or explicitly supplied reasoning/context. It does not obtain hidden internal reasoning. The MCP server also serves as an integration point for compatible CLIs.
    

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
|**MCP Logger**|**Agents (Claude, Devin, CLI)**|Structured (agent-supplied)|Low (Standard)|Records only fields the client or agent supplies.|
|**Process Shim**|**Legacy / Closed Binaries**|Low (Raw I/O)|Low (Wrapper)|Fallback only.|

## 11. Workflow & Lifecycle Scenarios

To ensure data integrity without disrupting the developer's flow, CVC employs an "Immediate Persistence, Late Binding" model.

### 11.1 The "Immediate Persistence" Rule

- **Logic:** Interactions are committed to the SQLite database immediately upon the completion of a turn (`$/cvc/turn/end`).
    
- **Rationale:** We cannot rely on memory buffers. IDE crashes, window reloads, or power failures would otherwise lose the locally available interaction record.
    
- **State:** At this stage, interactions are considered **"Floating"**. They exist, they are searchable, but they are not yet attached to a permanent Git artifact (Commit SHA).
    

### 11.2 The "Late Binding" of Artifacts

The connection between the Cognitive Graph and Git Graph is considered only after a standard `git commit`, normally through the `post-commit` hook. The hook is fail-safe: a linker error is reported as a warning and never fails the Git commit.

- **Policy and eligibility:** `LinkPolicy` considers floating nodes only when their timestamp is strictly after `max(first-parent commit time, now - link window)`. The first-parent time prevents a later commit from claiming earlier work; when there is no parent, the window bound is used. `cvc.linkWindow` accepts `0..=2592000` seconds (30 days), defaulting to `86400`; missing, malformed, negative, overflowing, or over-max values safely fall back to the default. `0` deliberately disables automatic linking. Nodes more than five minutes in the future are excluded; a first-parent timestamp beyond that skew fails closed and produces no automatic links.
- **Changed paths:** The linker compares the commit tree with its first parent (or an empty tree for a root commit), considering both old and new paths so renames and deletions can overlap. A node with explicit file context qualifies only when a normalized context path overlaps a changed path.
- **Binding:** One eligible overlapping node qualifies its conversation, and all eligible nodes in that conversation receive a `generated` link. An eligible node with no explicit context items can receive a lower-confidence `temporal` link instead. The complete automatic decision is persisted atomically, so a failure cannot half-bind a conversation. `linked_by` records the configured repository signature email as linking-identity attribution; it is not an author-based eligibility filter. Conflicting automatic link type or provenance fails rather than silently overwriting an existing record.
- **No weak heuristics:** Automatic linking never infers provenance from author, commit message, file-set similarity, or a `CVC-Session` trailer; that trailer is not implemented.
- **No forced association:** Stale nodes, nodes with explicit but disjoint context, and abandoned conversations remain floating. This conservatism is intentional: mislinked provenance is worse than no link.
    

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

The reader is compatible with sync formats **v1 through v5**. New outbound projections write format **v5**. V5 retains legacy-compatible node and link forms, preserves v4 tombstones, and adds immutable derivation evidence:

```text
nodes/<id[0..2]>/<interaction-id>.json   # immutable interaction node
by-commit/<commit-sha>/<interaction-id>  # zero-byte lookup index
links/<interaction-id>/<commit-sha>.json # immutable automatic-link event
tombstones/<id[0..2]>/<interaction-id>.json # immutable suppression record
events/<event-id[0..2]>/<event-id>.json     # immutable derivation event
ranges/<range-id[0..2]>/<range-id>.json     # immutable RangeEvidence
```

Interaction IDs are UUIDs; they are not Git content addresses. The JSON blobs are content-addressed Git objects separately. The `links/` event stream preserves node immutability: an interaction first pushed while floating can acquire an automatic link later. V5 derivation events are likewise immutable, canonical SHA-256-addressed explanations of a relation and its source events; range evidence is separately canonical SHA-256-addressed. Local trust observations and destination-specific source authorization are deliberately not portable claims in the Git tree. Tombstones are applied before nodes and suppress all legacy and sharded representations of their target **plus its v5 derivation/range closure** for that destination. They are suppression records, not proof of physical object deletion.

`RangeEvidence` is `cvc.range-evidence/v1`: SHA-1 object IDs, a repository identity, strict base/tip, ordered range members, base/result tree IDs, and a `cvc.changeset/v1` digest. The changeset hashes a bounded canonical, length-prefixed sequence of sorted raw-path deltas. Each delta contains its status and complete old/new endpoints (existence, object kind, mode, SHA-1 object ID); rename/copy detection is disabled, so configuration cannot change the delete/add representation. It covers binary, symlink, gitlink, and type changes, not patch text. This implementation is SHA-1-only because the current libgit2/object handling is SHA-1-oriented; unsupported object formats fail closed.

**1. The Push Flow (Hydration):**

- **Trigger:** A user explicitly shares a conversation for a destination and then runs `cvc push --manual --remote <name>`, or enables destination-specific auto-push after separate acknowledgement. A bare `cvc push` is auto-consent-gated.
    
- **Scan:** CVC identifies new, un-synced Interaction Nodes in the local SQLite DB.
    
- **Serialize:** Each new node is serialized into an immutable JSON blob; later automatic links and derivations are serialized as separate immutable events, with referenced range evidence where applicable.
    
- **Write:** These JSONs are written as Git Blobs (Objects) into a custom Tree.
    
- **Update Ref:** The `refs/cvc/main` ref is updated to point to this new Tree.
    
- **Transport:** The custom ref is pushed only after destination consent and the required manual TTY acknowledgement (unless that destination has acknowledged auto-push).

**3. Redaction and local cleanup:** `cvc redact` confirms an authorized interaction and creates a pending, destination-scoped tombstone. The required next command is `cvc push --manual --remote <name>`, which projects that tombstone. A later `redact` may build a `RedactionPlan` only after the fetched v5 baseline contains the tombstone; `--apply-local` changes only the local current CVC ref. `cvc delete-local` is local suppression and never propagates. Projection prunes the tombstoned node, indexes, derivation events, and range-source closure, while retaining the tombstone itself.

Tombstones are suppression, not physical erasure. SQLite uses `secure_delete` and attempts `wal_checkpoint(TRUNCATE)` followed by `VACUUM` after logical deletion, but those best-effort operations cannot guarantee removal from filesystem layers, SSD wear leveling, snapshots, backups, failed-operation WAL remnants, or immutable Git objects. Rotate exposed credentials first. Remote hard rewrite is unsupported pending an atomic force-with-lease design; no blind force-push procedure is supported.
    

**2. The Pull Flow (Ingestion):**

- **Trigger:** User runs `git cvc pull`.
    
- **Fetch:** CVC fetches the remote `refs/cvc/main`.
    
- **Diff:** It detects new node blobs, link/derivation events, range evidence, and tombstones in the remote tree that are missing locally.
    
- **Ingest:** It inserts new node blobs into the local SQLite cache and merges link/derivation events and ranges, including events for nodes already present locally. Remote evidence remains a remote assertion/observation; importing it does not establish local trust or destination authority.
    
- **Result:** The local "Cache" is now consistent with the distributed "Truth."
    

### 12.3 Conflict Resolution

- **Architecture:** Interaction IDs are UUIDs, while immutable node and link-event blobs are Git-content-addressed. Independent entries union cleanly in the append-only tree.
    
- **Scenario:** Alice and Bob both push new thoughts.
    
- **Result:** The `refs/cvc/main` tree can union independent blobs. If two automatic records name the same interaction and commit but disagree on link type or `linked_by`, ingestion fails rather than silently replacing provenance.
    

## 13. Future Considerations & Open Questions

While the Core Architecture is defined, the following areas represent edge cases and complex features deferred for future refinement.

### 13.1 Implemented privacy, secret detection, and retention caveats

- **The Risk:** Since interactions are synced to the remote, accidental pasting of API keys in chat could lead to permanent leaks in the Git Object DB.
    
- **Implemented boundary:** Aggregate capture is sanitized before any capture transaction. Built-in bounded detectors cover several high-confidence credential forms and credential-bearing JSON keys; a repository `.thoughtignore` can add path exclusions and literal/regex masks. Detection is defense in depth, not a guarantee: novel encodings, secrets not matching a detector, and data already copied elsewhere may remain. Never put secrets in `.thoughtignore` itself.
- **Retention:** `cvc delete-local` creates local suppression and never propagates. `cvc redact` creates a pending destination tombstone; project it next with `cvc push --manual --remote <name>`. A remote tombstone hides the target from future CVC projections for that destination, but does not erase already reachable Git objects. SQLite `secure_delete`, WAL checkpoint/truncation, and `VACUUM` are best-effort local cleanup only. Rotate exposed credentials first. Host support/removal requests are best effort; clones, forks, reflogs, caches, backups, filesystem layers, SSD wear leveling, and snapshots may preserve data.
- **Protected rewrite:** `RedactionPlan` is a 0600 local plan built against a freshly fetched v5 baseline. `--apply-local` switches only local `refs/cvc/main`; it does not guarantee deletion. Remote hard rewrite is **not implemented** until an atomic force-with-lease transport exists. No blind force command is a supported recovery procedure.
- **Reference:** See [Privacy.md](../Privacy.md) for the exact implemented `.thoughtignore` syntax, bounds, validation, and fail-closed behavior.
    

### 13.2 Garbage Collection (The "Floating Node" Buildup)

- **The Issue:** "Abandoned Thoughts" (chats that never resulted in a commit) accumulate indefinitely in SQLite.
    
- **Potential Solution:** A configurable `gc.ttl` (Time To Live). If a node is unlinked and older than 30 days, `git cvc gc` prunes it.
    

### 13.3 Exact Git rewrites and squashes

`post-rewrite` is installed as an advisory hook. It accepts only Git's strict, newline-terminated `amend` input (exactly one old/new full-OID pair) or `rebase` stream, verifies every new commit is reachable from HEAD, then durably queues the input before replay. It derives `rewrite_exact` only from trusted locally observed source events; malformed input is quarantined and retryable work remains in the inbox. Hook failure never blocks Git.

Squash recognition is intentionally exact, not heuristic. `cvc relink observe-range <BASE> <TIP>` records locally observed `RangeEvidence` only when `BASE` is a strict ancestor and the unique merge base of `TIP`; members are ordered and bounded. A one-parent candidate may receive `squash_exact` only if its parent→candidate `cvc.changeset/v1` equals the evidence's base→tip digest, exactly one locally trusted range matches, every referenced range object resolves, and source snapshots plus cursor/HEAD state pass transactional CAS checks. Missing evidence, ambiguous matching ranges, unavailable objects, or a deadline leave the target pending/floating—there is no guarantee of relinking.

Scans maintain a cursor per worktree and symbolic branch. Newly discovered candidates enter a bounded pending queue; attempts are ordered by least-recently attempted, then discovery order, so retryable/deadline work is not starved. The 5-second post-commit budget and longer pull/post-merge budgets are cooperative structural limits: checks occur between operations, but one libgit2 operation may exceed the nominal deadline.
    

### 13.4 Selective Synchronization (Private Thoughts)

- **The Need:** A user may want to commit the code publicly but keep the conversation private (e.g., "Explain this basic concept to me").
    
- **Implemented:** Interactions are private by default. Sharing records an exact current conversation snapshot for one remote; `--future` separately opts future turns into sharing for that remote. Consent is a separate, destination-fingerprinted acknowledgement, and auto-push is off until separately acknowledged.
    

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
        
    - It reconstructs a timeline from linked code changes and stored interaction fields, including exposed or explicitly supplied reasoning when present.
        
- **Value:** Reviewers see the available captured interaction context without needing to clone the repo or run special CLI commands. This is not a view into hidden model reasoning.
    

### 14.2 The "CVC Bot" (Automated Summarization)

A CI/CD integration that posts summaries to the PR conversation.

- **Trigger:** On `pull_request` creation.
    
- **Action:**
    
    - Extracts the linked conversations from the commits.
        
    - Uses an LLM to generate a high-level summary of _why_ changes were made (e.g., "The Agent explored 3 patterns for the auth middleware and settled on JWT due to statelessness requirements").
        
    - Posts this summary as a comment on the PR.
