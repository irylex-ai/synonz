//! Synonz core: agent, tool, and model contracts, the event model, canonical
//! messages, and the execution loop with first-class cancellation.
//!
//! # Layout
//!
//! - [`agent`] — the agent, its builder, and the run event stream.
//! - [`message`] — the canonical conversation form and its invariants.
//! - [`event`] — the run event model (the single ordered narrative of a
//!   run).
//! - [`error`] — run-level and model-call error types.
//! - [`io`] — run boundary types (input and final output).
//! - [`tool`] — the tool contract (capabilities).
//! - [`model`] — the model contract (LLM inference).
//! - `mock` — deterministic test doubles (feature `test-util`).
//!
//! Cancellation adopts the ecosystem primitive:
//! [`CancellationToken`] is re-exported here as the framework's cancellation
//! currency.
//!
//! All commonly used types are re-exported at the crate root.

pub mod agent;
pub mod bus;
pub mod context;
pub mod conversation;
pub mod error;
pub mod event;
pub mod inprocess;
pub mod io;
pub mod memory;
pub mod message;
pub mod model;
pub mod runtime;
pub mod scheduler;
pub mod subject;
pub mod tool;

mod cancel;

#[cfg(feature = "test-util")]
pub mod mock;

pub use agent::{Agent, AgentBuilder, DEFAULT_MAX_ROUNDS, Execution};
pub use bus::{
    ConversationEndReason, ConversationEvent, EventBus, EventSink, MemoryEvent,
    MemoryFlowFailedMoment, Observer, ObserverContext, SynonzEvent,
};
pub use context::{
    AssemblyFailure, Context, ContextAssembler, ContextAssemblerInput, ContextAssemblerOutput,
    ConversationTopicDetector, MemoryDistiller, MemoryFlowError, MemorySummarizer, TopicDecision,
    TurnInputRewriter,
};
pub use conversation::{
    Conversation, ConversationCursor, ConversationPage, ConversationQuery, ConversationState,
    ConversationStore, ConversationStoreError, ConversationSummary, Turn, TurnInput, TurnOutcome,
};
pub use error::{AgentError, ModelError};
pub use event::{
    CallPurpose, CancelReason, ExecutionEvent, LifecycleEvent, MemoryFlowStage, ModelDelta,
    ModelEvent, TokenUsage, ToolEvent, TurnEvent,
};
pub use io::{AgentInput, AgentOutput};
pub use memory::{
    L1Entry, L2Entry, L3Entry, L3Identity, Memory, MemoryCursor, MemoryForgetFailure,
    MemoryForgetResult, MemoryItem, MemoryL1Store, MemoryL2Store, MemoryL3Store, MemoryListCursor,
    MemoryPage, MemoryQuery, MemoryReader, MemorySource, MemoryStoreError, MemoryStoreQuery,
    MemoryType, Topic,
};
pub use message::{
    CallId, CanonicalViolation, ContentBlock, Message, Role, ToolCall, ToolContent, ToolResult,
    validate_conversation,
};
pub use model::{Model, ModelParams, ModelRequest, ModelStream, ModelStreamItem, complete};
pub use runtime::{ConversationTaskSpawner, RuntimeBuilder, SynonzRuntime};
pub use scheduler::{OverlapPolicy, Schedule, Scheduler, TaskHandle, TaskInfo};
pub use subject::{Subject, SubjectType};
pub use tokio_util::sync::CancellationToken;
pub use tool::{Tool, ToolContext, ToolError, ToolSpec};

#[cfg(feature = "test-util")]
pub use mock::MockModel;

// ── Derive macro and companion re-exports ──
//
// `#[derive(Tool)]` generates code referencing these paths, so downstream
// crates implement tools with `synonz` as their only dependency.

/// The derive macro behind `#[derive(Tool)]`.
///
/// Generates the [`Tool`] implementation for a struct whose fields are the
/// tool's arguments. See the macro's documentation for the contract.
pub use synonz_derive::Tool;

/// Boxed future used by the [`Tool`] and [`Model`] contracts.
///
/// A stable alias so downstream implementors do not need to depend on
/// `futures` directly.
pub type BoxFuture<'a, T> = futures::future::BoxFuture<'a, T>;

/// JSON Schema generation (re-exported for `#[derive(Tool)]` companions).
pub use schemars::schema_for;

/// The `JsonSchema` derive (re-exported for `#[derive(Tool)]` companions).
pub use schemars::JsonSchema;

/// Serialization derives (re-exported for `#[derive(Tool)]` companions).
pub use serde::{Deserialize, Serialize};

/// JSON serialization (re-exported; generated tool code references it).
pub use serde_json;
