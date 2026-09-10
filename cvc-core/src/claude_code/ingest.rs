//! Idempotent application of a transcript plan to the local store.
//!
//! Every run reads the transcript from the session's stored cursor, plans the
//! closed responses in that window, skips the ones already recorded (by their
//! deterministic ids), and commits the new captures together with the advanced
//! cursor in one transaction. A run that fails changes nothing.
use super::transcript::{self, ParsedLine, PlanInput, TranscriptError, HARNESS};
use crate::db::{CvcStore, DbError, HarnessCursorUpdate};
use crate::models::{Conversation, Interaction, InteractionId};
use crate::privacy::{self, ClaudeCodeCapture, PreparedPolicy};
use crate::repository::{RepositoryLayout, RepositoryLayoutError};
use chrono::Utc;
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// One read never buffers more than this; a transcript beyond it is treated as
/// corruption rather than an ingest target.
const MAX_READ_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DERIVED_TITLE_BYTES: usize = 80;
const DEFAULT_TITLE: &str = "Claude Code session";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestMode {
    /// Hook-driven mid-session read: only closed responses are recorded and
    /// a streaming or result-awaiting response stays pending.
    Incremental,
    /// Stop/SessionEnd or operator-driven read: every response is complete.
    Final,
}

#[derive(Debug, Error)]
pub enum IngestError {
    #[error(transparent)]
    Transcript(#[from] TranscriptError),
    #[error("transcript {0:?} is not a regular file")]
    NotRegularFile(PathBuf),
    #[error("transcript exceeds the supported size")]
    TooLarge,
    #[error("session id {0:?} is not a safe identifier")]
    InvalidSession(String),
    #[error("transcript file name does not name a session; pass the session id explicitly")]
    UnknownSession,
    #[error(transparent)]
    Layout(#[from] RepositoryLayoutError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("transcript I/O: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    pub session_id: String,
    pub inserted: usize,
    pub already_present: usize,
    pub pending_responses: usize,
    pub cursor: u64,
    pub transcript_len: u64,
}

/// Ingests `transcript_path` for the worktree described by `layout`.
///
/// `session_id` defaults to the transcript's file stem, which is how Claude
/// Code names session files; every entry must agree with it. The policy is the
/// exact `.thoughtignore` snapshot handed to persistence.
pub fn ingest(
    layout: &RepositoryLayout,
    store: &CvcStore,
    policy: &PreparedPolicy,
    transcript_path: &Path,
    session_id: Option<&str>,
    mode: IngestMode,
) -> Result<IngestReport, IngestError> {
    let session = session_id
        .map(str::to_owned)
        .or_else(|| {
            transcript_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned)
        })
        .ok_or(IngestError::UnknownSession)?;
    if !privacy::is_safe_identifier(&session) {
        return Err(IngestError::InvalidSession(session));
    }

    let metadata = fs::symlink_metadata(transcript_path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(IngestError::NotRegularFile(transcript_path.to_path_buf()));
    }
    let worktree_root = layout.worktree_root()?.to_path_buf();
    let capture_worktree = layout.worktree_origin()?;

    let mut file = fs::File::open(transcript_path)?;
    let mut offset = store
        .harness_ingest_cursor(HARNESS, &session)?
        .map(|cursor| cursor.byte_offset)
        .unwrap_or(0);
    if !cursor_is_consistent(&mut file, offset, metadata.len())? {
        // A rewritten or truncated transcript invalidates the cursor; the
        // deterministic ids make a full re-read harmless.
        offset = 0;
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_READ_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_READ_BYTES {
        return Err(IngestError::TooLarge);
    }
    let transcript_len = offset + bytes.len() as u64;

    let window = transcript::parse_window(&bytes, offset, &session)?;
    let path_aliases = path_aliases(&window.lines, &worktree_root);
    let plan = transcript::plan(
        &window.lines,
        offset,
        &PlanInput {
            session_id: &session,
            worktree_root: &worktree_root,
            path_aliases: &path_aliases,
            final_mode: mode == IngestMode::Final,
        },
    );

    let existing = store.get_conversation(&session)?;
    let title = plan
        .title
        .clone()
        .or_else(|| {
            existing
                .as_ref()
                .map(|conversation| conversation.title.clone())
                .filter(|title| !title.trim().is_empty())
        })
        .or_else(|| plan.first_human_prompt.as_deref().and_then(derive_title))
        .unwrap_or_else(|| DEFAULT_TITLE.to_owned());
    let created_at = existing
        .as_ref()
        .map(|conversation| conversation.created_at)
        .or_else(|| plan.interactions.first().map(|planned| planned.timestamp))
        .unwrap_or_else(Utc::now);
    let conversation = Conversation {
        id: session.clone(),
        title,
        created_at,
    };

    let mut captures = Vec::new();
    let mut batch_ids: HashSet<InteractionId> = HashSet::new();
    let mut already_present = 0usize;
    for planned in plan.interactions {
        if store.interaction_exists(&planned.id)? {
            already_present += 1;
            continue;
        }
        // A parent outside this batch and outside the store (suppressed, or
        // never recorded) breaks the chain rather than failing the batch.
        let parent_id = match planned.parent_id {
            Some(parent) if batch_ids.contains(&parent) || store.interaction_exists(&parent)? => {
                Some(parent)
            }
            _ => None,
        };
        let interaction = Interaction {
            id: planned.id.clone(),
            conversation_id: session.clone(),
            parent_id,
            timestamp: planned.timestamp,
            author: planned.author,
            user_prompt: planned.user_prompt,
            model_name: planned.model_name,
            model_cot: planned.model_cot,
            model_response: planned.model_response,
            source_request_id: Some(planned.response_key),
        };
        batch_ids.insert(planned.id);
        captures.push(ClaudeCodeCapture::new(
            conversation.clone(),
            interaction,
            planned.context_items,
            planned.tool_executions,
            policy.clone(),
            capture_worktree.clone(),
        ));
    }

    let outcome = store.capture_claude_code_batch(
        captures,
        HarnessCursorUpdate {
            harness: HARNESS,
            session_id: &session,
            transcript_path: &transcript_path.to_string_lossy(),
            byte_offset: plan.cursor_offset,
        },
    )?;
    Ok(IngestReport {
        session_id: session,
        inserted: outcome.inserted,
        already_present: already_present + outcome.suppressed,
        pending_responses: plan.pending_responses,
        cursor: plan.cursor_offset,
        transcript_len,
    })
}

/// The stored cursor is trusted only when it still lands on a line boundary
/// of the current file.
fn cursor_is_consistent(file: &mut fs::File, offset: u64, len: u64) -> std::io::Result<bool> {
    if offset == 0 {
        return Ok(true);
    }
    if offset > len {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(offset - 1))?;
    let mut previous = [0u8; 1];
    file.read_exact(&mut previous)?;
    Ok(previous[0] == b'\n')
}

/// Transcript `cwd` spellings that resolve inside the worktree, so tool paths
/// written through a symlinked or non-canonical checkout path still attribute.
fn path_aliases(lines: &[ParsedLine], worktree_root: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut seen = BTreeSet::new();
    let mut aliases = Vec::new();
    for line in lines {
        let ParsedLine::Entry(entry) = line else {
            continue;
        };
        let Some(cwd) = entry.cwd.as_deref() else {
            continue;
        };
        if !seen.insert(cwd.to_owned()) {
            continue;
        }
        let spelled = PathBuf::from(cwd);
        if spelled == worktree_root {
            continue;
        }
        if let Ok(canonical) = fs::canonicalize(&spelled) {
            if canonical.starts_with(worktree_root) {
                aliases.push((spelled, canonical));
            }
        }
    }
    aliases
}

fn derive_title(prompt: &str) -> Option<String> {
    let line = prompt.lines().find(|line| !line.trim().is_empty())?.trim();
    if line.len() <= MAX_DERIVED_TITLE_BYTES {
        return Some(line.to_owned());
    }
    let mut end = MAX_DERIVED_TITLE_BYTES;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!("{}…", &line[..end]))
}
