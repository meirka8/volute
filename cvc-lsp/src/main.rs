mod backend;
mod handlers;
mod protocol;
mod state;

use backend::Backend;
use protocol::{
    InteractionDetail, InteractionGetParams, LinkCommitParams, PrivacyStatusResponse,
    SessionStartParams, TimelineGetParams, TimelineGetResponse, TurnBatchParams, TurnEndParams,
    TurnStartParams,
};
use state::AppState;
use std::sync::Arc;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    env_logger::init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let state = Arc::new(AppState::new());
    let state_backend = state.clone();

    let (service, socket) = LspService::build(|client| Backend {
        client,
        state: state_backend,
    })
    .custom_method(
        "cvc/privacy/status",
        |backend: &Backend, _: serde_json::Value| {
            let state = backend.state.clone();
            async move {
                Ok::<PrivacyStatusResponse, tower_lsp::jsonrpc::Error>(
                    handlers::handle_privacy_status(state).await,
                )
            }
        },
    )
    .custom_method(
        "$/cvc/session/start",
        |backend: &Backend, params: SessionStartParams| {
            let client = backend.client.clone();
            let state = backend.state.clone();
            async move {
                handlers::handle_session_start(&client, state, params).await;
                // Return () to indicate this is a notification, not a request
            }
        },
    )
    .custom_method(
        "$/cvc/turn/start",
        |backend: &Backend, params: TurnStartParams| {
            let client = backend.client.clone();
            let state = backend.state.clone();
            async move {
                handlers::handle_turn_start(&client, state, params).await;
                // Return () to indicate this is a notification, not a request
            }
        },
    )
    .custom_method(
        "$/cvc/turn/end",
        |backend: &Backend, params: TurnEndParams| {
            let client = backend.client.clone();
            let state = backend.state.clone();
            async move {
                handlers::handle_turn_end(&client, state, params).await;
                // Return () to indicate this is a notification, not a request
            }
        },
    )
    .custom_method(
        "$/cvc/turn/batch",
        |backend: &Backend, params: TurnBatchParams| {
            let client = backend.client.clone();
            let state = backend.state.clone();
            async move {
                handlers::handle_turn_batch(&client, state, params).await;
            }
        },
    )
    .custom_method(
        "$/cvc/link/commit",
        |backend: &Backend, params: LinkCommitParams| {
            let client = backend.client.clone();
            let state = backend.state.clone();
            async move {
                handlers::handle_link_commit(&client, state, params).await;
                // Return () to indicate this is a notification, not a request
            }
        },
    )
    .custom_method(
        "cvc/timeline/get",
        |backend: &Backend, params: TimelineGetParams| {
            let client = backend.client.clone();
            let state = backend.state.clone();
            async move {
                let response = handlers::handle_timeline_get(&client, state, params).await;
                Ok::<TimelineGetResponse, tower_lsp::jsonrpc::Error>(response)
            }
        },
    )
    .custom_method(
        "cvc/interaction/get",
        |backend: &Backend, params: InteractionGetParams| {
            let client = backend.client.clone();
            let state = backend.state.clone();
            async move {
                let response = handlers::handle_interaction_get(&client, state, params).await;
                Ok::<Option<InteractionDetail>, tower_lsp::jsonrpc::Error>(response)
            }
        },
    )
    .finish();

    Server::new(stdin, stdout, socket).serve(service).await;
}
