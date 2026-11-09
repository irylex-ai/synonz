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
///
/// The contract also owns the **compaction cursor**: which turns the memory
/// pipeline has already compacted. Persistent implementations must persist
/// the cursor alongside the turns (for example as a snapshot field or a
/// position in the append log) and keep the uncompacted turns readable — a
/// capacity smaller than the compaction window silently drops pending turns.
///
/// It also owns the **compression boundary**: the last turn an explicit
/// context compression (`LayeredMemoryActuator::compact_context`) has
/// compressed out of the read phase. Persistent implementations persist it
/// alongside the turns and the compaction cursor (same durability path).
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

    /// The turns not yet marked compacted, oldest first — the durable
    /// pending set the maintenance batches derive from.
    ///
    /// Returns every retained turn while the cursor is unset, or while the
    /// cursor points at a turn the store no longer holds.
    fn uncompacted(&self, scope: &MemoryScope) -> Result<Vec<L1MemoryEntry>, MemoryStoreError>;

    /// Marks every turn up to and including `through` as compacted.
    ///
    /// Idempotent and monotonic: a turn the store no longer holds, or one
    /// already behind the cursor, is a no-op.
    fn mark_compacted(&self, scope: &MemoryScope, through: &str) -> Result<(), MemoryStoreError>;

    /// The read-phase compression boundary: the id of the last turn an
    /// explicit context compression has compressed out of the read phase
    /// (`None` = never compressed).
    ///
    /// The boundary stays set when the turn it names is removed: the
    /// read-phase exclusion resolves the boundary against the retained
    /// turns, and every remaining turn is newer.
    fn context_boundary(&self, scope: &MemoryScope) -> Result<Option<String>, MemoryStoreError>;

    /// Marks every turn up to and including `through` as compressed out of
    /// the read phase.
    ///
    /// Idempotent and monotonic: a turn the store no longer holds, or one
    /// already behind the boundary, is a no-op.
    fn mark_context_boundary(
        &self,
        scope: &MemoryScope,
        through: &str,
    ) -> Result<(), MemoryStoreError>;

    /// Removes one entry by id (idempotent).
    fn remove(&self, scope: &MemoryScope, id: &str) -> Result<bool, MemoryStoreError>;

    /// Clears the partition (including its compaction cursor and its
    /// compression boundary).
    fn clear(&self, scope: &MemoryScope) -> Result<(), MemoryStoreError>;
}

/// The bundled in-process L1 store: one queue per partition with
/// turn-capacity FIFO eviction.
pub struct InProcessL1MemoryStore {
    retained_turns: usize,
    entries: Mutex<HashMap<String, VecDeque<L1MemoryEntry>>>,
    cursors: Mutex<HashMap<String, String>>,
    boundaries: Mutex<HashMap<String, String>>,
}

impl InProcessL1MemoryStore {
    /// Creates the store with a turn capacity.
    pub fn new(retained_turns: usize) -> Self {
        Self {
            retained_turns,
            entries: Mutex::new(HashMap::new()),
            cursors: Mutex::new(HashMap::new()),
            boundaries: Mutex::new(HashMap::new()),
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

    fn uncompacted(&self, scope: &MemoryScope) -> Result<Vec<L1MemoryEntry>, MemoryStoreError> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let Some(queue) = entries.get(scope.as_str()) else {
            return Ok(Vec::new());
        };
        let cursors = self.cursors.lock().unwrap_or_else(|p| p.into_inner());
        let start = cursors
            .get(scope.as_str())
            .and_then(|through| queue.iter().position(|entry| entry.id == *through))
            .map(|index| index + 1)
            .unwrap_or(0);
        Ok(queue.iter().skip(start).cloned().collect())
    }

    fn mark_compacted(&self, scope: &MemoryScope, through: &str) -> Result<(), MemoryStoreError> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let Some(queue) = entries.get(scope.as_str()) else {
            return Ok(());
        };
        let Some(index) = queue.iter().position(|entry| entry.id == through) else {
            return Ok(());
        };
        let mut cursors = self.cursors.lock().unwrap_or_else(|p| p.into_inner());
        let current = cursors
            .get(scope.as_str())
            .and_then(|id| queue.iter().position(|entry| entry.id == *id));
        if current.is_some_and(|current| current >= index) {
            return Ok(());
        }
        cursors.insert(scope.to_string(), through.to_string());
        Ok(())
    }

    fn context_boundary(&self, scope: &MemoryScope) -> Result<Option<String>, MemoryStoreError> {
        Ok(self
            .boundaries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(scope.as_str())
            .cloned())
    }

    fn mark_context_boundary(
        &self,
        scope: &MemoryScope,
        through: &str,
    ) -> Result<(), MemoryStoreError> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let Some(queue) = entries.get(scope.as_str()) else {
            return Ok(());
        };
        let Some(index) = queue.iter().position(|entry| entry.id == through) else {
            return Ok(());
        };
        let mut boundaries = self.boundaries.lock().unwrap_or_else(|p| p.into_inner());
        let current = boundaries
            .get(scope.as_str())
            .and_then(|id| queue.iter().position(|entry| entry.id == *id));
        if current.is_some_and(|current| current >= index) {
            return Ok(());
        }
        boundaries.insert(scope.to_string(), through.to_string());
        Ok(())
    }

    fn remove(&self, scope: &MemoryScope, id: &str) -> Result<bool, MemoryStoreError> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let Some(queue) = entries.get_mut(scope.as_str()) else {
            return Ok(false);
        };
        let before = queue.len();
        queue.retain(|entry| entry.id != id);
        let removed = queue.len() != before;
        if removed {
            let mut cursors = self.cursors.lock().unwrap_or_else(|p| p.into_inner());
            if cursors
                .get(scope.as_str())
                .is_some_and(|through| through == id)
            {
                cursors.remove(scope.as_str());
            }
        }
        Ok(removed)
    }

    fn clear(&self, scope: &MemoryScope) -> Result<(), MemoryStoreError> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.remove(scope.as_str());
        let mut cursors = self.cursors.lock().unwrap_or_else(|p| p.into_inner());
        cursors.remove(scope.as_str());
        let mut boundaries = self.boundaries.lock().unwrap_or_else(|p| p.into_inner());
        boundaries.remove(scope.as_str());
        Ok(())
    }
}

/// The component's L1 domain object (internal): archiving completed turns,
/// reading the working-memory window, and the read-phase compression
/// boundary (read-through cached: the component is the boundary's only
/// writer, so the cache is refreshed on marking and never goes stale; a new
/// process reads the store once per conversation).
pub(crate) struct L1Memory {
    store: Arc<dyn L1MemoryStore>,
    boundary_cache: Mutex<HashMap<String, Option<String>>>,
}

impl L1Memory {
    /// Builds the layer over its store.
    pub(crate) fn new(store: Arc<dyn L1MemoryStore>) -> Self {
        Self {
            store,
            boundary_cache: Mutex::new(HashMap::new()),
        }
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

    /// The turns not yet marked compacted (the durable pending set).
    pub(crate) fn uncompacted(
        &self,
        scope: &MemoryScope,
    ) -> Result<Vec<L1MemoryEntry>, MemoryStoreError> {
        self.store.uncompacted(scope)
    }

    /// Marks every turn up to and including `through` as compacted.
    pub(crate) fn mark_compacted(
        &self,
        scope: &MemoryScope,
        through: &str,
    ) -> Result<(), MemoryStoreError> {
        self.store.mark_compacted(scope, through)
    }

    /// The read-phase compression boundary (read-through cached).
    pub(crate) fn context_boundary(
        &self,
        scope: &MemoryScope,
    ) -> Result<Option<String>, MemoryStoreError> {
        if let Some(cached) = self
            .boundary_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(scope.as_str())
        {
            return Ok(cached.clone());
        }
        let boundary = self.store.context_boundary(scope)?;
        self.boundary_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(scope.to_string(), boundary.clone());
        Ok(boundary)
    }

    /// Marks every turn up to and including `through` as compressed out of
    /// the read phase (refreshes the cache).
    pub(crate) fn mark_context_boundary(
        &self,
        scope: &MemoryScope,
        through: &str,
    ) -> Result<(), MemoryStoreError> {
        self.store.mark_context_boundary(scope, through)?;
        self.boundary_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(scope.to_string(), Some(through.to_string()));
        Ok(())
    }
}
