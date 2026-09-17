//! Transcript-derived commit links.
//!
//! The response that runs `git commit` is written to the transcript before the
//! tool runs, so it reaches the database only after the commit's own
//! `post-commit` hook has already linked what it could. The linker's
//! first-parent lower bound then permanently excludes that response, and the
//! reasoning leading up to it, from automatic linking by timing. But the
//! transcript records the commit the session itself created, which is exact,
//! locally observed evidence — stronger than the timing heuristic the linker
//! is forbidden to use across sessions.
//!
//! This pass rescues exactly those responses. For each commit a session's own
//! Git tool call created (verified against the repository, never trusted from
//! the text alone), it links that session's still-floating responses up to the
//! commit — segmented so each commit claims only the reasoning since the
//! previous one. Links are `generated`: the honest existing relation, recorded
//! through the same trusted batch path the linker uses, with no wire-format
//! change. A distinct provenance grade is a deliberate follow-up.
use crate::db::CvcStore;
use crate::linker::MAX_CLOCK_SKEW_SECS;
use crate::models::{CommitSha, InteractionId};
use chrono::{DateTime, Utc};
use git2::Repository;

const GENERATED_LINK_TYPE: &str = "generated";

/// One response that reported creating one or more commits.
pub struct CommitBearingResponse {
    pub interaction_id: InteractionId,
    pub timestamp: DateTime<Utc>,
    pub candidates: Vec<String>,
}

/// Links floating responses of `session_id` to the commits the session created,
/// returning the number of new links. Fail-closed: an unresolvable or
/// implausibly-timed candidate is skipped, and only genuinely floating
/// interactions of this worktree and session are touched, so an already-linked
/// response (by the linker or a previous ingest) is never disturbed.
pub fn link_transcript_commits(
    repo: &Repository,
    store: &CvcStore,
    session_id: &str,
    worktree_fingerprint: &str,
    responses: &[CommitBearingResponse],
    linked_by: Option<&str>,
) -> Result<usize, crate::db::DbError> {
    if responses.is_empty() {
        return Ok(0);
    }
    let now = Utc::now().timestamp();
    let skew = MAX_CLOCK_SKEW_SECS;

    // Resolve each candidate to a real commit and keep only those whose time is
    // plausible for the response that reported them: a commit cannot precede
    // the reasoning that made it (beyond clock skew), nor sit in the future.
    let mut verified: Vec<(String, i64)> = Vec::new();
    for response in responses {
        let response_ts = response.timestamp.timestamp();
        for candidate in &response.candidates {
            let Ok(object) = repo.revparse_single(candidate) else {
                continue;
            };
            let Some(commit) = object.as_commit() else {
                continue;
            };
            let commit_time = commit.time().seconds();
            if commit_time > now + skew || commit_time < response_ts - skew {
                continue;
            }
            let full = commit.id().to_string();
            if !verified.iter().any(|(sha, _)| sha == &full) {
                verified.push((full, commit_time));
            }
        }
    }
    if verified.is_empty() {
        return Ok(0);
    }
    // Earliest commit first, so each claims only the reasoning since the last.
    verified.sort_by_key(|(_, time)| *time);

    // Floating (unlinked) interactions of this worktree and session, oldest
    // first. Anything already linked is excluded here and left untouched.
    let mut floating: Vec<(InteractionId, i64)> = store
        .get_floating_interactions_for_worktree(worktree_fingerprint)?
        .into_iter()
        .filter(|interaction| interaction.conversation_id == session_id)
        .map(|interaction| (interaction.id, interaction.timestamp.timestamp()))
        .collect();
    floating.sort_by_key(|(_, ts)| *ts);

    let mut claimed = vec![false; floating.len()];
    let mut total = 0usize;
    for (sha, commit_time) in &verified {
        // A commit can only have been produced by reasoning at or before it, so
        // responses after it belong to a later commit, not this one. No skew on
        // this bound: the committing response is written before its tool runs,
        // so it always precedes the commit time.
        let mut ids: Vec<InteractionId> = Vec::new();
        for (index, (id, ts)) in floating.iter().enumerate() {
            if !claimed[index] && *ts <= *commit_time {
                claimed[index] = true;
                ids.push(id.clone());
            }
        }
        if ids.is_empty() {
            continue;
        }
        let links: Vec<(&InteractionId, &str)> =
            ids.iter().map(|id| (id, GENERATED_LINK_TYPE)).collect();
        total += store.link_automatic_interaction_batch_trusted(
            &links,
            &CommitSha::new(sha),
            linked_by,
        )?;
    }
    Ok(total)
}
