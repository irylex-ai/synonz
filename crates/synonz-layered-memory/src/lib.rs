//! The official layered memory component for Synonz.
//!
//! The component implements the framework's memory contract family with a
//! three-layer model:
//!
//! - **L1 working memory**: one entry per completed turn (the user's input
//!   plus the final answer), conversation-scoped and bounded to the most
//!   recent turns — the recall unit is the whole turn;
//! - **L2 event summaries**: conversation entries produced by **batch
//!   compaction** of L1 (when the window fills, the topic shifts, or the
//!   conversation ends); each entry keeps its current content, the history
//!   of previous contents, and an importance used by recall ranking;
//! - **L3 long-term memory**: a cross-conversation entity graph plus entity
//!   vectors, distilled on topic shifts and conversation ends; entity and
//!   relation types are constrained by the configured [`L3MemorySchema`]
//!   (the component hardcodes no domain vocabulary).
//!
//! The storage contracts ([`L1MemoryStore`] / [`L2MemoryStore`] /
//! [`L3MemoryGraphStore`] / [`L3MemoryVectorStore`]), the embedding port
//! ([`Embedding`]), and the semantic strategies ([`L2MemorySummarizer`] /
//! [`L3MemoryEntityExtractor`] / [`LayeredMemoryContextRewriter`]) are
//! replaceable; every piece ships a bundled default, so the component works
//! out of the box and scales by swapping pieces.
//!
//! # Registration
//!
//! ```no_run
//! use synonz::{Agent, SynonzRuntime};
//! use synonz_layered_memory::LayeredMemoryProvider;
//!
//! # fn demo(model: impl synonz::Model + 'static) {
//! let memory = LayeredMemoryProvider::builder().build();
//! let runtime = SynonzRuntime::builder()
//!     .memory_provider(LayeredMemoryProvider::builder().build())
//!     .build();
//! let agent = Agent::builder()
//!     .runtime(&runtime)
//!     .model(model)
//!     .rewriter_provider(memory.rewriter_provider())
//!     .topic_detector_provider(memory.topic_detector_provider())
//!     .build()
//!     .unwrap();
//! # }
//! ```
//!
//! The component's own observation face ([`LayeredMemoryObserver`]) reports
//! layered progress (compaction, distillation, normalization, skips)
//! without touching the core event bus; failures keep flowing through the
//! framework as `Failed` facts.

mod assembler;
mod config;
mod embedding;
mod l1_memory;
mod l2_memory;
mod l3_memory;
mod memory;
mod observation;
mod pipeline;
mod provider;
mod rewriter;
mod topic_detector;
mod utils;

pub use assembler::{
    LayeredMemoryContextRewriteInput, LayeredMemoryContextRewriter,
    LayeredMemoryPromptContextRewriter,
};
pub use config::{LayeredMemoryConfig, MemoryScopeResolver};
pub use embedding::{Embedding, HashEmbedding};
pub use l1_memory::{InProcessL1MemoryStore, L1MemoryEntry, L1MemoryStore};
pub use l2_memory::{
    InProcessL2MemoryStore, L2MemoryEntry, L2MemoryPromptSummarizer, L2MemoryStore,
    L2MemorySummarizer, L2MemorySummary, L2MemorySummaryInput, L2MemorySummaryOutput,
};
pub use l3_memory::{
    InProcessL3MemoryGraphStore, InProcessL3MemoryVectorStore, L3MemoryEntityExtractor,
    L3MemoryExtractionInput, L3MemoryGraph, L3MemoryGraphEdge, L3MemoryGraphEntity,
    L3MemoryGraphStore, L3MemoryPromptEntityExtractor, L3MemorySchema, L3MemoryVectorStore,
};
pub use memory::LayeredMemoryActuator;
pub use observation::{LayeredMemoryEvent, LayeredMemoryObserver};
pub use provider::{LayeredMemoryProvider, LayeredMemoryProviderBuilder};
pub use rewriter::CoreferenceInputRewriterProvider;
pub use topic_detector::SimilarityTopicDetectorProvider;
