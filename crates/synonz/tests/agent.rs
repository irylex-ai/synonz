//! End-to-end agent behavior tests driven by `MockModel`.
//!
//! These tests verify the loop's externally meaningful behavior against the
//! ADR commitments: explicit lifecycle, complete event narrative, soft tool
//! failures, cancellation semantics, and the round budget.
//!
//! Requires the `test-util` feature (the tests run against `MockModel`).
#![cfg(feature = "test-util")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use synonz::{
    Agent, AgentError, AgentEvent, CallId, CancelReason, ContentBlock, LifecycleEvent, MockModel,
    Model, ModelError, ModelEvent, ModelRequest, ModelStream, ModelStreamItem, Role, Tool,
    ToolCall, ToolContent, ToolError, ToolResult,
};

// ────────────────────────── helpers ──────────────────────────

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

async fn collect_events(stream: &mut synonz::RunStream) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

fn assert_terminal_invariant(events: &[AgentEvent]) {
    let terminals = events.iter().filter(|event| {
        matches!(
            event,
            AgentEvent::Lifecycle(
                LifecycleEvent::Completed { .. }
                    | LifecycleEvent::Failed { .. }
                    | LifecycleEvent::Cancelled { .. }
            )
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
            Some(AgentEvent::Lifecycle(
                LifecycleEvent::Completed { .. }
                    | LifecycleEvent::Failed { .. }
                    | LifecycleEvent::Cancelled { .. }
            ))
        ),
        "last event must be terminal: {events:?}"
    );
}

// ────────────────────────── tests ──────────────────────────

#[tokio::test]
async fn single_round_completes() {
    let agent = Agent::builder()
        .model(MockModel::finishing_with_text("beijing is sunny, 28C."))
        .system_prompt("weather assistant")
        .build()
        .unwrap();

    let mut stream = agent.run("weather?");
    let events = collect_events(&mut stream).await;

    assert_terminal_invariant(&events);
    assert!(matches!(
        &events[0],
        AgentEvent::Lifecycle(LifecycleEvent::Started { .. })
    ));
    assert_eq!(events.len(), 4); // Started, Requested, Responded, Completed
}

#[tokio::test]
async fn ask_returns_final_output() {
    let agent = Agent::builder()
        .model(MockModel::finishing_with_text("sunny, 28C."))
        .build()
        .unwrap();

    let output = agent.ask("weather?").await.unwrap();
    assert_eq!(output.text(), Some("sunny, 28C."));
    assert_eq!(output.usage.input_tokens, 1);
}

#[tokio::test]
async fn tool_loop_feeds_results_back() {
    let model = MockModel::new(vec![
        vec![finish_with_call("x1", "weather", "beijing")],
        vec![finish_text("beijing is sunny, 28C.")],
    ]);
    let agent = Agent::builder()
        .model(model.clone())
        .tool(StubTool::ok("weather", "sunny, 28C"))
        .build()
        .unwrap();

    let mut stream = agent.run("weather?");
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
        AgentEvent::Tool(synonz::ToolEvent::CallRequested { call })
            if call.call_id == CallId::new("x1")
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Tool(synonz::ToolEvent::CallCompleted { result, .. })
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
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Lifecycle(LifecycleEvent::Completed { .. }))
    ));
}

#[tokio::test]
async fn parallel_tools_pair_by_call_id_and_keep_conversation_order() {
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
        .model(model.clone())
        .tool(StubTool::slow("t1", Duration::from_millis(80)))
        .tool(StubTool::slow("t2", Duration::from_millis(5)))
        .build()
        .unwrap();

    let mut stream = agent.run("go");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_terminal_invariant(&events);

    // Completion-order events: the fast tool (b) completes first.
    let completions: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Tool(synonz::ToolEvent::CallCompleted { call_id, .. }) => {
                Some(call_id.as_str().to_string())
            }
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
    let model = MockModel::new(vec![
        vec![finish_with_call("x1", "broken", "{}")],
        vec![finish_text("recovered")],
    ]);
    let agent = Agent::builder()
        .model(model.clone())
        .tool(BrokenTool { name: "broken" })
        .build()
        .unwrap();

    let mut stream = agent.run("go");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_terminal_invariant(&events);
    // The run did NOT fail: two rounds happened and the run completed.
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Lifecycle(LifecycleEvent::Completed { .. }))
    ));
    // The machinery error was converted to a soft failure for the model.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Tool(synonz::ToolEvent::CallCompleted { result, .. })
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
    let model = MockModel::new(vec![
        vec![finish_with_call("x1", "nonexistent", "{}")],
        vec![finish_text("ok, skipping that")],
    ]);
    let agent = Agent::builder().model(model).build().unwrap();

    let mut stream = agent.run("go");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    assert_terminal_invariant(&events);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Tool(synonz::ToolEvent::CallCompleted { result, .. })
            if matches!(result, ToolResult::Err { message } if message.contains("unknown tool"))
    )));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Lifecycle(LifecycleEvent::Completed { .. }))
    ));
}

#[tokio::test]
async fn max_rounds_exceeded_fails_explicitly() {
    // The model always wants another tool call; the budget must stop it.
    let model = MockModel::new(vec![
        vec![finish_with_call("x1", "weather", "beijing")],
        vec![finish_with_call("x2", "weather", "shanghai")],
    ]);
    let agent = Agent::builder()
        .model(model)
        .tool(StubTool::ok("weather", "sunny"))
        .max_rounds(1)
        .build()
        .unwrap();

    let mut stream = agent.run("weather everywhere");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_terminal_invariant(&events);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Lifecycle(LifecycleEvent::Failed {
            error: AgentError::MaxRoundsExceeded
        }))
    ));
    assert_eq!(stream.rounds(), 1);
    let answer = agent.ask("weather everywhere").await;
    assert!(matches!(answer, Err(AgentError::MaxRoundsExceeded)));
}

#[tokio::test]
async fn cancel_by_external_token() {
    let token = CancellationToken::new();
    let agent = Agent::builder()
        .model(MockModel::hanging())
        .build()
        .unwrap();

    let mut stream = agent.run_with("go", token.clone());
    let started = stream.next().await;
    assert!(matches!(
        started,
        Some(AgentEvent::Lifecycle(LifecycleEvent::Started { .. }))
    ));

    token.cancel();
    // Buffered in-flight events may precede the terminal one.
    let mut cancelled = None;
    while let Some(event) = stream.next().await {
        if matches!(
            event,
            AgentEvent::Lifecycle(LifecycleEvent::Cancelled { .. })
        ) {
            cancelled = Some(event);
            break;
        }
    }
    assert!(matches!(
        cancelled,
        Some(AgentEvent::Lifecycle(LifecycleEvent::Cancelled {
            reason: CancelReason::UserRequested
        }))
    ));
    assert!(
        stream.next().await.is_none(),
        "stream closes after terminal"
    );
}

#[tokio::test]
async fn cancel_by_timeout() {
    let agent = Agent::builder()
        .model(MockModel::hanging())
        .build()
        .unwrap();

    let mut stream = agent.run("go").with_timeout(Duration::from_millis(50));
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_terminal_invariant(&events);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Lifecycle(LifecycleEvent::Cancelled {
            reason: CancelReason::Timeout
        }))
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

    let dropped = Arc::new(AtomicBool::new(false));
    let agent = Agent::builder()
        .model(HangingModel {
            dropped: Arc::clone(&dropped),
        })
        .build()
        .unwrap();

    let mut stream = agent.run("go");
    assert!(stream.next().await.is_some()); // Started
    assert!(matches!(
        stream.next().await,
        Some(AgentEvent::Model(ModelEvent::Requested { .. }))
    ));
    assert!(matches!(
        stream.next().await,
        Some(AgentEvent::Model(ModelEvent::StreamDelta { .. }))
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
    let agent = Agent::builder().model(FailingModel).build().unwrap();

    let mut stream = agent.run("go");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert_terminal_invariant(&events);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Lifecycle(LifecycleEvent::Failed {
            error: AgentError::Model(ModelError::Transport { .. })
        }))
    ));
}

#[tokio::test]
async fn event_narrative_is_replayable() {
    // A full run's narrative survives a JSON round-trip (record/replay).
    let model = MockModel::new(vec![
        vec![finish_with_call("x1", "weather", "beijing")],
        vec![finish_text("sunny")],
    ]);
    let agent = Agent::builder()
        .model(model)
        .tool(StubTool::ok("weather", "sunny, 28C"))
        .build()
        .unwrap();

    let mut stream = agent.run("weather?");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    let encoded = serde_json::to_string(&events).unwrap();
    let decoded: Vec<AgentEvent> = serde_json::from_str(&encoded).unwrap();
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
async fn concurrent_runs_of_one_agent_are_independent() {
    let model = MockModel::new(vec![
        vec![finish_text("answer-1")],
        vec![finish_text("answer-2")],
    ]);
    let agent = Agent::builder().model(model.clone()).build().unwrap();

    let (a, b) = tokio::join!(agent.ask("q1"), agent.ask("q2"));
    // Each run gets its own script; which run answers first is scheduling.
    let text_a = a.unwrap().text().unwrap().to_string();
    let text_b = b.unwrap().text().unwrap().to_string();
    let mut answers = vec![text_a, text_b];
    answers.sort();
    assert_eq!(answers, vec!["answer-1", "answer-2"]);
    assert_eq!(model.calls(), 2);
}
