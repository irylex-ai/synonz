//! `events`: watching the run's product narrative (ExecutionEvent).
//!
//! Run: `cargo run -p synonz-examples --bin events`

use futures::StreamExt;
use synonz::{Agent, ExecutionEvent, Subject, SubjectType, SynonzRuntime};

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
    let runtime = SynonzRuntime::builder().build();
    let mut conv = synonz::Conversation::new(&runtime, &Subject::of(SubjectType::User, "demo"));
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(StreamingModel)
        .build()
        .expect("model is set");

    let mut execution = agent.run(conv.turn_input("weather?"));
    let mut delta_text = String::new();
    while let Some(event) = execution.next().await {
        match event {
            ExecutionEvent::Delta(synonz::ModelDelta::Text { text }) => {
                delta_text.push_str(&text);
                println!("delta: {text:?}");
            }
            ExecutionEvent::Completed(output) => {
                println!("completed: {:?}", output.text());
            }
            other => println!("other narrative event: {other:?}"),
        }
    }
    println!("consumed rounds: {}", execution.rounds());
    assert_eq!(delta_text, "beijing is sunny");
}
