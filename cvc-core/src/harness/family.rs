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
use crate::db::{CvcStore, DbError, RelationKind, MAX_RELATION_DEPTH};
use crate::harness::HarnessId;
use std::collections::BTreeSet;

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
