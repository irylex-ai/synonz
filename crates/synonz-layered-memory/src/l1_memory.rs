//! L1 working memory: one entry per completed turn (the user's input plus
//! the final answer), conversation-scoped and bounded to the most recent
//! turns.
//!
//! Entries are the recall unit: a hit brings back the whole turn, so the
//! context stays coherent ("question + answer" pairs). The agent side keeps
//! the final answer only — tool results and intermediate messages live in
//! the conversation's truth archive, not in memory.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use synonz::{MemoryScope, MemoryStoreError};

use crate::utils::now_epoch;

/// One working-memory entry: one completed turn.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct L1MemoryEntry {
    /// The entry's stable id (generated at creation).
    pub id: String,
    /// The conversation partition the turn belongs to.
    pub scope: MemoryScope,
    /// The conversation topic at archive time.
    pub topic: String,
    /// The turn's user input (the memory-bearing side).
    pub input: String,
    /// The turn's final answer (tool results and intermediate messages are
    /// not part of working memory).
    pub response: String,
    /// Epoch seconds at which the turn was archived.
    pub created_at: u64,
}

impl L1MemoryEntry {
    /// Creates an entry (the id is generated here; stores only persist it).
    pub fn new(
        scope: MemoryScope,
        topic: impl Into<String>,
        input: impl Into<String>,
        response: impl Into<String>,
        created_at: u64,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            scope,
            topic: topic.into(),
            input: input.into(),
            response: response.into(),
            created_at,
        }
    }
}

/// The L1 storage contract: the working-memory window's persistence, so the
/// bundled in-process implementation can be replaced (Redis, ...).
///
/// Writes take the data (the entry carries its scope); reads and removals
/// take the partition. Implementations enforce their own turn capacity.
pub trait L1MemoryStore: Send + Sync + 'static {
    /// Appends one turn (the implementation evicts the oldest turns beyond
    /// its capacity).
    fn append(&self, entry: L1MemoryEntry) -> Result<(), MemoryStoreError>;

    /// The most recent `turns` entries of one partition, oldest first.
    fn recent(
        &self,
        scope: &MemoryScope,
        turns: usize,
    ) -> Result<Vec<L1MemoryEntry>, MemoryStoreError>;

    /// How many entries the partition holds.
    fn count(&self, scope: &MemoryScope) -> Result<usize, MemoryStoreError>;

    /// Removes one entry by id (idempotent).
    fn remove(&self, scope: &MemoryScope, id: &str) -> Result<bool, MemoryStoreError>;

    /// Clears the partition.
    fn clear(&self, scope: &MemoryScope) -> Result<(), MemoryStoreError>;
}

/// The bundled in-process L1 store: one queue per partition with
/// turn-capacity FIFO eviction.
pub struct InProcessL1MemoryStore {
    retained_turns: usize,
    entries: Mutex<HashMap<String, VecDeque<L1MemoryEntry>>>,
}

impl InProcessL1MemoryStore {
    /// Creates the store with a turn capacity.
    pub fn new(retained_turns: usize) -> Self {
        Self {
            retained_turns,
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl L1MemoryStore for InProcessL1MemoryStore {
    fn append(&self, entry: L1MemoryEntry) -> Result<(), MemoryStoreError> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let queue = entries.entry(entry.scope.to_string()).or_default();
        queue.push_back(entry);
        while queue.len() > self.retained_turns {
            queue.pop_front();
        }
        Ok(())
    }

    fn recent(
        &self,
        scope: &MemoryScope,
        turns: usize,
    ) -> Result<Vec<L1MemoryEntry>, MemoryStoreError> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let Some(queue) = entries.get(scope.as_str()) else {
            return Ok(Vec::new());
        };
        let start = queue.len().saturating_sub(turns);
        Ok(queue.iter().skip(start).cloned().collect())
    }

    fn count(&self, scope: &MemoryScope) -> Result<usize, MemoryStoreError> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        Ok(entries.get(scope.as_str()).map(VecDeque::len).unwrap_or(0))
    }

    fn remove(&self, scope: &MemoryScope, id: &str) -> Result<bool, MemoryStoreError> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let Some(queue) = entries.get_mut(scope.as_str()) else {
            return Ok(false);
        };
        let before = queue.len();
        queue.retain(|entry| entry.id != id);
        Ok(queue.len() != before)
    }

    fn clear(&self, scope: &MemoryScope) -> Result<(), MemoryStoreError> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.remove(scope.as_str());
        Ok(())
    }
}

/// The component's L1 domain object (internal): archiving completed turns
/// and reading the working-memory window.
pub(crate) struct L1Memory {
    store: Arc<dyn L1MemoryStore>,
}

impl L1Memory {
    /// Builds the layer over its store.
    pub(crate) fn new(store: Arc<dyn L1MemoryStore>) -> Self {
        Self { store }
    }

    /// Archives one completed turn (one entry).
    pub(crate) fn archive_turn(
        &self,
        scope: &MemoryScope,
        topic: &str,
        input: &str,
        response: &str,
    ) -> Result<(), MemoryStoreError> {
        self.store.append(L1MemoryEntry::new(
            scope.clone(),
            topic,
            input,
            response,
            now_epoch(),
        ))
    }

    /// The most recent `turns` entries of one partition, oldest first.
    pub(crate) fn entries(
        &self,
        scope: &MemoryScope,
        turns: usize,
    ) -> Result<Vec<L1MemoryEntry>, MemoryStoreError> {
        self.store.recent(scope, turns)
    }
}
