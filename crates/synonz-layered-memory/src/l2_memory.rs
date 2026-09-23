//! L2 event summaries: the conversation's entries (mid-term memory),
//! produced by batch compaction of L1 when the window fills, the topic
//! shifts, or the conversation ends.
//!
//! One compaction call turns a batch of turns into 1..N entries. An entry
//! keeps its current content plus the history of previous contents; its
//! importance feeds recall ranking. Only the user side becomes memory — the
//! assistant side is context for the summarizer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use synonz::{
    MemoryFailure, MemoryScope, MemoryStoreError, Message, Model, ModelRequest, Subject, complete,
};

use crate::embedding::Embedding;
use crate::l1_memory::L1MemoryEntry;
use crate::observation::{LayeredMemoryEvent, LayeredMemoryObserver};
use crate::utils::{message_text, now_epoch};

/// One event-summary entry.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct L2MemoryEntry {
    /// The entry's stable id (generated at creation).
    pub id: String,
    /// The conversation partition the entry belongs to.
    pub scope: MemoryScope,
    /// The topic the entry belongs to.
    pub topic: String,
    /// The entry's current content.
    pub content: String,
    /// The previous contents, newest first.
    pub versions: Vec<String>,
    /// The content's embedding (recall scoring).
    pub embedding: Vec<f32>,
    /// The entry's importance (0..=1; recall ranking).
    pub importance: f32,
    /// Epoch seconds at which the entry first appeared.
    pub created_at: u64,
    /// Epoch seconds at which the entry was last updated.
    pub updated_at: u64,
}

impl L2MemoryEntry {
    /// Creates an entry (the id is generated here; stores only persist it).
    pub fn new(
        scope: MemoryScope,
        topic: impl Into<String>,
        content: impl Into<String>,
        embedding: Vec<f32>,
        importance: f32,
        created_at: u64,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            scope,
            topic: topic.into(),
            content: content.into(),
            versions: Vec::new(),
            embedding,
            importance,
            created_at,
            updated_at: created_at,
        }
    }
}

/// The L2 storage contract: one conversation's event entries, replaceable
/// by a real backend (MongoDB, ...).
///
/// Writes take the data (the entry carries its scope); reads take the
/// partition. Implementations keep their own capacity (entries per
/// conversation, versions per entry).
pub trait L2MemoryStore: Send + Sync + 'static {
    /// Creates or replaces an entry (same id replaces; the implementation
    /// enforces its capacity).
    fn upsert(&self, entry: L2MemoryEntry) -> Result<(), MemoryStoreError>;

    /// Fetches one entry by id.
    fn get(&self, scope: &MemoryScope, id: &str)
    -> Result<Option<L2MemoryEntry>, MemoryStoreError>;

    /// The partition's entries, most recently updated first.
    fn list(&self, scope: &MemoryScope) -> Result<Vec<L2MemoryEntry>, MemoryStoreError>;

    /// Removes one entry by id (idempotent).
    fn remove(&self, scope: &MemoryScope, id: &str) -> Result<bool, MemoryStoreError>;

    /// How many entries the partition holds.
    fn count(&self, scope: &MemoryScope) -> Result<usize, MemoryStoreError>;
}

/// The bundled in-process L2 store: per-partition lists with capacity
/// eviction (oldest update first) and version truncation.
pub struct InProcessL2MemoryStore {
    events_per_conversation: usize,
    versions_per_entry: usize,
    entries: Mutex<HashMap<String, Vec<L2MemoryEntry>>>,
}

impl InProcessL2MemoryStore {
    /// Creates the store with its capacities.
    pub fn new(events_per_conversation: usize, versions_per_entry: usize) -> Self {
        Self {
            events_per_conversation,
            versions_per_entry,
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl L2MemoryStore for InProcessL2MemoryStore {
    fn upsert(&self, entry: L2MemoryEntry) -> Result<(), MemoryStoreError> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let list = entries.entry(entry.scope.to_string()).or_default();
        let mut entry = entry;
        entry.versions.truncate(self.versions_per_entry);
        match list.iter_mut().find(|existing| existing.id == entry.id) {
            Some(existing) => *existing = entry,
            None => list.push(entry),
        }
        if list.len() > self.events_per_conversation {
            list.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| a.id.cmp(&b.id))
            });
            list.truncate(self.events_per_conversation);
        }
        Ok(())
    }

    fn get(
        &self,
        scope: &MemoryScope,
        id: &str,
    ) -> Result<Option<L2MemoryEntry>, MemoryStoreError> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        Ok(entries
            .get(scope.as_str())
            .and_then(|list| list.iter().find(|entry| entry.id == id))
            .cloned())
    }

    fn list(&self, scope: &MemoryScope) -> Result<Vec<L2MemoryEntry>, MemoryStoreError> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let mut list = entries.get(scope.as_str()).cloned().unwrap_or_default();
        list.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(list)
    }

    fn remove(&self, scope: &MemoryScope, id: &str) -> Result<bool, MemoryStoreError> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let Some(list) = entries.get_mut(scope.as_str()) else {
            return Ok(false);
        };
        let before = list.len();
        list.retain(|entry| entry.id != id);
        Ok(list.len() != before)
    }

    fn count(&self, scope: &MemoryScope) -> Result<usize, MemoryStoreError> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        Ok(entries.get(scope.as_str()).map(Vec::len).unwrap_or(0))
    }
}

/// The batch summarizer's material: the turns to compact, the
/// conversation's existing entries, the content length limit, and the
/// narrated model handle.
#[non_exhaustive]
pub struct L2MemorySummaryInput<'a> {
    /// The batch of turns to compact, oldest first (the assistant side is
    /// context only; only the user side becomes memory).
    pub batch: &'a [L1MemoryEntry],
    /// The conversation's existing entries (the summarizer may update them).
    pub existing: &'a [L2MemoryEntry],
    /// The maximum length of an entry's content, in characters.
    pub max_chars: usize,
    /// The narrated model handle.
    pub model: Arc<dyn Model>,
}

impl<'a> L2MemorySummaryInput<'a> {
    /// Assembles the payload.
    pub fn new(
        batch: &'a [L1MemoryEntry],
        existing: &'a [L2MemoryEntry],
        max_chars: usize,
        model: Arc<dyn Model>,
    ) -> Self {
        Self {
            batch,
            existing,
            max_chars,
            model,
        }
    }
}

/// What one compaction produced: 1..N summaries (empty = nothing to
/// remember in this batch).
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct L2MemorySummaryOutput {
    /// The produced summaries.
    pub summaries: Vec<L2MemorySummary>,
}

impl L2MemorySummaryOutput {
    /// Creates an output from its summaries.
    pub fn new(summaries: Vec<L2MemorySummary>) -> Self {
        Self { summaries }
    }
}

/// One produced summary: update an existing entry or create a new one.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct L2MemorySummary {
    /// The entry to update (`None` = create a new entry).
    pub update: Option<String>,
    /// The entry's topic.
    pub topic: String,
    /// The entry's importance (0..=1).
    pub importance: f32,
    /// The entry's content (non-empty, at most `max_chars`).
    pub content: String,
}

impl L2MemorySummary {
    /// Creates a summary.
    pub fn new(
        update: Option<String>,
        topic: impl Into<String>,
        importance: f32,
        content: impl Into<String>,
    ) -> Self {
        Self {
            update,
            topic: topic.into(),
            importance,
            content: content.into(),
        }
    }
}

/// The batch compaction strategy: turns a batch of L1 turns into 1..N L2
/// entries (updates or creations), from the user side only.
///
/// The output's contents must be non-empty and at most `max_chars`
/// characters; a strategy that cannot satisfy this must return an error
/// rather than truncating.
pub trait L2MemorySummarizer: Send + Sync + 'static {
    /// Compacts one batch.
    fn summarize<'a>(
        &'a self,
        input: L2MemorySummaryInput<'a>,
    ) -> BoxFuture<'a, Result<L2MemorySummaryOutput, MemoryFailure>>;
}

/// The prompt-driven default summarizer.
pub struct L2MemoryPromptSummarizer;

impl L2MemorySummarizer for L2MemoryPromptSummarizer {
    fn summarize<'a>(
        &'a self,
        input: L2MemorySummaryInput<'a>,
    ) -> BoxFuture<'a, Result<L2MemorySummaryOutput, MemoryFailure>> {
        Box::pin(async move {
            let mut batch = String::new();
            for turn in input.batch {
                batch.push_str(&format!(
                    "- topic: {}\n  user: {}\n  assistant: {}\n",
                    turn.topic, turn.input, turn.response
                ));
            }
            let mut existing = String::new();
            for entry in input.existing {
                existing.push_str(&format!("- {}: {}\n", entry.id, entry.content));
            }
            let prompt = format!(
                "You maintain a conversation's memory of the user. Compact the batch below into \
                 entries describing the user's intents, preferences and facts. The assistant's \
                 text is context only and must never become an entry's content.\n\n\
                 Existing entries:\n{existing}\nBatch:\n{batch}\n\
                 Reply with one line per entry (or several lines), each formatted as:\n\
                 UPDATE <entry id>|<topic>|<importance 0..1>|<content>\n\
                 CREATE|<topic>|<importance 0..1>|<content>\n\
                 Content must be non-empty and at most {max} characters. Reply with NONE when \
                 the batch has nothing worth remembering.",
                max = input.max_chars
            );
            let request = ModelRequest::new(vec![Message::user(prompt)], Vec::new());
            let (message, _usage) = complete(&*input.model, request)
                .await
                .map_err(|error| MemoryFailure::new("summarize", error.to_string()))?;
            let text = message_text(&message);
            if text.trim().eq_ignore_ascii_case("NONE") {
                return Ok(L2MemorySummaryOutput::new(Vec::new()));
            }
            let fallback_topic = input
                .batch
                .last()
                .map(|turn| turn.topic.clone())
                .unwrap_or_default();
            let mut summaries = Vec::new();
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let mut parts = line.splitn(4, '|');
                let head = parts.next().unwrap_or_default().trim();
                let topic = parts.next().unwrap_or_default().trim().to_string();
                let importance = parts
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .parse::<f32>()
                    .map_err(|_| {
                        MemoryFailure::new("summarize", format!("unparsable importance: {line}"))
                    })?
                    .clamp(0.0, 1.0);
                let content = parts.next().unwrap_or_default().trim().to_string();
                if content.is_empty() || content.chars().count() > input.max_chars {
                    return Err(MemoryFailure::new(
                        "summarize",
                        format!("content violates the length contract: {line}"),
                    ));
                }
                let update = if let Some(id) = head.strip_prefix("UPDATE") {
                    let id = id.trim().to_string();
                    if id.is_empty() {
                        return Err(MemoryFailure::new(
                            "summarize",
                            format!("UPDATE without an entry id: {line}"),
                        ));
                    }
                    Some(id)
                } else if head.eq_ignore_ascii_case("CREATE") {
                    None
                } else {
                    return Err(MemoryFailure::new(
                        "summarize",
                        format!("unknown summary head: {line}"),
                    ));
                };
                summaries.push(L2MemorySummary {
                    update,
                    topic: if topic.is_empty() {
                        fallback_topic.clone()
                    } else {
                        topic
                    },
                    importance,
                    content,
                });
            }
            Ok(L2MemorySummaryOutput::new(summaries))
        })
    }
}

/// The component's L2 domain object (internal): batch compaction, the
/// per-conversation uncompacted-turn counter (compaction accounting), the
/// conversation index (partition → owner), and entry reads.
pub(crate) struct L2Memory {
    store: Arc<dyn L2MemoryStore>,
    summarizer: Arc<dyn L2MemorySummarizer>,
    embedding: Arc<dyn Embedding>,
    config: Arc<crate::config::LayeredMemoryConfig>,
    observer: Option<Arc<dyn LayeredMemoryObserver>>,
    uncompacted: Mutex<HashMap<String, usize>>,
    conversations: Mutex<HashMap<String, Subject>>,
}

impl L2Memory {
    /// Builds the layer over its collaborators.
    pub(crate) fn new(
        store: Arc<dyn L2MemoryStore>,
        summarizer: Arc<dyn L2MemorySummarizer>,
        embedding: Arc<dyn Embedding>,
        config: Arc<crate::config::LayeredMemoryConfig>,
        observer: Option<Arc<dyn LayeredMemoryObserver>>,
    ) -> Self {
        Self {
            store,
            summarizer,
            embedding,
            config,
            observer,
            uncompacted: Mutex::new(HashMap::new()),
            conversations: Mutex::new(HashMap::new()),
        }
    }

    /// Notes one archived turn: registers the conversation (partition →
    /// owner) and increments its uncompacted count.
    pub(crate) fn note_turn(&self, subject: &Subject, scope: &MemoryScope) {
        {
            let mut conversations = self.conversations.lock().unwrap_or_else(|p| p.into_inner());
            conversations.insert(scope.to_string(), subject.clone());
        }
        let mut counts = self.uncompacted.lock().unwrap_or_else(|p| p.into_inner());
        *counts.entry(scope.to_string()).or_insert(0) += 1;
    }

    /// How many turns await compaction in one conversation.
    pub(crate) fn uncompacted_turns(&self, scope: &MemoryScope) -> usize {
        let counts = self.uncompacted.lock().unwrap_or_else(|p| p.into_inner());
        counts.get(scope.as_str()).copied().unwrap_or(0)
    }

    /// Resets the counter (the conversation stays registered).
    pub(crate) fn clear_counter(&self, scope: &MemoryScope) {
        let mut counts = self.uncompacted.lock().unwrap_or_else(|p| p.into_inner());
        counts.remove(scope.as_str());
    }

    /// The subject's conversation partitions, sorted by scope.
    pub(crate) fn conversation_scopes(&self, subject: &Subject) -> Vec<MemoryScope> {
        let conversations = self.conversations.lock().unwrap_or_else(|p| p.into_inner());
        let mut scopes: Vec<MemoryScope> = conversations
            .iter()
            .filter(|(_, owner)| *owner == subject)
            .map(|(scope, _)| MemoryScope::new(scope))
            .collect();
        scopes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        scopes
    }

    /// The partition's entries, most recently updated first.
    pub(crate) fn entries(
        &self,
        scope: &MemoryScope,
    ) -> Result<Vec<L2MemoryEntry>, MemoryStoreError> {
        self.store.list(scope)
    }

    /// Compacts one batch into entries (one summarizer call, 1..N entries).
    pub(crate) async fn compact(
        &self,
        scope: &MemoryScope,
        batch: &[L1MemoryEntry],
        model: Arc<dyn Model>,
    ) -> Result<(), MemoryFailure> {
        let existing = self
            .store
            .list(scope)
            .map_err(|error| MemoryFailure::new("compact", error.to_string()))?;
        let output = self
            .summarizer
            .summarize(L2MemorySummaryInput::new(
                batch,
                &existing,
                self.config.l2_content_chars,
                model,
            ))
            .await?;
        if output.summaries.is_empty() {
            self.observe(LayeredMemoryEvent::Skipped {
                scope: scope.clone(),
                reason: "the batch produced no entries".to_string(),
            });
            self.clear_counter(scope);
            return Ok(());
        }
        let now = now_epoch();
        let mut created = 0usize;
        let mut updated = 0usize;
        for summary in output.summaries {
            let vector = self.embedding.embed(&summary.content).await?;
            match summary
                .update
                .as_ref()
                .and_then(|id| existing.iter().find(|entry| &entry.id == id))
            {
                Some(previous) => {
                    let mut entry = previous.clone();
                    entry.versions.insert(0, entry.content.clone());
                    entry.versions.truncate(self.config.l2_versions.max(1));
                    entry.content = summary.content;
                    entry.topic = summary.topic;
                    entry.embedding = vector;
                    entry.importance = summary.importance;
                    entry.updated_at = now;
                    self.store
                        .upsert(entry)
                        .map_err(|error| MemoryFailure::new("compact", error.to_string()))?;
                    updated += 1;
                }
                None => {
                    let entry = L2MemoryEntry::new(
                        scope.clone(),
                        summary.topic,
                        summary.content,
                        vector,
                        summary.importance,
                        now,
                    );
                    self.store
                        .upsert(entry)
                        .map_err(|error| MemoryFailure::new("compact", error.to_string()))?;
                    created += 1;
                }
            }
        }
        self.observe(LayeredMemoryEvent::Compacted {
            scope: scope.clone(),
            created,
            updated,
        });
        self.clear_counter(scope);
        Ok(())
    }

    /// Reports one progress fact.
    pub(crate) fn observe(&self, event: LayeredMemoryEvent) {
        if let Some(observer) = &self.observer {
            observer.on_progress(&event);
        }
    }
}
