//! End-to-end agent behavior tests driven by `MockModel`.
//!
//! These tests verify the loop's externally meaningful behavior: explicit
//! lifecycle, complete event narrative, soft tool failures, cancellation
//! semantics, and the round budget.
//!
//! Requires the `test-util` feature (the tests run against `MockModel`).
#![cfg(feature = "test-util")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use synonz::{
    Agent, AgentError, CallId, CancelReason, ContentBlock, Conversation, ExecutionEvent, MockModel,
    Model, ModelError, ModelRequest, ModelStream, ModelStreamItem, Role, Subject, SubjectType,
    SynonzRuntime, Tool, ToolCall, ToolContent, ToolError, ToolResult,
};

// ────────────────────────── helpers ──────────────────────────

/// A runtime + subject: every execution belongs to a conversation on a
/// runtime (every execution belongs to a conversation on a runtime).
fn fixture() -> (SynonzRuntime, Subject) {
    let runtime = SynonzRuntime::builder().build();
    let subject = Subject::of(SubjectType::User, "u-test");
    (runtime, subject)
}

/// A fresh conversation (created through the runtime: the lifecycle
/// entry persists the initial state and notifies the bus).
fn fresh_conv() -> Conversation {
    let runtime = SynonzRuntime::builder().build();
    Conversation::new(&runtime, &Subject::of(SubjectType::User, "u-test"))
}

/// A tool with a scripted outcome and optional delay.
struct StubTool {
    name: &'static str,
    result: ToolResult,
    delay: Duration,
}

impl StubTool {
    fn ok(name: &'static str, text: &'static str) -> Self {
        Self {
            name,
            result: ToolResult::Ok {
                content: ToolContent::Text { text: text.into() },
            },
            delay: Duration::ZERO,
        }
    }

    fn slow(name: &'static str, delay: Duration) -> Self {
        Self {
            name,
            result: ToolResult::Ok {
                content: ToolContent::Text {
                    text: "slow result".into(),
                },
            },
            delay,
        }
    }
}

impl Tool for StubTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "stub tool for tests"
    }
    fn parameters_schema(&self) -> &serde_json::Value {
        static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        SCHEMA.get_or_init(|| json!({"type": "object"}))
    }
    fn execute<'a>(
        &'a self,
        _args: serde_json::Value,
        _ctx: synonz::ToolContext,
    ) -> futures::future::BoxFuture<'a, Result<ToolResult, ToolError>> {
        Box::pin(async move {
            if self.delay > Duration::ZERO {
                tokio::time::sleep(self.delay).await;
            }
            Ok(self.result.clone())
        })
    }
}

/// A tool whose machinery itself fails (`Err(ToolError)` path).
struct BrokenTool {
    name: &'static str,
}

impl Tool for BrokenTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "always fails at the machinery level"
    }
    fn parameters_schema(&self) -> &serde_json::Value {
        static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        SCHEMA.get_or_init(|| json!({"type": "object"}))
    }
    fn execute<'a>(
        &'a self,
        _args: serde_json::Value,
        _ctx: synonz::ToolContext,
    ) -> futures::future::BoxFuture<'a, Result<ToolResult, ToolError>> {
        Box::pin(async move {
            Err(ToolError::Execution {
                message: "machinery broke".into(),
            })
        })
    }
}

fn finish_text(text: &str) -> ModelStreamItem {
    ModelStreamItem::Finish {
        message: synonz::Message::assistant_text(text),
        usage: synonz::TokenUsage::new(10, 5),
    }
}

fn finish_with_call(call_id: &str, tool: &str, city: &str) -> ModelStreamItem {
    ModelStreamItem::Finish {
        message: synonz::Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolCall(ToolCall::new(
                call_id,
                tool,
                json!({"city": city}),
            ))],
        ),
        usage: synonz::TokenUsage::new(10, 5),
    }
}

async fn collect_events(stream: &mut synonz::Execution<'_>) -> Vec<ExecutionEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

fn assert_terminal_invariant(events: &[ExecutionEvent]) {
    let terminals = events.iter().filter(|event| {
        matches!(
            event,
            ExecutionEvent::Completed(_) | ExecutionEvent::Failed(_) | ExecutionEvent::Cancelled(_)
        )
    });
    assert_eq!(
        terminals.count(),
        1,
        "exactly one terminal event expected: {events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(
                ExecutionEvent::Completed(_)
                    | ExecutionEvent::Failed(_)
                    | ExecutionEvent::Cancelled(_)
            )
        ),
        "last event must be terminal: {events:?}"
    );
}

// ────────────────────────── tests ──────────────────────────

#[tokio::test]
async fn single_round_completes() {
    let (runtime, subject) = fixture();
    let mut conv = Conversation::new(&runtime, &subject);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(MockModel::finishing_with_text("beijing is sunny, 28C."))
        .system_prompt("weather assistant")
        .build()
        .unwrap();

    let mut stream = agent.run(conv.turn_input("weather?"));
    let events = collect_events(&mut stream).await;

    assert_terminal_invariant(&events);
    // Input-side payloads (Started / Requested / Responded) stay on the
    // observation bypass; the single-round narrative is just the terminal.
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], ExecutionEvent::Completed(_)));
    // Truth archive: the completed turn is in the history with its output.
    assert_eq!(conv.len(), 1);
    assert!(conv.turns()[0].output().is_some());
}

#[tokio::test]
async fn run_returns_final_output() {
    let (runtime, subject) = fixture();
    let mut conv = Conversation::new(&runtime, &subject);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(MockModel::finishing_with_text("sunny, 28C."))
        .build()
        .unwrap();

    let output = agent.run(conv.turn_input("weather?")).await.unwrap();
    assert_eq!(output.text(), Some("sunny, 28C."));
    assert_eq!(output.usage.input_tokens, 1);
}

/// Regression: the completed turn archives its final assistant message, so
/// the next request's context replays the previous answer. A dropped answer
/// makes the model answer the earlier question again.
#[tokio::test]
async fn completed_turns_archive_the_final_answer() {
    let (runtime, subject) = fixture();
    let mut conv = Conversation::new(&runtime, &subject);
    let model = MockModel::new(vec![
        vec![finish_text("periodic answer")],
        vec![finish_text("new year answer")],
    ]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .build()
        .unwrap();

    let first = agent.run(conv.turn_input("periodic table?")).await.unwrap();
    assert_eq!(first.text(), Some("periodic answer"));

    // The archived turn carries the user message AND the assistant answer.
    let turns = conv.turns();
    assert!(turns[0].messages.iter().any(|message| {
        message.role == Role::Assistant
            && message.blocks.iter().any(
                |block| matches!(block, ContentBlock::Text { text } if text == "periodic answer"),
            )
    }));

    let second = agent.run(conv.turn_input("new year?")).await.unwrap();
    assert_eq!(second.text(), Some("new year answer"));

    // The second request replays the first answer into the model context.
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].messages.iter().any(|message| {
            message.role == Role::Assistant
                && message.blocks.iter().any(
                    |block| matches!(block, ContentBlock::Text { text } if text == "periodic answer"),
                )
        }),
        "the previous answer must be part of the next request"
    );
}

#[tokio::test]
async fn tool_loop_feeds_results_back() {
    let (runtime, subject) = fixture();
    let mut conv = Conversation::new(&runtime, &subject);
    let model = MockModel::new(vec![
        vec![finish_with_call("x1", "weather", "beijing")],
        vec![finish_text("beijing is sunny, 28C.")],
    ]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .tool(StubTool::ok("weather", "sunny, 28C"))
        .build()
        .unwrap();

    let mut stream = agent.run(conv.turn_input("weather?"));
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_terminal_invariant(&events);
    // Two reasoning rounds happened.
    assert_eq!(stream.rounds(), 2);
    // Tool activity is visible.
    assert!(events.iter().any(|e| matches!(
        e,
        ExecutionEvent::ToolRequested(call) if call.call_id == CallId::new("x1")
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        ExecutionEvent::ToolCompleted { result, .. }
            if matches!(result, ToolResult::Ok { .. })
    )));
    // The second request contained the tool result message.
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    let second = &requests[1];
    assert!(
        second.messages.iter().any(|m| m.role == Role::Tool),
        "tool result message must be fed back to the model"
    );
    // And the run completed.
    assert!(matches!(events.last(), Some(ExecutionEvent::Completed(_))));
}

#[tokio::test]
async fn parallel_tools_pair_by_call_id_and_keep_conversation_order() {
    let (runtime, subject) = fixture();
    let mut conv = Conversation::new(&runtime, &subject);
    let model = MockModel::new(vec![
        vec![ModelStreamItem::Finish {
            message: synonz::Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::ToolCall(ToolCall::new("a", "t1", json!({}))),
                    ContentBlock::ToolCall(ToolCall::new("b", "t2", json!({}))),
                ],
            ),
            usage: synonz::TokenUsage::new(10, 5),
        }],
        vec![finish_text("both done")],
    ]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .tool(StubTool::slow("t1", Duration::from_millis(80)))
        .tool(StubTool::slow("t2", Duration::from_millis(5)))
        .build()
        .unwrap();

    let mut stream = agent.run(conv.turn_input("go"));
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_terminal_invariant(&events);

    // Completion-order events: the fast tool (b) completes first.
    let completions: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            ExecutionEvent::ToolCompleted { call_id, .. } => Some(call_id.as_str().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(completions, vec!["b", "a"]);

    // Deterministic conversation order: call order (a then b).
    let second_round = &model.requests()[1];
    let tool_messages: Vec<&synonz::Message> = second_round
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .collect();
    assert_eq!(tool_messages.len(), 2);
    assert!(
        matches!(&tool_messages[0].blocks[0], ContentBlock::ToolResult { call_id, .. } if call_id.as_str() == "a")
    );
    assert!(
        matches!(&tool_messages[1].blocks[0], ContentBlock::ToolResult { call_id, .. } if call_id.as_str() == "b")
    );
}

#[tokio::test]
async fn soft_failure_is_fed_back_not_fatal() {
    let (runtime, subject) = fixture();
    let mut conv = Conversation::new(&runtime, &subject);
    let model = MockModel::new(vec![
        vec![finish_with_call("x1", "broken", "{}")],
        vec![finish_text("recovered")],
    ]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .tool(BrokenTool { name: "broken" })
        .build()
        .unwrap();

    let mut stream = agent.run(conv.turn_input("go"));
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_terminal_invariant(&events);
    // The run did NOT fail: two rounds happened and the run completed.
    assert!(matches!(events.last(), Some(ExecutionEvent::Completed(_))));
    // The machinery error was converted to a soft failure for the model.
    assert!(events.iter().any(|e| matches!(
        e,
        ExecutionEvent::ToolCompleted { result, .. }
            if matches!(result, ToolResult::Err { message } if message.contains("machinery broke"))
    )));
    // The model saw the failure text.
    let second = &model.requests()[1];
    assert!(second.messages.iter().any(|m| {
        m.role == Role::Tool
            && m.blocks.iter().any(|b| {
                matches!(
                    b,
                    ContentBlock::ToolResult {
                        result: ToolResult::Err { .. },
                        ..
                    }
                )
            })
    }));
}

#[tokio::test]
async fn unknown_tool_is_soft_failure() {
    let (runtime, subject) = fixture();
    let mut conv = Conversation::new(&runtime, &subject);
    let model = MockModel::new(vec![
        vec![finish_with_call("x1", "nonexistent", "{}")],
        vec![finish_text("ok, skipping that")],
    ]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .build()
        .unwrap();

    let mut stream = agent.run(conv.turn_input("go"));
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    assert_terminal_invariant(&events);
    assert!(events.iter().any(|e| matches!(
        e,
        ExecutionEvent::ToolCompleted { result, .. }
            if matches!(result, ToolResult::Err { message } if message.contains("unknown tool"))
    )));
    assert!(matches!(events.last(), Some(ExecutionEvent::Completed(_))));
}

#[tokio::test]
async fn max_rounds_exceeded_fails_explicitly() {
    let (runtime, subject) = fixture();
    let mut conv = Conversation::new(&runtime, &subject);
    // The model always wants another tool call; the budget must stop it.
    let model = MockModel::new(vec![
        vec![finish_with_call("x1", "weather", "beijing")],
        vec![finish_with_call("x2", "weather", "shanghai")],
    ]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .tool(StubTool::ok("weather", "sunny"))
        .max_rounds(1)
        .build()
        .unwrap();

    let mut stream = agent.run(conv.turn_input("weather everywhere"));
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_terminal_invariant(&events);
    assert!(matches!(
        events.last(),
        Some(ExecutionEvent::Failed(AgentError::MaxRoundsExceeded))
    ));
    assert_eq!(stream.rounds(), 1);
    // Truth archive: the failed turn is in the history, marked.
    let turns = conv.turns();
    assert_eq!(turns.len(), 1);
    assert!(matches!(
        turns[0].outcome,
        synonz::TurnOutcome::Failed(AgentError::MaxRoundsExceeded)
    ));
    // The memory layers were NOT fed from the failed turn.
    let memory = runtime.memory();
    assert_eq!(
        memory
            .l1_len_for_tests(&subject, conv.id())
            .expect("l1 len"),
        0
    );
    let retried = agent.run(conv.turn_input("weather everywhere")).await;
    assert!(matches!(retried, Err(AgentError::MaxRoundsExceeded)));
}

#[tokio::test]
async fn cancel_by_external_signal() {
    let (runtime, subject) = fixture();
    let mut conv = Conversation::new(&runtime, &subject);
    let cancel_token = CancellationToken::new();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(MockModel::hanging())
        .build()
        .unwrap();

    let mut stream = agent.run_with(conv.turn_input("go"), cancel_token.clone());
    // Input-side payloads stay on the observation bypass: on the narrative
    // face the first surfaced event is already the cancellation terminal.
    cancel_token.cancel();
    let mut cancelled = None;
    while let Some(event) = stream.next().await {
        if matches!(
            event,
            ExecutionEvent::Cancelled(CancelReason::UserRequested)
        ) {
            cancelled = Some(event);
            break;
        }
    }
    assert!(matches!(
        cancelled,
        Some(ExecutionEvent::Cancelled(CancelReason::UserRequested))
    ));
    assert!(
        stream.next().await.is_none(),
        "stream closes after terminal"
    );
    // Truth archive: the cancelled turn is in the history, marked.
    let turns = conv.turns();
    assert_eq!(turns.len(), 1);
    assert!(matches!(
        turns[0].outcome,
        synonz::TurnOutcome::Cancelled(CancelReason::UserRequested)
    ));
}

#[tokio::test]
async fn cancel_by_timeout() {
    let (runtime, subject) = fixture();
    let mut conv = Conversation::new(&runtime, &subject);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(MockModel::hanging())
        .build()
        .unwrap();

    let mut stream = agent
        .run(conv.turn_input("go"))
        .with_timeout(Duration::from_millis(50));
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_terminal_invariant(&events);
    assert!(matches!(
        events.last(),
        Some(ExecutionEvent::Cancelled(CancelReason::Timeout))
    ));
    // Truth archive: the timed-out turn is in the history, marked.
    let turns = conv.turns();
    assert_eq!(turns.len(), 1);
    assert!(matches!(
        turns[0].outcome,
        synonz::TurnOutcome::Cancelled(CancelReason::Timeout)
    ));
}

#[tokio::test]
async fn cancel_by_drop_reaches_inflight_model_stream() {
    // A model whose stream holds a sentinel after yielding one delta: when
    // the consumer drops the run stream, the loop must be torn down and the
    // sentinel dropped with it (cooperative interruption all the way).
    struct SentinelStream {
        sent: bool,
        dropped: Arc<AtomicBool>,
    }
    impl futures::Stream for SentinelStream {
        type Item = ModelStreamItem;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<ModelStreamItem>> {
            if self.sent {
                std::task::Poll::Pending
            } else {
                self.sent = true;
                std::task::Poll::Ready(Some(ModelStreamItem::Delta(synonz::ModelDelta::Text {
                    text: "tick".into(),
                })))
            }
        }
    }
    impl Drop for SentinelStream {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    struct HangingModel {
        dropped: Arc<AtomicBool>,
    }
    impl Model for HangingModel {
        fn stream(
            &self,
            _request: ModelRequest,
        ) -> futures::future::BoxFuture<'_, Result<ModelStream, ModelError>> {
            let dropped = Arc::clone(&self.dropped);
            Box::pin(async move {
                Ok(Box::pin(SentinelStream {
                    sent: false,
                    dropped,
                }) as ModelStream)
            })
        }
    }

    let (runtime, subject) = fixture();
    let mut conv = Conversation::new(&runtime, &subject);
    let dropped = Arc::new(AtomicBool::new(false));
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(HangingModel {
            dropped: Arc::clone(&dropped),
        })
        .build()
        .unwrap();

    let mut stream = agent.run(conv.turn_input("go"));
    // Input-side events are skipped on the narrative face: the first
    // surfaced item is the model's text delta.
    assert!(matches!(
        stream.next().await,
        Some(ExecutionEvent::Delta(synonz::ModelDelta::Text { .. }))
    ));

    drop(stream); // the drop-cancel entry
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        dropped.load(Ordering::Acquire),
        "in-flight model stream must be dropped when the run stream is dropped"
    );
}

#[tokio::test]
async fn model_failure_fails_the_run() {
    struct FailingModel;
    impl Model for FailingModel {
        fn stream(
            &self,
            _request: ModelRequest,
        ) -> futures::future::BoxFuture<'_, Result<ModelStream, ModelError>> {
            Box::pin(async move {
                Err(ModelError::Transport {
                    message: "connection reset".into(),
                })
            })
        }
    }
    let (runtime, _subject) = fixture();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(FailingModel)
        .build()
        .unwrap();

    let mut conv = fresh_conv();
    let mut stream = agent.run(conv.turn_input("go"));
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_terminal_invariant(&events);
    assert!(matches!(
        events.last(),
        Some(ExecutionEvent::Failed(AgentError::Model(
            ModelError::Transport { .. }
        )))
    ));
}

#[tokio::test]
async fn event_narrative_is_replayable() {
    // A full run's narrative survives a JSON round-trip (record/replay).
    let (runtime, _subject) = fixture();
    let mut conv = fresh_conv();
    let model = MockModel::new(vec![
        vec![finish_with_call("x1", "weather", "beijing")],
        vec![finish_text("sunny")],
    ]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .tool(StubTool::ok("weather", "sunny, 28C"))
        .build()
        .unwrap();

    let mut stream = agent.run(conv.turn_input("weather?"));
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    let encoded = serde_json::to_string(&events).unwrap();
    let decoded: Vec<ExecutionEvent> = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, events);
}

#[tokio::test]
async fn build_without_model_is_invalid_configuration() {
    let error = match Agent::builder().build() {
        Err(error) => error,
        Ok(_) => panic!("build without a model must fail"),
    };
    assert!(matches!(error, AgentError::InvalidConfiguration { .. }));
}

#[tokio::test]
async fn build_without_runtime_is_invalid_configuration() {
    let error = match Agent::builder()
        .model(MockModel::finishing_with_text("hi"))
        .build()
    {
        Err(error) => error,
        Ok(_) => panic!("build without a runtime must fail"),
    };
    assert!(matches!(error, AgentError::InvalidConfiguration { .. }));
}

#[tokio::test]
async fn concurrent_runs_of_one_agent_are_independent() {
    let (runtime, _subject) = fixture();
    let mut conv_a = fresh_conv();
    let mut conv_b = fresh_conv();
    let model = MockModel::new(vec![
        vec![finish_text("answer-1")],
        vec![finish_text("answer-2")],
    ]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .build()
        .unwrap();

    let (a, b) = tokio::join!(
        agent.run(conv_a.turn_input("q1")),
        agent.run(conv_b.turn_input("q2"))
    );
    // Each run gets its own script; which run answers first is scheduling.
    let text_a = a.unwrap().text().unwrap().to_string();
    let text_b = b.unwrap().text().unwrap().to_string();
    let mut answers = vec![text_a, text_b];
    answers.sort();
    assert_eq!(answers, vec!["answer-1", "answer-2"]);
    assert_eq!(model.calls(), 2);
}
