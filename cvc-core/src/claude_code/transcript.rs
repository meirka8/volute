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
//! * a message's content is neither text nor a list of typed blocks, or a
//!   block lacks the fields its type requires.
//!
//! A block whose *type* alone is unfamiliar does not fail the transcript.
//! Claude Code adds block types within a major version (`fallback`, a
//! model-switch marker, appeared in 2.1.x), and refusing a whole session over
//! one of them loses everything the session recorded, which misrepresents far
//! more than an omission would. Such a block is kept as a bracketed
//! placeholder naming its type, never its content, and counted so the ingest
//! report can say what was not recorded. `fallback` carries no conversation
//! content and is skipped outright.
//!
//! Entry types that carry no conversation content (session bookkeeping such as
//! `queue-operation` or `last-prompt`) are skipped by design. The conversation
//! is the `uuid`/`parentUuid` chain, so an entry without a `uuid` is
//! bookkeeping whatever else it carries (2.1.300 caches request messages as
//! chain-less `api-request-blob` entries with a `message`); a chained entry of
//! an unknown type is tolerated only when it has no `message`.
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
//!
//! # Provider-side (server) tools
//!
//! A tool the model provider runs (web search, web fetch, code execution, ...)
//! is a `server_tool_use` block, recorded as a `server:<name>` execution with
//! `tool_protocol` `server`. No user-side `tool_result` ever follows such a
//! call: the API delivers its result inside the same assistant message, as a
//! block whose type ends in `_tool_result` and whose `tool_use_id` names the
//! call. The status is read from that block (`server_result_status`); a call
//! with no result block in its response is a failure, like any other
//! interrupted call. Only a `[<block type>]` placeholder reaches the response
//! text, never the result's content, which is third-party text.
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
/// A tool the model provider runs is recorded as `server:<name>`.
const SERVER_TOOL_PREFIX: &str = "server:";
/// `tool_protocol` of every provider-side tool execution. Unlike `native` with
/// a `server:` name (0.7.0, which never read a result), it means the status was
/// taken from the result block the API delivered inside the response.
const TOOL_PROTOCOL_SERVER: &str = "server";

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
    ai_title: Option<String>,
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
    /// A session title update: one the user set (`custom-title`) or one
    /// Claude Code generated (`ai-title`, or the legacy `summary`).
    Title { text: String, source: TitleSource },
    /// Bookkeeping with no conversation content, or a sidechain entry.
    Skipped,
}

/// Where a session title came from. A title the user set outranks a
/// generated one wherever the two appear in a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSource {
    Custom,
    Generated,
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
    /// Block types kept in `texts` as placeholders, for reporting.
    pub unrecorded: Vec<String>,
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
        /// `server:<name>` for a provider-side call.
        name: String,
        input: Value,
        /// A provider-side call (`server_tool_use`): its result is a block of
        /// the same assistant message, never a user-side `tool_result`.
        server: bool,
    },
    /// The result of a provider-side call, delivered inside the assistant
    /// message as a block whose type ends in `_tool_result`. Only the outcome
    /// and a placeholder naming the block type are kept, never the content,
    /// which is third-party text.
    ServerToolResult {
        tool_use_id: String,
        /// `[<block type>]`, the text this block adds to the response.
        placeholder: String,
        status: ToolStatus,
    },
    /// A block of a type this parser does not record: the type name only.
    Unrecorded(String),
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
    include_sidechain: bool,
) -> Result<Window, TranscriptError> {
    let mut window = Window::default();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let rest = &bytes[cursor..];
        match rest.iter().position(|b| *b == b'\n') {
            Some(newline) => {
                let line = &rest[..newline];
                window.lines.push(parse_line(
                    line,
                    base + cursor as u64,
                    expected_session,
                    include_sidechain,
                )?);
                cursor += newline + 1;
                window.consumed = cursor as u64;
            }
            None => {
                match parse_line(
                    rest,
                    base + cursor as u64,
                    expected_session,
                    include_sidechain,
                ) {
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
///
/// `include_sidechain` selects the file being read: the main session transcript
/// (`false`) skips sidechain entries, which belong to subagents recorded in
/// their own files; a subagent transcript (`true`) is made entirely of sidechain
/// entries and keeps them.
pub fn parse_line(
    line: &[u8],
    offset: u64,
    expected_session: &str,
    include_sidechain: bool,
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
    if raw.is_sidechain.unwrap_or(false) && !include_sidechain {
        // Subagent side chains are recorded in their own transcript files;
        // main-transcript sidechain entries predate that layout and are not
        // part of this session's chain.
        return Ok(ParsedLine::Skipped);
    }
    match kind {
        "user" | "assistant" => {}
        "custom-title" => {
            return Ok(title_line(raw.custom_title.as_deref(), TitleSource::Custom));
        }
        // Claude Code 2.1.289 records its generated title under its own entry
        // type; earlier 2.1.x wrote it as `custom-title`.
        "ai-title" => return Ok(title_line(raw.ai_title.as_deref(), TitleSource::Generated)),
        "summary" => return Ok(title_line(raw.summary.as_deref(), TitleSource::Generated)),
        _ => {
            // The conversation is the uuid/parentUuid chain. An entry without
            // a uuid can neither be referenced by nor reference anything in
            // it, so it is bookkeeping whatever else it carries: Claude Code
            // 2.1.300 caches request messages as chain-less `api-request-blob`
            // entries that have a `message`. A chained entry of an unknown
            // type is tolerated only if it carries no message, because
            // silently dropping one could misrepresent the session.
            let Some(uuid) = raw.uuid.clone() else {
                return Ok(ParsedLine::Skipped);
            };
            if raw.message.is_some() && kind != "system" && kind != "attachment" {
                return Err(malformed(
                    offset,
                    format!("unknown entry type {kind:?} carries a message"),
                ));
            }
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

/// A title entry's payload as a parsed line; empty or placeholder titles are
/// bookkeeping and skipped.
fn title_line(raw: Option<&str>, source: TitleSource) -> ParsedLine {
    match raw.map(str::trim) {
        Some(title) if !title.is_empty() && title != PLACEHOLDER_TITLE => ParsedLine::Title {
            text: title.to_owned(),
            source,
        },
        _ => ParsedLine::Skipped,
    }
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
                        message.texts.push(format!("[{other}]"));
                        message.unrecorded.push(other.to_owned());
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
                    // A mid-session model switch, recorded by Claude Code 2.1.x
                    // as an entry of the same request whose only block names
                    // the models involved: bookkeeping, not conversation.
                    "fallback" => {}
                    "tool_use" | "server_tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| malformed(offset, "tool_use has no id"))?;
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| malformed(offset, "tool_use has no name"))?;
                        let server = block_type == "server_tool_use";
                        let name = if server {
                            format!("{SERVER_TOOL_PREFIX}{name}")
                        } else {
                            name.to_owned()
                        };
                        blocks.push(AssistantBlock::ToolUse {
                            id: id.to_owned(),
                            name,
                            input: block.get("input").cloned().unwrap_or(Value::Null),
                            server,
                        });
                    }
                    // The result of a provider-side call is delivered inside the
                    // assistant message, as a block of some `<kind>_tool_result`
                    // type (web search, web fetch, every code execution flavour,
                    // tool search, ...) naming its call. Matching by shape keeps
                    // types added later from falling out of the outcome rule.
                    result_type if result_type.ends_with("_tool_result") => {
                        match block.get("tool_use_id").and_then(Value::as_str) {
                            Some(tool_use_id) => blocks.push(AssistantBlock::ServerToolResult {
                                tool_use_id: tool_use_id.to_owned(),
                                placeholder: format!("[{result_type}]"),
                                status: server_result_status(result_type, block),
                            }),
                            // Without a call to match, the outcome is unusable. The
                            // types recognized before outcomes were read keep the
                            // placeholder they always had; any other stays an
                            // unrecorded block.
                            None if matches!(
                                result_type,
                                "web_search_tool_result"
                                    | "web_fetch_tool_result"
                                    | "code_execution_tool_result"
                            ) =>
                            {
                                blocks.push(AssistantBlock::Text(format!("[{result_type}]")))
                            }
                            None => blocks.push(AssistantBlock::Unrecorded(result_type.to_owned())),
                        }
                    }
                    // Kept as a placeholder naming the type, never its content;
                    // the planner reports it. See the module docs.
                    other => blocks.push(AssistantBlock::Unrecorded(other.to_owned())),
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

/// The outcome a provider-side tool result reports.
///
/// Errors do not raise in the API: the block's `content` is then a single error
/// object carrying an `error_code` (its `type` is `<kind>_error`), where a
/// success holds the result itself (a list of hits for web search). A code
/// execution that ran but exited non-zero is a failure too, as Claude Code marks
/// a local Bash call that exits non-zero, and so is a block that flags itself
/// `is_error`. Only this structure is read, never the result's text.
fn server_result_status(block_type: &str, block: &Value) -> ToolStatus {
    if block.get("is_error").and_then(Value::as_bool) == Some(true) {
        return ToolStatus::Failure;
    }
    let Some(content) = block.get("content").and_then(Value::as_object) else {
        return ToolStatus::Success;
    };
    let reports_error = content.contains_key("error_code")
        || content
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.ends_with("_error"));
    let exited_non_zero = matches!(
        block_type,
        "bash_code_execution_tool_result" | "code_execution_tool_result"
    ) && content.get("return_code").is_some_and(is_non_zero_integer);
    if reports_error || exited_non_zero {
        ToolStatus::Failure
    } else {
        ToolStatus::Success
    }
}

/// Only an integer other than zero counts; anything else (a string, a float, a
/// missing code) says nothing about the exit.
fn is_non_zero_integer(value: &Value) -> bool {
    value
        .as_i64()
        .map(|code| code != 0)
        .or_else(|| value.as_u64().map(|code| code != 0))
        .unwrap_or(false)
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
    /// Commit references named by a successful `git commit`/`git merge` tool
    /// result in this response. Raw candidate strings (short or full hex); the
    /// ingest step resolves and verifies each against the repository before
    /// linking, so a spurious token is harmless.
    pub commit_candidates: Vec<String>,
    /// Content block types this parser does not record, in the order met
    /// across the stimulus and the response; each is present in the captured
    /// text as a bracketed placeholder naming the type.
    pub unrecorded_blocks: Vec<String>,
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

/// One tool call a response made.
struct ToolCall<'a> {
    id: &'a str,
    name: &'a str,
    input: &'a Value,
    /// Provider-side: answered by a result block inside the response itself.
    server: bool,
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
    // The latest title of the highest-ranking source in the window: a title
    // the user set wins over one Claude Code generated.
    let latest_title = |wanted: TitleSource| {
        lines.iter().rev().find_map(|line| match line {
            ParsedLine::Title { text, source } if *source == wanted => {
                Some(bounded_text(text, MAX_TITLE_BYTES))
            }
            _ => None,
        })
    };
    let title = latest_title(TitleSource::Custom).or_else(|| latest_title(TitleSource::Generated));
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
        let group_blocks = || {
            group
                .positions
                .iter()
                .flat_map(|position| match &entries[*position].payload {
                    Payload::Assistant(message) => message.blocks.as_slice(),
                    _ => &[],
                })
        };
        let tool_uses: Vec<ToolCall<'_>> = group_blocks()
            .filter_map(|block| match block {
                AssistantBlock::ToolUse {
                    id,
                    name,
                    input,
                    server,
                } => Some(ToolCall {
                    id: id.as_str(),
                    name: name.as_str(),
                    input,
                    server: *server,
                }),
                _ => None,
            })
            .collect();
        // A provider-side call is answered by a block of its own response, so
        // its outcome is looked up in this group only; the first block naming
        // a call wins, as with user-side results.
        let mut server_outcomes: HashMap<&str, &ToolStatus> = HashMap::new();
        for block in group_blocks() {
            if let AssistantBlock::ServerToolResult {
                tool_use_id,
                status,
                ..
            } = block
            {
                server_outcomes
                    .entry(tool_use_id.as_str())
                    .or_insert(status);
            }
        }
        let has_successor = last + 1 < entries.len();
        // A provider-side call counts as answered once its result block is
        // there; without one the response stays open until the conversation
        // moves on, instead of recording a failure for a result still to come.
        let results_complete = tool_uses.iter().all(|call| {
            if call.server {
                server_outcomes.contains_key(call.id)
            } else {
                results.contains_key(call.id)
            }
        });
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
        // Block types this parser does not record are visible in the text as
        // placeholders and reported by name, so an omission is never silent.
        let mut unrecorded_blocks: Vec<String> = Vec::new();
        for position in stimulus_positions {
            if let Payload::User(message) = &entries[position].payload {
                unrecorded_blocks.extend(message.unrecorded.iter().cloned());
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

        let placeholders: Vec<String> = group_blocks()
            .filter_map(|block| match block {
                AssistantBlock::Unrecorded(kind) => Some(format!("[{kind}]")),
                _ => None,
            })
            .collect();
        let mut next_placeholder = placeholders.iter();
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
                        AssistantBlock::ServerToolResult { placeholder, .. } => {
                            texts.push(placeholder.as_str())
                        }
                        AssistantBlock::Unrecorded(kind) => {
                            unrecorded_blocks.push(kind.clone());
                            if let Some(placeholder) = next_placeholder.next() {
                                texts.push(placeholder.as_str());
                            }
                        }
                    }
                }
            }
        }
        let model_response = join_bounded(&texts, MAX_TEXT_BYTES);
        let model_cot = join_bounded(&thoughts, MAX_TEXT_BYTES);

        let mut budget = MAX_TOOL_ARGUMENTS_TOTAL_BYTES;
        let mut tool_executions = Vec::with_capacity(tool_uses.len());
        let mut context_items: Vec<ContextItem> = Vec::new();
        let mut commit_candidates: Vec<String> = Vec::new();
        for call in &tool_uses {
            let status = if call.server {
                // The result block of this response says how it went; none
                // means the call never finished (interrupted, or the session
                // ended), as for a user-side call without a result.
                server_outcomes
                    .get(call.id)
                    .map_or(ToolStatus::Failure, |outcome| (*outcome).clone())
            } else {
                match results.get(call.id) {
                    Some(result) if !result.is_error => ToolStatus::Success,
                    // Errors and results that never arrived (interrupted or the
                    // session ended) are both failures from the model's view.
                    _ => ToolStatus::Failure,
                }
            };
            if status == ToolStatus::Success && !call.server {
                if let Some(result) = results.get(call.id) {
                    for candidate in
                        commit_candidates_from_tool(call.name, call.input, &result.content)
                    {
                        if !commit_candidates.contains(&candidate) {
                            commit_candidates.push(candidate);
                        }
                    }
                }
            }
            tool_executions.push(ToolExecution {
                id: None,
                interaction_id: id.clone(),
                tool_protocol: if call.server {
                    TOOL_PROTOCOL_SERVER.to_owned()
                } else if call.name.starts_with("mcp__") {
                    "mcp".to_owned()
                } else {
                    "native".to_owned()
                },
                tool_name: call.name.to_owned(),
                arguments: bounded_arguments(call.input, &mut budget),
                status,
            });
            if let Some(item) = context_item_for(call.name, call.input, input, &id) {
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
            commit_candidates,
            unrecorded_blocks,
            first_offset: entries[first].offset,
        });
    }
    plan
}

/// Commit hashes named by a committing Bash command's recorded output, in
/// order of appearance. Four exact shapes are recognised:
///
/// * git's own summary line, `[<branch> <short-sha>] <subject>`;
/// * the line CVC's post-commit hook prints into that same output,
///   `CVC: Linked <n> thought(s) to commit <sha>.`, which is how a quiet
///   `git commit -q` still names its commit;
/// * a hash at the start of a line, as `git log --oneline`,
///   `git show --oneline` and `git rev-parse --short HEAD` print;
/// * a bare full-length hash anywhere, as `git rev-parse HEAD` prints.
///
/// Every candidate is verified against the repository before a link is made
/// (it must resolve to a commit whose time is consistent with the response),
/// so a token that merely looks like a hash is harmless.
fn commit_candidates_from_tool(tool_name: &str, input: &Value, result: &str) -> Vec<String> {
    if tool_name != "Bash" {
        return Vec::new();
    }
    let Some(command) = input.get("command").and_then(Value::as_str) else {
        return Vec::new();
    };
    if !creates_commit(command) {
        return Vec::new();
    }
    fn push(candidates: &mut Vec<String>, token: &str) {
        if is_hex_token(token) {
            let lowered = token.to_ascii_lowercase();
            if !candidates.contains(&lowered) {
                candidates.push(lowered);
            }
        }
    }
    let mut candidates = Vec::new();
    for line in result.lines() {
        let line = line.trim();
        // `[<branch> <short-sha>] <subject>`, possibly after other text.
        for (open, _) in line.match_indices('[') {
            if let Some(inside) = line[open + 1..].split(']').next() {
                if let Some((_, token)) = inside.rsplit_once(' ') {
                    push(&mut candidates, token);
                }
            }
        }
        // `CVC: Linked <n> thought(s) to commit <sha>.`
        if line.starts_with("CVC:") {
            if let Some((_, tail)) = line.rsplit_once(" to commit ") {
                push(&mut candidates, tail.trim_end_matches('.').trim());
            }
        }
        // `<sha> <subject>` or a lone `<sha>` at the start of a line.
        if let Some(first) = line.split_whitespace().next() {
            push(&mut candidates, first);
        }
    }
    // A full-length hash anywhere, as `git rev-parse HEAD` prints.
    for token in result.split(|c: char| !c.is_ascii_hexdigit()) {
        if token.len() == 40 {
            push(&mut candidates, token);
        }
    }
    candidates
}

fn creates_commit(command: &str) -> bool {
    // A crude but sufficient scan: the command mentions git and a
    // commit-producing verb. Verification against the repo is the real gate.
    let normalized = command.to_ascii_lowercase();
    normalized.contains("git")
        && (normalized.contains("commit")
            || normalized.contains("merge")
            || normalized.contains("cherry-pick")
            || normalized.contains("revert")
            || normalized.contains("rev-parse"))
}

fn is_hex_token(token: &str) -> bool {
    (7..=40).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_hexdigit())
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
    fn unknown_block_types_become_placeholders_and_fallback_is_skipped() {
        let assistant = serde_json::json!([
            {"type": "fallback", "from": {"model": "a"}, "to": {"model": "b"}},
            {"type": "text", "text": "hello"},
            {"type": "mystery", "payload": "never recorded"}
        ]);
        let blocks = parse_assistant_content(&assistant, 0).unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], AssistantBlock::Text(text) if text == "hello"));
        assert!(matches!(&blocks[1], AssistantBlock::Unrecorded(kind) if kind == "mystery"));

        let user = serde_json::json!([
            {"type": "text", "text": "hi"},
            {"type": "mystery", "payload": "never recorded"}
        ]);
        let message = parse_user_content(&user, 0, false, false).unwrap();
        assert_eq!(message.texts, vec!["hi".to_owned(), "[mystery]".to_owned()]);
        assert_eq!(message.unrecorded, vec!["mystery".to_owned()]);

        // A block without a type, or content of an unknown shape, still fails.
        assert!(parse_assistant_content(&serde_json::json!([{"text": "x"}]), 0).is_err());
        assert!(parse_user_content(&serde_json::json!(42), 0, false, false).is_err());
    }

    #[test]
    fn commit_candidates_only_from_committing_git_commands() {
        let commit = serde_json::json!({"command": "git commit -am work"});
        let candidates = commit_candidates_from_tool(
            "Bash",
            &commit,
            "[main 1a2b3c4] work\n 1 file changed, 2 insertions(+)",
        );
        assert_eq!(candidates, vec!["1a2b3c4".to_string()]);

        let revparse = serde_json::json!({"command": "git rev-parse HEAD"});
        let full = "a".repeat(40);
        assert_eq!(
            commit_candidates_from_tool("Bash", &revparse, &full),
            vec![full.clone()]
        );

        // A non-committing git command contributes nothing, even if its output
        // is full of hashes (a log, a diff).
        let log = serde_json::json!({"command": "git log --oneline"});
        assert!(commit_candidates_from_tool("Bash", &log, "1a2b3c4 old\n5d6e7f8 older").is_empty());
        // A non-Bash tool contributes nothing.
        assert!(commit_candidates_from_tool("Read", &commit, "[main 1a2b3c4] work").is_empty());
        // No spurious short token from ordinary prose or status output.
        assert!(commit_candidates_from_tool(
            "Bash",
            &commit,
            "nothing bracketed or hex-shaped here\n M fs.go\n?? deadbeef.txt"
        )
        .is_empty());

        // A quiet commit prints no summary line; the hash still reaches the
        // output through the agent's own `git log --oneline`, at the start of
        // a line, and through the line CVC's post-commit hook prints. Older
        // log entries are candidates too: verification rejects them by time.
        let quiet = serde_json::json!({
            "command": "git add -A && git commit -q -F - <<'EOF'\nfeature\nEOF\ngit log --oneline -3"
        });
        assert_eq!(
            commit_candidates_from_tool(
                "Bash",
                &quiet,
                "CVC: Linked 14 thought(s) to this commit.\n2a1fa36 Add roadmap\na3d57a5 Make copy safe\na7a6249 Merge pull request #36"
            ),
            vec![
                "2a1fa36".to_string(),
                "a3d57a5".to_string(),
                "a7a6249".to_string()
            ]
        );
        let hook_only = serde_json::json!({"command": "git commit -q -m feature"});
        assert_eq!(
            commit_candidates_from_tool(
                "Bash",
                &hook_only,
                "CVC: Linked 0 thought(s) to commit 2a1fa36b8c0d."
            ),
            vec!["2a1fa36b8c0d".to_string()]
        );
        // A short hash alone on a line, as `git rev-parse --short HEAD` prints.
        assert_eq!(
            commit_candidates_from_tool("Bash", &hook_only, "2a1fa36\n"),
            vec!["2a1fa36".to_string()]
        );
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

    // ---- provider-side (server) tool calls (public issue 134) ------------------

    const SERVER_SESSION: &str = "server-tools";

    fn entry_line(
        kind: &str,
        uuid: &str,
        parent: Option<&str>,
        request: Option<&str>,
        message: serde_json::Value,
    ) -> String {
        let mut entry = serde_json::json!({
            "parentUuid": parent,
            "isSidechain": false,
            "type": kind,
            "uuid": uuid,
            "timestamp": "2026-10-18T09:00:00.000Z",
            "sessionId": SERVER_SESSION,
            "version": "2.1.300",
            "cwd": "/work/repo",
            "message": message,
        });
        if let Some(request) = request {
            entry["requestId"] = serde_json::json!(request);
        }
        entry.to_string()
    }

    fn user_line(uuid: &str, parent: Option<&str>, content: serde_json::Value) -> String {
        let message = serde_json::json!({"role": "user", "content": content});
        entry_line("user", uuid, parent, None, message)
    }

    fn assistant_line(
        uuid: &str,
        parent: &str,
        request: &str,
        blocks: serde_json::Value,
    ) -> String {
        let message = serde_json::json!({
            "model": "claude-fixture-3", "id": "msg_x", "type": "message",
            "role": "assistant", "content": blocks
        });
        entry_line("assistant", uuid, Some(parent), Some(request), message)
    }

    /// A chained entry that is not a message, such as Claude Code's attachments.
    fn attachment_line(uuid: &str, parent: &str) -> String {
        serde_json::json!({
            "parentUuid": parent, "isSidechain": false, "type": "attachment",
            "attachment": {"type": "environment"}, "uuid": uuid,
            "timestamp": "2026-10-18T09:00:00.000Z", "sessionId": SERVER_SESSION,
            "version": "2.1.300", "cwd": "/work/repo"
        })
        .to_string()
    }

    fn plan_lines(lines: &[String], final_mode: bool) -> Plan {
        let text = lines.join("\n") + "\n";
        let window = parse_window(text.as_bytes(), 0, SERVER_SESSION, false).unwrap();
        plan(
            &window.lines,
            0,
            &PlanInput {
                session_id: SERVER_SESSION,
                worktree_root: Path::new("/work/repo"),
                path_aliases: &[],
                final_mode,
            },
        )
    }

    /// The single response whose content is `blocks`, after a plain prompt.
    fn respond(blocks: serde_json::Value) -> PlannedInteraction {
        let lines = [
            user_line("u1", None, serde_json::json!("go")),
            assistant_line("s1", "u1", "req_A", blocks),
        ];
        let mut planned = plan_lines(&lines, true);
        assert_eq!(planned.interactions.len(), 1);
        planned.interactions.remove(0)
    }

    /// Status recorded for one provider-side call answered by a `kind` block
    /// with `content`, after checking what else the capture holds.
    fn server_outcome(kind: &str, content: serde_json::Value) -> ToolStatus {
        let planned = respond(serde_json::json!([
            {"type": "server_tool_use", "id": "srvtoolu_1", "name": "probe", "input": {}},
            {"type": kind, "tool_use_id": "srvtoolu_1", "content": content}
        ]));
        assert_eq!(planned.tool_executions.len(), 1);
        assert_eq!(planned.tool_executions[0].tool_name, "server:probe");
        assert_eq!(planned.tool_executions[0].tool_protocol, "server");
        assert_eq!(
            planned.model_response.as_deref(),
            Some(format!("[{kind}]").as_str())
        );
        assert!(planned.unrecorded_blocks.is_empty(), "{planned:?}");
        planned.tool_executions[0].status.clone()
    }

    #[test]
    fn a_successful_server_call_is_recorded_as_a_server_success() {
        let planned = respond(serde_json::json!([
            {"type": "text", "text": "Searching."},
            {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search",
             "input": {"query": "fixture query"}},
            {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": [
                {"type": "web_search_result", "url": "https://example.invalid/a",
                 "title": "THIRD-PARTY-TITLE", "encrypted_content": "THIRD-PARTY-OPAQUE",
                 "page_age": null}
            ]},
            {"type": "text", "text": "Found it."}
        ]));
        assert_eq!(planned.tool_executions.len(), 1);
        let call = &planned.tool_executions[0];
        assert_eq!(call.tool_name, "server:web_search");
        assert_eq!(call.tool_protocol, "server");
        assert_eq!(call.status, ToolStatus::Success);
        assert_eq!(call.arguments, r#"{"query":"fixture query"}"#);
        // The block adds its placeholder in order, and nothing of what the
        // provider fetched for the model.
        assert_eq!(
            planned.model_response.as_deref(),
            Some("Searching.\n\n[web_search_tool_result]\n\nFound it.")
        );
        assert!(planned.unrecorded_blocks.is_empty());
        let captured = format!("{planned:?}");
        assert!(!captured.contains("THIRD-PARTY"), "{captured}");
        assert!(!captured.contains("example.invalid/a"), "{captured}");
    }

    #[test]
    fn an_errored_server_call_is_recorded_as_a_failure() {
        // The API reports errors as a single error object in `content`, not as
        // a raised error: `error_code`, typed `<kind>_error`.
        for (kind, content) in [
            (
                "web_search_tool_result",
                serde_json::json!({"type": "web_search_tool_result_error",
                                   "error_code": "max_uses_exceeded"}),
            ),
            (
                "web_fetch_tool_result",
                serde_json::json!({"type": "web_fetch_tool_error",
                                   "error_code": "url_not_accessible"}),
            ),
            (
                "tool_search_tool_result",
                serde_json::json!({"type": "tool_search_tool_result_error",
                                   "error_code": "invalid_pattern"}),
            ),
            // Either marker alone is enough.
            (
                "web_search_tool_result",
                serde_json::json!({"error_code": "unavailable"}),
            ),
            (
                "web_search_tool_result",
                serde_json::json!({"type": "web_search_tool_result_error"}),
            ),
        ] {
            assert_eq!(
                server_outcome(kind, content.clone()),
                ToolStatus::Failure,
                "{content}"
            );
        }
        // A list of hits (even an empty one) and a fetched page are successes.
        assert_eq!(
            server_outcome("web_search_tool_result", serde_json::json!([])),
            ToolStatus::Success
        );
        assert_eq!(
            server_outcome(
                "web_fetch_tool_result",
                serde_json::json!({"type": "web_fetch_result", "url": "https://example.invalid/p",
                                   "content": {"type": "document"}})
            ),
            ToolStatus::Success
        );
    }

    #[test]
    fn code_execution_status_follows_the_exit_code() {
        let ran = |kind: &str, code: serde_json::Value| {
            server_outcome(
                kind,
                serde_json::json!({"type": "bash_code_execution_result", "stdout": "",
                                   "stderr": "", "return_code": code, "content": []}),
            )
        };
        let bash = "bash_code_execution_tool_result";
        let python = "code_execution_tool_result";
        assert_eq!(ran(bash, serde_json::json!(0)), ToolStatus::Success);
        assert_eq!(ran(bash, serde_json::json!(2)), ToolStatus::Failure);
        assert_eq!(ran(bash, serde_json::json!(-9)), ToolStatus::Failure);
        assert_eq!(ran(python, serde_json::json!(0)), ToolStatus::Success);
        assert_eq!(ran(python, serde_json::json!(1)), ToolStatus::Failure);
        // Only an integer says how the command exited.
        assert_eq!(ran(bash, serde_json::Value::Null), ToolStatus::Success);
        assert_eq!(ran(bash, serde_json::json!("2")), ToolStatus::Success);
        assert_eq!(ran(bash, serde_json::json!(2.5)), ToolStatus::Success);
        // An execution that could not run is an error object, not an exit code.
        assert_eq!(
            server_outcome(
                bash,
                serde_json::json!({"type": "bash_code_execution_tool_result_error",
                                   "error_code": "execution_time_exceeded"})
            ),
            ToolStatus::Failure
        );

        // The output of a command never becomes a commit candidate.
        let planned = respond(serde_json::json!([
            {"type": "server_tool_use", "id": "srvtoolu_2", "name": "bash_code_execution",
             "input": {"command": "git commit -m x"}},
            {"type": bash, "tool_use_id": "srvtoolu_2", "content": {
                "type": "bash_code_execution_result", "stdout": "[main 1a2b3c4] x",
                "stderr": "", "return_code": 0, "content": []}}
        ]));
        assert_eq!(planned.tool_executions[0].status, ToolStatus::Success);
        assert!(planned.commit_candidates.is_empty());
        assert!(!format!("{planned:?}").contains("1a2b3c4"));
    }

    #[test]
    fn text_editor_results_succeed_unless_they_report_an_error() {
        let kind = "text_editor_code_execution_tool_result";
        for content in [
            serde_json::json!({"type": "text_editor_code_execution_view_result",
                               "content": "line", "file_type": "text", "num_lines": 1,
                               "start_line": 1, "total_lines": 1}),
            serde_json::json!({"type": "text_editor_code_execution_create_result",
                               "is_file_update": false}),
            serde_json::json!({"type": "text_editor_code_execution_str_replace_result",
                               "lines": ["x"], "new_lines": 1, "new_start": 1,
                               "old_lines": 1, "old_start": 1}),
        ] {
            assert_eq!(
                server_outcome(kind, content.clone()),
                ToolStatus::Success,
                "{content}"
            );
        }
        assert_eq!(
            server_outcome(
                kind,
                serde_json::json!({"type": "text_editor_code_execution_tool_result_error",
                                   "error_code": "file_not_found"})
            ),
            ToolStatus::Failure
        );
    }

    #[test]
    fn a_block_flagging_itself_as_an_error_is_a_failure() {
        // The MCP connector's result blocks carry `is_error` beside `content`.
        let planned = respond(serde_json::json!([
            {"type": "server_tool_use", "id": "srvtoolu_1", "name": "probe", "input": {}},
            {"type": "future_tool_result", "tool_use_id": "srvtoolu_1", "is_error": true,
             "content": [{"type": "text", "text": "boom"}]}
        ]));
        assert_eq!(planned.tool_executions[0].status, ToolStatus::Failure);
        assert!(!format!("{planned:?}").contains("boom"));
    }

    #[test]
    fn a_server_call_without_a_result_block_is_a_failure() {
        let planned = respond(serde_json::json!([
            {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search",
             "input": {"query": "never answered"}},
            // A result for some other call does not answer this one.
            {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_other", "content": []},
            {"type": "text", "text": "Done."}
        ]));
        assert_eq!(planned.tool_executions.len(), 1);
        assert_eq!(planned.tool_executions[0].tool_protocol, "server");
        assert_eq!(planned.tool_executions[0].status, ToolStatus::Failure);
        assert_eq!(
            planned.model_response.as_deref(),
            Some("[web_search_tool_result]\n\nDone.")
        );
    }

    #[test]
    fn a_server_result_answers_only_calls_of_its_own_response() {
        let lines = [
            user_line("u1", None, serde_json::json!("go")),
            assistant_line(
                "s1",
                "u1",
                "req_A",
                serde_json::json!([{"type": "server_tool_use", "id": "srvtoolu_x",
                                    "name": "web_search", "input": {"query": "a"}}]),
            ),
            assistant_line(
                "s2",
                "s1",
                "req_B",
                serde_json::json!([
                    {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_x", "content": []},
                    {"type": "server_tool_use", "id": "srvtoolu_y", "name": "web_search",
                     "input": {"query": "b"}},
                    {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_y", "content": []}
                ]),
            ),
        ];
        let planned = plan_lines(&lines, true).interactions;
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[0].tool_executions[0].status, ToolStatus::Failure);
        assert_eq!(planned[1].tool_executions.len(), 1);
        assert_eq!(planned[1].tool_executions[0].status, ToolStatus::Success);
    }

    #[test]
    fn client_tools_beside_server_calls_keep_their_own_rules() {
        let lines = [
            user_line("u1", None, serde_json::json!("go")),
            assistant_line(
                "s1",
                "u1",
                "req_A",
                serde_json::json!([
                    {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search",
                     "input": {"query": "q"}},
                    {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": []},
                    {"type": "tool_use", "id": "toolu_bash", "name": "Bash",
                     "input": {"command": "false"}},
                    {"type": "tool_use", "id": "toolu_mcp", "name": "mcp__srv__do", "input": {}},
                    {"type": "tool_use", "id": "toolu_read", "name": "Read",
                     "input": {"file_path": "/work/repo/src/lib.rs"}}
                ]),
            ),
            user_line(
                "u2",
                Some("s1"),
                serde_json::json!([
                    {"type": "tool_result", "tool_use_id": "toolu_bash", "content": "1",
                     "is_error": true},
                    {"type": "tool_result", "tool_use_id": "toolu_mcp", "content": "ok",
                     "is_error": false},
                    // A user-side result cannot overrule the provider's own.
                    {"type": "tool_result", "tool_use_id": "srvtoolu_1", "content": "no",
                     "is_error": true}
                ]),
            ),
        ];
        let planned = plan_lines(&lines, true).interactions;
        assert_eq!(planned.len(), 1);
        let summary: Vec<_> = planned[0]
            .tool_executions
            .iter()
            .map(|call| {
                (
                    call.tool_name.as_str(),
                    call.tool_protocol.as_str(),
                    call.status.clone(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("server:web_search", "server", ToolStatus::Success),
                ("Bash", "native", ToolStatus::Failure),
                ("mcp__srv__do", "mcp", ToolStatus::Success),
                // No result at all: interrupted.
                ("Read", "native", ToolStatus::Failure),
            ]
        );
        // Only the client Read became file context.
        assert_eq!(planned[0].context_items.len(), 1);
        assert_eq!(planned[0].context_items[0].file_path, "src/lib.rs");
    }

    #[test]
    fn result_blocks_are_recognized_by_their_shape() {
        let blocks = parse_assistant_content(
            &serde_json::json!([
                {"type": "tool_search_tool_result", "tool_use_id": "srvtoolu_1",
                 "content": {"type": "tool_search_tool_search_result", "tool_references": []}},
                {"type": "future_tool_result", "tool_use_id": "srvtoolu_2", "content": []},
                // Without a string id there is no call to answer: the three
                // types known before keep their placeholder, others are
                // unrecorded, and so is a bare `tool_result`.
                {"type": "web_fetch_tool_result", "content": []},
                {"type": "web_search_tool_result", "tool_use_id": 7, "content": []},
                {"type": "other_tool_result", "content": []},
                {"type": "tool_result", "tool_use_id": "srvtoolu_3", "content": []}
            ]),
            0,
        )
        .unwrap();
        assert!(matches!(
            &blocks[0],
            AssistantBlock::ServerToolResult { tool_use_id, placeholder, status }
                if tool_use_id == "srvtoolu_1"
                    && placeholder == "[tool_search_tool_result]"
                    && *status == ToolStatus::Success
        ));
        assert!(matches!(
            &blocks[1],
            AssistantBlock::ServerToolResult { tool_use_id, placeholder, .. }
                if tool_use_id == "srvtoolu_2" && placeholder == "[future_tool_result]"
        ));
        assert!(
            matches!(&blocks[2], AssistantBlock::Text(text) if text == "[web_fetch_tool_result]")
        );
        assert!(
            matches!(&blocks[3], AssistantBlock::Text(text) if text == "[web_search_tool_result]")
        );
        assert!(
            matches!(&blocks[4], AssistantBlock::Unrecorded(kind) if kind == "other_tool_result")
        );
        assert!(matches!(&blocks[5], AssistantBlock::Unrecorded(kind) if kind == "tool_result"));

        // Recognized results are not "unrecorded"; the others still are.
        let planned = respond(serde_json::json!([
            {"type": "server_tool_use", "id": "srvtoolu_1", "name": "tool_search", "input": {}},
            {"type": "tool_search_tool_result", "tool_use_id": "srvtoolu_1", "content": []},
            {"type": "future_tool_result", "content": []}
        ]));
        assert_eq!(
            planned.unrecorded_blocks,
            vec!["future_tool_result".to_owned()]
        );
        assert_eq!(
            planned.model_response.as_deref(),
            Some("[tool_search_tool_result]\n\n[future_tool_result]")
        );
    }

    #[test]
    fn an_unanswered_server_call_does_not_close_its_response_early() {
        let prompt = user_line("u1", None, serde_json::json!("go"));
        let call = assistant_line(
            "s1",
            "u1",
            "req_A",
            serde_json::json!([{"type": "server_tool_use", "id": "srvtoolu_1",
                                "name": "web_search", "input": {"query": "q"}}]),
        );
        let result = assistant_line(
            "s2",
            "s1",
            "req_A",
            serde_json::json!([{"type": "web_search_tool_result",
                                "tool_use_id": "srvtoolu_1", "content": []}]),
        );
        let thanks = user_line("u2", Some("s2"), serde_json::json!("thanks"));

        // Something follows the call, but its result may still be on the way:
        // recording a failure now would be permanent.
        let waiting = plan_lines(
            &[prompt.clone(), call.clone(), attachment_line("a1", "s1")],
            false,
        );
        assert!(waiting.interactions.is_empty());
        assert_eq!(waiting.pending_responses, 1);

        // The conversation moving on closes it, with the outcome it reported...
        let answered = plan_lines(&[prompt.clone(), call.clone(), result, thanks], false);
        assert_eq!(answered.pending_responses, 0);
        assert_eq!(answered.interactions.len(), 1);
        assert_eq!(
            answered.interactions[0].tool_executions[0].status,
            ToolStatus::Success
        );

        // ...or, with no result ever, as a failure.
        let abandoned = plan_lines(
            &[
                prompt,
                call,
                user_line("u2", Some("s1"), serde_json::json!("thanks")),
            ],
            false,
        );
        assert_eq!(abandoned.pending_responses, 0);
        assert_eq!(
            abandoned.interactions[0].tool_executions[0].status,
            ToolStatus::Failure
        );
    }
}
