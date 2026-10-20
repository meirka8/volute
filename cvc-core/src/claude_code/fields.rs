//! The captured fields that hold what the model or a tool said, other than the
//! stimulus (which has its own encoding, see [`crate::stimulus`]): the model's
//! text (`model_response`), its reasoning (`model_cot`), the arguments of a
//! tool call, and a title taken from the transcript.
//!
//! The planner hands these over raw. Each is scrubbed here, in full, with the
//! capture's own policy, and only then bounded, so a size limit never cuts a
//! secret in half and leaves the front of it behind.
//!
//! # Bounds (counted on the scrubbed text)
//!
//! | field                          | kept                                                   |
//! |--------------------------------|--------------------------------------------------------|
//! | `model_response`, `model_cot`  | the first 256 KiB, then `\n…[cvc: truncated N bytes]`  |
//! | a string in tool arguments     | the first 16 KiB, in place, with the same note inside it |
//! | one call's arguments           | at most 64 KiB, else `{"bytes":N,"cvc_truncated":true}` |
//! | all arguments of one response  | at most 2 MiB, else the same form for the calls past it |
//! | a title taken from the transcript | the first 120 bytes, then the same note               |
//!
//! A cut never lands inside a character or a `[CVC_REDACTED:v1:…]` marker.
//!
//! # A field the scrubber cannot process
//!
//! The capture boundary refuses the whole capture when one field cannot be
//! scrubbed (more than 256 matches of one detector, or more than 1 MiB). For a
//! transcript that would mean a single field could keep a session from being
//! recorded at all, because the cursor never moves past it. A field that cannot
//! be scrubbed is therefore recorded as a note, and the batch and the cursor
//! proceed:
//!
//! | field                         | recorded as                                          |
//! |-------------------------------|------------------------------------------------------|
//! | `model_response`, `model_cot` | `[cvc: withheld N bytes that could not be scrubbed]` |
//! | tool call `arguments`         | `{"bytes":N,"cvc_withheld":true}`                    |
//! | a string over 1 MiB inside tool arguments | that string alone is replaced by the text note |
//! | a title                       | none: the next source of a title is used             |
//!
//! `N` is the number of bytes the field held. The arguments forms are valid JSON
//! with their keys in alphabetical order, and both they and the text note are
//! left unchanged by the scrubber, so they are stable through every later pass.
//!
//! # Collections
//!
//! A capture holds at most 256 tool executions and 256 context items, and the
//! capture boundary refuses one that holds more. A response with more calls than
//! that (a model can make thousands) would then keep the session from being
//! recorded, for the same reason. A response is therefore captured with its first
//! 255 tool calls and, when any call or context item was left out, one last
//! record that says how many: a tool execution named `cvc-omitted` whose
//! arguments are `{"context_items":M,"cvc_omitted":true,"tool_calls":N}`. A
//! context item is one path inside the worktree that a call read or edited. It is
//! recorded scrubbed, and one whose path cannot be scrubbed (more than 256
//! matches, or more than 1 MiB) is left out and counted, rather than failing the
//! capture. A path the policy ignores is not recorded and not counted.
use crate::models::{ContextItem, InteractionId, ToolExecution, ToolStatus};
use crate::privacy::{PreparedPolicy, MAX_CONTEXT, MAX_TEXT, MAX_TOOLS};
use crate::stimulus;
use serde_json::Value;

pub(crate) const MAX_TEXT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_ARGUMENT_STRING_BYTES: usize = 16 * 1024;
pub(crate) const MAX_ARGUMENTS_BYTES: usize = 64 * 1024;
pub(crate) const MAX_ARGUMENTS_TOTAL_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_TITLE_BYTES: usize = 120;
/// What one response records of its tool calls and of its context items.
pub(crate) const MAX_TOOL_EXECUTIONS: usize = MAX_TOOLS;
pub(crate) const MAX_CONTEXT_ITEMS: usize = MAX_CONTEXT;
/// The protocol and name of the record that says calls or context items were
/// left out.
pub(crate) const OMITTED_PROTOCOL: &str = "cvc";
pub(crate) const OMITTED_TOOL_NAME: &str = "cvc-omitted";

/// What stands in for a text field of `bytes` bytes that could not be scrubbed.
pub(crate) fn withheld_text(bytes: usize) -> String {
    format!("[cvc: withheld {bytes} bytes that could not be scrubbed]")
}

/// What stands in for tool arguments of `bytes` bytes that could not be
/// scrubbed.
pub(crate) fn withheld_arguments(bytes: usize) -> String {
    format!("{{\"bytes\":{bytes},\"cvc_withheld\":true}}")
}

/// What stands in for tool arguments of `bytes` bytes that are too large to keep.
fn truncated_arguments(bytes: usize) -> String {
    format!("{{\"bytes\":{bytes},\"cvc_truncated\":true}}")
}

/// `text` cut to at most `max` bytes, never inside a character or a redaction
/// marker, with a note of how many bytes were cut. For text that has already
/// been scrubbed.
fn bounded(text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let end = stimulus::head_len(&text, max);
    format!(
        "{}\n…[cvc: truncated {} bytes]",
        &text[..end],
        text.len() - end
    )
}

/// `text` scrubbed with `policy`, or the note that stands in for it.
pub(crate) fn scrubbed_text(text: String, policy: &PreparedPolicy) -> String {
    policy
        .scrub(&text)
        .unwrap_or_else(|_| withheld_text(text.len()))
}

/// The model's text or reasoning as it is captured: scrubbed in full, then
/// bounded.
pub(crate) fn captured_text(text: String, policy: &PreparedPolicy) -> String {
    bounded(scrubbed_text(text, policy), MAX_TEXT_BYTES)
}

/// A title from the transcript as it is captured, or `None` when it cannot be
/// scrubbed.
pub(crate) fn captured_title(title: &str, policy: &PreparedPolicy) -> Option<String> {
    policy
        .scrub(title)
        .ok()
        .map(|scrubbed| bounded(scrubbed, MAX_TITLE_BYTES))
}

/// Tool-call `arguments` (a JSON document) scrubbed with `policy`, or the note
/// that stands in for them.
pub(crate) fn scrubbed_arguments(arguments: String, policy: &PreparedPolicy) -> String {
    policy
        .scrub_json(&arguments)
        .unwrap_or_else(|_| withheld_arguments(arguments.len()))
}

/// The arguments of one tool call as they are captured: scrubbed in full, then
/// kept valid JSON within the bounds. `budget` is what is left of the
/// response's total.
pub(crate) fn captured_arguments(
    arguments: String,
    budget: &mut usize,
    policy: &PreparedPolicy,
) -> String {
    let size = arguments.len();
    let Ok(mut value) = serde_json::from_str::<Value>(&arguments) else {
        return scrubbed_arguments(arguments, policy);
    };
    // A string the scrubber cannot take at all is withheld on its own, so the
    // rest of the call (the path, the command) is kept.
    withhold_oversized_strings(&mut value);
    let Ok(scrubbed) = policy.scrub_json(&value.to_string()) else {
        return withheld_arguments(size);
    };
    let Ok(mut value) = serde_json::from_str::<Value>(&scrubbed) else {
        return withheld_arguments(size);
    };
    bound_strings(&mut value);
    let bounded = value.to_string();
    if bounded.len() > MAX_ARGUMENTS_BYTES || bounded.len() > *budget {
        return truncated_arguments(size);
    }
    *budget -= bounded.len();
    bounded
}

/// The arguments of the record that says what a response left out.
pub(crate) fn omitted_arguments(tool_calls: usize, context_items: usize) -> String {
    format!(
        "{{\"context_items\":{context_items},\"cvc_omitted\":true,\"tool_calls\":{tool_calls}}}"
    )
}

/// The tool calls of one response as they are captured: each one's arguments
/// scrubbed and bounded (see [`captured_arguments`]), all of them drawing on one
/// budget, and at most [`MAX_TOOL_EXECUTIONS`] records. When a call was left out,
/// or `left_out_context_items` says an item was, the last record is the
/// `cvc-omitted` note. Returns the records and how many calls were left out.
pub(crate) fn captured_tool_executions(
    tools: Vec<ToolExecution>,
    left_out_context_items: usize,
    interaction_id: &InteractionId,
    policy: &PreparedPolicy,
) -> (Vec<ToolExecution>, usize) {
    let note = tools.len() > MAX_TOOL_EXECUTIONS || left_out_context_items > 0;
    let room = MAX_TOOL_EXECUTIONS - usize::from(note);
    let left_out = tools.len().saturating_sub(room);
    let mut budget = MAX_ARGUMENTS_TOTAL_BYTES;
    let mut captured: Vec<ToolExecution> = tools
        .into_iter()
        .take(room)
        .map(|mut tool| {
            tool.arguments = captured_arguments(tool.arguments, &mut budget, policy);
            tool
        })
        .collect();
    if note {
        captured.push(ToolExecution {
            id: None,
            interaction_id: interaction_id.clone(),
            tool_protocol: OMITTED_PROTOCOL.to_owned(),
            tool_name: OMITTED_TOOL_NAME.to_owned(),
            arguments: omitted_arguments(left_out, left_out_context_items),
            status: ToolStatus::Success,
        });
    }
    (captured, left_out)
}

/// The context items of one response as they are captured: the path of each
/// scrubbed with the capture's policy, at most [`MAX_CONTEXT_ITEMS`] of them. An
/// item whose path the policy ignores is not recorded and not counted. One whose
/// path cannot be scrubbed is left out, and so is each one past the limit.
/// Returns the items and how many were left out.
pub(crate) fn captured_context_items(
    items: Vec<ContextItem>,
    policy: &PreparedPolicy,
) -> (Vec<ContextItem>, usize) {
    let mut kept = Vec::new();
    let mut left_out = 0usize;
    for mut item in items {
        if policy.ignores_path(&item.file_path) {
            continue;
        }
        if kept.len() == MAX_CONTEXT_ITEMS {
            left_out += 1;
            continue;
        }
        match policy.scrub(&item.file_path) {
            Ok(path) => {
                item.file_path = path;
                kept.push(item);
            }
            Err(_) => left_out += 1,
        }
    }
    (kept, left_out)
}

fn withhold_oversized_strings(value: &mut Value) {
    match value {
        Value::String(text) if text.len() > MAX_TEXT => *text = withheld_text(text.len()),
        Value::Array(items) => items.iter_mut().for_each(withhold_oversized_strings),
        Value::Object(fields) => fields.values_mut().for_each(withhold_oversized_strings),
        _ => {}
    }
}

fn bound_strings(value: &mut Value) {
    match value {
        Value::String(text) if text.len() > MAX_ARGUMENT_STRING_BYTES => {
            *text = bounded(std::mem::take(text), MAX_ARGUMENT_STRING_BYTES);
        }
        Value::Array(items) => items.iter_mut().for_each(bound_strings),
        Value::Object(fields) => fields.values_mut().for_each(bound_strings),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privacy::{scrub, scrub_json, validate_sanitized_json};

    fn noisy() -> String {
        (0..300)
            .map(|n| format!("password=hunter2hunter{n}\n"))
            .collect()
    }

    #[test]
    fn the_notes_are_exactly_these() {
        assert_eq!(
            withheld_text(1234),
            "[cvc: withheld 1234 bytes that could not be scrubbed]"
        );
        assert_eq!(
            withheld_arguments(99),
            r#"{"bytes":99,"cvc_withheld":true}"#
        );
    }

    #[test]
    fn the_notes_survive_every_later_scrub_unchanged() {
        let text = withheld_text(7_000_000);
        assert_eq!(scrub(&text).unwrap(), text);
        let arguments = withheld_arguments(7_000_000);
        assert_eq!(scrub_json(&arguments).unwrap(), arguments);
        assert!(validate_sanitized_json(&arguments).is_ok());
        let value: serde_json::Value = serde_json::from_str(&arguments).unwrap();
        assert_eq!(value["bytes"], 7_000_000);
        assert_eq!(value["cvc_withheld"], true);
    }

    #[test]
    fn a_field_that_scrubs_is_scrubbed_and_one_that_cannot_is_withheld() {
        let policy = PreparedPolicy::built_ins_only();
        let key = "key AKIAABCDEFGHIJKLMNOP here".to_owned();
        let scrubbed = scrubbed_text(key, &policy);
        assert!(scrubbed.starts_with("key [CVC_REDACTED:v1:aws_access_key:"));

        let text = noisy();
        assert_eq!(
            scrubbed_text(text.clone(), &policy),
            format!(
                "[cvc: withheld {} bytes that could not be scrubbed]",
                text.len()
            )
        );
        let arguments = format!(r#"{{"content":{}}}"#, serde_json::to_string(&text).unwrap());
        assert_eq!(
            scrubbed_arguments(arguments.clone(), &policy),
            format!(r#"{{"bytes":{},"cvc_withheld":true}}"#, arguments.len())
        );
        // Over the scrubber's size limit is a failure too.
        let huge = "x".repeat(1024 * 1024 + 1);
        assert_eq!(
            scrubbed_text(huge.clone(), &policy),
            withheld_text(huge.len())
        );
    }

    #[test]
    fn the_limit_is_more_than_256_matches() {
        let policy = PreparedPolicy::built_ins_only();
        let lines = |count: usize| -> String {
            (0..count)
                .map(|n| format!("password=hunter2hunter{n}\n"))
                .collect()
        };
        assert!(!scrubbed_text(lines(256), &policy).starts_with("[cvc: withheld"));
        assert!(scrubbed_text(lines(257), &policy).starts_with("[cvc: withheld"));
    }

    const KEY: &str = "AKIAIOSFODNN7EXAMPLE";

    /// A text whose key id starts `before` bytes in (after a space, which the
    /// detector needs), so a bound of `before + k` falls inside it.
    fn text_with_key_at(before: usize) -> String {
        format!("{} {KEY} and some words after it", "x".repeat(before - 1))
    }

    #[test]
    fn text_is_scrubbed_before_it_is_cut_so_no_part_of_a_key_survives() {
        let policy = PreparedPolicy::built_ins_only();
        // The key crosses the bound.
        let captured = captured_text(text_with_key_at(MAX_TEXT_BYTES - 10), &policy);
        assert!(
            !captured.contains("AKIAIOSFO"),
            "{}",
            &captured[captured.len() - 200..]
        );
        assert!(captured.len() <= MAX_TEXT_BYTES + 64, "{}", captured.len());
        assert!(captured.contains("…[cvc: truncated "));
    }

    #[test]
    fn the_cut_is_counted_on_the_scrubbed_text_and_never_splits_a_character_or_marker() {
        let policy = PreparedPolicy::built_ins_only();
        // Short text is untouched.
        assert_eq!(captured_text("héllo".into(), &policy), "héllo");
        // Multibyte text is cut on a boundary and the count adds up exactly.
        let text = "é".repeat(MAX_TEXT_BYTES); // twice the bound in bytes
        let captured = captured_text(text.clone(), &policy);
        let (kept, note) = captured.split_once("\n…[cvc: truncated ").unwrap();
        let removed: usize = note.trim_end_matches(" bytes]").parse().unwrap();
        assert_eq!(kept.len() + removed, text.len());
        assert!(kept.len() <= MAX_TEXT_BYTES && kept.chars().all(|c| c == 'é'));
        // A marker on the bound moves the cut in front of it.
        let on_the_bound = format!(
            "{} {KEY} {}",
            "y".repeat(MAX_TEXT_BYTES - 6),
            "z".repeat(1000)
        );
        let captured = captured_text(on_the_bound, &policy);
        let (kept, _) = captured.split_once("\n…[cvc: truncated ").unwrap();
        assert!(
            kept.chars().all(|c| c == 'y' || c == ' '),
            "the marker was split"
        );
        assert!(
            !captured.contains("CVC_REDACTED"),
            "the marker is in the cut part"
        );
    }

    #[test]
    fn a_title_is_scrubbed_before_it_is_cut_and_none_when_it_cannot_be() {
        let policy = PreparedPolicy::built_ins_only();
        let title = captured_title(&text_with_key_at(MAX_TITLE_BYTES - 10), &policy).unwrap();
        assert!(!title.contains("AKIA"), "{title}");
        assert_eq!(
            captured_title("Greeting work", &policy).unwrap(),
            "Greeting work"
        );
        assert_eq!(captured_title(&noisy(), &policy), None);
    }

    #[test]
    fn strings_in_arguments_are_cut_in_place_after_scrubbing_and_stay_valid_json() {
        let policy = PreparedPolicy::built_ins_only();
        let mut budget = MAX_ARGUMENTS_TOTAL_BYTES;
        let input = serde_json::json!({
            "file_path": "/opt/x.txt",
            "content": text_with_key_at(MAX_ARGUMENT_STRING_BYTES - 10),
        });
        let captured = captured_arguments(input.to_string(), &mut budget, &policy);
        let value: Value = serde_json::from_str(&captured).unwrap();
        assert_eq!(value["file_path"], "/opt/x.txt");
        let content = value["content"].as_str().unwrap();
        assert!(!content.contains("AKIAIOSFO"), "a key prefix was kept");
        assert!(content.contains("…[cvc: truncated "));
        assert_eq!(budget, MAX_ARGUMENTS_TOTAL_BYTES - captured.len());
        // The capture boundary leaves it exactly as it is.
        assert_eq!(scrub_json(&captured).unwrap(), captured);
        assert!(validate_sanitized_json(&captured).is_ok());
    }

    #[test]
    fn arguments_over_the_limits_become_the_truncated_form_and_the_budget_is_shared() {
        let policy = PreparedPolicy::built_ins_only();
        // Many strings, each under its own bound, together over one call's.
        let many: Vec<String> = (0..10)
            .map(|n| format!("{n}{}", "m".repeat(10_000)))
            .collect();
        let input = serde_json::json!({ "items": many }).to_string();
        let mut budget = MAX_ARGUMENTS_TOTAL_BYTES;
        assert_eq!(
            captured_arguments(input.clone(), &mut budget, &policy),
            format!(r#"{{"bytes":{},"cvc_truncated":true}}"#, input.len())
        );
        assert_eq!(
            budget, MAX_ARGUMENTS_TOTAL_BYTES,
            "a truncated call spends nothing"
        );
        // A response's calls draw on one budget.
        let small = serde_json::json!({"file_path": "/opt/a"}).to_string();
        let mut budget = small.len() + 5;
        assert_eq!(
            captured_arguments(small.clone(), &mut budget, &policy),
            small
        );
        assert_eq!(budget, 5);
        assert_eq!(
            captured_arguments(small.clone(), &mut budget, &policy),
            format!(r#"{{"bytes":{},"cvc_truncated":true}}"#, small.len())
        );
    }

    #[test]
    fn a_string_the_scrubber_cannot_take_is_withheld_alone_and_the_rest_of_the_call_is_kept() {
        let policy = PreparedPolicy::built_ins_only();
        let mut budget = MAX_ARGUMENTS_TOTAL_BYTES;
        let huge = "z".repeat(MAX_TEXT + 1);
        let input = serde_json::json!({"file_path": "/opt/big.bin", "content": huge}).to_string();
        let captured = captured_arguments(input, &mut budget, &policy);
        let value: Value = serde_json::from_str(&captured).unwrap();
        assert_eq!(value["file_path"], "/opt/big.bin");
        assert_eq!(value["content"], withheld_text(MAX_TEXT + 1));

        // Too many matches in one string is the whole-call form.
        let noisy_input =
            serde_json::json!({"file_path": "/opt/x", "content": noisy()}).to_string();
        let mut budget = MAX_ARGUMENTS_TOTAL_BYTES;
        assert_eq!(
            captured_arguments(noisy_input.clone(), &mut budget, &policy),
            withheld_arguments(noisy_input.len())
        );
        assert_eq!(budget, MAX_ARGUMENTS_TOTAL_BYTES);
    }

    fn call(n: usize, id: &InteractionId) -> ToolExecution {
        ToolExecution {
            id: None,
            interaction_id: id.clone(),
            tool_protocol: "native".into(),
            tool_name: "Bash".into(),
            arguments: format!(r#"{{"command":"echo {n}"}}"#),
            status: ToolStatus::Success,
        }
    }

    fn item(path: &str, id: &InteractionId) -> ContextItem {
        ContextItem {
            id: None,
            interaction_id: id.clone(),
            file_path: path.into(),
            git_blob_sha: None,
            dirty_patch: None,
            start_line: None,
            end_line: None,
        }
    }

    #[test]
    fn up_to_the_limit_of_calls_is_recorded_whole_and_one_more_is_cut_with_a_note() {
        let policy = PreparedPolicy::built_ins_only();
        let id = InteractionId::new();
        let calls = |count: usize| (0..count).map(|n| call(n, &id)).collect::<Vec<_>>();

        let (kept, left_out) = captured_tool_executions(calls(256), 0, &id, &policy);
        assert_eq!((kept.len(), left_out), (256, 0));
        assert!(kept.iter().all(|tool| tool.tool_name == "Bash"));

        let (kept, left_out) = captured_tool_executions(calls(257), 0, &id, &policy);
        assert_eq!((kept.len(), left_out), (256, 2), "255 calls and the note");
        assert_eq!(kept[254].arguments, r#"{"command":"echo 254"}"#);
        let note = &kept[255];
        assert_eq!(
            (note.tool_protocol.as_str(), note.tool_name.as_str()),
            ("cvc", "cvc-omitted")
        );
        assert_eq!(
            note.arguments,
            r#"{"context_items":0,"cvc_omitted":true,"tool_calls":2}"#
        );
        assert_eq!(note.status, ToolStatus::Success);
        assert_eq!(note.interaction_id, id);

        let (kept, left_out) = captured_tool_executions(calls(10_000), 0, &id, &policy);
        assert_eq!((kept.len(), left_out), (256, 9_745));
    }

    #[test]
    fn left_out_context_items_are_noted_even_when_every_call_fits() {
        let policy = PreparedPolicy::built_ins_only();
        let id = InteractionId::new();
        let (kept, left_out) = captured_tool_executions(vec![call(0, &id)], 3, &id, &policy);
        assert_eq!((kept.len(), left_out), (2, 0));
        assert_eq!(
            kept[1].arguments,
            r#"{"context_items":3,"cvc_omitted":true,"tool_calls":0}"#
        );
        // With nothing left out there is no note.
        let (kept, _) = captured_tool_executions(vec![call(0, &id)], 0, &id, &policy);
        assert_eq!(kept.len(), 1);
        let (kept, _) = captured_tool_executions(Vec::new(), 0, &id, &policy);
        assert!(kept.is_empty());
        // A response with 256 calls that also left a context item out gives up
        // one call for the note.
        let calls: Vec<_> = (0..256).map(|n| call(n, &id)).collect();
        let (kept, left_out) = captured_tool_executions(calls, 1, &id, &policy);
        assert_eq!((kept.len(), left_out), (256, 1));
    }

    #[test]
    fn the_omitted_note_survives_every_later_scrub_unchanged() {
        let arguments = omitted_arguments(45, 44);
        assert_eq!(
            arguments,
            r#"{"context_items":44,"cvc_omitted":true,"tool_calls":45}"#
        );
        assert_eq!(scrub_json(&arguments).unwrap(), arguments);
        assert!(validate_sanitized_json(&arguments).is_ok());
        let policy = PreparedPolicy::built_ins_only();
        assert_eq!(policy.scrub_json(&arguments).unwrap(), arguments);
        for name in [OMITTED_PROTOCOL, OMITTED_TOOL_NAME] {
            assert_eq!(scrub(name).unwrap(), name);
        }
    }

    #[test]
    fn a_context_path_is_scrubbed_a_policy_ignored_one_is_dropped_and_an_unscrubbable_one_is_counted(
    ) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".thoughtignore"),
            "path:secrets
",
        )
        .unwrap();
        let policy = PreparedPolicy::load(dir.path()).unwrap();
        let id = InteractionId::new();
        let hostile = (0..300)
            .map(|n| format!("token=hunter2hunter{n} "))
            .collect::<String>();
        let (kept, left_out) = captured_context_items(
            vec![
                item("src/lib.rs", &id),
                item("secrets/key.pem", &id),
                item(&hostile, &id),
                item("src/token=abcdefgh1234.txt", &id),
                item("a/b.rs", &id),
            ],
            &policy,
        );
        let paths: Vec<&str> = kept.iter().map(|item| item.file_path.as_str()).collect();
        assert_eq!(paths.len(), 3, "{paths:?}");
        assert_eq!(paths[0], "src/lib.rs");
        assert!(paths[1].starts_with("src/[CVC_REDACTED:v1:secret_assignment:"));
        assert_eq!(paths[2], "a/b.rs");
        // The ignored path is not counted; the hostile one is.
        assert_eq!(left_out, 1);
    }

    #[test]
    fn context_items_past_the_limit_are_counted_and_not_scrubbed() {
        let policy = PreparedPolicy::built_ins_only();
        let id = InteractionId::new();
        let items: Vec<_> = (0..300)
            .map(|n| item(&format!("src/f{n}.rs"), &id))
            .collect();
        let (kept, left_out) = captured_context_items(items, &policy);
        assert_eq!((kept.len(), left_out), (256, 44));
        assert_eq!(kept[255].file_path, "src/f255.rs");
        // A path that cannot be scrubbed past the limit is counted, not read.
        let mut items: Vec<_> = (0..256)
            .map(|n| item(&format!("src/f{n}.rs"), &id))
            .collect();
        items.push(item(&"z".repeat(MAX_TEXT + 1), &id));
        let (kept, left_out) = captured_context_items(items, &policy);
        assert_eq!((kept.len(), left_out), (256, 1));
    }

    #[test]
    fn credential_keys_are_still_replaced_in_the_captured_arguments() {
        let policy = PreparedPolicy::built_ins_only();
        let mut budget = MAX_ARGUMENTS_TOTAL_BYTES;
        let input = serde_json::json!({"api_key": "plain-not-a-real-key-1234", "query": "q"});
        let captured = captured_arguments(input.to_string(), &mut budget, &policy);
        assert!(!captured.contains("plain-not-a-real-key"), "{captured}");
        assert!(captured.contains(r#""query":"q""#), "{captured}");
    }
}
