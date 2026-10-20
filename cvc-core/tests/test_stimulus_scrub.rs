//! How the version 2 stimulus meets the scrubber: part by part before it is
//! bounded, around the markers when it is scrubbed again, and whole when it is
//! not a verified stimulus.
use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::{Author, Conversation, FutureSharePolicy, Interaction, InteractionId};
use cvc_core::privacy::{final_scrub_for_sync, scrub, McpCapture, PreparedPolicy, PrivacyError};
use cvc_core::stimulus::{
    self, notes_in, read, NoteKind, PartKind, Reading, StimulusPart, MAX_STIMULUS_BYTES,
};
use cvc_core::sync;
use git2::Repository;
use std::fs;
use tempfile::tempdir;

const ID: &str = "3c2b1a09-8f7e-8d6c-9b5a-4f3e2d1c0b9a";
const DESTINATION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const FAKE_KEY: &str = "-----BEGIN RSA PRIVATE KEY-----\nMIIEfakeFAKEfake0123456789\nKEYBODYLINE0123456789abcdefghijklmnop\n-----END RSA PRIVATE KEY-----";

fn id() -> InteractionId {
    ID.parse().unwrap()
}

fn built_in(text: &str) -> Result<String, PrivacyError> {
    scrub(text)
}

/// Passes text through untouched, as a writer that scrubbed nothing would.
fn unscrubbed(text: &str) -> Result<String, PrivacyError> {
    Ok(text.to_owned())
}

fn render(parts: &[StimulusPart]) -> String {
    stimulus::render(&id(), parts, &built_in)
}

fn tool(name: &str, text: &str) -> StimulusPart {
    StimulusPart::tool_result(Some(name), false, text)
}

fn contents(text: &str) -> Vec<(PartKind, String)> {
    match read(&id(), text) {
        Reading::Verified(verified) => verified
            .parts
            .iter()
            .map(|part| (part.kind, part.content_of(text).to_owned()))
            .collect(),
        other => panic!("stimulus did not verify: {other:?}\n{text}"),
    }
}

/// The text with each redaction marker's random id replaced, so that two
/// scrubs of the same input can be compared.
fn masked(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("[CVC_REDACTED:v1:") {
        out.push_str(&rest[..at]);
        let marker = &rest[at..];
        let end = marker.find(']').map_or(marker.len(), |end| end + 1);
        let colon = marker[..end].rfind(':').unwrap_or(0);
        out.push_str(&marker[..=colon]);
        out.push_str("*]");
        rest = &marker[end..];
    }
    out.push_str(rest);
    out
}

fn token(text: &str) -> String {
    stimulus::tokens_in(text).next().unwrap().to_owned()
}

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

fn capture(store: &CvcStore, interaction: Interaction) {
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: interaction.conversation_id.clone(),
                title: "conversation".into(),
                created_at: Utc::now(),
            },
            interaction,
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
}

// ---- scrubbed part by part, in full, before anything is cut --------------------

#[test]
fn an_assignment_ending_a_part_cannot_reach_the_next_marker() {
    // Each of these ends a part the way a secret assignment starts: the detector
    // reads on across the blank line and consumes the start of the next marker.
    for tail in [
        "the password:",
        "token=",
        "api_key:",
        "client secret =",
        "aws_secret_access_key=",
    ] {
        let text = render(&[
            tool("Bash", &format!("end {tail}")),
            tool("Read", "next output"),
        ]);
        assert_ne!(
            scrub(&text).unwrap(),
            text,
            "{tail}: scrubbed whole, the marker is damaged"
        );
        // A verified stimulus is scrubbed part by part, so nothing changes.
        assert_eq!(
            stimulus::scrub_stimulus(&id(), &text, &built_in).unwrap(),
            text,
            "{tail}"
        );
        assert_eq!(contents(&text).len(), 2, "{tail}");
    }
}

#[test]
fn a_key_block_spanning_two_parts_cannot_swallow_the_marker_between_them() {
    let first = "output\n-----BEGIN RSA PRIVATE KEY-----\nMIIEfakeFAKEfake0123456789\n";
    let second = "KEYBODYLINE0123456789abcdefghijklmnop\n-----END RSA PRIVATE KEY-----\ndone";
    let parts = [tool("Bash", first), tool("Read", second)];
    // As one string the block runs from the first part's header to the second
    // part's footer and takes the second marker with it.
    let joined = format!("{first}\n\n[tool_result Read ~cvc-0123456789abcdef] {second}");
    assert!(!scrub(&joined).unwrap().contains("[tool_result Read"));
    let text = render(&parts);
    let read_back = contents(&text);
    assert_eq!(read_back.len(), 2);
    assert_eq!(read_back[1].0, PartKind::ToolResult);
    // The part holding the header is redacted from the header to the end of the
    // key material after it, so none of that key's material is kept...
    assert_eq!(masked(&read_back[0].1), "output\n[CVC_REDACTED:v1:pem:*]\n");
    assert!(!text.contains("MIIEfake"));
    // ...and the second part is scrubbed on its own terms, its marker untouched:
    // the key text before its END line goes with the line, and what follows stays.
    assert_eq!(masked(&read_back[1].1), "[CVC_REDACTED:v1:pem:*]\ndone");
    assert!(!text.contains("KEYBODYLINE"));
    assert!(text.contains("[tool_result Read "));
}

#[test]
fn a_key_straddling_the_head_cut_is_redacted_before_cutting() {
    let body = "KEYBODYLINE0123456789abcdefghijklmnopqrstuvwxyz\n".repeat(60);
    let original = format!(
        "{}\n-----BEGIN PRIVATE KEY-----\n{body}-----END PRIVATE KEY-----\n{}",
        "a".repeat(5_500),
        "b".repeat(4_000)
    );
    // The key starts inside the 6 KiB head and ends beyond it.
    assert!(original[..6 * 1024].contains("KEYBODYLINE"));
    let text = render(&[tool("Bash", &original)]);
    assert!(
        !text.contains("KEYBODYLINE"),
        "key material reached the capture"
    );
    assert!(!text.contains("BEGIN PRIVATE KEY"));
    assert!(text.contains("[CVC_REDACTED:v1:pem:"));
    // The cut still happened, around the marker.
    let b = token(&text);
    assert!(!notes_in(&contents(&text)[0].1, &b).is_empty());
}

#[test]
fn a_token_at_the_cut_is_redacted_whole_not_kept_in_part() {
    let key = "AKIAABCDEFGHIJKLMNOP";
    for offset in [6 * 1024 - 5, 6 * 1024 - 10, 6 * 1024 - 20, 6 * 1024 - 1] {
        let original = format!("{}{key}{}", "x ".repeat(offset / 2), " y".repeat(5_000));
        let text = render(&[tool("Bash", &original)]);
        assert!(
            !text.contains("AKIAABCD"),
            "offset {offset}: a prefix of the key survived"
        );
    }
}

#[test]
fn an_unterminated_key_is_redacted_to_the_end_of_its_part() {
    // The key material runs to the end of the part, so the redaction does too.
    let text = render(&[
        tool(
            "Bash",
            "log line\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nbm9uZQAAAAAAAAABAAAAMwAAAAtz",
        ),
        StimulusPart::prompt("and a person after it"),
    ]);
    let read_back = contents(&text);
    assert!(read_back[0]
        .1
        .starts_with("log line\n[CVC_REDACTED:v1:pem:"));
    assert!(read_back[0].1.ends_with(']'));
    assert!(!text.contains("b3BlbnNzaC1"));
    assert!(!text.contains("bm9uZQAAAA"));
    assert_eq!(read_back[1].1, "and a person after it");
}

#[test]
fn an_unterminated_key_redacts_its_key_text_and_keeps_the_prose_after_it() {
    let text = render(&[
        tool(
            "Bash",
            "log line\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nthe key stops above, this is prose",
        ),
        tool(
            "Read",
            "the file must start with -----BEGIN RSA PRIVATE KEY----- and a body",
        ),
    ]);
    let read_back = contents(&text);
    assert_eq!(
        masked(&read_back[0].1),
        "log line\n[CVC_REDACTED:v1:pem:*]\nthe key stops above, this is prose"
    );
    // A header that is only mentioned costs the part nothing.
    assert_eq!(
        read_back[1].1,
        "the file must start with -----BEGIN RSA PRIVATE KEY----- and a body"
    );
}

#[test]
fn a_part_with_too_many_matches_is_withheld_and_the_rest_proceed() {
    let noisy: String = (0..300)
        .map(|index| format!("password=hunter2hunter{index}\n"))
        .collect();
    let text = render(&[
        StimulusPart::prompt("run it"),
        tool("Bash", &noisy),
        tool("Read", "fine"),
    ]);
    let b = token(&text);
    let read_back = contents(&text);
    assert_eq!(read_back[0].1, "run it");
    assert_eq!(read_back[1].1, format!("[cvc: withheld {b}]"));
    assert_eq!(read_back[2].1, "fine");
    assert!(!text.contains("hunter2"));
    assert_eq!(notes_in(&read_back[1].1, &b)[0].kind, NoteKind::Withheld);
    // The same noisy text scrubbed whole is an error: that is why it is a part.
    assert!(scrub(&noisy).is_err());
}

#[test]
fn a_policy_literal_matching_a_server_name_redacts_the_name_and_the_text() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(".thoughtignore"), "literal:acme-internal\n").unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    let parts = [
        tool(
            "mcp__acme-internal__lookup",
            "found host acme-internal.example",
        ),
        tool("mcp__other__lookup", "plain"),
    ];
    let text = stimulus::render(&id(), &parts, &|text| policy.scrub(text));
    assert!(text.starts_with("[tool_result redacted-tool "), "{text}");
    assert!(text.contains("[tool_result mcp__other__lookup "));
    assert!(!text.contains("acme-internal"));
    assert!(contents(&text)[0]
        .1
        .contains("[CVC_REDACTED:v1:policy_literal:"));
}

#[test]
fn a_part_too_large_to_scrub_is_withheld_and_so_is_one_past_the_scrub_budget() {
    // Over the scrubber's limit for one value: withheld, instantly.
    let text = render(&[
        tool("Bash", &"x".repeat(2 * 1024 * 1024)),
        tool("Read", "small"),
    ]);
    let b = token(&text);
    assert_eq!(contents(&text)[0].1, format!("[cvc: withheld {b}]"));
    assert_eq!(contents(&text)[1].1, "small");

    // One stimulus asks the scrubber to scan at most 8 MiB, and only bytes that
    // are scanned count. Eight parts of 1,000,000 bytes scan 8,000,000 of them,
    // so the ninth, which would take the stimulus past its budget, is withheld
    // without being read; a small part after it still fits and is scrubbed.
    let mut parts: Vec<StimulusPart> = (0..9)
        .map(|_| tool("Bash", &"x".repeat(1_000_000)))
        .collect();
    parts.push(tool("Read", "tiny"));
    let text = render(&parts);
    let b = token(&text);
    let read_back = contents(&text);
    assert_eq!(read_back[8].1, format!("[cvc: withheld {b}]"));
    assert_eq!(read_back[9].1, "tiny");
    assert!(read_back[7].1.starts_with("xxxx"), "a scanned part is kept");
    assert!(text.len() <= MAX_STIMULUS_BYTES);
}

#[test]
fn parts_the_scrubber_rejects_unread_are_not_charged_to_the_scrub_budget() {
    // Five parts of 1.5 MiB are each over the one-value limit: rejected without
    // being scanned, so they take nothing from the 8 MiB the stimulus may scan,
    // and the part after them is scrubbed.
    let mut parts: Vec<StimulusPart> = (0..5)
        .map(|_| tool("Bash", &"x".repeat(1_536 * 1024)))
        .collect();
    parts.push(tool("Read", &"y".repeat(600_000)));
    parts.push(tool("Read", "tiny"));
    let text = render(&parts);
    let b = token(&text);
    let read_back = contents(&text);
    for withheld in &read_back[..5] {
        assert_eq!(withheld.1, format!("[cvc: withheld {b}]"));
    }
    assert!(read_back[5].1.starts_with("yyyy"), "the next part was read");
    assert_eq!(read_back[6].1, "tiny");
}

#[test]
fn a_persons_words_are_scrubbed_first_so_tool_output_cannot_starve_them() {
    // One tool result of 8 MiB, rejected for its size. It used to be charged to
    // the budget anyway, which withheld everything after it.
    let huge = tool("Bash", &"x".repeat(8 * 1024 * 1024));
    let text = render(&[
        huge,
        StimulusPart::prompt("STOP. Do not push to main."),
        tool("Read", "after"),
    ]);
    let read_back = contents(&text);
    assert_eq!(read_back[1].0, PartKind::Prompt);
    assert_eq!(read_back[1].1, "STOP. Do not push to main.");
    assert_eq!(read_back[2].1, "after");

    // Tool results that are all scanned and use the whole budget: the person's
    // words, which come last in the stimulus, are scrubbed before any of them.
    let mut parts: Vec<StimulusPart> = (0..9)
        .map(|_| tool("Bash", &"x".repeat(1_000_000)))
        .collect();
    parts.push(StimulusPart::prompt("STOP. Do not push to main."));
    parts.push(StimulusPart::task("A task for the subagent"));
    let text = render(&parts);
    let b = token(&text);
    let read_back = contents(&text);
    assert_eq!(read_back[9].1, "STOP. Do not push to main.");
    assert_eq!(read_back[10].1, "A task for the subagent");
    // The budget did run out on tool output: the ninth was withheld unread.
    assert_eq!(read_back[8].1, format!("[cvc: withheld {b}]"));
    // And the order of the parts in the stimulus is the order they came in.
    assert_eq!(read_back[9].0, PartKind::Prompt);
    assert_eq!(read_back[10].0, PartKind::Task);
}

// ---- scrubbed again around the markers ---------------------------------------

#[test]
fn scrubbing_a_verified_stimulus_again_changes_nothing() {
    let text = render(&[
        StimulusPart::prompt("deploy with care, key AKIAABCDEFGHIJKLMNOP"),
        tool("Bash", &"x".repeat(30_000)),
        tool("Read", "the password:"),
        StimulusPart::compaction_summary("summary"),
    ]);
    // The first scrub redacted the key; the stimulus is bounded and has a note.
    assert!(text.contains("[CVC_REDACTED:v1:aws_access_key:"));
    assert!(!notes_in(&contents(&text)[1].1, &token(&text)).is_empty());
    // Scrubbing it again leaves it byte for byte as it was: the existing
    // redaction marker is not redacted again and no marker or note is touched.
    for scrubbed in [
        stimulus::scrub_stimulus(&id(), &text, &built_in).unwrap(),
        stimulus::scrub_stimulus(&id(), &text, &|text| {
            PreparedPolicy::built_ins_only().scrub(text)
        })
        .unwrap(),
    ] {
        assert_eq!(scrubbed, text);
    }
}

#[test]
fn scrubbing_again_redacts_inside_a_part_and_keeps_the_structure() {
    // Text written by a writer that scrubbed nothing.
    let parts = [
        StimulusPart::prompt("use key AKIAABCDEFGHIJKLMNOP"),
        tool("Bash", &format!("{}\n{FAKE_KEY}\nafter", "a".repeat(9_000))),
        tool("Read", "clean"),
    ];
    let text = stimulus::render(&id(), &parts, &unscrubbed);
    assert!(text.contains("AKIAABCDEFGHIJKLMNOP"));
    let scrubbed = stimulus::scrub_stimulus(&id(), &text, &built_in).unwrap();
    assert!(!scrubbed.contains("AKIAABCDEFGHIJKLMNOP"));
    assert!(!scrubbed.contains("KEYBODYLINE"));
    // Still one verified stimulus, with the same parts, notes and boundary.
    let before = contents(&text);
    let after = contents(&scrubbed);
    assert_eq!(after.len(), 3);
    assert_eq!(token(&scrubbed), token(&text));
    assert_eq!(after[2], before[2]);
    assert!(after[0].1.contains("[CVC_REDACTED:v1:aws_access_key:"));
    assert_eq!(
        notes_in(&after[1].1, &token(&scrubbed)).len(),
        notes_in(&before[1].1, &token(&text)).len()
    );
}

#[test]
fn a_part_that_cannot_be_scrubbed_again_is_withheld_not_fatal() {
    let noisy: String = (0..300)
        .map(|index| format!("password=hunter2hunter{index}\n"))
        .collect();
    let text = stimulus::render(
        &id(),
        &[tool("Bash", &noisy), StimulusPart::prompt("kept")],
        &unscrubbed,
    );
    let scrubbed = stimulus::scrub_stimulus(&id(), &text, &built_in).unwrap();
    let b = token(&scrubbed);
    let read_back = contents(&scrubbed);
    assert_eq!(read_back[0].1, format!("[cvc: withheld {b}]"));
    assert_eq!(read_back[1].1, "kept");
}

#[test]
fn legacy_and_unverified_text_is_scrubbed_whole_as_before() {
    let v1 = "Add a greeting to lib.rs\n\n[meta] <system-reminder>x</system-reminder>\n\n[tool_result Bash] AKIAABCDEFGHIJKLMNOP";
    assert_eq!(read(&id(), v1), Reading::Legacy);
    let scrubbed = stimulus::scrub_stimulus(&id(), v1, &built_in).unwrap();
    assert_eq!(masked(&scrubbed), masked(&scrub(v1).unwrap()));
    assert!(scrubbed.starts_with("Add a greeting to lib.rs\n\n[meta] <system-reminder>x"));
    assert!(!scrubbed.contains("AKIA"));

    // A lookalike that does not verify is scrubbed whole too, errors included.
    let lookalike = "[prompt ~cvc-0123456789abcdef] AKIAABCDEFGHIJKLMNOP";
    assert_eq!(read(&id(), lookalike), Reading::Undivided);
    assert_eq!(
        masked(&stimulus::scrub_stimulus(&id(), lookalike, &built_in).unwrap()),
        masked(&scrub(lookalike).unwrap())
    );
    let noisy: String = (0..300)
        .map(|index| format!("password=hunter2hunter{index}\n"))
        .collect();
    assert!(stimulus::scrub_stimulus(&id(), &noisy, &built_in).is_err());
}

#[test]
fn the_capture_boundary_keeps_a_verified_stimulus_verified_and_scrubs_its_parts() {
    let dir = tempdir().unwrap();
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let text = stimulus::render(
        &id(),
        &[
            tool("Bash", "the password:"),
            tool("Read", "key AKIAABCDEFGHIJKLMNOP here"),
            StimulusPart::prompt("a person"),
        ],
        &unscrubbed,
    );
    capture(&store, interaction(&id(), text.clone()));
    let stored = store.get_interaction(&id()).unwrap().unwrap().user_prompt;
    assert_ne!(stored, text);
    assert!(!stored.contains("AKIAABCDEFGHIJKLMNOP"));
    let read_back = contents(&stored);
    assert_eq!(read_back.len(), 3);
    assert_eq!(read_back[0].1, "the password:");
    assert_eq!(read_back[2].1, "a person");
    assert_eq!(token(&stored), token(&text));
}

#[test]
fn a_version_1_row_is_stored_exactly_as_before() {
    let dir = tempdir().unwrap();
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let v1 = "Fix it\n\n[tool_result Bash] ok\n\n[tool_result Read error] boom";
    let other = InteractionId::new();
    capture(&store, interaction(&other, v1.to_owned()));
    assert_eq!(
        store.get_interaction(&other).unwrap().unwrap().user_prompt,
        v1
    );
}

#[test]
fn final_scrub_for_sync_keeps_a_verified_stimulus_verified() {
    let text = render(&[
        tool("Bash", "the password:"),
        tool("Read", "next"),
        StimulusPart::prompt("then"),
    ]);
    let mut node = interaction(&id(), text.clone());
    final_scrub_for_sync(&mut node, &mut [], &mut []).unwrap();
    assert_eq!(node.user_prompt, text);
    // A stimulus that does not verify is scrubbed whole, as always.
    let mut other = interaction(&InteractionId::new(), text.clone());
    final_scrub_for_sync(&mut other, &mut [], &mut []).unwrap();
    assert_ne!(other.user_prompt, text);
}

#[test]
fn projection_and_import_keep_a_verified_stimulus_verified() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let source = CvcStore::open(dir.path().join("source.db")).unwrap();
    // A part that ends in a keyword, which a whole-string check would trip on.
    let text = render(&[
        tool("Bash", "the password:"),
        tool("Read", &"y".repeat(20_000)),
        StimulusPart::prompt("and a person"),
    ]);
    capture(&source, interaction(&id(), text.clone()));
    source
        .share_conversation_for_remote("conversation", DESTINATION, FutureSharePolicy::Private)
        .unwrap();
    let sync::ProjectionResult::Candidate { oid, candidate, .. } =
        sync::push_projection_to_ref(&repo, &source, "", DESTINATION).unwrap()
    else {
        panic!("expected a projection")
    };
    repo.reference("refs/cvc/stimulus-source", oid, true, "test")
        .unwrap();
    drop(candidate);

    let imported = CvcStore::open(dir.path().join("imported.db")).unwrap();
    sync::pull_from_ref(&repo, &imported, "refs/cvc/stimulus-source").unwrap();
    let pulled = imported
        .get_interaction(&id())
        .unwrap()
        .unwrap()
        .user_prompt;
    assert_eq!(pulled, text);
    assert_eq!(contents(&pulled).len(), 3);
}

// ---- the name a marker carries is text too -------------------------------------

const SECRET_NAME: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

/// A stimulus as a writer that scrubbed nothing, or a crafted node, would hold
/// it: secrets in the names its markers carry.
fn stimulus_with_secret_names() -> String {
    stimulus::render(
        &id(),
        &[
            StimulusPart::tool_result(Some(SECRET_NAME), false, "output"),
            StimulusPart::tool_result(Some("Bash"), true, "boom"),
            StimulusPart::tool_result(Some(SECRET_NAME), true, "failed"),
            StimulusPart::prompt("then a person"),
        ],
        &unscrubbed,
    )
}

#[test]
fn a_secret_in_a_marker_name_is_rewritten_when_a_stimulus_is_scrubbed_again() {
    let text = stimulus_with_secret_names();
    assert!(text.contains(&format!("[tool_result {SECRET_NAME} ")));
    // As it stands it verifies: names are not part of what the boundary covers.
    let before = contents(&text);
    assert_eq!(before.len(), 4);

    let scrubbed = stimulus::scrub_stimulus(&id(), &text, &built_in).unwrap();
    let b = token(&text);
    assert!(!scrubbed.contains("ghp_"), "{scrubbed}");
    assert!(scrubbed.starts_with(&format!("[tool_result redacted-tool {b}] output")));
    assert!(scrubbed.contains(&format!("[tool_result Bash error {b}] boom")));
    assert!(scrubbed.contains(&format!("[tool_result redacted-tool error {b}] failed")));
    // Still the same verified stimulus, part for part.
    assert_eq!(contents(&scrubbed), before);
    assert_eq!(token(&scrubbed), b);
    // And scrubbing it again changes nothing.
    assert_eq!(
        stimulus::scrub_stimulus(&id(), &scrubbed, &built_in).unwrap(),
        scrubbed
    );
}

#[test]
fn a_clean_marker_name_is_kept_byte_for_byte() {
    let text = render(&[
        tool("Bash", "one"),
        tool("mcp__github__create_issue", "two"),
        StimulusPart::tool_result(Some("Read"), true, "three"),
    ]);
    assert_eq!(
        stimulus::scrub_stimulus(&id(), &text, &built_in).unwrap(),
        text
    );
}

#[test]
fn a_policy_literal_in_a_marker_name_is_rewritten_too() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(".thoughtignore"), "literal:acme-internal\n").unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    // The name was valid and unredacted when it was written, before the rule.
    let text = stimulus::render(
        &id(),
        &[
            tool("mcp__acme-internal__lookup", "found it"),
            tool("Read", "plain"),
        ],
        &unscrubbed,
    );
    let scrubbed = stimulus::scrub_stimulus(&id(), &text, &|text| policy.scrub(text)).unwrap();
    assert!(
        scrubbed.starts_with("[tool_result redacted-tool "),
        "{scrubbed}"
    );
    assert!(scrubbed.contains("[tool_result Read "));
    assert!(!scrubbed.contains("acme-internal"));
    assert_eq!(contents(&scrubbed), contents(&text));
}

#[test]
fn final_scrub_for_sync_rewrites_a_secret_marker_name() {
    let text = stimulus_with_secret_names();
    let mut node = interaction(&id(), text.clone());
    final_scrub_for_sync(&mut node, &mut [], &mut []).unwrap();
    assert!(!node.user_prompt.contains("ghp_"));
    assert!(node
        .user_prompt
        .contains(&format!("[tool_result redacted-tool {}]", token(&text))));
    assert_eq!(contents(&node.user_prompt), contents(&text));
}
