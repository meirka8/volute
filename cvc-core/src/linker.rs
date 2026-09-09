use crate::db::CvcStore;
use crate::models::{CommitSha, Interaction, InteractionId};
use chrono::{DateTime, Duration, Utc};
use git2::{DiffOptions, Repository};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};
use thiserror::Error;

pub const DEFAULT_LINK_WINDOW_SECS: u64 = 24 * 60 * 60;
pub const MAX_LINK_WINDOW_SECS: u64 = 30 * 24 * 60 * 60;
pub const MAX_CLOCK_SKEW_SECS: i64 = 5 * 60;

const GENERATED_LINK_TYPE: &str = "generated";
const TEMPORAL_LINK_TYPE: &str = "temporal";

/// Bounded deterministic policy. `cvc.linkWindow` accepts unsigned seconds;
/// invalid values and values above 30 days use the 24 hour default. Zero is a
/// deliberate opt-out from automatic linking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkPolicy {
    window_secs: u64,
}

impl Default for LinkPolicy {
    fn default() -> Self {
        Self::new(DEFAULT_LINK_WINDOW_SECS)
    }
}

impl LinkPolicy {
    pub fn new(window_secs: u64) -> Self {
        Self {
            window_secs: if window_secs <= MAX_LINK_WINDOW_SECS {
                window_secs
            } else {
                DEFAULT_LINK_WINDOW_SECS
            },
        }
    }

    pub fn window_secs(self) -> u64 {
        self.window_secs
    }

    pub fn from_repository(repo: &Repository) -> Self {
        let seconds = repo
            .config()
            .ok()
            .and_then(|config| config.get_string("cvc.linkWindow").ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|seconds| *seconds <= MAX_LINK_WINDOW_SECS)
            .unwrap_or(DEFAULT_LINK_WINDOW_SECS);
        Self::new(seconds)
    }
}

#[derive(Error, Debug)]
pub enum LinkerError {
    #[error("Git error: {0}")]
    Git(#[from] crate::git::GitError),
    #[error("DB error: {0}")]
    Db(#[from] crate::db::DbError),
    #[error("Git error: {0}")]
    Git2(#[from] git2::Error),
    #[error("Clock arithmetic error: {0}")]
    Clock(String),
    #[error("worktree origin unavailable: {0}")]
    Origin(String),
}

pub type Result<T> = std::result::Result<T, LinkerError>;

pub fn link_current_commit_to_floating_nodes(repo: &Repository, db: &CvcStore) -> Result<usize> {
    link_current_commit_with_policy(repo, db, LinkPolicy::from_repository(repo))
}

pub fn link_current_commit_with_policy(
    repo: &Repository,
    db: &CvcStore,
    policy: LinkPolicy,
) -> Result<usize> {
    if policy.window_secs() == 0 {
        return Ok(0);
    }

    let head = repo.head()?.peel_to_commit()?;
    let commit_sha = CommitSha::new(head.id().to_string());
    let changed_paths = changed_paths(repo, &head)?;
    let now = Utc::now();
    let parent_time = head.parent(0).ok().map(|parent| parent.time().seconds());
    let Some(lower_bound) = eligibility_lower_bound(now, policy, parent_time) else {
        // An implausibly future parent means time evidence is not trustworthy.
        return Ok(0);
    };
    let upper_bound = now
        .checked_add_signed(Duration::seconds(MAX_CLOCK_SKEW_SECS))
        .ok_or_else(|| LinkerError::Clock("upper bound overflow".into()))?;
    // Attribution represents who ran linking, not the historical commit author.
    let linked_by = repo
        .signature()
        .ok()
        .and_then(|signature| signature.email().map(str::to_owned));

    // The shared database serves every linked worktree, so eligibility must be
    // scoped to nodes this worktree captured (legacy rows without an origin
    // stay eligible). Without an origin there is no linking: claiming another
    // active checkout's pending thoughts is worse than leaving them floating.
    let workdir = repo
        .workdir()
        .ok_or_else(|| LinkerError::Origin("bare repository has no worktree".into()))?;
    let capture_worktree = crate::repository::worktree_origin_fingerprint(workdir)
        .map_err(|error| LinkerError::Origin(error.to_string()))?;

    let eligible: Vec<Interaction> = db
        .get_floating_interactions_for_worktree(&capture_worktree)?
        .into_iter()
        .filter(|node| node.timestamp > lower_bound && node.timestamp <= upper_bound)
        .collect();

    let mut contexts = HashMap::with_capacity(eligible.len());
    let mut qualifying_conversations = HashSet::new();
    for node in &eligible {
        let context_items = db.get_context_items(&node.id)?;
        let has_explicit_context = !context_items.is_empty();
        let paths: HashSet<String> = context_items
            .into_iter()
            .filter_map(|item| normalize_repository_path(&item.file_path))
            .collect();
        if !paths.is_empty() && !paths.is_disjoint(&changed_paths) {
            qualifying_conversations.insert(node.conversation_id.clone());
        }
        contexts.insert(node.id.clone(), has_explicit_context);
    }

    let mut generated: Vec<InteractionId> = Vec::new();
    let mut temporal: Vec<InteractionId> = Vec::new();
    for node in eligible {
        if qualifying_conversations.contains(&node.conversation_id) {
            generated.push(node.id);
        } else if contexts
            .get(&node.id)
            .is_some_and(|has_context| !has_context)
        {
            temporal.push(node.id);
        }
    }

    let links: Vec<(&InteractionId, &str)> = generated
        .iter()
        .map(|id| (id, GENERATED_LINK_TYPE))
        .chain(temporal.iter().map(|id| (id, TEMPORAL_LINK_TYPE)))
        .collect();
    Ok(db.link_automatic_interaction_batch_trusted(&links, &commit_sha, linked_by.as_deref())?)
}

fn eligibility_lower_bound(
    now: DateTime<Utc>,
    policy: LinkPolicy,
    parent_time: Option<i64>,
) -> Option<DateTime<Utc>> {
    let window_start = now.checked_sub_signed(Duration::seconds(policy.window_secs() as i64))?;
    let max_parent_time = now.checked_add_signed(Duration::seconds(MAX_CLOCK_SKEW_SECS))?;
    match parent_time.and_then(|seconds| DateTime::from_timestamp(seconds, 0)) {
        Some(parent_time) if parent_time > max_parent_time => None,
        Some(parent_time) if parent_time > window_start => Some(parent_time),
        _ => Some(window_start),
    }
}

fn changed_paths(repo: &Repository, head: &git2::Commit<'_>) -> Result<HashSet<String>> {
    let new_tree = head.tree()?;
    let parent_tree = head
        .parent(0)
        .ok()
        .map(|parent| parent.tree())
        .transpose()?;
    let diff = repo.diff_tree_to_tree(
        parent_tree.as_ref(),
        Some(&new_tree),
        Some(&mut DiffOptions::new()),
    )?;
    let mut paths = HashSet::new();
    for delta in diff.deltas() {
        for file in [delta.old_file(), delta.new_file()] {
            // Non-UTF8 Git paths cannot safely match UTF-8 context strings.
            if let Some(path) = file.path().and_then(|path| path.to_str()) {
                if let Some(path) = normalize_repository_path(path) {
                    paths.insert(path);
                }
            }
        }
    }
    Ok(paths)
}

fn normalize_repository_path(path: &str) -> Option<String> {
    if path.is_empty() || path.contains('\0') {
        return None;
    }
    let slash_separated = path.replace('\\', "/");
    if slash_separated.starts_with("//")
        || (slash_separated.len() >= 2
            && slash_separated.as_bytes()[0].is_ascii_alphabetic()
            && slash_separated.as_bytes()[1] == b':')
    {
        return None;
    }
    let path = Path::new(&slash_separated);
    if path.is_absolute() {
        return None;
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => components.push(value.to_str()?.to_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

#[cfg(test)]
mod tests {
    use super::{eligibility_lower_bound, normalize_repository_path, LinkPolicy};
    use chrono::{Duration, TimeZone, Utc};

    #[test]
    fn future_parent_fails_closed() {
        let now = Utc.with_ymd_and_hms(2026, 1, 2, 12, 0, 0).unwrap();
        assert_eq!(
            eligibility_lower_bound(
                now,
                LinkPolicy::default(),
                Some((now + Duration::minutes(6)).timestamp()),
            ),
            None
        );
    }

    #[test]
    fn rejects_cross_platform_absolute_context_paths() {
        assert_eq!(normalize_repository_path("C:\\work\\file.rs"), None);
        assert_eq!(normalize_repository_path("C:secret.rs"), None);
        assert_eq!(normalize_repository_path("\\\\host\\share\\file.rs"), None);
    }
}
