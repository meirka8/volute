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
    Push,
    /// Pull CVC interactions from the remote
    Pull,
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
        Commands::Push => {
            commands::sync::push().await?;
        }
        Commands::Pull => {
            commands::sync::pull().await?;
        }
        Commands::Hook { command } => match command {
            HookCommands::PostCommit => {
                commands::hook::post_commit().await?;
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
