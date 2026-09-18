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
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Subject;

/// A memory fragment's topic tag.
pub type Topic = String;

/// The identity triple locating one memory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct L3Identity {
    /// The owning subject (full `(type, id)` identity).
    pub subject_id: String,
    /// The conversation the entry came from.
    pub conversation_id: String,
    /// The entry's topic.
    pub topic: Topic,
}

/// One L3 long-term knowledge entry.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L3Entry {
    /// The framework-generated opaque id (stable across edits).
    /// Entries from older serialized data get one on load.
    #[serde(default = "generate_id")]
    pub id: String,
    /// Where and under what topic this knowledge came from.
    pub identity: L3Identity,
    /// The distilled knowledge (a fact, preference, or conclusion).
    pub content: String,
    /// Epoch seconds at which the entry was created.
    pub created_at: u64,
    /// Epoch seconds at which the entry was last written — the freshness
    /// key (list ordering, pagination cursors, recall ranking). Entries
    /// from older serialized data carry 0; order by
    /// `max(updated_at, created_at)`.
    #[serde(default)]
    pub updated_at: u64,
}

impl L3Entry {
    /// Creates a knowledge entry: the id is generated and the
    /// creation/update times are stamped automatically.
    pub fn new(identity: L3Identity, content: impl Into<String>) -> Self {
        let now = now_epoch();
        Self {
            id: generate_id(),
            identity,
            content: content.into(),
            created_at: now,
            updated_at: now,
        }
    }

    /// The freshness stamp used for ordering (`max(updated_at, created_at)`).
    pub(crate) fn freshness(&self) -> u64 {
        self.updated_at.max(self.created_at)
    }
}

/// An L2 summary entry of this conversation's earlier turns.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L2Entry {
    /// The framework-generated opaque id (stable across edits).
    /// Entries from older serialized data get one on load.
    #[serde(default = "generate_id")]
    pub id: String,
    /// The conversation this summary belongs to.
    pub conversation_id: String,
    /// The summarized content.
    pub content: String,
    /// Sequence order among summary entries (oldest first).
    pub index: u64,
    /// Epoch seconds at which the entry was created.
    pub created_at: u64,
    /// Epoch seconds at which the entry was last written — the freshness
    /// key (list ordering, pagination cursors, recall ranking). Entries
    /// from older serialized data carry 0; order by
    /// `max(updated_at, created_at)`.
    #[serde(default)]
    pub updated_at: u64,
}

impl L2Entry {
    /// Creates an L2 entry: the id is generated and the creation/update
    /// times are stamped automatically.
    pub fn new(conversation_id: impl Into<String>, content: impl Into<String>, index: u64) -> Self {
        let now = now_epoch();
        Self {
            id: generate_id(),
            conversation_id: conversation_id.into(),
            content: content.into(),
            index,
            created_at: now,
            updated_at: now,
        }
    }

    /// The freshness stamp used for ordering (`max(updated_at, created_at)`).
    pub(crate) fn freshness(&self) -> u64 {
        self.updated_at.max(self.created_at)
    }
}

/// A turn recorded in L1.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L1Entry {
    /// The conversation this turn belongs to.
    pub conversation_id: String,
    /// The turn's topic (inherited from the conversation topic state machine).
    pub topic: Topic,
    /// The canonical messages of that turn.
    pub messages: Vec<crate::Message>,
    /// Epoch seconds at which the entry was created (recency ranking).
    pub created_at: u64,
}

impl L1Entry {
    /// Creates an L1 entry (used by the framework and custom L1 stores;
    /// the creation time is stamped automatically).
    pub fn new(
        conversation_id: impl Into<String>,
        topic: impl Into<String>,
        messages: Vec<crate::Message>,
    ) -> Self {
        Self {
            conversation_id: conversation_id.into(),
            topic: topic.into(),
            messages,
            created_at: now_epoch(),
        }
    }
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
    /// The requested entry id does not exist (update/edit paths).
    #[error("memory entry not found: {0}")]
    EntryNotFound(String),
}

/// One ordering position of the store-level keyset pagination: the
/// entry's freshness stamp + id.
///
/// The listing order is **freshness (`max(updated_at, created_at)`)
/// descending, id ascending**; a cursor resumes strictly after that
/// position. Being a value (not an offset), it stays correct when
/// entries are deleted between pages.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCursor {
    /// The entry's freshness stamp (`max(updated_at, created_at)`).
    pub updated_at: u64,
    /// The entry's id (the ascending tiebreak).
    pub id: String,
}

impl MemoryCursor {
    /// Creates a cursor at the given position.
    pub fn new(updated_at: u64, id: impl Into<String>) -> Self {
        Self {
            updated_at,
            id: id.into(),
        }
    }
}

/// One page of a store listing.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryPage<T, C = MemoryCursor> {
    /// The page's items, in listing order (up to the requested limit).
    pub items: Vec<T>,
    /// The cursor to resume from when the store observed at least one
    /// more matching entry; `None` when the page is the last.
    pub next: Option<C>,
}

impl<T, C> MemoryPage<T, C> {
    /// Creates a page (store implementations build these when answering
    /// list queries).
    pub fn new(items: Vec<T>, next: Option<C>) -> Self {
        Self { items, next }
    }
}

/// The store-level listing query: filters + keyset pagination.
///
/// This is the **single-source** form: the store's layer implies its
/// memory type; the application face composes per-type pages into the
/// unified listing.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryStoreQuery {
    /// Restrict to one conversation (`None` matches all).
    pub conversation_id: Option<String>,
    /// Restrict by freshness `>= from` (`None` = unbounded).
    pub from: Option<u64>,
    /// Restrict by freshness `< to` (`None` = unbounded).
    pub to: Option<u64>,
    /// Case-insensitive substring over the content; `None` matches all.
    pub keyword: Option<String>,
    /// Resume strictly after this position; `None` starts from the top.
    pub after: Option<MemoryCursor>,
    /// The maximum number of entries to return (must be positive;
    /// `0` yields an empty last page).
    pub limit: usize,
}

impl MemoryStoreQuery {
    /// Creates a query returning up to `limit` entries.
    pub fn new(limit: usize) -> Self {
        Self {
            conversation_id: None,
            from: None,
            to: None,
            keyword: None,
            after: None,
            limit,
        }
    }

    /// Filters to one conversation.
    pub fn with_conversation(mut self, conversation_id: impl Into<String>) -> Self {
        self.conversation_id = Some(conversation_id.into());
        self
    }

    /// Filters by freshness (lower bound, inclusive).
    pub fn with_from(mut self, from: u64) -> Self {
        self.from = Some(from);
        self
    }

    /// Filters by freshness (upper bound, exclusive).
    pub fn with_to(mut self, to: u64) -> Self {
        self.to = Some(to);
        self
    }

    /// Filters by a case-insensitive content substring.
    pub fn with_keyword(mut self, keyword: impl Into<String>) -> Self {
        self.keyword = Some(keyword.into());
        self
    }

    /// Resumes after a cursor (a previous page's `next`).
    pub fn with_after(mut self, cursor: MemoryCursor) -> Self {
        self.after = Some(cursor);
        self
    }
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
    fn append(&self, subject: &Subject, block: L2Entry) -> Result<(), MemoryStoreError>;

    /// The conversation's L2 summary blocks, oldest first.
    fn read(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<L2Entry>, MemoryStoreError>;

    /// How many L2 blocks a conversation currently holds.
    fn len(&self, subject: &Subject, conversation_id: &str) -> Result<usize, MemoryStoreError>;

    /// Removes the oldest `n` L2 blocks of a conversation and returns
    /// them (for distillation into L3).
    fn pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<L2Entry>, MemoryStoreError>;

    /// Fetches one entry by its stable id (`None` when absent).
    fn get(&self, subject: &Subject, id: &str) -> Result<Option<L2Entry>, MemoryStoreError>;

    /// Replaces the entry carrying the same id (the caller supplies the
    /// full entry; the id is the stable address). Returns `false` when
    /// no such id exists.
    fn update(&self, subject: &Subject, entry: L2Entry) -> Result<bool, MemoryStoreError>;

    /// Removes the entry by id. Idempotent: returns `false` when absent.
    fn remove(&self, subject: &Subject, id: &str) -> Result<bool, MemoryStoreError>;

    /// Lists entries with filters and keyset pagination (freshness
    /// descending, id ascending).
    fn list(
        &self,
        subject: &Subject,
        query: MemoryStoreQuery,
    ) -> Result<MemoryPage<L2Entry>, MemoryStoreError>;
}

/// The L3 storage contract: the knowledge layer (cross-conversation,
/// distilled).
///
/// Implementations own the retrieval *logic* — topic matching, semantic
/// search, hybrids — not just storage.
pub trait MemoryL3Store: Send + Sync + 'static {
    /// Upserts an L3 knowledge entry: same-identity entries are replaced
    /// **preserving the original id** (the id is the entry's stable
    /// address across updates); otherwise the entry is appended.
    fn upsert(&self, subject: &Subject, fragment: L3Entry) -> Result<(), MemoryStoreError>;

    /// Retrieves relevant L3 fragments for the query.
    fn query(
        &self,
        subject: &Subject,
        query: &str,
        topic: &Topic,
        budget: usize,
    ) -> Result<Vec<L3Entry>, MemoryStoreError>;

    /// The subject's complete L3 fragment count (introspection for
    /// budgeting and diagnostics).
    fn len(&self, subject: &Subject) -> Result<usize, MemoryStoreError>;

    /// Fetches one entry by its stable id (`None` when absent).
    fn get(&self, subject: &Subject, id: &str) -> Result<Option<L3Entry>, MemoryStoreError>;

    /// Replaces the entry carrying the same id (the caller supplies the
    /// full entry; the id is the stable address). Returns `false` when
    /// no such id exists.
    fn update(&self, subject: &Subject, entry: L3Entry) -> Result<bool, MemoryStoreError>;

    /// Removes the entry by id. Idempotent: returns `false` when absent.
    fn remove(&self, subject: &Subject, id: &str) -> Result<bool, MemoryStoreError>;

    /// Lists entries with filters and keyset pagination (freshness
    /// descending, id ascending).
    fn list(
        &self,
        subject: &Subject,
        query: MemoryStoreQuery,
    ) -> Result<MemoryPage<L3Entry>, MemoryStoreError>;
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
    pub fn l2_append(&self, subject: &Subject, block: L2Entry) -> Result<(), MemoryStoreError> {
        self.l2.append(subject, block)
    }

    /// The conversation's L2 summary blocks, oldest first.
    pub fn l2_read(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<L2Entry>, MemoryStoreError> {
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
    ) -> Result<Vec<L2Entry>, MemoryStoreError> {
        self.l2.pop_oldest(subject, conversation_id, n)
    }

    /// Upserts an L3 knowledge fragment.
    pub fn l3_upsert(&self, subject: &Subject, fragment: L3Entry) -> Result<(), MemoryStoreError> {
        self.l3.upsert(subject, fragment)
    }

    /// Retrieves relevant L3 fragments for the query.
    pub fn l3_query(
        &self,
        subject: &Subject,
        query: &str,
        topic: &Topic,
        budget: usize,
    ) -> Result<Vec<L3Entry>, MemoryStoreError> {
        self.l3.query(subject, query, topic, budget)
    }

    /// The subject's complete L3 fragment count.
    pub fn l3_len(&self, subject: &Subject) -> Result<usize, MemoryStoreError> {
        self.l3.len(subject)
    }

    /// A read-only, subject-scoped view of this memory: the strategy
    /// slots' material face. Writes and curation are not expressible
    /// through the view (compile-time rejection) — writes stay closed
    /// over the engine.
    pub fn reader<'a>(&'a self, subject: &'a Subject) -> MemoryReader<'a> {
        MemoryReader {
            memory: self,
            subject,
        }
    }
}

/// A read-only, subject-scoped view of the layered memory: the material
/// face strategy slots read through.
///
/// The view has no append/pop/upsert methods — a read-only strategy
/// cannot write or curate even by mistake (the stronger form of "curation
/// stays in the engine layer"); construct one through
/// [`Memory::reader`].
#[derive(Clone, Copy)]
pub struct MemoryReader<'a> {
    memory: &'a Memory,
    subject: &'a Subject,
}

impl<'a> MemoryReader<'a> {
    /// The conversation's recent L1 entries, oldest first.
    pub fn l1_window(&self, conversation_id: &str) -> Result<Vec<L1Entry>, MemoryStoreError> {
        self.memory.l1_window(self.subject, conversation_id)
    }

    /// How many L1 entries a conversation currently holds.
    pub fn l1_len(&self, conversation_id: &str) -> Result<usize, MemoryStoreError> {
        self.memory.l1_len(self.subject, conversation_id)
    }

    /// The conversation's L2 entries, oldest first.
    pub fn l2_read(&self, conversation_id: &str) -> Result<Vec<L2Entry>, MemoryStoreError> {
        self.memory.l2_read(self.subject, conversation_id)
    }

    /// How many L2 entries a conversation currently holds.
    pub fn l2_len(&self, conversation_id: &str) -> Result<usize, MemoryStoreError> {
        self.memory.l2_len(self.subject, conversation_id)
    }

    /// Retrieves relevant L3 entries for the query (budget-controlled).
    pub fn l3_query(
        &self,
        query: &str,
        topic: &Topic,
        budget: usize,
    ) -> Result<Vec<L3Entry>, MemoryStoreError> {
        self.memory.l3_query(self.subject, query, topic, budget)
    }

    /// The subject's complete L3 entry count.
    pub fn l3_len(&self) -> Result<usize, MemoryStoreError> {
        self.memory.l3_len(self.subject)
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Generates a framework entry id: process-unique without new
/// dependencies (epoch seconds + process id + atomic counter mixed with
/// the nanosecond remainder).
fn generate_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let nth = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{:x}-{:x}-{:x}",
        now.as_secs(),
        std::process::id(),
        nth ^ u64::from(now.subsec_nanos())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_construction_stamps_id_and_freshness() {
        let l2 = L2Entry::new("c1", "summary", 0);
        assert!(!l2.id.is_empty());
        assert_eq!(l2.updated_at, l2.created_at);
        assert_eq!(l2.freshness(), l2.created_at);

        let l3 = L3Entry::new(
            L3Identity {
                subject_id: "u".into(),
                conversation_id: "c1".into(),
                topic: "t".into(),
            },
            "knowledge",
        );
        assert!(!l3.id.is_empty());
        assert_eq!(l3.updated_at, l3.created_at);
    }

    #[test]
    fn legacy_serialized_entries_get_an_id_and_freshness_fallback() {
        // 0.5.0-era payloads carry neither id nor updated_at.
        let legacy =
            r#"{"conversation_id":"c1","content":"old summary","index":0,"created_at":42}"#;
        let l2: L2Entry = serde_json::from_str(legacy).unwrap();
        assert!(!l2.id.is_empty());
        assert_eq!(l2.updated_at, 0);
        assert_eq!(l2.freshness(), 42);

        let legacy = concat!(
            r#"{"identity":{"subject_id":"u","conversation_id":"c1","topic":"t"},"#,
            r#""content":"old","created_at":7}"#
        );
        let l3: L3Entry = serde_json::from_str(legacy).unwrap();
        assert!(!l3.id.is_empty());
        assert_eq!(l3.updated_at, 0);
        assert_eq!(l3.freshness(), 7);
    }
}
