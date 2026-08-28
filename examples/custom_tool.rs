//! `custom_tool`: a fully offline agent using `#[derive(Tool)]` and an
//! inline model.
//!
//! Run: `cargo run -p synonz-examples --bin custom_tool`

use futures::StreamExt;
use synonz::{
    Agent, AgentEvent, Deserialize, JsonSchema, LifecycleEvent, Tool, ToolContent, ToolError,
    ToolResult,
};

/// 查询指定城市的天气。
#[derive(Tool, Deserialize, JsonSchema)]
struct Weather {
    /// 城市名，如 "beijing"。
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

    // The dual-layer API: `run` yields the full event narrative ...
    let mut run = agent.run("weather in beijing?");
    while let Some(event) = run.next().await {
        if let AgentEvent::Lifecycle(LifecycleEvent::Completed { response }) = event {
            println!("completed: {:?}", response.text());
        }
    }

    // ... and `ask` is the convenience shell built on that same stream.
    let output = agent.ask("weather?").await.expect("run completes");
    println!("ask answer: {:?}", output.text());
}
