use crate::protocol::*;
use crate::state::AppState;
use cvc_core::models::{Author, Interaction, InteractionId};
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
            format!("Turn started. Prompt: {}", params.prompt),
        )
        .await;

    // Store the prompt temporarily in state
    if let Ok(mut pending) = state.pending_prompt.lock() {
        *pending = Some(params.prompt.clone());
    }
}

pub async fn handle_turn_end(client: &Client, state: Arc<AppState>, params: TurnEndParams) {
    // Retrieve pending prompt
    let prompt = {
        let mut pending = state.pending_prompt.lock().unwrap();
        pending
            .take()
            .unwrap_or_else(|| "Unknown prompt".to_string())
    };

    let state_clone = state.clone();
    let client_clone = client.clone(); // Client is cheap to clone (Arc internal) probably?
                                       // Wait, tower_lsp::Client is Clone? Yes.

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
        let store_guard = state_clone.store.lock().unwrap();
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
