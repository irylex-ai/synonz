//! M14 acceptance: the observation bypass (Observer / dispatcher).
//!
//! Verifies the dispatcher contract: delivery in emission order (full
//! stream, terminal included), lag reporting on overflow, panic
//! circuit-breaking (the execution and the other observers are
//! unaffected), the agent-level switch (off = zero dispatch), and
//! execution-id attribution.

#![cfg(feature = "test-util")]

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use synonz::{
    Agent, AgentEvent, Conversation, MockModel, ModelDelta, ModelStreamItem, Observer,
    ObserverContext, SubjectType, SynonzRuntime,
};

fn runtime_with(observer: impl Observer) -> SynonzRuntime {
    SynonzRuntime::builder().observer(observer).build()
}

fn text_model(replies: &[&str]) -> MockModel {
    MockModel::new(
        replies
            .iter()
            .map(|text| {
                vec![ModelStreamItem::Finish {
                    message: synonz::Message::assistant_text(*text),
                    usage: synonz::TokenUsage::new(1, 1),
                }]
            })
            .collect(),
    )
}

/// Records every delivered event (execution id + kind) and every lag
/// report. The shared inner state lets the test read what the observer
/// (owned by the runtime) recorded.
#[derive(Default, Clone)]
struct Recorder {
    inner: Arc<Mutex<Recording>>,
}

#[derive(Default)]
struct Recording {
    events: Vec<(u64, String)>,
    lags: Vec<u64>,
}

impl Recorder {
    fn recording(&self) -> std::sync::MutexGuard<'_, Recording> {
        self.inner.lock().unwrap()
    }
}

impl Observer for Recorder {
    fn on_event(&self, ctx: &ObserverContext, event: &AgentEvent) {
        let kind = match event {
            AgentEvent::Lifecycle(lifecycle) => match lifecycle {
                synonz::LifecycleEvent::Started { .. } => "started",
                synonz::LifecycleEvent::Completed { .. } => "completed",
                synonz::LifecycleEvent::Failed { .. } => "failed",
                synonz::LifecycleEvent::Cancelled { .. } => "cancelled",
                synonz::LifecycleEvent::MemoryFlowFailed { .. } => "memory-flow-failed",
                _ => "lifecycle-other",
            },
            AgentEvent::Model(synonz::ModelEvent::StreamDelta { .. }) => "delta",
            AgentEvent::Model(_) => "model",
            AgentEvent::Tool(_) => "tool",
            _ => "other",
        };
        self.recording()
            .events
            .push((ctx.execution_id, kind.to_string()));
    }

    fn on_lagged(&self, _ctx: &ObserverContext, dropped: u64) {
        self.recording().lags.push(dropped);
    }
}

#[tokio::test]
async fn observer_receives_the_full_stream_in_emission_order() {
    let recorder = Recorder::default();
    let runtime = runtime_with(recorder.clone());
    let mut conv = Conversation::new(&runtime, &synonz::Subject::of(SubjectType::User, "u"));
    let agent = Agent::builder()
        .runtime(&runtime)
        .observability(true)
        .model(MockModel::new(vec![
            vec![
                ModelStreamItem::Delta(ModelDelta::Text {
                    text: "beijing ".into(),
                }),
                ModelStreamItem::Finish {
                    message: synonz::Message::assistant_text("beijing is sunny"),
                    usage: synonz::TokenUsage::new(1, 1),
                },
            ],
            vec![ModelStreamItem::Finish {
                message: synonz::Message::assistant_text("done"),
                usage: synonz::TokenUsage::new(1, 1),
            }],
        ]))
        .build()
        .unwrap();

    let _ = agent.run(conv.turn_input("weather?")).await;
    tokio::time::sleep(Duration::from_millis(100)).await; // dispatcher drain

    let recording = recorder.recording();
    let kinds: Vec<&str> = recording
        .events
        .iter()
        .map(|(_, kind)| kind.as_str())
        .collect();
    // Full stream, single round: started → requested → delta → responded
    // → completed. Terminal last; input side visible.
    assert_eq!(
        kinds,
        vec!["started", "model", "delta", "model", "completed"],
        "full stream in emission order"
    );
    // No overflow on this small stream.
    assert!(recording.lags.is_empty());
}

// Multi-thread runtime: the slow observer must not throttle the loop
// (a current-thread runtime would serialize them, and the queue would
// never overflow).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lag_is_reported_when_the_queue_overflows() {
    /// A slow observer: the flood outpaces the queue and overflows it.
    #[derive(Default, Clone)]
    struct SlowRecorder {
        inner: Arc<Mutex<Recording>>,
    }
    impl Observer for SlowRecorder {
        fn on_event(&self, ctx: &ObserverContext, event: &AgentEvent) {
            std::thread::sleep(Duration::from_millis(1));
            let kind = match event {
                AgentEvent::Model(synonz::ModelEvent::StreamDelta { .. }) => "delta",
                _ => "other",
            };
            self.inner
                .lock()
                .unwrap()
                .events
                .push((ctx.execution_id, kind.to_string()));
        }
        fn on_lagged(&self, _ctx: &ObserverContext, dropped: u64) {
            self.inner.lock().unwrap().lags.push(dropped);
        }
    }

    let recorder = SlowRecorder::default();
    let runtime = runtime_with(recorder.clone());
    let mut conv = Conversation::new(&runtime, &synonz::Subject::of(SubjectType::User, "u"));
    // One call flooding 300 deltas: far beyond the 256-capacity queue.
    let mut script = Vec::new();
    for i in 0..300 {
        script.push(ModelStreamItem::Delta(ModelDelta::Text {
            text: format!("w{i}"),
        }));
    }
    script.push(ModelStreamItem::Finish {
        message: synonz::Message::assistant_text("flooded"),
        usage: synonz::TokenUsage::new(1, 1),
    });
    let agent = Agent::builder()
        .runtime(&runtime)
        .observability(true)
        .model(MockModel::new(vec![script]))
        .build()
        .unwrap();

    let _ = agent.run(conv.turn_input("flood")).await;
    tokio::time::sleep(Duration::from_millis(700)).await; // slow drain

    let recording = recorder.inner.lock().unwrap();
    // Total events of the run: started + requested + 300 deltas + responded
    // + completed = 304. Delivered + dropped must account for every one of
    // them — nothing vanishes silently.
    let total_dropped = recording.lags.last().copied().unwrap_or(0);
    assert!(
        !recording.lags.is_empty(),
        "overflow must be reported, not silent: delivered={}, lags={:?}",
        recording.events.len(),
        recording.lags
    );
    assert_eq!(
        recording.events.len() as u64 + total_dropped,
        304,
        "accounting is complete"
    );
}

#[tokio::test]
async fn panicking_observer_is_circuit_broken_without_harming_others() {
    /// Panics on the first delivery, then would panic again — the
    /// dispatcher must disable it after the first failure.
    #[derive(Default)]
    struct PanickingOnce {
        calls: Arc<Mutex<usize>>,
    }
    impl Observer for PanickingOnce {
        fn on_event(&self, _ctx: &ObserverContext, _event: &AgentEvent) {
            {
                let mut calls = self.calls.lock().unwrap();
                *calls += 1;
            } // guard dropped BEFORE panicking (no mutex poisoning)
            panic!("observer bug");
        }
    }

    let panic_calls = Arc::new(Mutex::new(0usize));
    let panicking = PanickingOnce {
        calls: Arc::clone(&panic_calls),
    };
    let recorder = Recorder::default();
    let runtime = SynonzRuntime::builder()
        .observer(panicking)
        .observer(recorder.clone())
        .build();
    let mut conv = Conversation::new(&runtime, &synonz::Subject::of(SubjectType::User, "u"));
    let agent = Agent::builder()
        .runtime(&runtime)
        .observability(true)
        .model(text_model(&["done"]))
        .build()
        .unwrap();

    let _ = agent.run(conv.turn_input("go")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The panicking observer was called exactly once, then disabled.
    assert_eq!(*panic_calls.lock().unwrap(), 1, "circuit breaker fired");
    // The other observer received the full stream, terminal included.
    let recording = recorder.recording();
    let kinds: Vec<&str> = recording
        .events
        .iter()
        .map(|(_, kind)| kind.as_str())
        .collect();
    assert_eq!(kinds.first(), Some(&"started"));
    assert_eq!(kinds.last(), Some(&"completed"));
}

#[tokio::test]
async fn switch_off_closes_the_observation_face() {
    let recorder = Recorder::default();
    // The runtime HAS an observer — but the agent's switch is off (default).
    let runtime = runtime_with(recorder.clone());
    let mut conv = Conversation::new(&runtime, &synonz::Subject::of(SubjectType::User, "u"));
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(text_model(&["done"]))
        .build()
        .unwrap();

    let _ = agent.run(conv.turn_input("go")).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        recorder.recording().events.is_empty(),
        "switch off = zero dispatch"
    );
}

#[tokio::test]
async fn execution_ids_attribute_concurrent_runs() {
    let recorder = Recorder::default();
    let runtime = runtime_with(recorder.clone());
    let agent = Agent::builder()
        .runtime(&runtime)
        .observability(true)
        .model(text_model(&["a", "b"]))
        .build()
        .unwrap();
    let mut conv_a = Conversation::new(&runtime, &synonz::Subject::of(SubjectType::User, "a"));
    let mut conv_b = Conversation::new(&runtime, &synonz::Subject::of(SubjectType::User, "b"));

    let (ra, rb) = tokio::join!(
        agent.run(conv_a.turn_input("q1")),
        agent.run(conv_b.turn_input("q2"))
    );
    let _ = (ra.unwrap(), rb.unwrap());
    tokio::time::sleep(Duration::from_millis(100)).await;

    let recording = recorder.recording();
    let mut ids: Vec<u64> = recording.events.iter().map(|(id, _)| *id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids.len(),
        2,
        "two runs, two distinct execution ids: {ids:?}"
    );
}
