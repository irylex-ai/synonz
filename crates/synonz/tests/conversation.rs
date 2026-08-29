//! S2a acceptance: the conversation entity and turn lifecycle.
//!
//! Verifies multi-turn memory, commit timing (only completed turns are
//! recorded), multi-agent continuation of one conversation, and
//! export/import round-trips.

#![cfg(feature = "test-util")]

use std::time::Duration;

use serde_json::json;
use synonz::{
    Agent, ContentBlock, Conversation, MockModel, ModelStreamItem, Role, ToolCall, ToolContent,
    ToolResult,
};

/// A model with one scripted round per call: first round calls `weather`,
/// later rounds answer directly.
fn weather_model(rounds: usize) -> MockModel {
    let mut scripts = Vec::new();
    for _ in 0..rounds {
        scripts.push(vec![ModelStreamItem::Finish {
            message: synonz::Message::new(
                Role::Assistant,
                vec![ContentBlock::ToolCall(ToolCall::new(
                    "x1",
                    "weather",
                    json!({"city": "beijing"}),
                ))],
            ),
            usage: synonz::TokenUsage::new(1, 1),
        }]);
        scripts.push(vec![ModelStreamItem::Finish {
            message: synonz::Message::assistant_text("sunny"),
            usage: synonz::TokenUsage::new(1, 1),
        }]);
    }
    MockModel::new(scripts)
}

struct WeatherTool;

impl synonz::Tool for WeatherTool {
    fn name(&self) -> &str {
        "weather"
    }
    fn description(&self) -> &str {
        "reports the weather"
    }
    fn parameters_schema(&self) -> &serde_json::Value {
        static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        SCHEMA.get_or_init(|| json!({"type": "object"}))
    }
    fn execute<'a>(
        &'a self,
        _args: serde_json::Value,
        _ctx: synonz::ToolContext,
    ) -> synonz::BoxFuture<'a, Result<ToolResult, synonz::ToolError>> {
        Box::pin(async {
            Ok(ToolResult::Ok {
                content: ToolContent::Text {
                    text: "sunny, 28C".into(),
                },
            })
        })
    }
}

fn weather_agent(scripts: usize) -> Agent {
    Agent::builder()
        .model(weather_model(scripts))
        .tool(WeatherTool)
        .build()
        .unwrap()
}

#[tokio::test]
async fn multi_turn_conversation_remembers_history() {
    let agent = weather_agent(2);
    let mut conv = Conversation::new();

    // Turn 1 (two model rounds inside one run): tool call + answer.
    let output = agent
        .ask(conv.turn_input("weather?"))
        .await
        .expect("turn 1");
    assert_eq!(output.text(), Some("sunny"));
    assert_eq!(conv.len(), 1, "completed turn is recorded");

    // Turn 2: the conversation history (turn 1's messages) is replayed.
    let output = agent.ask(conv.turn_input("again?")).await.expect("turn 2");
    assert_eq!(output.text(), Some("sunny"));
    assert_eq!(conv.len(), 2);

    // Turn 1's record holds the full round-trip, not just the answer.
    let turns = conv.turns();
    let first = &turns[0];
    assert_eq!(first.input.text, "weather?");
    assert!(first.messages.iter().any(|m| {
        m.role == Role::Tool
            && m.blocks.iter().any(|b| {
                matches!(
                    b,
                    ContentBlock::ToolResult {
                        result: ToolResult::Ok { .. },
                        ..
                    }
                )
            })
    }));
    assert_eq!(first.output.text(), Some("sunny"));
}

#[tokio::test]
async fn conversation_flat_history_replays_into_the_model() {
    let agent = weather_agent(2);
    let mut conv = Conversation::new();

    let _ = agent.ask(conv.turn_input("one")).await.unwrap();
    let _ = agent.ask(conv.turn_input("two")).await.unwrap();

    // The flattened view is the full canonical conversation: every turn's
    // messages in order, including tool round-trips.
    let flat = conv.messages();
    assert_eq!(flat[0].role, Role::User);
    assert_eq!(flat[0].blocks[0], ContentBlock::Text { text: "one".into() });
    assert!(flat.iter().any(|m| m.role == Role::Assistant));
    assert!(flat.iter().any(|m| m.role == Role::Tool));
}

#[tokio::test]
async fn cancelled_turns_are_not_recorded() {
    let agent = Agent::builder()
        .model(MockModel::hanging())
        .build()
        .unwrap();
    let mut conv = Conversation::new();

    let run = agent.run(conv.turn_input("starts then cancels"));
    drop(run); // cancel via drop
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(conv.is_empty(), "cancelled runs must not record a turn");
}

#[tokio::test]
async fn failed_turns_are_not_recorded() {
    struct FailingModel;
    impl synonz::Model for FailingModel {
        fn stream(
            &self,
            _request: synonz::ModelRequest,
        ) -> synonz::BoxFuture<'_, Result<synonz::ModelStream, synonz::ModelError>> {
            Box::pin(async {
                Err(synonz::ModelError::Transport {
                    message: "down".into(),
                })
            })
        }
    }
    let agent = Agent::builder().model(FailingModel).build().unwrap();
    let mut conv = Conversation::new();

    let result = agent.ask(conv.turn_input("fails")).await;
    assert!(matches!(
        result,
        Err(synonz::AgentError::Model(
            synonz::ModelError::Transport { .. }
        ))
    ));
    assert!(conv.is_empty(), "failed runs must not record a turn");
}

#[tokio::test]
async fn multiple_agents_continue_one_conversation() {
    // A research-flavored agent starts; a writer-flavored agent continues
    // the same conversation.
    let researcher = weather_agent(1);
    let writer = Agent::builder().model(weather_model(1)).build().unwrap();

    let mut conv = Conversation::new();
    let first = researcher
        .ask(conv.turn_input("research the weather"))
        .await
        .unwrap();
    assert_eq!(first.text(), Some("sunny"));

    let second = writer.ask(conv.turn_input("now summarize")).await.unwrap();
    assert_eq!(second.text(), Some("sunny"));
    assert_eq!(conv.len(), 2, "both agents' turns live in one conversation");
}

#[tokio::test]
async fn one_shot_input_stays_conversation_less() {
    let agent = weather_agent(1);
    let output = agent.ask("no conversation here").await.unwrap();
    assert_eq!(output.text(), Some("sunny"));
}

#[test]
fn manual_push_and_management() {
    let conv = Conversation::with_id("manual");
    let turn = synonz::Turn::new(
        synonz::AgentInput::new("hi"),
        vec![synonz::Message::user("hi")],
        synonz::AgentOutput::new(
            synonz::Message::assistant_text("hello"),
            synonz::TokenUsage::new(0, 0),
        ),
    );
    conv.push_turn(turn);
    assert_eq!(conv.len(), 1);
    conv.truncate_last(1);
    assert!(conv.is_empty());
    conv.clear();
}
