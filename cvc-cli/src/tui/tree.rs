//! Grouping conversations into parents and their agents, and the order they are
//! listed in. Pure data: no terminal, no store.
//!
//! Claude Code stores each subagent as its own conversation named
//! `<session>:agent-<agent id>` (see `cvc ingest claude-code`). The view puts
//! such a conversation under its session, which is still a separate
//! conversation for sharing: nothing here links their share state.

use std::cmp::Reverse;
use std::collections::HashMap;

use cvc_core::db::ConversationSummary;

const AGENT_MARKER: &str = ":agent-";

/// `(parent id, agent id)` of a conversation id shaped `<parent>:agent-<agent>`,
/// split at the last marker so that an imported conversation, whose id starts
/// `remote:<fingerprint>:` and keeps that prefix in the parent's id, groups
/// under its imported session. `None` when the id has no marker or either side
/// of it is empty.
pub fn split_agent_id(id: &str) -> Option<(&str, &str)> {
    let (parent, agent) = id.rsplit_once(AGENT_MARKER)?;
    (!parent.is_empty() && !agent.is_empty()).then_some((parent, agent))
}

/// Where a conversation sits in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A conversation with no agents under it.
    Plain,
    /// A conversation with agents under it, in the order they were spawned
    /// (indices into [`Tree::entries`]).
    Parent { children: Vec<usize> },
    /// An agent conversation listed under its parent (an index into
    /// [`Tree::entries`]).
    Child { parent: usize },
    /// An agent conversation whose parent is not in the list, listed at its own
    /// place.
    Orphan,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub summary: ConversationSummary,
    pub kind: Kind,
}

/// The listed conversations and the order of their groups.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub entries: Vec<Entry>,
    /// The top-level entries (everything except a [`Kind::Child`]) in list
    /// order: the group whose newest activity is latest first, ties by id.
    pub order: Vec<usize>,
}

impl Tree {
    /// Builds the tree of the conversations that have at least one thought; a
    /// conversation with none is not listed at all.
    ///
    /// A conversation is an agent's child when the part of its id before the
    /// last `:agent-` is the id of a listed conversation that is not itself an
    /// agent's child; the tree is two levels deep, so an agent of an agent is
    /// listed on its own like an orphan.
    pub fn build(summaries: &[ConversationSummary]) -> Self {
        let summaries: Vec<&ConversationSummary> =
            summaries.iter().filter(|s| s.thoughts > 0).collect();
        let index: HashMap<&str, usize> = summaries
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.as_str(), i))
            .collect();
        let candidate: Vec<Option<usize>> = summaries
            .iter()
            .map(|s| {
                split_agent_id(&s.id)
                    .and_then(|(parent, _)| index.get(parent))
                    .copied()
            })
            .collect();
        let parent: Vec<Option<usize>> = candidate
            .iter()
            .map(|c| c.filter(|&p| candidate[p].is_none()))
            .collect();

        let mut children: Vec<Vec<usize>> = vec![Vec::new(); summaries.len()];
        for (child, parent) in parent.iter().enumerate() {
            if let Some(parent) = parent {
                children[*parent].push(child);
            }
        }
        for list in &mut children {
            // Spawn order.
            list.sort_by(|&a, &b| {
                (summaries[a].first_activity, &summaries[a].id)
                    .cmp(&(summaries[b].first_activity, &summaries[b].id))
            });
        }

        let entries: Vec<Entry> = summaries
            .iter()
            .enumerate()
            .map(|(i, summary)| {
                let kind = if let Some(parent) = parent[i] {
                    Kind::Child { parent }
                } else if !children[i].is_empty() {
                    Kind::Parent {
                        children: children[i].clone(),
                    }
                } else if split_agent_id(&summary.id).is_some() {
                    Kind::Orphan
                } else {
                    Kind::Plain
                };
                Entry {
                    summary: (*summary).clone(),
                    kind,
                }
            })
            .collect();

        let newest = |top: usize| {
            children[top]
                .iter()
                .map(|&child| summaries[child].last_activity)
                .chain(std::iter::once(summaries[top].last_activity))
                .max()
                .unwrap_or(0)
        };
        let mut order: Vec<usize> = (0..entries.len())
            .filter(|&i| parent[i].is_none())
            .collect();
        order.sort_by(|&a, &b| {
            (Reverse(newest(a)), &summaries[a].id).cmp(&(Reverse(newest(b)), &summaries[b].id))
        });
        Self { entries, order }
    }

    /// The entries in the order they are shown, with each parent's agents
    /// after it when `expanded` says the parent is open.
    pub fn visible(&self, expanded: impl Fn(&Entry) -> bool) -> Vec<usize> {
        let mut shown = Vec::with_capacity(self.entries.len());
        for &top in &self.order {
            shown.push(top);
            if let Kind::Parent { children } = &self.entries[top].kind {
                if expanded(&self.entries[top]) {
                    shown.extend(children);
                }
            }
        }
        shown
    }

    /// The entry an agent is listed under.
    pub fn parent_of(&self, index: usize) -> Option<usize> {
        match self.entries[index].kind {
            Kind::Child { parent } => Some(parent),
            _ => None,
        }
    }

    /// The agents listed under an entry (empty for anything but a parent).
    pub fn children_of(&self, index: usize) -> &[usize] {
        match &self.entries[index].kind {
            Kind::Parent { children } => children,
            _ => &[],
        }
    }

    /// The position of the conversation with this id.
    pub fn position(&self, id: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.summary.id == id)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A listed conversation with the given activity range and `thoughts`.
    pub fn conversation(id: &str, first: i64, last: i64, thoughts: i64) -> ConversationSummary {
        ConversationSummary {
            id: id.to_owned(),
            title: format!("title of {id}"),
            thoughts,
            linked: 0,
            first_activity: first,
            last_activity: last,
            shared: false,
            shared_count: 0,
            published: 0,
            unconfirmed: 0,
            share_future: None,
            imported: false,
            held: 0,
            held_published: 0,
        }
    }

    fn ids(tree: &Tree, indices: &[usize]) -> Vec<String> {
        indices
            .iter()
            .map(|&i| tree.entries[i].summary.id.clone())
            .collect()
    }

    fn listed(tree: &Tree) -> Vec<String> {
        ids(tree, &tree.visible(|_| true))
    }

    fn kind_of(tree: &Tree, id: &str) -> Kind {
        tree.entries[tree.position(id).unwrap()].kind.clone()
    }

    #[test]
    fn an_agent_id_is_split_at_the_last_marker() {
        assert_eq!(
            split_agent_id("session-1:agent-a1b2"),
            Some(("session-1", "a1b2"))
        );
        assert_eq!(
            split_agent_id("remote:abc123:session-1:agent-a1b2"),
            Some(("remote:abc123:session-1", "a1b2"))
        );
        // The last marker wins.
        assert_eq!(
            split_agent_id("session:agent-a:agent-b"),
            Some(("session:agent-a", "b"))
        );
        assert_eq!(split_agent_id("session-1"), None);
        assert_eq!(split_agent_id("session-1:agent-"), None);
        assert_eq!(split_agent_id(":agent-a1b2"), None);
        assert_eq!(split_agent_id("session-1agent-a1b2"), None);
    }

    #[test]
    fn agents_are_listed_under_their_parent_in_spawn_order() {
        let tree = Tree::build(&[
            conversation("s:agent-late", 300, 400, 2),
            conversation("s", 100, 500, 10),
            conversation("s:agent-early", 200, 250, 3),
            conversation("s:agent-tie-b", 300, 301, 1),
            conversation("s:agent-tie-a", 300, 302, 1),
        ]);
        assert_eq!(
            listed(&tree),
            vec![
                "s",
                "s:agent-early",
                // Same first activity: by id.
                "s:agent-late",
                "s:agent-tie-a",
                "s:agent-tie-b",
            ]
        );
        let Kind::Parent { children } = kind_of(&tree, "s") else {
            panic!("s should be a parent");
        };
        assert_eq!(children.len(), 4);
        assert!(matches!(
            kind_of(&tree, "s:agent-early"),
            Kind::Child { .. }
        ));
        assert_eq!(tree.order.len(), 1);
    }

    #[test]
    fn a_collapsed_parent_hides_its_agents() {
        let tree = Tree::build(&[
            conversation("s", 100, 200, 4),
            conversation("s:agent-a", 110, 120, 1),
        ]);
        assert_eq!(ids(&tree, &tree.visible(|_| false)), vec!["s"]);
        assert_eq!(ids(&tree, &tree.visible(|_| true)), vec!["s", "s:agent-a"]);
    }

    #[test]
    fn an_agent_without_a_parent_is_listed_on_its_own() {
        let tree = Tree::build(&[
            conversation("lonely:agent-x", 100, 900, 2),
            conversation("other", 100, 500, 1),
        ]);
        assert_eq!(kind_of(&tree, "lonely:agent-x"), Kind::Orphan);
        assert_eq!(kind_of(&tree, "other"), Kind::Plain);
        // At its own recency.
        assert_eq!(listed(&tree), vec!["lonely:agent-x", "other"]);
    }

    #[test]
    fn imported_agents_group_under_their_imported_session() {
        let prefix = format!("remote:{}:", "a".repeat(64));
        let parent = format!("{prefix}session-9");
        let child = format!("{parent}:agent-7");
        let tree = Tree::build(&[
            conversation(&child, 150, 160, 1),
            conversation(&parent, 100, 200, 5),
            // A local session with the same agent id is a different conversation.
            conversation("session-9:agent-7", 150, 160, 1),
        ]);
        assert!(matches!(kind_of(&tree, &parent), Kind::Parent { .. }));
        assert!(matches!(kind_of(&tree, &child), Kind::Child { .. }));
        assert_eq!(kind_of(&tree, "session-9:agent-7"), Kind::Orphan);
    }

    #[test]
    fn conversations_without_thoughts_are_not_listed() {
        let tree = Tree::build(&[
            conversation("empty", 100, 900, 0),
            conversation("s", 100, 200, 1),
            conversation("s:agent-a", 110, 120, 0),
        ]);
        assert_eq!(listed(&tree), vec!["s"]);
        // With no listed agent it is a plain conversation, not a parent.
        assert_eq!(kind_of(&tree, "s"), Kind::Plain);
    }

    #[test]
    fn an_agent_whose_parent_has_no_thoughts_is_an_orphan() {
        let tree = Tree::build(&[
            conversation("s", 100, 200, 0),
            conversation("s:agent-a", 110, 120, 2),
        ]);
        assert_eq!(listed(&tree), vec!["s:agent-a"]);
        assert_eq!(kind_of(&tree, "s:agent-a"), Kind::Orphan);
    }

    #[test]
    fn groups_are_ordered_by_their_newest_activity_then_id() {
        let tree = Tree::build(&[
            conversation("old", 10, 100, 1),
            // Its own activity is old, but an agent was active last.
            conversation("busy", 10, 50, 1),
            conversation("busy:agent-a", 20, 900, 1),
            conversation("tie-b", 10, 500, 1),
            conversation("tie-a", 10, 500, 1),
            conversation("fresh", 10, 700, 1),
        ]);
        assert_eq!(
            listed(&tree),
            vec!["busy", "busy:agent-a", "fresh", "tie-a", "tie-b", "old"]
        );
    }

    #[test]
    fn the_tree_is_two_levels_deep() {
        let tree = Tree::build(&[
            conversation("s", 100, 200, 1),
            conversation("s:agent-a", 110, 120, 1),
            conversation("s:agent-a:agent-b", 115, 118, 1),
        ]);
        assert!(matches!(kind_of(&tree, "s"), Kind::Parent { .. }));
        assert!(matches!(kind_of(&tree, "s:agent-a"), Kind::Child { .. }));
        // Listed on its own rather than hidden below a row that has no
        // disclosure.
        assert_eq!(kind_of(&tree, "s:agent-a:agent-b"), Kind::Orphan);
        assert_eq!(
            listed(&tree).len(),
            3,
            "every listed conversation is reachable"
        );
    }

    #[test]
    fn lookups_follow_the_tree() {
        let tree = Tree::build(&[
            conversation("s", 100, 200, 1),
            conversation("s:agent-a", 110, 120, 1),
        ]);
        let parent = tree.position("s").unwrap();
        let child = tree.position("s:agent-a").unwrap();
        assert_eq!(tree.parent_of(child), Some(parent));
        assert_eq!(tree.parent_of(parent), None);
        assert_eq!(tree.children_of(parent), &[child]);
        assert!(tree.children_of(child).is_empty());
        assert_eq!(tree.position("missing"), None);
    }
}
