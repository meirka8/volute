use anyhow::{Context, Result};
use cvc_core::db::CvcStore;
use cvc_core::models::Interaction;
use cvc_core::repository::RepositoryLayout;
use cvc_core::stimulus::{self, PartKind, Reading};
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

    println!("CVC Log ({} thought(s))", ids.len());
    println!("----------------------------------------");

    for id in ids {
        if let Some(interaction) = store.get_interaction(&id)? {
            println!("Node: {}", interaction.id);
            println!("Time: {}", interaction.timestamp);
            println!("Auth: {:?}", interaction.author);
            println!("Prom: {}", prompt_preview(&interaction));
            println!(
                "Resp: {:.80}...",
                interaction
                    .model_response
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("")
            );
            println!("----------------------------------------");
        }
    }

    Ok(())
}

/// What the log shows of an interaction's prompt. A stimulus recorded with
/// boundary markers shows the first line of the person's text (or, in a
/// subagent, of its task), or a short label of what it holds when there is
/// none; any other text, version 1 or a stimulus whose boundary does not
/// verify, shows its first line as it always did.
fn prompt_preview(interaction: &Interaction) -> String {
    let first_line = |text: &str| format!("{:.80}...", text.lines().next().unwrap_or(""));
    let text = &interaction.user_prompt;
    let Reading::Verified(verified) = stimulus::read(&interaction.id, text) else {
        return first_line(text);
    };
    if let Some(part) = verified
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::Prompt | PartKind::Task))
    {
        return first_line(part.content_of(text));
    }
    let tool_results = verified
        .parts
        .iter()
        .filter(|part| part.kind == PartKind::ToolResult)
        .count();
    if tool_results > 0 {
        format!("tool results ({tool_results})")
    } else if verified
        .parts
        .iter()
        .any(|part| part.kind == PartKind::CompactionSummary)
    {
        "compaction summary".to_owned()
    } else {
        "harness context".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::prompt_preview;
    use chrono::Utc;
    use cvc_core::models::{Author, Interaction, InteractionId};
    use cvc_core::privacy::scrub;
    use cvc_core::stimulus::{self, StimulusPart};

    fn interaction(id: &InteractionId, user_prompt: String) -> Interaction {
        Interaction {
            id: id.clone(),
            conversation_id: "conversation".into(),
            parent_id: None,
            timestamp: Utc::now(),
            author: Author::Human,
            user_prompt,
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        }
    }

    fn preview_of(parts: &[StimulusPart]) -> String {
        let id = InteractionId::new();
        let text = stimulus::render(&id, parts, &|text| scrub(text));
        assert!(text.contains("~cvc-"), "{text}");
        prompt_preview(&interaction(&id, text))
    }

    fn tool(text: &str) -> StimulusPart {
        StimulusPart::tool_result(Some("Bash"), false, text)
    }

    #[test]
    fn a_persons_text_shows_its_first_line_wherever_it_sits() {
        assert_eq!(
            preview_of(&[StimulusPart::prompt("Add a greeting\nand a second line")]),
            "Add a greeting..."
        );
        assert_eq!(
            preview_of(&[
                StimulusPart::meta("<system-reminder>x</system-reminder>"),
                StimulusPart::prompt("Do the thing"),
            ]),
            "Do the thing..."
        );
        // Typed while tools ran: it follows their results.
        assert_eq!(
            preview_of(&[tool("output"), tool("more"), StimulusPart::prompt("Stop")]),
            "Stop..."
        );
        assert_eq!(
            preview_of(&[StimulusPart::task("Audit the diff\nthoroughly")]),
            "Audit the diff..."
        );
    }

    #[test]
    fn a_stimulus_without_a_person_shows_a_label_of_what_it_holds() {
        assert_eq!(preview_of(&[tool("a")]), "tool results (1)");
        assert_eq!(
            preview_of(&[tool("a"), tool("b"), tool("c")]),
            "tool results (3)"
        );
        assert_eq!(
            preview_of(&[StimulusPart::meta("context"), tool("a")]),
            "tool results (1)"
        );
        assert_eq!(
            preview_of(&[StimulusPart::compaction_summary("summary")]),
            "compaction summary"
        );
        assert_eq!(
            preview_of(&[StimulusPart::meta("context")]),
            "harness context"
        );
    }

    #[test]
    fn a_long_first_line_is_cut_to_eighty_characters() {
        let long = "é".repeat(200);
        let shown = preview_of(&[StimulusPart::prompt(long)]);
        assert_eq!(shown, format!("{}...", "é".repeat(80)));
    }

    #[test]
    fn other_text_shows_its_raw_first_line_as_before() {
        let id = InteractionId::new();
        // Version 1.
        let v1 = interaction(&id, "Add a greeting\n\n[tool_result Bash] ok".into());
        assert_eq!(prompt_preview(&v1), "Add a greeting...");
        // Markers whose boundary does not verify for this interaction.
        let lookalike = interaction(&id, "[prompt ~cvc-0123456789abcdef] hello".into());
        assert_eq!(
            prompt_preview(&lookalike),
            "[prompt ~cvc-0123456789abcdef] hello..."
        );
        // A stimulus written for another interaction does not verify here.
        let other = InteractionId::new();
        let text = stimulus::render(&other, &[StimulusPart::prompt("hi")], &|text| scrub(text));
        assert_eq!(
            prompt_preview(&interaction(&id, text.clone())),
            format!("{text}...")
        );
        // Nothing recorded.
        let none = interaction(&id, "[no stimulus recorded]".into());
        assert_eq!(prompt_preview(&none), "[no stimulus recorded]...");
    }
}
