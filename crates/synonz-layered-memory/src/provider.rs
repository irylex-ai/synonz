//! The component: the builder, the provider registration, and the internal
//! wiring.

use std::sync::Arc;

use synonz::{EventBus, Memory, MemoryContextAssembler, MemoryPipeline, MemoryProvider, Model};

use crate::assembler::{
    LayeredMemoryContextAssembler, LayeredMemoryContextRewriter, LayeredMemoryPromptContextRewriter,
};
use crate::config::{LayeredMemoryConfig, MemoryScopeResolver, SubjectScopeResolver};
use crate::embedding::{Embedding, HashEmbedding};
use crate::l1_memory::{InProcessL1MemoryStore, L1Memory, L1MemoryStore};
use crate::l2_memory::{
    InProcessL2MemoryStore, L2Memory, L2MemoryPromptSummarizer, L2MemoryStore, L2MemorySummarizer,
};
use crate::l3_memory::{
    InProcessL3MemoryGraphStore, InProcessL3MemoryVectorStore, L3Memory, L3MemoryEntityExtractor,
    L3MemoryGraphStore, L3MemoryPromptEntityExtractor, L3MemoryVectorStore,
};
use crate::memory::{LayeredMemory, LayeredMemoryActuator};
use crate::observation::LayeredMemoryObserver;
use crate::pipeline::LayeredMemoryPipeline;
use crate::rewriter::CoreferenceInputRewriterProvider;
use crate::topic_detector::SimilarityTopicDetectorProvider;

/// The official layered memory component: L1 turn entries, L2 batch
/// compaction, and an L3 entity graph with vectors, behind the core's
/// memory contract family.
///
/// Register it on the runtime ([`synonz::RuntimeBuilder::memory_provider`]);
/// register its read-side preprocessing providers on an agent when wanted
/// ([`LayeredMemoryProvider::rewriter_provider`] /
/// [`LayeredMemoryProvider::topic_detector_provider`]); read the typed
/// documents through [`LayeredMemoryProvider::actuator`].
pub struct LayeredMemoryProvider {
    pub(crate) l1: Arc<L1Memory>,
    pub(crate) l2: Arc<L2Memory>,
    pub(crate) l3: Arc<L3Memory>,
    pub(crate) l2_store: Arc<dyn L2MemoryStore>,
    pub(crate) l3_graph: Arc<dyn L3MemoryGraphStore>,
    pub(crate) l3_vectors: Arc<dyn L3MemoryVectorStore>,
    pub(crate) embedding: Arc<dyn Embedding>,
    pub(crate) context_rewriter: Arc<dyn LayeredMemoryContextRewriter>,
    pub(crate) scope_resolver: Arc<dyn MemoryScopeResolver>,
    pub(crate) config: Arc<LayeredMemoryConfig>,
    pub(crate) model: Option<Arc<dyn Model>>,
}

impl LayeredMemoryProvider {
    /// Starts building the component.
    pub fn builder() -> LayeredMemoryProviderBuilder {
        LayeredMemoryProviderBuilder::default()
    }

    /// The actuator: typed reads (L2 entries, L3 entities, L3 relations)
    /// plus relation-level removal.
    pub fn actuator(&self) -> LayeredMemoryActuator {
        LayeredMemoryActuator::new(
            Arc::clone(&self.l2),
            Arc::clone(&self.l3),
            Arc::clone(&self.scope_resolver),
        )
    }

    /// The read-side preprocessing provider (input rewrite), to register on
    /// an agent.
    pub fn rewriter_provider(&self) -> CoreferenceInputRewriterProvider {
        CoreferenceInputRewriterProvider::new(self.model.clone())
    }

    /// The topic detection provider, to register on an agent.
    pub fn topic_detector_provider(&self) -> SimilarityTopicDetectorProvider {
        SimilarityTopicDetectorProvider::new(
            Arc::clone(&self.embedding),
            Arc::clone(&self.config),
            self.model.clone(),
        )
    }
}

impl MemoryProvider for LayeredMemoryProvider {
    fn memory(&self, bus: EventBus) -> Arc<dyn Memory> {
        Arc::new(LayeredMemory::new(
            Arc::clone(&self.l2),
            Arc::clone(&self.l2_store),
            Arc::clone(&self.l3_graph),
            Arc::clone(&self.l3_vectors),
            Arc::clone(&self.embedding),
            Arc::clone(&self.scope_resolver),
            Arc::clone(&self.config),
            bus,
        ))
    }

    fn context_assembler(&self) -> Arc<dyn MemoryContextAssembler> {
        Arc::new(LayeredMemoryContextAssembler::new(
            Arc::clone(&self.l1),
            Arc::clone(&self.l2),
            Arc::clone(&self.l3),
            Arc::clone(&self.embedding),
            Arc::clone(&self.context_rewriter),
            Arc::clone(&self.scope_resolver),
            Arc::clone(&self.config),
        ))
    }

    fn pipeline(&self) -> Arc<dyn MemoryPipeline> {
        Arc::new(LayeredMemoryPipeline::new(
            Arc::clone(&self.l1),
            Arc::clone(&self.l2),
            Arc::clone(&self.l3),
            Arc::clone(&self.config),
        ))
    }

    fn model(&self) -> Option<Arc<dyn Model>> {
        self.model.clone()
    }
}

/// The component's builder: stores, embedding, strategies, configuration,
/// observation, the scope resolver, and the component's model. Every piece
/// has a bundled default, so `build()` works with no configuration.
#[derive(Default)]
pub struct LayeredMemoryProviderBuilder {
    l1_store: Option<Arc<dyn L1MemoryStore>>,
    l2_store: Option<Arc<dyn L2MemoryStore>>,
    l3_graph_store: Option<Arc<dyn L3MemoryGraphStore>>,
    l3_vector_store: Option<Arc<dyn L3MemoryVectorStore>>,
    embedding: Option<Arc<dyn Embedding>>,
    summarizer: Option<Arc<dyn L2MemorySummarizer>>,
    entity_extractor: Option<Arc<dyn L3MemoryEntityExtractor>>,
    context_rewriter: Option<Arc<dyn LayeredMemoryContextRewriter>>,
    scope_resolver: Option<Arc<dyn MemoryScopeResolver>>,
    config: LayeredMemoryConfig,
    observer: Option<Arc<dyn LayeredMemoryObserver>>,
    model: Option<Arc<dyn Model>>,
}

impl LayeredMemoryProviderBuilder {
    /// Sets the L1 working-memory store (default: the bundled in-process
    /// store, sized by the configuration).
    pub fn l1_store(mut self, store: impl L1MemoryStore) -> Self {
        self.l1_store = Some(Arc::new(store));
        self
    }

    /// Sets the L2 entry store (default: the bundled in-process store,
    /// sized by the configuration).
    pub fn l2_store(mut self, store: impl L2MemoryStore) -> Self {
        self.l2_store = Some(Arc::new(store));
        self
    }

    /// Sets the L3 graph store (default: the bundled in-process store).
    pub fn l3_graph_store(mut self, store: impl L3MemoryGraphStore) -> Self {
        self.l3_graph_store = Some(Arc::new(store));
        self
    }

    /// Sets the L3 vector store (default: the bundled in-process store).
    pub fn l3_vector_store(mut self, store: impl L3MemoryVectorStore) -> Self {
        self.l3_vector_store = Some(Arc::new(store));
        self
    }

    /// Sets the embedding port (default: the bundled deterministic
    /// embedding).
    pub fn embedding(mut self, embedding: impl Embedding + 'static) -> Self {
        self.embedding = Some(Arc::new(embedding));
        self
    }

    /// Sets the batch summarizer (default: the prompt-driven summarizer).
    pub fn summarizer(mut self, summarizer: impl L2MemorySummarizer) -> Self {
        self.summarizer = Some(Arc::new(summarizer));
        self
    }

    /// Sets the entity extractor (default: the prompt-driven extractor).
    pub fn entity_extractor(mut self, extractor: impl L3MemoryEntityExtractor) -> Self {
        self.entity_extractor = Some(Arc::new(extractor));
        self
    }

    /// Sets the context rewriter (default: the prompt-driven rewriter).
    pub fn context_rewriter(mut self, rewriter: impl LayeredMemoryContextRewriter) -> Self {
        self.context_rewriter = Some(Arc::new(rewriter));
        self
    }

    /// Sets the long-term partition resolver (default: one per-subject
    /// partition).
    pub fn scope_resolver(mut self, resolver: impl MemoryScopeResolver) -> Self {
        self.scope_resolver = Some(Arc::new(resolver));
        self
    }

    /// Sets the component's tunables (default: the documented defaults).
    pub fn config(mut self, config: LayeredMemoryConfig) -> Self {
        self.config = config;
        self
    }

    /// Sets the component's observation face (default: none).
    pub fn observer(mut self, observer: impl LayeredMemoryObserver + 'static) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }

    /// Sets the component's context-management model (default: none — the
    /// framework falls back to the agent's model for per-turn work; the
    /// conversation-end maintenance requires it).
    pub fn model(mut self, model: impl Model + 'static) -> Self {
        self.model = Some(Arc::new(model));
        self
    }

    /// Builds the component.
    pub fn build(self) -> LayeredMemoryProvider {
        let config = Arc::new(self.config);
        let l1_store = self
            .l1_store
            .unwrap_or_else(|| Arc::new(InProcessL1MemoryStore::new(config.l1_turns)));
        let l2_store = self.l2_store.unwrap_or_else(|| {
            Arc::new(InProcessL2MemoryStore::new(
                config.l2_entries,
                config.l2_versions,
            ))
        });
        let l3_graph_store = self
            .l3_graph_store
            .unwrap_or_else(|| Arc::new(InProcessL3MemoryGraphStore::default()));
        let l3_vector_store = self
            .l3_vector_store
            .unwrap_or_else(|| Arc::new(InProcessL3MemoryVectorStore::default()));
        let embedding: Arc<dyn Embedding> = self
            .embedding
            .unwrap_or_else(|| Arc::new(HashEmbedding::default()));
        let summarizer: Arc<dyn L2MemorySummarizer> = self
            .summarizer
            .unwrap_or_else(|| Arc::new(L2MemoryPromptSummarizer));
        let extractor: Arc<dyn L3MemoryEntityExtractor> = self
            .entity_extractor
            .unwrap_or_else(|| Arc::new(L3MemoryPromptEntityExtractor));
        let context_rewriter: Arc<dyn LayeredMemoryContextRewriter> = self
            .context_rewriter
            .unwrap_or_else(|| Arc::new(LayeredMemoryPromptContextRewriter));
        let scope_resolver: Arc<dyn MemoryScopeResolver> = self
            .scope_resolver
            .unwrap_or_else(|| Arc::new(SubjectScopeResolver));
        let l1 = Arc::new(L1Memory::new(Arc::clone(&l1_store)));
        let l2 = Arc::new(L2Memory::new(
            Arc::clone(&l2_store),
            Arc::clone(&summarizer),
            Arc::clone(&embedding),
            Arc::clone(&config),
            self.observer.clone(),
        ));
        let l3 = Arc::new(L3Memory::new(
            Arc::clone(&l3_graph_store),
            Arc::clone(&l3_vector_store),
            Arc::clone(&extractor),
            Arc::clone(&embedding),
            Arc::clone(&config),
            Arc::clone(&scope_resolver),
            self.observer.clone(),
        ));
        LayeredMemoryProvider {
            l1,
            l2,
            l3,
            l2_store,
            l3_graph: l3_graph_store,
            l3_vectors: l3_vector_store,
            embedding,
            context_rewriter,
            scope_resolver,
            config,
            model: self.model,
        }
    }
}
