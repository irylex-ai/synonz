//! Pure translation between Synonz canonical messages and the Anthropic
//! Messages API wire format.
//!
//! Wire-shape decisions locked by the canonical/anthropic difference table:
//!
//! | Canonical | Anthropic |
//! |---|---|
//! | `System` text | top-level `system` parameter (not a message) |
//! | any message | `content` is a block *array* |
//! | `Assistant` tool call | `{"type":"tool_use","id","name","input":{object}}` |
//! | `Tool` result | merged into a **user** message with native `is_error` |

use std::collections::BTreeMap;

use serde_json::{Value, json};

use synonz::message::{ContentBlock, Message, Role, ToolResult};
use synonz::{ModelDelta, ModelError, ModelStreamItem, TokenUsage};

/// Builds the `/v1/messages` request body.
pub(crate) fn request_body(
    model_name: &str,
    request: &synonz::ModelRequest,
) -> Result<Value, ModelError> {
    let mut system = String::new();
    let mut messages = Vec::new();
    for message in &request.messages {
        match message.role {
            Role::System => {
                for block in &message.blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            if !system.is_empty() {
                                system.push('\n');
                            }
                            system.push_str(text);
                        }
                        other => return Err(invalid_block("system", other)),
                    }
                }
            }
            Role::User => messages.push(user_message(message)?),
            Role::Assistant => messages.push(assistant_message(message)?),
            Role::Tool => messages.extend(tool_result_messages(message)?),
            other => {
                return Err(ModelError::InvalidRequest {
                    message: format!("unsupported message role: {other:?}"),
                });
            }
        }
    }

    let mut body = json!({
        "model": model_name,
        // Anthropic requires max_tokens; the default is documented.
        "max_tokens": request.params.max_tokens.unwrap_or(crate::DEFAULT_MAX_TOKENS),
        "stream": true,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|spec| {
                    json!({
                        "name": spec.name,
                        "description": spec.description,
                        "input_schema": spec.parameters_schema,
                    })
                })
                .collect(),
        );
    }
    if let Some(temperature) = request.params.temperature {
        body["temperature"] = json!(temperature);
    }
    Ok(body)
}

fn invalid_block(where_: &str, block: &ContentBlock) -> ModelError {
    ModelError::InvalidRequest {
        message: format!("unsupported block in {where_} message: {block:?}"),
    }
}

fn user_message(message: &Message) -> Result<Value, ModelError> {
    let mut blocks = Vec::new();
    for block in &message.blocks {
        match block {
            ContentBlock::Text { text } => blocks.push(json!({"type": "text", "text": text})),
            other => return Err(invalid_block("user", other)),
        }
    }
    Ok(json!({ "role": "user", "content": blocks }))
}

fn assistant_message(message: &Message) -> Result<Value, ModelError> {
    let mut blocks = Vec::new();
    for block in &message.blocks {
        match block {
            ContentBlock::Text { text } => blocks.push(json!({"type": "text", "text": text})),
            ContentBlock::ToolCall(call) => blocks.push(json!({
                "type": "tool_use",
                "id": call.call_id.as_str(),
                "name": call.name,
                // Anthropic carries the arguments as a JSON *object*.
                "input": call.arguments,
            })),
            other => return Err(invalid_block("assistant", other)),
        }
    }
    Ok(json!({ "role": "assistant", "content": blocks }))
}

/// Tool results merge into user-role messages with native `is_error`.
fn tool_result_messages(message: &Message) -> Result<Vec<Value>, ModelError> {
    let mut blocks = Vec::new();
    for block in &message.blocks {
        match block {
            ContentBlock::ToolResult { call_id, result } => {
                let (content, is_error) = match result {
                    ToolResult::Ok { content } => match content {
                        synonz::ToolContent::Text { text } => (text.clone(), false),
                        synonz::ToolContent::Json { value } => (value.to_string(), false),
                        other => {
                            return Err(ModelError::InvalidRequest {
                                message: format!("unsupported tool content: {other:?}"),
                            });
                        }
                    },
                    ToolResult::Err { message } => (message.clone(), true),
                    other => {
                        return Err(ModelError::InvalidRequest {
                            message: format!("unsupported tool result: {other:?}"),
                        });
                    }
                };
                blocks.push(json!({
                    "type": "tool_result",
                    "tool_use_id": call_id.as_str(),
                    "content": content,
                    "is_error": is_error,
                }));
            }
            other => return Err(invalid_block("tool", other)),
        }
    }
    Ok(vec![json!({ "role": "user", "content": blocks })])
}

#[derive(Debug, Clone, Default)]
enum AccumBlock {
    #[default]
    Unused,
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        args: String,
    },
}

/// Streaming-response accumulator: block-indexed content merging, usage
/// extraction, and terminal finish synthesis.
#[derive(Default)]
pub(crate) struct ResponseAccumulator {
    blocks: BTreeMap<u64, AccumBlock>,
    input_tokens: u64,
    output_tokens: u64,
}

impl ResponseAccumulator {
    /// Applies one SSE data payload (a JSON event). Returns the item the
    /// event produces: a text delta, the terminal finish, or a failure.
    pub(crate) fn apply_event(
        &mut self,
        event: &Value,
    ) -> Result<Option<ModelStreamItem>, ModelError> {
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(usage) = event
                    .pointer("/message/usage/input_tokens")
                    .and_then(Value::as_u64)
                {
                    self.input_tokens = usage;
                }
                Ok(None)
            }
            Some("content_block_start") => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(block) = event.get("content_block") {
                    let entry = match block.get("type").and_then(Value::as_str) {
                        Some("text") => AccumBlock::Text {
                            text: String::new(),
                        },
                        Some("tool_use") => AccumBlock::ToolUse {
                            id: block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            name: block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            args: String::new(),
                        },
                        _ => return Ok(None),
                    };
                    self.blocks.insert(index, entry);
                }
                Ok(None)
            }
            Some("content_block_delta") => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
                let delta = &event["delta"];
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if let Some(AccumBlock::Text { text: buffer }) = self.blocks.get_mut(&index)
                        {
                            buffer.push_str(&text);
                        }
                        Ok(Some(ModelStreamItem::Delta(ModelDelta::Text { text })))
                    }
                    Some("input_json_delta") => {
                        let partial = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if let Some(AccumBlock::ToolUse { args, .. }) = self.blocks.get_mut(&index)
                        {
                            args.push_str(partial);
                        }
                        Ok(None)
                    }
                    _ => Ok(None),
                }
            }
            Some("message_delta") => {
                if let Some(usage) = event
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64)
                {
                    self.output_tokens = usage;
                }
                // The stop_reason arrives here, ahead of message_stop.
                if event
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .is_some_and(|reason| !reason.is_empty())
                {
                    self.finish_item()
                } else {
                    Ok(None)
                }
            }
            Some("message_stop") => self.finish_item(),
            Some("error") => Ok(Some(ModelStreamItem::Failed(ModelError::Api {
                message: event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("anthropic stream error")
                    .to_string(),
            }))),
            _ => Ok(None), // ping and unknown event types
        }
    }

    /// Builds the terminal item from the accumulated blocks, or `None` when
    /// nothing accumulated.
    pub(crate) fn finish_item(&mut self) -> Result<Option<ModelStreamItem>, ModelError> {
        let mut blocks = Vec::new();
        for index in self.blocks.keys().copied().collect::<Vec<_>>() {
            let entry = self.blocks.remove(&index).expect("key collected above");
            match entry {
                AccumBlock::Unused => {}
                AccumBlock::Text { text } => {
                    if !text.is_empty() {
                        blocks.push(ContentBlock::Text { text });
                    }
                }
                AccumBlock::ToolUse { id, name, args } => {
                    let arguments: Value = serde_json::from_str(&args).map_err(|error| {
                        ModelError::InvalidRequest {
                            message: format!("malformed tool use input: {error}"),
                        }
                    })?;
                    blocks.push(ContentBlock::ToolCall(synonz::message::ToolCall::new(
                        id, name, arguments,
                    )));
                }
            }
        }
        if blocks.is_empty() {
            return Ok(None);
        }
        Ok(Some(ModelStreamItem::Finish {
            message: Message::new(Role::Assistant, blocks),
            usage: TokenUsage::new(self.input_tokens, self.output_tokens),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_is_top_level_parameter() {
        let request = synonz::ModelRequest::new(
            vec![
                Message::system("weather assistant"),
                Message::user("weather?"),
            ],
            vec![],
            synonz::ModelParams::default(),
        );
        let body = request_body("claude-sonnet-4-5", &request).unwrap();
        assert_eq!(body["system"], "weather assistant");
        // The system prompt is NOT a message.
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["type"], "text");
    }

    #[test]
    fn tool_use_carries_input_as_object() {
        let message = Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolCall(synonz::message::ToolCall::new(
                "toolu_1",
                "weather",
                json!({"city": "beijing"}),
            ))],
        );
        let wire = assistant_message(&message).unwrap();
        assert_eq!(wire["content"][0]["type"], "tool_use");
        assert_eq!(wire["content"][0]["id"], "toolu_1");
        assert_eq!(
            wire["content"][0]["input"],
            json!({"city": "beijing"}),
            "input must be a JSON object on the wire"
        );
    }

    #[test]
    fn tool_result_merges_into_user_message_with_is_error() {
        let message = synonz::Message::tool_result(
            "toolu_1",
            ToolResult::Err {
                message: "city not found".into(),
            },
        );
        let wire = tool_result_messages(&message).unwrap();
        assert_eq!(wire[0]["role"], "user");
        assert_eq!(wire[0]["content"][0]["type"], "tool_result");
        assert_eq!(wire[0]["content"][0]["is_error"], true);
        assert_eq!(wire[0]["content"][0]["content"], "city not found");
    }

    #[test]
    fn max_tokens_has_a_documented_default() {
        let request =
            synonz::ModelRequest::new(vec![Message::user("hi")], vec![], Default::default());
        let body = request_body("claude-sonnet-4-5", &request).unwrap();
        assert_eq!(body["max_tokens"], crate::DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn accumulator_merges_streamed_events() {
        let mut accumulator = ResponseAccumulator::default();
        let events: Vec<Value> = vec![
            json!({"type":"message_start","message":{"usage":{"input_tokens":7}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"beijing "}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"is sunny"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"weather","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"bei"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"jing\"}"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}),
            json!({"type":"message_stop"}),
        ];
        let mut deltas = Vec::new();
        let mut finish = None;
        for event in &events {
            if let Some(item) = accumulator.apply_event(event).unwrap() {
                match item {
                    ModelStreamItem::Delta(_) => deltas.push(item),
                    item @ ModelStreamItem::Finish { .. } => finish = Some(item),
                    other => panic!("unexpected stream item: {other:?}"),
                }
            }
        }
        assert_eq!(deltas.len(), 2);
        let Some(ModelStreamItem::Finish { message, usage }) = finish else {
            panic!("expected finish item");
        };
        assert_eq!(usage, TokenUsage::new(7, 3));
        assert!(message.blocks.iter().any(|block| matches!(
            block,
            ContentBlock::ToolCall(call)
                if call.call_id == synonz::CallId::new("toolu_1")
                    && call.name == "weather"
                    && call.arguments == json!({"city": "beijing"})
        )));
    }
}
