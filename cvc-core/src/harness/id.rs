//! Agent-harness identity: which agents CVC knows, how each is named in
//! storage, the conversation-id namespace each owns, and the passive consent
//! channel each is covered by.
//!
//! Every harness is provisioned here, whether or not this build has an
//! adapter for it, so that the capture-source values, the reserved
//! conversation-id prefixes and the consent channels are fixed once and the
//! adapters of later versions cannot disagree about them.
//!
//! # Conversation ids
//!
//! A new agent's conversation id is `<name>:<agent id>`, for example
//! `codex:<thread id>`. Claude Code's ids stay unprefixed (they were stored
//! before namespacing existed, and stored ids are never rewritten); its
//! prefix `claude-code:` is reserved for later use. A reserved prefix can be
//! written only by its own harness and by a pull ([`crate::privacy`] refuses
//! every other capture whose conversation id carries one), and a prefixed id
//! never contains `:agent-`, which is Claude Code's subagent convention and
//! which the TUI and the Reviewer parse.
//!
//! # Interaction ids
//!
//! A harness capture's interaction id is derived from the conversation id
//! and the harness's own stable key for the response
//! ([`derived_interaction_id`]), domain-separated by harness, so a response
//! re-read later maps to the same row.
use crate::models::InteractionId;
use crate::privacy::is_safe_identifier;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

/// An agent harness CVC knows.
///
/// The order of the variants is the order of [`HarnessId::ALL`] and of the
/// channels an acknowledgement records; it is never reordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HarnessId {
    ClaudeCode,
    Codex,
    OpenCode,
    Cursor,
    Antigravity,
}

/// The longest conversation id any capture may have (the limit of
/// [`is_safe_identifier`]).
pub const MAX_CONVERSATION_ID_BYTES: usize = 256;

/// Claude Code's subagent convention, `<session>:agent-<id>`. Readers parse
/// it, so no other harness's id may contain it.
pub const SUBAGENT_MARKER: &str = ":agent-";

/// Why [`HarnessId::conversation_id`] refused an agent id.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdError {
    #[error("the agent id is empty")]
    Empty,
    #[error("the conversation id is longer than {MAX_CONVERSATION_ID_BYTES} bytes")]
    TooLong,
    #[error("the conversation id is not a safe identifier")]
    Unsafe,
    #[error("the conversation id contains the subagent marker \":agent-\"")]
    SubagentMarker,
    #[error("the conversation id carries a prefix reserved for another harness")]
    Reserved,
}

impl HarnessId {
    pub const ALL: [HarnessId; 5] = [
        HarnessId::ClaudeCode,
        HarnessId::Codex,
        HarnessId::OpenCode,
        HarnessId::Cursor,
        HarnessId::Antigravity,
    ];

    /// The harness's name on the command line, in cursor rows, lock files
    /// and consent channels.
    pub const fn name(self) -> &'static str {
        match self {
            HarnessId::ClaudeCode => "claude-code",
            HarnessId::Codex => "codex",
            HarnessId::OpenCode => "opencode",
            HarnessId::Cursor => "cursor",
            HarnessId::Antigravity => "antigravity",
        }
    }

    /// The harness's name for people.
    pub const fn display(self) -> &'static str {
        match self {
            HarnessId::ClaudeCode => "Claude Code",
            HarnessId::Codex => "Codex",
            HarnessId::OpenCode => "OpenCode",
            HarnessId::Cursor => "Cursor",
            HarnessId::Antigravity => "Antigravity",
        }
    }

    /// The value of `interactions.capture_source` for a capture by this
    /// harness. `claude_code` is what 0.7.x stores; it never changes.
    pub const fn capture_source_str(self) -> &'static str {
        match self {
            HarnessId::ClaudeCode => "claude_code",
            HarnessId::Codex => "codex",
            HarnessId::OpenCode => "opencode",
            HarnessId::Cursor => "cursor",
            HarnessId::Antigravity => "antigravity",
        }
    }

    /// The conversation-id prefix reserved for this harness: `<name>:`.
    pub const fn reserved_prefix(self) -> &'static str {
        match self {
            HarnessId::ClaudeCode => "claude-code:",
            HarnessId::Codex => "codex:",
            HarnessId::OpenCode => "opencode:",
            HarnessId::Cursor => "cursor:",
            HarnessId::Antigravity => "antigravity:",
        }
    }

    /// The prefix this harness's conversation ids carry. `None` for Claude
    /// Code, whose ids are unprefixed; its prefix is reserved all the same.
    pub const fn conversation_prefix(self) -> Option<&'static str> {
        match self {
            HarnessId::ClaudeCode => None,
            other => Some(other.reserved_prefix()),
        }
    }

    /// The passive consent channel a capture by this harness needs covered.
    pub const fn channel(self) -> PassiveChannel {
        PassiveChannel::Harness(self)
    }

    /// What a hook of this harness must print on stdout when cvc refuses it
    /// before any adapter runs (this build has none for the harness): `{}`
    /// for the agents that treat anything else as a failed hook (Cursor,
    /// Antigravity), nothing for the others. An adapter's own
    /// [`HookCaller::contract`](crate::harness::adapter::HookCaller::contract)
    /// for an event with no name agrees with it.
    pub const fn hook_contract(self) -> crate::harness::adapter::OutputContract {
        use crate::harness::adapter::OutputContract;
        match self {
            HarnessId::Cursor | HarnessId::Antigravity => OutputContract::EmptyObject,
            HarnessId::ClaudeCode | HarnessId::Codex | HarnessId::OpenCode => {
                OutputContract::Silent
            }
        }
    }

    /// The harness named `name` ([`HarnessId::name`]), exactly.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.name() == name)
    }

    /// The harness whose [`capture_source_str`](Self::capture_source_str) is
    /// `value`, exactly.
    pub fn from_capture_source_str(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|h| h.capture_source_str() == value)
    }

    /// The prefixed harness whose namespace `id` is in, read from the exact
    /// prefix. This is a label, not provenance: an imported conversation's id
    /// says what its publisher claimed. Claude Code ids are unprefixed, so
    /// they are never recognised here.
    pub fn from_conversation_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| {
            h.conversation_prefix()
                .and_then(|prefix| id.strip_prefix(prefix))
                .is_some_and(|rest| !rest.is_empty())
        })
    }

    /// Whether a capture by this harness joins its conversation's future
    /// sharing (see `CaptureSource::inherits_future_sharing`). Every harness
    /// observes the session of the conversation it records into, so all of
    /// them do. There is no catch-all arm: a new harness does not compile
    /// until it is decided here.
    pub(crate) const fn inherits_future_sharing(self) -> bool {
        match self {
            HarnessId::ClaudeCode
            | HarnessId::Codex
            | HarnessId::OpenCode
            | HarnessId::Cursor
            | HarnessId::Antigravity => true,
        }
    }

    /// Whether a capture by this harness renames an existing conversation
    /// (see `CaptureSource::refreshes_conversation_title`). Every harness
    /// keeps the title of the session it observes current. No catch-all arm.
    pub(crate) const fn refreshes_conversation_title(self) -> bool {
        match self {
            HarnessId::ClaudeCode
            | HarnessId::Codex
            | HarnessId::OpenCode
            | HarnessId::Cursor
            | HarnessId::Antigravity => true,
        }
    }

    /// The conversation id for the agent's own id `agent_id`: `<prefix><agent
    /// id>`, or `agent_id` unchanged for Claude Code.
    ///
    /// The result is a safe identifier of at most
    /// [`MAX_CONVERSATION_ID_BYTES`] bytes; a prefixed one never contains
    /// [`SUBAGENT_MARKER`], and an unprefixed one never starts with a prefix
    /// reserved for another harness.
    pub fn conversation_id(self, agent_id: &str) -> Result<String, IdError> {
        if agent_id.is_empty() {
            return Err(IdError::Empty);
        }
        let id = match self.conversation_prefix() {
            Some(prefix) => format!("{prefix}{agent_id}"),
            None => agent_id.to_owned(),
        };
        if id.len() > MAX_CONVERSATION_ID_BYTES {
            return Err(IdError::TooLong);
        }
        if !is_safe_identifier(&id) {
            return Err(IdError::Unsafe);
        }
        if self.conversation_prefix().is_some() && id.contains(SUBAGENT_MARKER) {
            return Err(IdError::SubagentMarker);
        }
        match is_reserved_conversation_id(&id) {
            Some(owner) if owner != self => Err(IdError::Reserved),
            _ => Ok(id),
        }
    }

    /// The domain string of [`derived_interaction_id`] for this harness.
    fn interaction_domain(self) -> String {
        format!("cvc.{}.interaction/v1\0", self.name())
    }
}

impl std::fmt::Display for HarnessId {
    /// The harness's name for people ([`HarnessId::display`]).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display())
    }
}

/// The interaction id of the response `response_key` in `conversation_id`
/// captured by `harness`: the first 16 bytes of
/// `SHA-256("cvc.<name>.interaction/v1" 0x00 conversation_id 0x00 response_key)`,
/// laid out as an RFC 9562 version 8 UUID.
///
/// For Claude Code this is byte for byte what 0.7.x derives
/// (`claude_code::transcript::derived_interaction_id`), so stored ids do not
/// change.
pub fn derived_interaction_id(
    harness: HarnessId,
    conversation_id: &str,
    response_key: &str,
) -> InteractionId {
    let mut hash = Sha256::new();
    hash.update(harness.interaction_domain().as_bytes());
    hash.update(conversation_id.as_bytes());
    hash.update(b"\0");
    hash.update(response_key.as_bytes());
    let digest = hash.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    InteractionId::from_uuid(Uuid::from_bytes(bytes))
}

/// The harness whose reserved prefix `id` starts with, compared without
/// regard to ASCII case so that a look-alike such as `Codex:` is reserved as
/// well. Used to refuse captures, never to label one (see
/// [`HarnessId::from_conversation_id`]).
pub fn is_reserved_conversation_id(id: &str) -> Option<HarnessId> {
    HarnessId::ALL.into_iter().find(|h| {
        let prefix = h.reserved_prefix();
        id.len() >= prefix.len()
            && id.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    })
}

/// A passive capture channel: one that records without the model or the
/// person asking for each record, and so needs the repository's capture
/// acknowledgement to cover it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PassiveChannel {
    /// The VS Code extension's chat-session watcher and its notifications.
    Vscode,
    /// An agent harness's hooks and transcript ingestion.
    Harness(HarnessId),
}

impl PassiveChannel {
    /// Every channel, in the order an acknowledgement records them.
    pub const ALL: [PassiveChannel; 6] = [
        PassiveChannel::Vscode,
        PassiveChannel::Harness(HarnessId::ClaudeCode),
        PassiveChannel::Harness(HarnessId::Codex),
        PassiveChannel::Harness(HarnessId::OpenCode),
        PassiveChannel::Harness(HarnessId::Cursor),
        PassiveChannel::Harness(HarnessId::Antigravity),
    ];

    /// What an acknowledgement that records no channel list covers: the
    /// channels cvc 0.7.1 and 0.7.2 described when they asked for it. Every
    /// acknowledgement covers at least these.
    pub const LEGACY_COVERED: [PassiveChannel; 2] = [
        PassiveChannel::Vscode,
        PassiveChannel::Harness(HarnessId::ClaudeCode),
    ];

    /// The channel's name in `privacy.json`.
    pub const fn name(self) -> &'static str {
        match self {
            PassiveChannel::Vscode => "vscode",
            PassiveChannel::Harness(harness) => harness.name(),
        }
    }

    /// The channel's name for people.
    pub const fn display(self) -> &'static str {
        match self {
            PassiveChannel::Vscode => "VS Code",
            PassiveChannel::Harness(harness) => harness.display(),
        }
    }

    /// The channel named `name`, exactly.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    /// What capture through this channel reads and records, as the capture
    /// acknowledgement shows it before the typed phrase. `None` when this
    /// build cannot capture through the channel; such a channel cannot be
    /// acknowledged ([`crate::privacy::acknowledge_capture_channels`]
    /// refuses it), so an acknowledgement never covers an adapter that did
    /// not exist when it was given.
    ///
    /// A channel name stands for exactly the scope its notice describes. A
    /// later version that reads more through a channel (another record, other
    /// fields) gives it a new name, such as `codex/2`, with a notice of its
    /// own, so that acknowledgements of the old scope do not cover the new
    /// one. Each harness's lane adds its notice together with its adapter.
    pub const fn notice(self) -> Option<&'static str> {
        match self {
            PassiveChannel::Vscode => Some(
                "VS Code: the CVC extension reads the GitHub Copilot chat session files that VS Code keeps for this workspace, and records each chat request: your prompt, Copilot's response, the reasoning the chat exposes and the model's name.",
            ),
            PassiveChannel::Harness(HarnessId::ClaudeCode) => Some(
                "Claude Code: CVC's hooks, and `cvc ingest claude-code`, read the transcript of each Claude Code session started in this repository, with its subagents' transcripts and their .meta.json descriptions, and record each response: prompts, the tool output Claude read (file contents, command output), responses, the reasoning the transcript exposes, tool calls with their arguments, the paths they touched and the session's title. A session is recorded whole, including its work in other folders and repositories.",
            ),
            PassiveChannel::Harness(
                HarnessId::Codex | HarnessId::OpenCode | HarnessId::Cursor | HarnessId::Antigravity,
            ) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_code_ids_are_derived_exactly_as_before() {
        for (session, key) in [
            ("4f2a9c1e-0000-4000-8000-000000000001", "req_011CQ"),
            ("4f2a9c1e-0000-4000-8000-000000000001:agent-a1b2", "uuid:x"),
            ("", ""),
        ] {
            assert_eq!(
                derived_interaction_id(HarnessId::ClaudeCode, session, key),
                crate::claude_code::transcript::derived_interaction_id(session, key)
            );
        }
    }

    #[test]
    fn derived_ids_are_domain_separated_by_harness() {
        let ids: std::collections::HashSet<_> = HarnessId::ALL
            .into_iter()
            .map(|h| derived_interaction_id(h, "c", "k"))
            .collect();
        assert_eq!(ids.len(), HarnessId::ALL.len());
        // Version 8, RFC variant.
        let id = derived_interaction_id(HarnessId::Codex, "codex:t", "resp:r").to_string();
        assert_eq!(&id[14..15], "8");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn names_round_trip_and_are_pinned() {
        let pinned = [
            (
                HarnessId::ClaudeCode,
                "claude-code",
                "claude_code",
                "Claude Code",
            ),
            (HarnessId::Codex, "codex", "codex", "Codex"),
            (HarnessId::OpenCode, "opencode", "opencode", "OpenCode"),
            (HarnessId::Cursor, "cursor", "cursor", "Cursor"),
            (
                HarnessId::Antigravity,
                "antigravity",
                "antigravity",
                "Antigravity",
            ),
        ];
        assert_eq!(pinned.len(), HarnessId::ALL.len());
        for (h, name, source, display) in pinned {
            assert_eq!(h.name(), name);
            assert_eq!(h.capture_source_str(), source);
            assert_eq!(h.display(), display);
            assert_eq!(h.reserved_prefix(), format!("{name}:"));
            assert_eq!(HarnessId::from_name(name), Some(h));
            assert_eq!(HarnessId::from_capture_source_str(source), Some(h));
            assert_eq!(PassiveChannel::from_name(name), Some(h.channel()));
        }
        assert_eq!(
            PassiveChannel::from_name("vscode"),
            Some(PassiveChannel::Vscode)
        );
        assert_eq!(HarnessId::from_name("Codex"), None);
    }

    #[test]
    fn conversation_ids_are_namespaced_and_checked() {
        assert_eq!(
            HarnessId::Codex.conversation_id("019a-thread").unwrap(),
            "codex:019a-thread"
        );
        assert_eq!(
            HarnessId::OpenCode.conversation_id("ses_abc").unwrap(),
            "opencode:ses_abc"
        );
        // Claude Code ids are unchanged, its subagent convention included.
        assert_eq!(
            HarnessId::ClaudeCode
                .conversation_id("s1:agent-a1")
                .unwrap(),
            "s1:agent-a1"
        );
        assert_eq!(
            HarnessId::ClaudeCode
                .conversation_id("claude-code:x")
                .unwrap(),
            "claude-code:x"
        );
        assert_eq!(
            HarnessId::ClaudeCode.conversation_id("codex:x"),
            Err(IdError::Reserved)
        );
        assert_eq!(HarnessId::Codex.conversation_id(""), Err(IdError::Empty));
        assert_eq!(
            HarnessId::Codex.conversation_id("a:agent-b"),
            Err(IdError::SubagentMarker)
        );
        assert_eq!(
            HarnessId::Codex.conversation_id("a b"),
            Err(IdError::Unsafe)
        );
        assert_eq!(
            HarnessId::Codex.conversation_id(&"x".repeat(251)),
            Err(IdError::TooLong)
        );
        assert!(HarnessId::Codex.conversation_id(&"x".repeat(250)).is_ok());
    }

    #[test]
    fn reserved_prefixes_ignore_case_but_labels_do_not() {
        assert_eq!(
            is_reserved_conversation_id("codex:1"),
            Some(HarnessId::Codex)
        );
        assert_eq!(
            is_reserved_conversation_id("CODEX:1"),
            Some(HarnessId::Codex)
        );
        assert_eq!(
            is_reserved_conversation_id("Claude-Code:1"),
            Some(HarnessId::ClaudeCode)
        );
        assert_eq!(is_reserved_conversation_id("codex"), None);
        assert_eq!(is_reserved_conversation_id("codexx:1"), None);
        assert_eq!(is_reserved_conversation_id("session-codex:1"), None);
        assert_eq!(is_reserved_conversation_id(""), None);
        assert_eq!(is_reserved_conversation_id("é"), None);

        assert_eq!(
            HarnessId::from_conversation_id("codex:1"),
            Some(HarnessId::Codex)
        );
        assert_eq!(HarnessId::from_conversation_id("CODEX:1"), None);
        assert_eq!(HarnessId::from_conversation_id("codex:"), None);
        assert_eq!(HarnessId::from_conversation_id("claude-code:1"), None);
    }

    #[test]
    fn channels_are_ordered_and_legacy_coverage_is_pinned() {
        let names: Vec<_> = PassiveChannel::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(
            names,
            [
                "vscode",
                "claude-code",
                "codex",
                "opencode",
                "cursor",
                "antigravity"
            ]
        );
        let legacy: Vec<_> = PassiveChannel::LEGACY_COVERED
            .iter()
            .map(|c| c.name())
            .collect();
        assert_eq!(legacy, ["vscode", "claude-code"]);
        let mut sorted = PassiveChannel::ALL;
        sorted.sort();
        assert_eq!(sorted, PassiveChannel::ALL);
    }

    #[test]
    fn only_channels_this_build_captures_through_have_a_notice() {
        let noticed: Vec<_> = PassiveChannel::ALL
            .into_iter()
            .filter(|c| c.notice().is_some())
            .collect();
        assert_eq!(noticed, PassiveChannel::LEGACY_COVERED);
        for channel in noticed {
            let notice = channel.notice().unwrap();
            assert!(notice.starts_with(channel.display()), "{notice}");
        }
    }
}
