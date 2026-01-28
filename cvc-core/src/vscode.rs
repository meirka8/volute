use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatSession {
    pub version: Option<u32>,
    pub requests: Vec<ChatRequest>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub request_id: String,
    pub message: ChatMessage,
    pub response: Option<Vec<ResponsePartRaw>>,
    pub variable_data: Option<VariableData>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatMessage {
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VariableData {
    pub variables: Vec<Variable>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Variable {
    pub kind: String,
    pub value: Value,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ToolMessage {
    pub value: String,
}

/// Helper enum to handle polymorphic fields that can be either a String or a ToolMessage object.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum ToolMessageOrString {
    Msg(ToolMessage),
    Str(String),
}

impl ToolMessageOrString {
    pub fn value(&self) -> &str {
        match self {
            Self::Msg(m) => &m.value,
            Self::Str(s) => s,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TextEdit {
    pub text: String,
    pub range: Value,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ResponsePartRaw {
    pub kind: Option<String>,
    pub value: Option<String>,

    #[serde(rename = "invocationMessage")]
    pub invocation_message: Option<ToolMessageOrString>,

    #[serde(rename = "pastTenseMessage")]
    pub past_tense_message: Option<ToolMessageOrString>,

    pub edits: Option<Vec<Vec<TextEdit>>>,

    #[serde(rename = "toolName")]
    pub tool_name: Option<String>,

    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

impl ChatSession {
    pub fn parse(json_content: &str) -> anyhow::Result<Self> {
        let session: Self = serde_json::from_str(json_content)?;
        Ok(session)
    }
}

impl ChatRequest {
    pub fn reconstruct_full_response(&self) -> String {
        self.response
            .as_ref()
            .map(|parts| Self::reconstruct_from_parts(parts))
            .unwrap_or_default()
    }

    pub fn reconstruct_from_parts(parts: &[ResponsePartRaw]) -> String {
        let mut output = String::new();
        for part in parts {
            let kind = part.kind.as_deref().unwrap_or("text");

            match kind {
                "text" | "markdownContent" => {
                    if let Some(val) = &part.value {
                        output.push_str(val);
                    }
                }
                "thinking" => {
                    // Skip thinking for now
                }
                "toolInvocationSerialized" => {
                    if let Some(msg) = &part.past_tense_message {
                        if !output.ends_with('\n') && !output.is_empty() {
                            output.push('\n');
                        }
                        output.push_str(msg.value());
                        output.push('\n');
                    } else if let Some(msg) = &part.invocation_message {
                        if !output.ends_with('\n') && !output.is_empty() {
                            output.push('\n');
                        }
                        output.push_str(msg.value());
                        output.push('\n');
                    }
                }
                "textEditGroup" => {
                    // Ignore content
                }
                "undoStop" | "prepareToolInvocation" | "mcpServersStarting" => {
                    // Ignore
                }
                _ => {
                    if let Some(val) = &part.value {
                        output.push_str(val);
                    }
                }
            }
        }
        output
    }
}
