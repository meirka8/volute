#![cfg(unix)]

//! `cvc share --future` through the real binary on a pseudo-terminal, for a
//! conversation that holds thoughts recorded through MCP: a line before the
//! phrase says that future sharing does not extend to later ones, and it
//! does not.

mod common;

use chrono::Utc;
use common::{Fixture, PtySession};
use cvc_core::db::CvcStore;
use cvc_core::models::{Author, Conversation, Interaction, InteractionId};
use cvc_core::privacy::{McpCapture, PreparedPolicy};

const NOTE: &str = "Future sharing does not extend to thoughts recorded through MCP: later ones stay private until this conversation is shared again.";

/// Records one thought into `conversation` the way the MCP server does.
fn record_through_mcp(fixture: &Fixture, conversation: &str) -> InteractionId {
    let id = InteractionId::new();
    CvcStore::open(fixture.index_db_path())
        .unwrap()
        .capture_mcp(McpCapture::new(
            Conversation {
                id: conversation.into(),
                title: format!("Session {conversation}"),
                created_at: Utc::now(),
            },
            Interaction {
                id: id.clone(),
                conversation_id: conversation.into(),
                parent_id: None,
                timestamp: Utc::now(),
                author: Author::Agent,
                user_prompt: "recorded through MCP".into(),
                model_name: Some("agent".into()),
                model_cot: Some("reasoning".into()),
                model_response: None,
                source_request_id: None,
            },
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    id
}

/// Runs `cvc` with `args`, answers its one `I SHARE` phrase, and returns what
/// it printed.
fn share(fixture: &Fixture, args: &[&str]) -> String {
    let mut session = PtySession::spawn(fixture.cvc_command(args), b"");
    let challenge = session.wait_for_challenge(1);
    assert!(challenge.starts_with("I SHARE "), "{challenge}");
    session.write(format!("{challenge}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    output
}

#[test]
fn sharing_the_future_of_a_conversation_with_mcp_thoughts_says_later_ones_stay_private() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let earlier = record_through_mcp(&fixture, "mcp-session");

    let output = share(
        &fixture,
        &["share", "mcp-session", "--remote", "origin", "--future"],
    );
    let note = output.find(NOTE).unwrap_or_else(|| panic!("{output}"));
    let phrase = output.find("Type exactly 'I SHARE ").unwrap();
    assert!(note < phrase, "the note comes before the phrase: {output}");
    assert!(
        output.contains("1 thought(s) marked shared; future thoughts are shared."),
        "{output}"
    );

    // The share covered the thought already there; a later one stays private.
    let later = record_through_mcp(&fixture, "mcp-session");
    let destination =
        cvc_core::privacy::remote_destination(&fixture.open_repo(), "origin").unwrap();
    let shared = CvcStore::open(fixture.index_db_path())
        .unwrap()
        .projection_interaction_ids(&destination.fingerprint)
        .unwrap();
    assert_eq!(shared, vec![earlier]);
    assert!(!shared.contains(&later));
}

#[test]
fn the_note_appears_only_when_future_sharing_meets_mcp_thoughts() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    record_through_mcp(&fixture, "mcp-session");

    // A snapshot share records no future sharing, so it has nothing to say.
    let output = share(&fixture, &["share", "mcp-session", "--remote", "origin"]);
    assert!(!output.contains("recorded through MCP"), "{output}");

    // Future sharing of a conversation without MCP thoughts says nothing either.
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    let run = fixture.only_run_conversation_id();
    let output = share(&fixture, &["share", &run, "--remote", "origin", "--future"]);
    assert!(!output.contains("recorded through MCP"), "{output}");
    assert!(
        output.contains("1 thought(s) marked shared; future thoughts are shared."),
        "{output}"
    );
}
