//! `custom_tool`: a fully offline agent using `#[derive(Tool)]` and an
//! inline model.
//!
//! Run: `cargo run -p synonz-examples --bin custom_tool`

use futures::StreamExt;
use synonz::{
    Agent, Deserialize, ExecutionEvent, JsonSchema, Tool, ToolContent, ToolError, ToolResult,
};

/// Queries the current weather for a city.
#[derive(Tool, Deserialize, JsonSchema)]
struct Weather {
    /// The city name, e.g. "beijing".
    city: String,
}

impl Weather {
    async fn run(&self) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::Ok {
            content: ToolContent::Text {
                text: format!("{}: sunny, 28C", self.city),
            },
        })
    }
}

/// An inline scripted model answering one round with a weather call.
struct ScriptedModel;

impl synonz::Model for ScriptedModel {
    fn stream(
        &self,
        request: synonz::ModelRequest,
    ) -> synonz::BoxFuture<'_, Result<synonz::ModelStream, synonz::ModelError>> {
        Box::pin(async move {
            // First round: call the weather tool; later rounds: answer.
            let item = if request.messages.len() <= 2 {
                synonz::ModelStreamItem::Finish {
                    message: synonz::Message::new(
                        synonz::Role::Assistant,
                        vec![synonz::ContentBlock::ToolCall(synonz::ToolCall::new(
                            "x1",
                            "weather",
                            serde_json::json!({"city": "beijing"}),
                        ))],
                    ),
                    usage: synonz::TokenUsage::new(1, 1),
                }
            } else {
                synonz::ModelStreamItem::Finish {
                    message: synonz::Message::assistant_text("Done — the weather tool ran."),
                    usage: synonz::TokenUsage::new(1, 1),
                }
            };
            Ok(futures::stream::iter(vec![item]).boxed())
        })
    }
}

#[tokio::main]
async fn main() {
    let agent = Agent::builder()
        .model(ScriptedModel)
        .system_prompt("you are a weather assistant")
        .tool(Weather {
            city: String::new(),
        })
        .build()
        .expect("model is set");

    // `run` streams the product narrative: deltas, tool activity, and the
    // terminal event carrying the output.
    let mut execution = agent.run("weather in beijing?");
    while let Some(event) = execution.next().await {
        if let ExecutionEvent::Completed(output) = event {
            println!("completed: {:?}", output.text());
        }
    }

    // One-shot spelling: awaiting the handle resolves to the same output.
    let output = agent.run("weather?").await.expect("run completes");
    println!("run answer: {:?}", output.text());
}
