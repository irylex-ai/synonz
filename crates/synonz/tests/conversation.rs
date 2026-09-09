//! S2a acceptance: the conversation entity and turn lifecycle (0.2.0 form).
//!
//! Verifies multi-turn memory, the truth archive (all outcomes enter,
//! marked with their outcome), multi-agent continuation of one
//! conversation, and the export boundary.

#![cfg(feature = "test-util")]

use std::time::Duration;

use serde_json::json;
use synonz::{
    Agent, ContentBlock, Conversation, MockModel, ModelStreamItem, Role, Subject, SubjectType,
    SynonzRuntime, ToolCall, ToolContent, ToolResult, TurnOutcome,
};

/// Test fixture: a fresh runtime and a subject.
fn env() -> (SynonzRuntime, Subject) {
    (
        SynonzRuntime::builder().build(),
        Subject::of(SubjectType::User, "test-user"),
    )
}

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

fn weather_agent(runtime: &SynonzRuntime, scripts: usize) -> Agent {
    Agent::builder()
        .runtime(runtime)
        .model(weather_model(scripts))
        .tool(WeatherTool)
        .build()
        .unwrap()
}

#[tokio::test]
async fn multi_turn_conversation_remembers_history() {
    let (runtime, subject) = env();
    let agent = weather_agent(&runtime, 2);
    let mut conv = Conversation::new(&runtime, &subject);

    // Turn 1 (two model rounds inside one run): tool call + answer.
    let output = agent
        .run(conv.turn_input("weather?"))
        .await
        .expect("turn 1");
    assert_eq!(output.text(), Some("sunny"));
    assert_eq!(conv.len(), 1, "completed turn is recorded");

    // Turn 2: the conversation history (turn 1's messages) is replayed.
    let output = agent.run(conv.turn_input("again?")).await.expect("turn 2");
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
    assert_eq!(first.output().map(|o| o.text()), Some(Some("sunny")));
}

#[tokio::test]
async fn conversation_flat_history_replays_into_the_model() {
    let (runtime, subject) = env();
    let agent = weather_agent(&runtime, 2);
    let mut conv = Conversation::new(&runtime, &subject);

    let _ = agent.run(conv.turn_input("one")).await.unwrap();
    let _ = agent.run(conv.turn_input("two")).await.unwrap();

    // The flattened view is the full canonical conversation: every turn's
    // messages in order, including tool round-trips.
    let flat = conv.messages();
    assert_eq!(flat[0].role, Role::User);
    assert_eq!(flat[0].blocks[0], ContentBlock::Text { text: "one".into() });
    assert!(flat.iter().any(|m| m.role == Role::Assistant));
    assert!(flat.iter().any(|m| m.role == Role::Tool));
}

#[tokio::test]
async fn cancelled_turns_enter_the_history_marked() {
    let (runtime, subject) = env();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(MockModel::hanging())
        .build()
        .unwrap();
    let mut conv = Conversation::new(&runtime, &subject);

    let execution = agent.run(conv.turn_input("starts then cancels"));
    drop(execution); // cancel via drop
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The truth archive keeps the full audit trail — the
    // cancelled turn is recorded, marked with its outcome.
    let turns = conv.turns();
    assert_eq!(turns.len(), 1, "the cancelled turn enters the history");
    assert!(matches!(
        turns[0].outcome,
        TurnOutcome::Cancelled(synonz::CancelReason::UserRequested)
    ));
}

#[tokio::test]
async fn failed_turns_enter_the_history_marked() {
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
    let (runtime, subject) = env();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(FailingModel)
        .build()
        .unwrap();
    let mut conv = Conversation::new(&runtime, &subject);

    let result = agent.run(conv.turn_input("fails")).await;
    assert!(matches!(
        result,
        Err(synonz::AgentError::Model(
            synonz::ModelError::Transport { .. }
        ))
    ));
    let turns = conv.turns();
    assert_eq!(turns.len(), 1, "the failed turn enters the history");
    assert!(matches!(
        turns[0].outcome,
        TurnOutcome::Failed(synonz::AgentError::Model(_))
    ));
    // The memory layers are not fed from failed turns (only success turns
    // write L1).
    assert_eq!(
        runtime
            .memory()
            .l1_len(&subject, conv.id())
            .expect("l1 len"),
        0
    );
}

#[tokio::test]
async fn multiple_agents_continue_one_conversation() {
    // A research-flavored agent starts; a writer-flavored agent continues
    // the same conversation.
    let (runtime, subject) = env();
    let researcher = weather_agent(&runtime, 1);
    let writer = Agent::builder()
        .runtime(&runtime)
        .model(weather_model(1))
        .build()
        .unwrap();

    let mut conv = Conversation::new(&runtime, &subject);
    let first = researcher
        .run(conv.turn_input("research the weather"))
        .await
        .unwrap();
    assert_eq!(first.text(), Some("sunny"));

    let second = writer.run(conv.turn_input("now summarize")).await.unwrap();
    assert_eq!(second.text(), Some("sunny"));
    assert_eq!(conv.len(), 2, "both agents' turns live in one conversation");
}

#[test]
fn export_round_trips_the_truth_record() {
    let runtime = SynonzRuntime::builder().build();
    let subject = Subject::of(SubjectType::User, "test-user");
    let conv = Conversation::with_id(&runtime, &subject, "exported");
    let turns_before = conv.export().expect("export");

    // The export holds the truth record; restoration goes through a store
    // that holds the state (`of`). The memory layers do not travel with
    // the export — that is the Memory facade's own transaction.
    let state: synonz::ConversationState = serde_json::from_slice(&turns_before).unwrap();
    assert_eq!(state.id, "exported");
    assert!(state.turns.is_empty());
}

// ── M19: the conversation lifecycle (entries, idempotent end, sweep) ──

/// An observer recording conversation facts (Created / Ended with their
/// reasons).
#[derive(Default, Clone)]
struct LifecycleRecorder {
    facts: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl synonz::Observer for LifecycleRecorder {
    fn on_event(&self, _ctx: &synonz::ObserverContext, event: &synonz::SynonzEvent) {
        if let synonz::SynonzEvent::Conversation(fact) = event {
            let line = match fact {
                synonz::ConversationEvent::Created { .. } => "created".to_string(),
                synonz::ConversationEvent::Ended { reason, .. } => {
                    format!("ended:{reason:?}")
                }
                _ => "conversation-other".to_string(),
            };
            self.facts.lock().unwrap().push(line);
        }
    }
}

#[tokio::test]
async fn creation_persists_the_initial_state_and_notifies() {
    let recorder = LifecycleRecorder::default();
    let runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
    let subject = Subject::of(SubjectType::User, "lifecycle");

    // The three acts: construct + persist + notify.
    let conv = Conversation::with_id(&runtime, &subject, "born");
    let restored = Conversation::of(&runtime, &subject, "born").expect("the state persisted");
    assert_eq!(restored.id(), "born");
    assert!(!conv.is_ended());

    tokio::time::sleep(Duration::from_millis(50)).await;
    let facts = recorder.facts.lock().unwrap();
    assert_eq!(
        facts.first(),
        Some(&"created".to_string()),
        "the birth fact rides the bus"
    );
}

#[tokio::test]
async fn end_is_idempotent_and_notifies_once() {
    let recorder = LifecycleRecorder::default();
    let runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
    let subject = Subject::of(SubjectType::User, "lifecycle");
    let conv = Conversation::with_id(&runtime, &subject, "close-me");

    conv.end(&runtime).await;
    assert!(conv.is_ended());
    conv.end(&runtime).await; // the second end is a structural no-op

    tokio::time::sleep(Duration::from_millis(50)).await;
    let facts = recorder.facts.lock().unwrap();
    let ends: Vec<&String> = facts.iter().filter(|f| f.starts_with("ended")).collect();
    assert_eq!(ends.len(), 1, "ended notifies exactly once");
    assert_eq!(
        ends[0],
        &format!("ended:{:?}", synonz::ConversationEndReason::Explicit)
    );
}

#[tokio::test]
async fn sweep_ends_idle_conversations_and_skips_ended_ones() {
    let recorder = LifecycleRecorder::default();
    let runtime = SynonzRuntime::builder()
        .observer(recorder.clone())
        .conversation_idle_timeout(Duration::from_secs(1))
        .build();
    let subject = Subject::of(SubjectType::User, "lifecycle");

    // Two conversations, backdated past the idle threshold; one already
    // ended.
    let _idle = Conversation::with_id(&runtime, &subject, "idle-one");
    let already = Conversation::with_id(&runtime, &subject, "already-ended");
    already.end(&runtime).await;

    tokio::time::sleep(Duration::from_millis(1100)).await; // past the threshold
    let swept = runtime.sweep_stale().await;
    assert_eq!(swept, 1, "only the idle, un-ended conversation swept");

    let idle_after = Conversation::of(&runtime, &subject, "idle-one").unwrap();
    assert!(idle_after.is_ended());
    let already_after = Conversation::of(&runtime, &subject, "already-ended").unwrap();
    assert!(already_after.is_ended());

    tokio::time::sleep(Duration::from_millis(50)).await;
    let facts = recorder.facts.lock().unwrap();
    let idle_sweeps: Vec<&String> = facts.iter().filter(|f| f.starts_with("ended:")).collect();
    assert_eq!(
        idle_sweeps.len(),
        2,
        "one explicit end + one swept end: {facts:?}"
    );
    assert!(idle_sweeps.contains(&&format!(
        "ended:{:?}",
        synonz::ConversationEndReason::IdleSwept
    )));
}

#[tokio::test]
async fn of_restores_the_ended_state() {
    let runtime = SynonzRuntime::builder().build();
    let subject = Subject::of(SubjectType::User, "lifecycle");
    let conv = Conversation::with_id(&runtime, &subject, "ended-state");
    assert!(!conv.is_ended());
    conv.end(&runtime).await;
    assert!(conv.is_ended());

    // The restored conversation carries the lifecycle state (the audit
    // and sweep behaviors read it from the store).
    let restored = Conversation::of(&runtime, &subject, "ended-state").unwrap();
    assert!(restored.is_ended());
}
