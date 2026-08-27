use crate::state::{AppState, BoundRepository};
use cvc_core::db::CvcStore;
use cvc_core::repository::RepositoryLayout;
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
        let root = initialize_root(&params)?;
        let layout = RepositoryLayout::discover(root)
            .and_then(|layout| {
                layout.worktree_root()?;
                Ok(layout)
            })
            .map_err(|_| {
                tower_lsp::jsonrpc::Error::invalid_params(
                    "CVC requires a valid non-bare Git worktree",
                )
            })?;

        let hooks_failed = {
            let mut binding = self
                .state
                .binding
                .lock()
                .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
            if let Some(existing) = binding.as_ref() {
                if existing.layout.common_git_dir() != layout.common_git_dir()
                    || existing.layout.worktree_root().ok() != layout.worktree_root().ok()
                {
                    // Deliberately one repository and one worktree per LSP session.
                    return Err(tower_lsp::jsonrpc::Error::invalid_request());
                }
                return Ok(initialize_result());
            }

            // RepositoryLayout supplies validated common storage. In a linked
            // worktree `.git` is a gitfile and must never be used as a DB parent.
            let store = CvcStore::open_initialized(layout.db_path())
                .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
            let hooks_failed = cvc_core::hooks::install_layout(&layout).is_err();
            *binding = Some(BoundRepository { layout, store });
            hooks_failed
        };
        if hooks_failed {
            self.client
                .log_message(MessageType::WARNING, "CVC hooks could not be installed")
                .await;
        }
        Ok(initialize_result())
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "CVC LSP initialized!")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

fn initialize_result() -> InitializeResult {
    InitializeResult {
        capabilities: ServerCapabilities {
            text_document_sync: None,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Prefer the LSP root URI, then the first workspace folder. The process cwd is
/// only a compatibility fallback for clients that send neither root form.
fn initialize_root(params: &InitializeParams) -> Result<std::path::PathBuf> {
    if params
        .workspace_folders
        .as_ref()
        .is_some_and(|folders| folders.len() > 1)
    {
        return Err(tower_lsp::jsonrpc::Error::invalid_params(
            "CVC LSP does not support multi-root workspaces",
        ));
    }
    let uri = params.root_uri.as_ref().or_else(|| {
        params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first().map(|folder| &folder.uri))
    });
    match uri {
        Some(uri) => uri.to_file_path().map_err(|_| {
            tower_lsp::jsonrpc::Error::invalid_params("CVC initialize root must be a file URI")
        }),
        None => std::env::current_dir().map_err(|_| tower_lsp::jsonrpc::Error::internal_error()),
    }
}
