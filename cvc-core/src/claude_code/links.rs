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
//! the text alone), it links the session family's still-floating responses up
//! to the commit — segmented so each commit claims only the reasoning since
//! the previous one. Links are `generated`: the honest existing relation,
//! recorded through the same trusted batch path the linker uses, with no
//! wire-format change. A distinct provenance grade is a deliberate follow-up.
//!
//! The family is the session's own conversation and the conversations of its
//! subagents. A subagent's entries carry the parent's session id and run in
//! the same worktree, and subagents rarely run `git commit` themselves: the
//! session that commits what a subagent wrote is usually the parent, so the
//! evidence of a commit made by any member covers all of them.
use crate::db::CvcStore;
use crate::linker::MAX_CLOCK_SKEW_SECS;
use crate::models::{CommitSha, InteractionId};
use chrono::{DateTime, Utc};
use git2::Repository;
use std::collections::BTreeSet;

const GENERATED_LINK_TYPE: &str = "generated";

/// One response that reported creating one or more commits.
pub struct CommitBearingResponse {
    pub interaction_id: InteractionId,
    pub timestamp: DateTime<Utc>,
    pub candidates: Vec<String>,
}

/// The conversations of one Claude Code session: its own, and one per subagent
/// transcript found beside it. An explicit set, never a prefix match, so no
/// other session's conversation, whatever its id looks like, can belong to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFamily {
    parent: String,
    subagents: BTreeSet<String>,
}

impl SessionFamily {
    pub fn new(parent: impl Into<String>) -> Self {
        Self {
            parent: parent.into(),
            subagents: BTreeSet::new(),
        }
    }

    /// Adds the conversation a subagent transcript is stored under.
    pub fn add_subagent(&mut self, conversation_id: impl Into<String>) {
        self.subagents.insert(conversation_id.into());
    }

    fn is_parent(&self, conversation_id: &str) -> bool {
        self.parent == conversation_id
    }

    fn contains(&self, conversation_id: &str) -> bool {
        self.is_parent(conversation_id) || self.subagents.contains(conversation_id)
    }
}

/// New links a family rescue made, split by whose responses they are.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FamilyLinks {
    /// Responses of the session's own conversation.
    pub parent: usize,
    /// Responses of its subagents' conversations.
    pub subagents: usize,
}

/// Links floating responses of the session family to the commits any of its
/// transcripts reported creating, returning the new links. Fail-closed: an
/// unresolvable or implausibly-timed candidate is skipped, and only genuinely
/// floating interactions of this worktree and family are touched, so an
/// already-linked response (by the linker or a previous ingest) is never
/// disturbed, and no response is linked twice.
pub fn link_transcript_commits(
    repo: &Repository,
    store: &CvcStore,
    family: &SessionFamily,
    worktree_fingerprint: &str,
    responses: &[CommitBearingResponse],
    linked_by: Option<&str>,
) -> Result<FamilyLinks, crate::db::DbError> {
    if responses.is_empty() {
        return Ok(FamilyLinks::default());
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
        return Ok(FamilyLinks::default());
    }
    // Earliest commit first, so each claims only the reasoning since the last.
    verified.sort_by_key(|(_, time)| *time);

    // Floating (unlinked) interactions of this worktree and family, oldest
    // first. Anything already linked is excluded here and left untouched.
    struct Floating {
        id: InteractionId,
        ts: i64,
        parent: bool,
    }
    let mut floating: Vec<Floating> = store
        .get_floating_interactions_for_worktree(worktree_fingerprint)?
        .into_iter()
        .filter(|interaction| family.contains(&interaction.conversation_id))
        .map(|interaction| Floating {
            parent: family.is_parent(&interaction.conversation_id),
            ts: interaction.timestamp.timestamp(),
            id: interaction.id,
        })
        .collect();
    floating.sort_by_key(|response| response.ts);

    let mut claimed = vec![false; floating.len()];
    let mut total = FamilyLinks::default();
    for (sha, commit_time) in &verified {
        // A commit can only have been produced by reasoning at or before it, so
        // responses after it belong to a later commit, not this one. No skew on
        // this bound: the committing response is written before its tool runs,
        // so it always precedes the commit time.
        let mut ids: Vec<&InteractionId> = Vec::new();
        let mut parent_ids = 0usize;
        for (index, response) in floating.iter().enumerate() {
            if !claimed[index] && response.ts <= *commit_time {
                claimed[index] = true;
                ids.push(&response.id);
                parent_ids += usize::from(response.parent);
            }
        }
        if ids.is_empty() {
            continue;
        }
        let links: Vec<(&InteractionId, &str)> =
            ids.iter().map(|id| (*id, GENERATED_LINK_TYPE)).collect();
        // One atomic batch per commit, however many conversations it spans.
        // Every id is floating, so each is inserted; the count is attributed
        // parent-first only so that a row another process linked in the
        // meantime can never make the two figures add up to more than was made.
        let inserted = store.link_automatic_interaction_batch_trusted(
            &links,
            &CommitSha::new(sha),
            linked_by,
        )?;
        let to_parent = inserted.min(parent_ids);
        total.parent += to_parent;
        total.subagents += inserted - to_parent;
    }
    Ok(total)
}
