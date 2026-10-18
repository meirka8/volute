use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

mod commands;
mod terminal_text;
mod tty;
mod tui;

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
    /// Browse conversations and their per-destination share state
    ///
    /// In a terminal this opens an interactive view of the captured
    /// conversations, most recent first, with their share state for one remote.
    /// The view can hand over to the usual typed-phrase prompts (the ones of
    /// `cvc share`, `cvc unshare`, `cvc push --manual` and `cvc privacy
    /// acknowledge-sharing`) and does not answer them; no key shares or
    /// publishes by itself. Where auto-push is on, every git push to that
    /// remote publishes shared thoughts.
    ///
    /// When standard input or output is not a terminal (a pipe, a redirect, a
    /// script), or with --plain, it prints the plain listing instead, which only
    /// reads.
    Conversations {
        /// Remote to show share state for (default: origin)
        #[arg(long)]
        remote: Option<String>,
        /// Show at most this many conversations (plain default: 20; the interactive view shows up to 10,000 unless set)
        #[arg(long)]
        limit: Option<usize>,
        /// Print the plain listing instead of the interactive view
        ///
        /// Use it in scripts, and in any tool that runs cvc under a
        /// pseudo-terminal: that looks like a terminal, so without --plain the
        /// tool would get the interactive view.
        #[arg(long)]
        plain: bool,
    },
    /// Share a conversation explicitly; with no id, pick one interactively
    Share {
        conversation_id: Option<String>,
        #[arg(long)]
        remote: Option<String>,
        /// Also share thoughts added to this conversation later
        #[arg(long)]
        future: bool,
        #[arg(long)]
        push: bool,
    },
    /// Make a conversation's shared thoughts private again for a remote; refused once any of it has been published, or while a publish of it is unfinished
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
        let cli = match Cli::try_parse_from(&args) {
            Ok(cli) => cli,
            // `--help` and `--version` are successful output, not failures:
            // clap prints them to stdout and exits 0.
            Err(error)
                if matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                ) =>
            {
                error.exit()
            }
            Err(error) => return Err(error.into()),
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(dispatch(cli))
    })();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) if advisory_hook => {
            eprintln!(
                "CVC Hook Warning: advisory hook skipped: {}",
                error_text(&error, false)
            );
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}", error_text(&error, true));
            std::process::ExitCode::FAILURE
        }
    }
}

/// An error as it is printed, with its causes (`outer: inner`) when `chain` is
/// set. What it says can come from outside: the text of a remote's error
/// reaches it, a value that was refused is quoted in it, and a stored title or
/// id can be named in it. None of that may carry anything that acts on the
/// terminal, so every control and format character is replaced, a line feed
/// excepted: an error is several lines when it has several (the usage that
/// follows a command-line mistake), and a line feed cannot redraw what came
/// before it.
fn error_text(error: &anyhow::Error, chain: bool) -> String {
    let text = if chain {
        format!("{error:#}")
    } else {
        error.to_string()
    };
    terminal_text::sanitize_lines_for_terminal(&text).into_owned()
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
        Commands::Conversations {
            remote,
            limit,
            plain,
        } => commands::conversations::run(remote.as_deref(), limit, plain).await?,
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

    #[test]
    fn conversations_limit_is_optional_and_plain_is_a_flag() {
        let parse = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
            Commands::Conversations {
                remote,
                limit,
                plain,
            } => (remote, limit, plain),
            _ => panic!("not the conversations command: {args:?}"),
        };
        assert_eq!(parse(&["cvc", "conversations"]), (None, None, false));
        assert_eq!(
            parse(&["cvc", "conversations", "--limit", "5"]),
            (None, Some(5), false)
        );
        assert_eq!(
            parse(&[
                "cvc",
                "conversations",
                "--plain",
                "--remote",
                "upstream",
                "--limit",
                "0"
            ]),
            (Some("upstream".to_owned()), Some(0), true)
        );
        assert!(Cli::try_parse_from(["cvc", "conversations", "--limit", "many"]).is_err());
        assert!(Cli::try_parse_from(["cvc", "conversations", "--plain=maybe"]).is_err());
    }

    /// The help of the `conversations`, `share` and `unshare` commands, as the
    /// person reading `--help` gets it.
    fn rendered_help(command: &str) -> String {
        use clap::CommandFactory;
        let mut cli = Cli::command();
        cli.find_subcommand_mut(command)
            .unwrap_or_else(|| panic!("no {command} command"))
            .render_long_help()
            .to_string()
    }

    fn argument_help(command: &str, argument: &str) -> (String, Option<String>) {
        use clap::CommandFactory;
        let cli = Cli::command();
        let argument = cli
            .find_subcommand(command)
            .and_then(|command| command.get_arguments().find(|a| a.get_id() == argument))
            .unwrap_or_else(|| panic!("no --{argument} on {command}"))
            .clone();
        (
            argument.get_help().expect("it has help").to_string(),
            argument.get_long_help().map(|help| help.to_string()),
        )
    }

    #[test]
    fn the_conversations_help_explains_the_view_and_the_listing() {
        use clap::CommandFactory;
        let cli = Cli::command();
        let conversations = cli.find_subcommand("conversations").unwrap();
        assert_eq!(
            conversations.get_about().unwrap().to_string(),
            "Browse conversations and their per-destination share state"
        );
        let long = rendered_help("conversations");
        // What the view is, and that it adds no way to consent.
        assert!(
            long.contains("In a terminal this opens an interactive view"),
            "{long}"
        );
        assert!(
            long.contains("hand over to the usual typed-phrase prompts"),
            "{long}"
        );
        assert!(long.contains("and does not answer them"), "{long}");
        assert!(
            long.contains("no key shares or publishes by itself"),
            "{long}"
        );
        // And what a standing choice does without one.
        assert!(
            long.contains(
                "Where auto-push is on, every git push to that remote publishes shared thoughts."
            ),
            "{long}"
        );
        // And when it does not open.
        assert!(long.contains("not a terminal"), "{long}");
        assert!(
            long.contains("with --plain, it prints the plain listing instead"),
            "{long}"
        );

        assert_eq!(
            argument_help("conversations", "remote").0,
            "Remote to show share state for (default: origin)"
        );
        assert_eq!(
            argument_help("conversations", "limit").0,
            "Show at most this many conversations (plain default: 20; the interactive view shows up to 10,000 unless set)"
        );
        // The help names the number the view really starts with.
        assert_eq!(commands::sync::SUMMARY_SCAN_LIMIT, 10_000);
        let (plain, long_plain) = argument_help("conversations", "plain");
        assert_eq!(
            plain,
            "Print the plain listing instead of the interactive view"
        );
        let long_plain = long_plain.expect("--plain has a long help");
        assert!(long_plain.contains("scripts"), "{long_plain}");
        assert!(long_plain.contains("pseudo-terminal"), "{long_plain}");
    }

    #[test]
    fn the_share_and_unshare_help_say_what_they_do_and_refuse() {
        assert_eq!(
            argument_help("share", "future").0,
            "Also share thoughts added to this conversation later"
        );
        use clap::CommandFactory;
        let cli = Cli::command();
        assert_eq!(
            cli.find_subcommand("unshare")
                .unwrap()
                .get_about()
                .unwrap()
                .to_string(),
            "Make a conversation's shared thoughts private again for a remote; refused once any of it has been published, or while a publish of it is unfinished"
        );
        // The help of a command that exists to stop sharing must not promise
        // more than it does: it cannot recall what a remote already holds.
        let unshare = rendered_help("unshare");
        assert!(
            unshare.contains("refused once any of it has been published"),
            "{unshare}"
        );
    }

    #[test]
    fn a_printed_error_cannot_act_on_the_terminal_but_keeps_its_lines() {
        let error = anyhow::anyhow!("remote said: \u{1b}[2J\u{1b}]0;pwned\u{7}\r\u{202e}gnp")
            .context("push failed\nsecond line");
        // ESC, BEL, the carriage return and the right-to-left override are each
        // one `?`; the line feed between the two lines of the message stays.
        assert_eq!(
            error_text(&error, true),
            "push failed\nsecond line: remote said: ?[2J?]0;pwned???gnp"
        );
        // Only the outermost message when the chain is not asked for.
        assert_eq!(error_text(&error, false), "push failed\nsecond line");
        // Text without such characters, several lines included, is unchanged.
        let plain = anyhow::anyhow!("שלום – café ✓\nUsage: cvc <COMMAND>");
        assert_eq!(
            error_text(&plain, true),
            "שלום – café ✓\nUsage: cvc <COMMAND>"
        );
    }

    #[test]
    fn stubbed_subcommands_no_longer_exist() {
        // `component` and `auth` only ever simulated success.
        assert!(Cli::try_parse_from(["cvc", "component", "list"]).is_err());
        assert!(Cli::try_parse_from(["cvc", "component", "install", "mcp"]).is_err());
        assert!(Cli::try_parse_from(["cvc", "auth", "login"]).is_err());
    }
}
