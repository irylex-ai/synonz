//! Anthropic `Model` implementation translating between Synonz canonical
//! messages and the Anthropic Messages API.
//!
//! The adapter always streams (`stream: true`); text deltas are forwarded
//! as [`ModelStreamItem::Delta`], tool-use arguments are accumulated from
//! `input_json_delta` fragments, and the final message (text and tool calls
//! in block order) is emitted as [`ModelStreamItem::Finish`].

pub mod sse;
pub mod translate;

use std::sync::{Arc, RwLock};

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use synonz::{BoxFuture, ModelError, ModelStream, ModelStreamItem};

/// The default public API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// The `max_tokens` default: the Messages API requires the field, and the
/// framework does not invent hidden budgets — this is a documented
/// transport-level floor, not a policy.
pub const DEFAULT_MAX_TOKENS: u32 = 1024;

/// The Anthropic API version header value.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The request options of an Anthropic Messages API call, adjustable at
/// runtime through [`Client::set_options`].
///
/// Serialization-friendly (usable in an application's config records):
/// the provider-neutral parameters stay in
/// [`ModelParams`][synonz::ModelParams]; `effort` and `thinking` are
/// Anthropic-specific. Unset fields are omitted from requests (provider
/// defaults apply).
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelOptions {
    /// The provider-neutral parameters (temperature, token budget).
    #[serde(flatten)]
    pub params: synonz::ModelParams,
    /// The reasoning effort (`output_config.effort`), when the model
    /// supports it.
    pub effort: Option<Effort>,
    /// The thinking mode (`thinking.type`): adaptive or disabled.
    pub thinking: Option<ThinkingMode>,
}

/// The effort level of an Anthropic call.
///
/// Levels map to the wire values `low`, `medium`, `high`, `xhigh`, and
/// `max`. Support and acceptance are model-specific; the adapter does not
/// pre-validate — the provider rejects unsupported levels.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effort {
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

impl Effort {
    /// The wire value.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::ExtraHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Whether the model thinks before answering (`thinking.type`).
///
/// `Adaptive` lets the model decide when and how deeply to think;
/// `Disabled` turns thinking off. Models that think by default need no
/// mode at all (leave unset).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingMode {
    /// Adaptive thinking (`adaptive`).
    #[serde(rename = "adaptive")]
    Adaptive,
    /// Thinking disabled (`disabled`).
    #[serde(rename = "disabled")]
    Disabled,
}

impl ThinkingMode {
    /// The wire value.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Adaptive => "adaptive",
            Self::Disabled => "disabled",
        }
    }
}

/// An Anthropic Messages API client implementing [`Model`][synonz::Model].
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
    /// `https://api.anthropic.com`).
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
    /// Unset fields fall back to provider defaults (note: Anthropic
    /// requires `max_tokens`; the documented default applies when unset).
    /// Adjust them later through [`Client::set_options`].
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

    /// Creates a client from the environment.
    ///
    /// Reads `ANTHROPIC_API_KEY` (required), `ANTHROPIC_BASE_URL` and
    /// `ANTHROPIC_MODEL` (optional).
    pub fn from_env() -> Result<Self, ModelError> {
        let api_key =
            std::env::var("ANTHROPIC_API_KEY").map_err(|_| ModelError::InvalidRequest {
                message: "ANTHROPIC_API_KEY is not set".into(),
            })?;
        let base_url =
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let model_name =
            std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-sonnet-4-5".into());
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
                .post(self.endpoint("v1/messages"))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body)
                .send()
                .await
                .map_err(|error| ModelError::Transport {
                    message: error.to_string(),
                })?;

            let status = response.status().as_u16();
            if !response.status().is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(match status {
                    429 => ModelError::RateLimited { message: body },
                    400 | 404 | 422 => ModelError::InvalidRequest { message: body },
                    _ => ModelError::Api { message: body },
                });
            }

            Ok(transform_sse(response))
        })
    }
}

/// Transforms an HTTP response body (SSE framed) into model stream items.
///
/// Yields at most one item per poll; deltas always precede the terminal
/// finish item.
fn transform_sse(response: reqwest::Response) -> ModelStream {
    futures::stream::unfold(
        (
            response,
            sse::SseParser::new(),
            translate::ResponseAccumulator::default(),
            std::collections::VecDeque::new(),
            false,
        ),
        |(mut response, mut parser, mut accumulator, mut pending, done)| async move {
            loop {
                if done {
                    return None;
                }

                // Process one buffered payload, or fetch more bytes.
                let Some(payload) = pending.pop_front() else {
                    match response.chunk().await {
                        Ok(Some(chunk)) => {
                            pending.extend(parser.feed(&chunk));
                            continue;
                        }
                        Ok(None) => {
                            return match accumulator.finish_item() {
                                Ok(Some(item)) => {
                                    Some((item, (response, parser, accumulator, pending, true)))
                                }
                                _ => None,
                            };
                        }
                        Err(error) => {
                            return Some((
                                ModelStreamItem::Failed(ModelError::Transport {
                                    message: error.to_string(),
                                }),
                                (response, parser, accumulator, pending, true),
                            ));
                        }
                    }
                };

                let Ok(event) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    continue; // non-JSON keepalive payload
                };
                match accumulator.apply_event(&event) {
                    Ok(Some(item)) => {
                        return Some((item, (response, parser, accumulator, pending, false)));
                    }
                    Ok(None) => continue,
                    Err(error) => {
                        return Some((
                            ModelStreamItem::Failed(error),
                            (response, parser, accumulator, pending, true),
                        ));
                    }
                }
            }
        },
    )
    .boxed()
}
