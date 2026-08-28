//! Serialization contract tests: round-trips and locked tag formats.
//!
//! The serialized shape of events is a wire contract (recording/replay,
//! transport). These tests lock the exact JSON structure so accidental
//! format changes surface as test failures.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use synonz::{
    AgentError, AgentEvent, AgentInput, AgentOutput, CallId, CallPurpose, CancelReason,
    ContentBlock, LifecycleEvent, Message, ModelDelta, ModelError, ModelEvent, Role, TokenUsage,
    ToolCall, ToolContent, ToolEvent, ToolResult,
};

fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let encoded = serde_json::to_string(value).expect("serialize");
    let decoded: T = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(&decoded, value, "round-trip mismatch for: {encoded}");
}

fn sample_messages() -> Vec<Message> {
    vec![
        Message::system("you are a weather assistant"),
        Message::user("weather in beijing?"),
        Message::new(
            Role::Assistant,
            vec![
                ContentBlock::Text {
                    text: "let me check.".into(),
                },
                ContentBlock::ToolCall(ToolCall::new("x1", "weather", json!({"city": "beijing"}))),
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
    ]
}

#[test]
fn events_roundtrip() {
    let events = vec![
        AgentEvent::Lifecycle(LifecycleEvent::Started {
            input: AgentInput::new("weather in beijing?"),
        }),
        AgentEvent::Model(ModelEvent::Requested {
            purpose: CallPurpose::Reasoning,
            messages: sample_messages(),
        }),
        AgentEvent::Model(ModelEvent::StreamDelta {
            delta: ModelDelta::Text {
                text: "beijing is".into(),
            },
        }),
        AgentEvent::Model(ModelEvent::Responded {
            message: Message::assistant_text("beijing is sunny, 28C."),
            usage: TokenUsage::new(120, 8),
        }),
        AgentEvent::Tool(ToolEvent::CallRequested {
            call: ToolCall::new("x1", "weather", json!({"city": "beijing"})),
        }),
        AgentEvent::Tool(ToolEvent::CallCompleted {
            call_id: CallId::new("x1"),
            result: ToolResult::Err {
                message: "service unavailable".into(),
            },
        }),
        AgentEvent::Lifecycle(LifecycleEvent::Cancelled {
            reason: CancelReason::Timeout,
        }),
        AgentEvent::Lifecycle(LifecycleEvent::Failed {
            error: AgentError::Model(ModelError::RateLimited {
                message: "429".into(),
            }),
        }),
        AgentEvent::Lifecycle(LifecycleEvent::Failed {
            error: AgentError::MaxRoundsExceeded,
        }),
        AgentEvent::Lifecycle(LifecycleEvent::Completed {
            response: AgentOutput::new(
                Message::assistant_text("beijing is sunny, 28C."),
                TokenUsage::new(120, 8),
            ),
        }),
    ];
    for event in &events {
        roundtrip(event);
    }
}

#[test]
fn messages_roundtrip() {
    roundtrip(&sample_messages());
}

#[test]
fn started_event_snapshot() {
    let event = AgentEvent::Lifecycle(LifecycleEvent::Started {
        input: AgentInput::new("hi"),
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "lifecycle",
            "event": "started",
            "input": { "text": "hi" },
        })
    );
}

#[test]
fn model_requested_event_snapshot() {
    let event = AgentEvent::Model(ModelEvent::Requested {
        purpose: CallPurpose::Reasoning,
        messages: vec![Message::user("hi")],
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "model",
            "event": "requested",
            "purpose": "reasoning",
            "messages": [
                {
                    "role": "user",
                    "blocks": [
                        { "block": "text", "text": "hi" }
                    ],
                }
            ],
        })
    );
}

#[test]
fn tool_call_requested_event_snapshot() {
    let event = AgentEvent::Tool(ToolEvent::CallRequested {
        call: ToolCall::new("x1", "weather", json!({"city": "beijing"})),
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "tool",
            "event": "call_requested",
            "call": {
                "call_id": "x1",
                "name": "weather",
                "arguments": { "city": "beijing" },
            },
        })
    );
}

#[test]
fn tool_call_completed_event_snapshot() {
    let event = AgentEvent::Tool(ToolEvent::CallCompleted {
        call_id: CallId::new("x1"),
        result: ToolResult::Ok {
            content: ToolContent::Text {
                text: "sunny".into(),
            },
        },
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "tool",
            "event": "call_completed",
            "call_id": "x1",
            "result": {
                "ok": {
                    "content": { "kind": "text", "text": "sunny" }
                }
            },
        })
    );
}

#[test]
fn cancelled_event_snapshot() {
    let event = AgentEvent::Lifecycle(LifecycleEvent::Cancelled {
        reason: CancelReason::UserRequested,
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "lifecycle",
            "event": "cancelled",
            "reason": "user_requested",
        })
    );
}

#[test]
fn assistant_tool_call_block_snapshot() {
    let message = Message::new(
        Role::Assistant,
        vec![
            ContentBlock::Text {
                text: "let me check.".into(),
            },
            ContentBlock::ToolCall(ToolCall::new("x1", "weather", json!({"city": "beijing"}))),
        ],
    );
    assert_eq!(
        serde_json::to_value(&message).unwrap(),
        json!({
            "role": "assistant",
            "blocks": [
                { "block": "text", "text": "let me check." },
                {
                    "block": "tool_call",
                    "call_id": "x1",
                    "name": "weather",
                    "arguments": { "city": "beijing" },
                }
            ],
        })
    );
}

#[test]
fn stream_delta_snapshot() {
    let event = AgentEvent::Model(ModelEvent::StreamDelta {
        delta: ModelDelta::Text {
            text: "frag".into(),
        },
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "model",
            "event": "stream_delta",
            "delta": { "kind": "text", "text": "frag" },
        })
    );
}
