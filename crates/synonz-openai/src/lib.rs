//! OpenAI-compatible `Model` implementation translating between Synonz
//! canonical messages and the OpenAI chat-completions API.
//!
//! Works with any OpenAI-compatible endpoint (api.openai.com, vLLM,
//! llama.cpp server, ...). The adapter always streams (`stream: true` with
//! `include_usage`); text deltas are forwarded as
//! [`ModelStreamItem::Delta`] and the final message (with accumulated tool
//! calls) is emitted as [`ModelStreamItem::Finish`].

pub mod sse;
pub mod translate;

use futures::StreamExt;

use synonz::BoxFuture;
use synonz::ModelDelta;
use synonz::ModelError;
use synonz::ModelStream;
use synonz::ModelStreamItem;

/// The default public API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// An OpenAI-compatible chat-completions client implementing
/// [`Model`][synonz::Model].
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    model_name: String,
    api_key: String,
}

impl Client {
    /// Creates a client from explicit parts.
    ///
    /// `base_url` is the API root *without* a trailing slash (for example
    /// `https://api.openai.com/v1`).
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_name: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            model_name: model_name.into(),
            api_key: api_key.into(),
        }
    }

    /// Creates a client from the environment.
    ///
    /// Reads `OPENAI_API_KEY` (required), `OPENAI_BASE_URL` (optional) and
    /// `OPENAI_MODEL` (optional).
    pub fn from_env() -> Result<Self, ModelError> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| ModelError::InvalidRequest {
            message: "OPENAI_API_KEY is not set".into(),
        })?;
        let base_url =
            std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let model_name = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
        Ok(Self::new(base_url, api_key, model_name))
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }
}

impl synonz::Model for Client {
    fn stream(
        &self,
        request: synonz::ModelRequest,
    ) -> BoxFuture<'_, Result<ModelStream, ModelError>> {
        Box::pin(async move {
            let body = translate::request_body(&self.model_name, &request)?;
            let response = self
                .http
                .post(self.endpoint("chat/completions"))
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|error| ModelError::Transport {
                    message: error.to_string(),
                })?;

            let status = response.status().as_u16();
            if !response.status().is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(translate::status_error(status, body));
            }

            Ok(transform_sse(response))
        })
    }
}

/// Per-response streaming state.
struct SseState {
    parser: sse::SseParser,
    accumulator: translate::ResponseAccumulator,
    pending: std::collections::VecDeque<String>,
    pending_finish: bool,
}

/// Transforms an HTTP response body (SSE framed) into model stream items.
///
/// Yields at most one item per poll; deltas are emitted before the terminal
/// finish item.
fn transform_sse(response: reqwest::Response) -> ModelStream {
    let state = SseState {
        parser: sse::SseParser::new(),
        accumulator: translate::ResponseAccumulator::default(),
        pending: std::collections::VecDeque::new(),
        pending_finish: false,
    };
    futures::stream::unfold(
        (response, state, false),
        |(mut response, mut state, done)| async move {
            loop {
                if done {
                    return None;
                }

                // Process one buffered payload, or fetch more bytes.
                let Some(payload) = state.pending.pop_front() else {
                    match response.chunk().await {
                        Ok(Some(chunk)) => {
                            let payloads = state.parser.feed(&chunk);
                            state.pending.extend(payloads);
                            continue;
                        }
                        Ok(None) => {
                            return state
                                .accumulator
                                .finish_item()
                                .map(|item| (item, (response, state, true)));
                        }
                        Err(error) => {
                            return Some((
                                ModelStreamItem::Failed(ModelError::Transport {
                                    message: error.to_string(),
                                }),
                                (response, state, true),
                            ));
                        }
                    }
                };

                if payload == "[DONE]" {
                    return state
                        .accumulator
                        .finish_item()
                        .map(|item| (item, (response, state, true)));
                }
                let Ok(chunk) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    continue; // non-JSON keepalive payload
                };

                // Text fragments of this chunk become one delta item.
                let mut text = String::new();
                if let Some(choices) = chunk.get("choices").and_then(serde_json::Value::as_array) {
                    for choice in choices {
                        if let Some(fragment) = choice
                            .pointer("/delta/content")
                            .and_then(serde_json::Value::as_str)
                        {
                            text.push_str(fragment);
                        }
                        if choice
                            .get("finish_reason")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|reason| !reason.is_empty())
                        {
                            state.pending_finish = true;
                        }
                    }
                }
                if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
                    state.accumulator.absorb_usage(usage);
                }

                if !text.is_empty() {
                    // Accumulate text for the final message, and yield the
                    // delta; the finish (if flagged) goes on the next poll.
                    state.accumulator.push_text(&text);
                    return Some((
                        ModelStreamItem::Delta(ModelDelta::Text { text }),
                        (response, state, false),
                    ));
                }
                if state.pending_finish {
                    return state
                        .accumulator
                        .finish_item()
                        .map(|item| (item, (response, state, true)));
                }
            }
        },
    )
    .boxed()
}
