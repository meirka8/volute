use crate::tools;
use anyhow::{Context, Result};
use cvc_core::db::CvcStore;
use cvc_core::models::InteractionId;
use cvc_core::repository::{RepositoryLayout, RepositoryLayoutError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

/// Shared MCP server state: the DB handle plus the current session's identity.
///
/// One MCP server process is spawned per client session by every mainstream host
/// (Claude Code, Cursor, Zed), so process lifetime is a sound default session
/// boundary — `conversation_id` and `last_interaction_id` live here rather than
/// being threaded through each tool call.
pub struct AppState {
    store: Arc<Mutex<CvcStore>>,
    /// Canonical identity of the repository selected when this process started.
    /// Do not retain `git2::Repository` here: it is deliberately not Send/Sync.
    repository: RepositoryBinding,
    pub conversation_id: Mutex<Option<String>>,
    pub last_interaction_id: Mutex<Option<InteractionId>>,
}

#[derive(Debug)]
pub(crate) struct RepositoryBinding {
    git_dir: std::path::PathBuf,
    common_git_dir: std::path::PathBuf,
    worktree_root: std::path::PathBuf,
    policy_root: std::path::PathBuf,
    cvc_dir: std::path::PathBuf,
    db_path: std::path::PathBuf,
    identities: BindingIdentities,
}

#[derive(Debug)]
struct BindingIdentities {
    worktree_root: FileIdentity,
    git_dir: FileIdentity,
    common_git_dir: FileIdentity,
    cvc_dir: FileIdentity,
    db_path: FileIdentity,
}

#[derive(Debug, PartialEq, Eq)]
struct FileIdentity {
    handle: same_file::Handle,
}

#[derive(Clone, Copy)]
enum ExpectedType {
    Directory,
    File,
}

fn file_identity(path: &std::path::Path, expected: ExpectedType) -> std::io::Result<FileIdentity> {
    fn validate(metadata: &std::fs::Metadata, expected: ExpectedType) -> std::io::Result<()> {
        let valid = match expected {
            ExpectedType::Directory => metadata.is_dir(),
            ExpectedType::File => metadata.is_file(),
        };
        if metadata.file_type().is_symlink() || !valid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unexpected filesystem object",
            ));
        }
        Ok(())
    }
    validate(&std::fs::symlink_metadata(path)?, expected)?;
    let handle = same_file::Handle::from_path(path)?;
    validate(&std::fs::symlink_metadata(path)?, expected)?;
    Ok(FileIdentity { handle })
}

impl RepositoryBinding {
    pub(crate) fn from_layout(layout: &RepositoryLayout) -> Result<Self, RepositoryLayoutError> {
        let worktree_root = layout.worktree_root()?.to_owned();
        let git_dir = layout.git_dir().to_owned();
        let common_git_dir = layout.common_git_dir().to_owned();
        let cvc_dir = layout.cvc_dir();
        let db_path = layout.db_path();
        let identities = BindingIdentities {
            worktree_root: file_identity(&worktree_root, ExpectedType::Directory)
                .map_err(|e| RepositoryLayoutError::Metadata(e.to_string()))?,
            git_dir: file_identity(&git_dir, ExpectedType::Directory)
                .map_err(|e| RepositoryLayoutError::Metadata(e.to_string()))?,
            common_git_dir: file_identity(&common_git_dir, ExpectedType::Directory)
                .map_err(|e| RepositoryLayoutError::Metadata(e.to_string()))?,
            cvc_dir: file_identity(&cvc_dir, ExpectedType::Directory)
                .map_err(|e| RepositoryLayoutError::Metadata(e.to_string()))?,
            db_path: file_identity(&db_path, ExpectedType::File)
                .map_err(|e| RepositoryLayoutError::Metadata(e.to_string()))?,
        };
        Ok(Self {
            git_dir,
            common_git_dir,
            policy_root: layout.policy_root()?.to_owned(),
            cvc_dir,
            db_path,
            worktree_root,
            identities,
        })
    }

    pub(crate) fn policy_root(&self) -> &std::path::Path {
        &self.policy_root
    }

    /// Rediscover rather than retaining git2 state across async boundaries. Every
    /// operation is constrained to both the common Git directory and active
    /// worktree selected at startup; a same-repository sibling worktree is not a
    /// substitute because its policy and working context can differ.
    pub(crate) fn rediscover(
        &self,
        path: &std::path::Path,
    ) -> Result<RepositoryLayout, JsonRpcError> {
        let layout = RepositoryLayout::discover(path).map_err(|_| JsonRpcError {
            code: -32602,
            message: "Invalid repository target".into(),
            data: None,
        })?;
        let same_git_dir = layout.git_dir() == self.git_dir;
        let same_common = layout.common_git_dir() == self.common_git_dir;
        let same_worktree = layout.worktree_root().ok() == Some(self.worktree_root.as_path());
        let same_db_path = layout.db_path() == self.db_path;
        let identities_match =
            same_path_identity(
                &self.worktree_root,
                &self.identities.worktree_root,
                ExpectedType::Directory,
            ) && same_path_identity(
                &self.git_dir,
                &self.identities.git_dir,
                ExpectedType::Directory,
            ) && same_path_identity(
                &self.common_git_dir,
                &self.identities.common_git_dir,
                ExpectedType::Directory,
            ) && same_path_identity(
                &self.cvc_dir,
                &self.identities.cvc_dir,
                ExpectedType::Directory,
            ) && same_path_identity(&self.db_path, &self.identities.db_path, ExpectedType::File);
        if !same_git_dir || !same_common || !same_worktree || !same_db_path || !identities_match {
            return Err(JsonRpcError {
                code: -32602,
                message: "Repository mismatch".into(),
                data: None,
            });
        }
        Ok(layout)
    }
}

fn same_path_identity(path: &std::path::Path, expected: &FileIdentity, kind: ExpectedType) -> bool {
    matches!(file_identity(path, kind), Ok(actual) if actual == *expected)
}

impl AppState {
    /// Opens the sole store permitted for this state from the validated layout.
    pub fn open(layout: &RepositoryLayout) -> anyhow::Result<Self> {
        // This must precede even calculating CVC storage paths: a bare common
        // Git directory is not a worktree and startup must leave it untouched.
        layout.worktree_root()?;
        let db_path = layout.db_path();
        let cvc_dir = layout.cvc_dir();
        let before = storage_snapshot(&cvc_dir, &db_path)?;
        if before.cvc.is_none() {
            std::fs::create_dir(&cvc_dir)?;
        }
        let after_dir = storage_snapshot(&cvc_dir, &db_path)?;
        if before.cvc.is_some() && before.cvc != after_dir.cvc {
            anyhow::bail!("CVC storage changed during startup");
        }
        // Reserve a fresh database atomically.  This distinguishes a database
        // absent at validation from one replaced before SQLite opens it.
        if before.db.is_none() {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&db_path)?;
        }
        let before_open = storage_snapshot(&cvc_dir, &db_path)?;
        if after_dir.cvc != before_open.cvc
            || (before.db.is_some() && before.db != before_open.db)
            || before_open.db.is_none()
        {
            anyhow::bail!("CVC storage changed during startup");
        }
        let store = CvcStore::open_initialized(&db_path)?;
        store.init()?;
        let after_open = storage_snapshot(&cvc_dir, &db_path)?;
        if before_open.cvc != after_open.cvc || before_open.db != after_open.db {
            anyhow::bail!("CVC storage changed during startup");
        }
        let repository = RepositoryBinding::from_layout(layout)?;
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            repository,
            conversation_id: Mutex::new(None),
            last_interaction_id: Mutex::new(None),
        })
    }

    pub(crate) fn repository(&self) -> &RepositoryBinding {
        &self.repository
    }

    pub(crate) fn revalidate(&self) -> Result<RepositoryLayout, JsonRpcError> {
        self.repository.rediscover(&self.repository.policy_root)
    }

    pub(crate) fn with_store<T>(
        &self,
        operation: impl FnOnce(&CvcStore) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let store = self
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to lock store"))?;
        operation(&store)
    }
}

#[derive(PartialEq)]
struct StorageSnapshot {
    cvc: Option<FileIdentity>,
    db: Option<FileIdentity>,
}

fn storage_snapshot(
    cvc_dir: &std::path::Path,
    db_path: &std::path::Path,
) -> anyhow::Result<StorageSnapshot> {
    fn optional(
        path: &std::path::Path,
        kind: ExpectedType,
    ) -> anyhow::Result<Option<FileIdentity>> {
        match file_identity(path, kind) {
            Ok(identity) => Ok(Some(identity)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => anyhow::bail!("invalid CVC storage object"),
        }
    }
    let cvc = optional(cvc_dir, ExpectedType::Directory)?;
    let db = optional(db_path, ExpectedType::File)?;
    if cvc.is_none() && db.is_some() {
        anyhow::bail!("invalid CVC storage object");
    }
    Ok(StorageSnapshot { cvc, db })
}

/// Starts a new logical session. The aggregate capture boundary creates its
/// conversation only when the first scrubbed thought is persisted.
pub fn start_session(state: &AppState, client_name: &str) -> anyhow::Result<String> {
    let id = Uuid::new_v4().to_string();
    let _ = client_name;

    *state
        .conversation_id
        .lock()
        .map_err(|_| anyhow::anyhow!("Failed to lock session state"))? = Some(id.clone());
    *state
        .last_interaction_id
        .lock()
        .map_err(|_| anyhow::anyhow!("Failed to lock session state"))? = None;

    Ok(id)
}

#[derive(Serialize, Deserialize, Debug)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    params: Option<Value>,
    id: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
    id: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

pub async fn run() -> Result<()> {
    let cwd = std::env::current_dir().context("Failed to get server working directory")?;
    let layout = RepositoryLayout::discover(&cwd).context("Failed to discover Git repository")?;
    let state = Arc::new(AppState::open(&layout).context("Failed to open CVC database")?);

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut writer = stdout;

    let mut line = String::new();
    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            break;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Failed to parse JSON-RPC request: {}", e);
                continue;
            }
        };

        let is_notification = request.id.is_none();
        let response = handle_request(request, state.clone()).await;

        if !is_notification {
            let response_json = serde_json::to_string(&response)?;
            writer.write_all(response_json.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
    }

    Ok(())
}

async fn handle_request(req: JsonRpcRequest, state: Arc<AppState>) -> JsonRpcResponse {
    let result = match req.method.as_str() {
        "initialize" => {
            let client_name = req
                .params
                .as_ref()
                .and_then(|p| p.get("clientInfo"))
                .and_then(|ci| ci.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("unknown-client")
                .to_string();

            let state_for_session = state.clone();
            let session_res = tokio::task::spawn_blocking(move || {
                start_session(&state_for_session, &client_name)
            })
            .await;

            match session_res {
                Ok(Ok(_)) => Ok(json!({
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {
                        "name": "cvc-mcp",
                        "version": "0.1.0"
                    },
                    "capabilities": {
                        "tools": {
                            "listChanged": false
                        }
                    }
                })),
                Ok(Err(e)) => Err(JsonRpcError {
                    code: -32603,
                    message: format!("Failed to start session: {}", e),
                    data: None,
                }),
                Err(e) => Err(JsonRpcError {
                    code: -32603,
                    message: format!("Internal Error: {}", e),
                    data: None,
                }),
            }
        }
        "notifications/initialized" => Ok(json!({})),
        "tools/list" => Ok(tools::list_tools()),
        "tools/call" => {
            if let Some(params) = req.params {
                tools::call_tool(params, state).await
            } else {
                Err(JsonRpcError {
                    code: -32602,
                    message: "Missing params argument".to_string(),
                    data: None,
                })
            }
        }
        _ => Err(JsonRpcError {
            code: -32601,
            message: "Method not found".to_string(),
            data: None,
        }),
    };

    match result {
        Ok(res) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(res),
            error: None,
            id: req.id,
        },
        Err(e) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(e),
            id: req.id,
        },
    }
}
