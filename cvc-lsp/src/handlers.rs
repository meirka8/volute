use crate::protocol::*;
use crate::state::AppState;
use chrono::Utc;
use cvc_core::models::{Author, Conversation, Interaction, InteractionId};
use cvc_core::vscode::ChatRequest;
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

    let model_response = if let Some(raw) = &params.raw_response {
        Some(ChatRequest::reconstruct_from_parts(raw))
    } else {
        params.response.clone()
    };

    let interaction = Interaction {
        id: InteractionId::new(),
        conversation_id: "default-session".to_string(),
        parent_id: None,
        timestamp: chrono::Utc::now(),
        author: Author::Human,
        user_prompt: prompt,
        model_name: params.model,
        model_cot: params.chain_of_thought,
        model_response: model_response,
        source_request_id: None,
    };

    // Offload DB write to background thread
    let result = task::spawn_blocking(move || {
        // Safer lock handling
        let store_guard = state_clone.store.lock().expect("CVC Store mutex poisoned");

        if let Some(store) = store_guard.as_ref() {
            // Ensure conversation exists before creating interaction (FK constraint)
            let conv_id = &interaction.conversation_id;
            if store.get_conversation(conv_id)?.is_none() {
                store.create_conversation(&Conversation {
                    id: conv_id.clone(),
                    title: "Copilot Chat Session".to_string(),
                    created_at: Utc::now(),
                })?;
            }

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

/// Handle a batch of segmented interactions from the Chat Session Watcher.
///
/// This replaces the turn/start + turn/end flow for the watcher use-case.
/// It processes a complete VS Code chat request as multiple interaction segments
/// (human turn + agent reasoning turns), linked via parent_id.
pub async fn handle_turn_batch(client: &Client, state: Arc<AppState>, params: TurnBatchParams) {
    let segment_count = params.interactions.len();
    client
        .log_message(
            MessageType::INFO,
            format!(
                "Turn batch (source: {}, session: {}, segments: {})",
                params.source_request_id, params.session_id, segment_count
            ),
        )
        .await;

    let state_clone = state.clone();
    let client_clone = client.clone();
    let source_id_for_log = params.source_request_id.clone();

    let result = task::spawn_blocking(move || {
        let store_guard = state_clone.store.lock().expect("CVC Store mutex poisoned");

        if let Some(store) = store_guard.as_ref() {
            // Ensure conversation exists
            if store.get_conversation(&params.session_id)?.is_none() {
                store.create_conversation(&Conversation {
                    id: params.session_id.clone(),
                    title: "Copilot Chat Session".to_string(),
                    created_at: Utc::now(),
                })?;
            }

            // Delete previous interactions from this source request (deduplication)
            store.delete_interactions_by_source_request_id(&params.source_request_id)?;

            // Insert new segmented interactions with parent chaining
            let mut previous_id: Option<InteractionId> = None;

            for segment in &params.interactions {
                let id = InteractionId::new();

                let author = match segment.author {
                    Author::Human => Author::Human,
                    Author::Agent => Author::Agent,
                    Author::System => Author::System,
                    Author::External => Author::External,
                };

                let interaction = Interaction {
                    id: id.clone(),
                    conversation_id: params.session_id.clone(),
                    parent_id: previous_id.clone(),
                    timestamp: Utc::now(),
                    author,
                    user_prompt: segment.user_prompt.clone().unwrap_or_default(),
                    model_name: params.model.clone(),
                    model_cot: segment.chain_of_thought.clone(),
                    model_response: segment.response.clone(),
                    source_request_id: Some(params.source_request_id.clone()),
                };

                store.create_interaction(&interaction)?;
                previous_id = Some(id);
            }

            Ok(())
        } else {
            Err(cvc_core::db::DbError::Migration("DB not open".to_string()))
        }
    })
    .await;

    match result {
        Ok(Ok(_)) => {
            client_clone
                .log_message(
                    MessageType::INFO,
                    format!(
                        "Batch saved: {} segments for source {}",
                        segment_count, source_id_for_log
                    ),
                )
                .await;
        }
        Ok(Err(e)) => {
            client_clone
                .log_message(MessageType::ERROR, format!("Failed to save batch: {}", e))
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

/// Filters a list of commits, keeping only those that are ancestors of the given HEAD SHA.
/// If `head_sha` is None, no filtering is performed.
fn filter_commits_by_reachability(
    repo_path_opt: Option<&std::path::Path>,
    head_sha_opt: Option<&str>,
    commits_data: &mut Vec<(cvc_core::models::CommitSha, Vec<Interaction>)>,
) {
    if let Some(sha_str) = head_sha_opt {
        if let Some(path) = repo_path_opt {
            if let Ok(repo) = git2::Repository::open(path) {
                if let Ok(head_oid) = git2::Oid::from_str(sha_str) {
                    let mut reachable_commits = std::collections::HashSet::new();
                    match repo.revwalk() {
                        Ok(mut revwalk) => {
                            if let Err(e) = revwalk.push(head_oid) {
                                log::warn!("Failed to push HEAD to revwalk: {}", e);
                            } else {
                                for oid_result in revwalk {
                                    match oid_result {
                                        Ok(oid) => {
                                            reachable_commits.insert(oid);
                                        }
                                        Err(e) => {
                                            log::debug!("Error during Revwalk iteration: {}", e);
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to initialize Revwalk: {}", e);
                        }
                    }

                    commits_data.retain(|(commit_sha, _)| {
                        if let Ok(commit_oid) = git2::Oid::from_str(commit_sha.as_str()) {
                            reachable_commits.contains(&commit_oid)
                        } else {
                            log::warn!("Invalid commit SHA in database: {}", commit_sha.as_str());
                            false
                        }
                    });
                } else {
                    log::warn!("Invalid HEAD SHA provided by client: {}", sha_str);
                    // If HEAD SHA is invalid, we might want to clear or leave as is.
                    // The user requested to handle errors gracefully. If HEAD is invalid, we don't filter.
                }
            } else {
                log::error!("Failed to open Git repository at path: {:?}", path);
            }
        }
    }
}

/// Handle timeline/get request - returns pending thoughts and commits with linked interactions
pub async fn handle_timeline_get(
    client: &Client,
    state: Arc<AppState>,
    params: TimelineGetParams,
) -> TimelineGetResponse {
    let max_items = params.max_items.unwrap_or(50) as usize;
    let include_unbound = params.include_unbound.unwrap_or(true);
    let head_sha = params.head_sha.clone();

    client
        .log_message(
            MessageType::INFO,
            format!(
                "Timeline get: max_items={}, include_unbound={}, head_sha={:?}",
                max_items, include_unbound, head_sha
            ),
        )
        .await;

    let state_clone = state.clone();
    let root_path = state.root_path.lock().unwrap().clone();

    // Run DB and Git operations in blocking thread
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
            let mut commits_data = store.get_commits_with_interactions().unwrap_or_default();

            // Filter commits by reachability from HEAD if provided
            filter_commits_by_reachability(
                root_path.as_deref(),
                head_sha.as_deref(),
                &mut commits_data,
            );

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

    // Check which content types have meaningful content
    let has_prompt = has_meaningful_content(&interaction.user_prompt);
    let has_cot = interaction
        .model_cot
        .as_ref()
        .map(|s| has_meaningful_content(s))
        .unwrap_or(false);
    let has_response = interaction
        .model_response
        .as_ref()
        .map(|s| has_meaningful_content(s))
        .unwrap_or(false);

    InteractionSummary {
        id: interaction.id.to_string(),
        prompt_preview,
        timestamp: interaction.timestamp.timestamp(),
        author: author_str.to_string(),
        has_prompt,
        has_cot,
        has_response,
    }
}

/// Check if text has meaningful content (not just whitespace, quotes, etc.)
fn has_meaningful_content(text: &str) -> bool {
    text.chars()
        .any(|c| !c.is_whitespace() && c != '"' && c != '\'' && c != '`')
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

#[cfg(test)]
mod tests {
    use super::*;
    use ::tempfile::TempDir;
    use chrono::Utc;
    use cvc_core::models::{Author, CommitSha, InteractionId};
    use git2::{Repository, Signature};

    fn create_test_repo() -> (TempDir, String, String, String) {
        let temp_dir = TempDir::new().unwrap();
        let repo = Repository::init(temp_dir.path()).unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();

        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        // 1. Initial commit (Root)
        let root_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "Initial", &tree, &[])
            .unwrap();
        let root_commit = repo.find_commit(root_oid).unwrap();

        // 2. Commit on main
        let main_oid = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "Main commit",
                &tree,
                &[&root_commit],
            )
            .unwrap();

        // 3. Create branch feature
        let _branch = repo.branch("feature", &root_commit, false).unwrap();

        // 4. Commit on feature
        repo.set_head("refs/heads/feature").unwrap();
        let feature_oid = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "Feature commit",
                &tree,
                &[&root_commit],
            )
            .unwrap();

        (
            temp_dir,
            root_oid.to_string(),
            main_oid.to_string(),
            feature_oid.to_string(),
        )
    }

    fn dummy_interaction() -> Interaction {
        Interaction {
            id: InteractionId::new(),
            conversation_id: "test".to_string(),
            parent_id: None,
            timestamp: Utc::now(),
            author: Author::Human,
            user_prompt: "test".to_string(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        }
    }

    #[test]
    fn test_filter_commits_by_reachability() {
        let (temp_dir, root_sha, main_sha, feature_sha) = create_test_repo();

        let commits_data = vec![
            (CommitSha::new(root_sha.clone()), vec![dummy_interaction()]),
            (CommitSha::new(main_sha.clone()), vec![dummy_interaction()]),
            (
                CommitSha::new(feature_sha.clone()),
                vec![dummy_interaction()],
            ),
        ];

        // 1. Test from main branch (should see main and root, but not feature)
        let mut data_main = commits_data.clone();
        filter_commits_by_reachability(Some(temp_dir.path()), Some(&main_sha), &mut data_main);
        assert_eq!(data_main.len(), 2);
        let shas: Vec<_> = data_main.iter().map(|(s, _)| s.as_str()).collect();
        assert!(shas.contains(&root_sha.as_str()));
        assert!(shas.contains(&main_sha.as_str()));

        // 2. Test from feature branch (should see feature and root, but not main)
        let mut data_feature = commits_data.clone();
        filter_commits_by_reachability(
            Some(temp_dir.path()),
            Some(&feature_sha),
            &mut data_feature,
        );
        assert_eq!(data_feature.len(), 2);
        let shas_feature: Vec<_> = data_feature.iter().map(|(s, _)| s.as_str()).collect();
        assert!(shas_feature.contains(&root_sha.as_str()));
        assert!(shas_feature.contains(&feature_sha.as_str()));

        // 3. Test with root HEAD (should only see root)
        let mut data_root = commits_data.clone();
        filter_commits_by_reachability(Some(temp_dir.path()), Some(&root_sha), &mut data_root);
        assert_eq!(data_root.len(), 1);
        assert_eq!(data_root[0].0.as_str(), root_sha);

        // 4. Test with no HEAD (should see all)
        let mut data_none = commits_data.clone();
        filter_commits_by_reachability(Some(temp_dir.path()), None, &mut data_none);
        assert_eq!(data_none.len(), 3);

        // 5. Test error cases (invalid SHA) - graceful handling, should not filter
        let mut data_invalid = commits_data.clone();
        filter_commits_by_reachability(
            Some(temp_dir.path()),
            Some("invalid_sha"),
            &mut data_invalid,
        );
        assert_eq!(data_invalid.len(), 3);
    }
}
