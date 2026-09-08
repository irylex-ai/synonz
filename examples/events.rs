//! `events`: the observation bypass — a recording Observer watching the
//! full event stream of a run.
//!
//! The observer sees everything, including the input-side payloads
//! (Started / Requested / Responded) that the product narrative face
//! filters out. Run:
//! `cargo run -p synonz-examples --bin events`

use std::sync::Arc;
use std::sync::Mutex;

use futures::StreamExt;
use synonz::{
    Agent, AgentEvent, ExecutionEvent, Observer, ObserverContext, Subject, SubjectType,
    SynonzRuntime,
};

/// A model that streams two text deltas and then finishes.
struct StreamingModel;

impl synonz::Model for StreamingModel {
    fn stream(
        &self,
        _request: synonz::ModelRequest,
    ) -> synonz::BoxFuture<'_, Result<synonz::ModelStream, synonz::ModelError>> {
        Box::pin(async move {
            let items = vec![
                synonz::ModelStreamItem::Delta(synonz::ModelDelta::Text {
                    text: "beijing ".into(),
                }),
                synonz::ModelStreamItem::Delta(synonz::ModelDelta::Text {
                    text: "is sunny".into(),
                }),
                synonz::ModelStreamItem::Finish {
                    message: synonz::Message::assistant_text("beijing is sunny"),
                    usage: synonz::TokenUsage::new(3, 2),
                },
            ];
            Ok(futures::stream::iter(items).boxed())
        })
    }
}

/// A recording observer: appends every event to a shared log. Real
/// recorders would serialize (AgentEvent is serde) into files, tracing
/// pipelines, or an OTel exporter — heavy work belongs in the observer's
/// own queue, never on the dispatcher.
#[derive(Default)]
struct RecordingObserver {
    log: Arc<Mutex<Vec<String>>>,
}

impl Observer for RecordingObserver {
    fn on_event(&self, ctx: &ObserverContext, event: &AgentEvent) {
        let line = match event {
            AgentEvent::Lifecycle(synonz::LifecycleEvent::Started { input }) => {
                format!("[{}] started: {}", ctx.execution_id, input.text)
            }
            AgentEvent::Model(synonz::ModelEvent::Requested { purpose, .. }) => {
                format!("[{}] model call requested ({purpose:?})", ctx.execution_id)
            }
            AgentEvent::Model(synonz::ModelEvent::StreamDelta {
                delta: synonz::ModelDelta::Text { text },
            }) => {
                format!("[{}] delta: {text:?}", ctx.execution_id)
            }
            AgentEvent::Model(synonz::ModelEvent::Responded { usage, .. }) => {
                format!("[{}] model responded ({usage:?})", ctx.execution_id)
            }
            AgentEvent::Lifecycle(synonz::LifecycleEvent::Completed { .. }) => {
                format!("[{}] completed", ctx.execution_id)
            }
            other => format!("[{}] other: {other:?}", ctx.execution_id),
        };
        println!("{line}");
        self.log.lock().unwrap().push(line);
    }
}

#[tokio::main]
async fn main() {
    let observer = RecordingObserver::default();
    let runtime = SynonzRuntime::builder().observer(observer).build();
    let mut conv = synonz::Conversation::new(&runtime, &Subject::of(SubjectType::User, "demo"));
    let agent = Agent::builder()
        .runtime(&runtime)
        .observability(true) // open the observation face for this agent
        .model(StreamingModel)
        .build()
        .expect("model is set");

    // The product narrative face: deltas for the UI, terminal with output.
    let mut execution = agent.run(conv.turn_input("weather?"));
    let mut delta_text = String::new();
    while let Some(event) = execution.next().await {
        if let ExecutionEvent::Delta(synonz::ModelDelta::Text { text }) = event {
            delta_text.push_str(&text);
            print!("narrative delta: {text:?}");
        }
    }
    println!();
    println!("narrative text: {delta_text}");
    assert_eq!(delta_text, "beijing is sunny");
}
