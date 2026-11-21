-- Schema of a CVC index.db as cvc 0.7.0 creates it on a new repository.
-- Generated from sqlite_master (schema only, no rows) plus the internal
-- migration markers that version records. Never edit by hand.
CREATE TABLE conversations (
    id TEXT PRIMARY KEY, -- UUID
    title TEXT,          -- Auto-generated summary (e.g. "Refactoring Auth")
    created_at INTEGER
);
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
    model_response TEXT, source_request_id TEXT, visibility TEXT NOT NULL DEFAULT 'private' CHECK(visibility IN ('private','shared')), capture_source TEXT NOT NULL DEFAULT 'legacy' CHECK(capture_source IN ('mcp','vscode_passive','vscode_explicit','cli_run','sync_import','legacy','claude_code')), scrubber_version INTEGER NOT NULL DEFAULT 0 CHECK(scrubber_version BETWEEN 0 AND 1), capture_worktree TEXT CHECK(capture_worktree IS NULL OR (length(capture_worktree)=64 AND capture_worktree NOT GLOB '*[^0-9a-f]*')), -- Final visible response
    
    FOREIGN KEY(conversation_id) REFERENCES conversations(id),
    FOREIGN KEY(parent_id) REFERENCES interactions(id)
);
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
CREATE TABLE artifact_links (
    interaction_id TEXT,
    git_commit_hash TEXT,
    link_type TEXT NOT NULL DEFAULT 'generated', -- 'generated' or 'temporal' for automatic links
    linked_by TEXT, -- Git commit author email responsible for automatic binding
    PRIMARY KEY (interaction_id, git_commit_hash)
);
CREATE INDEX idx_interactions_conversation_id ON interactions(conversation_id);
CREATE INDEX idx_artifact_links_commit_hash ON artifact_links(git_commit_hash);
CREATE INDEX idx_interactions_source_request_id ON interactions(source_request_id);
CREATE INDEX idx_interactions_visibility ON interactions(visibility);
CREATE INDEX idx_interactions_source_request ON interactions(source_request_id);
CREATE TABLE conversation_share_policy (conversation_id TEXT PRIMARY KEY, future_shared INTEGER NOT NULL CHECK(future_shared IN (0,1)));
CREATE TABLE conversation_shares (conversation_id TEXT NOT NULL, remote_fingerprint TEXT NOT NULL, share_future INTEGER NOT NULL CHECK(share_future IN (0,1)), PRIMARY KEY(conversation_id,remote_fingerprint));
CREATE TABLE interaction_shares (interaction_id TEXT NOT NULL, remote_fingerprint TEXT NOT NULL, PRIMARY KEY(interaction_id, remote_fingerprint));
CREATE INDEX idx_interaction_shares_remote ON interaction_shares(remote_fingerprint);
CREATE TABLE publications (interaction_id TEXT NOT NULL, remote_fingerprint TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','published','unknown')), updated_at INTEGER NOT NULL, PRIMARY KEY(interaction_id, remote_fingerprint));
CREATE INDEX idx_publications_remote_state ON publications(remote_fingerprint,state);
CREATE TABLE harness_ingest_cursors (harness TEXT NOT NULL, session_id TEXT NOT NULL, transcript_path TEXT NOT NULL, byte_offset INTEGER NOT NULL CHECK(byte_offset >= 0), updated_at INTEGER NOT NULL, PRIMARY KEY(harness, session_id));
CREATE TABLE tombstones (interaction_id TEXT NOT NULL, scope_kind TEXT NOT NULL CHECK(scope_kind IN ('local','remote')), remote_fingerprint TEXT, format TEXT NOT NULL, version INTEGER NOT NULL, deleted_at INTEGER NOT NULL, reason_code TEXT NOT NULL CHECK(reason_code IN ('user_requested','security','retention')), previous_node_oid TEXT, source TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'published' CHECK(state IN ('pending','published')), CHECK(((scope_kind='local' AND remote_fingerprint IS NULL) OR (scope_kind='remote' AND remote_fingerprint IS NOT NULL)) AND format='cvc.tombstone/v1' AND version=1), PRIMARY KEY(interaction_id,scope_kind,remote_fingerprint));
CREATE INDEX idx_tombstones_remote ON tombstones(remote_fingerprint);
CREATE UNIQUE INDEX idx_tombstones_scope_identity ON tombstones(interaction_id,scope_kind,COALESCE(remote_fingerprint,''));
CREATE TABLE derivation_events (event_id TEXT PRIMARY KEY, interaction_id TEXT NOT NULL, target_commit TEXT NOT NULL, relation TEXT NOT NULL CHECK(relation IN ('generated','temporal','verified','rewrite_exact','squash_exact')), evidence_version INTEGER NOT NULL, evidence_kind TEXT NOT NULL CHECK(evidence_kind IN ('locally_observed','imported_legacy','remote_assertion')), origin TEXT NOT NULL CHECK(origin IN ('local_hook','local_linker','remote_assertion','legacy_import')), source_event_ids TEXT NOT NULL, old_oid TEXT, new_oid TEXT, range_id TEXT, linked_by TEXT, payload TEXT NOT NULL);
CREATE INDEX idx_derivation_endpoint ON derivation_events(interaction_id,target_commit);
CREATE TABLE derivation_authorizations (event_id TEXT NOT NULL, remote_fingerprint TEXT NOT NULL, source_event_id TEXT NOT NULL, PRIMARY KEY(event_id,remote_fingerprint,source_event_id));
CREATE TABLE rewrite_batches (batch_id TEXT PRIMARY KEY, mode TEXT NOT NULL, payload_hash TEXT NOT NULL);
CREATE TABLE derivation_observations (event_id TEXT NOT NULL, source_fingerprint TEXT, source_key TEXT NOT NULL, origin TEXT NOT NULL CHECK(origin IN ('local_hook','local_api','remote_sync')), trusted_local INTEGER NOT NULL CHECK(trusted_local IN (0,1)) CHECK(trusted_local=0 OR origin IN ('local_hook','local_api')), PRIMARY KEY(event_id,source_key));
CREATE INDEX idx_derivation_observations_trust ON derivation_observations(event_id,trusted_local);
CREATE TABLE range_evidence (range_id TEXT PRIMARY KEY, payload TEXT NOT NULL, payload_digest TEXT NOT NULL, changeset_algorithm TEXT NOT NULL, changeset_digest TEXT NOT NULL);
CREATE INDEX idx_range_changeset ON range_evidence(changeset_algorithm,changeset_digest);
CREATE TABLE range_observations (range_id TEXT NOT NULL, source_key TEXT NOT NULL, origin TEXT NOT NULL CHECK(origin IN ('explicit','pre_push','remote_sync')), trusted_local INTEGER NOT NULL CHECK(trusted_local IN (0,1)) CHECK(trusted_local=0 OR origin IN ('explicit','pre_push')), source_ref TEXT, source_remote TEXT, PRIMARY KEY(range_id,source_key));
CREATE INDEX idx_range_trust ON range_observations(range_id,trusted_local);
CREATE TABLE range_authorizations (range_id TEXT NOT NULL,remote_fingerprint TEXT NOT NULL,PRIMARY KEY(range_id,remote_fingerprint));
CREATE TABLE range_source_snapshots (range_id TEXT NOT NULL,source_id TEXT NOT NULL,snapshot TEXT NOT NULL,PRIMARY KEY(range_id,source_id));
CREATE TABLE branch_scan_cursors (worktree_key TEXT NOT NULL,symbolic_ref TEXT NOT NULL,last_tip TEXT NOT NULL,PRIMARY KEY(worktree_key,symbolic_ref));
CREATE TABLE range_interaction_sources (range_id TEXT NOT NULL,interaction_id TEXT NOT NULL,source_id TEXT NOT NULL,snapshot TEXT NOT NULL,PRIMARY KEY(range_id,interaction_id,source_id));
CREATE INDEX idx_range_interaction_sources ON range_interaction_sources(range_id,interaction_id);
CREATE TABLE pending_squash_targets (worktree_key TEXT NOT NULL,symbolic_ref TEXT NOT NULL,target_commit TEXT NOT NULL,parent_commit TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('pending','matched','unsupported')),discovered_order INTEGER NOT NULL,last_attempt_seq INTEGER NOT NULL DEFAULT 0,attempt_count INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(worktree_key,symbolic_ref,target_commit));
CREATE INDEX idx_pending_squash ON pending_squash_targets(worktree_key,symbolic_ref,status,last_attempt_seq,discovered_order);
CREATE TABLE cvc_internal_migrations(name TEXT PRIMARY KEY);
INSERT INTO cvc_internal_migrations(name) VALUES('capture-source-claude-code/v1');
INSERT INTO cvc_internal_migrations(name) VALUES('legacy-link-observations/v1');
