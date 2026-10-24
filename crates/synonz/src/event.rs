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
///
/// `input_tokens` is the total input consumed by the request: adapters
/// normalize it so the cache read and cache write portions are included.
/// The cache fields break that input down; `None` means the provider did
/// not report the metric, while `Some(0)` is an explicit zero.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Total input tokens consumed by the request (cache read and cache
    /// write included).
    pub input_tokens: u64,
    /// Tokens produced by the response.
    pub output_tokens: u64,
    /// Input tokens served from the provider's prompt cache (`None` when
    /// the provider reports no cache metric).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    /// Input tokens written into the provider's prompt cache (`None` when
    /// the provider reports no cache metric).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_input_tokens: Option<u64>,
}

impl TokenUsage {
    /// Creates a usage record without cache accounting.
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
        }
    }

    /// Sets the input tokens served from the prompt cache.
    pub fn cache_read_input_tokens(mut self, tokens: Option<u64>) -> Self {
        self.cache_read_input_tokens = tokens;
        self
    }

    /// Sets the input tokens written into the prompt cache.
    pub fn cache_write_input_tokens(mut self, tokens: Option<u64>) -> Self {
        self.cache_write_input_tokens = tokens;
        self
    }

    /// Adds one call's usage into this total (per-round accumulation):
    /// counts sum, and a cache metric stays `None` only while both sides
    /// are unreported.
    pub(crate) fn accumulate(&mut self, other: TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens =
            add_reported(self.cache_read_input_tokens, other.cache_read_input_tokens);
        self.cache_write_input_tokens = add_reported(
            self.cache_write_input_tokens,
            other.cache_write_input_tokens,
        );
    }
}

/// Adds two optional reported counts: `None` means "not reported" and
/// contributes zero; the sum stays `None` only when both sides are
/// unreported.
fn add_reported(total: Option<u64>, increment: Option<u64>) -> Option<u64> {
    match (total, increment) {
        (None, None) => None,
        (total, increment) => Some(total.unwrap_or(0) + increment.unwrap_or(0)),
    }
}

/// An incremental piece of a streamed model response.
///
/// Response text and reasoning may interleave; tool calls arrive complete
/// in the finish message. Reasoning fragments are narration only — they
/// are not part of the canonical assistant message.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelDelta {
    /// A fragment of response text.
    Text {
        /// The text fragment.
        text: String,
    },
    /// A fragment of the model's reasoning output, when the provider
    /// streams it (never part of the canonical assistant message).
    Reasoning {
        /// The reasoning fragment.
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

    #[test]
    fn token_usage_cache_fields_round_trip() {
        let usage = TokenUsage::new(100, 20)
            .cache_read_input_tokens(Some(80))
            .cache_write_input_tokens(Some(10));
        let json = serde_json::to_value(usage).unwrap();
        assert_eq!(json["input_tokens"], 100);
        assert_eq!(json["cache_read_input_tokens"], 80);
        assert_eq!(json["cache_write_input_tokens"], 10);
        let back: TokenUsage = serde_json::from_value(json).unwrap();
        assert_eq!(back, usage);
    }

    #[test]
    fn token_usage_omits_unreported_cache_fields() {
        let usage = TokenUsage::new(10, 5);
        let json = serde_json::to_value(usage).unwrap();
        assert!(json.get("cache_read_input_tokens").is_none());
        assert!(json.get("cache_write_input_tokens").is_none());
        // A payload from before the fields existed restores as unreported.
        let legacy: TokenUsage =
            serde_json::from_value(serde_json::json!({"input_tokens": 10, "output_tokens": 5}))
                .unwrap();
        assert_eq!(legacy, TokenUsage::new(10, 5));
        assert_eq!(legacy.cache_read_input_tokens, None);
        assert_eq!(legacy.cache_write_input_tokens, None);
    }

    #[test]
    fn token_usage_accumulates_counts_and_reported_cache() {
        let mut total = TokenUsage::new(0, 0);
        total.accumulate(TokenUsage::new(10, 2));
        // Unreported on both sides stays unreported.
        assert_eq!(total.cache_read_input_tokens, None);
        // An explicit zero counts as reported.
        total.accumulate(TokenUsage::new(5, 1).cache_read_input_tokens(Some(0)));
        assert_eq!(total.cache_read_input_tokens, Some(0));
        total.accumulate(
            TokenUsage::new(7, 3)
                .cache_read_input_tokens(Some(4))
                .cache_write_input_tokens(Some(2)),
        );
        assert_eq!(
            total,
            TokenUsage::new(22, 6)
                .cache_read_input_tokens(Some(4))
                .cache_write_input_tokens(Some(2))
        );
    }
}
