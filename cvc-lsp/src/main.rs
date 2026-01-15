mod backend;
mod handlers;
mod protocol;
mod state;

use backend::Backend;
use protocol::*;
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
        "$/cvc/session/start",
        |backend: &Backend, params: SessionStartParams| {
            let client = backend.client.clone();
            let state = backend.state.clone();
            async move {
                handlers::handle_session_start(&client, state, params).await;
                Ok::<serde_json::Value, tower_lsp::jsonrpc::Error>(serde_json::Value::Null)
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
                Ok::<serde_json::Value, tower_lsp::jsonrpc::Error>(serde_json::Value::Null)
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
                Ok::<serde_json::Value, tower_lsp::jsonrpc::Error>(serde_json::Value::Null)
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
                Ok::<serde_json::Value, tower_lsp::jsonrpc::Error>(serde_json::Value::Null)
            }
        },
    )
    .finish();

    Server::new(stdin, stdout, socket).serve(service).await;
}
