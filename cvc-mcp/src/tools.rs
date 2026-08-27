use crate::server::{AppState, JsonRpcError};
use chrono::Utc;
use cvc_core::models::{Author, Conversation, Interaction, InteractionId};
use cvc_core::privacy::{McpCapture, PreparedPolicy};
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
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Optional bound worktree path; a different repository or worktree is rejected."
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
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Optional bound worktree path; a different repository or worktree is rejected."
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
        "get_context" => get_context(args, state).await,
        "setup_cvc" => setup_cvc(args, state).await,
        _ => Err(JsonRpcError {
            code: -32601,
            message: format!("Unknown tool: {}", name),
            data: None,
        }),
    }
}

async fn commit_thought(args: Value, state: Arc<AppState>) -> Result<Value, JsonRpcError> {
    let layout = bound_layout(&args, &state)?;
    let policy_root = layout
        .policy_root()
        .map_err(repository_mismatch)?
        .to_owned();
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

    let state_for_store = state.clone();
    let res = tokio::task::spawn_blocking(move || {
        let conversation_id = explicit_conversation_id
            .or(session_conversation_id)
            .unwrap_or_else(|| "agent-session-default".to_string());
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

        let policy = PreparedPolicy::load(&policy_root)?;
        state_for_store
            .revalidate()
            .map_err(|_| anyhow::anyhow!("repository binding changed"))?;
        state_for_store.with_store(|store| {
            Ok(store.capture_mcp(McpCapture::new(
                Conversation {
                    id: interaction.conversation_id.clone(),
                    title: format!("Session {}", interaction.conversation_id),
                    created_at: Utc::now(),
                },
                interaction.clone(),
                Vec::new(),
                Vec::new(),
                policy,
            ))?)
        })?;
        Ok::<_, anyhow::Error>(interaction.id)
    })
    .await
    .map_err(|e| tool_failure("Failed to record thought", e))?;

    match res {
        Ok(id) => {
            *state.last_interaction_id.lock().unwrap() = Some(id.clone());
            Ok(json!({
                "content": [{ "type": "text", "text": format!("Thought recorded. ID: {}", id) }]
            }))
        }
        Err(e) => Err(tool_failure("Failed to record thought", e)),
    }
}

async fn read_history(args: Value, state: Arc<AppState>) -> Result<Value, JsonRpcError> {
    let _layout = bound_layout_without_cwd(&state)?;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let commit = args
        .get("commit")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let conversation_id = state.conversation_id.lock().unwrap().clone();

    let state_for_store = state.clone();
    let res = tokio::task::spawn_blocking(move || {
        state_for_store
            .revalidate()
            .map_err(|_| anyhow::anyhow!("repository binding changed"))?;
        let interactions = state_for_store.with_store(|store| {
            Ok(if let Some(commit_sha) = commit {
                // Resuming at a known commit: return exactly what led to it, not a mix
                // diluted with unrelated recent chatter.
                store.get_interactions_for_commit(&cvc_core::models::CommitSha::new(commit_sha))?
            } else {
                // Recent interactions for the current session's conversation first, then
                // fill the rest with recent repo-wide interactions, regardless of link
                // status -- agent memory should survive a commit, not go blank the moment
                // one lands.
                let mut recent = match &conversation_id {
                    Some(conv_id) => {
                        store.get_recent_interactions_for_conversation(conv_id, limit)?
                    }
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
            })
        })?;
        state_for_store
            .revalidate()
            .map_err(|_| anyhow::anyhow!("repository binding changed"))?;

        Ok::<_, anyhow::Error>(interactions)
    })
    .await
    .map_err(|e| tool_failure("History unavailable", e))?;

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
        Err(e) => Err(tool_failure("History unavailable", e)),
    }
}

async fn sync_history(args: Value, state: Arc<AppState>) -> Result<Value, JsonRpcError> {
    let layout = bound_layout(&args, &state)?;

    let state_for_store = state.clone();
    let res = tokio::task::spawn_blocking(move || {
        let repo = layout.into_repository();

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

        let destination = cvc_core::privacy::remote_destination(&repo, &remote_name)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let _operation_lock =
            cvc_core::privacy::destination_operation_lock(&repo, &destination.fingerprint)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        // Network fetch deliberately occurs outside the SQLite mutex.
        cvc_core::sync::fetch_destination(&repo, &destination)?;
        // Revalidate after network work and immediately before import/mutation.
        state_for_store
            .revalidate()
            .map_err(|_| anyhow::anyhow!("repository binding changed"))?;
        let new_count = state_for_store.with_store(|store| {
            Ok(cvc_core::sync::pull_destination(
                &repo,
                store,
                &destination,
            )?)
        })?;
        Ok::<_, anyhow::Error>((remote_name, new_count))
    })
    .await
    .map_err(|e| tool_failure("Sync failed", e))?;

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
        Err(e) => Err(tool_failure("Sync failed", e)),
    }
}

async fn get_context(args: Value, state: Arc<AppState>) -> Result<Value, JsonRpcError> {
    let layout = bound_layout(&args, &state)?;
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
        let repo = layout.into_repository();

        // We need a dummy interaction ID to call snapshot_context (it links items to it)
        // But here we are just *reading* context, not necessarily saving it yet?
        // snapshot_context returns ContextItem struct which has interaction_id field.
        // We can ignore it or use a default.
        let interaction_id = InteractionId::new();

        state
            .revalidate()
            .map_err(|_| anyhow::anyhow!("repository binding changed"))?;
        let items = cvc_core::git::snapshot_context(&repo, &interaction_id, &[file_path])
            .map_err(|e| anyhow::anyhow!("Failed to snapshot context: {}", e))?;
        state
            .revalidate()
            .map_err(|_| anyhow::anyhow!("repository binding changed"))?;

        Ok::<_, anyhow::Error>(items)
    })
    .await
    .map_err(|e| tool_failure("Context unavailable", e))?;

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
        Err(e) => Err(tool_failure("Context unavailable", e)),
    }
}

async fn setup_cvc(args: Value, state: Arc<AppState>) -> Result<Value, JsonRpcError> {
    let layout = bound_layout(&args, &state)?;
    let state_for_store = state.clone();

    let res = tokio::task::spawn_blocking(move || {
        // The state store was opened from this exact common-dir path at startup.
        // Never open a request-selected database here.
        state_for_store
            .revalidate()
            .map_err(|_| anyhow::anyhow!("repository binding changed"))?;
        state_for_store.with_store(|store| {
            store
                .init()
                .map_err(|e| anyhow::anyhow!("Failed to init DB schema: {}", e))
        })?;

        // 2. Install Hooks (idempotent)
        state_for_store
            .revalidate()
            .map_err(|_| anyhow::anyhow!("repository binding changed"))?;
        cvc_core::hooks::install_layout(&layout)
            .map_err(|e| anyhow::anyhow!("Failed to install hooks: {}", e))?;
        state_for_store
            .revalidate()
            .map_err(|_| anyhow::anyhow!("repository binding changed"))?;

        Ok::<_, anyhow::Error>(())
    })
    .await
    .map_err(|e| tool_failure("Setup failed", e))?;

    if let Err(e) = res {
        return Err(tool_failure("Setup failed", e));
    }

    Ok(
        json!({ "content": [{ "type": "text", "text": "CVC initialized and hooks installed successfully." }] }),
    )
}

/// Resolve an optional legacy cwd only after proving it is the startup
/// worktree. Omitting it still revalidates the bound root, so metadata changes
/// after startup fail closed rather than redirecting an operation.
fn bound_layout(
    args: &Value,
    state: &AppState,
) -> Result<cvc_core::repository::RepositoryLayout, JsonRpcError> {
    let path = match args.get("cwd") {
        Some(Value::String(path)) if !path.is_empty() => std::path::PathBuf::from(path),
        Some(_) => return Err(invalid_cwd()),
        None => state.repository().policy_root().to_owned(),
    };
    state.repository().rediscover(&path)
}

fn bound_layout_without_cwd(
    state: &AppState,
) -> Result<cvc_core::repository::RepositoryLayout, JsonRpcError> {
    state
        .repository()
        .rediscover(state.repository().policy_root())
}

fn invalid_cwd() -> JsonRpcError {
    JsonRpcError {
        code: -32602,
        message: "Invalid 'cwd' parameter".into(),
        data: None,
    }
}

fn repository_mismatch(_: cvc_core::repository::RepositoryLayoutError) -> JsonRpcError {
    JsonRpcError {
        code: -32602,
        message: "Repository mismatch".into(),
        data: None,
    }
}

fn tool_failure(message: &'static str, error: impl std::fmt::Display) -> JsonRpcError {
    let _ = error;
    eprintln!("cvc-mcp: {message}");
    JsonRpcError {
        code: -32603,
        message: message.into(),
        data: None,
    }
}
