//! Shared test doubles and the fixture wiring for the component's
//! integration tests.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use synonz::{
    Conversation, MemoryScope, Message, Model, ModelError, ModelRequest, ModelStream,
    ModelStreamItem, Observer, ObserverContext, Subject, SubjectType, SynonzEvent, SynonzRuntime,
    TokenUsage, TopicDetectInput, TopicDetector, TopicDetectorProvider,
};
use synonz_layered_memory::{
    InProcessL1MemoryStore, InProcessL2MemoryStore, InProcessL3MemoryGraphStore,
    InProcessL3MemoryVectorStore, L2MemorySummarizer, L2MemorySummaryOutput,
    L3MemoryEntityExtractor, L3MemoryExtractionInput, L3MemoryGraph, LayeredMemoryActuator,
    LayeredMemoryConfig, LayeredMemoryContextRewriteInput, LayeredMemoryContextRewriter,
    LayeredMemoryEvent, LayeredMemoryObserver, LayeredMemoryProvider, MemoryScopeResolver,
};

// ────────────────────────── models ──────────────────────────

/// A model that records every request it receives and answers with fixed
/// text.
#[derive(Clone)]
pub struct RecordingModel {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    reply: String,
}

impl RecordingModel {
    pub fn new(reply: impl Into<String>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            reply: reply.into(),
        }
    }

    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// The user-role texts of the last recorded request.
    pub fn last_user_text(&self) -> String {
        self.requests
            .lock()
            .unwrap()
            .last()
            .map(|request| {
                request
                    .messages
                    .iter()
                    .filter(|message| message.role == synonz::Role::User)
                    .flat_map(|message| {
                        message.blocks.iter().filter_map(|block| match block {
                            synonz::ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
    }
}

impl Model for RecordingModel {
    fn stream(&self, request: ModelRequest) -> BoxFuture<'_, Result<ModelStream, ModelError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.reply.clone();
        Box::pin(async move {
            use futures::StreamExt;
            Ok(futures::stream::iter(vec![ModelStreamItem::Finish {
                message: Message::assistant_text(reply),
                usage: TokenUsage::new(1, 1),
            }])
            .boxed())
        })
    }
}

// ────────────────────────── strategies ──────────────────────────

/// One scripted agent reply per run.
pub fn mock(replies: &[&str]) -> synonz::MockModel {
    synonz::MockModel::new(
        replies
            .iter()
            .map(|reply| {
                vec![ModelStreamItem::Finish {
                    message: Message::assistant_text(*reply),
                    usage: TokenUsage::new(1, 1),
                }]
            })
            .collect(),
    )
}

/// A summarizer playing queued outputs (empty queue = no entries).
pub struct StubSummarizer {
    outputs: Mutex<VecDeque<L2MemorySummaryOutput>>,
}

impl StubSummarizer {
    pub fn new(outputs: Vec<L2MemorySummaryOutput>) -> Self {
        Self {
            outputs: Mutex::new(outputs.into_iter().collect()),
        }
    }
}

impl L2MemorySummarizer for StubSummarizer {
    fn summarize<'a>(
        &'a self,
        _input: synonz_layered_memory::L2MemorySummaryInput<'a>,
    ) -> BoxFuture<'a, Result<L2MemorySummaryOutput, synonz::MemoryFailure>> {
        let output = self.outputs.lock().unwrap().pop_front().unwrap_or_default();
        Box::pin(async move { Ok(output) })
    }
}

/// An extractor playing queued graphs (empty queue = an empty graph).
pub struct StubExtractor {
    graphs: Mutex<VecDeque<L3MemoryGraph>>,
}

impl StubExtractor {
    pub fn new(graphs: Vec<L3MemoryGraph>) -> Self {
        Self {
            graphs: Mutex::new(graphs.into_iter().collect()),
        }
    }
}

impl L3MemoryEntityExtractor for StubExtractor {
    fn extract<'a>(
        &'a self,
        _input: L3MemoryExtractionInput<'a>,
    ) -> BoxFuture<'a, Result<L3MemoryGraph, synonz::MemoryFailure>> {
        let graph = self.graphs.lock().unwrap().pop_front().unwrap_or_default();
        Box::pin(async move { Ok(graph) })
    }
}

/// A context rewriter echoing the recalled items into the enhanced input.
pub struct EchoRewriter {
    pub seen: Arc<Mutex<Vec<Vec<String>>>>,
}

impl Default for EchoRewriter {
    fn default() -> Self {
        Self {
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl LayeredMemoryContextRewriter for EchoRewriter {
    fn rewrite<'a>(
        &'a self,
        input: LayeredMemoryContextRewriteInput<'a>,
    ) -> BoxFuture<'a, Result<String, synonz::MemoryFailure>> {
        self.seen.lock().unwrap().push(input.recalled.to_vec());
        let recalled = input.recalled.join("; ");
        let enhanced = format!("{} [recalled: {recalled}]", input.input);
        Box::pin(async move { Ok(enhanced) })
    }
}

/// A detector with scripted verdicts (None = keep).
pub struct StubDetector {
    verdicts: Mutex<VecDeque<Option<String>>>,
}

impl StubDetector {
    pub fn new(verdicts: Vec<Option<String>>) -> Self {
        Self {
            verdicts: Mutex::new(verdicts.into_iter().collect()),
        }
    }
}

impl TopicDetector for StubDetector {
    fn detect<'a>(
        &'a self,
        _input: TopicDetectInput<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, synonz::MemoryFailure>> {
        let verdict = self.verdicts.lock().unwrap().pop_front().flatten();
        Box::pin(async move { Ok(verdict) })
    }
}

/// The detector's factory for agent registration.
pub struct StubDetectorProvider {
    pub detector: Arc<dyn TopicDetector>,
}

impl TopicDetectorProvider for StubDetectorProvider {
    fn topic_detector(&self) -> Arc<dyn TopicDetector> {
        Arc::clone(&self.detector)
    }
}

// ────────────────────────── logs ──────────────────────────

/// Records the component's progress facts.
#[derive(Default, Clone)]
pub struct ProgressLog {
    pub events: Arc<Mutex<Vec<LayeredMemoryEvent>>>,
}

impl LayeredMemoryObserver for ProgressLog {
    fn on_progress(&self, event: &LayeredMemoryEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

/// Records the core's memory facts.
#[derive(Default, Clone)]
pub struct FactLog {
    pub facts: Arc<Mutex<Vec<String>>>,
}

impl Observer for FactLog {
    fn on_event(&self, _ctx: &ObserverContext, event: &SynonzEvent) {
        let fact = match event {
            SynonzEvent::Memory(synonz::MemoryEvent::TurnArchived { .. }) => {
                Some("archived".to_string())
            }
            SynonzEvent::Memory(synonz::MemoryEvent::Updated { scope, id, .. }) => {
                Some(format!("updated:{scope}:{id}"))
            }
            SynonzEvent::Memory(synonz::MemoryEvent::Removed { scope, ids, .. }) => {
                Some(format!("removed:{scope}:{}", ids.join(",")))
            }
            SynonzEvent::Memory(synonz::MemoryEvent::Failed { stage, .. }) => {
                Some(format!("failed:{stage}"))
            }
            SynonzEvent::Conversation(synonz::ConversationEvent::TopicShifted { .. }) => {
                Some("shifted".to_string())
            }
            _ => None,
        };
        if let Some(fact) = fact {
            self.facts.lock().unwrap().push(fact);
        }
    }
}

// ────────────────────────── fixture ──────────────────────────

/// The fixture's injectable parts.
#[derive(Default)]
pub struct Parts {
    pub summarizer: Option<Arc<dyn L2MemorySummarizer>>,
    pub extractor: Option<Arc<dyn L3MemoryEntityExtractor>>,
    pub context_rewriter: Option<Arc<dyn LayeredMemoryContextRewriter>>,
    pub embedding: Option<Arc<dyn synonz_layered_memory::Embedding>>,
    pub scope_resolver: Option<Arc<dyn MemoryScopeResolver>>,
    pub model: Option<Arc<dyn Model>>,
    /// Leaves the provider without a context-management model (the
    /// framework still falls back to the agent's model per turn; the
    /// conversation-end maintenance then fails explicitly).
    pub without_model: bool,
    pub config: Option<LayeredMemoryConfig>,
}

/// A wired component holding its stores for inspection.
pub struct Fixture {
    pub runtime: SynonzRuntime,
    pub subject: Subject,
    pub l1: Arc<InProcessL1MemoryStore>,
    pub l2: Arc<InProcessL2MemoryStore>,
    pub graph: Arc<InProcessL3MemoryGraphStore>,
    pub vectors: Arc<InProcessL3MemoryVectorStore>,
    pub actuator: LayeredMemoryActuator,
    pub progress: ProgressLog,
    pub facts: FactLog,
}

pub fn fixture(parts: Parts) -> Fixture {
    let config = parts.config.unwrap_or_default();
    let l1 = Arc::new(InProcessL1MemoryStore::new(config.l1_turns));
    let l2 = Arc::new(InProcessL2MemoryStore::new(
        config.l2_entries,
        config.l2_versions,
    ));
    let graph = Arc::new(InProcessL3MemoryGraphStore::default());
    let vectors = Arc::new(InProcessL3MemoryVectorStore::default());
    let progress = ProgressLog::default();
    let facts = FactLog::default();
    let mut builder = LayeredMemoryProvider::builder()
        .l1_store(InProcessL1MemoryStoreView(Arc::clone(&l1)))
        .l2_store(InProcessL2MemoryStoreView(Arc::clone(&l2)))
        .l3_graph_store(InProcessL3MemoryGraphStoreView(Arc::clone(&graph)))
        .l3_vector_store(InProcessL3MemoryVectorStoreView(Arc::clone(&vectors)))
        .config(config)
        .observer(progress.clone());
    if let Some(summarizer) = parts.summarizer {
        builder = builder.summarizer(SharedSummarizer(summarizer));
    }
    if let Some(extractor) = parts.extractor {
        builder = builder.entity_extractor(SharedExtractor(extractor));
    }
    if let Some(rewriter) = parts.context_rewriter {
        builder = builder.context_rewriter(SharedRewriter(rewriter));
    }
    if let Some(embedding) = parts.embedding {
        builder = builder.embedding(SharedEmbedding(embedding));
    }
    if let Some(resolver) = parts.scope_resolver {
        builder = builder.scope_resolver(SharedResolver(resolver));
    }
    if let Some(model) = parts.model {
        builder = builder.model(SharedModel(model));
    } else if !parts.without_model {
        builder = builder.model(SharedModel(Arc::new(RecordingModel::new("ok"))));
    }
    let provider = builder.build();
    let actuator = provider.actuator();
    let runtime = SynonzRuntime::builder()
        .memory_provider(provider)
        .observer(facts.clone())
        .build();
    Fixture {
        runtime,
        subject: Subject::of(SubjectType::User, "u-layered"),
        l1,
        l2,
        graph,
        vectors,
        actuator,
        progress,
        facts,
    }
}

impl Fixture {
    pub fn conversation(&self) -> Conversation {
        Conversation::new(&self.runtime, &self.subject)
    }

    pub fn conversation_scope(&self, conversation: &Conversation) -> MemoryScope {
        MemoryScope::new(format!("conversation:{}", conversation.id()))
    }
}

// Arc-backed forwarding wrappers so the fixture can share its store
// instances with the component (the builder consumes `impl Trait` values).
pub struct InProcessL1MemoryStoreView(pub Arc<InProcessL1MemoryStore>);
impl synonz_layered_memory::L1MemoryStore for InProcessL1MemoryStoreView {
    fn append(
        &self,
        entry: synonz_layered_memory::L1MemoryEntry,
    ) -> Result<(), synonz::MemoryStoreError> {
        self.0.append(entry)
    }
    fn recent(
        &self,
        scope: &MemoryScope,
        turns: usize,
    ) -> Result<Vec<synonz_layered_memory::L1MemoryEntry>, synonz::MemoryStoreError> {
        self.0.recent(scope, turns)
    }
    fn count(&self, scope: &MemoryScope) -> Result<usize, synonz::MemoryStoreError> {
        self.0.count(scope)
    }
    fn remove(&self, scope: &MemoryScope, id: &str) -> Result<bool, synonz::MemoryStoreError> {
        self.0.remove(scope, id)
    }
    fn clear(&self, scope: &MemoryScope) -> Result<(), synonz::MemoryStoreError> {
        self.0.clear(scope)
    }
}

pub struct InProcessL2MemoryStoreView(pub Arc<InProcessL2MemoryStore>);
impl synonz_layered_memory::L2MemoryStore for InProcessL2MemoryStoreView {
    fn upsert(
        &self,
        entry: synonz_layered_memory::L2MemoryEntry,
    ) -> Result<(), synonz::MemoryStoreError> {
        self.0.upsert(entry)
    }
    fn get(
        &self,
        scope: &MemoryScope,
        id: &str,
    ) -> Result<Option<synonz_layered_memory::L2MemoryEntry>, synonz::MemoryStoreError> {
        self.0.get(scope, id)
    }
    fn list(
        &self,
        scope: &MemoryScope,
    ) -> Result<Vec<synonz_layered_memory::L2MemoryEntry>, synonz::MemoryStoreError> {
        self.0.list(scope)
    }
    fn scopes(&self, subject: &Subject) -> Result<Vec<MemoryScope>, synonz::MemoryStoreError> {
        self.0.scopes(subject)
    }
    fn remove(&self, scope: &MemoryScope, id: &str) -> Result<bool, synonz::MemoryStoreError> {
        self.0.remove(scope, id)
    }
    fn count(&self, scope: &MemoryScope) -> Result<usize, synonz::MemoryStoreError> {
        self.0.count(scope)
    }
}

pub struct InProcessL3MemoryGraphStoreView(pub Arc<InProcessL3MemoryGraphStore>);
impl synonz_layered_memory::L3MemoryGraphStore for InProcessL3MemoryGraphStoreView {
    fn upsert_entity(
        &self,
        entity: synonz_layered_memory::L3MemoryGraphEntity,
    ) -> Result<(), synonz::MemoryStoreError> {
        self.0.upsert_entity(entity)
    }
    fn get_entity(
        &self,
        scope: &MemoryScope,
        canonical_name: &str,
    ) -> Result<Option<synonz_layered_memory::L3MemoryGraphEntity>, synonz::MemoryStoreError> {
        self.0.get_entity(scope, canonical_name)
    }
    fn get_entity_by_id(
        &self,
        id: &str,
    ) -> Result<Option<synonz_layered_memory::L3MemoryGraphEntity>, synonz::MemoryStoreError> {
        self.0.get_entity_by_id(id)
    }
    fn entities(
        &self,
        scope: &MemoryScope,
    ) -> Result<Vec<synonz_layered_memory::L3MemoryGraphEntity>, synonz::MemoryStoreError> {
        self.0.entities(scope)
    }
    fn upsert_edge(
        &self,
        edge: synonz_layered_memory::L3MemoryGraphEdge,
    ) -> Result<(), synonz::MemoryStoreError> {
        self.0.upsert_edge(edge)
    }
    fn edges_of(
        &self,
        scope: &MemoryScope,
        name: &str,
    ) -> Result<Vec<synonz_layered_memory::L3MemoryGraphEdge>, synonz::MemoryStoreError> {
        self.0.edges_of(scope, name)
    }
    fn remove_entity_by_id(&self, id: &str) -> Result<bool, synonz::MemoryStoreError> {
        self.0.remove_entity_by_id(id)
    }
    fn remove_edge(
        &self,
        scope: &MemoryScope,
        from: &str,
        relation_type: &str,
        to: &str,
    ) -> Result<bool, synonz::MemoryStoreError> {
        self.0.remove_edge(scope, from, relation_type, to)
    }
    fn count(&self, scope: &MemoryScope) -> Result<usize, synonz::MemoryStoreError> {
        self.0.count(scope)
    }
}

pub struct InProcessL3MemoryVectorStoreView(pub Arc<InProcessL3MemoryVectorStore>);
impl synonz_layered_memory::L3MemoryVectorStore for InProcessL3MemoryVectorStoreView {
    fn upsert(
        &self,
        scope: &MemoryScope,
        canonical_name: &str,
        vector: Vec<f32>,
    ) -> Result<(), synonz::MemoryStoreError> {
        self.0.upsert(scope, canonical_name, vector)
    }
    fn remove(
        &self,
        scope: &MemoryScope,
        canonical_name: &str,
    ) -> Result<bool, synonz::MemoryStoreError> {
        self.0.remove(scope, canonical_name)
    }
    fn search(
        &self,
        scope: &MemoryScope,
        vector: &[f32],
        budget: usize,
    ) -> Result<Vec<(String, f32)>, synonz::MemoryStoreError> {
        self.0.search(scope, vector, budget)
    }
}

pub struct SharedSummarizer(pub Arc<dyn L2MemorySummarizer>);
impl L2MemorySummarizer for SharedSummarizer {
    fn summarize<'a>(
        &'a self,
        input: synonz_layered_memory::L2MemorySummaryInput<'a>,
    ) -> BoxFuture<'a, Result<L2MemorySummaryOutput, synonz::MemoryFailure>> {
        self.0.summarize(input)
    }
}

pub struct SharedExtractor(pub Arc<dyn L3MemoryEntityExtractor>);
impl L3MemoryEntityExtractor for SharedExtractor {
    fn extract<'a>(
        &'a self,
        input: L3MemoryExtractionInput<'a>,
    ) -> BoxFuture<'a, Result<L3MemoryGraph, synonz::MemoryFailure>> {
        self.0.extract(input)
    }
}

pub struct SharedRewriter(pub Arc<dyn LayeredMemoryContextRewriter>);
impl LayeredMemoryContextRewriter for SharedRewriter {
    fn rewrite<'a>(
        &'a self,
        input: LayeredMemoryContextRewriteInput<'a>,
    ) -> BoxFuture<'a, Result<String, synonz::MemoryFailure>> {
        self.0.rewrite(input)
    }
}

pub struct SharedEmbedding(pub Arc<dyn synonz_layered_memory::Embedding>);
impl synonz_layered_memory::Embedding for SharedEmbedding {
    fn embed<'a>(
        &'a self,
        text: &'a str,
    ) -> BoxFuture<'a, Result<Vec<f32>, synonz::MemoryFailure>> {
        self.0.embed(text)
    }

    fn embed_inline(&self, text: &str) -> Option<Result<Vec<f32>, synonz::MemoryFailure>> {
        self.0.embed_inline(text)
    }
}

pub struct SharedResolver(pub Arc<dyn MemoryScopeResolver>);
impl MemoryScopeResolver for SharedResolver {
    fn resolve(&self, subject: &Subject, conversation_id: &str) -> Vec<MemoryScope> {
        self.0.resolve(subject, conversation_id)
    }
}

pub struct SharedModel(pub Arc<dyn Model>);
impl Model for SharedModel {
    fn stream(&self, request: ModelRequest) -> BoxFuture<'_, Result<ModelStream, ModelError>> {
        self.0.stream(request)
    }
}
