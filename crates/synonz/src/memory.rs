//! The memory system: fragment model and the layered memory store
//! contracts.
//!
//! Memory is the subject-owned abstraction of interaction. Three layers,
//! each an independent storage contract (heterogeneous backends are the
//! norm: L1 in-process memory, L2 Redis, L3 vector stores — each slot
//! scales and swaps on its own):
//!
//! - **L1**: the current conversation's recent turns (verbatim);
//! - **L2**: summaries of this conversation's earlier turns (cached);
//! - **L3**: distilled long-term knowledge, cross-conversation.
//!
//! Every fragment is uniquely located by the triple
//! `(subject_id, conversation_id, topic)`. Orchestration (when flows
//! happen) belongs to the framework; storage and retrieval logic belongs
//! to the store implementations.
//!
//! [`Memory`] is the domain object on the usage side: one handle
//! aggregating the three slots, so callers read and write through a
//! single coherent facade (`memory.l1_append(..)`) while the storage
//! sides stay independently replaceable. It is assembled by the runtime
//! and never constructed by hand — the runtime is its single authority.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Subject;

/// A memory fragment's topic tag.
pub type Topic = String;

/// The identity triple locating one memory fragment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FragmentIdentity {
    /// The owning subject (full `(type, id)` identity).
    pub subject_id: String,
    /// The conversation the fragment came from.
    pub conversation_id: String,
    /// The fragment's topic.
    pub topic: Topic,
}

/// One L3 long-term knowledge fragment.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeFragment {
    /// Where and under what topic this knowledge came from.
    pub identity: FragmentIdentity,
    /// The distilled knowledge (a fact, preference, or conclusion).
    pub content: String,
    /// Epoch seconds at which the fragment was created (recency ranking).
    pub created_at: u64,
}

impl KnowledgeFragment {
    /// Creates a knowledge fragment.
    pub fn new(identity: FragmentIdentity, content: impl Into<String>) -> Self {
        Self {
            identity,
            content: content.into(),
            created_at: now_epoch(),
        }
    }
}

/// An L2 summary block of this conversation's earlier turns.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryBlock {
    /// The conversation this summary belongs to.
    pub conversation_id: String,
    /// The summarized content.
    pub content: String,
    /// Sequence order among summary blocks (oldest first).
    pub index: u64,
}

impl SummaryBlock {
    /// Creates a summary block.
    pub fn new(conversation_id: impl Into<String>, content: impl Into<String>, index: u64) -> Self {
        Self {
            conversation_id: conversation_id.into(),
            content: content.into(),
            index,
        }
    }
}

/// A turn recorded in L1.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L1Entry {
    /// The conversation this turn belongs to.
    pub conversation_id: String,
    /// The turn's topic (inherited from the session topic state machine).
    pub topic: Topic,
    /// The canonical messages of that turn.
    pub messages: Vec<crate::Message>,
}

/// Memory-store failures (bridging/storage machinery; soft where the
/// behavior model permits).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Error)]
pub enum MemoryStoreError {
    /// The backing storage failed.
    #[error("memory storage failure: {0}")]
    Storage(String),
    /// The requested subject was not found.
    #[error("subject not found: {0}")]
    SubjectNotFound(String),
}

/// The L1 storage contract: the working memory layer (recent turns,
/// verbatim).
///
/// Implementations own L1 *storage*: what to persist and how to fetch it.
/// The default expectation is memory-grade latency — L1 is the freshness
/// layer of the perceptual-latency gradient (see the crate architecture
/// documentation).
pub trait MemoryL1Store: Send + Sync + 'static {
    /// Appends one turn for the given conversation and topic.
    fn append(
        &self,
        subject: &Subject,
        conversation_id: &str,
        topic: &Topic,
        messages: Vec<crate::Message>,
    ) -> Result<(), MemoryStoreError>;

    /// The conversation's recent L1 turns, oldest first.
    fn window(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<L1Entry>, MemoryStoreError>;

    /// Removes and returns the oldest `n` L1 turns of a conversation
    /// (used by the framework's compaction flow).
    fn pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<L1Entry>, MemoryStoreError>;

    /// How many L1 turns a conversation currently holds.
    fn len(&self, subject: &Subject, conversation_id: &str) -> Result<usize, MemoryStoreError>;
}

/// The L2 storage contract: the summary layer (earlier turns, compressed).
pub trait MemoryL2Store: Send + Sync + 'static {
    /// Appends an L2 summary block.
    fn append(&self, subject: &Subject, block: SummaryBlock) -> Result<(), MemoryStoreError>;

    /// The conversation's L2 summary blocks, oldest first.
    fn read(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<SummaryBlock>, MemoryStoreError>;

    /// How many L2 blocks a conversation currently holds.
    fn len(&self, subject: &Subject, conversation_id: &str) -> Result<usize, MemoryStoreError>;

    /// Removes the oldest `n` L2 blocks of a conversation and returns
    /// them (for distillation into L3).
    fn pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<SummaryBlock>, MemoryStoreError>;
}

/// The L3 storage contract: the knowledge layer (cross-conversation,
/// distilled).
///
/// Implementations own the retrieval *logic* — topic matching, semantic
/// search, hybrids — not just storage.
pub trait MemoryL3Store: Send + Sync + 'static {
    /// Upserts an L3 knowledge fragment.
    fn upsert(
        &self,
        subject: &Subject,
        fragment: KnowledgeFragment,
    ) -> Result<(), MemoryStoreError>;

    /// Retrieves relevant L3 fragments for the query.
    fn query(
        &self,
        subject: &Subject,
        query: &str,
        topic: &Topic,
        budget: usize,
    ) -> Result<Vec<KnowledgeFragment>, MemoryStoreError>;

    /// The subject's complete L3 fragment count (introspection for
    /// budgeting and diagnostics).
    fn len(&self, subject: &Subject) -> Result<usize, MemoryStoreError>;
}

/// The layered memory as one domain object: the three storage slots
/// aggregated behind a single coherent facade.
///
/// Assembled by the runtime (its sole constructor); clones share the same
/// slots. Callers read and write through the facade methods while the
/// storage sides stay independently replaceable. **Boundary legislation**:
/// a store implementation must never curate (compact, distill, promote on
/// its own) — curation timing is driven by lifecycle facts and its
/// visibility is legislated in the state engine layer.
#[derive(Clone)]
pub struct Memory {
    l1: Arc<dyn MemoryL1Store>,
    l2: Arc<dyn MemoryL2Store>,
    l3: Arc<dyn MemoryL3Store>,
}

impl Memory {
    /// Assembles the facade from the three slots (runtime-only).
    pub(crate) fn new(
        l1: Arc<dyn MemoryL1Store>,
        l2: Arc<dyn MemoryL2Store>,
        l3: Arc<dyn MemoryL3Store>,
    ) -> Self {
        Self { l1, l2, l3 }
    }

    /// Appends one turn to L1 for the given conversation and topic.
    pub fn l1_append(
        &self,
        subject: &Subject,
        conversation_id: &str,
        topic: &Topic,
        messages: Vec<crate::Message>,
    ) -> Result<(), MemoryStoreError> {
        self.l1.append(subject, conversation_id, topic, messages)
    }

    /// The current conversation's recent L1 turns, oldest first.
    pub fn l1_window(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<L1Entry>, MemoryStoreError> {
        self.l1.window(subject, conversation_id)
    }

    /// Removes and returns the oldest `n` L1 turns of a conversation.
    pub fn l1_pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<L1Entry>, MemoryStoreError> {
        self.l1.pop_oldest(subject, conversation_id, n)
    }

    /// How many L1 turns a conversation currently holds.
    pub fn l1_len(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<usize, MemoryStoreError> {
        self.l1.len(subject, conversation_id)
    }

    /// Appends an L2 summary block.
    pub fn l2_append(
        &self,
        subject: &Subject,
        block: SummaryBlock,
    ) -> Result<(), MemoryStoreError> {
        self.l2.append(subject, block)
    }

    /// The conversation's L2 summary blocks, oldest first.
    pub fn l2_read(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<SummaryBlock>, MemoryStoreError> {
        self.l2.read(subject, conversation_id)
    }

    /// How many L2 blocks a conversation currently holds.
    pub fn l2_len(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<usize, MemoryStoreError> {
        self.l2.len(subject, conversation_id)
    }

    /// Removes the oldest `n` L2 blocks of a conversation and returns them.
    pub fn l2_pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<SummaryBlock>, MemoryStoreError> {
        self.l2.pop_oldest(subject, conversation_id, n)
    }

    /// Upserts an L3 knowledge fragment.
    pub fn l3_upsert(
        &self,
        subject: &Subject,
        fragment: KnowledgeFragment,
    ) -> Result<(), MemoryStoreError> {
        self.l3.upsert(subject, fragment)
    }

    /// Retrieves relevant L3 fragments for the query.
    pub fn l3_query(
        &self,
        subject: &Subject,
        query: &str,
        topic: &Topic,
        budget: usize,
    ) -> Result<Vec<KnowledgeFragment>, MemoryStoreError> {
        self.l3.query(subject, query, topic, budget)
    }

    /// The subject's complete L3 fragment count.
    pub fn l3_len(&self, subject: &Subject) -> Result<usize, MemoryStoreError> {
        self.l3.len(subject)
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
