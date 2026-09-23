//! Serialization contract tests: round-trips and locked tag formats.
//!
//! The serialized shape of events is a wire contract (recording/replay,
//! transport). These tests lock the exact JSON structure so accidental
//! format changes surface as test failures.
//!
//! Tag scheme (three levels, three keys — no collisions):
//! - `SynonzEvent`: `"type"` = entity family (turn / conversation / memory)
//! - `TurnEvent`: `"kind"` = concern (lifecycle / model / tool)
//! - variant enums: `"event"` = the kind (started / requested / ...)

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use synonz::{
    AgentError, AgentInput, AgentOutput, CallId, CallPurpose, CancelReason, ContentBlock,
    ConversationEndReason, ConversationEvent, LifecycleEvent, MemoryEvent, MemoryFailedMoment,
    MemoryScope, Message, ModelDelta, ModelError, ModelEvent, Role, SynonzEvent, TokenUsage,
    ToolCall, ToolContent, ToolEvent, ToolResult, TurnEvent,
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
        TurnEvent::Lifecycle(LifecycleEvent::Started {
            input: AgentInput::new("weather in beijing?"),
        }),
        TurnEvent::Model(ModelEvent::Requested {
            purpose: CallPurpose::Reasoning,
            messages: sample_messages(),
            round: Some(1),
        }),
        TurnEvent::Model(ModelEvent::StreamDelta {
            delta: ModelDelta::Text {
                text: "beijing is".into(),
            },
            round: Some(1),
        }),
        TurnEvent::Model(ModelEvent::Responded {
            message: Message::assistant_text("beijing is sunny, 28C."),
            usage: TokenUsage::new(120, 8),
            round: Some(1),
        }),
        TurnEvent::Tool(ToolEvent::CallRequested {
            call: ToolCall::new("x1", "weather", json!({"city": "beijing"})),
            round: Some(1),
        }),
        TurnEvent::Tool(ToolEvent::CallCompleted {
            call_id: CallId::new("x1"),
            result: ToolResult::Err {
                message: "service unavailable".into(),
            },
            round: Some(1),
        }),
        TurnEvent::Lifecycle(LifecycleEvent::Cancelled {
            reason: CancelReason::Timeout,
        }),
        TurnEvent::Lifecycle(LifecycleEvent::Failed {
            error: AgentError::Model(ModelError::RateLimited {
                message: "429".into(),
            }),
        }),
        TurnEvent::Lifecycle(LifecycleEvent::Failed {
            error: AgentError::MaxRoundsExceeded,
        }),
        TurnEvent::Lifecycle(LifecycleEvent::Completed {
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
fn bus_families_roundtrip() {
    let events = vec![
        SynonzEvent::Turn(TurnEvent::Lifecycle(LifecycleEvent::Started {
            input: AgentInput::new("hi"),
        })),
        SynonzEvent::Conversation(ConversationEvent::Created {
            conversation_id: "conv-1".into(),
            subject_id: "user-1".into(),
        }),
        SynonzEvent::Conversation(ConversationEvent::Ended {
            conversation_id: "conv-1".into(),
            subject_id: "user-1".into(),
            reason: ConversationEndReason::IdleSwept,
        }),
        SynonzEvent::Conversation(ConversationEvent::TopicShifted {
            conversation_id: "conv-1".into(),
            from: "weather".into(),
            to: "travel".into(),
        }),
        SynonzEvent::Memory(MemoryEvent::TurnArchived {
            conversation_id: "conv-1".into(),
            subject_id: "user-1".into(),
            topic: "weather".into(),
        }),
        SynonzEvent::Memory(MemoryEvent::Updated {
            subject_id: "user-1".into(),
            scope: MemoryScope::new("project:abc"),
            id: "entry-1".into(),
        }),
        SynonzEvent::Memory(MemoryEvent::Removed {
            subject_id: "user-1".into(),
            scope: MemoryScope::new("project:abc"),
            ids: vec!["entry-1".into(), "entry-2".into()],
        }),
        SynonzEvent::Memory(MemoryEvent::Failed {
            stage: "summarize".into(),
            detail: "model call failed".into(),
            moment: MemoryFailedMoment::Background,
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
    let event = TurnEvent::Lifecycle(LifecycleEvent::Started {
        input: AgentInput::new("hi"),
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "kind": "lifecycle",
            "event": "started",
            "input": { "text": "hi" },
        })
    );
}

#[test]
fn turn_event_nests_inside_the_bus_envelope() {
    let event = SynonzEvent::Turn(TurnEvent::Lifecycle(LifecycleEvent::Started {
        input: AgentInput::new("hi"),
    }));
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "turn",
            "kind": "lifecycle",
            "event": "started",
            "input": { "text": "hi" },
        })
    );
}

#[test]
fn model_requested_event_snapshot() {
    let event = TurnEvent::Model(ModelEvent::Requested {
        purpose: CallPurpose::Reasoning,
        messages: vec![Message::user("hi")],
        round: Some(1),
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "kind": "model",
            "event": "requested",
            "purpose": "reasoning",
            "round": 1,
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
fn auxiliary_model_call_carries_null_round() {
    let event = SynonzEvent::Turn(TurnEvent::Model(ModelEvent::Requested {
        purpose: CallPurpose::ContextManagement,
        messages: vec![Message::user("summarize")],
        round: None,
    }));
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "turn",
            "kind": "model",
            "event": "requested",
            "purpose": "context_management",
            "round": null,
            "messages": [
                {
                    "role": "user",
                    "blocks": [
                        { "block": "text", "text": "summarize" }
                    ],
                }
            ],
        })
    );
}

#[test]
fn tool_call_requested_event_snapshot() {
    let event = TurnEvent::Tool(ToolEvent::CallRequested {
        call: ToolCall::new("x1", "weather", json!({"city": "beijing"})),
        round: Some(1),
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "kind": "tool",
            "event": "call_requested",
            "round": 1,
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
    let event = TurnEvent::Tool(ToolEvent::CallCompleted {
        call_id: CallId::new("x1"),
        result: ToolResult::Ok {
            content: ToolContent::Text {
                text: "sunny".into(),
            },
        },
        round: Some(1),
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "kind": "tool",
            "event": "call_completed",
            "round": 1,
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
    let event = TurnEvent::Lifecycle(LifecycleEvent::Cancelled {
        reason: CancelReason::UserRequested,
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "kind": "lifecycle",
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
    let event = TurnEvent::Model(ModelEvent::StreamDelta {
        delta: ModelDelta::Text {
            text: "frag".into(),
        },
        round: Some(2),
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "kind": "model",
            "event": "stream_delta",
            "round": 2,
            "delta": { "kind": "text", "text": "frag" },
        })
    );
}

#[test]
fn conversation_ended_event_snapshot() {
    let event = SynonzEvent::Conversation(ConversationEvent::Ended {
        conversation_id: "conv-1".into(),
        subject_id: "user-1".into(),
        reason: ConversationEndReason::Explicit,
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "conversation",
            "event": "ended",
            "conversation_id": "conv-1",
            "subject_id": "user-1",
            "reason": "explicit",
        })
    );
}

#[test]
fn memory_failed_event_snapshot() {
    let event = SynonzEvent::Memory(MemoryEvent::Failed {
        stage: "archive".into(),
        detail: "l1 append failed".into(),
        moment: MemoryFailedMoment::AfterTurn,
    });
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "memory",
            "event": "failed",
            "stage": "archive",
            "detail": "l1 append failed",
            "moment": "after_turn",
        })
    );
}

#[test]
fn memory_management_events_carry_scope_snapshots() {
    let updated = SynonzEvent::Memory(MemoryEvent::Updated {
        subject_id: "user-1".into(),
        scope: MemoryScope::new("project:abc"),
        id: "entry-1".into(),
    });
    assert_eq!(
        serde_json::to_value(&updated).unwrap(),
        json!({
            "type": "memory",
            "event": "updated",
            "subject_id": "user-1",
            "scope": "project:abc",
            "id": "entry-1",
        })
    );

    let removed = SynonzEvent::Memory(MemoryEvent::Removed {
        subject_id: "user-1".into(),
        scope: MemoryScope::new("user:u1"),
        ids: vec!["a".into(), "b".into()],
    });
    assert_eq!(
        serde_json::to_value(&removed).unwrap(),
        json!({
            "type": "memory",
            "event": "removed",
            "subject_id": "user-1",
            "scope": "user:u1",
            "ids": ["a", "b"],
        })
    );
}
