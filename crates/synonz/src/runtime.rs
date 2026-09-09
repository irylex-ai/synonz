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

use crate::bus::EventBus;
use crate::context::{ContextAssembly, LayeredMemory};
use crate::conversation::{Conversation, ConversationStore};
use crate::inprocess::{
    InProcessConversationStore, InProcessMemoryL1Store, InProcessMemoryL2Store,
    InProcessMemoryL3Store,
};
use crate::memory::{Memory, MemoryL1Store, MemoryL2Store, MemoryL3Store};
use crate::trigger::{FirstSegmentDetector, MemoryPolicies, TopicDetector};

/// The startup registry: every service has an in-process default;
/// registration replaces it. Resolution can never fail.
#[derive(Default)]
pub struct RuntimeBuilder {
    conversation_store: Option<Arc<dyn ConversationStore>>,
    memory_l1_store: Option<Arc<dyn MemoryL1Store>>,
    memory_l2_store: Option<Arc<dyn MemoryL2Store>>,
    memory_l3_store: Option<Arc<dyn MemoryL3Store>>,
    context_assembly: Option<Arc<dyn ContextAssembly>>,
    observers: Vec<Arc<dyn crate::bus::Observer>>,
    memory_policies: MemoryPolicies,
    topic_detector: Option<Arc<dyn TopicDetector>>,
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

    /// Sets the context assembly strategy (default: `LayeredMemory`).
    pub fn context_assembly(mut self, context_assembly: impl ContextAssembly) -> Self {
        self.context_assembly = Some(Arc::new(context_assembly));
        self
    }

    /// Registers an observer of the full event stream. Unlike the other
    /// services there is **no default** — with no observer registered, the
    /// observation lane delivers to nobody (the bus itself still runs).
    pub fn observer(mut self, observer: impl crate::bus::Observer) -> Self {
        self.observers.push(Arc::new(observer));
        self
    }

    /// Sets the memory policies (the resource floors always apply;
    /// `extra` stacks event policies on top).
    pub fn memory_policies(mut self, memory_policies: MemoryPolicies) -> Self {
        self.memory_policies = memory_policies;
        self
    }

    /// Sets the topic detector (default: first-segment heuristic).
    pub fn topic_detector(mut self, topic_detector: impl TopicDetector) -> Self {
        self.topic_detector = Some(Arc::new(topic_detector));
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
            context_assembly: self
                .context_assembly
                .unwrap_or_else(|| Arc::new(LayeredMemory)),
            event_bus: EventBus::new(self.observers),
            memory_policies: self.memory_policies,
            topic_detector: self
                .topic_detector
                .unwrap_or_else(|| Arc::new(FirstSegmentDetector)),
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
    context_assembly: Arc<dyn ContextAssembly>,
    event_bus: EventBus,
    memory_policies: MemoryPolicies,
    topic_detector: Arc<dyn TopicDetector>,
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

    /// The registered (or default) context assembly strategy.
    pub(crate) fn context_assembly(&self) -> Arc<dyn ContextAssembly> {
        Arc::clone(&self.context_assembly)
    }

    /// The event bus (the resident dual-lane dispatch facility).
    pub(crate) fn event_bus(&self) -> &EventBus {
        &self.event_bus
    }

    /// The memory policies (floors always apply).
    pub(crate) fn memory_policies(&self) -> MemoryPolicies {
        self.memory_policies.clone()
    }

    /// The topic detector.
    pub(crate) fn topic_detector(&self) -> Arc<dyn TopicDetector> {
        Arc::clone(&self.topic_detector)
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
                let soft_errors = conversation.end(self);
                if soft_errors.is_empty() {
                    ended += 1;
                }
            }
        }
        ended
    }
}
