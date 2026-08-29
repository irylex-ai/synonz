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

use crate::conversation::ConversationStore;
use crate::inprocess::{InProcessConversationStore, InProcessMemoryStore};
use crate::memory::MemoryStore;

/// The startup registry: every service has an in-process default;
/// registration replaces it. Resolution can never fail.
#[derive(Default)]
pub struct RuntimeBuilder {
    conversation_store: Option<Arc<dyn ConversationStore>>,
    memory: Option<Arc<dyn MemoryStore>>,
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

    /// Builds the runtime with registered or default implementations.
    pub fn build(self) -> SynonzRuntime {
        SynonzRuntime {
            conversation_store: self
                .conversation_store
                .unwrap_or_else(|| Arc::new(InProcessConversationStore::default())),
            memory: self
                .memory
                .unwrap_or_else(|| Arc::new(InProcessMemoryStore::default())),
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
    // Consumed by the context engine and trigger flows (M11b/M11c).
    #[allow(dead_code)]
    pub(crate) fn memory(&self) -> Arc<dyn MemoryStore> {
        Arc::clone(&self.memory)
    }
}
