//! `events`: the event bus — a recording Observer watching the full event
//! stream of a run.
//!
//! The observer sees everything, including the input-side payloads
//! (Started / Requested / Responded) that the product narrative face
//! filters out. Registered observers observe every run — unconditionally.
//! Run:
//! `cargo run -p synonz-examples --bin events`

use std::sync::Arc;
use std::sync::Mutex;

use futures::StreamExt;
use synonz::{
    Agent, ExecutionEvent, Observer, ObserverContext, Subject, SubjectType, SynonzEvent,
    SynonzRuntime, TurnEvent,
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
/// recorders would serialize (SynonzEvent is serde) into files, tracing
/// pipelines, or an OTel exporter — heavy work belongs in the observer's
/// own queue, never on the dispatcher.
#[derive(Default)]
struct RecordingObserver {
    log: Arc<Mutex<Vec<String>>>,
}

impl Observer for RecordingObserver {
    fn on_event(&self, ctx: &ObserverContext, event: &SynonzEvent) {
        let run = ctx
            .execution_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "external".into());
        let line = match event {
            SynonzEvent::Turn(TurnEvent::Lifecycle(synonz::LifecycleEvent::Started { input })) => {
                format!("[{run}] started: {}", input.text)
            }
            SynonzEvent::Turn(TurnEvent::Model(synonz::ModelEvent::Requested {
                purpose, ..
            })) => {
                format!("[{run}] model call requested ({purpose:?})")
            }
            SynonzEvent::Turn(TurnEvent::Model(synonz::ModelEvent::StreamDelta {
                delta: synonz::ModelDelta::Text { text },
                ..
            })) => {
                format!("[{run}] delta: {text:?}")
            }
            SynonzEvent::Turn(TurnEvent::Model(synonz::ModelEvent::Responded {
                usage, ..
            })) => {
                format!("[{run}] model responded ({usage:?})")
            }
            SynonzEvent::Turn(TurnEvent::Lifecycle(synonz::LifecycleEvent::Completed {
                ..
            })) => {
                format!("[{run}] completed")
            }
            other => format!("[{run}] other: {other:?}"),
        };
        println!("{line}");
        self.log.lock().unwrap().push(line);
    }
}

#[tokio::main]
async fn main() {
    let observer = RecordingObserver::default();
    let runtime = SynonzRuntime::builder().observer(observer).build();
    let mut conv = synonz::Conversation::new(&Subject::of(SubjectType::User, "demo"));
    let agent = Agent::builder()
        .runtime(&runtime)
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
