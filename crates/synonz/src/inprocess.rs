//! The in-process default implementations: conversation persistence and
//! the three memory layer stores (register nothing, get the in-process
//! defaults).
//!
//! All are process-local (data does not survive restart), deterministic,
//! and dependency-free — the bootstrap-quality defaults. Register real
//! implementations (Redis, SQL, vector stores) on the runtime for
//! persistence. The defaults are internal (pub(crate)): the public face
//! is the contracts, not the bundled implementations.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::Subject;
use crate::conversation::{
    ConversationCursor, ConversationPage, ConversationQuery, ConversationState, ConversationStore,
    ConversationStoreError, ConversationSummary,
};
use crate::memory::{
    KnowledgeFragment, L1Entry, MemoryL1Store, MemoryL2Store, MemoryL3Store, MemoryStoreError,
    SummaryBlock, Topic,
};

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

    fn list_stale(
        &self,
        before: u64,
        after: Option<ConversationCursor>,
        limit: usize,
    ) -> Result<ConversationPage, ConversationStoreError> {
        let map = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let states = map
            .values()
            .filter(|state| !state.ended && state.last_active > 0 && state.last_active <= before);
        Ok(page_of(states, after, limit))
    }

    fn list(&self, query: ConversationQuery) -> Result<ConversationPage, ConversationStoreError> {
        let map = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let ConversationQuery {
            keyword,
            after,
            limit,
        } = query;
        let states = map.values().filter(|state| match &keyword {
            Some(keyword) => matches_keyword(state, keyword),
            None => true,
        });
        Ok(page_of(states, after, limit))
    }
}

/// Projects matching states into one keyset page. The listing order is
/// `last_active` descending, `id` ascending; `next` points strictly past
/// the last returned item when more matches exist.
fn page_of<'a>(
    states: impl Iterator<Item = &'a ConversationState>,
    after: Option<ConversationCursor>,
    limit: usize,
) -> ConversationPage {
    let mut items: Vec<ConversationSummary> = states
        .map(|state| {
            ConversationSummary::new(
                state.id.clone(),
                state.subject_id.clone(),
                state.topic.clone(),
                state.last_active,
                state.ended,
            )
        })
        .collect();
    items.sort_by(|a, b| {
        b.last_active
            .cmp(&a.last_active)
            .then_with(|| a.id.cmp(&b.id))
    });
    if limit == 0 {
        return ConversationPage::new(Vec::new(), None);
    }
    let start = match after {
        Some(cursor) => items
            .iter()
            .position(|item| is_after(item, &cursor))
            .unwrap_or(items.len()),
        None => 0,
    };
    let remaining = items.len() - start;
    let take = remaining.min(limit);
    let page_items: Vec<ConversationSummary> = items[start..start + take].to_vec();
    let next = if remaining > take {
        page_items
            .last()
            .map(|item| ConversationCursor::new(item.last_active, item.id.clone()))
    } else {
        None
    };
    ConversationPage::new(page_items, next)
}

/// Whether `item` sits strictly after the cursor in the listing order.
fn is_after(item: &ConversationSummary, cursor: &ConversationCursor) -> bool {
    item.last_active < cursor.last_active
        || (item.last_active == cursor.last_active && item.id > cursor.id)
}

/// Metadata keyword match: case-insensitive substring over `id`,
/// `subject_id`, or `topic` (an empty keyword matches everything).
fn matches_keyword(state: &ConversationState, keyword: &str) -> bool {
    let needle = keyword.to_lowercase();
    let contains = |value: &str| value.to_lowercase().contains(&needle);
    contains(&state.id)
        || contains(&state.subject_id)
        || state.topic.as_deref().is_some_and(contains)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subject::SubjectType;

    fn state(
        subject: &Subject,
        id: &str,
        last_active: u64,
        ended: bool,
        topic: Option<&str>,
    ) -> ConversationState {
        ConversationState {
            subject_id: subject.to_string(),
            id: id.to_string(),
            turns: Vec::new(),
            topic: topic.map(str::to_string),
            last_active,
            ended,
        }
    }

    #[test]
    fn list_stale_filters_and_orders() {
        let store = InProcessConversationStore::default();
        let subject = Subject::of(SubjectType::User, "u-1");
        store
            .save(state(&subject, "old", 100, false, None))
            .unwrap();
        store
            .save(state(&subject, "new", 300, false, None))
            .unwrap();
        store
            .save(state(&subject, "ended", 50, true, None))
            .unwrap();
        store
            .save(state(&subject, "unstamped", 0, false, None))
            .unwrap();

        let page = store.list_stale(200, None, 10).unwrap();
        let ids: Vec<&str> = page.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["old"], "only not-ended, stamped, <= before");
        assert!(page.next.is_none());
    }

    #[test]
    fn list_stale_paginates_by_cursor() {
        let store = InProcessConversationStore::default();
        let subject = Subject::of(SubjectType::User, "u-2");
        for (id, last_active) in [("a", 400u64), ("b", 300), ("c", 200), ("d", 100)] {
            store
                .save(state(&subject, id, last_active, false, None))
                .unwrap();
        }

        let first = store.list_stale(1000, None, 2).unwrap();
        let ids: Vec<&str> = first.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
        let next = first.next.expect("a second page exists");
        assert_eq!((next.last_active, next.id.as_str()), (300, "b"));

        let second = store.list_stale(1000, Some(next), 2).unwrap();
        let ids: Vec<&str> = second.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "d"]);
        assert!(second.next.is_none());
    }

    #[test]
    fn list_stale_ties_break_by_id_ascending() {
        let store = InProcessConversationStore::default();
        let subject = Subject::of(SubjectType::User, "u-3");
        store.save(state(&subject, "b", 500, false, None)).unwrap();
        store.save(state(&subject, "a", 500, false, None)).unwrap();

        let page = store.list_stale(1000, None, 10).unwrap();
        let ids: Vec<&str> = page.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn list_matches_metadata_keyword_case_insensitively() {
        let store = InProcessConversationStore::default();
        let subject = Subject::of(SubjectType::User, "Alice");
        store
            .save(state(&subject, "Alpha-1", 10, false, None))
            .unwrap();
        store
            .save(state(&subject, "beta-2", 20, false, Some("Billing")))
            .unwrap();

        let by_id = store
            .list(ConversationQuery::new(10).with_keyword("alpha"))
            .unwrap();
        assert_eq!(by_id.items.len(), 1);
        assert_eq!(by_id.items[0].id, "Alpha-1");

        let by_subject = store
            .list(ConversationQuery::new(10).with_keyword("alice"))
            .unwrap();
        assert_eq!(by_subject.items.len(), 2);

        let by_topic = store
            .list(ConversationQuery::new(10).with_keyword("bill"))
            .unwrap();
        assert_eq!(by_topic.items.len(), 1);
        assert_eq!(by_topic.items[0].id, "beta-2");
    }
}

// ───────────────────────── memory layer stores ──────────────────────────

/// In-process L1 working memory (default implementation): the recent
/// turns of each conversation, verbatim, memory-grade latency.
#[derive(Default)]
pub(crate) struct InProcessMemoryL1Store {
    // subject identity string -> conversation id -> ordered L1 turns.
    l1: Mutex<HashMap<String, Vec<L1Entry>>>,
}

impl MemoryL1Store for InProcessMemoryL1Store {
    fn append(
        &self,
        subject: &Subject,
        conversation_id: &str,
        topic: &Topic,
        messages: Vec<crate::Message>,
    ) -> Result<(), MemoryStoreError> {
        let mut l1 = self.l1.lock().unwrap_or_else(|p| p.into_inner());
        let entry = L1Entry {
            conversation_id: conversation_id.to_string(),
            topic: topic.to_string(),
            messages,
        };
        l1.entry(subject.to_string()).or_default().push(entry);
        Ok(())
    }

    fn window(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<L1Entry>, MemoryStoreError> {
        let l1 = self.l1.lock().unwrap_or_else(|p| p.into_inner());
        Ok(l1
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

    fn pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<L1Entry>, MemoryStoreError> {
        let mut l1 = self.l1.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entries) = l1.get_mut(&subject.to_string()) else {
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

    fn len(&self, subject: &Subject, conversation_id: &str) -> Result<usize, MemoryStoreError> {
        let l1 = self.l1.lock().unwrap_or_else(|p| p.into_inner());
        Ok(l1
            .get(&subject.to_string())
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| e.conversation_id == conversation_id)
                    .count()
            })
            .unwrap_or(0))
    }
}

/// In-process L2 summary store (default implementation).
#[derive(Default)]
pub(crate) struct InProcessMemoryL2Store {
    // subject -> conversation id -> ordered L2 blocks.
    l2: Mutex<HashMap<String, Vec<SummaryBlock>>>,
}

impl MemoryL2Store for InProcessMemoryL2Store {
    fn append(&self, subject: &Subject, block: SummaryBlock) -> Result<(), MemoryStoreError> {
        let mut l2 = self.l2.lock().unwrap_or_else(|p| p.into_inner());
        l2.entry(subject.to_string()).or_default().push(block);
        Ok(())
    }

    fn read(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<Vec<SummaryBlock>, MemoryStoreError> {
        let l2 = self.l2.lock().unwrap_or_else(|p| p.into_inner());
        Ok(l2
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

    fn len(&self, subject: &Subject, conversation_id: &str) -> Result<usize, MemoryStoreError> {
        let l2 = self.l2.lock().unwrap_or_else(|p| p.into_inner());
        Ok(l2
            .get(&subject.to_string())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.conversation_id == conversation_id)
                    .count()
            })
            .unwrap_or(0))
    }

    fn pop_oldest(
        &self,
        subject: &Subject,
        conversation_id: &str,
        n: usize,
    ) -> Result<Vec<SummaryBlock>, MemoryStoreError> {
        let mut l2 = self.l2.lock().unwrap_or_else(|p| p.into_inner());
        let Some(blocks) = l2.get_mut(&subject.to_string()) else {
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
}

/// In-process L3 knowledge store (default implementation).
///
/// Retrieval uses topic matching plus recency ranking — zero external
/// dependencies. Storage is process-local.
#[derive(Default)]
pub(crate) struct InProcessMemoryL3Store {
    // subject -> ordered L3 fragments.
    l3: Mutex<HashMap<String, Vec<KnowledgeFragment>>>,
}

impl MemoryL3Store for InProcessMemoryL3Store {
    fn upsert(
        &self,
        subject: &Subject,
        fragment: KnowledgeFragment,
    ) -> Result<(), MemoryStoreError> {
        let mut l3 = self.l3.lock().unwrap_or_else(|p| p.into_inner());
        let fragments = l3.entry(subject.to_string()).or_default();
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

    fn query(
        &self,
        subject: &Subject,
        query: &str,
        topic: &Topic,
        budget: usize,
    ) -> Result<Vec<KnowledgeFragment>, MemoryStoreError> {
        let l3 = self.l3.lock().unwrap_or_else(|p| p.into_inner());
        let mut candidates: Vec<KnowledgeFragment> = l3
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

    fn len(&self, subject: &Subject) -> Result<usize, MemoryStoreError> {
        let l3 = self.l3.lock().unwrap_or_else(|p| p.into_inner());
        Ok(l3
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
