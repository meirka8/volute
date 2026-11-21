//! The repository-wide record that the Claude Code harness is wanted:
//! `<common git dir>/cvc/harness/claude-code.path`, which Git's
//! `post-checkout` hook reads to install the hook entries into a new linked
//! worktree. See [`crate::harness::record`], which keeps the record of every
//! harness the same way, for the format and what its checks protect.
use crate::harness::{record, HarnessId};
use crate::repository::RepositoryLayout;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Where the record lives for this repository (it may not exist).
pub fn record_path(layout: &RepositoryLayout) -> PathBuf {
    record::record_path(layout, HarnessId::ClaudeCode)
}

/// Writes the record atomically (see [`record::write_record`]).
pub fn write_record(layout: &RepositoryLayout, binary: &Path) -> Result<()> {
    record::write_record(layout, HarnessId::ClaudeCode, binary)
}

/// Reads the recorded binary path (see [`record::read_record`]).
pub fn read_record(layout: &RepositoryLayout) -> Result<Option<PathBuf>> {
    record::read_record(layout, HarnessId::ClaudeCode)
}

/// Removes the record (see [`record::clear_record`]).
pub fn clear_record(layout: &RepositoryLayout) -> Result<bool> {
    record::clear_record(layout, HarnessId::ClaudeCode)
}
