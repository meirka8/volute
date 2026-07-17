use crate::tools;
use anyhow::{Context, Result};
use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::{Conversation, InteractionId};
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
    pub store: Arc<Mutex<CvcStore>>,
    pub conversation_id: Mutex<Option<String>>,
    pub last_interaction_id: Mutex<Option<InteractionId>>,
}

impl AppState {
    pub fn new(store: Arc<Mutex<CvcStore>>) -> Self {
        Self {
            store,
            conversation_id: Mutex::new(None),
            last_interaction_id: Mutex::new(None),
        }
    }
}

/// Starts a new session on `state`: creates a fresh conversation titled after the
/// connecting client and resets parent chaining. Called once per MCP `initialize`
/// handshake. Returns the new conversation ID.
pub fn start_session(state: &AppState, client_name: &str) -> anyhow::Result<String> {
    let id = Uuid::new_v4().to_string();
    let title = format!("{} session {}", client_name, Utc::now().to_rfc3339());

    {
        let store = state
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to lock store"))?;
        store.create_conversation(&Conversation {
            id: id.clone(),
            title,
            created_at: Utc::now(),
        })?;
    }

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
    // Initialize DB connection
    // We assume the process is running in the repo root.
    // The DB is at .git/cvc/index.db
    let db_path = std::path::Path::new(".git/cvc/index.db");

    // Ensure directory exists - CvcStore::open expects the file path, but parent dir must exist?
    // Actually CvcStore::open just calls Connection::open.
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create cvc directory")?;
    }

    let store = CvcStore::open_initialized(db_path).context("Failed to open CVC database")?;
    store.init().context("Failed to run migrations")?;
    let state = Arc::new(AppState::new(Arc::new(Mutex::new(store))));

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
