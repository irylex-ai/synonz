//! Canonical message types shared by the agent loop, model adapters, and
//! tool bridges.
//!
//! Synonz defines its own canonical conversation form
//! ([`Message`] = [`Role`] + content blocks). Provider adapters translate
//! between this form and their wire formats; the agent loop, event model,
//! and tool bridges only ever see the canonical form.
//!
//! # Canonical invariants
//!
//! A well-formed conversation satisfies:
//!
//! 1. [`ContentBlock::ToolCall`] blocks appear only in [`Role::Assistant`]
//!    messages;
//! 2. [`ContentBlock::ToolResult`] blocks appear only in [`Role::Tool`]
//!    messages;
//! 3. every [`ContentBlock::ToolResult`] references, via [`CallId`], a
//!    [`ContentBlock::ToolCall`] that appears earlier in the conversation.
//!
//! Use [`validate_conversation`] to check these invariants.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Opaque identifier correlating a tool call with its tool result.
///
/// Ids originate from provider responses (or the framework for local
/// execution) and pair [`ContentBlock::ToolCall`] with
/// [`ContentBlock::ToolResult`] blocks, including across parallel calls.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CallId(pub String);

impl CallId {
    /// Creates a call id from a string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Views the id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for CallId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for CallId {
    fn from(id: &str) -> Self {
        Self(id.into())
    }
}

impl From<String> for CallId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

/// The role of a [`Message`] in a conversation.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// System instructions shaping agent behavior.
    System,
    /// Input from the user or application.
    User,
    /// Output produced by the model.
    Assistant,
    /// Tool execution results fed back to the model.
    Tool,
}

/// A capability invocation requested by the model.
///
/// Carried by [`ContentBlock::ToolCall`] in assistant messages and echoed by
/// [`AgentEvent::Tool`][crate::AgentEvent] events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Correlation id pairing this call with its result.
    pub call_id: CallId,
    /// Name of the tool to invoke.
    pub name: String,
    /// Tool arguments as a JSON object.
    pub arguments: Value,
}

impl ToolCall {
    /// Creates a tool call.
    pub fn new(call_id: impl Into<CallId>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            call_id: call_id.into(),
            name: name.into(),
            arguments,
        }
    }
}

impl From<&ToolCall> for ToolCall {
    fn from(call: &ToolCall) -> Self {
        call.clone()
    }
}

/// Successful content returned by a tool.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolContent {
    /// Plain text content.
    Text {
        /// The text payload.
        text: String,
    },
    /// Structured JSON content.
    Json {
        /// The JSON payload.
        value: Value,
    },
}

/// The outcome of a tool invocation.
///
/// `Err` is a *soft failure*: the message is fed back to the model, which
/// may retry, adjust, or abandon. Tool failures do not terminate a run.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResult {
    /// Successful execution with content for the model.
    Ok {
        /// The content payload.
        content: ToolContent,
    },
    /// Soft failure; `message` is shown to the model.
    Err {
        /// Human-readable failure description for the model.
        message: String,
    },
}

/// One block of content inside a [`Message`].
///
/// Blocks form a sequence so that a single message can mix text and tool
/// activity (for example, an assistant message that narrates and then calls
/// a tool), and so that multimodal blocks can be added later without
/// breaking changes.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "block", rename_all = "snake_case")]
pub enum ContentBlock {
    /// A text block.
    Text {
        /// The text payload.
        text: String,
    },
    /// A tool call requested by the model (assistant messages only).
    ToolCall(ToolCall),
    /// A tool result fed back to the model (tool messages only).
    ToolResult {
        /// Correlation id of the answered [`ContentBlock::ToolCall`].
        call_id: CallId,
        /// The tool outcome.
        result: ToolResult,
    },
}

/// A single canonical conversation message.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// The authoring role of this message.
    pub role: Role,
    /// Ordered content blocks.
    pub blocks: Vec<ContentBlock>,
}

impl Message {
    /// Creates a message from a role and content blocks.
    pub fn new(role: Role, blocks: Vec<ContentBlock>) -> Self {
        Self { role, blocks }
    }

    /// Creates a [`Role::System`] message with a single text block.
    pub fn system(text: impl Into<String>) -> Self {
        Self::text_message(Role::System, text)
    }

    /// Creates a [`Role::User`] message with a single text block.
    pub fn user(text: impl Into<String>) -> Self {
        Self::text_message(Role::User, text)
    }

    /// Creates an [`Role::Assistant`] message with a single text block.
    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self::text_message(Role::Assistant, text)
    }

    /// Creates a [`Role::Tool`] message carrying one tool result.
    pub fn tool_result(call_id: impl Into<CallId>, result: ToolResult) -> Self {
        Self::new(
            Role::Tool,
            vec![ContentBlock::ToolResult {
                call_id: call_id.into(),
                result,
            }],
        )
    }

    fn text_message(role: Role, text: impl Into<String>) -> Self {
        Self::new(role, vec![ContentBlock::Text { text: text.into() }])
    }
}

impl From<&Message> for Message {
    fn from(message: &Message) -> Self {
        message.clone()
    }
}

/// A violation of the canonical conversation invariants.
///
/// Produced by [`validate_conversation`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Error, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalViolation {
    /// A tool call block appeared outside an assistant message.
    #[error("tool call block in non-assistant message (role: {role:?})")]
    ToolCallInNonAssistantMessage {
        /// The offending role.
        role: Role,
    },
    /// A tool result block appeared outside a tool message.
    #[error("tool result block in non-tool message (role: {role:?})")]
    ToolResultInNonToolMessage {
        /// The offending role.
        role: Role,
    },
    /// A tool result referenced a call id with no earlier matching call.
    #[error("tool result references unknown call id: {call_id}")]
    UnknownCallId {
        /// The dangling call id.
        call_id: CallId,
    },
}

/// Validates the canonical invariants of a conversation.
///
/// Checks that tool calls only appear in assistant messages, tool results
/// only in tool messages, and every tool result references an earlier tool
/// call by [`CallId`]. Returns the first violation found.
///
/// The agent loop maintains these invariants by construction; this helper
/// exists for debugging and for custom execution engines built directly on
/// the public primitives.
pub fn validate_conversation(messages: &[Message]) -> Result<(), CanonicalViolation> {
    let mut known_calls: std::collections::HashSet<&CallId> = std::collections::HashSet::new();
    for message in messages {
        for block in &message.blocks {
            match block {
                ContentBlock::ToolCall(call) => {
                    if message.role != Role::Assistant {
                        return Err(CanonicalViolation::ToolCallInNonAssistantMessage {
                            role: message.role,
                        });
                    }
                    known_calls.insert(&call.call_id);
                }
                ContentBlock::ToolResult { call_id, .. } => {
                    if message.role != Role::Tool {
                        return Err(CanonicalViolation::ToolResultInNonToolMessage {
                            role: message.role,
                        });
                    }
                    if !known_calls.contains(call_id) {
                        return Err(CanonicalViolation::UnknownCallId {
                            call_id: call_id.clone(),
                        });
                    }
                }
                ContentBlock::Text { .. } => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_conversation() -> Vec<Message> {
        vec![
            Message::system("you are a weather assistant"),
            Message::user("weather in beijing?"),
            Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::Text {
                        text: "let me check.".into(),
                    },
                    ContentBlock::ToolCall(ToolCall::new(
                        "x1",
                        "weather",
                        json!({"city": "beijing"}),
                    )),
                ],
            ),
            Message::tool_result(
                "x1",
                ToolResult::Ok {
                    content: ToolContent::Text {
                        text: "sunny, 28C".into(),
                    },
                },
            ),
            Message::assistant_text("beijing is sunny, 28C."),
        ]
    }

    #[test]
    fn valid_conversation_passes() {
        assert_eq!(validate_conversation(&sample_conversation()), Ok(()));
    }

    #[test]
    fn tool_call_outside_assistant_is_rejected() {
        let messages = vec![Message::new(
            Role::User,
            vec![ContentBlock::ToolCall(ToolCall::new(
                "x1",
                "weather",
                json!({}),
            ))],
        )];
        assert!(matches!(
            validate_conversation(&messages),
            Err(CanonicalViolation::ToolCallInNonAssistantMessage { .. })
        ));
    }

    #[test]
    fn tool_result_outside_tool_message_is_rejected() {
        let messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolResult {
                call_id: CallId::new("x1"),
                result: ToolResult::Err {
                    message: "boom".into(),
                },
            }],
        )];
        assert!(matches!(
            validate_conversation(&messages),
            Err(CanonicalViolation::ToolResultInNonToolMessage { .. })
        ));
    }

    #[test]
    fn dangling_call_id_is_rejected() {
        let messages = vec![Message::tool_result(
            "nope",
            ToolResult::Err {
                message: "unknown call".into(),
            },
        )];
        assert!(matches!(
            validate_conversation(&messages),
            Err(CanonicalViolation::UnknownCallId { .. })
        ));
    }

    #[test]
    fn parallel_calls_pair_by_call_id() {
        let messages = vec![
            Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::ToolCall(ToolCall::new("a", "t1", json!({}))),
                    ContentBlock::ToolCall(ToolCall::new("b", "t2", json!({}))),
                ],
            ),
            Message::new(
                Role::Tool,
                vec![
                    ContentBlock::ToolResult {
                        call_id: CallId::new("b"),
                        result: ToolResult::Err {
                            message: "second".into(),
                        },
                    },
                    ContentBlock::ToolResult {
                        call_id: CallId::new("a"),
                        result: ToolResult::Err {
                            message: "first".into(),
                        },
                    },
                ],
            ),
        ];
        assert_eq!(validate_conversation(&messages), Ok(()));
    }

    #[test]
    fn call_id_serializes_transparently() {
        let id = CallId::new("x1");
        assert_eq!(serde_json::to_value(&id).unwrap(), json!("x1"));
    }
}
