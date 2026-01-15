use crate::server::JsonRpcError;
use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::{Author, Interaction, InteractionId};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub fn list_tools() -> Value {
    json!({
        "tools": [
            {
                "name": "commit_thought",
                "description": "Log a reasoning step, task, and response into CVC. Call this after completing a reasoning step or task.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "The task or prompt you were asked to complete (what the user requested)"
                        },
                        "reasoning": {
                            "type": "string",
                            "description": "Your internal reasoning, chain of thought, or plan for how to approach the task"
                        },
                        "response": {
                            "type": "string",
                            "description": "Your response or the action you took to complete the task"
                        },
                        "context_summary": {
                            "type": "string",
                            "description": "Optional summary of relevant file context or state"
                        }
                    },
                    "required": ["task", "reasoning"]
                }
            },
            {
                "name": "read_history",
                "description": "Read recent interaction history.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer" }
                    }
                }
            },
            {
                "name": "get_context",
                "description": "Get the file context (clean or dirty state).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string" }
                    },
                    "required": ["file_path"]
                }
            },
            {
                "name": "setup_cvc",
                "description": "Initialize CVC in the current directory if missing.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            }
        ]
    })
}

pub async fn call_tool(params: Value, store: Arc<Mutex<CvcStore>>) -> Result<Value, JsonRpcError> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or(JsonRpcError {
            code: -32602,
            message: "Missing 'name' parameter".to_string(),
            data: None,
        })?;

    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    match name {
        "commit_thought" => commit_thought(args, store).await,
        "read_history" => read_history(args, store).await,
        "get_context" => get_context(args).await,
        "setup_cvc" => setup_cvc(args).await,
        _ => Err(JsonRpcError {
            code: -32601,
            message: format!("Unknown tool: {}", name),
            data: None,
        }),
    }
}

async fn commit_thought(args: Value, store: Arc<Mutex<CvcStore>>) -> Result<Value, JsonRpcError> {
    // New schema fields
    let task = args
        .get("task")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let reasoning = args
        .get("reasoning")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let response = args
        .get("response")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let context_summary = args
        .get("context_summary")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    // Build the chain of thought, optionally including context summary
    let full_cot = if !context_summary.is_empty() {
        format!("{}\n\nContext Summary:\n{}", reasoning, context_summary)
    } else {
        reasoning
    };

    // Create a new Conversation ID for this session if we don't have one?
    // The spec says "Conversation maps 1:1 with a VS Code Chat Session ID".
    // For MCP, we might not have a session ID provided by the client easily.
    // For MVP, lets just use a fixed UUID or generate one per run?
    // Better: Generate a specialized "Agent Session" conversation on first call?
    // But we are stateless between tool calls unless we store state in `main.rs`.
    // Let's create a new conversation for EACH tool call? No that's bad.
    // Let's create one conversation ID for the LIFETIME of the MCP server process.
    // But we are inside `call_tool`.
    // Let's just create a random one for now is probably wrong.
    // Ideally we should receive conversation_id in args.
    // But the tool definition in Design.md didn't specify it.
    // Let's just hardcode a placeholder or generate a fresh one for each thought for now (creates many 1-item conversations),
    // OR: Assume a "Global Agent Conversation" with a known ID?
    // Let's use a "Default Agent Conversation".

    let conversation_id = "agent-session-default".to_string(); // TODO: Manage sessions properly

    // Ensure conversation exists
    let store_clone = store.clone();
    let conv_id_clone = conversation_id.clone();

    // We need to run blocking DB ops in spawn_blocking usually, but `rusqlite` is blocking.
    // Since we are in async fn, we should use spawn_blocking.

    let res = tokio::task::spawn_blocking(move || {
        let store = store_clone
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to lock store"))?;

        // Upsert conversation (ignore if exists)
        // CvcStore check?
        if store.get_conversation(&conv_id_clone)?.is_none() {
            store.create_conversation(&cvc_core::models::Conversation {
                id: conv_id_clone.clone(),
                title: "Agent Session".to_string(),
                created_at: Utc::now(),
            })?;
        }

        let interaction = Interaction {
            id: InteractionId::new(),
            conversation_id: conv_id_clone,
            parent_id: None, // We don't track parent in this simple tool yet (need history)
            timestamp: Utc::now(),
            author: Author::Agent,
            user_prompt: task, // The task/prompt the agent was asked to complete
            model_name: Some("agent".to_string()),
            model_cot: Some(full_cot), // The agent's reasoning/chain of thought
            model_response: response,  // The agent's response/action taken
        };

        store.create_interaction(&interaction)?;
        Ok::<_, anyhow::Error>(interaction.id)
    })
    .await
    .map_err(|e| JsonRpcError {
        code: -32603,
        message: format!("Internal Error: {}", e),
        data: None,
    })?;

    match res {
        Ok(id) => Ok(json!({
            "content": [{ "type": "text", "text": format!("Thought recorded. ID: {}", id) }]
        })),
        Err(e) => Err(JsonRpcError {
            code: -32603,
            message: format!("DB Error: {}", e),
            data: None,
        }),
    }
}

async fn read_history(args: Value, store: Arc<Mutex<CvcStore>>) -> Result<Value, JsonRpcError> {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let res = tokio::task::spawn_blocking(move || {
        let store = store
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to lock store"))?;
        let interactions = store.get_floating_interactions()?;
        // Sort by timestamp desc? `get_floating_interactions` order is explicit in SQL? No order clause.
        // Let's just take last N.
        let mut recent = interactions;
        // Ideally we sort by timestamp.
        recent.sort_by_key(|i| i.timestamp);
        recent.reverse();
        recent.truncate(limit);
        Ok::<_, anyhow::Error>(recent)
    })
    .await
    .map_err(|e| JsonRpcError {
        code: -32603,
        message: format!("Internal Error: {}", e),
        data: None,
    })?;

    match res {
        Ok(interactions) => {
            // Format as text
            let mut text = String::new();
            for i in interactions {
                text.push_str(&format!(
                    "ID: {}\nTime: {}\nPrompt: {}\nCoT: {:?}\n\n",
                    i.id, i.timestamp, i.user_prompt, i.model_cot
                ));
            }
            if text.is_empty() {
                text = "No recent history found.".to_string();
            }
            Ok(json!({ "content": [{ "type": "text", "text": text }] }))
        }
        Err(e) => Err(JsonRpcError {
            code: -32603,
            message: format!("DB Error: {}", e),
            data: None,
        }),
    }
}

async fn get_context(args: Value) -> Result<Value, JsonRpcError> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or(JsonRpcError {
            code: -32602,
            message: "Missing 'file_path' argument".to_string(),
            data: None,
        })?
        .to_string(); // Own the string

    let res = tokio::task::spawn_blocking(move || {
        // Assume current directory is repo root
        let repo = cvc_core::git::open_repo(".")
            .map_err(|e| anyhow::anyhow!("Failed to open repo: {}", e))?;

        // We need a dummy interaction ID to call snapshot_context (it links items to it)
        // But here we are just *reading* context, not necessarily saving it yet?
        // snapshot_context returns ContextItem struct which has interaction_id field.
        // We can ignore it or use a default.
        let interaction_id = InteractionId::new();

        let items = cvc_core::git::snapshot_context(&repo, &interaction_id, &[file_path])
            .map_err(|e| anyhow::anyhow!("Failed to snapshot context: {}", e))?;

        Ok::<_, anyhow::Error>(items)
    })
    .await
    .map_err(|e| JsonRpcError {
        code: -32603,
        message: format!("Internal Error: {}", e),
        data: None,
    })?;

    match res {
        Ok(items) => {
            // Return the first item (since we asked for one file)
            if let Some(item) = items.first() {
                let status = if item.dirty_patch.is_some() {
                    "dirty"
                } else {
                    "clean"
                };
                Ok(json!({
                    "content": [{
                        "type": "text",
                        "text": format!("File: {}\nStatus: {}\nBlobSHA: {:?}\nHasPatch: {}",
                           item.file_path, status, item.git_blob_sha, item.dirty_patch.is_some())
                    }]
                }))
            } else {
                Ok(json!({ "content": [{ "type": "text", "text": "File not found or ignored." }] }))
            }
        }
        Err(e) => Err(JsonRpcError {
            code: -32603,
            message: format!("Git Error: {}", e),
            data: None,
        }),
    }
}

async fn setup_cvc(args: Value) -> Result<Value, JsonRpcError> {
    // We allow "cwd" argument for testing purposes, or default to current_dir.
    let current_dir = if let Some(cwd) = args.get("cwd").and_then(|v| v.as_str()) {
        std::path::PathBuf::from(cwd)
    } else {
        std::env::current_dir().map_err(|e| JsonRpcError {
            code: -32603,
            message: format!("Failed to get current directory: {}", e),
            data: None,
        })?
    };

    let res = tokio::task::spawn_blocking(move || {
        // 0. Validate Git Repo
        // Ensure we are in a valid git repository before doing anything.
        // We use discovery to find the repo root if we are in a subdir.
        let repo = cvc_core::git::open_repo(&current_dir)
            .map_err(|_| anyhow::anyhow!("Current directory is not a git repository. CVC requires a git repository to function."))?;

        // We use the workdir as root for hooks installation
        let repo_root = repo.workdir().unwrap_or(&current_dir).to_path_buf();

        // 1. Initialize DB (idempotent)
        let cvc_dir = repo_root.join(".git").join("cvc");
        let db_path = cvc_dir.join("index.db");

        // Ensure parent dir exists
        if !cvc_dir.exists() {
            std::fs::create_dir_all(&cvc_dir)
                .map_err(|e| anyhow::anyhow!("Failed to create cvc dir: {}", e))?;
        }

        // Open store and init schema
        let store = cvc_core::db::CvcStore::open(&db_path)
            .map_err(|e| anyhow::anyhow!("Failed to open DB: {}", e))?;
        store
            .init()
            .map_err(|e| anyhow::anyhow!("Failed to init DB schema: {}", e))?;

        // 2. Install Hooks (idempotent)
        cvc_core::hooks::install(&current_dir)
            .map_err(|e| anyhow::anyhow!("Failed to install hooks: {}", e))?;

        Ok::<_, anyhow::Error>(())
    })
    .await
    .map_err(|e| JsonRpcError {
        code: -32603,
        message: format!("Internal Error: {}", e),
        data: None,
    })?;

    if let Err(e) = res {
        return Err(JsonRpcError {
            code: -32603,
            message: format!("Setup Failed: {}", e),
            data: None,
        });
    }

    Ok(
        json!({ "content": [{ "type": "text", "text": "CVC initialized and hooks installed successfully." }] }),
    )
}
