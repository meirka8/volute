//! Scrub, then bound, then mark: the one way a [`CaptureCandidate`] becomes a
//! capture, for every harness.
//!
//! A planner hands everything over raw. `seal` runs each field through the
//! capture's own policy in full and only then bounds it (see
//! [`crate::harness::fields`] for the bounds and the notes that stand in for
//! what cannot be scrubbed), so a size limit never cuts a secret in half and
//! leaves the front of it behind. Each step works on what the previous step
//! left, in this order:
//!
//! 1. the stimulus: [`crate::stimulus::render`], each part scrubbed in full,
//!    then bounded, then marked;
//! 2. the response: scrubbed, then bounded to 256 KiB;
//! 3. the reasoning: scrubbed, then bounded, and only then, after that last
//!    policy scrub, marked with its kind ([`crate::reasoning::mark`], which
//!    takes the [`SealedText`] only this module makes). A field the policy
//!    could not scrub is the withheld note, and is never marked: the seal
//!    knows it withheld the field from the failed scrub itself;
//! 4. the context items: each path scrubbed, policy-ignored paths dropped, at
//!    most 256;
//! 5. the tool calls: inner calls placed right after the call they ran inside,
//!    then each call's arguments scrubbed and bounded, at most 255 calls plus
//!    the `cvc-omitted` record when anything was left out.
//!
//! The result is a [`HarnessCaptureInput`] whose source is the harness the
//! seal was told, which only code in this crate can make: provenance is
//! assigned by adapters, never decoded from a payload.
use crate::harness::candidate::{CaptureCandidate, ToolCallRecord};
use crate::harness::fields::{self, CapturedText};
use crate::harness::HarnessId;
use crate::models::{Conversation, Interaction, InteractionId, ToolExecution};
use crate::privacy::{HarnessCaptureInput, PreparedPolicy};
use crate::reasoning::{self, SealedText};
use crate::stimulus;

/// What stays the same for every candidate of one conversation's batch.
pub(crate) struct SealContext<'a> {
    pub harness: HarnessId,
    pub conversation: &'a Conversation,
    pub policy: &'a PreparedPolicy,
    /// The worktree-origin fingerprint of the worktree the capture is
    /// attributed to.
    pub capture_worktree: &'a str,
}

/// A text field the seal withheld: the capture's policy could not scrub it,
/// and the note stands in for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WithheldField {
    Response,
    Reasoning,
}

/// A sealed candidate.
pub(crate) struct Sealed {
    pub capture: HarnessCaptureInput,
    /// Tool calls left out past the 255 a capture records.
    pub omitted_tool_calls: usize,
    /// Context items left out: past the 256 a capture records, or a path
    /// that could not be scrubbed.
    pub omitted_context_items: usize,
    /// The text fields stored as the withheld note.
    pub withheld: Vec<WithheldField>,
}

/// Seals `candidate` into a capture of `context.conversation`, as a child of
/// `parent` (the engine has checked that `parent` is stored or in the same
/// batch). See the module docs.
pub(crate) fn seal(
    candidate: CaptureCandidate,
    parent: Option<InteractionId>,
    context: &SealContext<'_>,
) -> Sealed {
    let policy = context.policy;
    let id = candidate.id;
    let mut withheld = Vec::new();

    let user_prompt = stimulus::render(&id, &candidate.stimulus, &|text| policy.scrub(text));
    let model_response = candidate
        .response
        .map(|text| match fields::capture_text(text, policy) {
            CapturedText::Kept(text) => text,
            CapturedText::Withheld(note) => {
                withheld.push(WithheldField::Response);
                note
            }
        });
    let model_cot =
        candidate.reasoning.map(
            |reasoning| match fields::capture_text(reasoning.text, policy) {
                CapturedText::Kept(text) => {
                    reasoning::mark(&id, reasoning.kind, SealedText::sealed(text))
                }
                CapturedText::Withheld(note) => {
                    withheld.push(WithheldField::Reasoning);
                    note
                }
            },
        );
    let (context_items, omitted_context_items) =
        fields::captured_context_items(candidate.context, policy);
    let tools = in_nesting_order(candidate.tools)
        .into_iter()
        .map(|tool| ToolExecution {
            id: None,
            interaction_id: id.clone(),
            tool_protocol: tool.protocol.as_str().to_owned(),
            tool_name: tool.name,
            arguments: tool.arguments,
            status: tool.status,
        })
        .collect();
    let (tool_executions, omitted_tool_calls) =
        fields::captured_tool_executions(tools, omitted_context_items, &id, policy);

    let interaction = Interaction {
        id,
        conversation_id: context.conversation.id.clone(),
        parent_id: parent,
        timestamp: candidate.timestamp,
        author: candidate.author,
        user_prompt,
        model_name: candidate.model_name,
        model_cot,
        model_response,
        source_request_id: Some(candidate.key),
    };
    Sealed {
        capture: HarnessCaptureInput::new(
            context.harness,
            context.conversation.clone(),
            interaction,
            context_items,
            tool_executions,
            policy.clone(),
            context.capture_worktree.to_owned(),
        ),
        omitted_tool_calls,
        omitted_context_items,
        withheld,
    }
}

/// `tools` with every inner call right after the call it ran inside, in the
/// order the planner gave them. An inner call whose `within` does not name
/// another call of the response itself stays where it is. A response without
/// inner calls is returned as it is.
fn in_nesting_order(tools: Vec<ToolCallRecord>) -> Vec<ToolCallRecord> {
    if tools.iter().all(|tool| tool.within.is_none()) {
        return tools;
    }
    let count = tools.len();
    let mut inner: Vec<Vec<usize>> = vec![Vec::new(); count];
    let mut nested = vec![false; count];
    for (index, tool) in tools.iter().enumerate() {
        if let Some(outer) = tool.within {
            if outer != index && outer < count && tools[outer].within.is_none() {
                inner[outer].push(index);
                nested[index] = true;
            }
        }
    }
    let mut slots: Vec<Option<ToolCallRecord>> = tools.into_iter().map(Some).collect();
    let mut ordered = Vec::with_capacity(count);
    for index in 0..count {
        if nested[index] {
            continue;
        }
        ordered.extend(slots[index].take());
        for &call in &inner[index] {
            ordered.extend(slots[call].take());
        }
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::candidate::{Reasoning, ReasoningKind, SourcePosition, ToolProtocol};
    use crate::models::{Author, ToolStatus};
    use crate::privacy::PreparedPolicy;
    use crate::reasoning::{read, ReasoningView};
    use crate::stimulus::StimulusPart;
    use chrono::{TimeZone, Utc};

    const WORKTREE: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn policy_of(rules: &str) -> PreparedPolicy {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join(".thoughtignore"), rules).unwrap();
        PreparedPolicy::load(dir.path()).unwrap()
    }

    fn conversation() -> Conversation {
        Conversation {
            id: "codex:t1".into(),
            title: "t".into(),
            created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        }
    }

    fn candidate(reasoning: Option<Reasoning>, tools: Vec<ToolCallRecord>) -> CaptureCandidate {
        CaptureCandidate {
            key: "resp:r1".into(),
            id: crate::harness::derived_interaction_id(HarnessId::Codex, "codex:t1", "resp:r1"),
            parent: None,
            timestamp: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            author: Author::Human,
            stimulus: vec![StimulusPart::prompt("fix the flaky test")],
            model_name: None,
            reasoning,
            response: Some("done".into()),
            tools,
            context: Vec::new(),
            commit_candidates: Vec::new(),
            unrecorded: Vec::new(),
            start: SourcePosition::Ordinal(41),
        }
    }

    fn tool(name: &str, within: Option<usize>) -> ToolCallRecord {
        ToolCallRecord {
            protocol: ToolProtocol::Native,
            name: name.into(),
            arguments: "{}".into(),
            status: ToolStatus::Success,
            call_id: None,
            within,
        }
    }

    fn sealed(candidate: CaptureCandidate, policy: &PreparedPolicy) -> Sealed {
        let conversation = conversation();
        seal(
            candidate,
            None,
            &SealContext {
                harness: HarnessId::Codex,
                conversation: &conversation,
                policy,
                capture_worktree: WORKTREE,
            },
        )
    }

    fn noisy() -> String {
        (0..300)
            .map(|n| format!("password=hunter2hunter{n}\n"))
            .collect()
    }

    /// The policy's rules match the summary's words; they are scrubbed from
    /// the body before it is marked, and the marker verifies.
    #[test]
    fn a_provider_summary_is_marked_after_the_policy_scrub() {
        let policy = policy_of("literal:secret-word\n");
        let summary = Reasoning {
            text: "looked at secret-word in the loop".into(),
            kind: ReasoningKind::ProviderSummary,
        };
        let sealed = sealed(candidate(Some(summary), Vec::new()), &policy);
        assert!(sealed.withheld.is_empty());
        let capture = sealed.capture.into_capture();
        let cot = capture.interaction.model_cot.unwrap();
        match read(&capture.interaction.id, &cot) {
            ReasoningView::Verified { kind, body } => {
                assert_eq!(kind, ReasoningKind::ProviderSummary);
                assert!(!body.contains("secret-word"), "{body}");
                assert!(body.starts_with("looked at [CVC_REDACTED:v1:"), "{body}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn exposed_reasoning_is_the_captured_text_unmarked() {
        let policy = PreparedPolicy::built_ins_only();
        let text = "thinking about the retry loop";
        let sealed = sealed(
            candidate(Some(Reasoning::exposed(text)), Vec::new()),
            &policy,
        );
        let capture = sealed.capture.into_capture();
        assert_eq!(capture.interaction.model_cot.as_deref(), Some(text));
        assert_eq!(capture.interaction.model_response.as_deref(), Some("done"));
        assert_eq!(
            capture.interaction.source_request_id.as_deref(),
            Some("resp:r1")
        );
    }

    /// A summary the policy cannot scrub is withheld, reported as such, and
    /// stored as the note without a marker.
    #[test]
    fn a_withheld_summary_is_reported_and_never_marked() {
        let policy = PreparedPolicy::built_ins_only();
        let text = noisy();
        let mut withheld = candidate(
            Some(Reasoning {
                text: text.clone(),
                kind: ReasoningKind::ProviderSummary,
            }),
            Vec::new(),
        );
        withheld.response = Some(noisy());
        let sealed = sealed(withheld, &policy);
        assert_eq!(
            sealed.withheld,
            [WithheldField::Response, WithheldField::Reasoning]
        );
        let capture = sealed.capture.into_capture();
        assert_eq!(
            capture.interaction.model_cot.as_deref(),
            Some(fields::withheld_text(text.len()).as_str())
        );
    }

    #[test]
    fn inner_calls_follow_the_call_they_ran_inside() {
        let policy = PreparedPolicy::built_ins_only();
        let tools = vec![
            tool("exec", None),
            tool("read", None),
            tool("exec:cargo", Some(0)),
            tool("exec:git", Some(0)),
            // Names no call of the response: stays where it is.
            tool("stray", Some(9)),
            // Nested in a nested call: stays where it is.
            tool("deep", Some(2)),
        ];
        let capture = sealed(candidate(None, tools), &policy)
            .capture
            .into_capture();
        let names: Vec<&str> = capture
            .tool_executions
            .iter()
            .map(|tool| tool.tool_name.as_str())
            .collect();
        assert_eq!(
            names,
            ["exec", "exec:cargo", "exec:git", "read", "stray", "deep"]
        );
        assert!(capture
            .tool_executions
            .iter()
            .all(|tool| tool.tool_protocol == "native"));
    }

    #[test]
    fn calls_without_nesting_keep_their_order() {
        let tools: Vec<_> = ["b", "a", "c"].iter().map(|n| tool(n, None)).collect();
        let ordered: Vec<String> = in_nesting_order(tools)
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(ordered, ["b", "a", "c"]);
    }
}
