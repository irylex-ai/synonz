//! Synonz core: agent, tool, and model contracts, the event model, canonical
//! messages, and the execution loop with first-class cancellation.
//!
//! # Layout
//!
//! - [`message`] — the canonical conversation form and its invariants.
//! - [`event`] — the run event model (the single ordered narrative of a
//!   run).
//! - [`error`] — run-level and model-call error types.
//! - [`io`] — run boundary types (input and final output).
//! - [`tool`] — the tool contract (capabilities).
//! - [`model`] — the model contract (LLM inference).
//!
//! Cancellation adopts the ecosystem primitive:
//! [`CancellationToken`] is re-exported here as the framework's cancellation
//! currency.
//!
//! All commonly used types are re-exported at the crate root.

pub mod error;
pub mod event;
pub mod io;
pub mod message;
pub mod model;
pub mod tool;

mod cancel;

pub use error::{AgentError, ModelError};
pub use event::{
    AgentEvent, CallPurpose, CancelReason, LifecycleEvent, ModelDelta, ModelEvent, TokenUsage,
    ToolEvent,
};
pub use io::{AgentInput, AgentOutput};
pub use message::{
    CallId, CanonicalViolation, ContentBlock, Message, Role, ToolCall, ToolContent, ToolResult,
    validate_conversation,
};
pub use model::{Model, ModelParams, ModelRequest, ModelStream, ModelStreamItem, complete};
pub use tokio_util::sync::CancellationToken;
pub use tool::{Tool, ToolContext, ToolError, ToolSpec};
