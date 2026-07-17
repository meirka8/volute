use crate::server::{AppState, JsonRpcError};
use chrono::Utc;
use cvc_core::models::{Author, Conversation, Interaction, InteractionId};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

pub fn list_tools() -> Value {
    json!({
        "tools": [
            {
                "name": "commit_thought",
                "description": "You MUST call this after every meaningful step: forming a plan, making a non-trivial decision, rejecting an approach, or finishing a subtask -- not only once at the very end. This is the only durable record of your reasoning. If you skip it, the plan, the rationale, and the paths you considered and rejected are gone the moment this session ends, unrecoverable by you or by anyone (including you, on another machine, via another harness) picking up this work later. Log liberally rather than sparingly; keep each entry concise and focused, not exhaustive.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "The task, request, or subtask being worked on"
                        },
                        "reasoning": {
                            "type": "string",
                            "description": "Concise rationale, decision notes, or important reasoning worth retaining"
                        },
                        "response": {
                            "type": "string",
                            "description": "Optional result, response, or action taken"
                        },
                        "context_summary": {
                            "type": "string",
                            "description": "Optional summary of the relevant code or repo state"
                        },
                        "conversation_id": {
                            "type": "string",
                            "description": "Optional conversation ID for clients/harnesses that manage their own sessions instead of using the server's default per-process session. Created if it doesn't already exist."
                        },
                        "parent_id": {
                            "type": "string",
                            "description": "Optional explicit parent interaction ID, for branching instead of continuing the default linear per-session chain."
                        }
                    },
                    "required": ["task", "reasoning"]
                }
            },
            {
                "name": "read_history",
                "description": "You MUST call this at the start of a task, before assuming you are starting from a blank slate. It recovers the reasoning and decisions already recorded for this repository, including from earlier sessions and other machines -- not just this conversation. If the repository was cloned, pulled, or checked out somewhere new since the history you need was recorded, call sync_history first so this tool has something to find.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": {
                            "type": "integer",
                            "description": "Optional number of recent entries to return. Ignored if 'commit' is set."
                        },
                        "commit": {
                            "type": "string",
                            "description": "Optional commit SHA. If set, returns the interactions that led to that specific commit instead of recent history -- use this to resume work at a known commit, e.g. after cloning or checking out on a different machine."
                        }
                    }
                }
            },
            {
                "name": "sync_history",
                "description": "Call this before read_history whenever you cannot be sure the local CVC history cache is current: a fresh clone, a repository you have not worked in during this session, a different machine than the one that recorded prior thoughts, or a long-idle checkout. It fetches CVC history pushed to the git remote by other sessions or teammates and ingests it locally so read_history can actually find it. It never pushes local thoughts anywhere -- syncing outward happens automatically on git push, not through this tool.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cwd": {
                            "type": "string",
                            "description": "Optional working directory to sync, mainly for tests or explicit repo targeting"
                        }
                    }
                }
            },
            {
                "name": "get_context",
                "description": "Inspect git-backed context for one file. Call this before summarizing or recording a file's state in a thought -- do not guess whether a file is clean or has uncommitted changes. It reports one file per call.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Repository-relative path for the file to inspect"
                        }
                    },
                    "required": ["file_path"]
                }
            },
            {
                "name": "setup_cvc",
                "description": "Call this proactively, without asking for confirmation, the first time another CVC tool fails because storage or hooks are missing for the current repository. It only initializes a local SQLite cache and local git hooks -- nothing leaves the machine. It only works inside a git repository.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cwd": {
                            "type": "string",
                            "description": "Optional working directory to initialize, mainly for tests or explicit repo targeting"
                        }
                    }
                }
            }
        ]
    })
}

pub async fn call_tool(params: Value, state: Arc<AppState>) -> Result<Value, JsonRpcError> {
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
        "commit_thought" => commit_thought(args, state).await,
        "read_history" => read_history(args, state).await,
        "sync_history" => sync_history(args, state).await,
        "get_context" => get_context(args).await,
        "setup_cvc" => setup_cvc(args).await,
        _ => Err(JsonRpcError {
            code: -32601,
            message: format!("Unknown tool: {}", name),
            data: None,
        }),
    }
}

async fn commit_thought(args: Value, state: Arc<AppState>) -> Result<Value, JsonRpcError> {
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

    // Clients/harnesses that manage their own sessions can override the server's
    // default per-process session and chaining behavior.
    let explicit_conversation_id = args
        .get("conversation_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let explicit_parent_id = match args.get("parent_id").and_then(|v| v.as_str()) {
        Some(s) => Some(s.parse::<InteractionId>().map_err(|e| JsonRpcError {
            code: -32602,
            message: format!("Invalid 'parent_id': {}", e),
            data: None,
        })?),
        None => None,
    };

    let session_conversation_id = state.conversation_id.lock().unwrap().clone();
    let session_parent_id = state.last_interaction_id.lock().unwrap().clone();

    let store = state.store.clone();
    let res = tokio::task::spawn_blocking(move || {
        let store = store
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to lock store"))?;

        let conversation_id = explicit_conversation_id
            .or(session_conversation_id)
            .unwrap_or_else(|| "agent-session-default".to_string());

        // Covers both a client-supplied conversation_id we haven't seen before and
        // the fallback default when no MCP `initialize` handshake set up a session.
        if store.get_conversation(&conversation_id)?.is_none() {
            store.create_conversation(&Conversation {
                id: conversation_id.clone(),
                title: format!("Session {}", conversation_id),
                created_at: Utc::now(),
            })?;
        }

        let interaction = Interaction {
            id: InteractionId::new(),
            conversation_id,
            parent_id: explicit_parent_id.or(session_parent_id),
            timestamp: Utc::now(),
            author: Author::Agent,
            user_prompt: task, // The task/prompt the agent was asked to complete
            model_name: Some("agent".to_string()),
            model_cot: Some(full_cot), // The agent's reasoning/chain of thought
            model_response: response,  // The agent's response/action taken
            source_request_id: None,
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
        Ok(id) => {
            *state.last_interaction_id.lock().unwrap() = Some(id.clone());
            Ok(json!({
                "content": [{ "type": "text", "text": format!("Thought recorded. ID: {}", id) }]
            }))
        }
        Err(e) => Err(JsonRpcError {
            code: -32603,
            message: format!("DB Error: {}", e),
            data: None,
        }),
    }
}

async fn read_history(args: Value, state: Arc<AppState>) -> Result<Value, JsonRpcError> {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let commit = args
        .get("commit")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let conversation_id = state.conversation_id.lock().unwrap().clone();

    let store = state.store.clone();
    let res = tokio::task::spawn_blocking(move || {
        let store = store
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to lock store"))?;

        let interactions = if let Some(commit_sha) = commit {
            // Resuming at a known commit: return exactly what led to it, not a mix
            // diluted with unrelated recent chatter.
            store.get_interactions_for_commit(&cvc_core::models::CommitSha::new(commit_sha))?
        } else {
            // Recent interactions for the current session's conversation first, then
            // fill the rest with recent repo-wide interactions, regardless of link
            // status -- agent memory should survive a commit, not go blank the moment
            // one lands.
            let mut recent = match &conversation_id {
                Some(conv_id) => store.get_recent_interactions_for_conversation(conv_id, limit)?,
                None => Vec::new(),
            };

            if recent.len() < limit {
                let seen: HashSet<_> = recent.iter().map(|i| i.id.clone()).collect();
                for interaction in store.get_recent_interactions(limit)? {
                    if recent.len() >= limit {
                        break;
                    }
                    if !seen.contains(&interaction.id) {
                        recent.push(interaction);
                    }
                }
            }

            recent
        };

        Ok::<_, anyhow::Error>(interactions)
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

async fn sync_history(args: Value, state: Arc<AppState>) -> Result<Value, JsonRpcError> {
    let current_dir = if let Some(cwd) = args.get("cwd").and_then(|v| v.as_str()) {
        std::path::PathBuf::from(cwd)
    } else {
        std::env::current_dir().map_err(|e| JsonRpcError {
            code: -32603,
            message: format!("Failed to get current directory: {}", e),
            data: None,
        })?
    };

    let store = state.store.clone();
    let res = tokio::task::spawn_blocking(move || {
        let repo = cvc_core::git::open_repo(&current_dir)
            .map_err(|e| anyhow::anyhow!("Failed to open repo: {}", e))?;

        let remotes = repo.remotes()?;
        let remote_name = if remotes.iter().any(|r| r == Some("origin")) {
            "origin".to_string()
        } else {
            remotes
                .iter()
                .flatten()
                .next()
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow::anyhow!("No git remotes configured for this repository"))?
        };

        let store = store
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to lock store"))?;

        let new_count = cvc_core::sync::fetch_and_pull(&repo, &store, &remote_name)?;
        Ok::<_, anyhow::Error>((remote_name, new_count))
    })
    .await
    .map_err(|e| JsonRpcError {
        code: -32603,
        message: format!("Internal Error: {}", e),
        data: None,
    })?;

    match res {
        Ok((remote_name, new_count)) => Ok(json!({
            "content": [{
                "type": "text",
                "text": format!(
                    "Synced with remote '{}'. Pulled {} new interaction(s) into local history.",
                    remote_name, new_count
                )
            }]
        })),
        Err(e) => Err(JsonRpcError {
            code: -32603,
            message: format!("Sync Error: {}", e),
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
        let store = cvc_core::db::CvcStore::open_initialized(&db_path)
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
