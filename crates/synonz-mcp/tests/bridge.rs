//! Bridge tests: discovery + call round-trips over an in-memory transport
//! and over a real stdio child process.

use rmcp::handler::server::ServerHandler;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ErrorData, ServerCapabilities, ServerInfo};
use rmcp::{ServiceExt, tool, tool_handler, tool_router};
use serde_json::json;

use synonz::{
    CancellationToken, Deserialize, JsonSchema, Serialize, Tool, ToolContent, ToolContext,
    ToolError, ToolResult,
};
use synonz_mcp::McpBridge;

/// The MCP test server: an echo tool and an always-failing tool.
#[derive(Clone)]
struct EchoServer;

/// Echo tool arguments.
#[derive(Deserialize, Serialize, JsonSchema)]
struct EchoArgs {
    /// The text to echo back.
    text: String,
}

#[tool_router]
impl EchoServer {
    #[tool(description = "echoes its input text")]
    async fn echo(&self, Parameters(args): Parameters<EchoArgs>) -> String {
        format!("echo: {}", args.text)
    }

    #[tool(description = "always fails")]
    async fn fail(&self) -> Result<String, ErrorData> {
        Err(ErrorData::internal_error("boom", None))
    }
}

#[tool_handler]
impl ServerHandler for EchoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

fn context() -> ToolContext {
    ToolContext::new(CancellationToken::new())
}

#[tokio::test]
async fn bridge_discovers_and_calls_tools_over_duplex() {
    let (client_side, server_side) = tokio::io::duplex(1024);
    // The join handle keeps the server's running service alive for the
    // duration of the test; dropping it early would cancel the connection.
    let server_task = tokio::spawn(async move {
        EchoServer
            .serve(server_side)
            .await
            .expect("server side")
            .waiting()
            .await
            .expect("server ends when the client disconnects");
    });

    let bridge = McpBridge::connect(client_side).await.expect("connect");

    // Discovery: both tools with their metadata.
    assert_eq!(bridge.tools().len(), 2);
    let echo = bridge.tool("echo").expect("echo tool");
    assert_eq!(echo.name(), "echo");
    assert_eq!(echo.description(), "echoes its input text");
    assert!(echo.parameters_schema().get("properties").is_some());

    // Call round-trip: success.
    let result = echo
        .execute(json!({"text": "hi"}), context())
        .await
        .expect("call succeeds");
    assert_eq!(
        result,
        ToolResult::Ok {
            content: ToolContent::Text {
                text: "echo: hi".into(),
            },
        }
    );

    // Soft failure: the server's error maps to ToolResult::Err.
    let fail = bridge.tool("fail").expect("fail tool");
    let result = fail
        .execute(json!({}), context())
        .await
        .expect("soft failure");
    assert!(matches!(result, ToolResult::Err { message } if message.contains("boom")));

    // Non-object arguments are rejected before the wire.
    let error = echo.execute(json!(42), context()).await;
    assert!(matches!(error, Err(ToolError::InvalidArguments { .. })));

    drop(bridge); // closes the session
    server_task.await.expect("server task ends");
}

/// Serves the test server over stdio; invoked as a child test process by
/// `bridge_roundtrips_over_stdio`.
#[tokio::test]
#[ignore = "runs only as the stdio child process of bridge_roundtrips_over_stdio"]
async fn stdio_helper() {
    EchoServer
        .serve(rmcp::transport::stdio())
        .await
        .expect("stdio server")
        .waiting()
        .await
        .expect("server ends when the client disconnects");
}

#[tokio::test]
async fn bridge_roundtrips_over_stdio() {
    let exe = std::env::current_exe().expect("current exe");
    let exe_path = exe.to_str().expect("utf-8 path");
    let bridge = McpBridge::connect_stdio(
        exe_path,
        &["--exact", "stdio_helper", "--ignored", "--nocapture"],
    )
    .await
    .expect("stdio connect");

    let echo = bridge.tool("echo").expect("echo tool");
    let result = echo
        .execute(json!({"text": "over stdio"}), context())
        .await
        .expect("stdio call");
    assert_eq!(
        result,
        ToolResult::Ok {
            content: ToolContent::Text {
                text: "echo: over stdio".into(),
            },
        }
    );

    drop(bridge); // closing the bridge ends the child server
}
