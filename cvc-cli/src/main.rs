use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

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
    /// List conversations with activity and per-destination share state
    Conversations {
        #[arg(long)]
        remote: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Share a conversation explicitly; with no id, pick one interactively
    Share {
        conversation_id: Option<String>,
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
    /// Manage exact derivation evidence.
    Relink {
        #[command(subcommand)]
        command: RelinkCommands,
    },
    /// Manage agent-harness capture hooks in this checkout
    Harness {
        #[command(subcommand)]
        command: HarnessCommands,
    },
    /// Record an agent-harness session transcript as local private capture
    Ingest {
        #[command(subcommand)]
        command: IngestCommands,
    },
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

#[derive(Clone, Copy, ValueEnum)]
enum HarnessKind {
    /// Claude Code (Anthropic's CLI agent)
    ClaudeCode,
}

#[derive(Subcommand)]
enum HarnessCommands {
    /// Install lifecycle hooks in this checkout's local harness settings
    Install {
        harness: HarnessKind,
        /// Absolute path of the cvc binary the hooks run (default: this executable)
        #[arg(long)]
        binary: Option<PathBuf>,
    },
    /// Remove the hook entries installed by `cvc harness install`
    Uninstall { harness: HarnessKind },
}

#[derive(Subcommand)]
enum IngestCommands {
    /// Ingest a Claude Code JSONL session transcript
    ClaudeCode {
        /// Transcript file (~/.claude/projects/<workspace>/<session>.jsonl)
        #[arg(long, conflicts_with = "hook")]
        transcript: Option<PathBuf>,
        /// Session id; defaults to the transcript file name
        #[arg(long, requires = "transcript")]
        session: Option<String>,
        /// Read the hook payload Claude Code delivers on stdin
        #[arg(long)]
        hook: bool,
    },
}

#[derive(Subcommand)]
enum HookCommands {
    PostCommit,
    PrePush {
        remote_name: String,
        remote_url: String,
    },
    PostMerge {
        /// Git supplies 0/1 to indicate whether the merge was a squash.
        squash: Option<String>,
    },
    PostRewrite {
        mode: String,
    },
}

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    let advisory_hook = args.get(1).is_some_and(|arg| arg == "hook");
    let result = (|| -> Result<()> {
        let cli = Cli::try_parse_from(&args)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(dispatch(cli))
    })();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) if advisory_hook => {
            eprintln!("CVC Hook Warning: advisory hook skipped: {error}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn dispatch(cli: Cli) -> Result<()> {
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
        Commands::Harness { command } => match command {
            HarnessCommands::Install {
                harness: HarnessKind::ClaudeCode,
                binary,
            } => commands::harness::install_claude_code(binary).await?,
            HarnessCommands::Uninstall {
                harness: HarnessKind::ClaudeCode,
            } => commands::harness::uninstall_claude_code().await?,
        },
        Commands::Ingest { command } => match command {
            IngestCommands::ClaudeCode {
                transcript,
                session,
                hook,
            } => commands::ingest::claude_code(transcript, session, hook).await?,
        },
        Commands::Hook { command } => match command {
            HookCommands::PostCommit => {
                commands::hook::post_commit().await?;
            }
            HookCommands::PrePush {
                remote_name,
                remote_url,
            } => commands::hook::pre_push(&remote_name, &remote_url).await?,
            HookCommands::PostMerge { squash } => {
                commands::hook::post_merge(squash.as_deref()).await?
            }
            HookCommands::PostRewrite { mode } => commands::hook::post_rewrite(&mode).await?,
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
        Commands::Conversations { remote, limit } => {
            commands::conversations::run(remote.as_deref(), limit).await?
        }
        Commands::Share {
            conversation_id,
            remote,
            future,
            push,
        } => {
            commands::sync::share(conversation_id.as_deref(), future, push, remote.as_deref())
                .await?
        }
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
        Commands::Relink { command } => match command {
            RelinkCommands::ObserveRange { base, tip, remote } => {
                commands::sync::observe_range(&base, &tip, remote.as_deref()).await?
            }
        },
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
enum RelinkCommands {
    ObserveRange {
        base: String,
        tip: String,
        #[arg(long)]
        remote: Option<String>,
    },
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

#[cfg(test)]
mod hook_cli_tests {
    use super::*;

    #[test]
    fn clap_accepts_git_hook_argument_shapes() {
        for args in [
            vec!["cvc", "hook", "post-commit"],
            vec!["cvc", "hook", "pre-push", "origin", "file:///remote"],
            vec!["cvc", "hook", "post-merge", "1"],
            vec!["cvc", "hook", "post-rewrite", "rebase"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }

    #[test]
    fn clap_accepts_harness_and_ingest_shapes() {
        for args in [
            vec!["cvc", "harness", "install", "claude-code"],
            vec![
                "cvc",
                "harness",
                "install",
                "claude-code",
                "--binary",
                "/opt/cvc/bin/cvc",
            ],
            vec!["cvc", "harness", "uninstall", "claude-code"],
            vec!["cvc", "ingest", "claude-code", "--hook"],
            vec!["cvc", "ingest", "claude-code", "--transcript", "s.jsonl"],
            vec![
                "cvc",
                "ingest",
                "claude-code",
                "--transcript",
                "s.jsonl",
                "--session",
                "s",
            ],
        ] {
            assert!(Cli::try_parse_from(&args).is_ok(), "{args:?}");
        }
        // Hook mode and an explicit transcript are different entry points.
        assert!(Cli::try_parse_from([
            "cvc",
            "ingest",
            "claude-code",
            "--hook",
            "--transcript",
            "s.jsonl"
        ])
        .is_err());
        assert!(Cli::try_parse_from(["cvc", "harness", "install", "cursor"]).is_err());
    }
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
