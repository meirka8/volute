use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use cvc_core::db::{ConversationSummary, CvcStore};
use cvc_core::privacy;
use cvc_core::repository::RepositoryLayout;
use git2::Repository;
use std::env;

/// Renders one summary line. Kept width-stable so the picker and the listing
/// read identically.
pub(crate) fn render_line(summary: &ConversationSummary, with_destination: bool) -> String {
    let when = |seconds: i64| {
        Utc.timestamp_opt(seconds, 0)
            .single()
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "-".into())
    };
    let state = if !with_destination {
        String::new()
    } else if summary.published > 0 && summary.published == summary.thoughts {
        format!("  [shared, {} published]", summary.published)
    } else if summary.shared {
        format!(
            "  [shared, {}/{} published]",
            summary.published, summary.thoughts
        )
    } else {
        "  [private]".into()
    };
    format!(
        "{}  {:>3} thought(s), {:>3} linked  {}{}\n      {}",
        when(summary.last_activity),
        summary.thoughts,
        summary.linked,
        summary.id,
        state,
        summary.title,
    )
}

/// Resolve the destination fingerprint for display, tolerating repositories
/// with no usable remote: the listing is read-only and must still work there.
pub(crate) fn display_destination(
    repo: &Repository,
    requested: Option<&str>,
) -> Option<(String, String)> {
    let name = requested.unwrap_or("origin").to_owned();
    privacy::remote_destination(repo, &name)
        .ok()
        .map(|destination| (name, destination.fingerprint))
}

pub async fn run(requested: Option<&str>, limit: usize) -> Result<()> {
    let current_dir = env::current_dir()?;
    let layout =
        RepositoryLayout::discover(&current_dir).context("Failed to discover Git repository")?;
    layout.worktree_root()?;
    if !layout.cvc_dir().exists() {
        println!("CVC is not initialized in this repository. Run 'cvc init' to setup.");
        return Ok(());
    }
    let store = CvcStore::open_initialized(layout.db_path())?;
    let repo = layout.into_repository();
    let destination = display_destination(&repo, requested);
    let summaries = store.list_conversation_summaries(
        destination
            .as_ref()
            .map(|(_, fingerprint)| fingerprint.as_str()),
        limit,
    )?;
    if summaries.is_empty() {
        println!("No conversations captured yet.");
        return Ok(());
    }
    match &destination {
        Some((name, fingerprint)) => println!(
            "Conversations (most recent first); share state for remote '{}' ({}…):",
            name,
            &fingerprint[..12]
        ),
        None => println!("Conversations (most recent first); no remote resolved for share state:"),
    }
    for summary in &summaries {
        println!("{}", render_line(summary, destination.is_some()));
    }
    println!(
        "\nShare one with: cvc share <conversation-id> --remote <name>  (or run `cvc share` interactively)"
    );
    Ok(())
}
