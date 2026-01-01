use anyhow::{bail, Context, Result};
use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::git::{self, open_repo};
use cvc_core::models::{Author, Interaction, InteractionId};
use std::env;
use std::process::{Command, Stdio};

pub async fn run(args: Vec<String>) -> Result<()> {
    if args.is_empty() {
        bail!("No command provided to run.");
    }

    let current_dir = env::current_dir()?;
    let cvc_dir = current_dir.join(".git").join("cvc");
    let db_path = cvc_dir.join("index.db");

    // 1. Snapshot Context (Git State)
    let mut context_items = Vec::new();
    let mut repo_opt = None;

    if cvc_dir.exists() {
        if let Ok(repo) = open_repo(&current_dir) {
            if let Ok(dirty_files) = git::get_dirty_files(&repo) {
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
        .stderr(Stdio::inherit())
        .spawn()
        .context(format!("Failed to execute command: {}", cmd_name))?;

    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    print!("{}", stdout);

    if !cvc_dir.exists() {
        return Ok(());
    }

    // 3. Save Interaction
    if let Some(_repo) = repo_opt {
        if let Ok(store) = CvcStore::open(&db_path) {
            let interaction_id = InteractionId::new();

            // Ensure conversation exists
            let session_id = "cli-session";
            if store.get_conversation(session_id)?.is_none() {
                store.create_conversation(&cvc_core::models::Conversation {
                    id: session_id.to_string(),
                    title: "CLI Session".to_string(),
                    created_at: Utc::now(),
                })?;
            }

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
                conversation_id: session_id.to_string(),
                parent_id: None,
                timestamp: Utc::now(),
                author: Author::System,
                user_prompt: prompt_str,
                model_name: Some("process-shim".to_string()),
                model_cot: None,
                model_response: Some(stdout),
            };

            store.create_interaction(&interaction)?;
            for item in final_context_items {
                store.add_context_item(&item)?;
            }
        }
    }

    if !output.status.success() {
        bail!("Command failed with exit code: {:?}", output.status.code());
    }

    Ok(())
}
