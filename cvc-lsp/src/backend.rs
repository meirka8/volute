use crate::state::AppState;
use cvc_core::db::CvcStore;
use std::path::PathBuf;
use std::sync::Arc;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

pub struct Backend {
    pub client: Client,
    pub state: Arc<AppState>,
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let root_uri = params.root_uri;
        let root_path = root_uri
            .and_then(|uri| uri.to_file_path().ok())
            .unwrap_or_else(|| PathBuf::from("."));

        let db_path = root_path.join(".git/cvc/index.db");
        self.client
            .log_message(
                MessageType::INFO,
                format!("Initializing CVC LSP at {:?}", db_path),
            )
            .await;

        // Try to open the DB
        let store = CvcStore::open_initialized(&db_path);
        match store {
            Ok(s) => {
                // Ensure schema is init
                if let Err(e) = s.init() {
                    self.client
                        .log_message(
                            MessageType::ERROR,
                            format!("Failed to init DB schema: {}", e),
                        )
                        .await;
                }
                *self.state.store.lock().unwrap() = Some(s);

                // Install hooks
                // We do this here because it's a good time to ensure the environment is set up
                // and we know we have a valid repo/DB context.
                match cvc_core::hooks::install(&root_path) {
                    Ok(_) => {
                        self.client
                            .log_message(MessageType::INFO, "CVC hooks installed/verified")
                            .await;
                    }
                    Err(e) => {
                        self.client
                            .log_message(
                                MessageType::WARNING,
                                format!("Failed to install CVC hooks: {}", e),
                            )
                            .await;
                    }
                }
            }
            Err(e) => {
                self.client
                    .log_message(MessageType::ERROR, format!("Failed to open DB: {}", e))
                    .await;
            }
        }

        *self.state.root_path.lock().unwrap() = Some(root_path);

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                // We define custom notifications, but standard capabilities are minimal for now
                text_document_sync: None,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "CVC LSP initialized!")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    // Custom Notification Handlers
    // Note: tower-lsp doesn't auto-route custom notifications in the trait easily without jsonrpc implementation details
    // usually we have to register them or handle them in the router.
    // BUT tower-lsp 0.20 supports `custom_method` if we route it manually, or we can use the `method` macro if we were defining a trait.
    // Actually, handling custom notifications is often done by implementing the logic and calling it from `main.rs` loop or using `jsonrpc` delegation.
    // Wait, tower-lsp `service` definition in `main.rs` takes the backend.

    // To handle custom notifications (which are not part of the standard LanguageServer trait),
    // we normally have to intercept them or use specific mechanisms.
    // For simplicity with standard tower-lsp usage:
    // We can just rely on standard methods for now if we can map them, BUT the requirement is custom methods.
    // Tower LSP allows handling unknown requests/notifications??
    // Actually, looking at tower-lsp docs/examples, one usually just defines the methods in the impl block and uses `LspService::build(backend).custom_method("...", ...).finish()`.
}
