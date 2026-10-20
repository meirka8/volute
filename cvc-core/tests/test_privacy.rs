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

fn project_fixture_to_ref(repo: &Repository, store: &CvcStore, ref_name: &str, destination: &str) {
    let projection = sync::push_projection_to_ref(repo, store, "", destination).unwrap();
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
    ]
    .iter()
    .map(|fragment| (*fragment).to_owned())
    .chain([marker])
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
        // A header or END line with no key text beside it is only text.
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
    let projection = sync::push_projection_to_ref(&repo, &store, "", "remote").unwrap();
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
