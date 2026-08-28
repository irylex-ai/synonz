//! MCP bridge: tools exposed by MCP (Model Context Protocol) servers become
//! Synonz [`Tool`] implementations.
//!
//! The bridge is an adapter, not a protocol layer of its own: it connects
//! to an MCP server (stdio child process, or any transport), discovers its
//! tools, and hands them to the agent loop as ordinary tools. Cancellation
//! follows the remote-tool class of the tiered contract (dropping the
//! connection abandons in-flight calls).
//!
//! # Usage
//!
//! ```no_run
//! # async fn demo() {
//! let bridge = synonz_mcp::McpBridge::connect_stdio(
//!     "npx",
//!     &["-y", "@modelcontextprotocol/server-everything"],
//! )
//! .await
//! .unwrap();
//! let tools = bridge.tools();
//! # }
//! ```

use std::sync::Arc;

use rmcp::ServiceExt as _;
use rmcp::service::{Peer, RoleClient, RunningService};
use rmcp::transport::IntoTransport;
use rmcp::transport::child_process::TokioChildProcess;
use serde_json::Value;
use thiserror::Error;

use synonz::{Tool, ToolContent, ToolContext, ToolError, ToolResult};

/// Bridge-level failures: connection setup and discovery.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum McpError {
    /// The MCP server process could not be spawned.
    #[error("failed to spawn MCP server process: {0}")]
    Spawn(std::io::Error),
    /// The MCP initialization handshake failed.
    #[error("MCP initialization failed: {0}")]
    Initialize(Box<rmcp::service::ClientInitializeError>),
    /// Tool discovery failed.
    #[error("tool discovery failed: {0}")]
    Discovery(rmcp::ServiceError),
}

/// A connected MCP server whose tools are available as Synonz tools.
pub struct McpBridge {
    // Keeps the connection (and its runtime task) alive; dropping it
    // cancels the session, which aborts in-flight calls.
    _service: RunningService<RoleClient, ()>,
    tools: Vec<Arc<McpTool>>,
}

impl McpBridge {
    /// Connects over any MCP transport (the generic escape hatch; stdio and
    /// streamable HTTP both fit here).
    pub async fn connect<E, A, T>(transport: T) -> Result<McpBridge, McpError>
    where
        T: IntoTransport<RoleClient, E, A> + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        let client =
            ().serve(transport)
                .await
                .map_err(|error| McpError::Initialize(Box::new(error)))?;
        McpBridge::from_running(client).await
    }

    /// Connects to an MCP server running as a child process over stdio.
    pub async fn connect_stdio(command: &str, args: &[&str]) -> Result<McpBridge, McpError> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args);
        let (transport, _stderr) = TokioChildProcess::builder(cmd)
            .spawn()
            .map_err(McpError::Spawn)?;
        McpBridge::connect(transport).await
    }

    async fn from_running(client: RunningService<RoleClient, ()>) -> Result<McpBridge, McpError> {
        let discovered = client.list_all_tools().await.map_err(McpError::Discovery)?;
        let peer = client.peer().clone();
        let tools = discovered
            .into_iter()
            .map(|tool| Arc::new(McpTool::new(tool, peer.clone())))
            .collect();
        Ok(McpBridge {
            _service: client,
            tools,
        })
    }

    /// All discovered tools, ready for `AgentBuilder::tools`.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools
            .iter()
            .cloned()
            .map(|tool| tool as Arc<dyn Tool>)
            .collect()
    }

    /// One discovered tool by name.
    pub fn tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .cloned()
            .map(|tool| tool as Arc<dyn Tool>)
    }
}

/// One MCP server tool bridged to the Synonz [`Tool`] contract.
struct McpTool {
    peer: Peer<RoleClient>,
    name: String,
    description: String,
    schema: Value,
}

impl McpTool {
    fn new(tool: rmcp::model::Tool, peer: Peer<RoleClient>) -> Self {
        Self {
            peer,
            name: tool.name.into_owned(),
            description: tool.description.map(|d| d.into_owned()).unwrap_or_default(),
            schema: Value::Object((*tool.input_schema).clone()),
        }
    }
}

impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> &Value {
        &self.schema
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _ctx: ToolContext,
    ) -> synonz::BoxFuture<'a, Result<ToolResult, ToolError>> {
        Box::pin(async move {
            let Value::Object(arguments) = args else {
                return Err(ToolError::InvalidArguments {
                    message: "tool arguments must be a JSON object".into(),
                });
            };
            let params = rmcp::model::CallToolRequestParams::new(self.name.clone())
                .with_arguments(arguments);
            match self.peer.call_tool(params).await {
                Ok(result) => Ok(map_call_result(result)),
                // Server-side tool failures surface as JSON-RPC errors in
                // rmcp; both are soft failures fed back to the model.
                Err(error) => Ok(ToolResult::Err {
                    message: error.to_string(),
                }),
            }
        })
    }
}

/// Maps an MCP call result onto the Synonz soft-failure semantics.
fn map_call_result(result: rmcp::model::CallToolResult) -> ToolResult {
    if result.is_error == Some(true) {
        ToolResult::Err {
            message: text_of(&result.content)
                .unwrap_or_else(|| "tool reported an error without a message".into()),
        }
    } else if let Some(json) = result.structured_content {
        ToolResult::Ok {
            content: ToolContent::Json { value: json },
        }
    } else {
        ToolResult::Ok {
            content: ToolContent::Text {
                text: text_of(&result.content).unwrap_or_default(),
            },
        }
    }
}

fn text_of(blocks: &[rmcp::model::ContentBlock]) -> Option<String> {
    let mut text = String::new();
    for block in blocks {
        if let rmcp::model::ContentBlock::Text(content) = block {
            text.push_str(&content.text);
        }
    }
    (!text.is_empty()).then_some(text)
}
