//! Transcript-derived commit links for a Claude Code session family.
//!
//! The rescue itself is the one every harness shares,
//! [`crate::harness::family::link_family_commits`]; see that module for what
//! it links and why. Claude Code's family is the session's own conversation
//! and one conversation per subagent transcript found beside it: a
//! subagent's entries carry the parent's session id and run in the same
//! worktree, and the session that commits what a subagent wrote is usually
//! the parent, so the evidence of a commit made by any member covers all of
//! them.
use crate::db::CvcStore;
use crate::harness::family::{self, Family};
use crate::harness::HarnessId;
use git2::Repository;

pub use crate::harness::family::CommitBearingResponse;

/// The conversations of one Claude Code session: its own, and one per subagent
/// transcript found beside it. An explicit set, never a prefix match, so no
/// other session's conversation, whatever its id looks like, can belong to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFamily(Family);

impl SessionFamily {
    pub fn new(parent: impl Into<String>) -> Self {
        Self(Family::new(HarnessId::ClaudeCode, parent))
    }

    /// Adds the conversation a subagent transcript is stored under.
    pub fn add_subagent(&mut self, conversation_id: impl Into<String>) {
        self.0.add_member(conversation_id);
    }

    /// The family, as every harness's is.
    pub fn family(&self) -> &Family {
        &self.0
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

impl From<family::FamilyLinks> for FamilyLinks {
    fn from(links: family::FamilyLinks) -> Self {
        Self {
            parent: links.root,
            subagents: links.members,
        }
    }
}

/// Links floating responses of the session family to the commits any of its
/// transcripts reported creating, returning the new links (see
/// [`family::link_family_commits`]).
pub fn link_transcript_commits(
    repo: &Repository,
    store: &CvcStore,
    family: &SessionFamily,
    worktree_fingerprint: &str,
    responses: &[CommitBearingResponse],
    linked_by: Option<&str>,
) -> Result<FamilyLinks, crate::db::DbError> {
    family::link_family_commits(
        repo,
        store,
        &family.0,
        worktree_fingerprint,
        responses,
        linked_by,
    )
    .map(FamilyLinks::from)
}
