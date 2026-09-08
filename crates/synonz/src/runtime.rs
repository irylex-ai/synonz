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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::context::{ContextAssembly, LayeredMemory};
use crate::conversation::{Conversation, ConversationStore};
use crate::inprocess::{InProcessConversationStore, InProcessMemoryStore};
use crate::memory::MemoryStore;
use crate::trigger::{FirstSegmentDetector, MemoryPolicies, TopicDetector};

static RUNTIME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The startup registry: every service has an in-process default;
/// registration replaces it. Resolution can never fail.
#[derive(Default)]
pub struct RuntimeBuilder {
    conversation_store: Option<Arc<dyn ConversationStore>>,
    memory_store: Option<Arc<dyn MemoryStore>>,
    context_assembly: Option<Arc<dyn ContextAssembly>>,
    observers: Vec<Arc<dyn crate::observer::Observer>>,
    memory_policies: MemoryPolicies,
    topic_detector: Option<Arc<dyn TopicDetector>>,
    idle_timeout: Option<Duration>,
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

    /// Sets the memory store (default: in-process).
    pub fn memory_store(mut self, memory_store: impl MemoryStore) -> Self {
        self.memory_store = Some(Arc::new(memory_store));
        self
    }

    /// Sets the context assembly strategy (default: `LayeredMemory`).
    pub fn context_assembly(mut self, context_assembly: impl ContextAssembly) -> Self {
        self.context_assembly = Some(Arc::new(context_assembly));
        self
    }

    /// Registers an observer of the full event stream. Unlike the other
    /// services there is **no default** — with no observer registered, the
    /// observation face is fully closed. Observability is additionally
    /// gated per agent (`AgentBuilder::observability`).
    pub fn observer(mut self, observer: impl crate::observer::Observer) -> Self {
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
    pub fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }

    /// Builds the runtime with registered or default implementations.
    pub fn build(self) -> SynonzRuntime {
        SynonzRuntime {
            id: RUNTIME_COUNTER.fetch_add(1, Ordering::Relaxed),
            conversation_store: self
                .conversation_store
                .unwrap_or_else(|| Arc::new(InProcessConversationStore::default())),
            memory_store: self
                .memory_store
                .unwrap_or_else(|| Arc::new(InProcessMemoryStore::default())),
            context_assembly: self
                .context_assembly
                .unwrap_or_else(|| Arc::new(LayeredMemory)),
            observers: self.observers.into(),
            memory_policies: self.memory_policies,
            topic_detector: self
                .topic_detector
                .unwrap_or_else(|| Arc::new(FirstSegmentDetector)),
            idle_timeout: self.idle_timeout,
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
    /// Process-unique identity: clones share it; separate `build()` calls
    /// never do. The execution entry compares this between the agent and
    /// the conversation to reject cross-runtime mixing.
    id: u64,
    conversation_store: Arc<dyn ConversationStore>,
    memory_store: Arc<dyn MemoryStore>,
    context_assembly: Arc<dyn ContextAssembly>,
    observers: Arc<[Arc<dyn crate::observer::Observer>]>,
    memory_policies: MemoryPolicies,
    topic_detector: Arc<dyn TopicDetector>,
    idle_timeout: Option<Duration>,
}

impl SynonzRuntime {
    /// Starts building a runtime.
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }

    /// The runtime's process-unique identity.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// The registered (or default) conversation store.
    pub(crate) fn conversation_store(&self) -> Arc<dyn ConversationStore> {
        Arc::clone(&self.conversation_store)
    }

    /// The registered (or default) memory store.
    ///
    /// Public for the Low Level track: direct store access (diagnostics,
    /// custom stores, explicit operations) alongside the framework's own
    /// orchestration.
    pub fn memory_store(&self) -> Arc<dyn MemoryStore> {
        Arc::clone(&self.memory_store)
    }

    /// The registered (or default) context assembly strategy.
    pub(crate) fn context_assembly(&self) -> Arc<dyn ContextAssembly> {
        Arc::clone(&self.context_assembly)
    }

    /// The registered observers (empty = the observation face is closed).
    pub(crate) fn observers(&self) -> &[Arc<dyn crate::observer::Observer>] {
        &self.observers
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
        let Some(timeout) = self.idle_timeout else {
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
                let soft_errors = conversation.end();
                if soft_errors.is_empty() {
                    ended += 1;
                }
            }
        }
        ended
    }
}
