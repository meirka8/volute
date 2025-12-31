use crate::tools;
use anyhow::{Context, Result};
use cvc_core::db::CvcStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

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
    result: Option<Value>,
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

    let store = CvcStore::open(db_path).context("Failed to open CVC database")?;
    store.init().context("Failed to run migrations")?;
    let store = Arc::new(Mutex::new(store));

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

        let response = handle_request(request, store.clone()).await;
        let response_json = serde_json::to_string(&response)?;
        writer.write_all(response_json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }

    Ok(())
}

async fn handle_request(req: JsonRpcRequest, store: Arc<Mutex<CvcStore>>) -> JsonRpcResponse {
    let result = match req.method.as_str() {
        "initialize" => Ok(json!({
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
        "notifications/initialized" => Ok(json!({})),
        "tools/list" => Ok(tools::list_tools()),
        "tools/call" => tools::call_tool(req.params.unwrap_or(Value::Null), store).await,
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
