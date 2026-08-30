//! The process-level runtime: explicit bootstrap, startup registry, and
//! default implementations (ADR-0012 决策四).
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

use crate::context::{ContextAssembly, LayeredMemory};
use crate::conversation::{Conversation, ConversationStore};
use crate::inprocess::{InProcessConversationStore, InProcessMemoryStore};
use crate::memory::MemoryStore;
use crate::trigger::{FirstSegmentDetector, MemoryPolicies, TopicDetector};

/// The startup registry: every service has an in-process default;
/// registration replaces it. Resolution can never fail.
#[derive(Default)]
pub struct RuntimeBuilder {
    conversation_store: Option<Arc<dyn ConversationStore>>,
    memory: Option<Arc<dyn MemoryStore>>,
    assembly: Option<Arc<dyn ContextAssembly>>,
    policies: MemoryPolicies,
    detector: Option<Arc<dyn TopicDetector>>,
    idle_timeout: Option<Duration>,
}

impl RuntimeBuilder {
    /// Starts building a runtime.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a conversation store (default: in-process).
    pub fn register_conversation_store(mut self, store: impl ConversationStore) -> Self {
        self.conversation_store = Some(Arc::new(store));
        self
    }

    /// Registers a memory store (default: in-process).
    pub fn register_memory(mut self, memory: impl MemoryStore) -> Self {
        self.memory = Some(Arc::new(memory));
        self
    }

    /// Registers an assembly strategy (default: `LayeredMemory`).
    pub fn register_assembly(mut self, assembly: impl ContextAssembly) -> Self {
        self.assembly = Some(Arc::new(assembly));
        self
    }

    /// Sets the memory policies (the resource floors always apply;
    /// `extra` stacks event policies on top).
    pub fn memory_policies(mut self, policies: MemoryPolicies) -> Self {
        self.policies = policies;
        self
    }

    /// Registers a topic detector (default: first-segment heuristic).
    pub fn register_topic_detector(mut self, detector: impl TopicDetector) -> Self {
        self.detector = Some(Arc::new(detector));
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
            conversation_store: self
                .conversation_store
                .unwrap_or_else(|| Arc::new(InProcessConversationStore::default())),
            memory: self
                .memory
                .unwrap_or_else(|| Arc::new(InProcessMemoryStore::default())),
            assembly: self.assembly.unwrap_or_else(|| Arc::new(LayeredMemory)),
            policies: self.policies,
            detector: self
                .detector
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
    conversation_store: Arc<dyn ConversationStore>,
    memory: Arc<dyn MemoryStore>,
    assembly: Arc<dyn ContextAssembly>,
    policies: MemoryPolicies,
    detector: Arc<dyn TopicDetector>,
    idle_timeout: Option<Duration>,
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

    /// The registered (or default) memory store.
    ///
    /// Public for the Low Level track: direct store access (diagnostics,
    /// custom stores, explicit operations) alongside the framework's own
    /// orchestration.
    pub fn memory(&self) -> Arc<dyn MemoryStore> {
        Arc::clone(&self.memory)
    }

    /// The registered (or default) assembly strategy.
    pub(crate) fn assembly(&self) -> Arc<dyn ContextAssembly> {
        Arc::clone(&self.assembly)
    }

    /// The memory policies (floors always apply).
    pub(crate) fn memory_policies(&self) -> MemoryPolicies {
        self.policies.clone()
    }

    /// The topic detector.
    pub(crate) fn topic_detector(&self) -> Arc<dyn TopicDetector> {
        Arc::clone(&self.detector)
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
            // Rebuild the conversation on this runtime to run its flows.
            let subject = crate::Subject::of(crate::SubjectType::User, &state.subject_id);
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
