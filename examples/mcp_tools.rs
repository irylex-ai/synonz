//! `mcp_tools`: bridging an embedded in-process MCP server.
//!
//! The example defines a minimal MCP server (with the official rmcp SDK)
//! and connects to it through [`synonz_mcp::McpBridge`] — fully offline.
//!
//! Run: `cargo run -p synonz-examples --bin mcp_tools`

use futures::StreamExt;
use rmcp::handler::server::ServerHandler;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServiceExt, tool, tool_handler, tool_router};

use synonz::{Agent, Deserialize, JsonSchema, Serialize, ToolContent, ToolResult};
use synonz_mcp::McpBridge;

/// The embedded MCP server: one tool.
#[derive(Clone)]
struct GreetingServer;

/// `greet` tool arguments.
#[derive(Deserialize, Serialize, JsonSchema)]
struct GreetArgs {
    /// Who to greet.
    name: String,
}

#[tool_router]
impl GreetingServer {
    #[tool(description = "greets someone by name")]
    async fn greet(&self, Parameters(args): Parameters<GreetArgs>) -> String {
        format!("hello, {}!", args.name)
    }
}

#[tool_handler]
impl ServerHandler for GreetingServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

/// A model that always calls `greet` with the name "synonz".
struct ScriptedModel;

impl synonz::Model for ScriptedModel {
    fn stream(
        &self,
        request: synonz::ModelRequest,
    ) -> synonz::BoxFuture<'_, Result<synonz::ModelStream, synonz::ModelError>> {
        Box::pin(async move {
            let item = if request.messages.len() <= 2 {
                synonz::ModelStreamItem::Finish {
                    message: synonz::Message::new(
                        synonz::Role::Assistant,
                        vec![synonz::ContentBlock::ToolCall(synonz::ToolCall::new(
                            "x1",
                            "greet",
                            serde_json::json!({"name": "synonz"}),
                        ))],
                    ),
                    usage: synonz::TokenUsage::new(1, 1),
                }
            } else {
                synonz::ModelStreamItem::Finish {
                    message: synonz::Message::assistant_text("greeted."),
                    usage: synonz::TokenUsage::new(1, 1),
                }
            };
            Ok(futures::stream::iter(vec![item]).boxed())
        })
    }
}

#[tokio::main]
async fn main() {
    // Serve the MCP server in-process over an in-memory transport.
    let (client_side, server_side) = tokio::io::duplex(4096);
    let server_task = tokio::spawn(async move {
        GreetingServer
            .serve(server_side)
            .await
            .expect("mcp server")
            .waiting()
            .await
            .expect("server ends when the bridge drops");
    });

    // Bridge the server's tools into Synonz.
    let bridge = McpBridge::connect(client_side).await.expect("bridge");
    let tools = bridge.tools();
    println!(
        "bridged MCP tools: {:?}",
        tools.iter().map(|t| t.name()).collect::<Vec<_>>()
    );

    // The MCP tool is an ordinary tool: call it directly ...
    let greet = bridge.tool("greet").expect("greet tool");
    let result = greet
        .execute(
            serde_json::json!({"name": "direct"}),
            synonz::ToolContext::new(synonz::CancellationToken::new()),
        )
        .await
        .expect("direct call");
    if let ToolResult::Ok {
        content: ToolContent::Text { text },
    } = result
    {
        println!("direct call: {text}");
    }

    // ... and hand it to an agent.
    let agent = Agent::builder()
        .model(ScriptedModel)
        .tools(tools)
        .build()
        .expect("model is set");
    let output = agent.ask("say hi").await.expect("run completes");
    println!("agent run: {:?}", output.text());

    drop(bridge);
    server_task.await.expect("server task");
}
