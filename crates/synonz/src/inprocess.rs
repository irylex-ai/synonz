//! The in-process default implementations: conversation persistence and
//! the framework's bundled memory provider.
//!
//! Both are process-local (data does not survive restart), deterministic,
//! and dependency-free — the bootstrap-quality defaults. Register real
//! implementations (Redis, SQL, vector stores, the layered component) on
//! the runtime for persistence and richer memory. The bundled types are
//! internal: the public face is the contracts, not the bundled
//! implementations.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;

use crate::Subject;
use crate::bus::EventBus;
use crate::context::{
    MemoryContextAssembleInput, MemoryContextAssembleOutput, MemoryContextAssembler, MemoryFailure,
    MemoryPipeline, MemoryProvider, PipelineConversationContext, PipelineTurnContext,
};
use crate::conversation::{
    ConversationCursor, ConversationPage, ConversationQuery, ConversationState, ConversationStore,
    ConversationStoreError, ConversationSummary,
};
use crate::memory::{
    Memory, MemoryForgetResult, MemoryItem, MemoryListCursor, MemoryPage, MemoryQuery,
    MemoryStoreError,
};
use crate::message::Message;

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

// ───────────────────────── the bundled memory provider ──────────────────

/// How many recent messages the bundled provider replays as the run's
/// conversation window (a fixed floor: the bundled provider is a bootstrap
/// default, not a tunable memory system).
const DEFAULT_WINDOW_MESSAGES: usize = 40;

/// The bundled provider's turn store: the recent messages of each
/// conversation, process-local.
#[derive(Default)]
struct InProcessTurnStore {
    // (subject identity, conversation id) -> recent messages, oldest first.
    turns: Mutex<HashMap<(String, String), Vec<Message>>>,
}

impl InProcessTurnStore {
    fn append(&self, subject: &Subject, conversation_id: &str, messages: Vec<Message>) {
        if messages.is_empty() {
            return;
        }
        let mut turns = self.turns.lock().unwrap_or_else(|p| p.into_inner());
        let entry = turns
            .entry((subject.to_string(), conversation_id.to_string()))
            .or_default();
        entry.extend(messages);
        let excess = entry.len().saturating_sub(DEFAULT_WINDOW_MESSAGES);
        if excess > 0 {
            entry.drain(..excess);
        }
    }

    fn recent(&self, subject: &Subject, conversation_id: &str) -> Vec<Message> {
        let turns = self.turns.lock().unwrap_or_else(|p| p.into_inner());
        turns
            .get(&(subject.to_string(), conversation_id.to_string()))
            .cloned()
            .unwrap_or_default()
    }
}

/// The framework's bundled memory provider: in-process and non-layered.
///
/// The read face replays the conversation's recent messages; the write
/// face appends each completed turn's new messages (the user input and the
/// model's responses); the management face is minimal — the bundled
/// provider keeps no management-grade entries (register a real memory
/// provider for management).
pub(crate) struct InProcessMemoryProvider {
    store: Arc<InProcessTurnStore>,
}

impl InProcessMemoryProvider {
    /// Creates the bundled provider.
    pub(crate) fn new() -> Self {
        Self {
            store: Arc::new(InProcessTurnStore::default()),
        }
    }
}

impl MemoryProvider for InProcessMemoryProvider {
    fn memory(&self, _bus: EventBus) -> Arc<dyn Memory> {
        Arc::new(InProcessMemory)
    }

    fn context_assembler(&self) -> Arc<dyn MemoryContextAssembler> {
        Arc::new(InProcessContextAssembler {
            store: Arc::clone(&self.store),
        })
    }

    fn pipeline(&self) -> Arc<dyn MemoryPipeline> {
        Arc::new(InProcessPipeline {
            store: Arc::clone(&self.store),
        })
    }
}

/// The bundled provider's management face: minimal by design (no entries).
struct InProcessMemory;

impl Memory for InProcessMemory {
    fn list(
        &self,
        _subject: &Subject,
        _query: MemoryQuery,
    ) -> Result<MemoryPage<MemoryItem, MemoryListCursor>, MemoryStoreError> {
        Ok(MemoryPage::new(Vec::new(), None))
    }

    fn get(&self, _subject: &Subject, _id: &str) -> Result<Option<MemoryItem>, MemoryStoreError> {
        Ok(None)
    }

    fn edit(
        &self,
        _subject: &Subject,
        id: &str,
        _content: &str,
    ) -> Result<MemoryItem, MemoryStoreError> {
        Err(MemoryStoreError::EntryNotFound(id.to_string()))
    }

    fn forget(
        &self,
        _subject: &Subject,
        _id: &str,
    ) -> Result<MemoryForgetResult, MemoryStoreError> {
        Ok(MemoryForgetResult {
            removed: 0,
            failures: Vec::new(),
        })
    }

    fn forget_matching(
        &self,
        _subject: &Subject,
        _query: MemoryQuery,
    ) -> Result<MemoryForgetResult, MemoryStoreError> {
        Ok(MemoryForgetResult {
            removed: 0,
            failures: Vec::new(),
        })
    }
}

/// The bundled provider's read face: the conversation's recent messages
/// followed by this turn's model-visible user message.
struct InProcessContextAssembler {
    store: Arc<InProcessTurnStore>,
}

impl MemoryContextAssembler for InProcessContextAssembler {
    fn assemble<'a>(
        &'a self,
        input: MemoryContextAssembleInput<'a>,
    ) -> BoxFuture<'a, MemoryContextAssembleOutput> {
        Box::pin(async move {
            let mut messages = self.store.recent(input.subject, input.conversation_id);
            messages.push(Message::user(input.rewritten_input.unwrap_or(input.input)));
            MemoryContextAssembleOutput::new(messages, Vec::new())
        })
    }
}

/// The bundled provider's write face: append each completed turn's new
/// messages (the user input and the model's responses) to the window.
struct InProcessPipeline {
    store: Arc<InProcessTurnStore>,
}

impl MemoryPipeline for InProcessPipeline {
    fn archive_turn<'a>(
        &'a self,
        ctx: &'a PipelineTurnContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
        Box::pin(async move {
            let mut messages = Vec::with_capacity(ctx.responses().len() + 1);
            messages.push(Message::user(ctx.input()));
            messages.extend(ctx.responses().iter().cloned());
            self.store
                .append(ctx.subject(), ctx.conversation_id(), messages);
            Ok(())
        })
    }

    fn spawn_task<'a>(
        &'a self,
        _ctx: &'a PipelineTurnContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
        Box::pin(async { Ok(()) })
    }

    fn finalize_conversation<'a>(
        &'a self,
        _ctx: &'a PipelineConversationContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod bundled_tests {
    use super::*;
    use crate::subject::SubjectType;

    #[test]
    fn the_turn_store_keeps_the_window_bounded() {
        let store = InProcessTurnStore::default();
        let subject = Subject::of(SubjectType::User, "u-window");
        for index in 0..(DEFAULT_WINDOW_MESSAGES + 5) {
            store.append(&subject, "c1", vec![Message::user(format!("m{index}"))]);
        }
        let recent = store.recent(&subject, "c1");
        assert_eq!(recent.len(), DEFAULT_WINDOW_MESSAGES);
        assert_eq!(
            recent.first().unwrap().blocks[0],
            crate::message::ContentBlock::Text {
                text: "m5".to_string()
            }
        );
    }

    #[test]
    fn the_turn_store_is_conversation_scoped() {
        let store = InProcessTurnStore::default();
        let subject = Subject::of(SubjectType::User, "u-scope");
        store.append(&subject, "c1", vec![Message::user("one")]);
        store.append(&subject, "c2", vec![Message::user("two")]);
        assert_eq!(store.recent(&subject, "c1").len(), 1);
        assert_eq!(store.recent(&subject, "c2").len(), 1);
    }
}
