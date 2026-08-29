//! M10 acceptance: the converged handle family (Answer / Run).
//!
//! Verifies the dual-faced semantics: streaming via `next()`, final result
//! via `.await`, explicit `cancel()`, and the interleaving rules.

#![cfg(feature = "test-util")]

use std::time::Duration;

use synonz::{
    Agent, AgentEvent, CancelReason, LifecycleEvent, MockModel, ModelDelta, ModelStreamItem,
};

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
async fn answer_streams_deltas_then_resolves_on_await() {
    let agent = Agent::builder().model(streaming_model()).build().unwrap();

    let mut answer = agent.ask("weather?");

    let mut text = String::new();
    while let Some(delta) = answer.next().await {
        if let ModelDelta::Text { text: fragment } = delta {
            text.push_str(&fragment);
        }
    }
    assert_eq!(text, "beijing is sunny");

    // Awaiting after full iteration still resolves (terminal was stashed).
    let output = answer.await.expect("answer resolves");
    assert_eq!(output.text(), Some("beijing is sunny"));
}

#[tokio::test]
async fn answer_awaits_directly_as_one_shot() {
    let agent = Agent::builder().model(streaming_model()).build().unwrap();

    // The zero-breaking spelling: identical to the previous blocking ask.
    let output = agent.ask("weather?").await.unwrap();
    assert_eq!(output.text(), Some("beijing is sunny"));
    assert_eq!(output.usage.input_tokens, 3);
}

#[tokio::test]
async fn run_is_a_stream_and_a_future() {
    let agent = Agent::builder().model(streaming_model()).build().unwrap();

    // Future face: await directly.
    let run = agent.run("weather?");
    let output = run.await.expect("run resolves");
    assert_eq!(output.text(), Some("beijing is sunny"));

    // Stream face (fresh agent + script): iterate the full narrative, then
    // await still resolves.
    let stream_agent = Agent::builder().model(streaming_model()).build().unwrap();
    let run = stream_agent.run("weather?");
    let mut events = Vec::new();
    let mut run = run;
    while let Some(event) = run.next().await {
        events.push(event);
    }
    assert!(!events.is_empty());
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Lifecycle(LifecycleEvent::Completed { .. }))
    ));
    let output = run.await.expect("resolves after iteration");
    assert_eq!(output.text(), Some("beijing is sunny"));
}

#[tokio::test]
async fn cancel_is_explicit_and_observable() {
    let agent = Agent::builder()
        .model(MockModel::hanging())
        .build()
        .unwrap();

    let mut run = agent.run("go");
    let started = run.next().await;
    assert!(matches!(
        started,
        Some(AgentEvent::Lifecycle(LifecycleEvent::Started { .. }))
    ));

    run.cancel();

    let mut cancelled = None;
    while let Some(event) = run.next().await {
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
    // Await maps cancellation onto the error channel.
    let run = agent.run("go");
    run.cancel();
    assert!(matches!(
        run.await,
        Err(synonz::AgentError::Cancelled(CancelReason::UserRequested))
    ));
}

#[tokio::test]
async fn agent_default_timeout_applies_to_all_runs() {
    let agent = Agent::builder()
        .model(MockModel::hanging())
        .build()
        .unwrap()
        .with_timeout(Duration::from_millis(50));

    let result = agent.run("go").await;
    assert!(matches!(
        result,
        Err(synonz::AgentError::Cancelled(CancelReason::Timeout))
    ));

    let ask_agent = Agent::builder()
        .model(MockModel::hanging())
        .build()
        .unwrap()
        .with_timeout(Duration::from_millis(50));
    let result = ask_agent.ask("go").await;
    assert!(matches!(
        result,
        Err(synonz::AgentError::Cancelled(CancelReason::Timeout))
    ));
}

#[tokio::test]
async fn partial_iteration_then_await_discards_remaining_deltas() {
    let agent = Agent::builder().model(streaming_model()).build().unwrap();

    let mut answer = agent.ask("weather?");
    let first = answer.next().await;
    assert!(matches!(first, Some(ModelDelta::Text { .. })));

    // Awaiting early drives the run to completion, silently discarding the
    // remaining deltas.
    let output = answer.await.expect("resolves");
    assert_eq!(output.text(), Some("beijing is sunny"));
}

#[tokio::test]
async fn run_with_timeout_and_answer_with_timeout_are_chainable() {
    let agent = Agent::builder()
        .model(MockModel::hanging())
        .build()
        .unwrap();

    let result = agent
        .run("go")
        .with_timeout(Duration::from_millis(30))
        .await;
    assert!(matches!(
        result,
        Err(synonz::AgentError::Cancelled(CancelReason::Timeout))
    ));

    let ask_agent = Agent::builder()
        .model(MockModel::hanging())
        .build()
        .unwrap();
    let result = ask_agent
        .ask("go")
        .with_timeout(Duration::from_millis(30))
        .await;
    assert!(matches!(
        result,
        Err(synonz::AgentError::Cancelled(CancelReason::Timeout))
    ));
}
