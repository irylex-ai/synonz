//! The memory management contract: entries, views, and the item-level
//! abstraction.
//!
//! Memory is the subject-owned abstraction of interaction. The framework
//! defines the **entry-level** contract only — a stable id, the content,
//! the partition it belongs to, and freshness stamps — plus the management
//! verbs over it ([`Memory::list`] / [`Memory::get`] / [`Memory::edit`] /
//! [`Memory::forget`] / [`Memory::forget_matching`]). What memory *is*
//! (layers, semantic types, storage) belongs to the implementation behind
//! [`Memory`]; the framework guarantees the entry semantics and the
//! management facts.
//!
//! Two audiences, two views over one abstraction:
//!
//! - [`Memory`] is the **application face**: item-level management over
//!   layer-agnostic [`MemoryItem`]s, reached through
//!   [`SynonzRuntime::memory`](crate::SynonzRuntime::memory);
//! - [`MemoryReader`] is the **read-only projection** the framework hands
//!   to context assemblers through their payloads.
//!
//! Entries are addressed by a framework-opaque id (stable across edits) and
//! partitioned by an opaque [`MemoryScope`] — the framework does not
//! interpret the scope's value; it only groups and filters by equality. The
//! listing order is freshness (`max(updated_at, created_at)`) descending,
//! id ascending; cursors are values, so they stay correct when entries are
//! deleted between pages.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Subject;

/// A memory entry's topic tag.
pub type Topic = String;

/// An opaque memory partition identifier.
///
/// The framework treats the value as opaque: it groups and filters by
/// equality, never interpreting what it means. Applications define the
/// partitions their memory lives in (for example `"project:abc"` or
/// `"user:u1"`); a memory entry's identity is `(subject, scope, …)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemoryScope(String);

impl MemoryScope {
    /// Creates a scope from its opaque value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Views the scope's opaque value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for MemoryScope {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for MemoryScope {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl core::fmt::Display for MemoryScope {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Memory-store failures (bridging and storage machinery; soft where the
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

/// One ordering position of keyset pagination: an entry's freshness stamp
/// plus id.
///
/// The listing order is **freshness (`max(updated_at, created_at)`)
/// descending, id ascending**; a cursor resumes strictly after that
/// position. Being a value (not an offset), it stays correct when entries
/// are deleted between pages.
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

/// The application listing cursor: a flat list position.
///
/// `position: None` means "the start of the listing".
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryListCursor {
    /// The position inside the listing (`None` = the start).
    pub position: Option<MemoryCursor>,
}

impl MemoryListCursor {
    /// Creates a cursor at a position.
    pub fn new(position: MemoryCursor) -> Self {
        Self {
            position: Some(position),
        }
    }

    /// Creates a cursor at the start of the listing.
    pub fn start() -> Self {
        Self { position: None }
    }
}

/// One page of a memory listing.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryPage<T, C = MemoryCursor> {
    /// The page's items, in listing order (up to the requested limit).
    pub items: Vec<T>,
    /// The cursor to resume from when the store observed at least one more
    /// matching entry; `None` when the page is the last.
    pub next: Option<C>,
}

impl<T, C> MemoryPage<T, C> {
    /// Creates a page (implementations build these when answering list
    /// queries).
    pub fn new(items: Vec<T>, next: Option<C>) -> Self {
        Self { items, next }
    }
}

/// The listing/search query: filters + keyset pagination.
///
/// `scope: None` lists every partition merged; `topic: None` does not
/// filter by topic.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryQuery {
    /// Restrict to one conversation (`None` matches all).
    pub conversation_id: Option<String>,
    /// Restrict to one topic (`None` matches all).
    pub topic: Option<Topic>,
    /// Restrict to one partition (`None` = all partitions merged).
    pub scope: Option<MemoryScope>,
    /// Restrict by freshness `>= from` (`None` = unbounded).
    pub from: Option<u64>,
    /// Restrict by freshness `< to` (`None` = unbounded).
    pub to: Option<u64>,
    /// Case-insensitive substring over the content; `None` matches all.
    pub keyword: Option<String>,
    /// Resume strictly after this position; `None` starts from the top.
    pub after: Option<MemoryListCursor>,
    /// The maximum number of items to return (`list`) / entries to remove
    /// (`forget_matching`); must be positive — `0` yields an empty last
    /// page / no removals.
    pub limit: usize,
}

impl MemoryQuery {
    /// Creates a query returning up to `limit` items.
    pub fn new(limit: usize) -> Self {
        Self {
            conversation_id: None,
            topic: None,
            scope: None,
            from: None,
            to: None,
            keyword: None,
            after: None,
            limit,
        }
    }

    /// Restricts to one conversation.
    pub fn with_conversation(mut self, conversation_id: impl Into<String>) -> Self {
        self.conversation_id = Some(conversation_id.into());
        self
    }

    /// Restricts to one topic.
    pub fn with_topic(mut self, topic: impl Into<Topic>) -> Self {
        self.topic = Some(topic.into());
        self
    }

    /// Restricts to one partition.
    pub fn with_scope(mut self, scope: impl Into<MemoryScope>) -> Self {
        self.scope = Some(scope.into());
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

/// Where one memory item came from (its most recent write).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySource {
    /// The conversation that produced the item.
    pub conversation_id: String,
    /// The topic carried at that time (may be empty).
    pub topic: Topic,
}

impl MemorySource {
    /// Creates a source record.
    pub fn new(conversation_id: impl Into<String>, topic: impl Into<Topic>) -> Self {
        Self {
            conversation_id: conversation_id.into(),
            topic: topic.into(),
        }
    }
}

/// One memory item as the application sees it: layer-agnostic, with a
/// stable id, its partition, and freshness stamps.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryItem {
    /// The framework-generated opaque id (stable across edits).
    pub id: String,
    /// The item's content.
    pub content: String,
    /// Where the item came from.
    pub source: MemorySource,
    /// The partition the item belongs to.
    pub scope: MemoryScope,
    /// Epoch seconds at which the item was created.
    pub created_at: u64,
    /// Epoch seconds at which the item was last written.
    pub updated_at: u64,
}

impl MemoryItem {
    /// Creates an item view (implementations build these when answering
    /// reads).
    pub fn new(
        id: impl Into<String>,
        content: impl Into<String>,
        source: MemorySource,
        scope: MemoryScope,
        created_at: u64,
        updated_at: u64,
    ) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            source,
            scope,
            created_at,
            updated_at,
        }
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

impl MemoryForgetFailure {
    /// Creates a failure record.
    pub fn new(id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            reason: reason.into(),
        }
    }
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

impl MemoryForgetResult {
    /// Creates a forget result (implementations build these when answering
    /// forget calls).
    pub fn new(removed: usize, failures: Vec<MemoryForgetFailure>) -> Self {
        Self { removed, failures }
    }
}

/// The item-level management abstraction: the application face of a memory
/// implementation, produced by its provider and reached through
/// [`SynonzRuntime::memory`](crate::SynonzRuntime::memory).
///
/// Implementations own the memory semantics (layers, types, storage); the
/// framework owns the entry contract and the management facts:
///
/// - the subject is passed per call (one implementation serves every
///   subject);
/// - entries are addressed by id; `get` / `edit` / `forget` take the id,
///   the scope is carried by the entry;
/// - writes are in-place: an edit keeps the id, source and creation time
///   and refreshes the freshness stamp;
/// - every successful edit emits [`crate::MemoryEvent::Updated`] and every
///   successful removal emits [`crate::MemoryEvent::Removed`] (content-free,
///   with the entry's scope) — implementations report management actions
///   through the bus they hold.
pub trait Memory: Send + Sync + 'static {
    /// Lists items matching the query (freshness descending, id ascending;
    /// `scope: None` merges every partition).
    fn list(
        &self,
        subject: &Subject,
        query: MemoryQuery,
    ) -> Result<MemoryPage<MemoryItem, MemoryListCursor>, MemoryStoreError>;

    /// Fetches one item by id (`None` when absent).
    fn get(&self, subject: &Subject, id: &str) -> Result<Option<MemoryItem>, MemoryStoreError>;

    /// Corrects an item in place: the content is replaced; the id, source
    /// and creation time stay, and the freshness stamp refreshes.
    fn edit(
        &self,
        subject: &Subject,
        id: &str,
        content: &str,
    ) -> Result<MemoryItem, MemoryStoreError>;

    /// Forgets one item by id (idempotent: removing an absent id reports
    /// `removed: 0`, not an error).
    fn forget(&self, subject: &Subject, id: &str) -> Result<MemoryForgetResult, MemoryStoreError>;

    /// Forgets the matching items, best-effort: the query's `limit` caps
    /// the batch; a single failure does not abort the rest.
    fn forget_matching(
        &self,
        subject: &Subject,
        query: MemoryQuery,
    ) -> Result<MemoryForgetResult, MemoryStoreError>;
}

/// A read-only projection of a [`Memory`] implementation: the same
/// item-level reads, without the write verbs.
///
/// The projection is derived from the memory itself
/// ([`dyn Memory::reader`]); the framework hands it to context assemblers
/// through their payloads. A read-only strategy cannot write or curate
/// through it — the write verbs do not exist here.
#[derive(Clone, Copy)]
pub struct MemoryReader<'a> {
    memory: &'a dyn Memory,
}

impl<'a> MemoryReader<'a> {
    /// Builds the projection (framework-internal: `dyn Memory::reader` is
    /// the sole construction site).
    pub(crate) fn new(memory: &'a dyn Memory) -> Self {
        Self { memory }
    }

    /// Lists items matching the query (freshness descending, id ascending;
    /// `scope: None` merges every partition).
    pub fn list(
        &self,
        subject: &Subject,
        query: MemoryQuery,
    ) -> Result<MemoryPage<MemoryItem, MemoryListCursor>, MemoryStoreError> {
        self.memory.list(subject, query)
    }

    /// Fetches one item by id (`None` when absent).
    pub fn get(&self, subject: &Subject, id: &str) -> Result<Option<MemoryItem>, MemoryStoreError> {
        self.memory.get(subject, id)
    }
}

impl dyn Memory {
    /// The read-only projection of this implementation: the item-level
    /// reads without the write verbs.
    ///
    /// The projection is always a faithful view over the implementation it
    /// is derived from; the framework puts it into context-assembly
    /// payloads so strategies read the state without being able to change
    /// it.
    pub fn reader(&self) -> MemoryReader<'_> {
        MemoryReader::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_an_opaque_string_value() {
        let scope = MemoryScope::new("project:abc");
        assert_eq!(scope.as_str(), "project:abc");
        assert_eq!(scope.to_string(), "project:abc");
        let from_string: MemoryScope = "user:u1".to_string().into();
        assert_eq!(from_string.as_str(), "user:u1");
        let json = serde_json::to_string(&scope).unwrap();
        assert_eq!(json, "\"project:abc\"");
        let back: MemoryScope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, scope);
    }

    #[test]
    fn query_builders_compose() {
        let query = MemoryQuery::new(5)
            .with_conversation("c1")
            .with_topic("billing")
            .with_scope("project:abc")
            .with_from(10)
            .with_to(20)
            .with_keyword("invoice")
            .with_after(MemoryListCursor::new(MemoryCursor::new(15, "e1")));
        assert_eq!(query.limit, 5);
        assert_eq!(query.conversation_id.as_deref(), Some("c1"));
        assert_eq!(query.topic.as_deref(), Some("billing"));
        assert_eq!(
            query.scope.as_ref().map(MemoryScope::as_str),
            Some("project:abc")
        );
        assert_eq!((query.from, query.to), (Some(10), Some(20)));
        assert_eq!(query.keyword.as_deref(), Some("invoice"));
        assert_eq!(
            query.after.as_ref().and_then(|c| c.position.as_ref()),
            Some(&MemoryCursor::new(15, "e1"))
        );
    }

    #[test]
    fn list_cursor_start_has_no_position() {
        assert!(MemoryListCursor::start().position.is_none());
        assert_eq!(
            MemoryListCursor::new(MemoryCursor::new(1, "a")).position,
            Some(MemoryCursor::new(1, "a"))
        );
    }
}
