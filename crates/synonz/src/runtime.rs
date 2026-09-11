//! The process-level runtime: explicit bootstrap, startup registry, and
//! default implementations.
//!
//! `SynonzRuntime` is the single source of the process's environment
//! services: conversation persistence, layered memory, and context
//! assembly. There is no implicit default runtime — entities that touch
//! environment services are created through an explicit runtime (the
//! static factory family on `Conversation` takes `&runtime`), so a
//! conversation and the memory it resolves can never split across
//! environments.

use std::sync::Arc;
use std::time::Duration;

use crate::bus::{
    ConversationEndReason, EventBus, MemoryEvent, MemoryFlowFailedMoment, SynonzEvent,
};
use crate::conversation::{
    Conversation, ConversationPage, ConversationQuery, ConversationStore, ConversationStoreError,
};
use crate::event::MemoryFlowStage;
use crate::inprocess::{
    InProcessConversationStore, InProcessMemoryL1Store, InProcessMemoryL2Store,
    InProcessMemoryL3Store,
};
use crate::memory::{Memory, MemoryL1Store, MemoryL2Store, MemoryL3Store};

/// The startup registry: every service has an in-process default;
/// registration replaces it. Resolution can never fail.
#[derive(Default)]
pub struct RuntimeBuilder {
    conversation_store: Option<Arc<dyn ConversationStore>>,
    memory_l1_store: Option<Arc<dyn MemoryL1Store>>,
    memory_l2_store: Option<Arc<dyn MemoryL2Store>>,
    memory_l3_store: Option<Arc<dyn MemoryL3Store>>,
    observers: Vec<Arc<dyn crate::bus::Observer>>,
    conversation_idle_timeout: Option<Duration>,
}

impl RuntimeBuilder {
    /// Starts building a runtime.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the conversation store (default: in-process).
    pub fn conversation_store(mut self, store: impl ConversationStore) -> Self {
        self.conversation_store = Some(Arc::new(store));
        self
    }

    /// Sets the L1 working memory store (default: built-in in-process
    /// memory — the freshness layer expects memory-grade latency).
    pub fn memory_l1_store(mut self, store: impl MemoryL1Store) -> Self {
        self.memory_l1_store = Some(Arc::new(store));
        self
    }

    /// Sets the L2 summary store (default: in-process).
    pub fn memory_l2_store(mut self, store: impl MemoryL2Store) -> Self {
        self.memory_l2_store = Some(Arc::new(store));
        self
    }

    /// Sets the L3 knowledge store (default: in-process).
    pub fn memory_l3_store(mut self, store: impl MemoryL3Store) -> Self {
        self.memory_l3_store = Some(Arc::new(store));
        self
    }

    /// Registers an observer of the full event stream. Unlike the other
    /// services there is **no default** — with no observer registered, the
    /// observation lane delivers to nobody (the bus itself still runs).
    pub fn observer(mut self, observer: impl crate::bus::Observer) -> Self {
        self.observers.push(Arc::new(observer));
        self
    }

    /// Sets the conversation idle timeout: conversations with no activity
    /// for this long are ended by [`SynonzRuntime::sweep_stale`]
    /// (the ConversationEnd fallback; explicit `Conversation::end`
    /// remains the primary trigger).
    pub fn conversation_idle_timeout(mut self, timeout: Duration) -> Self {
        self.conversation_idle_timeout = Some(timeout);
        self
    }

    /// Builds the runtime with registered or default implementations.
    pub fn build(self) -> SynonzRuntime {
        SynonzRuntime {
            conversation_store: self
                .conversation_store
                .unwrap_or_else(|| Arc::new(InProcessConversationStore::default())),
            memory: Memory::new(
                self.memory_l1_store
                    .unwrap_or_else(|| Arc::new(InProcessMemoryL1Store::default())),
                self.memory_l2_store
                    .unwrap_or_else(|| Arc::new(InProcessMemoryL2Store::default())),
                self.memory_l3_store
                    .unwrap_or_else(|| Arc::new(InProcessMemoryL3Store::default())),
            ),
            event_bus: EventBus::new(self.observers),
            maintenance: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            conversation_idle_timeout: self.conversation_idle_timeout,
            maintenance_drain_timeout: MAINTENANCE_DRAIN_TIMEOUT,
        }
    }
}

/// The per-conversation drain budget: how long conversation-end teardown
/// waits for background maintenance tasks before proceeding without them.
const MAINTENANCE_DRAIN_TIMEOUT: Duration = Duration::from_secs(60);

/// How many stale conversations one sweep page holds (fixed by design:
/// the sweep pages through the store instead of loading everything).
const SWEEP_PAGE_SIZE: usize = 128;

/// The process-level environment handle.
///
/// Cheap to clone (shared state); every entity created with it shares the
/// same environment — the explicit same-source guarantee. Entities are
/// created via the static factory family on their own types
/// (`Conversation::new(&runtime, &subject)`), not through the runtime
/// itself (factory attribution: the product type owns its construction).
#[derive(Clone)]
pub struct SynonzRuntime {
    conversation_store: Arc<dyn ConversationStore>,
    /// The layered memory as one domain object (assembled from the three
    /// storage slots at build time; the runtime is its single authority).
    memory: Memory,
    event_bus: EventBus,
    /// The session maintenance table: conversation id → the background
    /// maintenance tasks the agents' engines spawned there (the system
    /// schedules what applications produce; conversation-end teardown
    /// drains them).
    maintenance:
        Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<tokio::task::JoinHandle<()>>>>>,
    conversation_idle_timeout: Option<Duration>,
    /// The per-conversation budget for draining background maintenance
    /// tasks at conversation end (bounded teardown).
    maintenance_drain_timeout: Duration,
}

impl SynonzRuntime {
    /// Starts building a runtime.
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }

    /// The registered (or default) conversation store.
    pub(crate) fn conversation_store(&self) -> Arc<dyn ConversationStore> {
        Arc::clone(&self.conversation_store)
    }

    /// The layered memory (the three storage slots behind one facade).
    ///
    /// Public for the Low Level track: direct memory access (diagnostics,
    /// custom flows, explicit operations) alongside the framework's own
    /// orchestration. This is the object's single authority — nobody
    /// constructs a `Memory` by hand.
    pub fn memory(&self) -> Memory {
        self.memory.clone()
    }

    /// The event bus (the resident dual-lane dispatch facility).
    pub(crate) fn event_bus(&self) -> &EventBus {
        &self.event_bus
    }

    /// A maintenance-task registry handle scoped to one conversation.
    pub(crate) fn task_registry(&self, conversation_id: &str) -> crate::context::TaskRegistry {
        crate::context::TaskRegistry::new(Arc::clone(&self.maintenance), conversation_id)
    }

    /// Test-only override for the drain budget (kept off the public
    /// configuration surface by design).
    #[cfg(test)]
    pub(crate) fn set_maintenance_drain_timeout(&mut self, timeout: Duration) {
        self.maintenance_drain_timeout = timeout;
    }

    /// The conversation-end teardown (the system's structural behavior):
    /// drains the conversation's background maintenance tasks, then
    /// mechanically promotes all L2 blocks into L3 (no model call, no
    /// strategy). Failures surface as `FlowFailed { moment:
    /// AtConversationEnd }` memory facts; success surfaces as `Promoted`.
    pub(crate) async fn finalize_conversation(&self, conversation: &Conversation) {
        // 1. Drain: the conversation's in-flight maintenance completes
        //    before the promotion reads L2 — no unfinished blocks. The
        //    drain is bounded per conversation: a stuck task delays
        //    teardown by at most the configured budget; stuck tasks and
        //    panicked tasks surface as FlowFailed facts.
        let handles = self
            .maintenance
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(conversation.id());
        if let Some(handles) = handles {
            let deadline = tokio::time::Instant::now() + self.maintenance_drain_timeout;
            let total = handles.len();
            let mut timed_out = 0usize;
            for handle in handles {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    timed_out += 1;
                    continue;
                }
                match tokio::time::timeout(remaining, handle).await {
                    Ok(Ok(())) => {}
                    Ok(Err(join_error)) => {
                        self.event_bus
                            .emit(SynonzEvent::Memory(MemoryEvent::FlowFailed {
                                stage: MemoryFlowStage::Drain,
                                detail: format!("maintenance task failed: {join_error}"),
                                moment: MemoryFlowFailedMoment::AtConversationEnd,
                            }));
                    }
                    Err(_) => timed_out += 1,
                }
            }
            if timed_out > 0 {
                self.event_bus
                    .emit(SynonzEvent::Memory(MemoryEvent::FlowFailed {
                        stage: MemoryFlowStage::Drain,
                        detail: format!(
                            "drain timed out: {timed_out} of {total} maintenance task(s) still running"
                        ),
                        moment: MemoryFlowFailedMoment::AtConversationEnd,
                    }));
            }
        }

        // 2. Mechanical promotion: every L2 block becomes L3 knowledge
        //    under the conversation's topic.
        let subject = conversation.subject();
        let memory = &self.memory;
        let l2_len = match memory.l2_len(subject, conversation.id()) {
            Ok(len) => len,
            Err(error) => {
                self.event_bus
                    .emit(SynonzEvent::Memory(MemoryEvent::FlowFailed {
                        stage: MemoryFlowStage::Distill,
                        detail: format!("promotion read: {error}"),
                        moment: MemoryFlowFailedMoment::AtConversationEnd,
                    }));
                return;
            }
        };
        if l2_len == 0 {
            return;
        }
        let blocks = match memory.l2_pop_oldest(subject, conversation.id(), l2_len) {
            Ok(blocks) => blocks,
            Err(error) => {
                self.event_bus
                    .emit(SynonzEvent::Memory(MemoryEvent::FlowFailed {
                        stage: MemoryFlowStage::Distill,
                        detail: format!("promotion pop: {error}"),
                        moment: MemoryFlowFailedMoment::AtConversationEnd,
                    }));
                return;
            }
        };
        let topic = conversation.topic().unwrap_or_default();
        let mut promoted = 0usize;
        for block in blocks {
            let fragment = crate::memory::KnowledgeFragment {
                identity: crate::memory::FragmentIdentity {
                    subject_id: subject.to_string(),
                    conversation_id: block.conversation_id,
                    topic: topic.clone(),
                },
                content: block.content,
                created_at: now_epoch(),
            };
            if let Err(error) = memory.l3_upsert(subject, fragment) {
                self.event_bus
                    .emit(SynonzEvent::Memory(MemoryEvent::FlowFailed {
                        stage: MemoryFlowStage::Distill,
                        detail: format!("promotion upsert: {error}"),
                        moment: MemoryFlowFailedMoment::AtConversationEnd,
                    }));
            } else {
                promoted += 1;
            }
        }
        if promoted > 0 {
            self.event_bus
                .emit(SynonzEvent::Memory(MemoryEvent::Promoted {
                    conversation_id: conversation.id().to_string(),
                    count: promoted,
                }));
        }
    }

    /// Lists conversations matching a metadata keyword with keyset
    /// pagination — the Low Level track (session management, diagnostics).
    ///
    /// Ordering is `last_active` descending, `id` ascending; pass a
    /// previous page's `next` back through [`ConversationQuery::with_after`]
    /// to continue.
    pub fn list_conversations(
        &self,
        query: ConversationQuery,
    ) -> Result<ConversationPage, ConversationStoreError> {
        self.conversation_store.list(query)
    }

    /// Sweeps conversations with no activity past the idle timeout,
    /// running their ConversationEnd flows. Returns how many were ended.
    ///
    /// Pages through the store's stale query: the cursor advances past
    /// failed entries (retried on the next sweep), so one bad conversation
    /// never blocks the rest.
    pub async fn sweep_stale(&self) -> usize {
        let Some(timeout) = self.conversation_idle_timeout else {
            return 0;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let threshold = now.saturating_sub(timeout.as_secs().max(1));

        let mut cursor = None;
        let mut ended = 0;
        while let Ok(page) = self
            .conversation_store
            .list_stale(threshold, cursor, SWEEP_PAGE_SIZE)
        {
            if page.items.is_empty() {
                break;
            }
            for summary in &page.items {
                let Some(subject) = crate::Subject::parse(&summary.subject_id) else {
                    continue;
                };
                if let Ok(conversation) = Conversation::of(self, &subject, &summary.id) {
                    conversation
                        .end_with(self, ConversationEndReason::IdleSwept)
                        .await;
                    ended += 1;
                }
            }
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        ended
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Observer, ObserverContext};
    use crate::conversation::Conversation;
    use crate::subject::{Subject, SubjectType};
    use std::sync::{Arc, Mutex};

    /// Records memory-flow failure facts for assertions.
    #[derive(Default, Clone)]
    struct FactRecorder {
        facts: Arc<Mutex<Vec<String>>>,
    }

    impl Observer for FactRecorder {
        fn on_event(&self, _ctx: &ObserverContext, event: &SynonzEvent) {
            if let SynonzEvent::Memory(MemoryEvent::FlowFailed { detail, .. }) = event {
                self.facts.lock().unwrap().push(detail.clone());
            }
        }
    }

    #[tokio::test]
    async fn finalize_drain_is_bounded_and_visible() {
        let recorder = FactRecorder::default();
        let mut runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
        runtime.set_maintenance_drain_timeout(Duration::from_millis(50));
        let subject = Subject::of(SubjectType::User, "u-drain");
        let conversation = Conversation::with_id(&runtime, &subject, "drain-bounded");

        runtime
            .task_registry(conversation.id())
            .spawn(std::future::pending::<()>());

        let started = std::time::Instant::now();
        conversation.end(&runtime).await;
        assert!(conversation.is_ended());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the drain must be bounded (took {:?})",
            started.elapsed()
        );

        tokio::time::sleep(Duration::from_millis(50)).await;
        let facts = recorder.facts.lock().unwrap();
        assert!(
            facts.iter().any(|fact| fact.contains("drain timed out")),
            "the timeout must be visible: {facts:?}"
        );
    }

    #[tokio::test]
    async fn finalize_drain_surfaces_task_panics() {
        let recorder = FactRecorder::default();
        let runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
        let subject = Subject::of(SubjectType::User, "u-panic");
        let conversation = Conversation::with_id(&runtime, &subject, "drain-panic");

        runtime
            .task_registry(conversation.id())
            .spawn(async { panic!("maintenance boom") });
        // Let the task panic before teardown awaits it.
        tokio::time::sleep(Duration::from_millis(20)).await;

        conversation.end(&runtime).await;
        assert!(conversation.is_ended());

        tokio::time::sleep(Duration::from_millis(50)).await;
        let facts = recorder.facts.lock().unwrap();
        assert!(
            facts
                .iter()
                .any(|fact| fact.contains("maintenance task failed")),
            "the task panic must be visible: {facts:?}"
        );
    }

    #[tokio::test]
    async fn sweep_pages_through_many_stale_conversations() {
        let runtime = SynonzRuntime::builder()
            .conversation_idle_timeout(Duration::from_millis(1))
            .build();
        let subject = Subject::of(SubjectType::User, "u-sweep");
        let stale_stamp = now_epoch().saturating_sub(3600);
        let total = 130usize;
        for index in 0..total {
            runtime
                .conversation_store
                .save(crate::conversation::ConversationState {
                    subject_id: subject.to_string(),
                    id: format!("sweep-{index:03}"),
                    turns: Vec::new(),
                    topic: None,
                    last_active: stale_stamp,
                    ended: false,
                })
                .unwrap();
        }

        let ended = runtime.sweep_stale().await;
        assert_eq!(ended, total, "the sweep must page past one page size");
    }
}
