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
//!
//! # Replayed commits
//!
//! A run only reads a record from its cursor, so a commit whose response an
//! earlier batch or run read is known to a later rescue from the family's
//! stored evidence alone ([`crate::db::HarnessCommitEvidence`]). Such a
//! replayed commit takes part exactly as a commit the run read itself, with
//! three checks more:
//!
//! * it is replayed only while HEAD of the run's worktree, a local branch,
//!   a remote-tracking branch or a tag still reaches it: a commit that a
//!   rebase or an amend left behind gets no new links;
//! * it claims only rows of the worktree its reporting interaction was
//!   captured in, which is the run's (the engine replays no other), never a
//!   row without a worktree;
//! * it is resolved and its time checked again, as every commit is.
//!
//! A stored commit that is not replayed (one no longer reached, one reported
//! in another worktree, and every stored commit of a rescue that replays
//! nothing) still bounds the segments: it claims nothing, and a later
//! commit never claims the reasoning at or before it. Only the commits from
//! the latest one before the family's oldest floating response on are
//! considered at all, since nothing before that one is left to claim.
//!
//! The segments are those of one rescue of the whole family: every commit
//! known, read in this run or replayed, in time order, each claiming the
//! floating responses after the commit before it, up to its own time, with
//! no other lower bound (`cvc.linkWindow` is the time-window linker's, not
//! the rescue's). So many runs that each rescue what they know reach the
//! links one run of the whole family makes.
use crate::db::{CvcStore, DbError, RelationKind, MAX_RELATION_DEPTH};
use crate::harness::HarnessId;
use crate::linker::MAX_CLOCK_SKEW_SECS;
use crate::models::{CommitSha, InteractionId};
use chrono::{DateTime, Utc};
use git2::{BranchType, Oid, Repository, Sort};
use std::collections::{BTreeSet, HashSet};

/// How far before the oldest replayed commit the reachability walk goes on
/// looking, for history whose commit times are out of order.
const REACH_SLACK_SECS: i64 = 24 * 60 * 60;

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

/// The full id and time of the commit `candidate` names, when it names one
/// whose time is plausible for a response made at `response_at` (seconds):
/// not before it beyond clock skew, nor in the future. `None` for anything
/// else. These are the only checks a commit passes before the family's
/// evidence records it ([`crate::harness::engine`]) and again before it is
/// linked.
pub fn resolve_commit(
    repo: &Repository,
    candidate: &str,
    response_at: i64,
    now: i64,
) -> Option<(String, i64)> {
    let skew = MAX_CLOCK_SKEW_SECS;
    let object = repo.revparse_single(candidate).ok()?;
    let commit = object.as_commit()?;
    let commit_time = commit.time().seconds();
    if commit_time > now + skew || commit_time < response_at - skew {
        return None;
    }
    Some((commit.id().to_string(), commit_time))
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
    link_family_evidence(
        repo,
        store,
        family,
        worktree_fingerprint,
        responses,
        &[],
        linked_by,
    )
    .map(|rescued| rescued.links)
}

/// A commit the family's evidence recorded in an earlier batch or run
/// ([`CvcStore::harness_commit_evidence`]), replayed by a later rescue (see
/// the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayedCommit {
    /// The full commit id, as stored.
    pub commit: String,
    /// The stored response's time, in seconds.
    pub response_at: i64,
    /// Replayed, when still reached; otherwise only a boundary between the
    /// segments, which claims nothing.
    pub replay: bool,
}

/// What a rescue did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rescued {
    pub(crate) links: FamilyLinks,
    /// Replayed commits that HEAD and the local branches no longer reach:
    /// they bounded the segments and claimed nothing.
    pub(crate) unreachable: usize,
}

/// [`link_family_commits`] over this run's `responses` and the family's
/// `replayed` evidence, all of it reported in this worktree (see the module
/// docs). A commit both name is this run's.
pub(crate) fn link_family_evidence(
    repo: &Repository,
    store: &CvcStore,
    family: &Family,
    worktree_fingerprint: &str,
    responses: &[CommitBearingResponse],
    replayed: &[ReplayedCommit],
    linked_by: Option<&str>,
) -> Result<Rescued, DbError> {
    if responses.is_empty() && replayed.is_empty() {
        return Ok(Rescued::default());
    }
    let now = Utc::now().timestamp();

    // Resolve each candidate to a real commit and keep only those whose time is
    // plausible for the response that reported them: a commit cannot precede
    // the reasoning that made it (beyond clock skew), nor sit in the future.
    #[derive(PartialEq, Eq)]
    enum Claims {
        /// A commit this run read: rows of this worktree, and rows without
        /// one, as before durable evidence.
        ThisRun,
        /// A replayed commit: rows of this worktree only.
        Replayed,
        /// A replayed commit no longer reached: none.
        Nothing,
    }
    struct Verified {
        sha: String,
        time: i64,
        claims: Claims,
    }
    let mut verified: Vec<Verified> = Vec::new();
    for response in responses {
        let response_ts = response.timestamp.timestamp();
        for candidate in &response.candidates {
            let Some((sha, time)) = resolve_commit(repo, candidate, response_ts, now) else {
                continue;
            };
            match verified.iter_mut().find(|commit| commit.sha == sha) {
                Some(commit) => commit.claims = Claims::ThisRun,
                None => verified.push(Verified {
                    sha,
                    time,
                    claims: Claims::ThisRun,
                }),
            }
        }
    }
    // Stored commits: those to replay if still reached, and boundaries.
    let mut replays: Vec<(Oid, String, i64)> = Vec::new();
    for commit in replayed {
        // A stored commit is a full id the store holds; it is resolved again,
        // as every commit is before anything is linked to it, and for its
        // time.
        let Some((sha, time)) = resolve_commit(repo, &commit.commit, commit.response_at, now)
        else {
            continue;
        };
        if sha != commit.commit || verified.iter().any(|known| known.sha == sha) {
            continue;
        }
        let Ok(oid) = Oid::from_str(&sha) else {
            continue;
        };
        if replays.iter().any(|(known, _, _)| *known == oid) {
            continue;
        }
        if commit.replay {
            replays.push((oid, sha, time));
        } else {
            verified.push(Verified {
                sha,
                time,
                claims: Claims::Nothing,
            });
        }
    }

    // Floating (unlinked) interactions of this worktree, family and source,
    // oldest first. Anything already linked is excluded here and left
    // untouched.
    let conversations: Vec<&str> = std::iter::once(family.root())
        .chain(family.members())
        .collect();
    let mut floating = store.floating_family_interactions(
        worktree_fingerprint,
        family.claimed_source(),
        &conversations,
    )?;
    floating.sort_by_key(|response| response.timestamp);
    let Some(oldest) = floating.first().map(|response| response.timestamp) else {
        // Nothing is left to claim.
        return Ok(Rescued::default());
    };
    // The latest commit before the oldest floating response bounds every
    // segment that can claim anything; the commits before it can claim
    // nothing and bound nothing that matters, so they are not walked for.
    let floor = verified
        .iter()
        .map(|commit| commit.time)
        .chain(replays.iter().map(|(_, _, time)| *time))
        .filter(|time| *time < oldest)
        .max();
    if let Some(floor) = floor {
        verified.retain(|commit| commit.time >= floor);
        replays.retain(|(_, _, time)| *time >= floor);
    }
    let reached = reachable(repo, &replays);
    let mut unreachable = 0;
    for (oid, sha, time) in replays {
        let claims = if reached.contains(&oid) {
            Claims::Replayed
        } else {
            unreachable += 1;
            Claims::Nothing
        };
        verified.push(Verified { sha, time, claims });
    }
    // Earliest commit first; each claims only what follows the one before.
    verified.sort_by_key(|commit| commit.time);

    let mut claimed = vec![false; floating.len()];
    let mut total = FamilyLinks::default();
    let mut after = i64::MIN;
    for commit in &verified {
        let (from, to) = (after, commit.time);
        after = after.max(commit.time);
        if commit.claims == Claims::Nothing {
            continue;
        }
        // A commit can only have been produced by reasoning at or before it, so
        // responses after it belong to a later commit, not this one; and
        // reasoning at or before the commit before it is that commit's. No
        // skew on these bounds: the committing response is written before its
        // tool runs, so it always precedes the commit time.
        let mut ids: Vec<&InteractionId> = Vec::new();
        let mut root_ids = 0usize;
        for (index, response) in floating.iter().enumerate() {
            let in_segment = response.timestamp > from && response.timestamp <= to;
            let in_worktree = commit.claims == Claims::ThisRun
                || response.capture_worktree.as_deref() == Some(worktree_fingerprint);
            if !claimed[index] && in_segment && in_worktree {
                claimed[index] = true;
                ids.push(&response.id);
                root_ids += usize::from(family.is_root(&response.conversation_id));
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
        let inserted = store.link_rescued_interaction_batch(
            &links,
            &CommitSha::new(&commit.sha),
            linked_by,
        )?;
        let to_root = inserted.min(root_ids);
        total.root += to_root;
        total.members += inserted - to_root;
    }
    Ok(Rescued {
        links: total,
        unreachable,
    })
}

/// Which of `commits` HEAD of `repo` (the run's worktree), a local branch, a
/// remote-tracking branch or a tag still reaches: a branch deleted after it
/// was pushed is still reached through its remote-tracking branch.
/// Fail-closed: a commit the walk does not find, because the repository
/// cannot be walked or the commit lies past [`REACH_SLACK_SECS`] before the
/// oldest of them, is not reached.
fn reachable(repo: &Repository, commits: &[(Oid, String, i64)]) -> HashSet<Oid> {
    let mut found = HashSet::new();
    let mut wanted: HashSet<Oid> = commits.iter().map(|(oid, _, _)| *oid).collect();
    let Some(oldest) = commits.iter().map(|(_, _, time)| *time).min() else {
        return found;
    };
    let Ok(mut walk) = repo.revwalk() else {
        return found;
    };
    if walk.set_sorting(Sort::TIME).is_err() {
        return found;
    }
    let mut tips = Vec::new();
    if let Ok(head) = repo.head().and_then(|head| head.peel_to_commit()) {
        tips.push(head.id());
    }
    if let Ok(branches) = repo.branches(Some(BranchType::Local)) {
        for (branch, _) in branches.flatten() {
            if let Ok(commit) = branch.get().peel_to_commit() {
                tips.push(commit.id());
            }
        }
    }
    for glob in ["refs/remotes/*", "refs/tags/*"] {
        if let Ok(references) = repo.references_glob(glob) {
            for reference in references.flatten() {
                if let Ok(commit) = reference.peel_to_commit() {
                    tips.push(commit.id());
                }
            }
        }
    }
    for tip in tips {
        let _ = walk.push(tip);
    }
    let cutoff = oldest.saturating_sub(REACH_SLACK_SECS);
    for oid in walk {
        let Ok(oid) = oid else {
            break;
        };
        if wanted.remove(&oid) {
            found.insert(oid);
            if wanted.is_empty() {
                break;
            }
        }
        match repo.find_commit(oid) {
            Ok(commit) if commit.time().seconds() >= cutoff => {}
            _ => break,
        }
    }
    found
}
