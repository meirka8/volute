//! A session family: the conversations whose responses one commit can claim.
//!
//! A subagent works in its parent's session and worktree, and the session
//! that commits what a subagent wrote is usually the parent. So the evidence
//! of a commit made by any member of a family covers all of them.
//!
//! A family is an explicit set rooted at one conversation, never a prefix
//! match, so no other session's conversation, whatever its id looks like, can
//! belong to it:
//!
//! * Claude Code finds its members as files beside the session's transcript
//!   (`<session>/subagents/agent-<id>.jsonl`), as 0.7.x does, and records no
//!   relation for them: the relation is already in the member's id,
//!   `<session>:agent-<id>`.
//! * Every other harness takes its members from `conversation_relations`:
//!   the root and its recorded subagents, recursively. A fork is never
//!   family; it does not share the commits of its source.
//!
//! # The commit rescue
//!
//! The response that runs `git commit` is written to the agent's record before
//! the tool runs, so it reaches the database only after the commit's own
//! `post-commit` hook has already linked what it could. The linker's
//! first-parent lower bound then permanently excludes that response, and the
//! reasoning leading up to it, from automatic linking by timing. But the
//! record names the commit the session itself created, which is exact,
//! locally observed evidence, stronger than the timing heuristic the linker is
//! forbidden to use across sessions.
//!
//! [`link_family_commits`] rescues exactly those responses. For each commit a
//! session's own Git tool call created (verified against the repository, never
//! trusted from the text alone), it links the family's still-floating
//! responses up to the commit, segmented so each commit claims only the
//! reasoning since the previous one. Links are `generated`: the honest
//! existing relation, recorded through the same trusted batch path the linker
//! uses, with no wire-format change.
//!
//! Only floating responses of the family's conversations that were captured
//! in this worktree are claimed, and, outside Claude Code, only those of the
//! harness's own capture source ([`Family::claimed_source`]): another
//! source's rows in a member conversation (an MCP thought, say) are never
//! claimed.
use crate::db::{CvcStore, DbError, RelationKind, MAX_RELATION_DEPTH};
use crate::harness::HarnessId;
use crate::linker::MAX_CLOCK_SKEW_SECS;
use crate::models::{CommitSha, InteractionId};
use chrono::{DateTime, Utc};
use git2::Repository;
use std::collections::BTreeSet;

const GENERATED_LINK_TYPE: &str = "generated";

/// The conversations of one family. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Family {
    harness: HarnessId,
    root: String,
    members: BTreeSet<String>,
}

impl Family {
    /// A family of `harness` with only its root, `root`.
    pub fn new(harness: HarnessId, root: impl Into<String>) -> Self {
        Self {
            harness,
            root: root.into(),
            members: BTreeSet::new(),
        }
    }

    /// `root` and its recorded subagents of `harness`, recursively, at most
    /// [`MAX_RELATION_DEPTH`] levels below it. Forks and other harnesses'
    /// relations are not followed.
    pub fn from_relations(
        store: &CvcStore,
        harness: HarnessId,
        root: &str,
    ) -> Result<Self, DbError> {
        let mut family = Self::new(harness, root);
        let mut level = vec![root.to_owned()];
        for _ in 0..MAX_RELATION_DEPTH {
            let mut next = Vec::new();
            for parent in &level {
                for relation in store.conversation_children(parent)? {
                    if relation.kind != RelationKind::Subagent || relation.harness != harness {
                        continue;
                    }
                    if relation.child_id != family.root
                        && family.members.insert(relation.child_id.clone())
                    {
                        next.push(relation.child_id);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            level = next;
        }
        Ok(family)
    }

    /// Adds the conversation of a member found for this run.
    pub fn add_member(&mut self, conversation_id: impl Into<String>) {
        let id = conversation_id.into();
        if id != self.root {
            self.members.insert(id);
        }
    }

    pub fn harness(&self) -> HarnessId {
        self.harness
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    /// The members other than the root, by id.
    pub fn members(&self) -> impl Iterator<Item = &str> {
        self.members.iter().map(String::as_str)
    }

    pub fn is_root(&self, conversation_id: &str) -> bool {
        self.root == conversation_id
    }

    pub fn contains(&self, conversation_id: &str) -> bool {
        self.is_root(conversation_id) || self.members.contains(conversation_id)
    }

    /// The capture source whose interactions alone the family's rescue may
    /// claim: the harness's own. `None` for Claude Code, whose rescue keeps
    /// 0.7.x's rule (any floating interaction of the family's conversations
    /// in the worktree).
    pub fn claimed_source(&self) -> Option<&'static str> {
        match self.harness {
            HarnessId::ClaudeCode => None,
            other => Some(other.capture_source_str()),
        }
    }
}

/// One response that reported creating one or more commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitBearingResponse {
    pub interaction_id: InteractionId,
    pub timestamp: DateTime<Utc>,
    /// Raw tokens from the session's own Git output, short or full hex.
    pub candidates: Vec<String>,
}

/// New links a family rescue made, split by whose responses they are.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FamilyLinks {
    /// Responses of the root's conversation.
    pub root: usize,
    /// Responses of the other members' conversations.
    pub members: usize,
}

/// Links floating responses of `family` to the commits any of its records
/// reported creating, and returns the new links. Fail-closed: an
/// unresolvable or implausibly timed candidate is skipped, and only
/// genuinely floating interactions of this worktree, this family and the
/// family's claimed source are touched, so an already-linked response (by
/// the linker or a previous ingest) is never disturbed, and no response is
/// linked twice. See the module docs.
pub fn link_family_commits(
    repo: &Repository,
    store: &CvcStore,
    family: &Family,
    worktree_fingerprint: &str,
    responses: &[CommitBearingResponse],
    linked_by: Option<&str>,
) -> Result<FamilyLinks, DbError> {
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

    // Floating (unlinked) interactions of this worktree, family and source,
    // oldest first. Anything already linked is excluded here and left
    // untouched.
    struct Floating {
        id: InteractionId,
        ts: i64,
        root: bool,
    }
    let mut floating: Vec<Floating> = store
        .floating_interactions_of_source(worktree_fingerprint, family.claimed_source())?
        .into_iter()
        .filter(|interaction| family.contains(&interaction.conversation_id))
        .map(|interaction| Floating {
            root: family.is_root(&interaction.conversation_id),
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
        let mut root_ids = 0usize;
        for (index, response) in floating.iter().enumerate() {
            if !claimed[index] && response.ts <= *commit_time {
                claimed[index] = true;
                ids.push(&response.id);
                root_ids += usize::from(response.root);
            }
        }
        if ids.is_empty() {
            continue;
        }
        let links: Vec<(&InteractionId, &str)> =
            ids.iter().map(|id| (*id, GENERATED_LINK_TYPE)).collect();
        // One atomic batch per commit, however many conversations it spans.
        // Every id is floating, so each is inserted; the count is attributed
        // root-first only so that a row another process linked in the
        // meantime can never make the two figures add up to more than was made.
        let inserted = store.link_automatic_interaction_batch_trusted(
            &links,
            &CommitSha::new(sha),
            linked_by,
        )?;
        let to_root = inserted.min(root_ids);
        total.root += to_root;
        total.members += inserted - to_root;
    }
    Ok(total)
}
