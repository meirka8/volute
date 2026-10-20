//! The captured fields that hold what the model or a tool said, other than the
//! stimulus (which has its own encoding, see [`crate::stimulus`]): the model's
//! text (`model_response`), its reasoning (`model_cot`) and the arguments of a
//! tool call.
//!
//! The capture boundary scrubs every field and refuses the whole capture when
//! one cannot be scrubbed (more than 256 matches of one detector, or more than
//! 1 MiB). For a transcript that would mean a single field could keep a session
//! from being recorded at all, because the cursor never moves past it. The
//! harness adapter therefore scrubs these fields itself first, with the
//! capture's own policy, and records a fixed note for one that cannot be
//! scrubbed, so the batch and the cursor proceed.
//!
//! # What is recorded for a field that could not be scrubbed
//!
//! | field                       | recorded as                                          |
//! |-----------------------------|------------------------------------------------------|
//! | `model_response`, `model_cot` | `[cvc: withheld N bytes that could not be scrubbed]` |
//! | tool call `arguments`       | `{"bytes":N,"cvc_withheld":true}`                    |
//!
//! `N` is the number of bytes the field held. The arguments form is valid JSON
//! with its keys in alphabetical order, like the `cvc_truncated` form the
//! planner writes for arguments that are too large, and both forms are left
//! unchanged by the scrubber, so they are stable through every later pass.
use crate::privacy::PreparedPolicy;

/// What stands in for a text field of `bytes` bytes that could not be scrubbed.
pub(crate) fn withheld_text(bytes: usize) -> String {
    format!("[cvc: withheld {bytes} bytes that could not be scrubbed]")
}

/// What stands in for tool arguments of `bytes` bytes that could not be
/// scrubbed.
pub(crate) fn withheld_arguments(bytes: usize) -> String {
    format!("{{\"bytes\":{bytes},\"cvc_withheld\":true}}")
}

/// `text` scrubbed with `policy`, or the note that stands in for it.
pub(crate) fn scrubbed_text(text: String, policy: &PreparedPolicy) -> String {
    policy
        .scrub(&text)
        .unwrap_or_else(|_| withheld_text(text.len()))
}

/// Tool-call `arguments` (a JSON document) scrubbed with `policy`, or the
/// note that stands in for them.
pub(crate) fn scrubbed_arguments(arguments: String, policy: &PreparedPolicy) -> String {
    policy
        .scrub_json(&arguments)
        .unwrap_or_else(|_| withheld_arguments(arguments.len()))
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
}
