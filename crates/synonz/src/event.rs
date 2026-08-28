//! The event model: a run's single ordered narrative.
//!
//! Every meaningful thing that happens during one agent run is visible as an
//! [`AgentEvent`] on the run's stream. The stream is the *only* information
//! channel: replaying it reconstructs the run completely (observation,
//! auditing, deterministic testing). Events carry self-sufficient payloads —
//! understanding a step never requires state outside the stream.
//!
//! # Structure
//!
//! Events form a two-level enum: the top level classifies by concern
//! (lifecycle / model / tool), and each category owns its variants. In
//! serialized form the category appears under `"type"` and the kind under
//! `"event"`:
//!
//! ```
//! use synonz::{AgentEvent, LifecycleEvent};
//!
//! let event = AgentEvent::Lifecycle(LifecycleEvent::Started {
//!     input: "weather in beijing?".into(),
//! });
//! let json = serde_json::to_value(&event).unwrap();
//! assert_eq!(json["type"], "lifecycle");
//! assert_eq!(json["event"], "started");
//! ```
//!
//! # Invariants
//!
//! - Exactly one terminal lifecycle event (`Completed`, `Failed`, or
//!   `Cancelled`) is the *last* event of a run; the stream closes after it.
//! - Model and tool events between `Started` and the terminal event
//!   characterize the working phase; the working phase itself is not an
//!   event.
//! - A "round" spans from one
//!   [`ModelEvent::Requested`] with
//!   [`CallPurpose::Reasoning`] to the next; rounds are derived by consumers,
//!   not stored.

use crate::error::AgentError;
use crate::io::AgentOutput;
use crate::message::{CallId, Message, ToolCall, ToolResult};
use serde::{Deserialize, Serialize};

/// The top-level event classification by concern.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Run lifecycle markers, including the terminal event.
    Lifecycle(LifecycleEvent),
    /// Model interactions (requests, streamed deltas, responses).
    Model(ModelEvent),
    /// Tool invocation activity.
    Tool(ToolEvent),
}

/// Lifecycle markers of a run.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LifecycleEvent {
    /// The run started; carries the input that initiated it (replay anchor).
    Started {
        /// The input the run was asked to process.
        input: crate::io::AgentInput,
    },
    /// Terminal: the run finished successfully.
    Completed {
        /// The final output.
        response: AgentOutput,
    },
    /// Terminal: the run failed.
    Failed {
        /// The failure cause.
        error: AgentError,
    },
    /// Terminal: the run was cancelled.
    Cancelled {
        /// Why the run was cancelled.
        reason: CancelReason,
    },
}

/// Why a run was cancelled.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    /// A consumer or token holder requested cancellation.
    UserRequested,
    /// The run exceeded its time budget (`with_timeout`).
    Timeout,
    /// An upstream caller propagated cancellation (reserved for multi-agent
    /// orchestration; not produced in v1).
    Parent,
}

impl core::fmt::Display for CancelReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            CancelReason::UserRequested => "user requested",
            CancelReason::Timeout => "timeout",
            CancelReason::Parent => "parent",
        };
        f.write_str(s)
    }
}

/// Why a model call was made within a run.
///
/// All model consumption inside a run is visible in the event stream; the
/// purpose distinguishes reasoning-loop calls from auxiliary calls (which
/// the framework itself does not make in v1 — no hidden model calls).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPurpose {
    /// Part of the reasoning loop (round boundary marker).
    Reasoning,
    /// Context management such as summarization (future, S2).
    ContextManagement,
    /// Classification such as intent routing (future, S3).
    Classification,
}

/// Token accounting for one model call.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Tokens consumed by the request.
    pub input_tokens: u64,
    /// Tokens produced by the response.
    pub output_tokens: u64,
}

impl TokenUsage {
    /// Creates a usage record.
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
        }
    }
}

/// An incremental piece of a streamed model response.
///
/// v1 streams text only; tool calls arrive complete in the finish message.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelDelta {
    /// A fragment of response text.
    Text {
        /// The text fragment.
        text: String,
    },
}

/// Model interaction events.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ModelEvent {
    /// The agent is about to send `messages` to the model.
    Requested {
        /// Why this call is being made.
        purpose: CallPurpose,
        /// The full canonical message list being sent (self-sufficient
        /// payload: no external state needed to interpret it).
        messages: Vec<Message>,
    },
    /// A streamed response fragment.
    StreamDelta {
        /// The delta fragment.
        delta: ModelDelta,
    },
    /// The model produced a complete response.
    Responded {
        /// The complete response message (assistant).
        message: Message,
        /// Token accounting for this call.
        usage: TokenUsage,
    },
}

/// Tool invocation events.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ToolEvent {
    /// The loop is invoking a tool requested by the model.
    CallRequested {
        /// The invocation (id, tool name, arguments).
        call: ToolCall,
    },
    /// A tool invocation finished (success or soft failure).
    CallCompleted {
        /// Correlation id of the answered call.
        call_id: CallId,
        /// The tool outcome; `Err` is fed back to the model.
        result: ToolResult,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_reason_displays() {
        assert_eq!(CancelReason::UserRequested.to_string(), "user requested");
        assert_eq!(CancelReason::Timeout.to_string(), "timeout");
    }
}
