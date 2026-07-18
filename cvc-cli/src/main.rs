use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;

#[derive(Parser)]
#[command(name = "cvc")]
#[command(about = "Cognitive Version Control CLI", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize CVC in the current repository
    Init,
    /// Show the state of CVC
    Status,
    /// Push CVC interactions to the remote
    Push {
        #[arg(long)]
        remote: Option<String>,
        /// Human-initiated publication (bare push remains auto-consent gated)
        #[arg(long)]
        manual: bool,
    },
    /// Pull CVC interactions from the remote
    Pull,
    /// Inspect or change local privacy acknowledgements
    Privacy {
        #[command(subcommand)]
        command: PrivacyCommands,
    },
    /// Share a conversation explicitly
    Share {
        conversation_id: String,
        #[arg(long)]
        remote: Option<String>,
        #[arg(long)]
        future: bool,
        #[arg(long)]
        push: bool,
    },
    /// Stop sharing unpublished turns in a conversation
    Unshare {
        conversation_id: String,
        #[arg(long)]
        remote: Option<String>,
    },
    /// Propagate a tombstone; this suppresses future CVC projections, not Git objects.
    Redact {
        interaction_id: String,
        #[arg(long)]
        remote: String,
        /// Write a protected local hard-redaction plan (mode 0600).
        #[arg(long)]
        rewrite_plan: std::path::PathBuf,
        /// Switch only refs/cvc/main locally; never pushes or force-pushes.
        #[arg(long)]
        apply_local: bool,
    },
    /// Fetch and report whether a hard-redaction plan is still current.
    RedactVerifyPlan {
        path: std::path::PathBuf,
        #[arg(long)]
        remote: String,
    },
    /// Delete local CVC projections only; this does not erase a remote.
    DeleteLocal { interaction_id: String },
    /// Internal hook commands
    Hook {
        #[command(subcommand)]
        command: HookCommands,
    },
    /// Run a command and capture the interaction
    Run {
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// View the interaction log
    Log,
    /// Manage CVC components (lsp, mcp)
    Component {
        #[command(subcommand)]
        command: ComponentCommands,
    },
    /// Manage authentication
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
}

#[derive(Subcommand)]
enum HookCommands {
    PostCommit,
    PrePush,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            commands::init::run().await?;
        }
        Commands::Status => {
            commands::status::run().await?;
        }
        Commands::Push { remote, manual } => {
            commands::sync::push(remote.as_deref(), manual).await?;
        }
        Commands::Pull => {
            commands::sync::pull().await?;
        }
        Commands::Hook { command } => match command {
            HookCommands::PostCommit => {
                commands::hook::post_commit().await?;
            }
            HookCommands::PrePush => commands::hook::pre_push().await?,
        },
        Commands::Privacy { command } => match command {
            PrivacyCommands::Status { remote } => {
                commands::sync::privacy_status(remote.as_deref()).await?
            }
            PrivacyCommands::AcknowledgeCapture => commands::sync::acknowledge_capture().await?,
            PrivacyCommands::AcknowledgeSharing { remote } => {
                commands::sync::acknowledge_sharing(remote.as_deref()).await?
            }
            PrivacyCommands::SetAutoPush { value, remote } => {
                commands::sync::set_auto_push(&value, remote.as_deref()).await?
            }
            PrivacyCommands::Reconcile { remote } => {
                commands::sync::reconcile(remote.as_deref()).await?
            }
        },
        Commands::Share {
            conversation_id,
            remote,
            future,
            push,
        } => commands::sync::share(&conversation_id, future, push, remote.as_deref()).await?,
        Commands::Unshare {
            conversation_id,
            remote,
        } => commands::sync::unshare(&conversation_id, remote.as_deref()).await?,
        Commands::Redact {
            interaction_id,
            remote,
            rewrite_plan,
            apply_local,
        } => commands::sync::redact(&interaction_id, &remote, &rewrite_plan, apply_local).await?,
        Commands::RedactVerifyPlan { path, remote } => {
            commands::sync::verify_redaction_plan(&path, &remote).await?
        }
        Commands::DeleteLocal { interaction_id } => {
            commands::sync::delete_local(&interaction_id).await?
        }
        Commands::Run { args } => {
            commands::run::run(args).await?;
        }
        Commands::Log => {
            commands::log::run().await?;
        }
        Commands::Component { command } => match command {
            ComponentCommands::List => commands::component::list().await?,
            ComponentCommands::Install { name } => commands::component::install(&name).await?,
            ComponentCommands::Update { name } => commands::component::update(&name).await?,
        },
        Commands::Auth { command } => match command {
            AuthCommands::Login => commands::auth::login().await?,
            AuthCommands::Status => commands::auth::status().await?,
        },
    }

    Ok(())
}

#[derive(Subcommand)]
enum PrivacyCommands {
    Status {
        #[arg(long)]
        remote: Option<String>,
    },
    AcknowledgeCapture,
    AcknowledgeSharing {
        #[arg(long)]
        remote: Option<String>,
    },
    SetAutoPush {
        value: String,
        #[arg(long)]
        remote: Option<String>,
    },
    Reconcile {
        #[arg(long)]
        remote: Option<String>,
    },
}

#[derive(Subcommand)]
enum ComponentCommands {
    /// List available components
    List,
    /// Install a component
    Install { name: String },
    /// Update a component
    Update { name: String },
}

#[derive(Subcommand)]
enum AuthCommands {
    /// Log in to CVC Config
    Login,
    /// Check authentication status
    Status,
}
