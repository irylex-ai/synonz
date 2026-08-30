//! The in-process default implementations: conversation persistence and
//! layered memory (register nothing, get the in-process defaults).
//!
//! Both are process-local (data does not survive restart), deterministic,
//! and dependency-free — the bootstrap-quality defaults. Register real
//! implementations (Redis, SQL, vector stores) on the runtime for
//! persistence.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::Subject;
use crate::conversation::{ConversationState, ConversationStore, ConversationStoreError};
use crate::memory::{KnowledgeFragment, L1Entry, MemoryStore, MemoryStoreError, SummaryBlock};

// ─────────────────────── conversation persistence ───────────────────────

/// In-process conversation store (default implementation).
#[derive(Default)]
pub struct InProcessConversationStore {
    // Keyed by conversation id; subject association lives in the state.
    state: Mutex<HashMap<String, ConversationState>>,
}

impl ConversationStore for InProcessConversationStore {
    fn load(
        &self,
        subject: &Subject,
        id: &str,
    ) -> Result<ConversationState, ConversationStoreError> {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let found = state
            .get(id)
            .ok_or_else(|| ConversationStoreError::NotFound(id.to_string()))?;
        if found.subject_id != subject.to_string() {
            return Err(ConversationStoreError::NotFound(id.to_string()));
        }
        Ok(found.clone())
    }

    fn save(&self, state: ConversationState) -> Result<(), ConversationStoreError> {
        let mut map = self.state.lock().unwrap_or_else(|p| p.into_inner());
        map.insert(state.id.clone(), state);
        Ok(())
    }

    fn list(&self) -> Result<Vec<ConversationState>, ConversationStoreError> {
        let map = self.state.lock().unwrap_or_else(|p| p.into_inner());
        Ok(map.values().cloned().collect())
    }
}

// ─────────────────────────── layered memory ─────────────────────────────

#[derive(Default)]
struct InMemoryLayers {
    // subject identity string -> conversation id -> ordered L1 turns.
    l1: HashMap<String, Vec<L1Entry>>,
    // subject -> conversation id -> ordered L2 blocks.
    l2: HashMap<String, Vec<SummaryBlock>>,
    // subject -> ordered L3 fragments.
    l3: HashMap<String, Vec<KnowledgeFragment>>,
}

/// In-process layered memory (default implementation).
///
/// Retrieval uses topic matching plus recency ranking (TopicRecency) —
/// zero external dependencies. Storage is process-local.
#[derive(Default)]
pub struct InProcessMemoryStore {
    layers: Mutex<InMemoryLayers>,
}

impl MemoryStore for InProcessMemoryStore {
    fn l1_append(
        &self,
        subject: &Subject,
        conversation_id: &str,
        topic: &crate::memory::Topic,
        messages: Vec<crate::Message>,
    ) -> Result<(), MemoryStoreError> {
        let mut layers = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        let entry = L1Entry {
            conversation_id: conversation_id.to_string(),
            topic: topic.to_string(),
            messages,
        };
        layers
            .l1
            .entry(subject.to_string())
            .or_default()
            .push(entry);
        Ok(())
    }

    fn l1_window(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<L1Entry>, MemoryStoreError> {
        let layers = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        Ok(layers
            .l1
            .get(&subject.to_string())
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| e.conversation_id == conversation_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    fn l1_pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<L1Entry>, MemoryStoreError> {
        let mut layers = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entries) = layers.l1.get_mut(&subject.to_string()) else {
            return Ok(Vec::new());
        };
        let mut popped = Vec::new();
        let mut kept = Vec::with_capacity(entries.len());
        let mut remaining = n;
        for entry in entries.drain(..) {
            if remaining > 0 && entry.conversation_id == conversation_id {
                popped.push(entry);
                remaining -= 1;
            } else {
                kept.push(entry);
            }
        }
        *entries = kept;
        Ok(popped)
    }

    fn l1_len(&self, subject: &Subject, conversation_id: &str) -> Result<usize, MemoryStoreError> {
        let layers = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        Ok(layers
            .l1
            .get(&subject.to_string())
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| e.conversation_id == conversation_id)
                    .count()
            })
            .unwrap_or(0))
    }

    fn l2_append(&self, subject: &Subject, block: SummaryBlock) -> Result<(), MemoryStoreError> {
        let mut layers = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        layers
            .l2
            .entry(subject.to_string())
            .or_default()
            .push(block);
        Ok(())
    }

    fn l2_read(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<SummaryBlock>, MemoryStoreError> {
        let layers = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        Ok(layers
            .l2
            .get(&subject.to_string())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.conversation_id == conversation_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    fn l2_len(&self, subject: &Subject, conversation_id: &str) -> Result<usize, MemoryStoreError> {
        let layers = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        Ok(layers
            .l2
            .get(&subject.to_string())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.conversation_id == conversation_id)
                    .count()
            })
            .unwrap_or(0))
    }

    fn l2_pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<SummaryBlock>, MemoryStoreError> {
        let mut layers = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        let Some(blocks) = layers.l2.get_mut(&subject.to_string()) else {
            return Ok(Vec::new());
        };
        let mut popped = Vec::new();
        let mut kept = Vec::with_capacity(blocks.len());
        let mut remaining = n;
        for block in blocks.drain(..) {
            if remaining > 0 && block.conversation_id == conversation_id {
                popped.push(block);
                remaining -= 1;
            } else {
                kept.push(block);
            }
        }
        *blocks = kept;
        Ok(popped)
    }

    fn l3_upsert(
        &self,
        subject: &Subject,
        fragment: KnowledgeFragment,
    ) -> Result<(), MemoryStoreError> {
        let mut layers = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        let fragments = layers.l3.entry(subject.to_string()).or_default();
        // Upsert by identity: replace an existing fragment on the same
        // (conversation, topic) identity, otherwise append.
        if let Some(existing) = fragments
            .iter_mut()
            .find(|f| f.identity == fragment.identity)
        {
            *existing = fragment;
        } else {
            fragments.push(fragment);
        }
        Ok(())
    }

    fn l3_retrieve(
        &self,
        subject: &Subject,
        query: &str,
        topic: &crate::memory::Topic,
        budget: usize,
    ) -> Result<Vec<KnowledgeFragment>, MemoryStoreError> {
        let layers = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        let mut candidates: Vec<KnowledgeFragment> = layers
            .l3
            .get(&subject.to_string())
            .map(|fragments| {
                fragments
                    .iter()
                    .filter(|f| {
                        topic_matches(topic, &f.identity.topic) || text_matches(query, &f.content)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        // Recency ranking: newer first, then take the budget.
        candidates.sort_by_key(|f| std::cmp::Reverse(f.created_at));
        candidates.truncate(budget);
        Ok(candidates)
    }

    fn l3_len(&self, subject: &Subject) -> Result<usize, MemoryStoreError> {
        let layers = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        Ok(layers
            .l3
            .get(&subject.to_string())
            .map(|fragments| fragments.len())
            .unwrap_or(0))
    }
}

/// Cheap topic matching: exact match or token overlap.
fn topic_matches(current: &str, candidate: &str) -> bool {
    if current.is_empty() || candidate.is_empty() {
        return false;
    }
    if current == candidate {
        return true;
    }
    let current_tokens: std::collections::HashSet<&str> = current
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    candidate
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .any(|t| current_tokens.contains(t))
}

/// Cheap keyword overlap between the query and fragment content.
fn text_matches(query: &str, content: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    let query_tokens: std::collections::HashSet<&str> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    if query_tokens.is_empty() {
        return false;
    }
    content
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .any(|t| query_tokens.contains(t))
}
