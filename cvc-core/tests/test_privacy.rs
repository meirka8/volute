use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::FutureSharePolicy;
use cvc_core::models::{
    Author, ContextItem, Conversation, Interaction, InteractionId, ToolExecution, ToolStatus,
};
use cvc_core::privacy::{
    acknowledge_sharing, has_unterminated_private_key_block, privacy_status, scrub, scrub_json,
    validate_sanitized_json, CliRunCapture, McpCapture, PreparedPolicy,
};
use cvc_core::sync;
use git2::Repository;
use tempfile::tempdir;

/// "No remote baseline": pins a tracking ref that does not exist, so the
/// projection starts from an empty tree and verifies that nothing appeared.
fn no_baseline(repo: &Repository) -> sync::ImportedBaseline {
    sync::ImportedBaseline::observe(repo, "refs/remotes/unpublished/cvc/main").unwrap()
}

fn project_fixture_to_ref(repo: &Repository, store: &CvcStore, ref_name: &str, destination: &str) {
    let projection =
        sync::push_projection_to_ref(repo, store, &no_baseline(repo), destination).unwrap();
    let sync::ProjectionResult::Candidate { oid, candidate, .. } = projection else {
        panic!("fixture expected projection candidate");
    };
    repo.reference(ref_name, oid, true, "fixture: remote accepted candidate")
        .unwrap();
    drop(candidate);
}

fn interaction(id: InteractionId, conversation_id: &str) -> Interaction {
    Interaction {
        id,
        conversation_id: conversation_id.into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "sk-abcdefghijklmnopqrstuvwxyz123456".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    }
}

#[test]
fn scrub_covers_sensitive_forms() {
    assert!(!scrub("AKIAABCDEFGHIJKLMNOP secret=supersecretvalue")
        .unwrap()
        .contains("supersecretvalue"));
    assert_eq!(
        scrub("0123456789abcdef0123456789abcdef01234567").unwrap(),
        "0123456789abcdef0123456789abcdef01234567"
    );
}

/// The text with each redaction marker's random id replaced, so that scrubs of
/// the same input can be compared.
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

const KEY_BODY: &str = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7\nVJTUt9Us8cKjMzEfYyjiWA4R4/M2bS1GB4t7NXp98C3SC6dVMvDuictGeurT\n";

/// Sixty-four characters, the width of a PEM body line.
const LINE_64: &str = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7VJTUt9Us8cKj";

#[test]
fn a_lone_header_redacts_the_key_material_after_it_and_keeps_the_text_that_follows() {
    for kind in [
        "PRIVATE KEY",
        "RSA PRIVATE KEY",
        "EC PRIVATE KEY",
        "DSA PRIVATE KEY",
        "OPENSSH PRIVATE KEY",
        "ENCRYPTED PRIVATE KEY",
    ] {
        let text =
            format!("before\n-----BEGIN {kind}-----\n{KEY_BODY}and a sentence after the key lines");
        let scrubbed = scrub(&text).unwrap();
        assert_eq!(
            masked(&scrubbed),
            "before\n[CVC_REDACTED:v1:pem:*]\nand a sentence after the key lines",
            "{kind}"
        );
        assert!(
            !scrubbed.contains("MIIEvQ") && !scrubbed.contains("BEGIN"),
            "{kind}"
        );
    }
}

#[test]
fn key_material_that_runs_to_the_end_of_the_text_is_redacted_to_the_end() {
    // Cut mid-line, as a size bound or a tool leaves it.
    let cut = format!("x\n-----BEGIN PRIVATE KEY-----\n{LINE_64}\nVJTUt9Us8cKjMzEfYy");
    assert_eq!(masked(&scrub(&cut).unwrap()), "x\n[CVC_REDACTED:v1:pem:*]");
    // Cut after a whole line: the break after it is not key material.
    let whole = format!("x\n-----BEGIN PRIVATE KEY-----\n{KEY_BODY}");
    assert_eq!(
        masked(&scrub(&whole).unwrap()),
        "x\n[CVC_REDACTED:v1:pem:*]\n"
    );
    // Everything on one line, the body right behind the header.
    let one_line = format!("-----BEGIN PRIVATE KEY-----{LINE_64}{LINE_64}");
    assert_eq!(
        masked(&scrub(&one_line).unwrap()),
        "[CVC_REDACTED:v1:pem:*]"
    );
}

#[test]
fn a_header_that_is_only_mentioned_keeps_the_text_around_it() {
    for text in [
        "A key file must start with -----BEGIN RSA PRIVATE KEY----- and end with its END line.",
        "The header is -----BEGIN PRIVATE KEY-----\nThe next line is ordinary text, not a key.",
        "Format:\n-----BEGIN EC PRIVATE KEY-----\nNote: do not share this file",
        "-----BEGIN PRIVATE KEY-----",
        "-----BEGIN PRIVATE KEY-----\nMIIE",
        "if line == \"-----BEGIN PRIVATE KEY-----\" {\n    start()\n}",
        // A rule of equals signs is not key text.
        "-----BEGIN PRIVATE KEY-----\n================================\nafter",
    ] {
        assert_eq!(scrub(text).unwrap(), text, "{text}");
        assert!(!has_unterminated_private_key_block(text), "{text}");
    }
}

#[test]
fn an_end_line_with_key_text_before_it_redacts_both_and_keeps_the_rest() {
    let text = format!("the end of the key:\n{LINE_64}\nQ2VudCB0aGUgZW5k==\n-----END RSA PRIVATE KEY-----\nnext line");
    assert_eq!(
        masked(&scrub(&text).unwrap()),
        "the end of the key:\n[CVC_REDACTED:v1:pem:*]\nnext line"
    );
    // A body shown from the middle: the text starts inside it.
    let tail = format!("XYZabc12\n{LINE_64}\n{LINE_64}\n-----END PRIVATE KEY-----\ndone");
    assert_eq!(
        masked(&scrub(&tail).unwrap()),
        "[CVC_REDACTED:v1:pem:*]\ndone"
    );
    // The same written with escaped line breaks.
    let escaped =
        format!(r#"{{"out":"{LINE_64}\n{LINE_64}\nabc=\n-----END PRIVATE KEY-----\n","n":1}}"#);
    assert_eq!(
        masked(&scrub(&escaped).unwrap()),
        r#"{"out":"[CVC_REDACTED:v1:pem:*]\n","n":1}"#
    );
}

#[test]
fn an_end_line_that_is_mentioned_or_has_no_key_text_before_it_is_left_alone() {
    for text in [
        "The block ends with -----END PRIVATE KEY-----.",
        "just text\n-----END PRIVATE KEY-----\nmore",
        "-----END PRIVATE KEY-----",
        "\n-----END PRIVATE KEY-----\n",
        // A blank line between the key text and the END line breaks the run.
        "AAAAAAAAAAAAAAAAAAAAAAAA\n\n-----END PRIVATE KEY-----",
    ] {
        assert_eq!(scrub(text).unwrap(), text, "{text}");
        assert!(!has_unterminated_private_key_block(text), "{text}");
    }
}

#[test]
fn escaped_line_breaks_are_line_breaks_too() {
    let text = format!(
        r#"{{"out":"-----BEGIN PRIVATE KEY-----\n{LINE_64}\n{LINE_64}\nB4t7=\n","next":"x"}}"#
    );
    assert_eq!(
        masked(&scrub(&text).unwrap()),
        r#"{"out":"[CVC_REDACTED:v1:pem:*]\n","next":"x"}"#
    );
}

#[test]
fn a_key_inside_a_quoted_string_ends_where_the_string_does() {
    let text = format!(
        r#"{{"out":"-----BEGIN PRIVATE KEY-----\n{LINE_64}\n{LINE_64}\nB4t7=","next":"x"}}"#
    );
    assert_eq!(
        masked(&scrub(&text).unwrap()),
        r#"{"out":"[CVC_REDACTED:v1:pem:*]","next":"x"}"#
    );
    // A short quoted word before an END line is not key text.
    let word = "she said \"hello\n-----END PRIVATE KEY-----";
    assert_eq!(scrub(word).unwrap(), word);
    // Nor is a quote that closes with no key text in front of it.
    let closed = format!("-----BEGIN PRIVATE KEY-----\n{LINE_64}\n\"after\"");
    assert_eq!(
        masked(&scrub(&closed).unwrap()),
        "[CVC_REDACTED:v1:pem:*]\n\"after\""
    );
}

#[test]
fn lines_numbered_by_the_read_tool_or_cat_are_key_lines_with_their_numbers() {
    // What the Read tool prints for a range that starts inside a key: every
    // line has its number, a tab, and then the line.
    let tab = format!(
        "    57\t{LINE_64}\n    58\t{LINE_64}\n    59\tQ2VudCB0aGUgZW5k==\n    60\t-----END PRIVATE KEY-----\n    61\tnext line"
    );
    assert_eq!(
        masked(&scrub(&tab).unwrap()),
        "    [CVC_REDACTED:v1:pem:*]\n    61\tnext line"
    );
    assert!(!scrub(&tab).unwrap().contains("MIIEvQ"));
    // The arrow form, and the other separators.
    for separator in ["→", "|", ":"] {
        let numbered = format!(
            "  7{separator}{LINE_64}\n  8{separator} {LINE_64}\n  9{separator}-----END RSA PRIVATE KEY-----\nafter"
        );
        assert_eq!(
            masked(&scrub(&numbered).unwrap()),
            "  [CVC_REDACTED:v1:pem:*]\nafter",
            "{separator}"
        );
    }
    // A header at the top of a numbered range, then its numbered body.
    let header = format!(
        "     1\t-----BEGIN PRIVATE KEY-----\n     2\t{LINE_64}\n     3\t{LINE_64}\n     4\tfollowing prose"
    );
    assert_eq!(
        masked(&scrub(&header).unwrap()),
        "     1\t[CVC_REDACTED:v1:pem:*]\n     4\tfollowing prose"
    );
    // Armor and legacy-encryption lines are numbered too, and so are blank ones.
    let armored = format!(
        "  1\t-----BEGIN RSA PRIVATE KEY-----\n  2\tProc-Type: 4,ENCRYPTED\n  3\t\n  4\t{LINE_64}\n  5\tend of file"
    );
    assert_eq!(
        masked(&scrub(&armored).unwrap()),
        "  1\t[CVC_REDACTED:v1:pem:*]\n  5\tend of file"
    );
    // Numbers alone are not key text, and an END line quoted after a number in
    // prose is not at the start of its line.
    for text in [
        "    5\t-----END PRIVATE KEY-----\n    6\tnothing before it",
        "    4\t\n    5\t-----END PRIVATE KEY-----",
        "    12\tThe header ends with -----END PRIVATE KEY-----",
        "1234567890123\tx\n-----END PRIVATE KEY-----",
    ] {
        assert_eq!(scrub(text).unwrap(), text, "{text}");
    }
}

#[test]
fn key_lines_in_a_diff_or_a_quotation_are_key_lines() {
    // The end of a key in a unified diff: a removed, an added and a context line
    // of key text, then the END line as a removed line (six dashes), an added one
    // (a plus sign), a context one (a space), or a quoted one.
    for footer in [
        "------END PRIVATE KEY-----",
        "+-----END PRIVATE KEY-----",
        " -----END PRIVATE KEY-----",
        "> -----END PRIVATE KEY-----",
    ] {
        let diff =
            format!("@@ -1,4 +1,4 @@\n-{LINE_64}\n+{LINE_64}\n {LINE_64}\n{footer}\n context\n");
        let scrubbed = scrub(&diff).unwrap();
        assert!(!scrubbed.contains("MIIEvQ"), "{footer}: {scrubbed}");
        assert_eq!(
            masked(&scrubbed),
            "@@ -1,4 +1,4 @@\n[CVC_REDACTED:v1:pem:*]\n context\n",
            "{footer}"
        );
    }
    // A quoted reply that cuts a key short after its header.
    let quoted = format!("> -----BEGIN PRIVATE KEY-----\n> {LINE_64}\n> {LINE_64}\n>\nreply text");
    let scrubbed = scrub(&quoted).unwrap();
    assert!(!scrubbed.contains("MIIEvQ"), "{scrubbed}");
    assert!(scrubbed.ends_with("reply text"));
    // A dashed rule or a bullet is not key text.
    for text in [
        "-----BEGIN PRIVATE KEY-----\n----------------------------------\nafter",
        "-----END PRIVATE KEY-----\n- item one\n- item two",
        "-- a signature\n-----END PRIVATE KEY-----",
    ] {
        assert_eq!(scrub(text).unwrap(), text, "{text}");
    }
}

// ---- the layouts a cut key arrives in -------------------------------------------------
//
// Lines of base64 are the common case and are read as lines. A key printed another
// way (with a file name in front of each line, in a list of strings, inside a string
// inside a string, between HTML breaks, run together with spaces) is read as lines
// too where its layout is one the scanner knows, and otherwise as long base64
// tokens, so that none of it is left behind.

const BODY_B: &str = "N7ITxKLUkX0vyOYx6tzS+nWQ36xgB0Oz5eGSQw4zzZ/VZtQb1wMhDp9kY2uRS66M";
const BODY_C: &str = "DMqZmJeOHNKXceA9P5Nd+RbOmmgsmCV6ZKuaY7NvBK/YoJcXJUPt25nNlHHHlXQd";
/// The short last line of a body, with its padding.
const TAIL: &str = "Q2VudCB0aGUgZW5k==";

/// Whether any of the three body lines, or twenty characters of one, is in `text`.
fn holds_body(text: &str) -> bool {
    [LINE_64, BODY_B, BODY_C].iter().any(|line| {
        text.contains(&line[..20]) || text.contains(&line[20..40]) || text.contains(line)
    })
}

#[test]
fn grep_and_rg_output_with_file_names_is_key_text_behind_its_prefixes() {
    let cases = [
        // `grep -n -A`: the matching line has colons, the lines of context hyphens.
        (
            format!("src/keys/server.pem:1:-----BEGIN PRIVATE KEY-----\nsrc/keys/server.pem-2-{LINE_64}\nsrc/keys/server.pem-3-{BODY_B}\nsrc/keys/server.pem-4-{TAIL}\nsrc/keys/server.pem-5-and some prose after it"),
            "src/keys/server.pem:1:[CVC_REDACTED:v1:pem:*]\nsrc/keys/server.pem-5-and some prose after it",
        ),
        // `rg` with a heading: the file once, then bare numbers.
        (
            format!("server.pem\n1:-----BEGIN PRIVATE KEY-----\n2-{LINE_64}\n3-{BODY_B}\n4-{TAIL}\n--\n9-other"),
            "server.pem\n1:[CVC_REDACTED:v1:pem:*]\n--\n9-other",
        ),
        // `rg --column`: a column after the line number.
        (
            format!("key.pem:1:1:-----BEGIN PRIVATE KEY-----\nkey.pem:2:1:{LINE_64}\nkey.pem:3:1:{BODY_B}\nkey.pem:4:1:and more"),
            "key.pem:1:1:[CVC_REDACTED:v1:pem:*]\nkey.pem:4:1:and more",
        ),
        // `grep -n -B` before an END line.
        (
            format!("src/keys/server.pem-57-{LINE_64}\nsrc/keys/server.pem-58-{BODY_B}\nsrc/keys/server.pem-59-{TAIL}\nsrc/keys/server.pem:60:-----END PRIVATE KEY-----\nsrc/keys/server.pem-61-after"),
            "[CVC_REDACTED:v1:pem:*]\nsrc/keys/server.pem-61-after",
        ),
        // A file name that holds numbers and hyphens of its own.
        (
            format!("logs/key-2024-01-01.pem-1-{LINE_64}\nlogs/key-2024-01-01.pem-2-{BODY_B}\nlogs/key-2024-01-01.pem:3:-----END PRIVATE KEY-----"),
            "[CVC_REDACTED:v1:pem:*]",
        ),
        // The END line itself as a line of context, behind a hyphen of its own.
        (
            format!("a.pem-1-{LINE_64}\na.pem-2-{BODY_B}\na.pem-3------END PRIVATE KEY-----\na.pem-4-after"),
            "[CVC_REDACTED:v1:pem:*]\na.pem-4-after",
        ),
    ];
    for (text, expected) in cases {
        let scrubbed = scrub(&text).unwrap();
        assert!(!holds_body(&scrubbed), "{text} -> {scrubbed}");
        assert_eq!(masked(&scrubbed), expected, "{text}");
        assert!(has_unterminated_private_key_block(&text), "{text}");
        assert_eq!(scrub(&scrubbed).unwrap(), scrubbed, "{text}");
    }
}

#[test]
fn a_prefix_that_is_not_a_line_number_does_not_make_prose_key_text() {
    for text in [
        // Words behind what looks like a file name and a number.
        "notes.txt-2-and then the header -----BEGIN PRIVATE KEY----- appears in a sentence",
        "notes.txt:2:-----BEGIN PRIVATE KEY-----\nnotes.txt-3-the next line is a sentence, not a key",
        "readme.md-10-the end is shown as\nreadme.md:11:-----END PRIVATE KEY-----\nreadme.md-12-then more prose",
        // A date is a number and a hyphen, and what follows it is words.
        "2026-10-20 the header -----BEGIN PRIVATE KEY----- was mentioned",
    ] {
        assert_eq!(scrub(text).unwrap(), text, "{text}");
    }
}

#[test]
fn a_list_of_the_lines_of_a_key_is_redacted_through_its_last_long_token() {
    for (text, expected) in [
        // A Python list of lines, cut inside it.
        (
            format!(
                r"lines = ['-----BEGIN PRIVATE KEY-----\n', '{LINE_64}\n', '{BODY_B}\n', '{BODY_C}\n']"
            ),
            r"lines = ['[CVC_REDACTED:v1:pem:*]\n']",
        ),
        // The same as JSON, with double quotes.
        (
            format!(
                r#"["-----BEGIN RSA PRIVATE KEY-----\n","{LINE_64}\n","{BODY_B}\n","{BODY_C}\n"]"#
            ),
            r#"["[CVC_REDACTED:v1:pem:*]\n"]"#,
        ),
        // A list that ends with the END line.
        (
            format!(r"['{LINE_64}\n', '{BODY_B}\n', '{BODY_C}\n', '-----END PRIVATE KEY-----\n']"),
            r"['[CVC_REDACTED:v1:pem:*]\n']",
        ),
        (
            format!(r#"["{LINE_64}\n", "{BODY_B}\n", "-----END EC PRIVATE KEY-----\n"]"#),
            r#"["[CVC_REDACTED:v1:pem:*]\n"]"#,
        ),
    ] {
        let scrubbed = scrub(&text).unwrap();
        assert!(!holds_body(&scrubbed), "{text} -> {scrubbed}");
        assert_eq!(masked(&scrubbed), expected, "{text}");
        assert!(has_unterminated_private_key_block(&text), "{text}");
        assert_eq!(scrub(&scrubbed).unwrap(), scrubbed, "{text}");
    }
}

#[test]
fn a_key_inside_json_inside_json_is_redacted() {
    // A JSON document in a string of another one: the line breaks are escaped twice.
    for (text, expected) in [
        (
            format!(
                r#"{{"stdout":"{{\"key\":\"-----BEGIN PRIVATE KEY-----\\n{LINE_64}\\n{BODY_B}\\n{TAIL}\\n\"}}"}}"#
            ),
            r#"{"stdout":"{\"key\":\"[CVC_REDACTED:v1:pem:*]\\n\"}"}"#,
        ),
        (
            format!(
                r#"{{"stdout":"{{\"key\":\"{LINE_64}\\n{BODY_B}\\n{TAIL}\\n-----END PRIVATE KEY-----\\n\"}}"}}"#
            ),
            r#"{"stdout":"{\"key\":\"[CVC_REDACTED:v1:pem:*]\\n\"}"}"#,
        ),
    ] {
        let scrubbed = scrub(&text).unwrap();
        assert!(
            !holds_body(&scrubbed) && !scrubbed.contains(TAIL),
            "{text} -> {scrubbed}"
        );
        assert_eq!(masked(&scrubbed), expected, "{text}");
        assert_eq!(scrub(&scrubbed).unwrap(), scrubbed, "{text}");
    }
    // Three levels deep is no layout the lines are read in; the long tokens are redacted.
    let deeper = format!(
        r#"{{"a":"{{\"b\":\"{{\\\"c\\\":\\\"-----BEGIN PRIVATE KEY-----\\\\n{LINE_64}\\\\n{BODY_B}\\\\n{BODY_C}\\\\n\\\"}}\"}}"}}"#
    );
    let scrubbed = scrub(&deeper).unwrap();
    assert!(!holds_body(&scrubbed), "{scrubbed}");
    assert_eq!(scrub(&scrubbed).unwrap(), scrubbed);
}

#[test]
fn a_key_between_html_breaks_is_read_as_lines() {
    for (text, expected) in [
        (
            format!("-----BEGIN PRIVATE KEY-----<br>{LINE_64}<br>{BODY_B}<br>{TAIL}<br>after the key"),
            "[CVC_REDACTED:v1:pem:*]<br>after the key",
        ),
        (
            format!("<p>-----BEGIN PRIVATE KEY-----<br/>{LINE_64}<br/>{BODY_B}<br/>{TAIL}<br/>after the key</p>"),
            "<p>[CVC_REDACTED:v1:pem:*]<br/>after the key</p>",
        ),
        (
            format!("-----BEGIN PRIVATE KEY-----<BR />{LINE_64}<BR />{BODY_B}<BR />{TAIL}<BR />after it"),
            "[CVC_REDACTED:v1:pem:*]<BR />after it",
        ),
        // Each break followed by a newline.
        (
            format!("-----BEGIN PRIVATE KEY-----<br>\n{LINE_64}<br>\n{BODY_B}<br>\n{TAIL}<br>\nafter the key"),
            "[CVC_REDACTED:v1:pem:*]<br>\nafter the key",
        ),
        // Cut before the END line.
        (
            format!("{LINE_64}<br>{BODY_B}<br>{TAIL}<br>-----END PRIVATE KEY-----<br>after"),
            "[CVC_REDACTED:v1:pem:*]<br>after",
        ),
    ] {
        let scrubbed = scrub(&text).unwrap();
        assert!(!holds_body(&scrubbed) && !scrubbed.contains(TAIL), "{text} -> {scrubbed}");
        assert_eq!(masked(&scrubbed), expected, "{text}");
        assert_eq!(scrub(&scrubbed).unwrap(), scrubbed, "{text}");
    }
    // A tag that is not a break is no break.
    let text = format!("-----BEGIN PRIVATE KEY-----<break>and <b>bold</b> words {LINE_64}");
    assert!(
        !scrub(&text).unwrap().contains(LINE_64),
        "the long token is still found"
    );
}

#[test]
fn a_key_run_together_with_spaces_is_redacted_through_its_last_long_token() {
    for (text, expected) in [
        // As `echo $KEY` or an environment dump prints it.
        (
            format!("KEY=-----BEGIN PRIVATE KEY----- {LINE_64} {BODY_B} {BODY_C} next=1"),
            "KEY=[CVC_REDACTED:v1:pem:*] next=1",
        ),
        (
            format!("-----BEGIN OPENSSH PRIVATE KEY-----  {LINE_64}   {BODY_B}"),
            "[CVC_REDACTED:v1:pem:*]",
        ),
        // Cut before the END line, which is in the middle of the line.
        (
            format!("export KEY=\"{LINE_64} {BODY_B} {BODY_C} -----END PRIVATE KEY-----\""),
            "export KEY=\"[CVC_REDACTED:v1:pem:*]\"",
        ),
    ] {
        let scrubbed = scrub(&text).unwrap();
        assert!(!holds_body(&scrubbed), "{text} -> {scrubbed}");
        assert_eq!(masked(&scrubbed), expected, "{text}");
        assert!(has_unterminated_private_key_block(&text), "{text}");
        assert_eq!(scrub(&scrubbed).unwrap(), scrubbed, "{text}");
    }
}

#[test]
fn a_lone_header_or_end_line_in_prose_with_no_long_token_keeps_its_text() {
    // 39 characters is not a token; 40 is.
    let token = |length: usize| "Ab1+".repeat(length / 4 + 1)[..length].to_owned();
    assert_eq!(token(39).len(), 39);
    let below = format!(
        "The header -----BEGIN PRIVATE KEY----- is followed by {} and more words.",
        token(39)
    );
    assert_eq!(scrub(&below).unwrap(), below);
    let at_the_limit = format!(
        "The header -----BEGIN PRIVATE KEY----- is followed by {} and more words.",
        token(40)
    );
    let scrubbed = scrub(&at_the_limit).unwrap();
    assert!(!scrubbed.contains(&token(40)), "{scrubbed}");
    assert_eq!(
        masked(&scrubbed),
        "The header [CVC_REDACTED:v1:pem:*] and more words."
    );
    for text in [
        // Prose, code and rules.
        "A key file must start with -----BEGIN RSA PRIVATE KEY----- and then holds the body.".to_owned(),
        "The body is followed by -----END RSA PRIVATE KEY-----, which closes the file.".to_owned(),
        "if line.starts_with(\"-----BEGIN PRIVATE KEY-----\") { skip(); } // then read the body".to_owned(),
        "Header: -----BEGIN PRIVATE KEY-----\n==================================================================\nThen the rest of the document, with ordinary words only.".to_owned(),
        format!("-----END PRIVATE KEY----- closes it\n{}\nand that was all", "/".repeat(80)),
        format!("Section ----------------------------------------\n-----BEGIN PRIVATE KEY-----\n{}", "+".repeat(70)),
        // Words and numbers and paths whose pieces are short.
        "See cvc-core/src/claude_code/transcript.rs and cvc-core/src/privacy.rs for -----BEGIN PRIVATE KEY----- handling in 2026-10-20 builds.".to_owned(),
    ] {
        assert_eq!(scrub(&text).unwrap(), text, "{text}");
        assert!(!has_unterminated_private_key_block(&text), "{text}");
    }
}

#[test]
fn the_long_tokens_are_looked_for_only_within_a_window_of_the_line() {
    let prose = |bytes: usize| "word ".repeat(bytes / 5);
    // The token ends within 4 KiB of the header: found, and the prose between goes with it.
    let near = format!("-----BEGIN PRIVATE KEY----- {}{LINE_64}", prose(3_000));
    let scrubbed = scrub(&near).unwrap();
    assert_eq!(masked(&scrubbed), "[CVC_REDACTED:v1:pem:*]");
    // Beyond it: the header is left alone with the rest of the text.
    let far = format!("-----BEGIN PRIVATE KEY----- {}{LINE_64}", prose(4_200));
    assert_eq!(scrub(&far).unwrap(), far);
    // The same before an END line.
    let near_end = format!("{LINE_64}{} -----END PRIVATE KEY-----", prose(3_000));
    assert_eq!(
        masked(&scrub(&near_end).unwrap()),
        "[CVC_REDACTED:v1:pem:*]"
    );
    let far_end = format!("{LINE_64}{} -----END PRIVATE KEY-----", prose(4_200));
    assert_eq!(scrub(&far_end).unwrap(), far_end);
    // A key of 4096 bits (about 3.2 KB of base64) run together is reached from either end.
    let long_body: Vec<String> = (0..50)
        .map(|n| format!("{}{n:02}", &LINE_64[..62]))
        .collect();
    let joined = long_body.join(" ");
    for text in [
        format!("-----BEGIN PRIVATE KEY----- {joined}"),
        format!("{joined} -----END PRIVATE KEY-----"),
    ] {
        let scrubbed = scrub(&text).unwrap();
        assert!(!scrubbed.contains(&LINE_64[..40]), "{scrubbed}");
        assert_eq!(masked(&scrubbed), "[CVC_REDACTED:v1:pem:*]");
    }
}

// ---- commit ids and digests are not key material -------------------------------------
//
// `git` and the hooks print a commit or blob id (40 hex digits) or a SHA-256 digest (64)
// in every session, and either is a run of base64 characters long enough to be looked
// for as a key. The base64 of a key is never only hex digits, so a run of those, exactly
// `[0-9a-fA-F]+`, is not taken for one. A run with a single character outside them is.

/// A commit or blob id, as `git` prints one.
const SHA_1: &str = "0123456789abcdef0123456789abcdef01234567";
/// A SHA-256 digest.
const SHA_256: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

#[test]
fn a_lone_header_or_end_line_near_a_commit_id_or_a_digest_keeps_its_text() {
    assert_eq!((SHA_1.len(), SHA_256.len()), (40, 64));
    let sha_512 = format!("{SHA_256}{SHA_256}");
    let upper = SHA_256.to_uppercase();
    let mixed = "0123456789aBcDeF".repeat(4);
    let prose = |bytes: usize| "word ".repeat(bytes / 5);
    for id in [
        SHA_1,
        SHA_256,
        sha_512.as_str(),
        upper.as_str(),
        mixed.as_str(),
    ] {
        for text in [
            // A header named in prose, and an id after it, as a commit is named.
            format!("A key file starts with -----BEGIN RSA PRIVATE KEY----- and came in with commit {id}, which touched the docs."),
            format!("-----BEGIN PRIVATE KEY----- is what it looks for. Linked 2 interactions to commit {id}"),
            // An id and a 64-hex digest up to 3 KB after the header, inside the window:
            // the prose between them stays.
            format!("-----BEGIN PRIVATE KEY----- {}{id} and {SHA_256} after", prose(3_000)),
            // The same around an END line.
            format!("The tree {id} holds a file that ends with -----END RSA PRIVATE KEY-----, which closes it."),
            format!("{SHA_1} {id} {} -----END PRIVATE KEY-----", prose(3_000)),
            // `git log` and `git ls-tree` output.
            format!("commit {SHA_1}\nAuthor: A <a@example.com>\n\n    Document -----BEGIN PRIVATE KEY----- handling\n\ncommit {id}\n100644 blob {id}\tkeys/server.pem\n"),
        ] {
            assert_eq!(scrub(&text).unwrap(), text, "{text}");
            assert!(!has_unterminated_private_key_block(&text), "{text}");
        }
    }
}

#[test]
fn a_base64_run_still_starts_the_fallback_beside_commit_ids_and_digests() {
    for (text, expected) in [
        // A key after an id: through the last base64 token. The id after it stays.
        (
            format!(
                "-----BEGIN PRIVATE KEY----- {SHA_1} {LINE_64} {BODY_B} then {SHA_256} follows"
            ),
            format!("[CVC_REDACTED:v1:pem:*] then {SHA_256} follows"),
        ),
        // An id between two pieces of the key goes with them.
        (
            format!("-----BEGIN PRIVATE KEY----- {LINE_64} {SHA_256} {BODY_B}"),
            "[CVC_REDACTED:v1:pem:*]".to_owned(),
        ),
        // Prose, and the ids in it, between a header and its key go with them.
        (
            format!("-----BEGIN PRIVATE KEY----- see commit {SHA_1} and {SHA_256}: {LINE_64}"),
            "[CVC_REDACTED:v1:pem:*]".to_owned(),
        ),
        // Before an END line: from the first base64 token. The ids around stay.
        (
            format!(
                "{SHA_256} before {LINE_64} {BODY_B} -----END PRIVATE KEY----- and {SHA_1} after"
            ),
            format!("{SHA_256} before [CVC_REDACTED:v1:pem:*] and {SHA_1} after"),
        ),
        // Without an id, as before.
        (
            format!("-----BEGIN PRIVATE KEY----- {LINE_64} {BODY_B}"),
            "[CVC_REDACTED:v1:pem:*]".to_owned(),
        ),
    ] {
        let scrubbed = scrub(&text).unwrap();
        assert!(!holds_body(&scrubbed), "{text} -> {scrubbed}");
        assert_eq!(masked(&scrubbed), expected, "{text}");
        assert!(has_unterminated_private_key_block(&text), "{text}");
        assert_eq!(scrub(&scrubbed).unwrap(), scrubbed, "{text}");
    }
}

#[test]
fn a_run_is_not_key_material_only_when_every_character_of_it_is_a_hex_digit() {
    // Hex digits alone, `[0-9a-fA-F]+`, of any length from 40 up: not key material.
    let hex_only = [
        SHA_1.to_owned(),
        SHA_256.to_owned(),
        SHA_256.to_uppercase(),
        "0123456789aBcDeF".repeat(4),
        format!("{SHA_256}{SHA_256}"),
        // Each end of each range of digits.
        "0".repeat(40),
        "9".repeat(40),
        "a".repeat(40),
        "f".repeat(40),
        "A".repeat(40),
        "F".repeat(40),
        "9".repeat(2_000),
    ];
    // One character that is not one makes the run count, wherever it is in it: the
    // letter after `f`, the one after `F`, any other letter, `+`, `/` and `=`.
    let counts = [
        format!("g{}", &SHA_256[1..]),
        format!("{}g", &SHA_256[..63]),
        format!("{}g{}", &SHA_256[..30], &SHA_256[31..]),
        format!("G{}", &SHA_256[1..]),
        format!("{}Z", &SHA_256[..63]),
        format!("{}z{}", &SHA_1[..20], &SHA_1[21..]),
        format!("{}+{}", &SHA_1[..20], &SHA_1[21..]),
        format!("{}/{}", &SHA_1[..20], &SHA_1[21..]),
        format!("{SHA_1}="),
        format!("{SHA_256}=="),
        // The run is whatever base64 characters stand together, so a digest joined to a
        // word or a prefix by one is a single run with it.
        format!("0x{SHA_256}"),
        format!("sha256={SHA_256}"),
        format!("commit={SHA_1}"),
        // The shortest run that counts: 40 characters, one of them not a digit.
        format!("g{}", &SHA_1[1..]),
    ];
    // Still too short, with or without such a character: 39.
    let too_short = [SHA_1[1..].to_owned(), format!("g{}", &SHA_1[2..])];
    for token in &hex_only {
        assert!(token.len() >= 40 && token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        for text in [
            format!(
                "The header -----BEGIN PRIVATE KEY----- is followed by {token} and more words."
            ),
            format!("Words {token} then -----END PRIVATE KEY-----."),
        ] {
            assert_eq!(scrub(&text).unwrap(), text, "{text}");
            assert!(!has_unterminated_private_key_block(&text), "{text}");
        }
    }
    for token in &counts {
        assert!(token.len() >= 40 && !token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let after_header = format!(
            "The header -----BEGIN PRIVATE KEY----- is followed by {token} and more words."
        );
        let before_end = format!("Words {token} then -----END PRIVATE KEY-----.");
        for (text, expected) in [
            (
                after_header,
                "The header [CVC_REDACTED:v1:pem:*] and more words.",
            ),
            (before_end, "Words [CVC_REDACTED:v1:pem:*]."),
        ] {
            let scrubbed = scrub(&text).unwrap();
            assert!(!scrubbed.contains(token.as_str()), "{text} -> {scrubbed}");
            assert_eq!(masked(&scrubbed), expected, "{text}");
            assert!(has_unterminated_private_key_block(&text), "{text}");
            assert_eq!(scrub(&scrubbed).unwrap(), scrubbed, "{text}");
        }
    }
    for token in &too_short {
        assert_eq!(token.len(), 39);
        let text = format!(
            "The header -----BEGIN PRIVATE KEY----- is followed by {token} and more words."
        );
        assert_eq!(scrub(&text).unwrap(), text, "{text}");
    }
    // What stands next to an id and is not a base64 character is not part of its run, so
    // a word, a path or a quote in front of an id leaves it hex only: `sha256:<digest>`,
    // `commit-<id>`, `"<id>"`, `(<id>)`, `repo#<id>`, `a@<id>`. (These are the characters
    // on either side of the ranges of hex digits among them: `:` after `9`, `@` before
    // `A`, and a backtick before `a`.)
    for separator in [
        " ", "\t", ":", "-", "_", ".", ",", ";", "\"", "'", "(", ")", "[", "]", "{", "}", "<", ">",
        "#", "@", "`", "\\", "|", "~", "&", "?", "!", "$", "%", "*",
    ] {
        for id in [SHA_1, SHA_256] {
            let text = format!(
                "The header -----BEGIN PRIVATE KEY----- is followed by commit{separator}{id}{separator}and more words."
            );
            assert_eq!(scrub(&text).unwrap(), text, "{text:?}");
        }
    }
}

#[test]
fn a_redaction_that_brings_a_key_within_reach_is_made_before_scrub_returns() {
    // The header has no key within 4 KiB of it, until the assignment between them is
    // replaced by a marker. Scrubbing the result must then change nothing, as a push
    // checks, so the key is redacted the first time.
    let text = format!(
        "-----BEGIN PRIVATE KEY----- token={} {LINE_64}",
        "#".repeat(4_100)
    );
    let once = scrub(&text).unwrap();
    assert!(!once.contains(LINE_64), "{once}");
    assert_eq!(masked(&once), "[CVC_REDACTED:v1:pem:*]");
    assert_eq!(scrub(&once).unwrap(), once);
    // And several such steps in a row.
    let step = format!(" token={} {LINE_64}", "#".repeat(4_100));
    let chain = format!("-----BEGIN PRIVATE KEY-----{}", step.repeat(3));
    let settled = scrub(&chain).unwrap();
    assert!(!settled.contains(LINE_64), "{settled}");
    assert_eq!(scrub(&settled).unwrap(), settled);
}

/// A small deterministic generator, so that a failing case can be rerun.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

#[test]
fn scrubbing_key_like_text_never_panics_and_scrubbing_again_changes_nothing() {
    let marker = scrub("AKIAABCDEFGHIJKLMNOP").unwrap();
    let fragments: Vec<String> = [
        "-----BEGIN PRIVATE KEY-----",
        "-----BEGIN RSA PRIVATE KEY-----",
        "-----BEGIN PGP PRIVATE KEY BLOCK-----",
        "-----END PRIVATE KEY-----",
        "-----END PGP PRIVATE KEY BLOCK-----",
        "-----BEGIN PUBLIC KEY-----",
        LINE_64,
        "MIIEvQIBADANBgkq",
        "short=",
        "AKIAABCDEFGHIJKLMNOP",
        "password=hunter2hunter2",
        "Proc-Type: 4,ENCRYPTED",
        "Version: GnuPG v2",
        "\n",
        "\n",
        "\r\n",
        "\\n",
        "\t",
        "  ",
        " ",
        "\"",
        "'",
        "-",
        "+",
        ">",
        "    12\t",
        "7→",
        "9|",
        "é",
        "…",
        "日本",
        "a b c",
        "=====================",
        "prose with words",
        "\u{E000}",
        // The layouts a cut key arrives in, and the pieces of them.
        BODY_B,
        BODY_C,
        TAIL,
        "src/k.pem-2-",
        "src/k.pem:3:",
        "a-2024-01.pem-12-",
        "1:",
        "2-",
        "<br>",
        "<br/>",
        "<BR />",
        "<b>",
        "\\\\n",
        "', '",
        "\", \"",
        "['",
        "']",
        "token=###############",
        "export KEY=",
        // Commit ids and digests, and runs that are one character off being one.
        SHA_1,
        SHA_256,
        "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcdef0123456g",
        "sha256=",
        " commit ",
        // Prose to put distance between the pieces.
        "word word word word word word word word word word word word ",
        // Text that is not a marker and looks like one, and one that is.
        "[CVC_REDACTED:v1:x:0123456789abcdef0123456789abcdef]",
        "[CVC_REDACTED:v1:pem:3F2A9C1E-0B7D-4A5E-9C3D-2E1F0A9B8C7D]",
    ]
    .iter()
    .map(|fragment| (*fragment).to_owned())
    .chain([
        marker,
        // Long runs, so that a window of a few KB is crossed by a few pieces.
        "word ".repeat(300),
        format!("token={}", "#".repeat(2_500)),
    ])
    .collect();
    let mut random = Lcg(0x5eed);
    for case in 0..6_000 {
        let mut text = String::new();
        for _ in 0..random.below(14) + 1 {
            text.push_str(&fragments[random.below(fragments.len())]);
        }
        let once = match scrub(&text) {
            Ok(once) => once,
            Err(_) => continue,
        };
        // Scrubbing what was scrubbed is the identity: a push checks exactly that.
        let twice = scrub(&once).unwrap_or_else(|error| panic!("case {case}: {error}: {text:?}"));
        assert_eq!(twice, once, "case {case}: {text:?}");
        // What the scrubber says it would redact is what it redacted.
        let flagged = has_unterminated_private_key_block(&text);
        if flagged {
            assert_ne!(once, text, "case {case}: {text:?}");
        }
        // The text kept around redactions is valid UTF-8 cut on boundaries.
        assert!(once.is_char_boundary(0) && once.is_char_boundary(once.len()));
    }
}

#[test]
fn policy_scrubbing_of_marker_like_text_settles_and_what_a_push_checks_never_changes() {
    // Rules that match inside any marker, and text of every shape a marker can have
    // or be taken for: real ones, fakes, pieces of them, and what the rules are for.
    let policy = policy("literal:projectx\nliteral:v1\nregex:[0-9a-f]{8}\nregex:[A-Z_]{4,}\n");
    let real = scrub("AKIAABCDEFGHIJKLMNOP").unwrap();
    let pem = scrub(&format!(
        "-----BEGIN PRIVATE KEY-----\n{LINE_64}\n-----END PRIVATE KEY-----"
    ))
    .unwrap();
    let fragments: Vec<String> = vec![
        real,
        pem,
        format!("[CVC_REDACTED:v1:projectx:{SOME_UUID}]"),
        format!("[CVC_REDACTED:v1:pem:{}]", "0123456789abcdef".repeat(4)),
        format!("[CVC_REDACTED:v1:pem:{}]", SOME_UUID.to_uppercase()),
        format!("[CVC_REDACTED:v1:aws_access_key:{SOME_UUID}]"),
        format!("[CVC_REDACTED:v1:policy_regex:{SOME_UUID}]"),
        "[CVC_REDACTED:v1:".to_owned(),
        "]".to_owned(),
        "[".to_owned(),
        ":".to_owned(),
        "projectx".to_owned(),
        "deadbeef".to_owned(),
        "password=hunter2hunter2".to_owned(),
        " ".to_owned(),
        "\n".to_owned(),
        "-----BEGIN PRIVATE KEY-----".to_owned(),
        LINE_64.to_owned(),
    ];
    let mut random = Lcg(0xfeed);
    for case in 0..3_000 {
        let mut text = String::new();
        for _ in 0..random.below(10) + 1 {
            text.push_str(&fragments[random.below(fragments.len())]);
        }
        let Ok(once) = policy.scrub(&text) else {
            continue;
        };
        // The terms the rules are for are gone, wherever they were.
        assert!(
            !once.contains("projectx"),
            "case {case}: {text:?} -> {once:?}"
        );
        // A policy marker can end a word, and the built-in detectors then read what
        // follows it as they would after any other non-word character, so scrubbing
        // with the policy once more can redact more. It settles by the next scrub.
        let twice = policy
            .scrub(&once)
            .unwrap_or_else(|error| panic!("case {case}: {error}: {text:?}"));
        let thrice = policy.scrub(&twice).unwrap();
        assert_eq!(thrice, twice, "case {case}: {text:?}");
        // What a push checks, which has no policy, is stable for what was stored.
        let pushed = scrub(&once).unwrap();
        assert_eq!(scrub(&pushed).unwrap(), pushed, "case {case}: {text:?}");
    }
}

/// How long `scrub` may take for one of the texts below. They are the size of
/// the largest value the scrubber takes; a scan that is linear in the text takes
/// a fraction of a second even unoptimized, and one that is quadratic, minutes.
const LINEAR_TIME: std::time::Duration = std::time::Duration::from_secs(20);

#[test]
fn text_made_of_many_headers_or_end_lines_is_scrubbed_in_linear_time() {
    let mib = 1024 * 1024;
    let key_lines = format!("{LINE_64}\n");
    let cases: Vec<(&str, String)> = vec![
        (
            "headers on one line",
            "-----BEGIN PRIVATE KEY-----".repeat(mib / 27),
        ),
        (
            "END lines on one line",
            "-----END PRIVATE KEY-----".repeat(mib / 25),
        ),
        (
            "one header per line",
            "-----BEGIN PRIVATE KEY-----\n".repeat(mib / 28),
        ),
        (
            "one END line per line",
            "-----END PRIVATE KEY-----\n".repeat(mib / 26),
        ),
        (
            "a header, then digits, many times",
            format!("-----BEGIN PRIVATE KEY-----{}", "1234567890".repeat(20)).repeat(mib / 230),
        ),
        (
            "a header, then blanks, many times",
            format!("-----BEGIN PRIVATE KEY-----{}", " \t".repeat(100)).repeat(mib / 230),
        ),
        (
            "a header and a number prefix, many times",
            "-----BEGIN PRIVATE KEY-----\n    12\t\n".repeat(mib / 40),
        ),
        (
            "one long line of key text, then an END line",
            format!("{}\n-----END PRIVATE KEY-----", "A".repeat(mib - 40)),
        ),
        ("one long run of key lines", key_lines.repeat(mib / 65)),
        (
            "headers, each with a key line",
            format!("-----BEGIN PRIVATE KEY-----\n{key_lines}").repeat(mib / 94),
        ),
        (
            "key lines, each before an END line",
            format!("{key_lines}-----END PRIVATE KEY-----\n").repeat(mib / 91),
        ),
        // The layouts that are read as long tokens, and the prefixes and breaks.
        (
            "a header and a commit id, many times, on one line",
            format!("-----BEGIN PRIVATE KEY----- {SHA_256} ").repeat(mib / 94),
        ),
        (
            "a commit id and an END line, many times, on one line",
            format!("{SHA_256} -----END PRIVATE KEY----- ").repeat(mib / 92),
        ),
        (
            "a header and a long token, many times, on one line",
            format!("-----BEGIN PRIVATE KEY----- {LINE_64} ").repeat(mib / 93),
        ),
        (
            "a long token and an END line, many times, on one line",
            format!("{LINE_64} -----END PRIVATE KEY----- ").repeat(mib / 91),
        ),
        (
            "a header, then 3 KB of words and a long token, many times",
            format!(
                "-----BEGIN PRIVATE KEY----- {}{LINE_64}\n",
                "word ".repeat(600)
            )
            .repeat(mib / 3_100),
        ),
        (
            "key lines behind file names",
            format!(
                "-----BEGIN PRIVATE KEY-----\n{}",
                format!("src/keys/server.pem-2-{LINE_64}\n").repeat(mib / 90)
            ),
        ),
        (
            "key lines between HTML breaks",
            format!(
                "-----BEGIN PRIVATE KEY-----<br>{}",
                format!("{LINE_64}<br>").repeat(mib / 70)
            ),
        ),
        (
            "key lines with escaped line breaks, twice over",
            format!(
                "-----BEGIN PRIVATE KEY-----\\\\n{}",
                format!("{LINE_64}\\\\n").repeat(mib / 70)
            ),
        ),
        (
            "one line of colons and hyphens with headers in it",
            format!("-----BEGIN PRIVATE KEY----- {}", "a-1-:2:".repeat(mib / 8)),
        ),
    ];
    for (name, text) in cases {
        assert!(text.len() <= mib, "{name}: {}", text.len());
        let started = std::time::Instant::now();
        // More than the limit of matches is an error; either way it is quick.
        let once = scrub(&text);
        let elapsed = started.elapsed();
        assert!(elapsed < LINEAR_TIME, "{name}: scrubbing took {elapsed:?}");
        if let Ok(once) = once {
            let started = std::time::Instant::now();
            assert_eq!(scrub(&once).unwrap(), once, "{name}");
            assert!(
                started.elapsed() < LINEAR_TIME,
                "{name}: scrubbing again took too long"
            );
        }
    }
}

#[test]
fn a_text_that_holds_many_markers_is_scrubbed_in_linear_time_and_comes_back_as_it_was() {
    let marker = scrub("AKIAABCDEFGHIJKLMNOP").unwrap();
    // As many markers as fit in a value: each is put aside and put back.
    let text = format!("{marker} ").repeat(1024 * 1024 / (marker.len() + 1));
    let started = std::time::Instant::now();
    assert_eq!(scrub(&text).unwrap(), text);
    assert!(started.elapsed() < LINEAR_TIME, "{:?}", started.elapsed());
    // Placeholder-looking bytes in the text are not markers and stay as they are.
    let lookalike = "\u{0}CVC0\u{0} and \u{0}CVC99999999999999999999\u{0}";
    assert_eq!(scrub(lookalike).unwrap(), lookalike);
}

#[test]
fn more_lone_key_blocks_than_the_limit_is_an_error_like_every_other_detector() {
    let one = format!("-----BEGIN PRIVATE KEY-----\n{LINE_64}\n\n");
    assert!(scrub(&one.repeat(256)).is_ok());
    assert!(scrub(&one.repeat(257)).is_err());
}

#[test]
fn an_openpgp_private_key_block_is_redacted_whole_or_by_its_run() {
    let complete = format!(
        "-----BEGIN PGP PRIVATE KEY BLOCK-----\nVersion: GnuPG v2\n\n{LINE_64}\n=AbCd\n-----END PGP PRIVATE KEY BLOCK-----\nafter"
    );
    assert_eq!(
        masked(&scrub(&complete).unwrap()),
        "[CVC_REDACTED:v1:pem:*]\nafter"
    );
    // Cut before its END line: armor headers, a blank line, the body, the
    // checksum line.
    let cut = format!(
        "-----BEGIN PGP PRIVATE KEY BLOCK-----\nVersion: GnuPG v2\nComment: test\n\n{LINE_64}\n{LINE_64}\n=AbCd\nplain text after"
    );
    assert_eq!(
        masked(&scrub(&cut).unwrap()),
        "[CVC_REDACTED:v1:pem:*]\nplain text after"
    );
    // A public key block is not a private one.
    let public = format!(
        "-----BEGIN PGP PUBLIC KEY BLOCK-----\n{LINE_64}\n-----END PGP PUBLIC KEY BLOCK-----"
    );
    assert_eq!(scrub(&public).unwrap(), public);
}

#[test]
fn armor_and_legacy_encryption_headers_between_the_header_and_the_body_are_part_of_the_run() {
    let text = format!(
        "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,0123456789ABCDEF0123456789ABCDEF\n\n{KEY_BODY}after text"
    );
    assert_eq!(
        masked(&scrub(&text).unwrap()),
        "[CVC_REDACTED:v1:pem:*]\nafter text"
    );
}

#[test]
fn a_complete_block_then_a_lone_header_each_redacts_what_it_holds() {
    let text = format!(
        "one\n-----BEGIN PRIVATE KEY-----\n{KEY_BODY}-----END PRIVATE KEY-----\nbetween\n-----BEGIN RSA PRIVATE KEY-----\n{KEY_BODY}tail"
    );
    assert_eq!(
        masked(&scrub(&text).unwrap()),
        "one\n[CVC_REDACTED:v1:pem:*]\nbetween\n[CVC_REDACTED:v1:pem:*]"
    );
}

#[test]
fn a_complete_block_is_redacted_alone_and_the_text_around_it_is_kept() {
    let text =
        format!("a\n-----BEGIN PRIVATE KEY-----\n{KEY_BODY}-----END PRIVATE KEY-----\nafter");
    assert_eq!(
        masked(&scrub(&text).unwrap()),
        "a\n[CVC_REDACTED:v1:pem:*]\nafter"
    );
}

#[test]
fn text_that_only_resembles_a_private_key_header_is_left_alone() {
    for text in [
        "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8A\n",
        "-----BEGIN CERTIFICATE-----\nMIIDdzCCAl+gAwIBAgIEAgAAuTANBgkqhkiG9w0BAQUFADBaMQswCQYDVQQGEwJJ\n",
        "-----BEGIN PRIVATE KEY",
        "BEGIN PRIVATE KEY-----",
        "---BEGIN PRIVATE KEY---",
        "the PRIVATE KEY block begins with a dashed BEGIN line",
    ] {
        assert_eq!(scrub(text).unwrap(), text);
    }
}

#[test]
fn a_lone_header_keeps_markers_around_it_and_scrubbing_is_stable() {
    let first = scrub("key AKIAABCDEFGHIJKLMNOP here").unwrap();
    let text = format!("{first}\n-----BEGIN PRIVATE KEY-----\n{KEY_BODY}");
    let scrubbed = scrub(&text).unwrap();
    assert!(scrubbed.starts_with("key [CVC_REDACTED:v1:aws_access_key:"));
    assert_eq!(scrubbed.matches("[CVC_REDACTED:").count(), 2);
    assert!(!scrubbed.contains("MIIEvQ"));
    // A scrubbed text scrubs to itself, which is what the push checks rely on.
    assert_eq!(scrub(&scrubbed).unwrap(), scrubbed);
}

#[test]
fn an_existing_marker_next_to_the_run_is_not_part_of_it() {
    let marker = scrub("AKIAABCDEFGHIJKLMNOP").unwrap();
    let text = format!("x\n-----BEGIN PRIVATE KEY-----\n{KEY_BODY}{marker}\nend");
    assert_eq!(
        masked(&scrub(&text).unwrap()),
        "x\n[CVC_REDACTED:v1:pem:*]\n[CVC_REDACTED:v1:aws_access_key:*]\nend"
    );
}

#[test]
fn json_strings_and_policies_get_the_lone_header_rule_too() {
    let scrubbed = scrub_json(&format!(
        r#"{{"command":"cat key","output":"head\n-----BEGIN PRIVATE KEY-----\n{LINE_64}\nMIIEvQIBADANBg","after":"kept"}}"#
    ))
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&scrubbed).unwrap();
    assert!(!scrubbed.contains("MIIEvQ"));
    assert!(value["output"]
        .as_str()
        .unwrap()
        .starts_with("head\n[CVC_REDACTED:v1:pem:"));
    assert_eq!(value["after"], "kept");
    assert!(validate_sanitized_json(&scrubbed).is_ok());
    assert!(validate_sanitized_json(&format!(
        r#"{{"output":"-----BEGIN PRIVATE KEY-----\n{LINE_64}"}}"#
    ))
    .is_err());

    let dir = tempdir().unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    assert_eq!(
        masked(
            &policy
                .scrub(&format!("a\n-----BEGIN PRIVATE KEY-----\n{LINE_64}"))
                .unwrap()
        ),
        "a\n[CVC_REDACTED:v1:pem:*]"
    );
}

#[test]
fn an_unterminated_private_key_block_is_told_from_a_complete_one_and_from_prose() {
    let block = format!("-----BEGIN PRIVATE KEY-----\n{KEY_BODY}-----END PRIVATE KEY-----");
    for (text, expected) in [
        (
            format!("out\n-----BEGIN RSA PRIVATE KEY-----\n{LINE_64}"),
            true,
        ),
        (format!("{LINE_64}\n-----END PRIVATE KEY-----"), true),
        (
            format!("-----BEGIN PGP PRIVATE KEY BLOCK-----\n\n{LINE_64}"),
            true,
        ),
        (
            format!("-----BEGIN OPENSSH PRIVATE KEY-----\n{LINE_64}\nmore"),
            true,
        ),
        // A complete block is redacted whole, so it is not the lone case...
        (block.clone(), false),
        // ...unless a lone header with key text follows it.
        (
            format!("{block}\nlater\n-----BEGIN EC PRIVATE KEY-----\n{LINE_64}"),
            true,
        ),
        // A key in a layout that is read as long tokens is the lone case too.
        (
            format!(r"['-----BEGIN PRIVATE KEY-----\n', '{LINE_64}\n']"),
            true,
        ),
        (
            format!("KEY=-----BEGIN PRIVATE KEY----- {LINE_64} {BODY_B}"),
            true,
        ),
        (
            format!("src/k.pem-2-{LINE_64}\nsrc/k.pem:3:-----END PRIVATE KEY-----"),
            true,
        ),
        // A header or END line with no key text beside it is only text.
        (
            "A header, -----BEGIN PRIVATE KEY-----, then words only".into(),
            false,
        ),
        ("-----BEGIN PRIVATE KEY-----".into(), false),
        ("-----END PRIVATE KEY-----".into(), false),
        (
            "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8A".into(),
            false,
        ),
        (
            "the PRIVATE KEY block starts with a BEGIN line".into(),
            false,
        ),
        (String::new(), false),
    ] {
        assert_eq!(
            has_unterminated_private_key_block(&text),
            expected,
            "{text}"
        );
        // It is exactly the case the scrubber redacts on its own.
        let changed = scrub(&text).unwrap() != text;
        assert_eq!(
            changed,
            expected || text == block || text.starts_with(&block),
            "{text}"
        );
    }
}

#[test]
fn policy_rules_never_rewrite_redaction_markers_so_a_rescrub_changes_nothing() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".thoughtignore"), "regex:[0-9a-f]{8}\n").unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    // 150 ids the rule redacts, and a key the built-ins redact. Each marker's own
    // id holds two matches of the rule, so rewriting markers would count 300 more.
    let ids: String = (0..150).map(|n| format!("deadbe{n:02x} ")).collect();
    let text = format!("{ids}key AKIAABCDEFGHIJKLMNOP here");
    let once = policy.scrub(&text).unwrap();
    assert_eq!(
        once.matches("[CVC_REDACTED:v1:policy_regex:").count(),
        150,
        "{once}"
    );
    assert_eq!(once.matches("[CVC_REDACTED:v1:aws_access_key:").count(), 1);
    assert!(!once.contains("deadbe"));
    // Scrubbing again is the identity.
    assert_eq!(policy.scrub(&once).unwrap(), once);
    // And so is the JSON form that tool arguments take.
    let json = serde_json::json!({ "ids": text }).to_string();
    let json_once = policy.scrub_json(&json).unwrap();
    assert_eq!(policy.scrub_json(&json_once).unwrap(), json_once);
}

#[test]
fn a_policy_literal_that_a_marker_contains_cannot_loop_or_damage_it() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join(".thoughtignore"),
        "literal:CVC\nliteral:policy_literal\nliteral:v1\n",
    )
    .unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    let text = "v1 key AKIAABCDEFGHIJKLMNOP and CVC and policy_literal";
    let once = policy.scrub(text).unwrap();
    // The built-in marker is intact; the plain occurrences are redacted.
    assert!(once.contains("[CVC_REDACTED:v1:aws_access_key:"));
    assert_eq!(once.matches("[CVC_REDACTED:v1:policy_literal:").count(), 3);
    assert_eq!(policy.scrub(&once).unwrap(), once);
}

#[test]
fn a_policy_rule_still_redacts_a_span_that_has_a_marker_inside_it() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join(".thoughtignore"),
        "regex:BEGIN-INTERNAL[\\s\\S]*?END-INTERNAL\nliteral:ACME-INTERNAL\n",
    )
    .unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    // A secret the built-ins redact sits in the middle of a block the rule is
    // for. The rule redacts the whole block, marker included, as it did before.
    let text = "before\nBEGIN-INTERNAL\nplans for the launch, key AKIAABCDEFGHIJKLMNOP\nmore plans\nEND-INTERNAL\nafter";
    let once = policy.scrub(text).unwrap();
    assert_eq!(
        masked(&once),
        "before\n[CVC_REDACTED:v1:policy_regex:*]\nafter"
    );
    assert!(!once.contains("plans") && !once.contains("AKIA"));
    // The same, with the marker already there (scrubbed by a writer without the rule).
    let marked = scrub(text).unwrap();
    assert!(marked.contains("[CVC_REDACTED:v1:aws_access_key:"));
    assert_eq!(
        masked(&policy.scrub(&marked).unwrap()),
        "before\n[CVC_REDACTED:v1:policy_regex:*]\nafter"
    );
    // A literal is plain text: it does not match across a marker.
    let across = format!("ACME-{}INTERNAL", scrub("AKIAABCDEFGHIJKLMNOP").unwrap());
    assert_eq!(policy.scrub(&across).unwrap(), across);
    // And what the first scrub made is left as it is by the second.
    assert_eq!(policy.scrub(&once).unwrap(), once);
}

#[test]
fn a_policy_rule_that_matches_only_markers_or_nothing_redacts_nothing() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".thoughtignore"), "regex:\\W+\n").unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    let first = scrub("AKIAABCDEFGHIJKLMNOP").unwrap();
    let second = scrub("AKIAABCDEFGHIJKLMNOQ").unwrap();
    // Two markers side by side: the rule's match is made of what a marker stands
    // for and nothing else, so nothing is rewritten.
    let text = format!("{first}{second}");
    assert_eq!(policy.scrub(&text).unwrap(), text);
    // With text between them the rule matches that text, and takes the markers
    // in its way with it, as any rule takes what it spans.
    assert_eq!(
        masked(&policy.scrub(&format!("{first} {second}")).unwrap()),
        "[CVC_REDACTED:v1:policy_regex:*]"
    );

    // A rule that matches the empty string redacts nothing, and costs nothing.
    std::fs::write(dir.path().join(".thoughtignore"), "regex:x*\nregex:\\b\n").unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    assert_eq!(policy.scrub("abc def").unwrap(), "abc def");
    assert_eq!(policy.scrub("").unwrap(), "");
}

#[test]
fn text_that_uses_the_private_use_characters_is_staged_with_a_free_one() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".thoughtignore"), "literal:ACME\n").unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    // The text already holds the first placeholder candidates, and a marker.
    let text = format!(
        "\u{E000}\u{E001} ACME {} \u{E002}",
        scrub("AKIAABCDEFGHIJKLMNOP").unwrap()
    );
    let once = policy.scrub(&text).unwrap();
    assert_eq!(
        masked(&once),
        "\u{E000}\u{E001} [CVC_REDACTED:v1:policy_literal:*] [CVC_REDACTED:v1:aws_access_key:*] \u{E002}"
    );
    // Every candidate in the text: there is no placeholder to stage with, which
    // is an error for the text, not a panic.
    let all: String = (0xE000u32..=0xF8FF).filter_map(char::from_u32).collect();
    assert!(policy.scrub(&format!("{all} ACME")).is_err());
    // Without a rule that is no problem at all.
    assert!(PreparedPolicy::built_ins_only().scrub(&all).is_ok());
}

#[test]
fn policy_rules_still_redact_plain_text_and_still_have_a_match_limit() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join(".thoughtignore"),
        "literal:ACME-INTERNAL\nregex:ticket-[0-9]{4}\n",
    )
    .unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    assert_eq!(
        masked(&policy.scrub("see ACME-INTERNAL and ticket-1234.").unwrap()),
        "see [CVC_REDACTED:v1:policy_literal:*] and [CVC_REDACTED:v1:policy_regex:*]."
    );
    // A match cannot reach across a marker.
    let across = format!("ACME-{}INTERNAL", scrub("AKIAABCDEFGHIJKLMNOP").unwrap());
    assert_eq!(policy.scrub(&across).unwrap(), across);
    // More than 256 matches of the rules in one value is still an error.
    let many = "ACME-INTERNAL ".repeat(257);
    assert!(policy.scrub(&many).is_err());
    assert!(policy.scrub(&"ACME-INTERNAL ".repeat(256)).is_ok());
}

// ---- what a redaction marker is ---------------------------------------------------
//
// A marker is atomic to every detector and rule, so only what cvc writes may count
// as one: `[CVC_REDACTED:v1:<kind>:<uuid>]`, with a kind cvc writes and a lowercase
// hyphenated UUID. Anything that merely looks like one is ordinary text.

/// A policy snapshot holding these `.thoughtignore` lines.
fn policy(rules: &str) -> PreparedPolicy {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".thoughtignore"), rules).unwrap();
    PreparedPolicy::load(dir.path()).unwrap()
}

const SOME_UUID: &str = "3f2a9c1e-0b7d-4a5e-9c3d-2e1f0a9b8c7d";

#[test]
fn text_that_only_looks_like_a_marker_is_open_to_every_policy_rule() {
    let policy = policy(
        "literal:projectx\nliteral:PROJECTX\nregex:[0-9a-f]{32,}\nregex:[0-9A-F]{8}\nregex:[0-9a-f]{13}\n",
    );
    let long_hex = "0123456789abcdef".repeat(4);
    let no_hyphens = SOME_UUID.replace('-', "");
    let capitals = SOME_UUID.to_uppercase();
    // (text, what a rule is for in it)
    let cases: Vec<(String, String)> = vec![
        // A kind cvc does not write, carrying a term a rule is for.
        (
            format!("[CVC_REDACTED:v1:projectx:{SOME_UUID}]"),
            "projectx".into(),
        ),
        (format!("[CVC_REDACTED:v1:x:{long_hex}]"), long_hex.clone()),
        (
            format!("[CVC_REDACTED:v1:projectx_secret:{SOME_UUID}]"),
            "projectx".into(),
        ),
        // A known kind, but a long hex id instead of a UUID, or a UUID of another shape.
        (
            format!("[CVC_REDACTED:v1:pem:{long_hex}]"),
            long_hex.clone(),
        ),
        (
            format!("[CVC_REDACTED:v1:pem:{no_hyphens}]"),
            no_hyphens.clone(),
        ),
        (
            format!("[CVC_REDACTED:v1:pem:{SOME_UUID}0]"),
            "e1f0a9b8c7d0".into(),
        ),
        // Capital letters: in the id, in the kind, in the term.
        (
            format!("[CVC_REDACTED:v1:pem:{capitals}]"),
            "3F2A9C1E".into(),
        ),
        (
            format!("[CVC_REDACTED:v1:PROJECTX:{SOME_UUID}]"),
            "PROJECTX".into(),
        ),
        (
            format!("[CVC_REDACTED:v1:Pem:{capitals}]"),
            "3F2A9C1E".into(),
        ),
        // Brackets inside, or around: the inside is not a marker's, and the
        // outside is not part of one.
        (
            format!("[CVC_REDACTED:v1:pem:[projectx]{SOME_UUID}]"),
            "projectx".into(),
        ),
        (
            format!("[CVC_REDACTED:v1:[projectx]:{SOME_UUID}]"),
            "projectx".into(),
        ),
        (
            format!("[[CVC_REDACTED:v1:projectx:{SOME_UUID}]]"),
            "projectx".into(),
        ),
        // The wrong version, a space, a missing colon.
        (
            format!("[CVC_REDACTED:v2:pem:{long_hex}]"),
            long_hex.clone(),
        ),
        (
            format!("[CVC_REDACTED:v1: pem:{long_hex}]"),
            long_hex.clone(),
        ),
        (
            format!("[CVC_REDACTED:v1:pem {long_hex}]"),
            long_hex.clone(),
        ),
    ];
    for (text, needle) in cases {
        let scrubbed = policy.scrub(&text).unwrap();
        assert!(
            !scrubbed.contains(&needle),
            "{needle} survived in {text}: {scrubbed}"
        );
        // What was scrubbed is left as it is by another scrub, the way a push re-checks it.
        assert_eq!(policy.scrub(&scrubbed).unwrap(), scrubbed, "{text}");
        assert_eq!(scrub(&scrubbed).unwrap(), scrubbed, "{text}");
    }
}

#[test]
fn a_real_marker_inside_text_that_looks_like_one_stays_whole_and_the_rest_is_scrubbed() {
    let policy = policy("literal:projectx\n");
    let real = scrub("AKIAABCDEFGHIJKLMNOP").unwrap();
    // A fake marker whose kind slot holds a real one: the real one is atomic, and the
    // term around it is not shielded by the fake brackets.
    let text = format!("[CVC_REDACTED:v1:projectx:{real}] and projectx");
    let scrubbed = policy.scrub(&text).unwrap();
    assert!(scrubbed.contains(&real), "{scrubbed}");
    assert!(!scrubbed.contains("projectx"), "{scrubbed}");
    assert_eq!(policy.scrub(&scrubbed).unwrap(), scrubbed);
}

#[test]
fn a_marker_cvc_wrote_is_atomic_to_every_rule_whatever_kind_wrote_it() {
    // One marker of each kind the built-in detectors write.
    let samples = [
        "AKIAABCDEFGHIJKLMNOP".to_owned(),
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"
            .to_owned(),
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789".to_owned(),
        "sk-abcdefghijklmnopqrstuvwxyz123456".to_owned(),
        "xoxb-1234567890-abcdefghij".to_owned(),
        "ya29.abcdefghijklmnopqrstuvwxyz0123".to_owned(),
        "Authorization: Bearer abcdefghijklmnop1234".to_owned(),
        "https://user:pass@example.invalid/x".to_owned(),
        "password=supersecretvalue".to_owned(),
        format!("-----BEGIN PRIVATE KEY-----\n{LINE_64}\n-----END PRIVATE KEY-----"),
        format!("-----BEGIN PRIVATE KEY-----\n{LINE_64}\nand a sentence"),
    ];
    // Rules that match inside any marker: its id, its prefix, its kind.
    let policy = policy(
        "regex:[0-9a-f]{8}\nregex:[0-9a-f]{4}\nliteral:CVC\nliteral:REDACTED\nliteral:v1\nliteral:pem\nliteral:jwt\nliteral:aws_access_key\nliteral:github_token\nliteral:openai_token\nliteral:slack_token\nliteral:google_token\nliteral:authorization\nliteral:url_credentials\nliteral:secret_assignment\nliteral:policy_literal\nliteral:policy_regex\n",
    );
    for text in samples {
        let once = scrub(&text).unwrap();
        assert!(once.contains("[CVC_REDACTED:v1:"), "{text}");
        let again = policy.scrub(&once).unwrap();
        assert_eq!(again, once, "a rule rewrote a marker: {text}");
    }
    // The markers the policy writes are atomic to its own rules too.
    let made = policy.scrub("the pem and the jwt are words").unwrap();
    assert!(made.contains("[CVC_REDACTED:v1:policy_literal:"), "{made}");
    assert_eq!(policy.scrub(&made).unwrap(), made);
}

#[test]
fn the_built_in_detectors_read_inside_text_that_only_looks_like_a_marker() {
    // A kind that is a keyword a detector looks for, and a key where an id goes.
    for text in [
        format!(
            "[CVC_REDACTED:v1:password:{}]",
            "0123456789abcdef".repeat(2)
        ),
        "[CVC_REDACTED:v1:pem:AKIAABCDEFGHIJKLMNOP]".to_owned(),
        format!("[CVC_REDACTED:v1:token:{SOME_UUID}]"),
    ] {
        let scrubbed = scrub(&text).unwrap();
        assert_ne!(scrubbed, text, "{text}");
        assert!(!scrubbed.contains("AKIAABCD"), "{scrubbed}");
        assert!(!scrubbed.contains("0123456789abcdef"), "{scrubbed}");
        assert!(!scrubbed.contains(SOME_UUID), "{scrubbed}");
        assert_eq!(scrub(&scrubbed).unwrap(), scrubbed, "{text}");
    }
    // A shape that no detector reads inside is left alone.
    let inert = format!("[CVC_REDACTED:v1:x:{SOME_UUID}]");
    assert_eq!(scrub(&inert).unwrap(), inert);
}

#[test]
fn only_a_real_marker_stands_as_the_value_under_a_credential_key() {
    let real = scrub("AKIAABCDEFGHIJKLMNOP").unwrap();
    let kept = format!(r#"{{"password":"{real}"}}"#);
    assert_eq!(scrub_json(&kept).unwrap(), kept);
    assert!(validate_sanitized_json(&kept).is_ok());
    for fake in [
        format!("[CVC_REDACTED:v1:projectx:{SOME_UUID}]"),
        format!(
            "[CVC_REDACTED:v1:aws_access_key:{}]",
            SOME_UUID.to_uppercase()
        ),
        "[CVC_REDACTED:v1:pem:0123456789abcdef0123456789abcdef]".to_owned(),
    ] {
        let input = format!(r#"{{"token":"{fake}"}}"#);
        let scrubbed = scrub_json(&input).unwrap();
        assert_ne!(scrubbed, input);
        assert!(!scrubbed.contains(&fake), "{scrubbed}");
        assert!(
            scrubbed.contains("[CVC_REDACTED:v1:secret_assignment:"),
            "{scrubbed}"
        );
        assert!(validate_sanitized_json(&scrubbed).is_ok());
        assert!(validate_sanitized_json(&input).is_err());
    }
}

#[test]
fn credential_subtrees_keep_shape_but_not_raw_leaves() {
    let scrubbed = scrub_json(
        r#"{"password":{"value":"ordinary-sensitive-value","items":["nested-secret"]}}"#,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&scrubbed).unwrap();
    assert!(value["password"].is_object());
    assert!(!scrubbed.contains("ordinary-sensitive-value"));
    assert!(!scrubbed.contains("nested-secret"));
}

#[test]
fn sanitized_nested_credential_json_is_idempotent() {
    let once = scrub_json(r#"{"password":{"value":"raw-value","more":["raw-two"]}}"#).unwrap();
    assert_eq!(scrub_json(&once).unwrap(), once);
    assert!(validate_sanitized_json(&once).is_ok());
    assert!(validate_sanitized_json(r#"{"password":{"value":"raw-value"}}"#).is_err());
}

#[test]
fn policy_absent_valid_invalid_and_links_fail_closed() {
    let dir = tempdir().unwrap();
    assert!(!PreparedPolicy::load(dir.path())
        .unwrap()
        .ignores_path("src/a"));
    std::fs::write(
        dir.path().join(".thoughtignore"),
        "path:secrets\nliteral:DO_NOT_STORE",
    )
    .unwrap();
    assert!(PreparedPolicy::load(dir.path())
        .unwrap()
        .ignores_path("secrets/key"));
    std::fs::write(dir.path().join(".thoughtignore"), "path:../escape").unwrap();
    assert!(PreparedPolicy::load(dir.path()).is_err());
    std::fs::remove_file(dir.path().join(".thoughtignore")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("missing", dir.path().join(".thoughtignore")).unwrap();
        assert!(
            PreparedPolicy::load(dir.path()).is_err(),
            "dangling symlink is not absent"
        );
        std::fs::remove_file(dir.path().join(".thoughtignore")).unwrap();
        std::fs::write(dir.path().join("target"), "path:secret").unwrap();
        std::os::unix::fs::symlink("target", dir.path().join(".thoughtignore")).unwrap();
        assert!(PreparedPolicy::load(dir.path()).is_err());
    }
}

#[test]
fn aggregate_uses_the_supplied_policy_snapshot() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".thoughtignore"), "path:private").unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    // Replacing the file after load must not change the capture's policy.
    std::fs::write(dir.path().join(".thoughtignore"), "path:other").unwrap();
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let id = InteractionId::new();
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "c".into(),
                title: "c".into(),
                created_at: Utc::now(),
            },
            interaction(id.clone(), "c"),
            vec![ContextItem {
                id: None,
                interaction_id: id.clone(),
                file_path: "private/key".into(),
                git_blob_sha: None,
                dirty_patch: Some("secret=supersecretvalue".into()),
                start_line: None,
                end_line: None,
            }],
            Vec::new(),
            policy,
            "0".repeat(64),
        ))
        .unwrap();
    assert!(store.get_context_items(&id).unwrap().is_empty());
    assert!(!store
        .get_interaction(&id)
        .unwrap()
        .unwrap()
        .user_prompt
        .contains("abcdefghijklmnopqrstuvwxyz"));
}

#[test]
fn malformed_policy_aborts_before_aggregate_write() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".thoughtignore"), "invalid: policy").unwrap();
    assert!(PreparedPolicy::load(dir.path()).is_err());
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let id = InteractionId::new();
    store
        .capture_cli_run(CliRunCapture::new(
            Conversation {
                id: "c".into(),
                title: "c".into(),
                created_at: Utc::now(),
            },
            interaction(id.clone(), "c"),
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    assert!(store.get_interaction(&id).unwrap().is_some());
}

#[test]
fn sharing_consent_is_remote_fingerprinted_and_defaults_off() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.remote("origin", "https://example.test/one.git")
        .unwrap();
    let fresh = privacy_status(&repo, "origin").unwrap();
    assert!(!fresh.capture_acknowledged && !fresh.sharing_consented && !fresh.auto_push);
    acknowledge_sharing(&repo, "origin").unwrap();
    assert!(privacy_status(&repo, "origin").unwrap().sharing_consented);
    repo.remote_set_url("origin", "https://example.test/two.git")
        .unwrap();
    assert!(!privacy_status(&repo, "origin").unwrap().sharing_consented);
}

#[test]
fn sharing_is_atomic_and_projection_never_includes_private_peers() {
    let dir = tempdir().unwrap();
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let first = InteractionId::new();
    let second = InteractionId::new();
    for (id, conversation) in [(&first, "share-me"), (&second, "private-peer")] {
        store
            .capture_mcp(McpCapture::new(
                Conversation {
                    id: conversation.into(),
                    title: conversation.into(),
                    created_at: Utc::now(),
                },
                interaction(id.clone(), conversation),
                Vec::new(),
                Vec::new(),
                PreparedPolicy::built_ins_only(),
                "0".repeat(64),
            ))
            .unwrap();
    }
    assert!(store
        .projection_interaction_ids("remote")
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .share_conversation_for_remote("share-me", "remote", FutureSharePolicy::Private)
            .unwrap(),
        1
    );
    assert_eq!(
        store.projection_interaction_ids("remote").unwrap(),
        vec![first.clone()]
    );
    // Explicit future policy is persisted; an unrelated conversation is untouched.
    let future = InteractionId::new();
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "share-me".into(),
                title: "share-me".into(),
                created_at: Utc::now(),
            },
            interaction(future.clone(), "share-me"),
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    assert_eq!(
        store.projection_interaction_ids("remote").unwrap(),
        vec![first]
    );
}

#[test]
fn sharing_one_destination_never_authorizes_another() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.remote("origin", "https://example.test/origin.git")
        .unwrap();
    repo.remote("backup", "https://example.test/backup.git")
        .unwrap();
    let origin = privacy_status(&repo, "origin").unwrap().remote_fingerprint;
    let backup = privacy_status(&repo, "backup").unwrap().remote_fingerprint;
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let id = InteractionId::new();
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "multi-remote".into(),
                title: "multi-remote".into(),
                created_at: Utc::now(),
            },
            interaction(id.clone(), "multi-remote"),
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    store
        .share_conversation_for_remote("multi-remote", &backup, FutureSharePolicy::Private)
        .unwrap();
    assert!(store
        .projection_interaction_ids(&origin)
        .unwrap()
        .is_empty());
    assert_eq!(store.projection_interaction_ids(&backup).unwrap(), vec![id]);
}

#[test]
fn pulled_publication_does_not_grant_sharing_to_another_remote() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.remote("source", "https://example.test/source.git")
        .unwrap();
    repo.remote("other", "https://example.test/other.git")
        .unwrap();

    let source = CvcStore::open(dir.path().join("source.db")).unwrap();
    let id = InteractionId::new();
    source
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "remote-conversation".into(),
                title: "source".into(),
                created_at: Utc::now(),
            },
            interaction(id.clone(), "remote-conversation"),
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    let source_fingerprint = privacy_status(&repo, "source").unwrap().remote_fingerprint;
    source
        .share_conversation_for_remote(
            "remote-conversation",
            &source_fingerprint,
            FutureSharePolicy::Private,
        )
        .unwrap();
    project_fixture_to_ref(&repo, &source, "refs/cvc/source", &source_fingerprint);

    let imported = CvcStore::open(dir.path().join("imported.db")).unwrap();
    sync::pull_from_ref_for_remote(
        &repo,
        &imported,
        "refs/cvc/source",
        Some(&source_fingerprint),
    )
    .unwrap();
    let other_fingerprint = privacy_status(&repo, "other").unwrap().remote_fingerprint;
    assert!(imported
        .projection_interaction_ids(&other_fingerprint)
        .unwrap()
        .is_empty());
    assert!(imported
        .projection_interaction_ids(&source_fingerprint)
        .unwrap()
        .is_empty());

    // A later explicit local share is the only transition that makes it eligible.
    imported
        .share_conversation_for_remote(
            &format!("remote:{source_fingerprint}:remote-conversation"),
            &other_fingerprint,
            FutureSharePolicy::Private,
        )
        .unwrap();
    assert_eq!(
        imported
            .projection_interaction_ids(&other_fingerprint)
            .unwrap(),
        vec![id]
    );
}

#[cfg(unix)]
#[test]
fn privacy_store_files_are_private_and_reject_symlinks() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let cvc_dir = dir.path().join("cvc");
    let db = cvc_dir.join("index.db");
    let _store = CvcStore::open(&db).unwrap();
    assert_eq!(
        std::fs::metadata(&cvc_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&db).unwrap().permissions().mode() & 0o777,
        0o600
    );
    for suffix in ["-wal", "-shm"] {
        let sidecar = std::path::PathBuf::from(format!("{}{}", db.display(), suffix));
        if sidecar.exists() {
            assert_eq!(
                std::fs::metadata(sidecar).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    let target = dir.path().join("target.db");
    std::fs::write(&target, b"not sqlite").unwrap();
    let link = dir.path().join("linked.db");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(CvcStore::open(&link).is_err());
}

#[test]
fn aws_pair_is_absent_from_all_captured_fields_db_wal_and_projection() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let db_path = dir.path().join("index.db");
    let store = CvcStore::open(&db_path).unwrap();
    let key = "AKIAABCDEFGHIJKLMNOP";
    let secret = "aws_secret_access_key=abcdefghijklmnopqrstuvwx1234567890";
    let id = InteractionId::new();
    let mut captured = interaction(id.clone(), "aws-matrix");
    captured.user_prompt = format!("prompt {key} {secret}");
    captured.model_name = Some(format!("model {key}"));
    captured.model_cot = Some(format!("cot {secret}"));
    captured.model_response = Some(format!("response {key}"));
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "aws-matrix".into(),
                title: format!("title {key}"),
                created_at: Utc::now(),
            },
            captured,
            vec![ContextItem {
                id: None,
                interaction_id: id.clone(),
                file_path: format!("src/{key}.rs"),
                git_blob_sha: None,
                dirty_patch: Some(format!("patch {secret}")),
                start_line: None,
                end_line: None,
            }],
            vec![ToolExecution {
                id: None,
                interaction_id: id.clone(),
                tool_protocol: format!("mcp-{key}"),
                tool_name: format!("tool-{secret}"),
                arguments: format!(
                    r#"{{"nested":{{"aws_access_key":"{key}","secret":"{secret}"}}}}"#
                ),
                status: ToolStatus::Success,
            }],
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    store
        .share_conversation_for_remote("aws-matrix", "remote", FutureSharePolicy::Private)
        .unwrap();
    let saved = store.get_interaction(&id).unwrap().unwrap();
    assert!(!format!("{:?}", saved).contains(key));
    assert!(!format!("{:?}", store.get_context_items(&id).unwrap()).contains(secret));
    assert!(!format!("{:?}", store.get_tool_executions(&id).unwrap()).contains(key));
    let bytes = std::fs::read(&db_path).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(key));
    let wal = std::path::PathBuf::from(format!("{}-wal", db_path.display()));
    if wal.exists() {
        assert!(!String::from_utf8_lossy(&std::fs::read(wal).unwrap()).contains(secret));
    }
    let projection =
        sync::push_projection_to_ref(&repo, &store, &no_baseline(&repo), "remote").unwrap();
    let sync::ProjectionResult::Candidate { oid, candidate, .. } = projection else {
        panic!("expected projection")
    };
    let tree = repo.find_commit(oid).unwrap().tree().unwrap();
    let nodes = tree
        .get_name("nodes")
        .unwrap()
        .to_object(&repo)
        .unwrap()
        .peel_to_tree()
        .unwrap();
    let shard = nodes
        .get_name(&id.as_str()[..2])
        .unwrap()
        .to_object(&repo)
        .unwrap()
        .peel_to_tree()
        .unwrap();
    let blob = repo
        .find_blob(shard.get_name(&format!("{id}.json")).unwrap().id())
        .unwrap();
    let wire = String::from_utf8_lossy(blob.content());
    assert!(!wire.contains(key) && !wire.contains(secret));
    drop(candidate);
}

#[test]
fn share_snapshot_race_rejects_new_turn_without_sharing_it() {
    let dir = tempdir().unwrap();
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let first = InteractionId::new();
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "race".into(),
                title: "race".into(),
                created_at: Utc::now(),
            },
            interaction(first.clone(), "race"),
            vec![],
            vec![],
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    let snapshot = store.share_snapshot("race").unwrap();
    let second = InteractionId::new();
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "race".into(),
                title: "race".into(),
                created_at: Utc::now(),
            },
            interaction(second.clone(), "race"),
            vec![],
            vec![],
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    assert!(store
        .share_exact_snapshot("race", "remote", &snapshot, FutureSharePolicy::Private)
        .is_err());
    assert!(store
        .projection_interaction_ids("remote")
        .unwrap()
        .is_empty());
}

fn shown(raw: &str) -> Option<String> {
    cvc_core::privacy::display_location(raw)
}

#[test]
fn display_location_keeps_host_port_and_path_of_scheme_urls_and_nothing_else() {
    for (raw, expected) in [
        ("https://user:tok@host/a/b.git", "host/a/b.git"),
        ("https://tok@host/p?x=1#f", "host/p"),
        ("https://us%40er:p%3Ass@host/a", "host/a"),
        ("https://host/a/b.git?token=abc#frag", "host/a/b.git"),
        ("ssh://git@host:22/p", "host:22/p"),
        ("ssh://user:pw@host/p", "host/p"),
        ("git://host/o/r.git", "host/o/r.git"),
        (
            "https://HOST.Example/Org/Repo.git",
            "host.example/Org/Repo.git",
        ),
        ("https://host:8443", "host:8443"),
        ("https://host", "host"),
        ("https://host/", "host"),
        ("ssh://host", "host"),
        // The scheme's own default port is not shown.
        ("https://host:443/p", "host/p"),
        ("https://host/a%20b.git", "host/a%20b.git"),
        (
            "https://[2001:db8::1]:8443/r.git",
            "[2001:db8::1]:8443/r.git",
        ),
        ("https://u:p@[::1]/r.git", "[::1]/r.git"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
}

#[test]
fn display_location_drops_the_user_of_scp_like_remotes() {
    for (raw, expected) in [
        ("git@github.com:o/r.git", "github.com:o/r.git"),
        ("github.com:o/r.git", "github.com:o/r.git"),
        ("user@host.example:~/repo", "host.example:~/repo"),
        ("git@[2001:db8::1]:o/r.git", "[2001:db8::1]:o/r.git"),
        ("[::1]:path", "[::1]:path"),
        ("a@b@host:p", "host:p"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
}

#[test]
fn display_location_shows_local_paths_as_they_are() {
    for (raw, expected) in [
        ("/srv/r.git", "/srv/r.git"),
        ("  /srv/r.git  ", "/srv/r.git"),
        ("../sibling.git", "../sibling.git"),
        ("relative/dir", "relative/dir"),
        // A slash before the colon makes it a path, not host:path.
        ("/srv/a:b/r.git", "/srv/a:b/r.git"),
        ("C:\\repos\\x.git", "C:\\repos\\x.git"),
        ("C:/repos/x.git", "C:/repos/x.git"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
    #[cfg(unix)]
    for (raw, expected) in [
        ("file:///srv/r.git", "/srv/r.git"),
        ("file:///srv/r%20x.git", "/srv/r x.git"),
        ("file://localhost/srv/r.git", "/srv/r.git"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
}

#[test]
fn display_location_refuses_what_it_cannot_show_safely() {
    let secret_in_path = format!(
        "https://host/{}",
        "ghp_0123456789abcdefghijklmnopqrstuvwxyz"
    );
    let too_long = format!("https://host/{}", "a".repeat(3000));
    for raw in [
        "",
        "   ",
        "http://",
        "https://",
        "https://[::1",
        "https://host:notaport/p",
        "://x",
        "ht tp://x",
        "mailto:someone@example.com",
        // Git would read this as host `user` and path `secret@host:path`.
        "user:secret@host:path",
        // A query or fragment in a form that is not a URL has nothing to strip.
        "git@host:o/r.git?x=1",
        "/srv/r.git#frag",
        // Not a URL, not host:path, and holds an `@`: shown as nothing.
        "token@github.com/o/r",
        "git@github.com",
        "host:",
        "ho st:path",
        // Control and direction-changing characters.
        "host:\u{1b}[31mred",
        "host:line\nbreak",
        "/srv/\u{202e}evil.git",
        "file://server/share/r.git",
        // The built-in scrubber would redact something in the form.
        secret_in_path.as_str(),
        too_long.as_str(),
    ] {
        assert_eq!(shown(raw), None, "{raw:?}");
    }
}

#[test]
fn display_location_never_shows_a_credential() {
    let secrets = [
        "s3cr3t-value",
        "p%40ss",
        "ghp_0123456789abcdefghijklmnopqrstuvwxyz",
        "hunter2",
    ];
    for secret in secrets {
        for raw in [
            format!("https://user:{secret}@host/r.git"),
            format!("https://{secret}@host/r.git"),
            format!("ssh://{secret}@host/r.git"),
            format!("ssh://git:{secret}@host:2222/r.git"),
            format!("{secret}@host:o/r.git"),
            format!("git@host:o/r.git?token={secret}"),
            format!("user:{secret}@host:o/r.git"),
            format!("https://host/r.git?token={secret}"),
            format!("https://host/r.git#{secret}"),
            format!("https://user:{secret}@host:8443/r.git?x={secret}#{secret}"),
            format!("https://user:{secret}@[::1]/r.git"),
        ] {
            if let Some(location) = shown(&raw) {
                assert!(
                    !location.contains(secret) && !location.contains('@'),
                    "{raw} -> {location}"
                );
            }
        }
    }
}

#[test]
fn display_location_never_shows_a_host_other_than_the_one_git_contacts() {
    // The `url` crate ends the authority at `?`, `#` and `\`; libgit2 ends it
    // at the first `/` and takes the host after the last `@`. Each of these
    // parses as host `github.com` and is contacted as `evil.example`.
    for raw in [
        "https://github.com?@evil.example/o/r.git",
        "https://github.com#@evil.example/o/r.git",
        "https://github.com\\@evil.example/o/r.git",
        "ssh://github.com?@evil.example/o/r.git",
        "ssh://github.com#@evil.example/o/r.git",
        "ssh://github.com\\@evil.example/o/r.git",
        "git://github.com?@evil.example/o/r.git",
        "https://user@github.com?@evil.example/o/r.git",
        "https://github.com:443?@evil.example:8443/o/r.git",
        "https://github.com?x=1@evil.example/o/r.git",
        "https://github.com#frag@evil.example",
        "ssh+git://[::1]?@evil.example/r",
        // The host that is shown must also be the one that is written: a
        // normalized form (IDNA, percent-encoding, numeric) is not.
        "https://%65xample.com/r",
        "https://\u{430}pple.com/r",
        "https://0x7f.1/r",
        "https://2130706433/r",
        "ssh://h%C3%B6st/r",
        "ssh://github.com%40evil.example/r",
        "https://[2001:0db8::1]/r",
        // Nothing before the `://` may be anything but a scheme.
        "ht tp://github.com/r",
        "/srv/a://github.com/r",
        "git@host:o/r://github.com/r",
        // A `\` in a URL is a separator to one parser and a character to the
        // other, so the path shown might not be the path used either.
        "https://github.com/o\\r.git",
        "file:///srv\\r.git",
        // `file` has no host to contact, and only `localhost` means that.
        "file://github.com/srv/r.git",
        "file://user@/srv/r.git",
    ] {
        assert_eq!(shown(raw), None, "{raw}");
    }
    // The ordinary forms next to them are still shown.
    for (raw, expected) in [
        ("https://github.com/o/r.git", "github.com/o/r.git"),
        ("https://user@github.com/o/r.git", "github.com/o/r.git"),
        (
            "https://user:tok@GitHub.COM:8443/o/r.git",
            "github.com:8443/o/r.git",
        ),
        ("https://a%40b:c@github.com/o/r.git", "github.com/o/r.git"),
        ("ssh://git@github.com:22/o/r.git", "github.com:22/o/r.git"),
        ("ssh://git@[::1]:2222/o/r.git", "[::1]:2222/o/r.git"),
        ("https://xn--80ak6aa92e.com/r", "xn--80ak6aa92e.com/r"),
        ("https://github.com/o/r.git?x=1#frag", "github.com/o/r.git"),
        ("https://user@github.com/?q=a@evil.example", "github.com"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
}

/// The host libgit2 contacts for `scheme://...` (util/net.c, `git_net_url_parse`
/// and `url_parse_authority`): the authority ends at the first `/` and nowhere
/// else, the host is what follows the last `@`, and a numeric port is not part
/// of it. Lower-cased, since DNS names are not case sensitive.
fn libgit2_host(raw: &str) -> Option<String> {
    let authority = raw.split_once("://")?.1.split('/').next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = match host_port.find(']') {
        Some(end) => &host_port[..=end],
        None => host_port
            .rsplit_once(':')
            .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
            .map_or(host_port, |(host, _)| host),
    };
    Some(host.to_ascii_lowercase())
}

/// The host of a shown location, `host[:port][/path]`.
fn shown_host(location: &str) -> String {
    let end = if location.starts_with('[') {
        location.find(']').map(|index| index + 1)
    } else {
        location.find([':', '/'])
    };
    location[..end.unwrap_or(location.len())].to_ascii_lowercase()
}

#[test]
fn whatever_display_location_shows_the_host_it_shows_is_the_host_libgit2_contacts() {
    // Every combination is a user-information part glued to a host, an `@`, a
    // second host, and what can follow it. Each shown location costs a full
    // scrub, so the sets are small and chosen to differ in what matters: a host
    // name and an IPv6 literal, a special scheme and two that are not.
    let mut shown_count = 0;
    let mut refused_count = 0;
    for scheme in ["https", "ssh", "git"] {
        for first in ["github.com", "[::1]"] {
            for glue in [
                "", "?", "#", "\\", "?x=", "#y:", "\\u:", ":pw", ":", "%40", "%2F",
            ] {
                for second in ["evil.example", "[::1]"] {
                    for port in ["", ":8443"] {
                        for tail in ["", "/?q@evil.example/r"] {
                            let raw = format!("{scheme}://{first}{glue}@{second}{port}{tail}");
                            match shown(&raw) {
                                Some(location) => {
                                    shown_count += 1;
                                    assert_eq!(
                                        Some(shown_host(&location)),
                                        libgit2_host(&raw),
                                        "{raw} is shown as {location}"
                                    );
                                }
                                None => refused_count += 1,
                            }
                        }
                    }
                }
            }
        }
    }
    // Both outcomes were exercised, so the check above is not vacuous.
    assert!(shown_count > 50, "{shown_count} shown");
    assert!(refused_count > 50, "{refused_count} refused");
}

#[test]
fn display_location_shows_nothing_for_mistyped_forms_that_carry_a_credential() {
    for secret in [
        "hunter2",
        "pw",
        "p%40ss",
        "ghp_0123456789abcdefghijklmnopqrstuvwxyz",
    ] {
        for raw in [
            // A single slash after the scheme is not a URL; git reads it as
            // host `https` and a path.
            format!("https:/user:{secret}@github.com/o/r.git"),
            format!("git:/user:{secret}@host/r"),
            format!("ssh:/git:{secret}@host/repo"),
            format!("https:\\\\user:pa/{secret}@host\\repo"),
            // A `/` in the user information of an scp-like remote.
            format!("user:pa/{secret}@host:repo.git"),
            format!("oauth2:{secret}/def@gitlab.com:group/repo.git"),
            // A drive-letter lookalike.
            format!("C:/oauth2:{secret}@github.com/repo"),
            format!("C:\\oauth2:{secret}@github.com\\repo"),
            // A path that reads as user information.
            format!("https://github.com/x-access-token:{secret}@github.com/o/r.git"),
            format!("host:path/with{secret}@host"),
            format!("file:///tmp/x-access-token:{secret}@host/repo"),
            // A transport helper's address is a command line.
            format!("ext::sshpass -p {secret} ssh -l git host %S repo"),
            format!("ext::git-remote-foo --auth={secret} %S"),
            format!("gcrypt::rsync://user:{secret}@host/path"),
            format!("persistent-https::https://user:{secret}@host/r"),
            format!("foo::token:{secret}/host/repo"),
        ] {
            assert_eq!(shown(&raw), None, "{raw}");
        }
    }
}

#[test]
fn display_location_refuses_transport_helper_addresses_only() {
    for raw in [
        "ext::sshpass -p hunter2 ssh -l git host %S repo",
        "fd::7,8",
        "foo::bar",
        "a+b.c-d::x",
        "9ext::x",
        "EXT::x",
        "::x",
        "host::path",
    ] {
        assert_eq!(shown(raw), None, "{raw}");
    }
    // `::` elsewhere, and in an IPv6 host, is not a helper.
    for (raw, expected) in [
        ("host:a::b", "host:a::b"),
        ("[::1]:path", "[::1]:path"),
        ("git@[2001:db8::1]:o/r.git", "[2001:db8::1]:o/r.git"),
        ("https://host/a::b", "host/a::b"),
        ("/srv/a::b", "/srv/a::b"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
}

/// Every character of the Unicode general categories Cf, Zl and Zp that the
/// previous hand-written list did not cover, and the ones it did.
fn format_and_separator_characters() -> Vec<char> {
    [
        0x00ad..=0x00ad,
        0x0600..=0x0605,
        0x061c..=0x061c,
        0x06dd..=0x06dd,
        0x070f..=0x070f,
        0x0890..=0x0891,
        0x08e2..=0x08e2,
        0x180e..=0x180e,
        0x200b..=0x200f,
        0x2028..=0x2029,
        0x202a..=0x202e,
        0x2060..=0x2064,
        0x2066..=0x206f,
        0xfeff..=0xfeff,
        0xfff9..=0xfffb,
        0x110bd..=0x110bd,
        0x110cd..=0x110cd,
        0x13430..=0x1343f,
        0x1bca0..=0x1bca3,
        0x1d173..=0x1d17a,
        0xe0001..=0xe0001,
        0xe0020..=0xe007f,
    ]
    .into_iter()
    .flatten()
    .filter_map(char::from_u32)
    .collect()
}

#[test]
fn display_location_refuses_every_control_format_and_separator_character() {
    // `char::is_control` is exactly the category Cc.
    let controls = (0..=0x10ffffu32)
        .filter_map(char::from_u32)
        .filter(|c| c.is_control());
    let others = format_and_separator_characters();
    let mut count = 0;
    for c in controls.chain(others) {
        count += 1;
        for raw in [
            // A local path, an scp-like remote, a URL in its host and in its path.
            format!("/srv/a{c}b"),
            format!("host:a{c}b"),
            format!("https://host/a{c}b"),
            format!("https://ho{c}st/b"),
            // Parts of the string that are not shown are not looked at less.
            format!("https://user{c}@host/b"),
            format!("https://host/b?q={c}z"),
            format!("https://host/b#{c}z"),
            format!("ssh://host/b\u{200d}{c}"),
        ] {
            assert_eq!(shown(&raw), None, "U+{:04X} in {raw:?}", c as u32);
        }
    }
    assert!(count > 200, "{count} characters were tried");
    // The characters the issue names, once more by name.
    for (name, c) in [
        ("Arabic letter mark", '\u{061c}'),
        ("line separator", '\u{2028}'),
        ("paragraph separator", '\u{2029}'),
        ("soft hyphen", '\u{00ad}'),
        ("Mongolian vowel separator", '\u{180e}'),
        ("interlinear annotation anchor", '\u{fff9}'),
        ("interlinear annotation terminator", '\u{fffb}'),
        ("tag latin capital letter a", '\u{e0041}'),
        ("right-to-left override", '\u{202e}'),
        ("zero width space", '\u{200b}'),
        ("escape", '\u{1b}'),
        ("next line", '\u{85}'),
    ] {
        assert_eq!(shown(&format!("host:repo{c}evil")), None, "{name}");
        assert_eq!(shown(&format!("/srv/{c}x")), None, "{name}");
    }
}

#[test]
fn display_location_keeps_text_of_every_script_that_is_not_a_control() {
    for raw in [
        "/srv/r\u{e9}pos/caf\u{e9}.git",
        "/srv/\u{65e5}\u{672c}\u{8a9e}/\u{2713}.git",
        "/srv/\u{5e9}\u{5dc}\u{5d5}\u{5dd}/repo",
        "/srv/\u{645}\u{631}\u{62d}\u{628}\u{627}/repo",
        "/srv/emoji-\u{1f642}.git",
        "host:\u{65e5}\u{672c}/repo",
        // Variation selectors and combining marks are Mn, not Cf.
        "/srv/a\u{fe0f}b",
        "/srv/e\u{301}",
    ] {
        assert_eq!(shown(raw).as_deref(), Some(raw), "{raw}");
    }
}

#[test]
fn the_identifier_rule_is_1_to_256_ascii_letters_digits_and_four_marks() {
    use cvc_core::privacy::is_safe_identifier;

    let longest = "a".repeat(256);
    for id in [
        "a",
        "A1b2C3",
        "client-session_42",
        "harness.session:7",
        "-",
        "_",
        ".",
        ":",
        "3f2b8c1e-5d4a-4b7e-9c0d-1a2b3c4d5e6f",
        longest.as_str(),
    ] {
        assert!(is_safe_identifier(id), "{id:?}");
    }

    let too_long = "a".repeat(257);
    for id in [
        "",
        too_long.as_str(),
        " ",
        "a b",
        "a\tb",
        "a\nb",
        "a/b",
        "a\\b",
        "a@b",
        "a'b",
        "\u{e9}",
        "a\u{200b}",
        "a\u{202e}b",
        "a\u{1b}[2J",
        "emoji \u{1f680}",
    ] {
        assert!(!is_safe_identifier(id), "{id:?}");
    }

    // Byte by byte: of the ASCII characters, exactly the letters, the digits and
    // `.`, `_`, `-` and `:` are allowed.
    for byte in 0u8..=127 {
        let id = format!("a{}b", byte as char);
        let allowed = byte.is_ascii_alphanumeric() || b"._-:".contains(&byte);
        assert_eq!(is_safe_identifier(&id), allowed, "byte {byte:#04x}");
    }
    // No character outside ASCII is, whatever its category.
    for c in [
        '\u{e9}',
        '\u{5e9}',
        '\u{2713}',
        '\u{ff21}',
        '\u{1f680}',
        '\u{a0}',
    ] {
        assert!(!is_safe_identifier(&format!("a{c}b")), "{c:?}");
    }
}

#[test]
fn a_remote_destination_never_prints_its_url() {
    let destination = cvc_core::privacy::RemoteDestination {
        name: "origin".into(),
        effective_url: "https://user:tok-secret@host.example/o/r.git".into(),
        fingerprint: "f".repeat(64),
        ref_name: "refs/cvc/main".into(),
    };
    let debug = format!("{destination:?}");
    assert!(
        debug.contains("origin") && debug.contains(&"f".repeat(64)),
        "{debug}"
    );
    assert!(!debug.contains("tok-secret"), "{debug}");
    assert!(!debug.contains("host.example"), "{debug}");
    // The URL is still there for the code that has to use it.
    assert!(destination.effective_url.contains("tok-secret"));
}
