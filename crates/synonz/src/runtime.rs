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

use crate::bus::{EventBus, MemoryEvent, MemoryFlowFailedMoment, SynonzEvent};
use crate::conversation::{Conversation, ConversationStore};
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
        }
    }
}

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

    /// The conversation-end teardown (the system's structural behavior):
    /// drains the conversation's background maintenance tasks, then
    /// mechanically promotes all L2 blocks into L3 (no model call, no
    /// strategy). Failures surface as `FlowFailed { moment:
    /// AtConversationEnd }` memory facts; success surfaces as `Promoted`.
    pub(crate) async fn finalize_conversation(&self, conversation: &Conversation) {
        // 1. Drain: the conversation's in-flight maintenance completes
        //    before the promotion reads L2 — no unfinished blocks.
        let handles = self
            .maintenance
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(conversation.id());
        if let Some(handles) = handles {
            for handle in handles {
                let _ = handle.await; // job failures already surfaced via the bus
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

    /// Sweeps conversations with no activity past the idle timeout,
    /// running their ConversationEnd flows. Returns how many were ended.
    ///
    /// The idle-timeout fallback: applications schedule this (or an
    /// equivalent periodic task); the explicit [`Conversation::end`]
    /// remains the primary trigger with the initiating side in control.
    pub async fn sweep_stale(&self) -> usize {
        let Some(timeout) = self.conversation_idle_timeout else {
            return 0;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let threshold = now.saturating_sub(timeout.as_secs().max(1));

        let stale = match self.conversation_store.list() {
            Ok(states) => states
                .into_iter()
                .filter(|state| state.last_active <= threshold && state.last_active > 0)
                .collect::<Vec<_>>(),
            Err(_) => return 0,
        };

        let mut ended = 0;
        for state in stale {
            // Rebuild the subject from the stored full identity (encode and
            // decode are symmetric — the reconstruction previously wrapped
            // the display string a second time, silently skipping every
            // conversation).
            let Some(subject) = crate::Subject::parse(&state.subject_id) else {
                continue;
            };
            if let Ok(conversation) = Conversation::of(self, &subject, &state.id) {
                conversation.end(self).await;
                ended += 1;
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
