//! The memory system: fragment model and the layered memory store
//! contract (ADR-0012).
//!
//! Memory is the subject-owned abstraction of interaction. Three layers:
//!
//! - **L1**: the current conversation's recent turns (verbatim);
//! - **L2**: summaries of this conversation's earlier turns (cached);
//! - **L3**: distilled long-term knowledge, cross-conversation.
//!
//! Every fragment is uniquely located by the triple
//! `(subject_id, conversation_id, topic)`. Orchestration (when flows
//! happen) belongs to the framework; storage and retrieval logic belongs
//! to [`MemoryStore`] implementations.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Subject;

/// A memory fragment's topic tag.
pub type Topic = String;

/// The identity triple locating one memory fragment (ADR-0012 §5.2).
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

/// The layered memory contract.
///
/// Implementations own *storage and retrieval logic*: what to persist,
/// how to persist it, and which L3 fragments a retrieval returns.
/// The framework owns orchestration: when turns are written, when
/// summaries are generated, when demotions and promotions happen.
pub trait MemoryStore: Send + Sync + 'static {
    /// Appends one turn to L1 for the given conversation and topic.
    fn l1_append(
        &self,
        subject: &Subject,
        conversation_id: &str,
        topic: &Topic,
        messages: Vec<crate::Message>,
    ) -> Result<(), MemoryStoreError>;

    /// The current conversation's recent L1 turns, oldest first.
    fn l1_window(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<L1Entry>, MemoryStoreError>;

    /// Removes and returns the oldest `n` L1 turns of a conversation
    /// (used by the framework's demotion flow).
    fn l1_pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<L1Entry>, MemoryStoreError>;

    /// How many L1 turns a conversation currently holds.
    fn l1_len(&self, subject: &Subject, conversation_id: &str) -> Result<usize, MemoryStoreError>;

    /// Appends an L2 summary block.
    fn l2_append(&self, subject: &Subject, block: SummaryBlock) -> Result<(), MemoryStoreError>;

    /// The conversation's L2 summary blocks, oldest first.
    fn l2_read(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<SummaryBlock>, MemoryStoreError>;

    /// How many L2 blocks a conversation currently holds.
    fn l2_len(&self, subject: &Subject, conversation_id: &str) -> Result<usize, MemoryStoreError>;

    /// Removes the oldest `n` L2 blocks of a conversation and returns
    /// them (for distillation into L3).
    fn l2_pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<SummaryBlock>, MemoryStoreError>;

    /// Upserts an L3 knowledge fragment.
    fn l3_upsert(
        &self,
        subject: &Subject,
        fragment: KnowledgeFragment,
    ) -> Result<(), MemoryStoreError>;

    /// Retrieves relevant L3 fragments for the query (the retrieval
    /// *logic* — topic matching, semantic search, hybrids — lives here).
    fn l3_retrieve(
        &self,
        subject: &Subject,
        query: &str,
        topic: &Topic,
        budget: usize,
    ) -> Result<Vec<KnowledgeFragment>, MemoryStoreError>;

    /// The subject's complete L3 fragment count (introspection for
    /// budgeting and diagnostics).
    fn l3_len(&self, subject: &Subject) -> Result<usize, MemoryStoreError>;
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
