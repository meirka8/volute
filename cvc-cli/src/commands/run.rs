use anyhow::{bail, Context, Result};
use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::git::{self, open_repo};
use cvc_core::models::{Author, Interaction, InteractionId};
use cvc_core::privacy::{CliRunCapture, PreparedPolicy};
use std::env;
use std::process::{Command, Stdio};
use uuid::Uuid;
pub async fn run(args: Vec<String>) -> Result<()> {
    if args.is_empty() {
        bail!("No command provided to run.");
    }

    let current_dir = env::current_dir()?;
    let cvc_dir = current_dir.join(".git").join("cvc");
    let db_path = cvc_dir.join("index.db");
    // Load once before opening the repository or inspecting status/diffs. This
    // exact policy snapshot is later handed to persistence.
    let policy = if cvc_dir.exists() {
        PreparedPolicy::load(&current_dir)
            .map_err(|error| anyhow::anyhow!("CVC capture blocked by .thoughtignore: {error}"))?
    } else {
        PreparedPolicy::built_ins_only()
    };

    // 1. Snapshot Context (Git State)
    let mut context_items = Vec::new();
    let mut repo_opt = None;

    if cvc_dir.exists() {
        if let Ok(repo) = open_repo(&current_dir) {
            if let Ok(dirty_files) = git::get_dirty_files(&repo) {
                let dirty_files: Vec<String> = dirty_files
                    .into_iter()
                    .filter(|path| !policy.ignores_path(path))
                    .collect();
                // Generate temporary ID to associate context
                let temp_id = InteractionId::new();

                if let Ok(items) = git::snapshot_context(&repo, &temp_id, &dirty_files) {
                    context_items = items;
                }
                repo_opt = Some(repo);
            }
        }
    }

    // 2. Run Command
    let cmd_name = &args[0];
    let cmd_args = &args[1..];
    let prompt_str = args.join(" ");

    println!("CVC Running: {}", prompt_str);

    let child = Command::new(cmd_name)
        .args(cmd_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()) // Capture stderr too
        .spawn()
        .context(format!("Failed to execute command: {}", cmd_name))?;

    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    print!("{}", stdout);
    eprint!("{}", stderr); // Pipe stderr back to user

    if !cvc_dir.exists() {
        return Ok(());
    }

    // 3. Save Interaction
    let combined_response = if stderr.is_empty() {
        stdout
    } else {
        format!("{}\n\n[STDERR]\n{}", stdout, stderr)
    };

    if let Some(_repo) = repo_opt {
        // Warn if we can't open store but CVC dir exists
        match CvcStore::open_initialized(&db_path) {
            Ok(store) => {
                let interaction_id = InteractionId::new();

                // Generate a unique session ID for this run to keep it distinct
                // Or use a persistent one if we could track shell sessions.
                // For now, unique per run as requested.
                let session_id = format!("run-{}", Uuid::new_v4());

                // Update context items with real ID
                let final_context_items: Vec<_> = context_items
                    .into_iter()
                    .map(|mut item| {
                        item.interaction_id = interaction_id.clone();
                        item
                    })
                    .collect();

                let interaction = Interaction {
                    id: interaction_id.clone(),
                    conversation_id: session_id,
                    parent_id: None,
                    timestamp: Utc::now(),
                    author: Author::System,
                    user_prompt: prompt_str,
                    model_name: Some("process-shim".to_string()),
                    model_cot: None,
                    model_response: Some(combined_response),
                    source_request_id: None,
                };

                if let Err(e) = store.capture_cli_run(CliRunCapture::new(
                    cvc_core::models::Conversation {
                        id: interaction.conversation_id.clone(),
                        title: format!("Run: {cmd_name}"),
                        created_at: Utc::now(),
                    },
                    interaction,
                    final_context_items,
                    Vec::new(),
                    policy,
                )) {
                    eprintln!("CVC Warning: Failed to save interaction: {}", e);
                }
            }
            Err(e) => {
                eprintln!(
                    "CVC Warning: Failed to open database despite CVC initialization: {}",
                    e
                );
            }
        }
    }

    if !output.status.success() {
        bail!("Command failed with exit code: {:?}", output.status.code());
    }

    Ok(())
}
