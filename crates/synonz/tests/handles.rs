//! M12 acceptance: the single execution face (Execution / ExecutionEvent).
//!
//! Verifies the three-in-one handle: narrative streaming via `next()`,
//! the final result via `.await` (stream self-sufficiency), explicit
//! `cancel()`, timeout chaining, and the interleaving rules.

#![cfg(feature = "test-util")]

use std::time::Duration;

use futures::StreamExt;
use synonz::{
    Agent, CancelReason, Conversation, ExecutionEvent, MockModel, ModelDelta, ModelStreamItem,
    Subject, SubjectType, SynonzRuntime,
};

/// A fresh runtime + conversation: every execution belongs to a
/// conversation (ADR-0015).
fn fixture() -> (SynonzRuntime, Conversation) {
    let runtime = SynonzRuntime::builder().build();
    let conv = Conversation::new(&runtime, &Subject::of(SubjectType::User, "u-test"));
    (runtime, conv)
}

fn streaming_model() -> MockModel {
    MockModel::new(vec![vec![
        ModelStreamItem::Delta(ModelDelta::Text {
            text: "beijing ".into(),
        }),
        ModelStreamItem::Delta(ModelDelta::Text {
            text: "is sunny".into(),
        }),
        ModelStreamItem::Finish {
            message: synonz::Message::assistant_text("beijing is sunny"),
            usage: synonz::TokenUsage::new(3, 2),
        },
    ]])
}

#[tokio::test]
async fn execution_streams_narrative_then_resolves_on_await() {
    let (runtime, mut conv) = fixture();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(streaming_model())
        .build()
        .unwrap();

    let mut execution = agent.run(conv.turn_input("weather?"));

    let mut text = String::new();
    while let Some(event) = execution.next().await {
        if let ExecutionEvent::Delta(ModelDelta::Text { text: fragment }) = event {
            text.push_str(&fragment);
        }
    }
    assert_eq!(text, "beijing is sunny");

    // Awaiting after full iteration still resolves (terminal was stashed).
    let output = execution.await.expect("execution resolves");
    assert_eq!(output.text(), Some("beijing is sunny"));
}

#[tokio::test]
async fn execution_awaits_directly_as_one_shot() {
    let (runtime, mut conv) = fixture();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(streaming_model())
        .build()
        .unwrap();

    let output = agent.run(conv.turn_input("weather?")).await.unwrap();
    assert_eq!(output.text(), Some("beijing is sunny"));
    assert_eq!(output.usage.input_tokens, 3);
}

#[tokio::test]
async fn execution_is_a_stream_and_a_future() {
    let (runtime, mut conv) = fixture();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(streaming_model())
        .build()
        .unwrap();

    // Future face: await directly.
    let output = agent
        .run(conv.turn_input("weather?"))
        .await
        .expect("execution resolves");
    assert_eq!(output.text(), Some("beijing is sunny"));

    // Stream face: iterate the full narrative — the last item is the
    // terminal `Completed` carrying the output — then await still resolves.
    let (runtime2, mut conv2) = fixture();
    let stream_agent = Agent::builder()
        .runtime(&runtime2)
        .model(streaming_model())
        .build()
        .unwrap();
    let mut execution = stream_agent.run(conv2.turn_input("weather?"));
    let mut events = Vec::new();
    while let Some(event) = execution.next().await {
        events.push(event);
    }
    assert!(!events.is_empty());
    assert!(matches!(events.last(), Some(ExecutionEvent::Completed(_))));
    let output = execution.await.expect("resolves after iteration");
    assert_eq!(output.text(), Some("beijing is sunny"));
}

#[tokio::test]
async fn cancel_is_explicit_and_observable() {
    let (runtime, mut conv) = fixture();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(MockModel::hanging())
        .build()
        .unwrap();

    // Input-side events (Started / Requested) stay on the observation
    // bypass; on the narrative face the cancellation surfaces as the
    // terminal event.
    let mut execution = agent.run(conv.turn_input("go"));
    execution.cancel();

    let mut cancelled = None;
    while let Some(event) = execution.next().await {
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
    // Await maps cancellation onto the error channel (terminal stash).
    assert!(matches!(
        execution.await,
        Err(synonz::AgentError::Cancelled(CancelReason::UserRequested))
    ));
}

#[tokio::test]
async fn agent_default_timeout_applies_to_all_runs() {
    // A model that hangs on every call (unlimited scripts): the agent-level
    // default budget must stop each of this agent's runs.
    struct AlwaysHanging;
    impl synonz::Model for AlwaysHanging {
        fn stream(
            &self,
            _request: synonz::ModelRequest,
        ) -> synonz::BoxFuture<'_, Result<synonz::ModelStream, synonz::ModelError>> {
            Box::pin(async { Ok(futures::stream::once(std::future::pending()).boxed()) })
        }
    }

    let (runtime, mut conv) = fixture();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(AlwaysHanging)
        .build()
        .unwrap()
        .with_timeout(Duration::from_millis(50));

    // The agent-level default applies to every run of this agent.
    for _ in 0..2 {
        let result = agent.run(conv.turn_input("go")).await;
        assert!(matches!(
            result,
            Err(synonz::AgentError::Cancelled(CancelReason::Timeout))
        ));
    }
}

#[tokio::test]
async fn partial_iteration_then_await_discards_remaining_deltas() {
    let (runtime, mut conv) = fixture();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(streaming_model())
        .build()
        .unwrap();

    let mut execution = agent.run(conv.turn_input("weather?"));
    let first = execution.next().await;
    assert!(matches!(
        first,
        Some(ExecutionEvent::Delta(ModelDelta::Text { .. }))
    ));

    // Awaiting early drives the run to completion, silently discarding the
    // remaining narrative events.
    let output = execution.await.expect("resolves");
    assert_eq!(output.text(), Some("beijing is sunny"));
}

#[tokio::test]
async fn with_timeout_is_chainable() {
    let (runtime, mut conv) = fixture();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(MockModel::hanging())
        .build()
        .unwrap();

    let result = agent
        .run(conv.turn_input("go"))
        .with_timeout(Duration::from_millis(30))
        .await;
    assert!(matches!(
        result,
        Err(synonz::AgentError::Cancelled(CancelReason::Timeout))
    ));
}
