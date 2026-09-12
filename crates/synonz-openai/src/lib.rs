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

use std::sync::{Arc, RwLock};

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use synonz::BoxFuture;
use synonz::ModelError;
use synonz::ModelStream;
use synonz::ModelStreamItem;

/// The default public API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// The request options of an OpenAI-compatible model call, adjustable at
/// runtime through [`Client::set_options`].
///
/// Serialization-friendly (usable in an application's config records):
/// the provider-neutral parameters stay in
/// [`ModelParams`][synonz::ModelParams]; `reasoning_effort` is
/// OpenAI-specific. Unset fields are omitted from requests (provider
/// defaults apply).
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelOptions {
    /// The provider-neutral parameters (temperature, token budget).
    #[serde(flatten)]
    pub params: synonz::ModelParams,
    /// The reasoning effort, when the model supports it.
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// The reasoning effort level of an OpenAI-compatible model call.
///
/// Levels map to the wire values `none`, `minimal`, `low`, `medium`,
/// `high`, `xhigh`, and `max`. Support and acceptance are model-specific;
/// the adapter does not pre-validate — the provider rejects unsupported
/// levels.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasoningEffort {
    /// No reasoning (`none`).
    #[serde(rename = "none")]
    Off,
    /// Minimal reasoning (`minimal`).
    #[serde(rename = "minimal")]
    Minimal,
    /// Low effort (`low`).
    #[serde(rename = "low")]
    Low,
    /// Medium effort (`medium`).
    #[serde(rename = "medium")]
    Medium,
    /// High effort (`high`).
    #[serde(rename = "high")]
    High,
    /// Extra-high effort (`xhigh`).
    #[serde(rename = "xhigh")]
    ExtraHigh,
    /// Maximum effort (`max`).
    #[serde(rename = "max")]
    Max,
}

impl ReasoningEffort {
    /// The wire value.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::ExtraHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// An OpenAI-compatible chat-completions client implementing
/// [`Model`][synonz::Model].
///
/// Connection and credentials are bound at construction; the model name
/// and the request options are **runtime-adjustable** through
/// [`Client::set_model`] / [`Client::set_options`] and apply to the next
/// request. Clones share the same live state (setting options on any
/// clone affects all).
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    state: Arc<RwLock<ClientState>>,
}

/// The runtime-adjustable part of a client (shared by its clones).
struct ClientState {
    model_name: String,
    options: ModelOptions,
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
            api_key: api_key.into(),
            state: Arc::new(RwLock::new(ClientState {
                model_name: model_name.into(),
                options: ModelOptions::default(),
            })),
        }
    }

    /// Binds the initial inference parameters (temperature, token budget).
    /// Unset fields fall back to provider defaults; adjust them later
    /// through [`Client::set_options`].
    pub fn params(self, params: synonz::ModelParams) -> Self {
        let mut options = self.options();
        options.params = params;
        self.set_options(options);
        self
    }

    /// A snapshot of the current request options.
    pub fn options(&self) -> ModelOptions {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .options
            .clone()
    }

    /// Replaces the request options: the next request uses them.
    pub fn set_options(&self, options: ModelOptions) {
        self.state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .options = options;
    }

    /// Replaces the model name: the next request uses it.
    pub fn set_model(&self, model_name: impl Into<String>) {
        self.state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .model_name = model_name.into();
    }

    /// Lists the model ids the endpoint exposes (`GET /models`).
    ///
    /// Ids are sorted and deduplicated. Endpoints that do not implement the
    /// standard listing route surface the provider's error.
    pub async fn list_models(&self) -> Result<Vec<String>, ModelError> {
        let response = self
            .http
            .get(self.endpoint("models"))
            .bearer_auth(&self.api_key)
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
        let body: serde_json::Value = response.json().await.map_err(|error| ModelError::Api {
            message: format!("invalid model list: {error}"),
        })?;
        let mut models: Vec<String> = body
            .get("data")
            .and_then(serde_json::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        entry
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        models.sort();
        models.dedup();
        Ok(models)
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
            let (model_name, options) = {
                let state = self
                    .state
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                (state.model_name.clone(), state.options.clone())
            };
            let body = translate::request_body(&model_name, &request, &options)?;
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
    payloads: std::collections::VecDeque<String>,
    items: std::collections::VecDeque<ModelStreamItem>,
}

/// Transforms an HTTP response body (SSE framed) into model stream items.
///
/// Yields at most one item per poll; deltas are emitted before the terminal
/// finish item. All chunk parsing goes through
/// [`translate::ResponseAccumulator::apply_chunk`].
fn transform_sse(response: reqwest::Response) -> ModelStream {
    let state = SseState {
        parser: sse::SseParser::new(),
        accumulator: translate::ResponseAccumulator::default(),
        payloads: std::collections::VecDeque::new(),
        items: std::collections::VecDeque::new(),
    };
    futures::stream::unfold(
        (response, state, false),
        |(mut response, mut state, done)| async move {
            loop {
                if done {
                    return None;
                }
                if let Some(item) = state.items.pop_front() {
                    let terminal = matches!(
                        item,
                        ModelStreamItem::Finish { .. } | ModelStreamItem::Failed(_)
                    );
                    return Some((item, (response, state, terminal)));
                }

                // Fetch more payloads when the buffer runs dry.
                let Some(payload) = state.payloads.pop_front() else {
                    match response.chunk().await {
                        Ok(Some(chunk)) => {
                            state.payloads.extend(state.parser.feed(&chunk));
                            continue;
                        }
                        Ok(None) => {
                            let item = match state.accumulator.finish_item() {
                                Ok(Some(item)) => item,
                                Ok(None) => return None,
                                Err(error) => ModelStreamItem::Failed(error),
                            };
                            return Some((item, (response, state, true)));
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
                    let item = match state.accumulator.finish_item() {
                        Ok(Some(item)) => item,
                        Ok(None) => return None,
                        Err(error) => ModelStreamItem::Failed(error),
                    };
                    return Some((item, (response, state, true)));
                }
                let Ok(chunk) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    continue; // non-JSON keepalive payload
                };
                match state.accumulator.apply_chunk(&chunk) {
                    Ok(items) => state.items.extend(items),
                    Err(error) => {
                        return Some((ModelStreamItem::Failed(error), (response, state, true)));
                    }
                }
            }
        },
    )
    .boxed()
}
