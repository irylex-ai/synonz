//! The event model: a run's single ordered narrative.
//!
//! Every meaningful thing that happens during one agent run is visible as a
//! [`TurnEvent`] on the run's stream. The stream is the *only* information
//! channel: replaying it reconstructs the run completely (observation,
//! auditing, deterministic testing). Events carry self-sufficient payloads —
//! understanding a step never requires state outside the stream.
//!
//! # Structure
//!
//! Events form a two-level enum: the top level classifies by concern
//! (lifecycle / model / tool), and each category owns its variants. In
//! serialized form the category appears under `"kind"` and the kind under
//! `"event"` (the bus envelope [`crate::SynonzEvent`] adds the outer
//! `"type"` = the entity family):
//!
//! ```
//! use synonz::{TurnEvent, LifecycleEvent};
//!
//! let event = TurnEvent::Lifecycle(LifecycleEvent::Started {
//!     input: "weather in beijing?".into(),
//! });
//! let json = serde_json::to_value(&event).unwrap();
//! assert_eq!(json["kind"], "lifecycle");
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
//!   [`CallPurpose::Reasoning`] to the next; the round number is carried
//!   explicitly in the payload (`round`) — consumers never derive it.

use crate::error::AgentError;
use crate::io::AgentOutput;
use crate::message::{CallId, Message, ToolCall, ToolResult};
use serde::{Deserialize, Serialize};

/// The top-level event classification by concern.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnEvent {
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
/// carry `round: None` — see [`ModelEvent`]).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPurpose {
    /// Part of the reasoning loop (round boundary marker).
    Reasoning,
    /// Context management such as summarization.
    ContextManagement,
    /// Classification such as intent routing (reserved for S3).
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

/// The reasoning-loop round an event belongs to.
///
/// One-based, matching the loop's round counter. `None` marks a call
/// outside the reasoning loop (auxiliary calls such as summarization) —
/// mutually confirming with [`CallPurpose::ContextManagement`].
pub type Round = Option<usize>;

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
        /// The reasoning round this call belongs to (`None` for auxiliary
        /// calls outside the loop).
        round: Round,
    },
    /// A streamed response fragment.
    StreamDelta {
        /// The delta fragment.
        delta: ModelDelta,
        /// The reasoning round this delta belongs to.
        round: Round,
    },
    /// The model produced a complete response.
    Responded {
        /// The complete response message (assistant).
        message: Message,
        /// Token accounting for this call.
        usage: TokenUsage,
        /// The reasoning round this response belongs to.
        round: Round,
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
        /// The reasoning round whose response issued the call.
        round: Round,
    },
    /// A tool invocation finished (success or soft failure).
    CallCompleted {
        /// Correlation id of the answered call.
        call_id: CallId,
        /// The tool outcome; `Err` is fed back to the model.
        result: ToolResult,
        /// The reasoning round whose response issued the call.
        round: Round,
    },
}

/// Which stage of the background lifecycle failed.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryFlowStage {
    /// Reading a memory layer during background assembly.
    AssembleRead,
    /// Writing the L1 archive after a completed turn.
    Archive,
    /// Updating the session topic.
    TopicUpdate,
    /// Summarizing demoted L1 turns into L2.
    Summarize,
    /// Distilling L2 overflow into L3.
    Distill,
}

/// The product-narrative event of the execution face.
///
/// A filtered projection of [`TurnEvent`]: input-side payloads
/// (`Started` / `Requested` / `Responded`) stay on the observation
/// bypass; everything a product consumer renders surfaces here. The
/// terminal invariant carries over — a terminal variant is always the
/// last item, and the stream closes after it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExecutionEvent {
    /// A text delta as the model streams.
    Delta(ModelDelta),
    /// The model issued a tool call.
    ToolRequested(ToolCall),
    /// A tool invocation finished (success or soft failure).
    ToolCompleted {
        /// Correlation id of the answered call.
        call_id: CallId,
        /// The tool outcome; `Err` was fed back to the model.
        result: ToolResult,
    },
    /// The run failed; the stream closes after this.
    Failed(AgentError),
    /// The run was cancelled; the stream closes after this.
    Cancelled(CancelReason),
    /// The run completed, carrying the final output — the stream is
    /// self-sufficient: no extra await is required.
    Completed(AgentOutput),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_reason_displays() {
        assert_eq!(CancelReason::UserRequested.to_string(), "user requested");
        assert_eq!(CancelReason::Timeout.to_string(), "timeout");
    }

    #[test]
    fn turn_event_serializes_with_kind_and_event_tags() {
        let event = TurnEvent::Lifecycle(LifecycleEvent::Cancelled {
            reason: CancelReason::Timeout,
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "lifecycle");
        assert_eq!(json["event"], "cancelled");
        assert_eq!(json["reason"], "timeout");
    }

    #[test]
    fn round_travels_on_model_events() {
        let event = TurnEvent::Model(ModelEvent::Requested {
            purpose: CallPurpose::Reasoning,
            messages: Vec::new(),
            round: Some(2),
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "model");
        assert_eq!(json["event"], "requested");
        assert_eq!(json["round"], 2);
    }
}
