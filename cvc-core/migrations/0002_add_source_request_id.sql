-- Add source_request_id to track which VS Code chat request generated this interaction.
-- Used for deduplication: when a request is re-processed, old interactions are deleted
-- and replaced with newly segmented ones.
ALTER TABLE interactions ADD COLUMN source_request_id TEXT;
CREATE INDEX IF NOT EXISTS idx_interactions_source_request_id ON interactions(source_request_id);
