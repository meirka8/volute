use crate::protocol::*;
use crate::state::AppState;
use cvc_core::models::{Author, Interaction, InteractionId};
use std::process::Command;
use std::sync::Arc;
use tokio::task;
use tower_lsp::lsp_types::MessageType;
use tower_lsp::Client;

pub async fn handle_session_start(
    client: &Client,
    _state: Arc<AppState>,
    params: SessionStartParams,
) {
    client
        .log_message(
            MessageType::INFO,
            format!(
                "Session started: {} (TS: {:?})",
                params.title, params.timestamp
            ),
        )
        .await;
}

pub async fn handle_turn_start(client: &Client, state: Arc<AppState>, params: TurnStartParams) {
    client
        .log_message(
            MessageType::INFO,
            format!(
                "Turn started (ID: {}). Prompt: {}",
                params.id, params.prompt
            ),
        )
        .await;

    // Concurrent insert
    state.pending_turns.insert(params.id, params.prompt);
}

pub async fn handle_turn_end(client: &Client, state: Arc<AppState>, params: TurnEndParams) {
    // Concurrent retrieval
    let prompt = state
        .pending_turns
        .remove(&params.id)
        .map(|(_, p)| p)
        .unwrap_or_else(|| {
            // Warn if prompt missing (unbalanced calls)
            // Ideally we'd log this warning to client
            "Unknown prompt".to_string()
        });

    let state_clone = state.clone();
    let client_clone = client.clone();

    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "default-session".to_string(),
        parent_id: None,
        timestamp: chrono::Utc::now(),
        author: Author::Human,
        user_prompt: prompt,
        model_name: params.model,
        model_cot: params.chain_of_thought,
        model_response: params.response,
    };

    // Offload DB write to background thread
    let result = task::spawn_blocking(move || {
        // Safer lock handling
        let store_guard = state_clone.store.lock().expect("CVC Store mutex poisoned");

        if let Some(store) = store_guard.as_ref() {
            store.create_interaction(&interaction)
        } else {
            Err(cvc_core::db::DbError::Migration("DB not open".to_string()))
        }
    })
    .await;

    match result {
        Ok(Ok(_)) => {
            client_clone
                .log_message(MessageType::INFO, "Interaction saved to DB")
                .await;
        }
        Ok(Err(e)) => {
            client_clone
                .log_message(
                    MessageType::ERROR,
                    format!("Failed to save interaction: {}", e),
                )
                .await;
        }
        Err(e) => {
            client_clone
                .log_message(MessageType::ERROR, format!("Join error: {}", e))
                .await;
        }
    }
}

pub async fn handle_link_commit(client: &Client, _state: Arc<AppState>, params: LinkCommitParams) {
    client
        .log_message(
            MessageType::INFO,
            format!(
                "Linking commit {} to interactions {:?}",
                params.commit_sha, params.interaction_ids
            ),
        )
        .await;
    // TODO: Implement actual linking logic in CVC Store
}

/// Handle timeline/get request - returns pending thoughts and commits with linked interactions
pub async fn handle_timeline_get(
    client: &Client,
    state: Arc<AppState>,
    params: TimelineGetParams,
) -> TimelineGetResponse {
    let max_items = params.max_items.unwrap_or(50) as usize;
    let include_unbound = params.include_unbound.unwrap_or(true);

    client
        .log_message(
            MessageType::INFO,
            format!(
                "Timeline get: max_items={}, include_unbound={}",
                max_items, include_unbound
            ),
        )
        .await;

    let state_clone = state.clone();
    let root_path = state.root_path.lock().unwrap().clone();

    // Run DB operations in blocking thread
    let result = task::spawn_blocking(move || {
        let store_guard = state_clone.store.lock().expect("CVC Store mutex poisoned");

        if let Some(store) = store_guard.as_ref() {
            // Get pending (floating) interactions
            let pending = if include_unbound {
                store
                    .get_floating_interactions()
                    .unwrap_or_default()
                    .into_iter()
                    .take(max_items)
                    .map(interaction_to_summary)
                    .collect()
            } else {
                Vec::new()
            };

            // Get commits with their linked interactions
            let commits_data = store.get_commits_with_interactions().unwrap_or_default();

            // Convert to protocol format, getting commit messages from git
            let commits: Vec<CommitWithThoughts> = commits_data
                .into_iter()
                .take(max_items)
                .map(|(sha, interactions)| {
                    let (message, timestamp) =
                        get_commit_info(&sha.as_str(), root_path.as_ref().map(|p| p.as_path()));

                    CommitWithThoughts {
                        sha: sha.as_str().to_string(),
                        message,
                        timestamp,
                        thoughts: interactions
                            .into_iter()
                            .map(interaction_to_summary)
                            .collect(),
                    }
                })
                .collect();

            (pending, commits)
        } else {
            (Vec::new(), Vec::new())
        }
    })
    .await;

    match result {
        Ok((pending, commits)) => TimelineGetResponse { pending, commits },
        Err(e) => {
            client
                .log_message(MessageType::ERROR, format!("Timeline get failed: {}", e))
                .await;
            TimelineGetResponse {
                pending: Vec::new(),
                commits: Vec::new(),
            }
        }
    }
}

/// Handle interaction/get request - returns full details for a specific interaction
pub async fn handle_interaction_get(
    client: &Client,
    state: Arc<AppState>,
    params: InteractionGetParams,
) -> Option<InteractionDetail> {
    client
        .log_message(
            MessageType::INFO,
            format!("Interaction get: id={}", params.id),
        )
        .await;

    let state_clone = state.clone();
    let interaction_id = params.id.clone();

    let result = task::spawn_blocking(move || {
        let store_guard = state_clone.store.lock().expect("CVC Store mutex poisoned");

        if let Some(store) = store_guard.as_ref() {
            // Parse the interaction ID
            let id: InteractionId = match interaction_id.parse() {
                Ok(id) => id,
                Err(_) => return None,
            };

            // Get the interaction
            let interaction = match store.get_interaction(&id) {
                Ok(Some(i)) => i,
                _ => return None,
            };

            // Get linked commit if any
            let linked_commit = store.get_artifact_links(&id).ok().and_then(|links| {
                links
                    .first()
                    .map(|l| l.git_commit_hash.as_str().to_string())
            });

            // Get context files
            let context_files: Vec<ContextFileInfo> = store
                .get_context_items(&id)
                .unwrap_or_default()
                .into_iter()
                .map(|item| ContextFileInfo {
                    file_path: item.file_path,
                    start_line: item.start_line,
                    end_line: item.end_line,
                })
                .collect();

            let author_str = match interaction.author {
                Author::Human => "human",
                Author::Agent => "agent",
                Author::System => "system",
                Author::External => "external",
            };

            Some(InteractionDetail {
                id: interaction.id.to_string(),
                timestamp: interaction.timestamp.timestamp(),
                author: author_str.to_string(),
                user_prompt: interaction.user_prompt,
                model_name: interaction.model_name,
                model_response: interaction.model_response,
                chain_of_thought: interaction.model_cot,
                linked_commit,
                context_files,
            })
        } else {
            None
        }
    })
    .await;

    match result {
        Ok(detail) => detail,
        Err(e) => {
            client
                .log_message(MessageType::ERROR, format!("Interaction get failed: {}", e))
                .await;
            None
        }
    }
}

/// Convert an Interaction to an InteractionSummary
fn interaction_to_summary(interaction: Interaction) -> InteractionSummary {
    let author_str = match interaction.author {
        Author::Human => "human",
        Author::Agent => "agent",
        Author::System => "system",
        Author::External => "external",
    };

    // Create a preview of the prompt (first 100 chars)
    let prompt_preview = if interaction.user_prompt.len() > 100 {
        format!("{}...", &interaction.user_prompt[..97])
    } else {
        interaction.user_prompt.clone()
    };

    InteractionSummary {
        id: interaction.id.to_string(),
        prompt_preview,
        timestamp: interaction.timestamp.timestamp(),
        author: author_str.to_string(),
    }
}

/// Get commit message and timestamp from git
fn get_commit_info(sha: &str, root_path: Option<&std::path::Path>) -> (String, i64) {
    let output = if let Some(path) = root_path {
        Command::new("git")
            .args(["log", "-1", "--format=%s%n%ct", sha])
            .current_dir(path)
            .output()
    } else {
        Command::new("git")
            .args(["log", "-1", "--format=%s%n%ct", sha])
            .output()
    };

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = stdout.trim().lines().collect();
            if lines.len() >= 2 {
                let message = lines[0].to_string();
                let timestamp = lines[1].parse().unwrap_or(0);
                (message, timestamp)
            } else {
                (format!("Commit {}", &sha[..7.min(sha.len())]), 0)
            }
        }
        _ => (format!("Commit {}", &sha[..7.min(sha.len())]), 0),
    }
}
