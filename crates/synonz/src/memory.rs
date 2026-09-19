//! The memory system: entry model and the layered memory store
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
//! Entries carry framework-generated stable ids (the management address);
//! the `(subject_id, conversation_id, topic)` triple remains the L3
//! same-identity key (an upsert replaces that slot while preserving the
//! id). Orchestration (when flows happen) belongs to the framework;
//! storage and retrieval logic belongs to the store implementations.
//!
//! Two audiences, two views over one mechanism:
//!
//! - [`Memory`] is the **application face**: item-level management
//!   (`list` / `get` / `edit` / `forget` / `forget_matching`) over
//!   layer-agnostic [`MemoryItem`]s;
//! - [`MemoryReader`] is the **strategy material face**: the read-only,
//!   subject-scoped view handed to strategy slots through their payloads.
//!
//! Both sit on the crate-internal mechanism (the three storage slots
//! aggregated), assembled by the runtime — nobody constructs either by
//! hand; the runtime is the single authority.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Subject;
use crate::bus::{EventBus, MemoryEvent, SynonzEvent};

/// A memory entry's topic tag.
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
    /// The topic active when the summary was produced (empty for legacy
    /// payloads and test fixtures).
    #[serde(default)]
    pub topic: Topic,
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
            topic: String::new(),
            content: content.into(),
            index,
            created_at: now,
            updated_at: now,
        }
    }

    /// Sets the topic active when the summary was produced.
    pub fn with_topic(mut self, topic: impl Into<String>) -> Self {
        self.topic = topic.into();
        self
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
    /// Epoch seconds at which the entry was created.
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

    /// Removes and returns the oldest `n` L2 blocks of a conversation
    /// (a positional storage primitive; the framework's distillation
    /// claims its sources by id instead).
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
    fn upsert(&self, subject: &Subject, entry: L3Entry) -> Result<(), MemoryStoreError>;

    /// Retrieves relevant L3 entries for the query.
    fn query(
        &self,
        subject: &Subject,
        query: &str,
        topic: &Topic,
        budget: usize,
    ) -> Result<Vec<L3Entry>, MemoryStoreError>;

    /// The subject's complete L3 entry count (introspection for
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

/// The memory item discriminator: episodic summaries (the L2 layer) vs
/// long-term knowledge (the L3 layer).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    /// A summary of a conversation's earlier turns (L2).
    Summary,
    /// Distilled long-term knowledge — a fact, preference, or
    /// conclusion (L3).
    Knowledge,
}

/// Where one memory item came from.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySource {
    /// The conversation that produced the item.
    pub conversation_id: String,
    /// The topic carried at that time (may be empty for legacy payloads).
    pub topic: Topic,
}

/// One memory item as the application sees it: layer-agnostic, with a
/// stable id and freshness stamps.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryItem {
    /// The framework-generated opaque id (stable across edits).
    pub id: String,
    /// Which type of memory entry this is.
    pub memory_type: MemoryType,
    /// The item's content (a summary text or a knowledge text).
    pub content: String,
    /// Where the item came from.
    pub source: MemorySource,
    /// Epoch seconds at which the item was created.
    pub created_at: u64,
    /// Epoch seconds at which the item was last written.
    pub updated_at: u64,
}

/// The application-level keyset cursor: the current phase and the
/// single-source position inside it.
///
/// The listing order is structural — the `Summary` block first, then the
/// `Knowledge` block, each in freshness-descending order. `position:
/// None` means "the start of the phase".
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryListCursor {
    /// The phase (which block) this cursor points into.
    pub memory_type: MemoryType,
    /// The position inside the phase (`None` = phase start).
    pub position: Option<MemoryCursor>,
}

impl MemoryListCursor {
    /// Creates a cursor at a position inside a phase.
    pub fn new(memory_type: MemoryType, position: MemoryCursor) -> Self {
        Self {
            memory_type,
            position: Some(position),
        }
    }

    /// Creates a cursor at the start of a phase.
    pub fn start(memory_type: MemoryType) -> Self {
        Self {
            memory_type,
            position: None,
        }
    }
}

/// The application listing/search query: filters + keyset pagination.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryQuery {
    /// Restrict to one memory type (`None` = the unified listing).
    pub memory_type: Option<MemoryType>,
    /// Restrict to one conversation (`None` matches all).
    pub conversation_id: Option<String>,
    /// Restrict by freshness `>= from` (`None` = unbounded).
    pub from: Option<u64>,
    /// Restrict by freshness `< to` (`None` = unbounded).
    pub to: Option<u64>,
    /// Case-insensitive substring over the content; `None` matches all.
    pub keyword: Option<String>,
    /// Resume strictly after this position; `None` starts from the top.
    pub after: Option<MemoryListCursor>,
    /// The maximum number of items to return (`list`) / entries to
    /// remove (`forget_matching`); must be positive — `0` yields an empty
    /// last page / no removals.
    pub limit: usize,
}

impl MemoryQuery {
    /// Creates a query returning up to `limit` items.
    pub fn new(limit: usize) -> Self {
        Self {
            memory_type: None,
            conversation_id: None,
            from: None,
            to: None,
            keyword: None,
            after: None,
            limit,
        }
    }

    /// Restricts to one memory type.
    pub fn with_memory_type(mut self, memory_type: MemoryType) -> Self {
        self.memory_type = Some(memory_type);
        self
    }

    /// Restricts to one conversation.
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
    pub fn with_after(mut self, cursor: MemoryListCursor) -> Self {
        self.after = Some(cursor);
        self
    }
}

/// One failed forget step: the entry id and the failure reason
/// (content-free).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryForgetFailure {
    /// The entry that could not be removed.
    pub id: String,
    /// Why the removal failed.
    pub reason: String,
}

/// The result of a forget call (single or batch): how many entries were
/// removed plus per-entry failures. Batches run best-effort — a single
/// failure does not abort the rest.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryForgetResult {
    /// How many entries were removed.
    pub removed: usize,
    /// The entries that could not be removed.
    pub failures: Vec<MemoryForgetFailure>,
}

/// The crate-internal memory mechanism: the three storage slots
/// aggregated, exposing the layer primitives the framework's maintenance
/// needs and the by-id operations the management face uses. Cheap to
/// clone (three `Arc`s). External code cannot construct or reach it:
/// applications go through [`Memory`] (the management face), strategy
/// slots through [`MemoryReader`].
///
/// **Boundary legislation**: a store implementation must never curate
/// (compact, distill, promote on its own) — curation timing is driven by
/// lifecycle facts and its visibility is legislated in the state engine
/// layer.
#[derive(Clone)]
pub(crate) struct MemoryLayerStore {
    l1: Arc<dyn MemoryL1Store>,
    l2: Arc<dyn MemoryL2Store>,
    l3: Arc<dyn MemoryL3Store>,
}

impl MemoryLayerStore {
    /// Assembles the mechanism from the three slots (runtime-only).
    pub(crate) fn new(
        l1: Arc<dyn MemoryL1Store>,
        l2: Arc<dyn MemoryL2Store>,
        l3: Arc<dyn MemoryL3Store>,
    ) -> Self {
        Self { l1, l2, l3 }
    }

    // ── layer primitives (framework-internal) ──

    /// Appends one turn to L1 for the given conversation and topic.
    pub(crate) fn l1_append(
        &self,
        subject: &Subject,
        conversation_id: &str,
        topic: &Topic,
        messages: Vec<crate::Message>,
    ) -> Result<(), MemoryStoreError> {
        self.l1.append(subject, conversation_id, topic, messages)
    }

    /// The current conversation's recent L1 turns, oldest first.
    pub(crate) fn l1_window(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<L1Entry>, MemoryStoreError> {
        self.l1.window(subject, conversation_id)
    }

    /// Removes and returns the oldest `n` L1 turns of a conversation.
    pub(crate) fn l1_pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<L1Entry>, MemoryStoreError> {
        self.l1.pop_oldest(subject, conversation_id, n)
    }

    /// How many L1 turns a conversation currently holds.
    pub(crate) fn l1_len(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<usize, MemoryStoreError> {
        self.l1.len(subject, conversation_id)
    }

    /// Appends an L2 summary block.
    pub(crate) fn l2_append(
        &self,
        subject: &Subject,
        block: L2Entry,
    ) -> Result<(), MemoryStoreError> {
        self.l2.append(subject, block)
    }

    /// The conversation's L2 summary blocks, oldest first.
    pub(crate) fn l2_read(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<L2Entry>, MemoryStoreError> {
        self.l2.read(subject, conversation_id)
    }

    /// How many L2 blocks a conversation currently holds.
    pub(crate) fn l2_len(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<usize, MemoryStoreError> {
        self.l2.len(subject, conversation_id)
    }

    /// Upserts an L3 knowledge entry (same-identity replace preserves the id).
    pub(crate) fn l3_upsert(
        &self,
        subject: &Subject,
        entry: L3Entry,
    ) -> Result<(), MemoryStoreError> {
        self.l3.upsert(subject, entry)
    }

    /// Retrieves relevant L3 entries for the query.
    pub(crate) fn l3_query(
        &self,
        subject: &Subject,
        query: &str,
        topic: &Topic,
        budget: usize,
    ) -> Result<Vec<L3Entry>, MemoryStoreError> {
        self.l3.query(subject, query, topic, budget)
    }

    /// The subject's complete L3 entry count.
    pub(crate) fn l3_len(&self, subject: &Subject) -> Result<usize, MemoryStoreError> {
        self.l3.len(subject)
    }

    // ── by-id operations (ADR-0020; management face + maintenance) ──

    /// Fetches one L2 entry by id.
    pub(crate) fn l2_get(
        &self,
        subject: &Subject,
        id: &str,
    ) -> Result<Option<L2Entry>, MemoryStoreError> {
        self.l2.get(subject, id)
    }

    /// Replaces one L2 entry by id (`false` when absent).
    pub(crate) fn l2_update(
        &self,
        subject: &Subject,
        entry: L2Entry,
    ) -> Result<bool, MemoryStoreError> {
        self.l2.update(subject, entry)
    }

    /// Removes one L2 entry by id (idempotent).
    pub(crate) fn l2_remove(&self, subject: &Subject, id: &str) -> Result<bool, MemoryStoreError> {
        self.l2.remove(subject, id)
    }

    /// Lists L2 entries (filters + keyset pagination).
    pub(crate) fn l2_list(
        &self,
        subject: &Subject,
        query: MemoryStoreQuery,
    ) -> Result<MemoryPage<L2Entry>, MemoryStoreError> {
        self.l2.list(subject, query)
    }

    /// Fetches one L3 entry by id.
    pub(crate) fn l3_get(
        &self,
        subject: &Subject,
        id: &str,
    ) -> Result<Option<L3Entry>, MemoryStoreError> {
        self.l3.get(subject, id)
    }

    /// Replaces one L3 entry by id (`false` when absent).
    pub(crate) fn l3_update(
        &self,
        subject: &Subject,
        entry: L3Entry,
    ) -> Result<bool, MemoryStoreError> {
        self.l3.update(subject, entry)
    }

    /// Removes one L3 entry by id (idempotent).
    pub(crate) fn l3_remove(&self, subject: &Subject, id: &str) -> Result<bool, MemoryStoreError> {
        self.l3.remove(subject, id)
    }

    /// Lists L3 entries (filters + keyset pagination).
    pub(crate) fn l3_list(
        &self,
        subject: &Subject,
        query: MemoryStoreQuery,
    ) -> Result<MemoryPage<L3Entry>, MemoryStoreError> {
        self.l3.list(subject, query)
    }

    /// The read-only, subject-scoped strategy view (framework-internal
    /// construction; the engine hands it to strategy slots through their
    /// payloads).
    pub(crate) fn reader<'a>(&'a self, subject: &'a Subject) -> MemoryReader<'a> {
        MemoryReader {
            layers: self,
            subject,
        }
    }
}

/// The application face: memory item management (view, correct, forget)
/// over the crate-internal mechanism. Assembled by the runtime; clones
/// share the same mechanism. Strategy slots never see this type — they
/// receive [`MemoryReader`] through their payloads.
#[derive(Clone)]
pub struct Memory {
    layers: MemoryLayerStore,
    bus: EventBus,
}

impl Memory {
    /// Wraps the internal mechanism and the fact outlet (runtime-only).
    pub(crate) fn new(layers: MemoryLayerStore, bus: EventBus) -> Self {
        Self { layers, bus }
    }

    /// The internal mechanism (framework-internal access for the engine,
    /// runtime and background maintenance tasks).
    pub(crate) fn layers(&self) -> &MemoryLayerStore {
        &self.layers
    }

    /// Lists memory items: the `Summary` block first, then the
    /// `Knowledge` block, each in freshness-descending order; a page that
    /// is not filled continues into the next block (the boundary is
    /// crossed once). `memory_type` restricts to a single block.
    pub fn list(
        &self,
        subject: &Subject,
        query: MemoryQuery,
    ) -> Result<MemoryPage<MemoryItem, MemoryListCursor>, MemoryStoreError> {
        let limit = query.limit;
        if limit == 0 {
            return Ok(MemoryPage::new(Vec::new(), None));
        }
        let mut phase = query
            .after
            .as_ref()
            .map(|cursor| cursor.memory_type)
            .or(query.memory_type)
            .unwrap_or(MemoryType::Summary);
        let mut position = query.after.as_ref().and_then(|c| c.position.clone());
        let mut items: Vec<MemoryItem> = Vec::with_capacity(limit);

        loop {
            if query.memory_type.is_some_and(|kind| kind != phase) {
                break;
            }
            let store_query = MemoryStoreQuery {
                conversation_id: query.conversation_id.clone(),
                from: query.from,
                to: query.to,
                keyword: query.keyword.clone(),
                after: position.take(),
                limit: limit - items.len(),
            };
            let (page, mapped): (Option<MemoryCursor>, Vec<MemoryItem>) = match phase {
                MemoryType::Summary => {
                    let page = self.layers.l2_list(subject, store_query)?;
                    (page.next, page.items.into_iter().map(l2_item).collect())
                }
                MemoryType::Knowledge => {
                    let page = self.layers.l3_list(subject, store_query)?;
                    (page.next, page.items.into_iter().map(l3_item).collect())
                }
            };
            let store_has_more = page.is_some();
            items.extend(mapped);

            if items.len() == limit {
                let last = items
                    .last()
                    .map(|item| {
                        MemoryCursor::new(item.updated_at.max(item.created_at), item.id.clone())
                    })
                    .expect("a filled page has at least one item");
                let next = if store_has_more {
                    Some(MemoryListCursor::new(phase, last))
                } else if phase == MemoryType::Summary && query.memory_type.is_none() {
                    // The summary block ended exactly at the page boundary;
                    // peek the knowledge block to report an accurate `next`.
                    let peek = MemoryStoreQuery {
                        conversation_id: query.conversation_id.clone(),
                        from: query.from,
                        to: query.to,
                        keyword: query.keyword.clone(),
                        after: None,
                        limit: 1,
                    };
                    if self.layers.l3_list(subject, peek)?.items.is_empty() {
                        None
                    } else {
                        Some(MemoryListCursor::start(MemoryType::Knowledge))
                    }
                } else {
                    None
                };
                return Ok(MemoryPage::new(items, next));
            }

            // The current block is exhausted before filling the page:
            // continue into the next block (once), or finish.
            if phase == MemoryType::Summary && query.memory_type.is_none() {
                phase = MemoryType::Knowledge;
                position = None;
                continue;
            }
            return Ok(MemoryPage::new(items, None));
        }

        Ok(MemoryPage::new(items, None))
    }

    /// Fetches one item by id (`None` when absent).
    pub fn get(&self, subject: &Subject, id: &str) -> Result<Option<MemoryItem>, MemoryStoreError> {
        if let Some(entry) = self.layers.l2_get(subject, id)? {
            return Ok(Some(l2_item(entry)));
        }
        Ok(self.layers.l3_get(subject, id)?.map(l3_item))
    }

    /// Corrects an item in place: the content is replaced, the id, type,
    /// source and creation time stay, and the freshness stamp refreshes.
    pub fn edit(
        &self,
        subject: &Subject,
        id: &str,
        content: impl Into<String>,
    ) -> Result<MemoryItem, MemoryStoreError> {
        let content = content.into();
        if let Some(mut entry) = self.layers.l2_get(subject, id)? {
            entry.content = content;
            entry.updated_at = now_epoch();
            let updated = entry.clone();
            if !self.layers.l2_update(subject, updated)? {
                return Err(MemoryStoreError::EntryNotFound(id.to_string()));
            }
            let item = l2_item(entry);
            self.emit_updated(subject, MemoryType::Summary, &item.id);
            return Ok(item);
        }
        if let Some(mut entry) = self.layers.l3_get(subject, id)? {
            entry.content = content;
            entry.updated_at = now_epoch();
            let updated = entry.clone();
            if !self.layers.l3_update(subject, updated)? {
                return Err(MemoryStoreError::EntryNotFound(id.to_string()));
            }
            let item = l3_item(entry);
            self.emit_updated(subject, MemoryType::Knowledge, &item.id);
            return Ok(item);
        }
        Err(MemoryStoreError::EntryNotFound(id.to_string()))
    }

    /// Forgets one item by id (idempotent: removing an absent id reports
    /// `removed: 0`, not an error).
    pub fn forget(
        &self,
        subject: &Subject,
        id: &str,
    ) -> Result<MemoryForgetResult, MemoryStoreError> {
        let removed_type = if self.layers.l2_remove(subject, id)? {
            Some(MemoryType::Summary)
        } else if self.layers.l3_remove(subject, id)? {
            Some(MemoryType::Knowledge)
        } else {
            None
        };
        if let Some(memory_type) = removed_type {
            self.emit_removed(subject, memory_type, vec![id.to_string()]);
            Ok(MemoryForgetResult {
                removed: 1,
                failures: Vec::new(),
            })
        } else {
            Ok(MemoryForgetResult {
                removed: 0,
                failures: Vec::new(),
            })
        }
    }

    /// Forgets the matching items, best-effort: the query's `limit` caps
    /// the batch; a single failure does not abort the rest.
    pub fn forget_matching(
        &self,
        subject: &Subject,
        query: MemoryQuery,
    ) -> Result<MemoryForgetResult, MemoryStoreError> {
        let budget = query.limit;
        let mut result = MemoryForgetResult {
            removed: 0,
            failures: Vec::new(),
        };
        if budget == 0 {
            return Ok(result);
        }
        let mut phase = query.memory_type.unwrap_or(MemoryType::Summary);
        let mut cursor: Option<MemoryCursor> = query
            .after
            .as_ref()
            .and_then(|entry| entry.position.clone());
        let mut removed_summaries: Vec<String> = Vec::new();
        let mut removed_knowledge: Vec<String> = Vec::new();
        loop {
            if query.memory_type.is_some_and(|kind| kind != phase) {
                break;
            }
            let remaining = budget - result.removed;
            if remaining == 0 {
                break;
            }
            let store_query = MemoryStoreQuery {
                conversation_id: query.conversation_id.clone(),
                from: query.from,
                to: query.to,
                keyword: query.keyword.clone(),
                after: cursor.take(),
                limit: remaining,
            };
            let next = match phase {
                MemoryType::Summary => {
                    let page = self.layers.l2_list(subject, store_query)?;
                    for entry in page.items {
                        let id = entry.id.clone();
                        match self.layers.l2_remove(subject, &id) {
                            Ok(true) => {
                                result.removed += 1;
                                removed_summaries.push(id);
                            }
                            Ok(false) => {}
                            Err(error) => result.failures.push(MemoryForgetFailure {
                                id,
                                reason: error.to_string(),
                            }),
                        }
                    }
                    page.next
                }
                MemoryType::Knowledge => {
                    let page = self.layers.l3_list(subject, store_query)?;
                    for entry in page.items {
                        let id = entry.id.clone();
                        match self.layers.l3_remove(subject, &id) {
                            Ok(true) => {
                                result.removed += 1;
                                removed_knowledge.push(id);
                            }
                            Ok(false) => {}
                            Err(error) => result.failures.push(MemoryForgetFailure {
                                id,
                                reason: error.to_string(),
                            }),
                        }
                    }
                    page.next
                }
            };
            match next {
                Some(cursor_next) => cursor = Some(cursor_next),
                None => {
                    if phase == MemoryType::Summary && query.memory_type.is_none() {
                        phase = MemoryType::Knowledge;
                        cursor = None;
                    } else {
                        break;
                    }
                }
            }
        }
        self.emit_removed(subject, MemoryType::Summary, removed_summaries);
        self.emit_removed(subject, MemoryType::Knowledge, removed_knowledge);
        Ok(result)
    }

    /// Emits one management update fact (bus only, content-free).
    fn emit_updated(&self, subject: &Subject, memory_type: MemoryType, id: &str) {
        self.bus.emit(SynonzEvent::Memory(MemoryEvent::Updated {
            subject_id: subject.to_string(),
            memory_type,
            id: id.to_string(),
        }));
    }

    /// Emits one management removal fact (bus only, content-free).
    fn emit_removed(&self, subject: &Subject, memory_type: MemoryType, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        self.bus.emit(SynonzEvent::Memory(MemoryEvent::Removed {
            subject_id: subject.to_string(),
            memory_type,
            ids,
        }));
    }
}

/// Projects one L2 entry into the application item view.
fn l2_item(entry: L2Entry) -> MemoryItem {
    MemoryItem {
        id: entry.id,
        memory_type: MemoryType::Summary,
        content: entry.content,
        source: MemorySource {
            conversation_id: entry.conversation_id,
            topic: entry.topic,
        },
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

/// Projects one L3 entry into the application item view.
fn l3_item(entry: L3Entry) -> MemoryItem {
    MemoryItem {
        id: entry.id,
        memory_type: MemoryType::Knowledge,
        content: entry.content,
        source: MemorySource {
            conversation_id: entry.identity.conversation_id,
            topic: entry.identity.topic,
        },
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

/// Tests and fixtures only: the framework seeds and inspects the
/// internal mechanism through these entry points.
#[cfg(feature = "test-util")]
impl Memory {
    /// Seeds one L1 turn (tests/fixtures only).
    pub fn seed_l1(
        &self,
        subject: &Subject,
        conversation_id: &str,
        topic: &str,
        messages: Vec<crate::Message>,
    ) -> Result<(), MemoryStoreError> {
        self.layers
            .l1_append(subject, conversation_id, &topic.to_string(), messages)
    }

    /// Seeds one L2 summary entry (tests/fixtures only).
    pub fn seed_l2(&self, subject: &Subject, entry: L2Entry) -> Result<(), MemoryStoreError> {
        self.layers.l2_append(subject, entry)
    }

    /// Seeds one L3 knowledge entry (tests/fixtures only).
    pub fn seed_l3(&self, subject: &Subject, entry: L3Entry) -> Result<(), MemoryStoreError> {
        self.layers.l3_upsert(subject, entry)
    }

    /// A read-only view for testing strategy slots (tests only).
    pub fn reader_for_tests<'a>(&'a self, subject: &'a Subject) -> MemoryReader<'a> {
        self.layers.reader(subject)
    }

    /// How many L1 turns a conversation holds (tests only).
    pub fn l1_len_for_tests(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<usize, MemoryStoreError> {
        self.layers.l1_len(subject, conversation_id)
    }

    /// How many L2 entries a conversation holds (tests only).
    pub fn l2_len_for_tests(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<usize, MemoryStoreError> {
        self.layers.l2_len(subject, conversation_id)
    }

    /// How many L3 entries the subject holds (tests only).
    pub fn l3_len_for_tests(&self, subject: &Subject) -> Result<usize, MemoryStoreError> {
        self.layers.l3_len(subject)
    }
}

/// A read-only, subject-scoped view of the layered memory: the material
/// face strategy slots read through.
///
/// The view has no append/pop/upsert methods — a read-only strategy
/// cannot write or curate even by mistake (the stronger form of "curation
/// stays in the engine layer"). The framework constructs it internally
/// and hands it to strategy slots through their payloads
/// ([`ContextAssemblerInput`](crate::ContextAssemblerInput),
/// [`MemoryDistiller::distill`](crate::MemoryDistiller::distill)).
#[derive(Clone, Copy)]
pub struct MemoryReader<'a> {
    layers: &'a MemoryLayerStore,
    subject: &'a Subject,
}

impl<'a> MemoryReader<'a> {
    /// The conversation's recent L1 entries, oldest first.
    pub fn l1_window(&self, conversation_id: &str) -> Result<Vec<L1Entry>, MemoryStoreError> {
        self.layers.l1_window(self.subject, conversation_id)
    }

    /// How many L1 entries a conversation currently holds.
    pub fn l1_len(&self, conversation_id: &str) -> Result<usize, MemoryStoreError> {
        self.layers.l1_len(self.subject, conversation_id)
    }

    /// The conversation's L2 entries, oldest first.
    pub fn l2_read(&self, conversation_id: &str) -> Result<Vec<L2Entry>, MemoryStoreError> {
        self.layers.l2_read(self.subject, conversation_id)
    }

    /// How many L2 entries a conversation currently holds.
    pub fn l2_len(&self, conversation_id: &str) -> Result<usize, MemoryStoreError> {
        self.layers.l2_len(self.subject, conversation_id)
    }

    /// Retrieves relevant L3 entries for the query (budget-controlled).
    pub fn l3_query(
        &self,
        query: &str,
        topic: &Topic,
        budget: usize,
    ) -> Result<Vec<L3Entry>, MemoryStoreError> {
        self.layers.l3_query(self.subject, query, topic, budget)
    }

    /// The subject's complete L3 entry count.
    pub fn l3_len(&self) -> Result<usize, MemoryStoreError> {
        self.layers.l3_len(self.subject)
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
