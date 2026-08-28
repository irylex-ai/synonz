//! The model contract: LLM inference behind one trait.
//!
//! A `Model` is anything that turns a [`ModelRequest`] into a stream of
//! [`ModelStreamItem`]s. Provider adapters (OpenAI, Anthropic, ...) implement
//! the trait and translate between Synonz canonical messages and their wire
//! formats; non-streaming backends are a degenerate implementation that
//! yields a single [`ModelStreamItem::Finish`].
//!
//! # Cancellation
//!
//! The future returned by [`Model::stream`] may be dropped before it
//! resolves (cancellation); implementations must drop safely (close
//! connections, release resources). Dropping the *stream* abandons the
//! remaining response.
//!
//! # No hidden retries
//!
//! Retry policy belongs to the caller. The framework performs no automatic
//! retries; hidden retries would hide latency and cost.

use crate::BoxFuture;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::error::ModelError;
use crate::event::{ModelDelta, TokenUsage};
use crate::message::Message;
use crate::tool::ToolSpec;

/// The stream of items produced by one model call.
pub type ModelStream = BoxStream<'static, ModelStreamItem>;

/// Per-request inference parameters (minimal, provider-neutral set).
///
/// Provider-specific configuration belongs at client construction, not per
/// request.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ModelParams {
    /// Sampling temperature, when the backend supports it.
    pub temperature: Option<f32>,
    /// Maximum output tokens, when the backend supports it.
    pub max_tokens: Option<u32>,
}

impl ModelParams {
    /// Sets the sampling temperature.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Sets the maximum output token budget.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

/// A request to a model: the conversation, available tools, and parameters.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    /// The canonical conversation so far.
    pub messages: Vec<Message>,
    /// Tools the model may call.
    pub tools: Vec<ToolSpec>,
    /// Per-request parameters.
    pub params: ModelParams,
}

impl ModelRequest {
    /// Creates a request from its parts.
    pub fn new(messages: Vec<Message>, tools: Vec<ToolSpec>, params: ModelParams) -> Self {
        Self {
            messages,
            tools,
            params,
        }
    }
}

/// One item of a model response stream.
///
/// A well-formed stream yields any number of [`ModelStreamItem::Delta`] and
/// then exactly one [`ModelStreamItem::Finish`] (the terminal item), or a
/// terminal [`ModelStreamItem::Failed`] on mid-stream failure.
/// Non-streaming backends yield only the finish item.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ModelStreamItem {
    /// An incremental response fragment (text in v1).
    Delta(ModelDelta),
    /// The complete response and its token accounting; terminal item.
    Finish {
        /// The complete assistant message.
        message: Message,
        /// Token accounting for this call.
        usage: TokenUsage,
    },
    /// Mid-stream failure (transport or provider protocol); terminal for
    /// the call. Added after the initial two-variant contract: streams can
    /// fail mid-flight, and failing explicitly beats masking the error as
    /// content.
    Failed(ModelError),
}

/// The model contract: perform one inference call.
///
/// Implementations translate [`ModelRequest`] (canonical messages) to their
/// wire format and yield [`ModelStreamItem`]s. Held by the agent as
/// `Arc<dyn Model>` and shared across runs.
pub trait Model: Send + Sync {
    /// Performs one model call and returns the response stream.
    ///
    /// The returned future may be dropped before resolution (cancellation);
    /// implementations must drop safely. The stream itself is `'static` and
    /// outlives the borrow of `self`.
    fn stream(&self, request: ModelRequest) -> BoxFuture<'_, Result<ModelStream, ModelError>>;
}

impl<M: Model + ?Sized> Model for std::sync::Arc<M> {
    fn stream(&self, request: ModelRequest) -> BoxFuture<'_, Result<ModelStream, ModelError>> {
        (**self).stream(request)
    }
}

/// Convenience: non-streaming completion.
///
/// Folds the model's stream until its terminal [`ModelStreamItem::Finish`],
/// discarding deltas. Returns the complete message and the call's token
/// usage.
///
/// Fails with [`ModelError::Api`] when the stream ends without a finish
/// item (provider protocol violation).
pub async fn complete(
    model: &dyn Model,
    request: ModelRequest,
) -> Result<(Message, TokenUsage), ModelError> {
    let mut stream = model.stream(request).await?;
    loop {
        match std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
            Some(ModelStreamItem::Delta(_)) => continue,
            Some(ModelStreamItem::Failed(error)) => return Err(error),
            Some(ModelStreamItem::Finish { message, usage }) => return Ok((message, usage)),
            None => {
                return Err(ModelError::Api {
                    message: "stream ended without a finish item".into(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{CallId, ContentBlock, Role, ToolCall};
    use futures::StreamExt;
    use serde_json::json;

    struct ScriptedModel {
        items: Vec<ModelStreamItem>,
    }

    impl Model for ScriptedModel {
        fn stream(&self, _request: ModelRequest) -> BoxFuture<'_, Result<ModelStream, ModelError>> {
            let items = self.items.clone();
            Box::pin(async move { Ok(futures::stream::iter(items).boxed()) })
        }
    }

    fn request() -> ModelRequest {
        ModelRequest::new(
            vec![Message::user("hi")],
            vec![ToolSpec::new("echo", "echoes", json!({"type": "object"}))],
            ModelParams::default()
                .with_temperature(0.5)
                .with_max_tokens(64),
        )
    }

    #[test]
    fn models_are_dyn_compatible() {
        let model: std::sync::Arc<dyn Model> = std::sync::Arc::new(ScriptedModel { items: vec![] });
        let _held: std::sync::Arc<dyn Model> = model;
    }

    #[tokio::test]
    async fn complete_folds_stream_until_finish() {
        let model = ScriptedModel {
            items: vec![
                ModelStreamItem::Delta(ModelDelta::Text { text: "be".into() }),
                ModelStreamItem::Delta(ModelDelta::Text {
                    text: "ijing".into(),
                }),
                ModelStreamItem::Finish {
                    message: Message::new(
                        Role::Assistant,
                        vec![ContentBlock::ToolCall(ToolCall::new(
                            CallId::new("x1"),
                            "weather",
                            json!({}),
                        ))],
                    ),
                    usage: TokenUsage::new(10, 4),
                },
            ],
        };
        let (message, usage) = complete(&model, request()).await.expect("complete ok");
        assert_eq!(usage, TokenUsage::new(10, 4));
        assert_eq!(message.blocks.len(), 1);
    }

    #[tokio::test]
    async fn complete_fails_on_premature_stream_end() {
        let model = ScriptedModel {
            items: vec![ModelStreamItem::Delta(ModelDelta::Text {
                text: "partial".into(),
            })],
        };
        let err = complete(&model, request()).await.expect_err("must fail");
        assert!(matches!(err, ModelError::Api { .. }));
    }

    #[test]
    fn params_builders() {
        let params = ModelParams::default();
        assert_eq!(params.temperature, None);
        let params = params.with_temperature(0.2).with_max_tokens(8);
        assert_eq!(params.temperature, Some(0.2));
        assert_eq!(params.max_tokens, Some(8));
    }
}
