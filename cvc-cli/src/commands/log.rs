use crate::terminal_text::sanitize_for_terminal;
use anyhow::{Context, Result};
use cvc_core::db::CvcStore;
use cvc_core::repository::RepositoryLayout;
use std::borrow::Cow;
use std::env;

pub async fn run() -> Result<()> {
    let current_dir = env::current_dir()?;
    let layout =
        RepositoryLayout::discover(&current_dir).context("Failed to discover Git repository")?;
    let _worktree_root = layout.worktree_root()?;
    let cvc_dir = layout.cvc_dir();

    if !cvc_dir.exists() {
        println!("CVC is not initialized.");
        return Ok(());
    }

    let store = CvcStore::open_initialized(layout.db_path())?;

    // Simplistic log: Get all IDs and show them.
    let ids = store.get_all_interaction_ids()?;

    println!("CVC Log ({} interactions)", ids.len());
    println!("----------------------------------------");

    for id in ids {
        if let Some(interaction) = store.get_interaction(&id)? {
            println!("Node: {}", interaction.id);
            println!("Time: {}", interaction.timestamp);
            println!("Auth: {:?}", interaction.author);
            println!("Prom: {:.80}...", first_line(&interaction.user_prompt));
            println!(
                "Resp: {:.80}...",
                first_line(interaction.model_response.as_deref().unwrap_or(""))
            );
            println!("----------------------------------------");
        }
    }

    Ok(())
}

/// The first line of a prompt or a response, as the log shows it. Captured text
/// is whatever a person, a model or a harness wrote, and a pulled thought's is
/// whatever the remote's projection says, so it is shown with every control and
/// format character replaced by `?`, one for one, so the log's 80-character cut
/// falls where it did. The line is taken first: a prompt of several lines shows
/// its first line, as it always did.
fn first_line(text: &str) -> Cow<'_, str> {
    sanitize_for_terminal(text.lines().next().unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::first_line;

    #[test]
    fn the_first_line_is_shown_as_plain_text() {
        assert_eq!(first_line("Fix the build\nand the tests"), "Fix the build");
        assert_eq!(first_line("one line\r\nanother"), "one line");
        assert_eq!(first_line(""), "");
        // An erase, an OSC 52 clipboard write, a right-to-left override, a line
        // separator and a carriage return do not end the line; each is a `?`.
        assert_eq!(
            first_line("a\u{1b}[2Jb\u{1b}]52;c;SGVsbG8=\u{7}c\u{202e}d\u{2028}e\rf\nsecond"),
            "a?[2Jb?]52;c;SGVsbG8=?c?d?e?f"
        );
        assert_eq!(format!("{:.4}...", first_line("\u{1b}[2Jabc")), "?[2J...");
    }
}
