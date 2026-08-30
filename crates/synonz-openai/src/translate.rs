//! Pure translation between Synonz canonical messages and the OpenAI
//! chat-completions wire format.
//!
//! Wire-shape decisions locked by the canonical/openai difference table:
//!
//! | Canonical | OpenAI |
//! |---|---|
//! | `System` text | `{"role":"system","content":...}` |
//! | `Assistant` text + tool calls | `content` + `tool_calls[].function.arguments` (JSON *string*) |
//! | `Tool` result | `{"role":"tool","tool_call_id":...,"content":...}` |
//! | `ToolResult::Err` | no native flag; `[tool error]` text prefix |

use serde_json::{Value, json};
use synonz::message::{ContentBlock, Message, Role, ToolResult};
use synonz::{ModelDelta, ModelError, ModelStreamItem};

/// Builds the chat-completions request body.
pub(crate) fn request_body(
    model_name: &str,
    request: &synonz::ModelRequest,
) -> Result<Value, ModelError> {
    let mut body = json!({
        "model": model_name,
        "messages": to_wire_messages(&request.messages)?,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|spec| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": spec.name,
                            "description": spec.description,
                            "parameters": spec.parameters_schema,
                        }
                    })
                })
                .collect(),
        );
    }
    if let Some(temperature) = request.params.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(max_tokens) = request.params.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    Ok(body)
}

/// Translates canonical messages to the wire message array.
pub(crate) fn to_wire_messages(messages: &[Message]) -> Result<Vec<Value>, ModelError> {
    messages.iter().map(to_wire_message).collect()
}

fn to_wire_message(message: &Message) -> Result<Value, ModelError> {
    match message.role {
        Role::System | Role::User => {
            let mut content = String::new();
            for block in &message.blocks {
                match block {
                    ContentBlock::Text { text } => content.push_str(text),
                    other => {
                        return Err(ModelError::InvalidRequest {
                            message: format!(
                                "{:?} messages support text blocks only in v1 (got {other:?})",
                                message.role
                            ),
                        });
                    }
                }
            }
            Ok(json!({ "role": message.role, "content": content }))
        }
        Role::Assistant => {
            let mut content = String::new();
            let mut tool_calls = Vec::new();
            for block in &message.blocks {
                match block {
                    ContentBlock::Text { text } => content.push_str(text),
                    ContentBlock::ToolCall(call) => tool_calls.push(json!({
                        "id": call.call_id.as_str(),
                        "type": "function",
                        "function": {
                            "name": call.name,
                            // OpenAI carries arguments as a JSON *string*.
                            "arguments": call.arguments.to_string(),
                        }
                    })),
                    other => {
                        return Err(ModelError::InvalidRequest {
                            message: format!("invalid block in assistant message: {other:?}"),
                        });
                    }
                }
            }
            let mut wire = json!({ "role": "assistant" });
            if !content.is_empty() {
                wire["content"] = json!(content);
            }
            if !tool_calls.is_empty() {
                wire["tool_calls"] = Value::Array(tool_calls);
            }
            Ok(wire)
        }
        Role::Tool => {
            let mut wire_messages = Vec::new();
            for block in &message.blocks {
                match block {
                    ContentBlock::ToolResult { call_id, result } => {
                        wire_messages.push(json!({
                            "role": "tool",
                            "tool_call_id": call_id.as_str(),
                            "content": tool_content_string(result)?,
                        }));
                    }
                    other => {
                        return Err(ModelError::InvalidRequest {
                            message: format!(
                                "only tool result blocks are allowed in tool messages: {other:?}"
                            ),
                        });
                    }
                }
            }
            Ok(Value::Array(wire_messages))
        }
        _ => Err(ModelError::InvalidRequest {
            message: format!("unsupported message role: {:?}", message.role),
        }),
    }
}

/// Tool result content as an OpenAI `content` string.
fn tool_content_string(result: &ToolResult) -> Result<String, ModelError> {
    match result {
        ToolResult::Ok { content } => match content {
            synonz::ToolContent::Text { text } => Ok(text.clone()),
            synonz::ToolContent::Json { value } => Ok(value.to_string()),
            other => Err(ModelError::InvalidRequest {
                message: format!("unsupported tool content: {other:?}"),
            }),
        },
        // OpenAI has no native error flag; the convention is a text prefix.
        ToolResult::Err { message } => Ok(format!("[tool error] {message}")),
        other => Err(ModelError::InvalidRequest {
            message: format!("unsupported tool result: {other:?}"),
        }),
    }
}

/// Streaming-response accumulator: merges deltas into the final assistant
/// message and extracts usage.
#[derive(Default)]
pub(crate) struct ResponseAccumulator {
    text: String,
    // OpenAI streams tool calls as fragments keyed by index.
    calls: std::collections::BTreeMap<u64, (String, String, String)>,
    usage: Option<synonz::TokenUsage>,
}

impl ResponseAccumulator {
    /// Applies one SSE data payload (a JSON chunk), returning the items it
    /// produces: one [`ModelStreamItem::Delta`] per response text fragment,
    /// and a terminal [`ModelStreamItem::Finish`] when the chunk carried a
    /// finish signal.
    #[allow(dead_code)]
    pub(crate) fn apply_chunk(
        &mut self,
        chunk: &Value,
    ) -> Result<Vec<ModelStreamItem>, ModelError> {
        if let Some(usage) = chunk.get("usage")
            && !usage.is_null()
        {
            self.usage = Some(synonz::TokenUsage::new(
                usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ));
        }
        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        let mut items = Vec::new();
        let mut finished = false;
        for choice in choices {
            let delta = &choice["delta"];
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                self.text.push_str(text);
                items.push(ModelStreamItem::Delta(ModelDelta::Text {
                    text: text.to_string(),
                }));
            }
            if let Some(fragments) = delta.get("tool_calls").and_then(Value::as_array) {
                for fragment in fragments {
                    let index = fragment.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let entry = self.calls.entry(index).or_default();
                    if let Some(id) = fragment.get("id").and_then(Value::as_str) {
                        entry.0 = id.to_string();
                    }
                    if let Some(function) = fragment.get("function") {
                        if let Some(name) = function.get("name").and_then(Value::as_str) {
                            entry.1 = name.to_string();
                        }
                        if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                            entry.2.push_str(arguments);
                        }
                    }
                }
            }
            if choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| !reason.is_empty())
            {
                finished = true;
            }
        }
        if finished && let Some(item) = self.finish_item() {
            items.push(item);
        }
        Ok(items)
    }

    /// Builds the terminal item from the accumulated deltas, or `None` when
    /// nothing accumulated.
    pub(crate) fn finish_item(&mut self) -> Option<ModelStreamItem> {
        if !self.has_content() {
            return None;
        }
        let usage = self.usage_or_zero();
        let message = std::mem::take(self).into_message().ok()?;
        Some(ModelStreamItem::Finish { message, usage })
    }

    /// Appends text to the accumulated response text.
    pub(crate) fn push_text(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Merges a usage object (if any) into the accumulator.
    pub(crate) fn absorb_usage(&mut self, usage: &Value) {
        if usage.is_null() {
            return;
        }
        self.usage = Some(synonz::TokenUsage::new(
            usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ));
    }

    /// Whether any response content (text or tool calls) accumulated.
    pub(crate) fn has_content(&self) -> bool {
        !self.text.is_empty() || !self.calls.is_empty()
    }

    /// Builds the final assistant message from the accumulated deltas.
    pub(crate) fn into_message(mut self) -> Result<Message, ModelError> {
        let mut blocks = Vec::new();
        if !self.text.is_empty() {
            blocks.push(ContentBlock::Text {
                text: std::mem::take(&mut self.text),
            });
        }
        let mut indices: Vec<u64> = self.calls.keys().copied().collect();
        indices.sort_unstable();
        for index in indices {
            let (id, name, arguments) = &self.calls[&index];
            let arguments: Value =
                serde_json::from_str(arguments).map_err(|error| ModelError::InvalidRequest {
                    message: format!("malformed tool call arguments: {error}"),
                })?;
            blocks.push(ContentBlock::ToolCall(synonz::message::ToolCall::new(
                id.clone(),
                name.clone(),
                arguments,
            )));
        }
        Ok(Message::new(Role::Assistant, blocks))
    }

    /// The reported usage, or zeros when the backend did not report any.
    pub(crate) fn usage_or_zero(&self) -> synonz::TokenUsage {
        self.usage.unwrap_or(synonz::TokenUsage::new(0, 0))
    }
}

/// Maps an HTTP status to a [`ModelError`] with the response body as
/// context.
pub(crate) fn status_error(status: u16, body: String) -> ModelError {
    match status {
        429 => ModelError::RateLimited { message: body },
        400 | 404 | 422 => ModelError::InvalidRequest { message: body },
        _ => ModelError::Api { message: body },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synonz::ToolSpec;

    #[test]
    fn request_body_carries_tools_and_params() {
        let request = synonz::ModelRequest::new(
            vec![Message::user("hi")],
            vec![ToolSpec::new(
                "weather",
                "weather lookup",
                json!({"type": "object"}),
            )],
            synonz::ModelParams::default()
                .with_temperature(0.3)
                .with_max_tokens(256),
        );
        let body = request_body("gpt-4o-mini", &request).unwrap();
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["stream"], true);
        assert_eq!(body["temperature"], json!(0.3_f32));
        assert_eq!(body["max_tokens"], 256);
        assert_eq!(body["tools"][0]["function"]["name"], "weather");
    }

    #[test]
    fn assistant_tool_calls_serialize_arguments_as_string() {
        let message = Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolCall(synonz::message::ToolCall::new(
                "x1",
                "weather",
                json!({"city": "beijing"}),
            ))],
        );
        let wire = to_wire_message(&message).unwrap();
        assert_eq!(wire["tool_calls"][0]["id"], "x1");
        assert_eq!(
            wire["tool_calls"][0]["function"]["arguments"],
            json!("{\"city\":\"beijing\"}"),
            "arguments must be a JSON string on the wire"
        );
    }

    #[test]
    fn tool_error_gets_text_prefix() {
        let message = synonz::Message::tool_result(
            "x1",
            ToolResult::Err {
                message: "city not found".into(),
            },
        );
        let wire = to_wire_message(&message).unwrap();
        let wire_array = wire.as_array().unwrap();
        assert_eq!(
            wire_array[0]["content"], "[tool error] city not found",
            "OpenAI has no error flag; convention is a text prefix"
        );
    }

    #[test]
    fn accumulator_merges_fragments_into_final_message() {
        let mut accumulator = ResponseAccumulator::default();
        accumulator
            .apply_chunk(&json!({
                "choices": [{"delta": {"content": "beijing "}, "finish_reason": null}]
            }))
            .unwrap();
        let items = accumulator
            .apply_chunk(&json!({
                "choices": [{"delta": {"content": "is sunny"}, "finish_reason": null}]
            }))
            .unwrap();
        assert_eq!(items.len(), 1);
        let _finished = accumulator
            .apply_chunk(&json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "x1",
                            "function": {"name": "weather", "arguments": "{\"city\":\"bei"}
                        }]
                    },
                    "finish_reason": null
                }]
            }))
            .unwrap();
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], ModelStreamItem::Delta(_)));
        let items = accumulator
            .apply_chunk(&json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": {"arguments": "jing\"}"}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {"prompt_tokens": 7, "completion_tokens": 3}
            }))
            .unwrap();
        assert_eq!(items.len(), 1, "finish chunk produces the terminal item");
        let ModelStreamItem::Finish { message, usage } = &items[0] else {
            panic!("expected a finish item");
        };
        assert_eq!(*usage, synonz::TokenUsage::new(7, 3));
        let text = message.blocks.iter().find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        });
        assert_eq!(text.as_deref(), Some("beijing is sunny"));
        assert!(message.blocks.iter().any(|block| matches!(
            block,
            ContentBlock::ToolCall(call)
                if call.call_id == synonz::CallId::new("x1")
                    && call.name == "weather"
                    && call.arguments == json!({"city": "beijing"})
        )));
    }

    #[test]
    fn status_maps_to_error_kinds() {
        assert!(matches!(
            status_error(429, "slow down".into()),
            ModelError::RateLimited { .. }
        ));
        assert!(matches!(
            status_error(400, "bad".into()),
            ModelError::InvalidRequest { .. }
        ));
        assert!(matches!(
            status_error(500, "boom".into()),
            ModelError::Api { .. }
        ));
    }
}
