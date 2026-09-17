//! Claude Code JSONL session transcripts: line-level parsing and the pure
//! planning step that turns a window of entries into capture candidates.
//!
//! # Format pinning
//!
//! Claude Code documents its transcript entry format as internal and subject to
//! change between releases. This parser is therefore pinned to the observed
//! shape of major version 2 (the fixtures under `tests/fixtures/claude-code`
//! record the exact minor versions it was verified against) and fails loudly,
//! producing no captures, whenever:
//!
//! * an entry's `version` is missing, unparsable, or of another major version;
//! * a `user` or `assistant` entry lacks its identity fields or its `message`;
//! * a message carries a content block type this parser does not understand,
//!   because silently dropping it would misrepresent what the model saw or
//!   said.
//!
//! Entry types that carry no conversation content (session bookkeeping such as
//! `queue-operation` or `last-prompt`) are skipped by design: an unknown entry
//! type is tolerated only when it has no `message`.
//!
//! # Unit of capture
//!
//! One assistant API response (all `assistant` entries sharing a `requestId`)
//! becomes one interaction. Its stimulus (the human prompt or the tool results
//! the model was reacting to) becomes `user_prompt`, its `thinking` blocks
//! become `model_cot`, its `text` blocks become `model_response`, and its
//! `tool_use` blocks become tool executions whose status is taken from the
//! matching `tool_result`. That granularity is what lets hook-driven
//! ingestion place reasoning into the database before a mid-session commit's
//! post-commit hook runs.
use crate::models::{Author, ContextItem, InteractionId, ToolExecution, ToolStatus};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

/// Harness identifier used for cursor storage and user-facing messages.
pub const HARNESS: &str = "claude-code";
/// The transcript major version this parser is pinned to.
pub const SUPPORTED_MAJOR_VERSION: u64 = 2;

/// A single transcript line larger than this is treated as corruption rather
/// than buffered; Claude Code writes oversized tool output out of line.
const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;
/// Tool output rendered into the next node's prompt keeps its head and tail.
const TOOL_RESULT_HEAD_BYTES: usize = 6 * 1024;
const TOOL_RESULT_TAIL_BYTES: usize = 2 * 1024;
const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_TEXT_BYTES: usize = 256 * 1024;
/// Tool arguments stay valid JSON: long string values are truncated in place.
const MAX_ARGUMENT_STRING_BYTES: usize = 16 * 1024;
const MAX_ARGUMENTS_BYTES: usize = 64 * 1024;
const MAX_TOOL_ARGUMENTS_TOTAL_BYTES: usize = 2 * 1024 * 1024;
const MAX_TITLE_BYTES: usize = 120;
/// Claude Code records this title before a session has a real one.
const PLACEHOLDER_TITLE: &str = "New session";

#[derive(Debug, Error)]
pub enum TranscriptError {
    #[error("transcript entry at byte {offset} is malformed: {reason}")]
    Malformed { offset: u64, reason: String },
    #[error("transcript entry at byte {offset} has Claude Code version {version}; this cvc supports transcript major version {supported} only")]
    UnsupportedVersion {
        offset: u64,
        version: String,
        supported: u64,
    },
    #[error("transcript entry at byte {offset} belongs to session {found}, expected {expected}")]
    SessionMismatch {
        offset: u64,
        expected: String,
        found: String,
    },
}

/// One raw transcript line, deserialized permissively and validated in code so
/// every failure names the byte offset and the exact reason.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEntry {
    #[serde(rename = "type")]
    kind: Option<String>,
    uuid: Option<String>,
    parent_uuid: Option<String>,
    logical_parent_uuid: Option<String>,
    session_id: Option<String>,
    timestamp: Option<String>,
    version: Option<String>,
    request_id: Option<String>,
    subtype: Option<String>,
    custom_title: Option<String>,
    summary: Option<String>,
    cwd: Option<String>,
    is_sidechain: Option<bool>,
    is_meta: Option<bool>,
    is_compact_summary: Option<bool>,
    is_api_error_message: Option<bool>,
    message: Option<Value>,
}

/// A parsed transcript line.
#[derive(Debug, Clone)]
pub enum ParsedLine {
    /// A chain entry (message or chain-only bookkeeping) with a uuid.
    Entry(Entry),
    /// A session title update (`custom-title` or legacy `summary`).
    Title(String),
    /// Bookkeeping with no conversation content, or a sidechain entry.
    Skipped,
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Byte offset of this line's first byte within the transcript file.
    pub offset: u64,
    pub uuid: String,
    /// The chain parent; for compaction boundaries this is the logical parent
    /// so chains stay continuous across compaction.
    pub parent_uuid: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub cwd: Option<String>,
    pub payload: Payload,
}

#[derive(Debug, Clone)]
pub enum Payload {
    User(UserMessage),
    Assistant(AssistantMessage),
    /// Chain-only entries: system notices, attachments, API error notices.
    Other(OtherKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherKind {
    StopHookSummary,
    CompactBoundary,
    Attachment,
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct UserMessage {
    pub texts: Vec<String>,
    pub tool_results: Vec<ToolResultBlock>,
    pub meta: bool,
    pub compact_summary: bool,
}

#[derive(Debug, Clone)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub struct AssistantMessage {
    pub request_id: Option<String>,
    pub model: Option<String>,
    pub blocks: Vec<AssistantBlock>,
}

#[derive(Debug, Clone)]
pub enum AssistantBlock {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

/// The complete lines of one read of the transcript from a cursor onward.
#[derive(Debug, Default)]
pub struct Window {
    pub lines: Vec<ParsedLine>,
    /// Bytes (from the window start) covered by complete, parsed lines.
    pub consumed: u64,
    /// A trailing partial line was left for a later read.
    pub truncated_tail: bool,
}

/// Parses every complete line in `bytes`, which starts at file offset `base`.
/// A trailing line without a newline is accepted only if it already parses as
/// a complete entry; otherwise it is left unconsumed for the next read.
pub fn parse_window(
    bytes: &[u8],
    base: u64,
    expected_session: &str,
) -> Result<Window, TranscriptError> {
    let mut window = Window::default();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let rest = &bytes[cursor..];
        match rest.iter().position(|b| *b == b'\n') {
            Some(newline) => {
                let line = &rest[..newline];
                window
                    .lines
                    .push(parse_line(line, base + cursor as u64, expected_session)?);
                cursor += newline + 1;
                window.consumed = cursor as u64;
            }
            None => {
                match parse_line(rest, base + cursor as u64, expected_session) {
                    Ok(parsed) => {
                        window.lines.push(parsed);
                        window.consumed = bytes.len() as u64;
                    }
                    // A partial trailing write is expected while a session is
                    // live; it is not a format error until it is complete.
                    Err(TranscriptError::Malformed { .. }) => window.truncated_tail = true,
                    Err(error) => return Err(error),
                }
                cursor = bytes.len();
            }
        }
    }
    Ok(window)
}

/// Parses one transcript line located at `offset`.
pub fn parse_line(
    line: &[u8],
    offset: u64,
    expected_session: &str,
) -> Result<ParsedLine, TranscriptError> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(ParsedLine::Skipped);
    }
    if line.len() > MAX_LINE_BYTES {
        return Err(malformed(offset, "line exceeds the supported size"));
    }
    let raw: RawEntry = serde_json::from_slice(line)
        .map_err(|error| malformed(offset, format!("not a JSON transcript entry: {error}")))?;
    let Some(kind) = raw.kind.as_deref() else {
        return Err(malformed(offset, "entry has no type"));
    };
    if let Some(session) = raw.session_id.as_deref() {
        if session != expected_session {
            return Err(TranscriptError::SessionMismatch {
                offset,
                expected: expected_session.to_owned(),
                found: session.to_owned(),
            });
        }
    }
    if raw.is_sidechain.unwrap_or(false) {
        // Subagent side chains are recorded in their own transcript files;
        // main-transcript sidechain entries predate that layout and are not
        // part of this session's chain.
        return Ok(ParsedLine::Skipped);
    }
    match kind {
        "user" | "assistant" => {}
        "custom-title" => {
            return Ok(match raw.custom_title.as_deref().map(str::trim) {
                Some(title) if !title.is_empty() && title != PLACEHOLDER_TITLE => {
                    ParsedLine::Title(title.to_owned())
                }
                _ => ParsedLine::Skipped,
            });
        }
        "summary" => {
            return Ok(match raw.summary.as_deref().map(str::trim) {
                Some(title) if !title.is_empty() => ParsedLine::Title(title.to_owned()),
                _ => ParsedLine::Skipped,
            });
        }
        _ => {
            // Chain-only entries are tracked when they can be chained; any
            // other entry type is tolerated only if it carries no message.
            if raw.message.is_some() && kind != "system" && kind != "attachment" {
                return Err(malformed(
                    offset,
                    format!("unknown entry type {kind:?} carries a message"),
                ));
            }
            let Some(uuid) = raw.uuid.clone() else {
                return Ok(ParsedLine::Skipped);
            };
            let other = match (kind, raw.subtype.as_deref()) {
                ("system", Some("compact_boundary")) => OtherKind::CompactBoundary,
                ("system", Some("stop_hook_summary")) => OtherKind::StopHookSummary,
                ("attachment", _) => OtherKind::Attachment,
                _ => OtherKind::Unknown,
            };
            let parent_uuid = if other == OtherKind::CompactBoundary {
                raw.logical_parent_uuid.clone().or(raw.parent_uuid.clone())
            } else {
                raw.parent_uuid.clone()
            };
            return Ok(ParsedLine::Entry(Entry {
                offset,
                uuid,
                parent_uuid,
                timestamp: raw
                    .timestamp
                    .as_deref()
                    .map(|value| parse_timestamp(value, offset))
                    .transpose()?,
                cwd: raw.cwd.clone(),
                payload: Payload::Other(other),
            }));
        }
    }

    // Message entries are the conversation itself: every identity field is
    // required, and the version gate applies.
    let uuid = raw
        .uuid
        .clone()
        .ok_or_else(|| malformed(offset, "message entry has no uuid"))?;
    if raw.session_id.is_none() {
        return Err(malformed(offset, "message entry has no sessionId"));
    }
    let timestamp = raw
        .timestamp
        .as_deref()
        .ok_or_else(|| malformed(offset, "message entry has no timestamp"))
        .and_then(|value| parse_timestamp(value, offset))?;
    check_version(raw.version.as_deref(), offset)?;
    let message = raw
        .message
        .as_ref()
        .ok_or_else(|| malformed(offset, "message entry has no message"))?;
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(offset, "message has no role"))?;
    if role != kind {
        return Err(malformed(
            offset,
            format!("message role {role:?} does not match entry type {kind:?}"),
        ));
    }
    let content = message
        .get("content")
        .ok_or_else(|| malformed(offset, "message has no content"))?;

    let payload = if kind == "user" {
        Payload::User(parse_user_content(
            content,
            offset,
            raw.is_meta.unwrap_or(false),
            raw.is_compact_summary.unwrap_or(false),
        )?)
    } else if raw.is_api_error_message.unwrap_or(false) {
        // API error notices are harness bookkeeping shaped like a message;
        // keep them in the chain but never as a thought.
        Payload::Other(OtherKind::Unknown)
    } else {
        Payload::Assistant(AssistantMessage {
            request_id: raw.request_id.clone().filter(|value| !value.is_empty()),
            model: message
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned),
            blocks: parse_assistant_content(content, offset)?,
        })
    };
    Ok(ParsedLine::Entry(Entry {
        offset,
        uuid,
        parent_uuid: raw.parent_uuid.clone(),
        timestamp: Some(timestamp),
        cwd: raw.cwd.clone(),
        payload,
    }))
}

fn malformed(offset: u64, reason: impl Into<String>) -> TranscriptError {
    TranscriptError::Malformed {
        offset,
        reason: reason.into(),
    }
}

fn parse_timestamp(value: &str, offset: u64) -> Result<DateTime<Utc>, TranscriptError> {
    DateTime::parse_from_rfc3339(value)
        .map(|stamp| stamp.with_timezone(&Utc))
        .map_err(|error| malformed(offset, format!("invalid timestamp {value:?}: {error}")))
}

fn check_version(version: Option<&str>, offset: u64) -> Result<(), TranscriptError> {
    let version = version.ok_or_else(|| malformed(offset, "message entry has no version"))?;
    let major = version
        .split('.')
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .ok_or_else(|| malformed(offset, format!("unparsable version {version:?}")))?;
    if major != SUPPORTED_MAJOR_VERSION {
        return Err(TranscriptError::UnsupportedVersion {
            offset,
            version: version.to_owned(),
            supported: SUPPORTED_MAJOR_VERSION,
        });
    }
    Ok(())
}

fn parse_user_content(
    content: &Value,
    offset: u64,
    meta: bool,
    compact_summary: bool,
) -> Result<UserMessage, TranscriptError> {
    let mut message = UserMessage {
        meta,
        compact_summary,
        ..UserMessage::default()
    };
    match content {
        Value::String(text) => message.texts.push(text.clone()),
        Value::Array(blocks) => {
            for block in blocks {
                let block_type = block
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed(offset, "user content block has no type"))?;
                match block_type {
                    "text" => message.texts.push(
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    ),
                    "tool_result" => {
                        let tool_use_id = block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| malformed(offset, "tool_result has no tool_use_id"))?;
                        message.tool_results.push(ToolResultBlock {
                            tool_use_id: tool_use_id.to_owned(),
                            content: render_tool_result_content(block.get("content")),
                            is_error: block
                                .get("is_error")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        });
                    }
                    "image" => message.texts.push("[image]".to_owned()),
                    "document" => message.texts.push("[document]".to_owned()),
                    other => {
                        return Err(malformed(
                            offset,
                            format!("unknown user content block type {other:?}"),
                        ))
                    }
                }
            }
        }
        _ => return Err(malformed(offset, "user content is neither text nor blocks")),
    }
    Ok(message)
}

/// Tool output is opaque payload rather than conversation structure, so block
/// types unknown here degrade to a placeholder instead of failing the ingest.
fn render_tool_result_content(content: Option<&Value>) -> String {
    match content {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => {
            let mut parts = Vec::with_capacity(blocks.len());
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => parts.push(
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    ),
                    Some(other) => parts.push(format!("[{other}]")),
                    None => parts.push("[block]".to_owned()),
                }
            }
            parts.join("\n")
        }
        Some(other) => other.to_string(),
    }
}

fn parse_assistant_content(
    content: &Value,
    offset: u64,
) -> Result<Vec<AssistantBlock>, TranscriptError> {
    let mut blocks = Vec::new();
    match content {
        Value::String(text) => blocks.push(AssistantBlock::Text(text.clone())),
        Value::Array(items) => {
            for block in items {
                let block_type = block
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed(offset, "assistant content block has no type"))?;
                match block_type {
                    "text" => blocks.push(AssistantBlock::Text(
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    )),
                    "thinking" => blocks.push(AssistantBlock::Thinking(
                        block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    )),
                    // Redacted reasoning carries no readable content by design.
                    "redacted_thinking" => {}
                    "tool_use" | "server_tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| malformed(offset, "tool_use has no id"))?;
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| malformed(offset, "tool_use has no name"))?;
                        let name = if block_type == "server_tool_use" {
                            format!("server:{name}")
                        } else {
                            name.to_owned()
                        };
                        blocks.push(AssistantBlock::ToolUse {
                            id: id.to_owned(),
                            name,
                            input: block.get("input").cloned().unwrap_or(Value::Null),
                        });
                    }
                    // Server-side tool results are delivered inside the
                    // assistant message; record that they happened.
                    "web_search_tool_result"
                    | "web_fetch_tool_result"
                    | "code_execution_tool_result" => {
                        blocks.push(AssistantBlock::Text(format!("[{block_type}]")))
                    }
                    other => {
                        return Err(malformed(
                            offset,
                            format!("unknown assistant content block type {other:?}"),
                        ))
                    }
                }
            }
        }
        _ => {
            return Err(malformed(
                offset,
                "assistant content is neither text nor blocks",
            ))
        }
    }
    Ok(blocks)
}

/// Deterministic interaction identity for one response of one session, so
/// re-ingesting a transcript is idempotent by construction and never needs
/// delete-and-reinsert (which would drop existing commit links).
pub fn derived_interaction_id(session_id: &str, response_key: &str) -> InteractionId {
    let mut hash = Sha256::new();
    hash.update(b"cvc.claude-code.interaction/v1\0");
    hash.update(session_id.as_bytes());
    hash.update(b"\0");
    hash.update(response_key.as_bytes());
    let digest = hash.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // RFC 9562 custom (version 8) layout with the RFC variant bits.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    InteractionId::from_uuid(Uuid::from_bytes(bytes))
}

/// Inputs the planner needs beyond the parsed lines.
pub struct PlanInput<'a> {
    pub session_id: &'a str,
    /// Canonical root of the capturing worktree.
    pub worktree_root: &'a Path,
    /// Non-canonical spellings (as recorded in transcript `cwd` fields) paired
    /// with the canonical directory they resolve to inside the worktree.
    pub path_aliases: &'a [(PathBuf, PathBuf)],
    /// Stop/SessionEnd semantics: every response is complete, missing tool
    /// results are failures, nothing stays pending.
    pub final_mode: bool,
}

#[derive(Debug, Clone)]
pub struct PlannedInteraction {
    pub id: InteractionId,
    pub response_key: String,
    pub parent_id: Option<InteractionId>,
    pub timestamp: DateTime<Utc>,
    pub author: Author,
    pub user_prompt: String,
    pub model_name: Option<String>,
    pub model_cot: Option<String>,
    pub model_response: Option<String>,
    pub tool_executions: Vec<ToolExecution>,
    pub context_items: Vec<ContextItem>,
    /// Byte offset of the response's first entry.
    pub first_offset: u64,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub interactions: Vec<PlannedInteraction>,
    /// Responses still streaming or awaiting tool results (at most one).
    pub pending_responses: usize,
    /// Where the next read should start: the first byte of the last closed
    /// response, so its tool names and results stay in view for the next
    /// response's stimulus. Never moves backwards within one session.
    pub cursor_offset: u64,
    pub title: Option<String>,
    /// First human prompt in the window, for conversation titling.
    pub first_human_prompt: Option<String>,
}

struct Group {
    key: String,
    positions: Vec<usize>,
}

enum Stimulus<'a> {
    Human { text: &'a str, meta: bool },
    CompactSummary(&'a str),
    ToolResult(&'a ToolResultBlock),
}

/// Plans captures for one window of parsed lines starting at `window_start`.
pub fn plan(lines: &[ParsedLine], window_start: u64, input: &PlanInput<'_>) -> Plan {
    let entries: Vec<&Entry> = lines
        .iter()
        .filter_map(|line| match line {
            ParsedLine::Entry(entry) => Some(entry),
            _ => None,
        })
        .collect();
    let title = lines.iter().rev().find_map(|line| match line {
        ParsedLine::Title(title) => Some(bounded_text(title, MAX_TITLE_BYTES)),
        _ => None,
    });
    let mut by_uuid: HashMap<&str, usize> = HashMap::with_capacity(entries.len());
    for (position, entry) in entries.iter().enumerate() {
        by_uuid.entry(entry.uuid.as_str()).or_insert(position);
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut group_index: HashMap<String, usize> = HashMap::new();
    let mut group_of: Vec<Option<usize>> = vec![None; entries.len()];
    let mut results: HashMap<&str, &ToolResultBlock> = HashMap::new();
    let mut tool_names: HashMap<&str, &str> = HashMap::new();
    let mut first_human_prompt = None;
    for (position, entry) in entries.iter().enumerate() {
        match &entry.payload {
            Payload::Assistant(message) => {
                let key = message
                    .request_id
                    .clone()
                    .unwrap_or_else(|| format!("uuid:{}", entry.uuid));
                let index = *group_index.entry(key.clone()).or_insert_with(|| {
                    groups.push(Group {
                        key,
                        positions: Vec::new(),
                    });
                    groups.len() - 1
                });
                groups[index].positions.push(position);
                group_of[position] = Some(index);
                for block in &message.blocks {
                    if let AssistantBlock::ToolUse { id, name, .. } = block {
                        tool_names.entry(id.as_str()).or_insert(name.as_str());
                    }
                }
            }
            Payload::User(message) => {
                for result in &message.tool_results {
                    results.entry(result.tool_use_id.as_str()).or_insert(result);
                }
                if first_human_prompt.is_none() && !message.meta && !message.compact_summary {
                    if let Some(text) = message.texts.iter().find(|text| !text.trim().is_empty()) {
                        first_human_prompt = Some(text.clone());
                    }
                }
            }
            Payload::Other(_) => {}
        }
    }

    let mut plan = Plan {
        cursor_offset: window_start,
        title,
        first_human_prompt,
        ..Plan::default()
    };
    for group in &groups {
        let first = group.positions[0];
        let last = *group.positions.last().expect("group has entries");
        let tool_uses: Vec<(&str, &str, &Value)> = group
            .positions
            .iter()
            .flat_map(|position| match &entries[*position].payload {
                Payload::Assistant(message) => message.blocks.as_slice(),
                _ => &[],
            })
            .filter_map(|block| match block {
                AssistantBlock::ToolUse { id, name, input } => {
                    Some((id.as_str(), name.as_str(), input))
                }
                _ => None,
            })
            .collect();
        let has_successor = last + 1 < entries.len();
        let results_complete = tool_uses.iter().all(|(id, _, _)| results.contains_key(id));
        let moved_on = entries[last + 1..]
            .iter()
            .any(|entry| match &entry.payload {
                Payload::User(message) => !message.texts.is_empty() || message.compact_summary,
                Payload::Assistant(_) => true,
                Payload::Other(kind) => {
                    matches!(
                        kind,
                        OtherKind::StopHookSummary | OtherKind::CompactBoundary
                    )
                }
            });
        let closed = input.final_mode || (has_successor && (results_complete || moved_on));
        if !closed {
            plan.pending_responses += 1;
            continue;
        }

        // Stimulus: user entries between the previous response and this one,
        // found by walking the chain back until an assistant entry.
        let mut stimulus_positions = Vec::new();
        let mut parent_group = None;
        let mut cursor = entries[first].parent_uuid.as_deref();
        let mut steps = 0usize;
        while let Some(uuid) = cursor {
            steps += 1;
            if steps > entries.len() + 1 {
                break;
            }
            let Some(&position) = by_uuid.get(uuid) else {
                break;
            };
            match &entries[position].payload {
                Payload::Assistant(_) => {
                    parent_group = group_of[position];
                    break;
                }
                Payload::User(_) => stimulus_positions.push(position),
                Payload::Other(_) => {}
            }
            cursor = entries[position].parent_uuid.as_deref();
        }
        stimulus_positions.reverse();
        let mut stimuli = Vec::new();
        for position in stimulus_positions {
            if let Payload::User(message) = &entries[position].payload {
                for text in &message.texts {
                    if message.compact_summary {
                        stimuli.push(Stimulus::CompactSummary(text));
                    } else {
                        stimuli.push(Stimulus::Human {
                            text,
                            meta: message.meta,
                        });
                    }
                }
                for result in &message.tool_results {
                    stimuli.push(Stimulus::ToolResult(result));
                }
            }
        }
        let (user_prompt, author) = render_stimuli(&stimuli, &tool_names);

        let id = derived_interaction_id(input.session_id, &group.key);
        let parent_id =
            parent_group.map(|index| derived_interaction_id(input.session_id, &groups[index].key));

        let mut texts = Vec::new();
        let mut thoughts = Vec::new();
        let mut model_name = None;
        for position in &group.positions {
            if let Payload::Assistant(message) = &entries[*position].payload {
                if model_name.is_none() {
                    model_name = message.model.clone();
                }
                for block in &message.blocks {
                    match block {
                        AssistantBlock::Text(text) => texts.push(text.as_str()),
                        AssistantBlock::Thinking(text) => thoughts.push(text.as_str()),
                        AssistantBlock::ToolUse { .. } => {}
                    }
                }
            }
        }
        let model_response = join_bounded(&texts, MAX_TEXT_BYTES);
        let model_cot = join_bounded(&thoughts, MAX_TEXT_BYTES);

        let mut budget = MAX_TOOL_ARGUMENTS_TOTAL_BYTES;
        let mut tool_executions = Vec::with_capacity(tool_uses.len());
        let mut context_items: Vec<ContextItem> = Vec::new();
        for (tool_id, name, tool_input) in &tool_uses {
            let status = match results.get(tool_id) {
                Some(result) if !result.is_error => ToolStatus::Success,
                // Errors and results that never arrived (interrupted or the
                // session ended) are both failures from the model's view.
                _ => ToolStatus::Failure,
            };
            tool_executions.push(ToolExecution {
                id: None,
                interaction_id: id.clone(),
                tool_protocol: if name.starts_with("mcp__") {
                    "mcp".to_owned()
                } else {
                    "native".to_owned()
                },
                tool_name: (*name).to_owned(),
                arguments: bounded_arguments(tool_input, &mut budget),
                status,
            });
            if let Some(item) = context_item_for(name, tool_input, input, &id) {
                let duplicate = context_items.iter().any(|existing| {
                    existing.file_path == item.file_path
                        && existing.start_line == item.start_line
                        && existing.end_line == item.end_line
                });
                if !duplicate {
                    context_items.push(item);
                }
            }
        }

        let timestamp = entries[first].timestamp.unwrap_or_else(Utc::now);
        plan.cursor_offset = entries[first].offset;
        plan.interactions.push(PlannedInteraction {
            id,
            response_key: group.key.clone(),
            parent_id,
            timestamp,
            author,
            user_prompt,
            model_name,
            model_cot,
            model_response,
            tool_executions,
            context_items,
            first_offset: entries[first].offset,
        });
    }
    plan
}

fn render_stimuli(stimuli: &[Stimulus<'_>], tool_names: &HashMap<&str, &str>) -> (String, Author) {
    let mut author = Author::System;
    let mut parts = Vec::with_capacity(stimuli.len());
    for stimulus in stimuli {
        match stimulus {
            Stimulus::Human { text, meta } => {
                if !*meta {
                    author = Author::Human;
                    parts.push(bounded_text(text, MAX_PROMPT_BYTES));
                } else {
                    parts.push(format!("[meta] {}", bounded_text(text, MAX_PROMPT_BYTES)));
                }
            }
            Stimulus::CompactSummary(text) => parts.push(format!(
                "[compaction summary]\n{}",
                bounded_text(text, MAX_PROMPT_BYTES)
            )),
            Stimulus::ToolResult(result) => {
                let name = tool_names
                    .get(result.tool_use_id.as_str())
                    .copied()
                    .unwrap_or("unknown-tool");
                let outcome = if result.is_error { " error" } else { "" };
                parts.push(format!(
                    "[tool_result {name}{outcome}] {}",
                    bounded_head_tail(
                        &result.content,
                        TOOL_RESULT_HEAD_BYTES,
                        TOOL_RESULT_TAIL_BYTES
                    )
                ));
            }
        }
    }
    let prompt = if parts.is_empty() {
        "[no stimulus recorded]".to_owned()
    } else {
        bounded_text(&parts.join("\n\n"), MAX_PROMPT_BYTES)
    };
    (prompt, author)
}

fn join_bounded(parts: &[&str], max: usize) -> Option<String> {
    let joined = parts
        .iter()
        .filter(|part| !part.trim().is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n\n");
    if joined.is_empty() {
        None
    } else {
        Some(bounded_text(&joined, max))
    }
}

/// Serializes tool input as JSON that stays valid after size bounding.
fn bounded_arguments(input: &Value, budget: &mut usize) -> String {
    let mut value = input.clone();
    bound_strings(&mut value, MAX_ARGUMENT_STRING_BYTES);
    let serialized = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
    if serialized.len() > MAX_ARGUMENTS_BYTES || serialized.len() > *budget {
        return serde_json::json!({
            "cvc_truncated": true,
            "bytes": input.to_string().len(),
        })
        .to_string();
    }
    *budget -= serialized.len();
    serialized
}

fn bound_strings(value: &mut Value, max: usize) {
    match value {
        Value::String(text) => {
            if text.len() > max {
                *text = bounded_text(text, max);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| bound_strings(item, max)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|item| bound_strings(item, max)),
        _ => {}
    }
}

/// Paths the model read or edited inside the capturing worktree become context
/// items; everything else (other checkouts, system files, relative paths whose
/// base is unknown) is deliberately not attributed.
fn context_item_for(
    tool_name: &str,
    input: &Value,
    plan_input: &PlanInput<'_>,
    interaction_id: &InteractionId,
) -> Option<ContextItem> {
    let path_key = match tool_name {
        "Read" | "Edit" | "Write" | "MultiEdit" => "file_path",
        "NotebookEdit" => "notebook_path",
        _ => return None,
    };
    let raw = input.get(path_key)?.as_str()?;
    let file_path = worktree_relative(raw, plan_input.worktree_root, plan_input.path_aliases)?;
    let (start_line, end_line) = if tool_name == "Read" {
        let offset = input
            .get("offset")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 1);
        let limit = input
            .get("limit")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 1);
        match (offset, limit) {
            (Some(offset), Some(limit)) => (
                Some(offset.min(i32::MAX as i64) as i32),
                Some((offset + limit - 1).min(i32::MAX as i64) as i32),
            ),
            (Some(offset), None) => (Some(offset.min(i32::MAX as i64) as i32), None),
            _ => (None, None),
        }
    } else {
        (None, None)
    };
    Some(ContextItem {
        id: None,
        interaction_id: interaction_id.clone(),
        file_path,
        git_blob_sha: None,
        dirty_patch: None,
        start_line,
        end_line,
    })
}

fn worktree_relative(
    raw: &str,
    worktree_root: &Path,
    path_aliases: &[(PathBuf, PathBuf)],
) -> Option<String> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return None;
    }
    let resolved: PathBuf = match path.strip_prefix(worktree_root) {
        Ok(_) => path.to_path_buf(),
        Err(_) => path_aliases.iter().find_map(|(spelled, canonical)| {
            path.strip_prefix(spelled)
                .ok()
                .map(|rest| canonical.join(rest))
        })?,
    };
    let relative = resolved.strip_prefix(worktree_root).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    let mut rendered = String::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return None;
        };
        if !rendered.is_empty() {
            rendered.push('/');
        }
        rendered.push_str(part.to_str()?);
    }
    Some(rendered)
}

fn truncate_to_boundary(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn bounded_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let kept = truncate_to_boundary(text, max);
    format!(
        "{kept}\n…[cvc: truncated {} bytes]",
        text.len() - kept.len()
    )
}

fn bounded_head_tail(text: &str, head: usize, tail: usize) -> String {
    if text.len() <= head + tail {
        return text.to_owned();
    }
    let kept_head = truncate_to_boundary(text, head);
    let mut start = text.len() - tail;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    let omitted = start - kept_head.len();
    format!(
        "{kept_head}\n…[cvc: truncated {omitted} bytes]…\n{}",
        &text[start..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_helpers_respect_char_boundaries() {
        let text = "héllo wörld";
        assert_eq!(truncate_to_boundary(text, 2), "h");
        assert!(bounded_text(text, 3).starts_with("hé"));
        let long = "a".repeat(100) + "é" + &"b".repeat(100);
        let rendered = bounded_head_tail(&long, 10, 10);
        assert!(rendered.starts_with(&"a".repeat(10)));
        assert!(rendered.ends_with(&"b".repeat(10)));
        assert!(rendered.contains("truncated"));
    }

    #[test]
    fn derived_ids_are_stable_and_session_scoped() {
        let a = derived_interaction_id("session", "req_1");
        let b = derived_interaction_id("session", "req_1");
        let c = derived_interaction_id("other", "req_1");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.as_str().len(), 36);
    }

    #[test]
    fn worktree_relative_rejects_outside_and_relative_paths() {
        let root = Path::new("/work/repo");
        let aliases = vec![(PathBuf::from("/home/me/repo"), PathBuf::from("/work/repo"))];
        assert_eq!(
            worktree_relative("/work/repo/src/lib.rs", root, &aliases).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            worktree_relative("/home/me/repo/src/lib.rs", root, &aliases).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(worktree_relative("/etc/hosts", root, &aliases), None);
        assert_eq!(worktree_relative("src/lib.rs", root, &aliases), None);
        assert_eq!(worktree_relative("/work/repo", root, &aliases), None);
        assert_eq!(worktree_relative("/work/repo/../x", root, &aliases), None);
    }
}
