//! What a harness planner produces: one [`CaptureCandidate`] per closed model
//! response, raw, and the [`Plan`] that carries them with the cursor they
//! commit with.
//!
//! Nothing here is scrubbed or bounded. A candidate holds the agent's own text
//! exactly as its record does; [`crate::harness::seal`] is the one place that
//! scrubs every field with the capture's policy, then bounds it, then marks
//! it, for every harness alike. A planner therefore never needs to know the
//! capture limits, and cannot get them wrong.
//!
//! # Response keys
//!
//! [`CaptureCandidate::key`] is the harness's stable name for the response
//! inside its conversation, and the interaction id is derived from it
//! ([`crate::harness::derived_interaction_id`]), so a response read again
//! later maps to the row already stored. A key is short ASCII and never
//! changes once the first adapter ships:
//!
//! | Harness     | Key                                                                  |
//! |-------------|----------------------------------------------------------------------|
//! | Claude Code | `requestId`, else `uuid:<uuid>`                                      |
//! | Codex       | `resp:<response_id>`, else `ord:<first ordinal>`, else `off:<byte>`  |
//! | OpenCode    | `msg:<message id>`                                                   |
//! | Cursor      | `gen:<generation_id>`                                                |
//! | Antigravity | `step:<step_index>:<first 8 hex of the step's content hash>`         |
//!
//! # Fitting the agents
//!
//! The shapes here were checked against each record the v0.8.0 lanes read:
//!
//! * Codex groups a response by `response_id` and reads one source per regime
//!   (legacy `response_item`, paginated `item_completed`); the planner does
//!   both, so one candidate has one `response` and one `reasoning`. A
//!   `code_mode` call's inner commands are [`ToolCallRecord`]s whose
//!   [`within`](ToolCallRecord::within) names the enclosing call. A compaction
//!   is a [`StimulusPart::compaction_summary`]. Forks and reverts give the
//!   planner [`PlanInput::own_history_start`], so inherited records are
//!   skipped, and a fork is a [`ChildRef`] of kind [`RelationKind::Fork`].
//! * Cursor's window is spool rows of one generation rather than file lines
//!   ([`SourcePosition::Spool`]); Antigravity's is step lines
//!   ([`SourcePosition::Step`]). Neither needs anything the planner cannot
//!   compute from its window alone.
use crate::db::{RelationEvidence, RelationKind};
use crate::harness::drift::Drift;
use crate::harness::HarnessId;
use crate::models::{Author, ContextItem, InteractionId, ToolStatus};
use chrono::{DateTime, Utc};
use std::path::Path;

pub use crate::reasoning::ReasoningKind;
pub use crate::stimulus::{PartKind, StimulusPart};

/// One closed model response, raw: nothing scrubbed, nothing bounded.
#[derive(Debug, Clone)]
pub struct CaptureCandidate {
    /// The harness's stable key for the response (see the module docs). It
    /// is stored as the interaction's `source_request_id`, so it must be a
    /// safe identifier ([`crate::privacy::is_safe_identifier`]).
    pub key: String,
    /// `derived_interaction_id(harness, conversation_id, key)`.
    pub id: InteractionId,
    /// The previous response in this conversation's chain, if the record
    /// names one. The engine drops it when that response is stored nowhere.
    pub parent: Option<InteractionId>,
    pub timestamp: DateTime<Utc>,
    /// [`Author::Human`] when any stimulus part is a person's prompt or a
    /// delegating agent's task, else [`Author::System`].
    pub author: Author,
    /// What the model was reacting to, in typed parts; rendered by the seal
    /// through [`crate::stimulus`].
    pub stimulus: Vec<StimulusPart>,
    pub model_name: Option<String>,
    pub reasoning: Option<Reasoning>,
    pub response: Option<String>,
    pub tools: Vec<ToolCallRecord>,
    /// Worktree-relative paths the response read or edited, inside the
    /// worktree only.
    pub context: Vec<ContextItem>,
    /// Commit ids, short or full hex, that the session's own Git output named.
    /// Raw tokens: each is resolved and verified against the repository
    /// before anything is linked to it, so a spurious one is harmless.
    pub commit_candidates: Vec<String>,
    /// The unfamiliar block, part or step types this response holds a
    /// placeholder for, in the order met ([`crate::harness::drift`]).
    pub unrecorded: Vec<String>,
    /// Where the response begins in the agent's record.
    pub start: SourcePosition,
}

/// The model's reasoning and what kind of text it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reasoning {
    pub text: String,
    pub kind: ReasoningKind,
}

impl Reasoning {
    /// Reasoning text as the harness exposed it.
    pub fn exposed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: ReasoningKind::Exposed,
        }
    }
}

/// One tool call a response made.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRecord {
    pub protocol: ToolProtocol,
    pub name: String,
    /// Compact JSON, raw.
    pub arguments: String,
    pub status: ToolStatus,
    /// The agent's id for the call. For pairing results with calls inside the
    /// planner only; it is never stored.
    pub call_id: Option<String>,
    /// The index, in the same response's `tools`, of the call this one ran
    /// inside (Codex `code_mode` inner commands). The seal stores each inner
    /// call right after its enclosing one: the wire has no parent field, so
    /// order carries the nesting. `None` for a call of the response itself.
    pub within: Option<usize>,
}

/// What ran a tool. `cvc` is not one of them: it is reserved for the seal's
/// own `cvc-omitted` record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolProtocol {
    /// The harness itself.
    Native,
    /// An MCP server.
    Mcp,
    /// The model provider; the status is the one its result block reported.
    Server,
}

impl ToolProtocol {
    /// The value stored in `tool_executions.tool_protocol`.
    pub const fn as_str(self) -> &'static str {
        match self {
            ToolProtocol::Native => "native",
            ToolProtocol::Mcp => "mcp",
            ToolProtocol::Server => "server",
        }
    }

    /// The protocol whose stored value is `value`, exactly.
    pub fn from_stored(value: &str) -> Option<Self> {
        [
            ToolProtocol::Native,
            ToolProtocol::Mcp,
            ToolProtocol::Server,
        ]
        .into_iter()
        .find(|protocol| protocol.as_str() == value)
    }
}

/// Where a response begins in the agent's own record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourcePosition {
    /// A byte offset into a JSONL file (decoded, for a compressed one).
    Byte(u64),
    /// A record ordinal (Codex's paginated rollouts).
    Ordinal(u64),
    /// A SQLite row, by its `(time_updated, id)` watermark (OpenCode 1.x).
    Row { time_ms: i64, id: String },
    /// A sequence number (OpenCode 2.x).
    Seq(u64),
    /// A spool row (Cursor).
    Spool(u64),
    /// A step index (Antigravity).
    Step(u64),
}

/// A planner's output for one window of one conversation's record.
#[derive(Debug, Clone)]
pub struct Plan<C> {
    /// The closed responses of the window, in record order.
    pub candidates: Vec<CaptureCandidate>,
    /// Responses still open: streaming, or waiting for tool results.
    pub pending: usize,
    /// The cursor that commits with this plan's batch: where the next read
    /// starts. It never passes a response that is still open.
    pub resume: C,
    /// The title the record gave the session, raw.
    pub title: Option<TitleHint>,
    /// The first prompt a person wrote in the window, raw, for a title when
    /// the record gives none.
    pub first_prompt: Option<String>,
    /// Subagents and forks the window names, from exact evidence only.
    pub children: Vec<ChildRef>,
    /// What the window held that this version does not record in full.
    pub drift: Drift,
}

/// A title the agent's record gives its session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleHint {
    pub text: String,
    pub rank: TitleRank,
}

/// Who chose a title. A title the person set outranks one the agent
/// generated.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TitleRank {
    Generated,
    UserSet,
}

/// A child conversation the window names: a subagent or a fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRef {
    /// The agent's own id for the child (not yet namespaced).
    pub agent_id: String,
    pub kind: RelationKind,
    /// The key of the response that spawned it, where the record says.
    pub spawn_key: Option<String>,
    /// The agent's id for the spawning call: a safe identifier.
    pub spawn_call_id: Option<String>,
    pub evidence: RelationEvidence,
}

/// Whether a read sees the whole of what the agent will write for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Finality {
    /// Mid-session: only closed responses are recorded; one still streaming
    /// or waiting for its tool results stays pending.
    Incremental,
    /// The session stopped or ended, or an operator asked: every response is
    /// complete, and missing tool results are failures.
    Final,
}

/// What a conversation is to its family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversationRole {
    /// The session a person started.
    Root,
    /// A subagent: no person types into it, so its user text is a `task`,
    /// never a `prompt`.
    Subagent,
}

/// What a planner needs beyond its window. Pure data: a planner reads no
/// file, no database and no clock (except a fallback timestamp).
#[derive(Debug, Clone, Copy)]
pub struct PlanInput<'a> {
    pub harness: HarnessId,
    pub conversation_id: &'a str,
    /// The canonical root of the worktree the capture is attributed to.
    pub worktree_root: &'a Path,
    pub finality: Finality,
    pub role: ConversationRole,
    /// Forks and reverts: records before this position are inherited from
    /// another conversation and are skipped.
    pub own_history_start: Option<&'a SourcePosition>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_protocols_are_stored_as_before() {
        for (protocol, value) in [
            (ToolProtocol::Native, "native"),
            (ToolProtocol::Mcp, "mcp"),
            (ToolProtocol::Server, "server"),
        ] {
            assert_eq!(protocol.as_str(), value);
            assert_eq!(ToolProtocol::from_stored(value), Some(protocol));
        }
        assert_eq!(ToolProtocol::from_stored("cvc"), None);
    }

    #[test]
    fn a_user_set_title_outranks_a_generated_one() {
        assert!(TitleRank::UserSet > TitleRank::Generated);
    }
}
