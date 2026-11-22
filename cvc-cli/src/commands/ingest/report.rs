//! What an operator's `cvc ingest <harness>` prints of a run.
use crate::terminal_text::sanitize_for_terminal;
use cvc_core::db::HARNESS_REASONS_SHOWN;
use cvc_core::harness::cursor::CursorCodec;
use cvc_core::harness::engine::EngineReport;
use cvc_core::harness::HarnessId;
use std::collections::BTreeSet;

/// One run, in the figures `cvc ingest` prints: the session's own, with its
/// subagents' summed. For Claude Code these are exactly cvc 0.7.x's
/// `IngestReport` figures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Summary {
    pub(crate) harness: HarnessId,
    pub(crate) session_id: String,
    pub(crate) inserted: usize,
    pub(crate) already_present: usize,
    pub(crate) pending_responses: usize,
    /// Floating responses of the session's own conversation linked to a
    /// commit the session family created, by exact record evidence.
    pub(crate) linked_from_commits: usize,
    /// Where the session's next read resumes, in words (`byte 123`).
    pub(crate) cursor: String,
    /// How long the record was, as far as the run saw.
    pub(crate) record_len: Option<u64>,
    pub(crate) subagent_sessions: usize,
    pub(crate) subagent_inserted: usize,
    pub(crate) subagent_linked: usize,
    pub(crate) unrecorded_blocks: usize,
    /// Sorted, each once.
    pub(crate) unrecorded_block_types: Vec<String>,
    pub(crate) withheld: usize,
    pub(crate) lost: usize,
    pub(crate) withheld_reasons: Vec<String>,
    pub(crate) omitted_tool_calls: usize,
    pub(crate) omitted_context_items: usize,
}

impl Summary {
    pub(crate) fn new<C: CursorCodec>(harness: HarnessId, run: EngineReport<C>) -> Self {
        let root = run.root;
        let mut types: BTreeSet<String> = root.unrecorded.iter().cloned().collect();
        let mut summary = Summary {
            harness,
            session_id: root.conversation_id,
            inserted: root.inserted,
            already_present: root.already_present,
            pending_responses: root.pending,
            linked_from_commits: run.linked.root,
            cursor: root.cursor.describe(),
            record_len: root.record_len,
            subagent_sessions: 0,
            subagent_inserted: 0,
            subagent_linked: run.linked.members,
            unrecorded_blocks: root.unrecorded.len(),
            unrecorded_block_types: Vec::new(),
            withheld: root.withheld,
            lost: root.lost,
            withheld_reasons: root.withheld_reasons,
            omitted_tool_calls: root.omitted_tool_calls,
            omitted_context_items: root.omitted_context_items,
        };
        for member in run.members {
            summary.subagent_sessions += 1;
            summary.subagent_inserted += member.inserted;
            summary.unrecorded_blocks += member.unrecorded.len();
            types.extend(member.unrecorded);
            summary.withheld += member.withheld;
            summary.lost += member.lost;
            for reason in member.withheld_reasons {
                if summary.withheld_reasons.len() < HARNESS_REASONS_SHOWN
                    && !summary.withheld_reasons.contains(&reason)
                {
                    summary.withheld_reasons.push(reason);
                }
            }
            summary.omitted_tool_calls += member.omitted_tool_calls;
            summary.omitted_context_items += member.omitted_context_items;
        }
        summary.unrecorded_block_types = types.into_iter().collect();
        summary
    }
}

/// What a run prints. The session id passed the capture policy's identifier
/// rule, but the block types are whatever the record's `type` fields say, so
/// both are shown with every control and format character replaced by `?`,
/// each on the line it belongs to; so is the cursor's description, which an
/// adapter words.
pub(crate) fn lines(summary: &Summary) -> Vec<String> {
    let cursor = match summary.record_len {
        Some(len) => format!(
            "cursor at {} of {len}",
            sanitize_for_terminal(&summary.cursor)
        ),
        None => format!("cursor at {}", sanitize_for_terminal(&summary.cursor)),
    };
    let mut lines = vec![format!(
        "CVC: {} session {}: {} new thought(s), {} already recorded, {} pending, {} linked to session commits; {cursor}",
        summary.harness.display(),
        sanitize_for_terminal(&summary.session_id),
        summary.inserted,
        summary.already_present,
        summary.pending_responses,
        summary.linked_from_commits,
    )];
    if summary.subagent_sessions > 0 {
        lines.push(format!(
            "  plus {} subagent session(s): {} new thought(s), {} linked to commits",
            summary.subagent_sessions, summary.subagent_inserted, summary.subagent_linked
        ));
    }
    if summary.unrecorded_blocks > 0 {
        let types: Vec<_> = summary
            .unrecorded_block_types
            .iter()
            .map(|kind| sanitize_for_terminal(kind))
            .collect();
        lines.push(format!(
            "  {} content block(s) of a type this version does not record were kept as placeholders: {}",
            summary.unrecorded_blocks,
            types.join(", ")
        ));
    }
    lines.extend(left_out_lines(summary));
    lines
}

/// What the capture boundary could not keep, as lines to print, so that a
/// response recorded as a stand-in, or left out, is never silent. None of the
/// text of such a response is in them, only the limit that was hit.
fn left_out_lines(summary: &Summary) -> Vec<String> {
    let mut lines = Vec::new();
    if summary.withheld > 0 || summary.lost > 0 {
        let mut parts = Vec::new();
        if summary.withheld > 0 {
            parts.push(format!(
                "{} recorded as a stand-in that says its response was withheld",
                summary.withheld
            ));
        }
        if summary.lost > 0 {
            parts.push(format!("{} not recorded at all", summary.lost));
        }
        lines.push(format!(
            "  could not scrub {} response(s) within the limits: {} ({})",
            summary.withheld + summary.lost,
            parts.join(", "),
            sanitize_for_terminal(&summary.withheld_reasons.join("; "))
        ));
    }
    if summary.omitted_tool_calls > 0 || summary.omitted_context_items > 0 {
        lines.push(format!(
            "  left out of the responses recorded now: {} tool call(s) beyond the 255 a response records, {} context item(s) (a path that cannot be scrubbed, or beyond the 256 a response records); each such response ends with a cvc-omitted record that says so",
            summary.omitted_tool_calls, summary.omitted_context_items
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use cvc_core::harness::cursor::JsonlCursor;
    use cvc_core::harness::drift::Drift;
    use cvc_core::harness::engine::TargetReport;
    use cvc_core::harness::family::FamilyLinks;

    fn summary(session_id: &str, unrecorded_block_types: &[&str]) -> Summary {
        Summary {
            harness: HarnessId::ClaudeCode,
            session_id: session_id.into(),
            inserted: 2,
            already_present: 1,
            pending_responses: 0,
            linked_from_commits: 0,
            cursor: "byte 10".into(),
            record_len: Some(10),
            subagent_sessions: 0,
            subagent_inserted: 0,
            subagent_linked: 0,
            unrecorded_blocks: unrecorded_block_types.len(),
            unrecorded_block_types: unrecorded_block_types
                .iter()
                .map(|kind| (*kind).to_owned())
                .collect(),
            withheld: 0,
            lost: 0,
            withheld_reasons: Vec::new(),
            omitted_tool_calls: 0,
            omitted_context_items: 0,
        }
    }

    fn clean() -> Summary {
        Summary {
            inserted: 3,
            already_present: 0,
            ..summary("s", &[])
        }
    }

    #[test]
    fn the_report_shows_the_session_id_and_the_block_types_as_plain_text() {
        let hostile = summary(
            "s\u{1b}[2J\u{202e}1",
            &[
                "mys\u{1b}[2Jtery\u{1b}]52;c;SGVsbG8=\u{7}",
                "x\u{202e}y\u{2028}z",
            ],
        );
        assert_eq!(
            lines(&hostile),
            [
                "CVC: Claude Code session s?[2J?1: 2 new thought(s), 1 already recorded, 0 pending, 0 linked to session commits; cursor at byte 10 of 10",
                "  2 content block(s) of a type this version does not record were kept as placeholders: mys?[2Jtery?]52;c;SGVsbG8=?, x?y?z",
            ]
        );
        // What a run of a well-formed transcript reports is printed as it is.
        assert_eq!(
            lines(&summary("7c1e9f5a-2b2f", &["mystery"]))[1],
            "  1 content block(s) of a type this version does not record were kept as placeholders: mystery"
        );
        assert_eq!(lines(&summary("7c1e9f5a-2b2f", &[])).len(), 1);
    }

    /// The family rescue links the session's own responses and its subagents'
    /// apart, and a rescan that records nothing new still says what it linked.
    #[test]
    fn links_to_the_session_and_to_its_subagents_are_reported_apart() {
        let printed = lines(&Summary {
            inserted: 0,
            already_present: 12,
            linked_from_commits: 2,
            subagent_sessions: 3,
            subagent_inserted: 0,
            subagent_linked: 7,
            ..clean()
        });
        assert_eq!(
            printed,
            [
                "CVC: Claude Code session s: 0 new thought(s), 12 already recorded, 0 pending, 2 linked to session commits; cursor at byte 10 of 10",
                "  plus 3 subagent session(s): 0 new thought(s), 7 linked to commits",
            ]
        );
    }

    #[test]
    fn the_cursor_is_shown_as_plain_text() {
        let printed = lines(&Summary {
            cursor: "row \u{1b}[2J1".into(),
            record_len: None,
            ..clean()
        });
        assert!(printed[0].ends_with("; cursor at row ?[2J1"), "{printed:?}");
    }

    #[test]
    fn a_clean_ingest_prints_nothing_about_what_was_left_out() {
        assert!(left_out_lines(&clean()).is_empty());
    }

    #[test]
    fn withheld_lost_and_omitted_responses_are_said_with_their_reasons_and_no_text() {
        let printed = left_out_lines(&Summary {
            withheld: 2,
            lost: 1,
            withheld_reasons: vec![
                "capture collection exceeds limit".into(),
                "too many secret matches".into(),
            ],
            omitted_tool_calls: 45,
            omitted_context_items: 44,
            ..clean()
        });
        assert_eq!(printed.len(), 2, "{printed:?}");
        assert_eq!(
            printed[0],
            "  could not scrub 3 response(s) within the limits: 2 recorded as a stand-in that says its response was withheld, 1 not recorded at all (capture collection exceeds limit; too many secret matches)"
        );
        assert!(printed[1].contains("45 tool call(s)"), "{printed:?}");
        assert!(printed[1].contains("44 context item(s)"), "{printed:?}");
        assert!(printed[1].contains("cvc-omitted"), "{printed:?}");
        // Only counts and limits: never any text of a response.
        let only_omitted = left_out_lines(&Summary {
            omitted_context_items: 1,
            ..clean()
        });
        assert_eq!(only_omitted.len(), 1);
    }

    fn target(id: &str, unrecorded: &[&str], reasons: &[&str]) -> TargetReport<JsonlCursor> {
        TargetReport {
            conversation_id: id.into(),
            inserted: 1,
            already_present: 2,
            pending: 3,
            cursor: JsonlCursor::start("/t.jsonl").at(40),
            cursor_held: false,
            record_len: Some(50),
            unrecorded: unrecorded.iter().map(|kind| (*kind).to_owned()).collect(),
            drift: Drift::default(),
            withheld: 1,
            lost: 0,
            withheld_reasons: reasons.iter().map(|reason| (*reason).to_owned()).collect(),
            withheld_fields: 0,
            omitted_tool_calls: 4,
            omitted_context_items: 5,
            relations_recorded: 0,
            relations_skipped: 0,
        }
    }

    /// The figures 0.7.x's `IngestReport` gives the same run: the root's own,
    /// the members summed, block types sorted and each once, reasons in the
    /// order met and each once.
    #[test]
    fn a_run_is_summed_as_cvc_0_7_summed_it() {
        let run = EngineReport {
            root: target("s", &["zeta", "alpha", "zeta"], &["r1"]),
            members: vec![
                target("s:agent-a", &["beta", "alpha"], &["r2", "r1"]),
                target("s:agent-b", &[], &[]),
            ],
            linked: FamilyLinks {
                root: 2,
                members: 3,
            },
            rescue_skipped: false,
        };
        let summary = Summary::new(HarnessId::ClaudeCode, run);
        assert_eq!(summary.session_id, "s");
        assert_eq!(summary.cursor, "byte 40");
        assert_eq!(summary.record_len, Some(50));
        assert_eq!(
            (
                summary.inserted,
                summary.already_present,
                summary.pending_responses
            ),
            (1, 2, 3)
        );
        assert_eq!(
            (
                summary.subagent_sessions,
                summary.subagent_inserted,
                summary.linked_from_commits,
                summary.subagent_linked
            ),
            (2, 2, 2, 3)
        );
        assert_eq!(summary.unrecorded_blocks, 5);
        assert_eq!(summary.unrecorded_block_types, ["alpha", "beta", "zeta"]);
        assert_eq!(summary.withheld_reasons, ["r1", "r2"]);
        assert_eq!((summary.withheld, summary.lost), (3, 0));
        assert_eq!(
            (summary.omitted_tool_calls, summary.omitted_context_items),
            (12, 15)
        );
        assert_eq!(
            lines(&summary)[0],
            "CVC: Claude Code session s: 1 new thought(s), 2 already recorded, 3 pending, 2 linked to session commits; cursor at byte 40 of 50"
        );
    }
}
