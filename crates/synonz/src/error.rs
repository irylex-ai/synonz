//! Error types for the agent loop and model calls.

use crate::event::CancelReason;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Failure of a model call, as reported by a [`Model`][crate::Model]
/// implementation.
///
/// `ModelError` is *hard*: it aborts the run (wrapped as
/// [`AgentError::Model`]). Tool failures are soft by contrast and are fed
/// back to the model as
/// [`ToolResult::Err`][crate::ToolResult::Err].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelError {
    /// The request could not be delivered (network, DNS, connection).
    #[error("transport error: {message}")]
    Transport {
        /// What went wrong on the transport layer.
        message: String,
    },
    /// The provider returned an error response.
    #[error("api error: {message}")]
    Api {
        /// Provider-reported failure description.
        message: String,
    },
    /// The provider throttled the request.
    #[error("rate limited: {message}")]
    RateLimited {
        /// Provider-reported throttling description.
        message: String,
    },
    /// The request was malformed or rejected without being processed.
    #[error("invalid request: {message}")]
    InvalidRequest {
        /// What was invalid about the request.
        message: String,
    },
}

/// Run-level failure.
///
/// `AgentError` terminates a run with
/// [`LifecycleEvent::Failed`][crate::LifecycleEvent::Failed].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Error, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentError {
    /// A model call failed.
    #[error("model call failed: {0}")]
    Model(#[from] ModelError),
    /// The round budget was exhausted before the run completed.
    #[error("max rounds exceeded")]
    MaxRoundsExceeded,
    /// The run was cancelled (see [`CancelReason`]); not a failure of
    /// behavior.
    #[error("run cancelled: {0}")]
    Cancelled(CancelReason),
    /// The agent was assembled incorrectly (for example, without a model).
    #[error("invalid configuration: {message}")]
    InvalidConfiguration {
        /// What was misconfigured.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_display_with_context() {
        let err = AgentError::Model(ModelError::Transport {
            message: "connection reset".into(),
        });
        assert_eq!(
            err.to_string(),
            "model call failed: transport error: connection reset"
        );
        assert_eq!(
            AgentError::MaxRoundsExceeded.to_string(),
            "max rounds exceeded"
        );
        assert_eq!(
            AgentError::Cancelled(CancelReason::Timeout).to_string(),
            "run cancelled: timeout"
        );
    }
}
