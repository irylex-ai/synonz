//! `events`: watching the run's event narrative (the observability core).
//!
//! Run: `cargo run -p synonz-examples --bin events`

use futures::StreamExt;
use synonz::{Agent, AgentEvent};

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

#[tokio::main]
async fn main() {
    let agent = Agent::builder()
        .model(StreamingModel)
        .build()
        .expect("model is set");

    let mut run = agent.run("weather?");
    let mut delta_text = String::new();
    while let Some(event) = run.next().await {
        match event {
            AgentEvent::Lifecycle(synonz::LifecycleEvent::Started { input }) => {
                println!("started with: {}", input.text);
            }
            AgentEvent::Model(synonz::ModelEvent::StreamDelta {
                delta: synonz::ModelDelta::Text { text },
            }) => {
                delta_text.push_str(&text);
                println!("delta: {text:?}");
            }
            AgentEvent::Model(synonz::ModelEvent::Responded { usage, .. }) => {
                println!("model call used {usage:?}");
            }
            AgentEvent::Lifecycle(synonz::LifecycleEvent::Completed { response }) => {
                println!("completed: {:?}", response.text());
            }
            other => println!("other event: {other:?}"),
        }
    }
    println!("consumed rounds: {}", run.rounds());
    assert_eq!(delta_text, "beijing is sunny");
}
