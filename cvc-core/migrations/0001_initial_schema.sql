-- The Container: Grouping interaction trees into logical sessions
CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY, -- UUID
    title TEXT,          -- Auto-generated summary (e.g. "Refactoring Auth")
    created_at INTEGER
);

-- The primary nodes in the graph
CREATE TABLE IF NOT EXISTS interactions (
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
CREATE TABLE IF NOT EXISTS context_items (
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
CREATE TABLE IF NOT EXISTS tool_executions (
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
CREATE TABLE IF NOT EXISTS artifact_links (
    interaction_id TEXT,
    git_commit_hash TEXT,
    link_type TEXT, -- e.g., 'generated', 'verified', 'refactored'
    PRIMARY KEY (interaction_id, git_commit_hash)
);

-- Performance Indexes
CREATE INDEX IF NOT EXISTS idx_interactions_conversation_id ON interactions(conversation_id);
CREATE INDEX IF NOT EXISTS idx_artifact_links_commit_hash ON artifact_links(git_commit_hash);
