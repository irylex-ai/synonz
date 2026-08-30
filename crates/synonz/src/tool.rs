//! The tool contract: capabilities an agent can invoke.
//!
//! A tool is an in-process or bridged capability unit. The core trait is
//! deliberately *dynamic* (`serde_json::Value` arguments, dyn-compatible) so
//! that runtime-discovered tools (for example, MCP servers) and typed
//! in-process tools share one contract; typed ergonomics are recovered by
//! `#[derive(Tool)]` (see `synonz-derive`).
//!
//! # Soft failures
//!
//! Tool failures do not terminate a run. A tool may report failure either
//! by returning [`ToolResult::Err`] or by returning `Err(ToolError)`; both
//! are converted by the loop into a [`ToolResult::Err`] fed back to the
//! model, which may retry, adjust, or abandon.
//!
//! # Cancellation
//!
//! Implementations receive [`ToolContext`] with the run's cancellation
//! signal. Cooperative interruption semantics (the framework aborts at the
//! next await point) apply; implementations should document their
//! cancellation safety class.

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::BoxFuture;
use crate::message::ToolResult;

/// The context passed to every tool invocation.
///
/// Extensible without breaking implementers: new capabilities are added as
/// fields behind `#[non_exhaustive]`.
#[non_exhaustive]
#[derive(Clone)]
pub struct ToolContext {
    /// The run's cancellation signal. Tools should observe it at their await
    /// points and terminate promptly when it fires.
    pub cancel: crate::CancellationToken,
}

impl ToolContext {
    /// Creates a context with the given cancellation signal.
    pub fn new(cancel: crate::CancellationToken) -> Self {
        Self { cancel }
    }
}

/// A capability unit invocable by the agent.
///
/// Implement this trait directly (dynamic form) or use `#[derive(Tool)]`
/// for the typed form. The contract is dyn-compatible so an agent can hold
/// a heterogeneous set of tools (in-process and bridged) behind
/// `Arc<dyn Tool>`.
///
/// # Typed form via `#[derive(Tool)]`
///
/// ```
/// use synonz::{Deserialize, JsonSchema, Tool, ToolContent, ToolError, ToolResult};
///
/// /// Queries the current weather for a city.
/// #[derive(Tool, Deserialize, JsonSchema)]
/// struct Weather {
///     /// The city name.
///     city: String,
/// }
///
/// impl Weather {
///     async fn run(&self) -> Result<ToolResult, ToolError> {
///         Ok(ToolResult::Ok {
///             content: ToolContent::Text {
///                 text: format!("{}: sunny", self.city),
///             },
///         })
///     }
/// }
///
/// # async fn demo(ctx: synonz::ToolContext) {
/// let weather = Weather { city: "beijing".into() };
/// assert_eq!(weather.name(), "weather");
/// let result = weather
///     .execute(serde_json::json!({"city": "beijing"}), ctx)
///     .await
///     .unwrap();
/// assert!(matches!(result, ToolResult::Ok { .. }));
/// # }
/// ```
pub trait Tool: Send + Sync {
    /// The tool's name as the model addresses it.
    fn name(&self) -> &str;

    /// What the tool does, shown to the model.
    fn description(&self) -> &str;

    /// A JSON Schema object describing the accepted arguments.
    fn parameters_schema(&self) -> &Value;

    /// Invokes the tool.
    ///
    /// `args` is the JSON object the model produced; `ctx` carries the run's
    /// cancellation signal. Failures are soft: both `Err(ToolError)` and
    /// [`ToolResult::Err`] are fed back to the model rather than aborting
    /// the run.
    fn execute<'a>(
        &'a self,
        args: Value,
        ctx: ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, ToolError>>;
}

impl<T: Tool + ?Sized> Tool for std::sync::Arc<T> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn description(&self) -> &str {
        (**self).description()
    }
    fn parameters_schema(&self) -> &Value {
        (**self).parameters_schema()
    }
    fn execute<'a>(
        &'a self,
        args: Value,
        ctx: ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, ToolError>> {
        (**self).execute(args, ctx)
    }
}

/// Failure of a tool invocation's machinery.
///
/// Always soft: converted by the loop into [`ToolResult::Err`] for the
/// model. Distinguishes malformed arguments (schema/parse failures) from
/// execution-time failures so model feedback can be specific.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ToolError {
    /// The arguments did not match the tool's expected form.
    #[error("invalid arguments: {message}")]
    InvalidArguments {
        /// What was invalid about the arguments.
        message: String,
    },
    /// The tool failed before producing a result.
    #[error("tool execution failed: {message}")]
    Execution {
        /// What failed during execution.
        message: String,
    },
}

/// The model-facing description of a tool, as sent with model requests.
///
/// This is the meeting point of the tool and model contracts: the loop
/// collects [`ToolSpec`]s from registered tools and passes them in
/// [`ModelRequest`][crate::ModelRequest]s.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// The tool's name as the model addresses it.
    pub name: String,
    /// What the tool does, shown to the model.
    pub description: String,
    /// JSON Schema object describing the accepted arguments.
    pub parameters_schema: Value,
}

impl ToolSpec {
    /// Creates a spec from its parts.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters_schema,
        }
    }

    /// Collects the spec of a tool.
    pub fn for_tool(tool: &dyn Tool) -> Self {
        Self {
            name: tool.name().to_owned(),
            description: tool.description().to_owned(),
            parameters_schema: tool.parameters_schema().clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolContent;
    use serde_json::json;

    struct Echo {
        schema: Value,
    }

    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echoes its arguments back"
        }
        fn parameters_schema(&self) -> &Value {
            &self.schema
        }
        fn execute<'a>(
            &'a self,
            args: Value,
            _ctx: ToolContext,
        ) -> BoxFuture<'a, Result<ToolResult, ToolError>> {
            Box::pin(async move {
                Ok(ToolResult::Ok {
                    content: ToolContent::Json { value: args },
                })
            })
        }
    }

    #[test]
    fn tools_are_dyn_compatible() {
        let tools: Vec<std::sync::Arc<dyn Tool>> = vec![std::sync::Arc::new(Echo {
            schema: json!({"type": "object"}),
        })];
        assert_eq!(tools[0].name(), "echo");
    }

    #[tokio::test]
    async fn execute_roundtrips_arguments() {
        let tool = Echo {
            schema: json!({"type": "object"}),
        };
        let ctx = ToolContext::new(crate::CancellationToken::new());
        let result = tool
            .execute(json!({"x": 1}), ctx)
            .await
            .expect("execute ok");
        assert_eq!(
            result,
            ToolResult::Ok {
                content: ToolContent::Json {
                    value: json!({"x": 1})
                }
            }
        );
    }

    #[test]
    fn spec_collects_from_dyn_tool() {
        let tool = Echo {
            schema: json!({"type": "object"}),
        };
        let spec = ToolSpec::for_tool(&tool);
        assert_eq!(spec.name, "echo");
        assert_eq!(spec.parameters_schema, json!({"type": "object"}));
    }

    #[test]
    fn tool_error_displays() {
        assert_eq!(
            ToolError::InvalidArguments {
                message: "missing city".into()
            }
            .to_string(),
            "invalid arguments: missing city"
        );
    }
}
