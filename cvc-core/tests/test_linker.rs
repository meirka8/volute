use chrono::{Duration, Utc};
use cvc_core::db::CvcStore;
use cvc_core::linker;
use cvc_core::models::*;
use cvc_core::privacy::{McpCapture, PreparedPolicy};
use git2::{Repository, Signature, Status, Time};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn commit_files(repo: &Repository, files: &[(&str, &str)], message: &str) -> anyhow::Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("missing workdir"))?;
    for (path, content) in files {
        let path = workdir.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
    }

    let mut index = repo.index()?;
    for (path, _) in files {
        index.add_path(Path::new(path))?;
    }
    index.write()?;
    let tree = repo.find_tree(index.write_tree()?)?;
    // Git commit timestamps are second-granular while interaction timestamps
    // are persisted as seconds. Keep the fixture parent safely before nodes.
    let signature = if repo.head().is_err() {
        Signature::new(
            "Commit Author",
            "author@example.com",
            &Time::new(Utc::now().timestamp() - 60, 0),
        )?
    } else {
        Signature::now("Commit Author", "author@example.com")?
    };
    let parents = repo
        .head()
        .ok()
        .and_then(|head| head.peel_to_commit().ok())
        .into_iter()
        .collect::<Vec<_>>();
    let parent_refs = parents.iter().collect::<Vec<_>>();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parent_refs,
    )?;
    Ok(())
}

fn write_file(repo: &Repository, path: &str, content: &str) -> anyhow::Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("missing workdir"))?;
    let path = workdir.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

fn add_interaction(
    store: &CvcStore,
    conversation_id: &str,
    timestamp: chrono::DateTime<Utc>,
    context_path: Option<&str>,
) -> anyhow::Result<InteractionId> {
    let id = InteractionId::new();
    let interaction = Interaction {
        id: id.clone(),
        conversation_id: conversation_id.to_owned(),
        parent_id: None,
        timestamp,
        author: Author::Human,
        user_prompt: "Thinking about code".to_owned(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    };
    let context_items = context_path
        .map(|file_path| {
            vec![ContextItem {
                id: None,
                interaction_id: id.clone(),
                file_path: file_path.to_owned(),
                git_blob_sha: None,
                dirty_patch: None,
                start_line: None,
                end_line: None,
            }]
        })
        .unwrap_or_default();
    store.capture_mcp(McpCapture::new(
        Conversation {
            id: conversation_id.to_owned(),
            title: conversation_id.to_owned(),
            created_at: timestamp,
        },
        interaction,
        context_items,
        Vec::new(),
        PreparedPolicy::built_ins_only(),
    ))?;
    Ok(id)
}

fn setup() -> anyhow::Result<(TempDir, Repository, CvcStore)> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let mut config = repo.config()?;
    config.set_str("user.name", "Configured Linker")?;
    config.set_str("user.email", "linker@example.com")?;
    commit_files(
        &repo,
        &[("committed.rs", "old"), ("other.rs", "old")],
        "initial",
    )?;
    let store = CvcStore::open(temp_dir.path().join("cvc.db"))?;
    store.init()?;
    Ok((temp_dir, repo, store))
}

#[test]
fn stale_unrelated_chat_is_not_linked() -> anyhow::Result<()> {
    let (_temp_dir, repo, store) = setup()?;
    let stale = add_interaction(
        &store,
        "stale",
        Utc::now() - Duration::hours(25),
        Some("other.rs"),
    )?;
    commit_files(&repo, &[("committed.rs", "new")], "change committed")?;

    assert_eq!(
        linker::link_current_commit_to_floating_nodes(&repo, &store)?,
        0
    );
    assert!(store.get_artifact_links(&stale)?.is_empty());
    Ok(())
}

#[test]
fn interaction_at_parent_timestamp_is_excluded_by_strict_lower_bound() -> anyhow::Result<()> {
    let (_temp_dir, repo, store) = setup()?;
    let parent_time = repo.head()?.peel_to_commit()?.time().seconds();
    let at_parent = add_interaction(
        &store,
        "at-parent-boundary",
        chrono::DateTime::from_timestamp(parent_time, 0).unwrap(),
        Some("committed.rs"),
    )?;
    commit_files(&repo, &[("committed.rs", "new")], "change committed")?;

    assert_eq!(
        linker::link_current_commit_to_floating_nodes(&repo, &store)?,
        0,
        "eligibility is strictly after, not equal to, the parent timestamp"
    );
    assert!(store.get_artifact_links(&at_parent)?.is_empty());
    Ok(())
}

#[test]
fn partial_commit_links_only_the_staged_files_conversation() -> anyhow::Result<()> {
    let (_temp_dir, repo, store) = setup()?;
    let committed = add_interaction(&store, "committed", Utc::now(), Some("committed.rs"))?;
    let disjoint = add_interaction(&store, "disjoint", Utc::now(), Some("other.rs"))?;
    // Deliberately leave this edit unstaged. The commit helper stages only the
    // requested file, so the diff used by the linker must exclude other.rs.
    write_file(&repo, "other.rs", "unstaged")?;
    commit_files(&repo, &[("committed.rs", "new")], "change committed")?;

    let statuses = repo.statuses(None)?;
    assert!(statuses.iter().any(|entry| {
        entry.path() == Some("other.rs") && entry.status().contains(Status::WT_MODIFIED)
    }));
    assert_eq!(
        fs::read_to_string(repo.workdir().unwrap().join("other.rs"))?,
        "unstaged"
    );

    assert_eq!(
        linker::link_current_commit_to_floating_nodes(&repo, &store)?,
        1
    );
    let links = store.get_artifact_links(&committed)?;
    assert_eq!(links[0].link_type, "generated");
    assert_eq!(links[0].linked_by.as_deref(), Some("linker@example.com"));
    assert!(store.get_artifact_links(&disjoint)?.is_empty());
    Ok(())
}

#[test]
fn no_context_node_uses_temporal_link() -> anyhow::Result<()> {
    let (_temp_dir, repo, store) = setup()?;
    let node = add_interaction(&store, "no-context", Utc::now(), None)?;
    commit_files(&repo, &[("committed.rs", "new")], "change committed")?;

    assert_eq!(
        linker::link_current_commit_to_floating_nodes(&repo, &store)?,
        1
    );
    let links = store.get_artifact_links(&node)?;
    assert_eq!(links[0].link_type, "temporal");
    assert_eq!(links[0].linked_by.as_deref(), Some("linker@example.com"));
    Ok(())
}

#[test]
fn qualifying_conversation_links_all_its_in_window_nodes() -> anyhow::Result<()> {
    let (_temp_dir, repo, store) = setup()?;
    let overlapping = add_interaction(&store, "cohesive", Utc::now(), Some("committed.rs"))?;
    // This node has deliberately disjoint explicit context: cohesion, rather
    // than temporal fallback, must bind it after its conversation qualifies.
    let companion = add_interaction(&store, "cohesive", Utc::now(), Some("other.rs"))?;
    commit_files(&repo, &[("committed.rs", "new")], "change committed")?;

    assert_eq!(
        linker::link_current_commit_to_floating_nodes(&repo, &store)?,
        2
    );
    assert_eq!(
        store.get_artifact_links(&overlapping)?[0].link_type,
        "generated"
    );
    assert_eq!(
        store.get_artifact_links(&companion)?[0].link_type,
        "generated"
    );
    Ok(())
}

#[test]
fn root_commit_links_overlapping_context() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;
    let store = CvcStore::open(temp_dir.path().join("cvc.db"))?;
    store.init()?;
    let node = add_interaction(&store, "root", Utc::now(), Some("new.rs"))?;

    commit_files(&repo, &[("new.rs", "new")], "root commit")?;

    assert_eq!(
        linker::link_current_commit_to_floating_nodes(&repo, &store)?,
        1
    );
    assert_eq!(store.get_artifact_links(&node)?[0].link_type, "generated");
    Ok(())
}

#[test]
fn link_window_uses_default_for_missing_or_invalid_configuration() -> anyhow::Result<()> {
    let (_temp_dir, repo, _store) = setup()?;
    assert_eq!(
        linker::LinkPolicy::from_repository(&repo).window_secs(),
        linker::DEFAULT_LINK_WINDOW_SECS
    );

    let mut config = repo.config()?;
    config.set_str("cvc.linkWindow", "90")?;
    assert_eq!(linker::LinkPolicy::from_repository(&repo).window_secs(), 90);

    for invalid in ["-1", "not-a-number", "18446744073709551615", "2592001"] {
        config.set_str("cvc.linkWindow", invalid)?;
        assert_eq!(
            linker::LinkPolicy::from_repository(&repo).window_secs(),
            linker::DEFAULT_LINK_WINDOW_SECS,
            "{invalid} must fall back rather than disabling linking"
        );
    }
    config.set_str("cvc.linkWindow", " 0 ")?;
    assert_eq!(linker::LinkPolicy::from_repository(&repo).window_secs(), 0);
    config.set_str("cvc.linkWindow", "2592000")?;
    assert_eq!(
        linker::LinkPolicy::from_repository(&repo).window_secs(),
        linker::MAX_LINK_WINDOW_SECS
    );
    Ok(())
}

#[test]
fn zero_window_disables_automatic_linking() -> anyhow::Result<()> {
    let (_temp_dir, repo, store) = setup()?;
    let node = add_interaction(&store, "disabled", Utc::now(), None)?;
    let mut config = repo.config()?;
    config.set_str("cvc.linkWindow", "0")?;
    commit_files(&repo, &[("committed.rs", "new")], "disabled")?;
    assert_eq!(
        linker::link_current_commit_to_floating_nodes(&repo, &store)?,
        0
    );
    assert!(store.get_artifact_links(&node)?.is_empty());
    Ok(())
}

#[test]
fn future_interaction_beyond_clock_skew_is_not_linked() -> anyhow::Result<()> {
    let (_temp_dir, repo, store) = setup()?;
    let node = add_interaction(&store, "future", Utc::now() + Duration::minutes(6), None)?;
    commit_files(&repo, &[("committed.rs", "new")], "future")?;
    assert_eq!(
        linker::link_current_commit_to_floating_nodes(&repo, &store)?,
        0
    );
    assert!(store.get_artifact_links(&node)?.is_empty());
    Ok(())
}
