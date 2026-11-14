//! Anthropic Messages API types — requests, streaming-ready content blocks,
//! tool definitions and usage counters. Only the subset Beyond CLI uses.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------- messages

/// A single conversation message. Content is always a block list so we can
/// interleave text, tool_use and tool_result freely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
    pub fn tool_result(
        tool_use_id: String,
        text: String,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::User, // tool results are always user-role on the wire
            content: vec![ContentBlock::ToolResult {
                tool_use_id,
                content: text,
                is_error,
            }],
        }
    }
    /// Concatenated plain text of all text blocks (for chat display).
    pub fn plain_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// Whether this message contains any tool_use blocks.
    pub fn has_tool_use(&self) -> bool {
        self.content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    }
    /// Human title for session lists (first ~60 chars of first text block).
    pub fn title(&self) -> String {
        crate::util::one_line(&self.plain_text(), 60)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// Content blocks on the wire. Streaming deltas are merged into these by the
/// SSE client before being handed to the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: Value },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        /// Anthropic accepts a string directly; keep it simple.
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

// ------------------------------------------------------------------- tools

/// A tool definition sent in the `tools` array of the API request.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl ToolDef {
    /// Build a def from a JSON-schema object.
    pub fn new(name: &str, description: &str, schema: Value) -> Self {
        Self { name: name.into(), description: description.into(), input_schema: schema }
    }
}

/// Result of executing a tool (ours or an MCP server's).
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false }
    }
    pub fn err(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true }
    }
}

// ------------------------------------------------------------------- usage

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

