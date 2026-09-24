//! L3 long-term memory: a cross-conversation entity graph plus entity
//! vectors, distilled from the user side of compacted batches.
//!
//! The graph is the memory (entities + edges); the vectors are its recall
//! index (keyed by canonical name). Entity and relation types are opaque
//! strings constrained by the configured [`L3MemorySchema`] — the component
//! maps out-of-vocabulary values to the configured fallbacks instead of
//! hardcoding any domain vocabulary.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use synonz::{
    MemoryFailure, MemoryScope, MemoryStoreError, Message, Model, ModelRequest, Subject, complete,
};

use crate::config::{LayeredMemoryConfig, MemoryScopeResolver};
use crate::embedding::Embedding;
use crate::l1_memory::L1MemoryEntry;
use crate::l2_memory::L2MemoryEntry;
use crate::observation::{LayeredMemoryEvent, LayeredMemoryObserver};
use crate::utils::{conversation_scope, cosine, message_text, now_epoch};

/// The configured entity/relation vocabulary. Field values stay opaque
/// strings; the schema defines what the extraction prompt may use and which
/// fallback the component maps unknown values to. An empty list means "no
/// constraint" (values pass through unchanged).
#[derive(Debug, Clone, PartialEq)]
pub struct L3MemorySchema {
    /// The allowed entity types.
    pub entity_types: Vec<String>,
    /// The allowed relation types.
    pub relation_types: Vec<String>,
    /// The entity type used for values outside the vocabulary.
    pub entity_type_fallback: String,
    /// The relation type used for values outside the vocabulary.
    pub relation_type_fallback: String,
}

impl Default for L3MemorySchema {
    fn default() -> Self {
        Self {
            entity_types: [
                "person",
                "organization",
                "location",
                "topic",
                "event",
                "preference",
                "other",
            ]
            .iter()
            .map(|value| value.to_string())
            .collect(),
            relation_types: [
                "likes",
                "dislikes",
                "owns",
                "wants",
                "belongs_to",
                "related_to",
            ]
            .iter()
            .map(|value| value.to_string())
            .collect(),
            entity_type_fallback: "other".to_string(),
            relation_type_fallback: "related_to".to_string(),
        }
    }
}

impl L3MemorySchema {
    /// Maps one entity type to the configured canonical spelling (or the
    /// fallback).
    pub fn normalize_entity_type(&self, value: &str) -> String {
        normalize(value, &self.entity_types, &self.entity_type_fallback)
    }

    /// Maps one relation type to the configured canonical spelling (or the
    /// fallback).
    pub fn normalize_relation_type(&self, value: &str) -> String {
        normalize(value, &self.relation_types, &self.relation_type_fallback)
    }
}

fn normalize(value: &str, vocabulary: &[String], fallback: &str) -> String {
    let trimmed = value.trim();
    if vocabulary.is_empty() {
        return trimmed.to_string();
    }
    vocabulary
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(trimmed))
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

/// One entity node.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct L3MemoryGraphEntity {
    /// The record's stable id (generated at write; preserved on same-key
    /// replace by the store).
    pub id: String,
    /// The canonical name (the node's identity within its scope).
    pub canonical_name: String,
    /// Alternative names merged into this entity.
    pub aliases: Vec<String>,
    /// The configured entity type (an opaque string).
    pub entity_type: String,
    /// A short description.
    pub description: String,
    /// The partition the entity belongs to.
    pub scope: MemoryScope,
    /// Epoch seconds at which the entity first appeared.
    pub created_at: u64,
    /// Epoch seconds at which the entity was last updated.
    pub updated_at: u64,
}

impl L3MemoryGraphEntity {
    /// Creates a stored entity (the id is generated here; stores only
    /// persist it).
    pub fn new(
        canonical_name: impl Into<String>,
        entity_type: impl Into<String>,
        description: impl Into<String>,
        scope: MemoryScope,
        created_at: u64,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            canonical_name: canonical_name.into(),
            aliases: Vec::new(),
            entity_type: entity_type.into(),
            description: description.into(),
            scope,
            created_at,
            updated_at: created_at,
        }
    }

    /// Creates an extraction draft: `scope`, `id` and timestamps are stamped
    /// by the component at write time.
    pub fn extracted(
        canonical_name: impl Into<String>,
        entity_type: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self::new(
            canonical_name,
            entity_type,
            description,
            MemoryScope::new(""),
            0,
        )
    }
}

/// One relation edge.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct L3MemoryGraphEdge {
    /// The source entity's canonical name.
    pub from: String,
    /// The configured relation type (an opaque string).
    pub relation_type: String,
    /// The target entity's canonical name.
    pub to: String,
    /// The partition the edge belongs to.
    pub scope: MemoryScope,
    /// Epoch seconds at which the edge was last updated.
    pub updated_at: u64,
}

impl L3MemoryGraphEdge {
    /// Creates a stored edge.
    pub fn new(
        from: impl Into<String>,
        relation_type: impl Into<String>,
        to: impl Into<String>,
        scope: MemoryScope,
        updated_at: u64,
    ) -> Self {
        Self {
            from: from.into(),
            relation_type: relation_type.into(),
            to: to.into(),
            scope,
            updated_at,
        }
    }

    /// Creates an extraction draft: `scope` and the timestamp are stamped by
    /// the component at write time.
    pub fn extracted(
        from: impl Into<String>,
        relation_type: impl Into<String>,
        to: impl Into<String>,
    ) -> Self {
        Self::new(from, relation_type, to, MemoryScope::new(""), 0)
    }
}

/// The L3 memory unit: entities (nodes) plus edges. It appears as the
/// extraction output (a draft, pre-normalization) and as a recalled
/// subgraph.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct L3MemoryGraph {
    /// The graph's entities.
    pub entities: Vec<L3MemoryGraphEntity>,
    /// The graph's edges.
    pub edges: Vec<L3MemoryGraphEdge>,
}

impl L3MemoryGraph {
    /// Creates a graph from its parts.
    pub fn new(entities: Vec<L3MemoryGraphEntity>, edges: Vec<L3MemoryGraphEdge>) -> Self {
        Self { entities, edges }
    }
}

/// The L3 graph contract: entity and edge storage, replaceable by a real
/// graph database (Neo4j, ...).
///
/// Writes take the data (the record carries its scope); reads and removals
/// take the partition. Node identity is `(scope, canonical_name)`; the
/// synthetic `id` addresses a record for management. Edges have no id —
/// their identity is `(scope, from, relation_type, to)`.
pub trait L3MemoryGraphStore: Send + Sync + 'static {
    /// Creates or replaces one entity (same identity replaces; the record's
    /// id and creation time are preserved by the implementation).
    fn upsert_entity(&self, entity: L3MemoryGraphEntity) -> Result<(), MemoryStoreError>;

    /// Fetches one entity by its canonical name.
    fn get_entity(
        &self,
        scope: &MemoryScope,
        canonical_name: &str,
    ) -> Result<Option<L3MemoryGraphEntity>, MemoryStoreError>;

    /// Fetches one entity by its synthetic id.
    fn get_entity_by_id(&self, id: &str) -> Result<Option<L3MemoryGraphEntity>, MemoryStoreError>;

    /// The partition's entities, most recently updated first.
    fn entities(&self, scope: &MemoryScope) -> Result<Vec<L3MemoryGraphEntity>, MemoryStoreError>;

    /// Creates or replaces one edge (same identity replaces).
    fn upsert_edge(&self, edge: L3MemoryGraphEdge) -> Result<(), MemoryStoreError>;

    /// The edges touching one node (either end), most recently updated
    /// first.
    fn edges_of(
        &self,
        scope: &MemoryScope,
        name: &str,
    ) -> Result<Vec<L3MemoryGraphEdge>, MemoryStoreError>;

    /// Removes one entity and its touching edges (idempotent).
    fn remove_entity_by_id(&self, id: &str) -> Result<bool, MemoryStoreError>;

    /// Removes one edge by its identity (idempotent).
    fn remove_edge(
        &self,
        scope: &MemoryScope,
        from: &str,
        relation_type: &str,
        to: &str,
    ) -> Result<bool, MemoryStoreError>;

    /// How many entities the partition holds.
    fn count(&self, scope: &MemoryScope) -> Result<usize, MemoryStoreError>;
}

/// The L3 vector contract: the entity recall index, replaceable by a real
/// vector database (Milvus, ...). The key is the graph identity.
pub trait L3MemoryVectorStore: Send + Sync + 'static {
    /// Writes or replaces one entity's vector.
    fn upsert(
        &self,
        scope: &MemoryScope,
        canonical_name: &str,
        vector: Vec<f32>,
    ) -> Result<(), MemoryStoreError>;

    /// Removes one vector (idempotent).
    fn remove(&self, scope: &MemoryScope, canonical_name: &str) -> Result<bool, MemoryStoreError>;

    /// The nearest vectors of one partition, best first.
    fn search(
        &self,
        scope: &MemoryScope,
        vector: &[f32],
        budget: usize,
    ) -> Result<Vec<(String, f32)>, MemoryStoreError>;
}

/// The bundled in-process graph store: entity map + edge list.
#[derive(Default)]
pub struct InProcessL3MemoryGraphStore {
    entities: Mutex<HashMap<(String, String), L3MemoryGraphEntity>>,
    edges: Mutex<Vec<L3MemoryGraphEdge>>,
}

impl InProcessL3MemoryGraphStore {
    fn find_by_id(&self, id: &str) -> Option<L3MemoryGraphEntity> {
        let entities = self.entities.lock().unwrap_or_else(|p| p.into_inner());
        entities.values().find(|entity| entity.id == id).cloned()
    }
}

impl L3MemoryGraphStore for InProcessL3MemoryGraphStore {
    fn upsert_entity(&self, entity: L3MemoryGraphEntity) -> Result<(), MemoryStoreError> {
        let mut entities = self.entities.lock().unwrap_or_else(|p| p.into_inner());
        let key = (entity.scope.to_string(), entity.canonical_name.clone());
        let mut entity = entity;
        if let Some(previous) = entities.get(&key) {
            entity.id = previous.id.clone();
            entity.created_at = previous.created_at;
        }
        entities.insert(key, entity);
        Ok(())
    }

    fn get_entity(
        &self,
        scope: &MemoryScope,
        canonical_name: &str,
    ) -> Result<Option<L3MemoryGraphEntity>, MemoryStoreError> {
        let entities = self.entities.lock().unwrap_or_else(|p| p.into_inner());
        Ok(entities
            .get(&(scope.to_string(), canonical_name.to_string()))
            .cloned())
    }

    fn get_entity_by_id(&self, id: &str) -> Result<Option<L3MemoryGraphEntity>, MemoryStoreError> {
        Ok(self.find_by_id(id))
    }

    fn entities(&self, scope: &MemoryScope) -> Result<Vec<L3MemoryGraphEntity>, MemoryStoreError> {
        let entities = self.entities.lock().unwrap_or_else(|p| p.into_inner());
        let mut list: Vec<L3MemoryGraphEntity> = entities
            .values()
            .filter(|entity| entity.scope == *scope)
            .cloned()
            .collect();
        list.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.canonical_name.cmp(&b.canonical_name))
        });
        Ok(list)
    }

    fn upsert_edge(&self, edge: L3MemoryGraphEdge) -> Result<(), MemoryStoreError> {
        let mut edges = self.edges.lock().unwrap_or_else(|p| p.into_inner());
        match edges.iter_mut().find(|existing| {
            existing.scope == edge.scope
                && existing.from == edge.from
                && existing.relation_type == edge.relation_type
                && existing.to == edge.to
        }) {
            Some(existing) => *existing = edge,
            None => edges.push(edge),
        }
        Ok(())
    }

    fn edges_of(
        &self,
        scope: &MemoryScope,
        name: &str,
    ) -> Result<Vec<L3MemoryGraphEdge>, MemoryStoreError> {
        let edges = self.edges.lock().unwrap_or_else(|p| p.into_inner());
        let mut list: Vec<L3MemoryGraphEdge> = edges
            .iter()
            .filter(|edge| edge.scope == *scope && (edge.from == name || edge.to == name))
            .cloned()
            .collect();
        list.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.from.cmp(&b.from))
        });
        Ok(list)
    }

    fn remove_entity_by_id(&self, id: &str) -> Result<bool, MemoryStoreError> {
        let Some(entity) = self.find_by_id(id) else {
            return Ok(false);
        };
        let mut entities = self.entities.lock().unwrap_or_else(|p| p.into_inner());
        entities.remove(&(entity.scope.to_string(), entity.canonical_name.clone()));
        let mut edges = self.edges.lock().unwrap_or_else(|p| p.into_inner());
        edges.retain(|edge| {
            edge.scope != entity.scope
                || (edge.from != entity.canonical_name && edge.to != entity.canonical_name)
        });
        Ok(true)
    }

    fn remove_edge(
        &self,
        scope: &MemoryScope,
        from: &str,
        relation_type: &str,
        to: &str,
    ) -> Result<bool, MemoryStoreError> {
        let mut edges = self.edges.lock().unwrap_or_else(|p| p.into_inner());
        let before = edges.len();
        edges.retain(|edge| {
            !(edge.scope == *scope
                && edge.from == from
                && edge.relation_type == relation_type
                && edge.to == to)
        });
        Ok(edges.len() != before)
    }

    fn count(&self, scope: &MemoryScope) -> Result<usize, MemoryStoreError> {
        let entities = self.entities.lock().unwrap_or_else(|p| p.into_inner());
        Ok(entities
            .values()
            .filter(|entity| entity.scope == *scope)
            .count())
    }
}

/// The bundled in-process vector store: brute-force cosine search.
#[derive(Default)]
pub struct InProcessL3MemoryVectorStore {
    vectors: Mutex<HashMap<(String, String), Vec<f32>>>,
}

impl L3MemoryVectorStore for InProcessL3MemoryVectorStore {
    fn upsert(
        &self,
        scope: &MemoryScope,
        canonical_name: &str,
        vector: Vec<f32>,
    ) -> Result<(), MemoryStoreError> {
        let mut vectors = self.vectors.lock().unwrap_or_else(|p| p.into_inner());
        vectors.insert((scope.to_string(), canonical_name.to_string()), vector);
        Ok(())
    }

    fn remove(&self, scope: &MemoryScope, canonical_name: &str) -> Result<bool, MemoryStoreError> {
        let mut vectors = self.vectors.lock().unwrap_or_else(|p| p.into_inner());
        Ok(vectors
            .remove(&(scope.to_string(), canonical_name.to_string()))
            .is_some())
    }

    fn search(
        &self,
        scope: &MemoryScope,
        vector: &[f32],
        budget: usize,
    ) -> Result<Vec<(String, f32)>, MemoryStoreError> {
        let vectors = self.vectors.lock().unwrap_or_else(|p| p.into_inner());
        let mut scored: Vec<(String, f32)> = vectors
            .iter()
            .filter(|((entry_scope, _), _)| entry_scope == scope.as_str())
            .map(|((_, name), candidate)| (name.clone(), cosine(vector, candidate)))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(budget);
        Ok(scored)
    }
}

/// The extractor's material: the batch, the conversation's entries, the
/// related known entities, the schema, and the narrated model handle.
#[non_exhaustive]
pub struct L3MemoryExtractionInput<'a> {
    /// The batch's turns (the user side is the memory source; the assistant
    /// side is context only).
    pub batch: &'a [L1MemoryEntry],
    /// The conversation's entries (context).
    pub entries: &'a [L2MemoryEntry],
    /// The related known entities of the partition being distilled (a
    /// bounded retrieval result; reuse their canonical names when they
    /// match).
    pub known_entities: &'a [L3MemoryGraphEntity],
    /// The configured vocabulary.
    pub schema: &'a L3MemorySchema,
    /// The narrated model handle.
    pub model: Arc<dyn Model>,
}

impl<'a> L3MemoryExtractionInput<'a> {
    /// Assembles the payload.
    pub fn new(
        batch: &'a [L1MemoryEntry],
        entries: &'a [L2MemoryEntry],
        known_entities: &'a [L3MemoryGraphEntity],
        schema: &'a L3MemorySchema,
        model: Arc<dyn Model>,
    ) -> Self {
        Self {
            batch,
            entries,
            known_entities,
            schema,
            model,
        }
    }
}

/// The entity/relation extraction strategy: turns one batch (with its
/// context) into a pre-normalization graph. Types should come from the
/// schema; the component maps out-of-vocabulary values to the fallbacks.
pub trait L3MemoryEntityExtractor: Send + Sync + 'static {
    /// Extracts the graph of one batch.
    fn extract<'a>(
        &'a self,
        input: L3MemoryExtractionInput<'a>,
    ) -> BoxFuture<'a, Result<L3MemoryGraph, MemoryFailure>>;
}

/// The prompt-driven default extractor.
pub struct L3MemoryPromptEntityExtractor;

impl L3MemoryEntityExtractor for L3MemoryPromptEntityExtractor {
    fn extract<'a>(
        &'a self,
        input: L3MemoryExtractionInput<'a>,
    ) -> BoxFuture<'a, Result<L3MemoryGraph, MemoryFailure>> {
        Box::pin(async move {
            let mut batch = String::new();
            for turn in input.batch {
                batch.push_str(&format!(
                    "- topic: {}\n  user: {}\n  assistant: {}\n",
                    turn.topic, turn.input, turn.response
                ));
            }
            let mut entries = String::new();
            for entry in input.entries {
                entries.push_str(&format!("- {}\n", entry.content));
            }
            let mut known = String::new();
            for entity in input.known_entities {
                known.push_str(&format!(
                    "- {} ({}) — {}\n",
                    entity.canonical_name, entity.entity_type, entity.description
                ));
            }
            let entity_types = input.schema.entity_types.join(", ");
            let relation_types = input.schema.relation_types.join(", ");
            let prompt = format!(
                "Extract the long-term entities and their relations from the user's side of the \
                 batch below. The assistant's text is context only and must never become \
                 memory. Reuse the canonical names of known entities when they match.\n\n\
                 Allowed entity types: {entity_types}\n\
                 Allowed relation types: {relation_types}\n\n\
                 Known entities:\n{known}\nConversation entries:\n{entries}\nBatch:\n{batch}\n\
                 Reply with one line per record:\n\
                 E|<name>|<entity_type>|<description>\n\
                 R|<from>|<relation_type>|<to>\n\
                 Reply with NONE when there is nothing to extract."
            );
            let request = ModelRequest::new(vec![Message::user(prompt)], Vec::new());
            let (message, _usage) = complete(&*input.model, request)
                .await
                .map_err(|error| MemoryFailure::new("extract", error.to_string()))?;
            let text = message_text(&message);
            if text.trim().eq_ignore_ascii_case("NONE") {
                return Ok(L3MemoryGraph::default());
            }
            let mut entities = Vec::new();
            let mut edges = Vec::new();
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let mut parts = line.splitn(4, '|');
                match parts.next().unwrap_or_default().trim() {
                    "E" => {
                        let name = parts.next().unwrap_or_default().trim().to_string();
                        let entity_type = parts.next().unwrap_or_default().trim().to_string();
                        let description = parts.next().unwrap_or_default().trim().to_string();
                        if name.is_empty() {
                            return Err(MemoryFailure::new(
                                "extract",
                                format!("entity without a name: {line}"),
                            ));
                        }
                        entities.push(L3MemoryGraphEntity::extracted(
                            name,
                            entity_type,
                            description,
                        ));
                    }
                    "R" => {
                        let subject = parts.next().unwrap_or_default().trim().to_string();
                        let relation_type = parts.next().unwrap_or_default().trim().to_string();
                        let object = parts.next().unwrap_or_default().trim().to_string();
                        if subject.is_empty() || object.is_empty() {
                            return Err(MemoryFailure::new(
                                "extract",
                                format!("edge without both endpoints: {line}"),
                            ));
                        }
                        edges.push(L3MemoryGraphEdge::extracted(subject, relation_type, object));
                    }
                    other => {
                        return Err(MemoryFailure::new(
                            "extract",
                            format!("unknown record head '{other}': {line}"),
                        ));
                    }
                }
            }
            Ok(L3MemoryGraph::new(entities, edges))
        })
    }
}

/// The component's L3 domain object (internal): distillation (extraction →
/// normalization → graph and vector writes), reads, and relation removal.
pub(crate) struct L3Memory {
    graph: Arc<dyn L3MemoryGraphStore>,
    vectors: Arc<dyn L3MemoryVectorStore>,
    extractor: Arc<dyn L3MemoryEntityExtractor>,
    embedding: Arc<dyn Embedding>,
    config: Arc<LayeredMemoryConfig>,
    scope_resolver: Arc<dyn MemoryScopeResolver>,
    observer: Option<Arc<dyn LayeredMemoryObserver>>,
    touched: Mutex<HashMap<String, Vec<(MemoryScope, String)>>>,
}

impl L3Memory {
    /// Builds the layer over its collaborators.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        graph: Arc<dyn L3MemoryGraphStore>,
        vectors: Arc<dyn L3MemoryVectorStore>,
        extractor: Arc<dyn L3MemoryEntityExtractor>,
        embedding: Arc<dyn Embedding>,
        config: Arc<LayeredMemoryConfig>,
        scope_resolver: Arc<dyn MemoryScopeResolver>,
        observer: Option<Arc<dyn LayeredMemoryObserver>>,
    ) -> Self {
        Self {
            graph,
            vectors,
            extractor,
            embedding,
            config,
            scope_resolver,
            observer,
            touched: Mutex::new(HashMap::new()),
        }
    }

    /// The partition's entities, most recently updated first.
    pub(crate) fn entities(
        &self,
        scope: &MemoryScope,
    ) -> Result<Vec<L3MemoryGraphEntity>, MemoryStoreError> {
        self.graph.entities(scope)
    }

    /// The edges touching one node.
    pub(crate) fn relations_of(
        &self,
        scope: &MemoryScope,
        name: &str,
    ) -> Result<Vec<L3MemoryGraphEdge>, MemoryStoreError> {
        self.graph.edges_of(scope, name)
    }

    /// The vector-search anchors of one partition, best first.
    pub(crate) fn anchors(
        &self,
        scope: &MemoryScope,
        vector: &[f32],
        budget: usize,
    ) -> Result<Vec<(String, f32)>, MemoryStoreError> {
        self.vectors.search(scope, vector, budget)
    }

    /// One entity by its canonical name.
    pub(crate) fn entity(
        &self,
        scope: &MemoryScope,
        canonical_name: &str,
    ) -> Result<Option<L3MemoryGraphEntity>, MemoryStoreError> {
        self.graph.get_entity(scope, canonical_name)
    }

    /// One entity by its synthetic id.
    pub(crate) fn entity_by_id(
        &self,
        id: &str,
    ) -> Result<Option<L3MemoryGraphEntity>, MemoryStoreError> {
        self.graph.get_entity_by_id(id)
    }

    /// Corrects one entity's description in place: the embedding port's
    /// inline fast path refreshes the recall vector when available
    /// (otherwise the vector refreshes when the entity is next written
    /// back).
    pub(crate) fn edit_entity(
        &self,
        mut entity: L3MemoryGraphEntity,
        content: &str,
    ) -> Result<L3MemoryGraphEntity, MemoryStoreError> {
        entity.description = content.to_string();
        entity.updated_at = now_epoch();
        self.graph.upsert_entity(entity.clone())?;
        if let Some(Ok(vector)) = self.embedding.embed_inline(&entity_text(&entity)) {
            self.vectors
                .upsert(&entity.scope, &entity.canonical_name, vector)?;
        }
        Ok(entity)
    }

    /// Removes one entity and its recall vector (the graph store cascades
    /// the entity's edges).
    pub(crate) fn forget_entity(
        &self,
        entity: &L3MemoryGraphEntity,
    ) -> Result<bool, MemoryStoreError> {
        self.vectors.remove(&entity.scope, &entity.canonical_name)?;
        self.graph.remove_entity_by_id(&entity.id)
    }

    /// Removes one edge by its identity (the relation-management path).
    pub(crate) fn forget_relation(
        &self,
        scope: &MemoryScope,
        from: &str,
        relation_type: &str,
        to: &str,
    ) -> Result<bool, MemoryStoreError> {
        self.graph.remove_edge(scope, from, relation_type, to)
    }

    /// Distills one batch into every resolved long-term partition.
    pub(crate) async fn distill(
        &self,
        subject: &Subject,
        conversation_id: &str,
        batch: &[L1MemoryEntry],
        entries: &[L2MemoryEntry],
        model: Arc<dyn Model>,
    ) -> Result<(), MemoryFailure> {
        let scopes = self.scope_resolver.resolve(subject, conversation_id);
        if scopes.is_empty() {
            self.observe(LayeredMemoryEvent::Skipped {
                scope: conversation_scope(conversation_id),
                reason: "no long-term partitions resolved".to_string(),
            });
            return Ok(());
        }
        let query_text = batch
            .iter()
            .map(|turn| turn.input.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let query_vector = self.embedding.embed(&query_text).await?;
        for scope in scopes {
            let known = self.known_entities(&scope, &query_vector)?;
            let extracted = self
                .extractor
                .extract(L3MemoryExtractionInput::new(
                    batch,
                    entries,
                    &known,
                    &self.config.l3_schema,
                    Arc::clone(&model),
                ))
                .await?;
            self.write_graph(conversation_id, &scope, &extracted, &model)
                .await?;
        }
        Ok(())
    }

    /// Repairs the vectors of the entities this conversation touched
    /// (bounded; runs at conversation end).
    pub(crate) async fn repair_touched(&self, conversation_id: &str) -> Result<(), MemoryFailure> {
        let targets = {
            let mut touched = self.touched.lock().unwrap_or_else(|p| p.into_inner());
            touched.remove(conversation_id).unwrap_or_default()
        };
        for (scope, name) in targets {
            let Some(entity) = self
                .graph
                .get_entity(&scope, &name)
                .map_err(|error| MemoryFailure::new("repair", error.to_string()))?
            else {
                continue;
            };
            let vector = self.embedding.embed(&entity_text(&entity)).await?;
            self.vectors
                .upsert(&scope, &name, vector)
                .map_err(|error| MemoryFailure::new("repair", error.to_string()))?;
        }
        Ok(())
    }

    /// Clears the touched-entities bookkeeping of one conversation.
    pub(crate) fn forget_touched(&self, conversation_id: &str) {
        let mut touched = self.touched.lock().unwrap_or_else(|p| p.into_inner());
        touched.remove(conversation_id);
    }

    fn known_entities(
        &self,
        scope: &MemoryScope,
        query_vector: &[f32],
    ) -> Result<Vec<L3MemoryGraphEntity>, MemoryFailure> {
        let anchors = self
            .vectors
            .search(scope, query_vector, self.config.l3_extraction_entities)
            .map_err(|error| MemoryFailure::new("extract", error.to_string()))?;
        let mut known = Vec::new();
        for (name, similarity) in anchors {
            if similarity < self.config.l3_anchor_similarity {
                continue;
            }
            if let Some(entity) = self
                .graph
                .get_entity(scope, &name)
                .map_err(|error| MemoryFailure::new("extract", error.to_string()))?
            {
                known.push(entity);
            }
        }
        Ok(known)
    }

    async fn write_graph(
        &self,
        conversation_id: &str,
        scope: &MemoryScope,
        extracted: &L3MemoryGraph,
        model: &Arc<dyn Model>,
    ) -> Result<(), MemoryFailure> {
        let existing = self
            .graph
            .entities(scope)
            .map_err(|error| MemoryFailure::new("distill", error.to_string()))?;
        let now = now_epoch();
        let mut canonical_names: HashMap<String, String> = HashMap::new();
        let mut entities_written = 0usize;
        let mut edges_written = 0usize;
        for draft in &extracted.entities {
            let canonical = self
                .resolve_canonical(scope, &draft.canonical_name, &existing, model)
                .await?;
            canonical_names.insert(draft.canonical_name.clone(), canonical.clone());
            if canonical != draft.canonical_name {
                self.observe(LayeredMemoryEvent::Normalized {
                    scope: scope.clone(),
                    from: draft.canonical_name.clone(),
                    into: canonical.clone(),
                });
            }
            let previous = existing
                .iter()
                .find(|entity| entity.canonical_name == canonical);
            let mut aliases = previous
                .map(|entity| entity.aliases.clone())
                .unwrap_or_default();
            if canonical != draft.canonical_name && !aliases.contains(&draft.canonical_name) {
                aliases.push(draft.canonical_name.clone());
            }
            let entity = L3MemoryGraphEntity {
                id: previous
                    .map(|entity| entity.id.clone())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                canonical_name: canonical.clone(),
                aliases,
                entity_type: self
                    .config
                    .l3_schema
                    .normalize_entity_type(&draft.entity_type),
                description: draft.description.clone(),
                scope: scope.clone(),
                created_at: previous.map(|entity| entity.created_at).unwrap_or(now),
                updated_at: now,
            };
            // Vectors first: a partial failure leaves a harmless orphan
            // vector instead of an unrecallable entity.
            let vector = self.embedding.embed(&entity_text(&entity)).await?;
            self.vectors
                .upsert(scope, &canonical, vector)
                .map_err(|error| MemoryFailure::new("distill", error.to_string()))?;
            self.graph
                .upsert_entity(entity)
                .map_err(|error| MemoryFailure::new("distill", error.to_string()))?;
            self.touch(conversation_id, scope, &canonical);
            entities_written += 1;
        }
        for draft in &extracted.edges {
            let from = canonical_names
                .get(&draft.from)
                .cloned()
                .unwrap_or_else(|| draft.from.clone());
            let to = canonical_names
                .get(&draft.to)
                .cloned()
                .unwrap_or_else(|| draft.to.clone());
            self.graph
                .upsert_edge(L3MemoryGraphEdge {
                    from,
                    relation_type: self
                        .config
                        .l3_schema
                        .normalize_relation_type(&draft.relation_type),
                    to,
                    scope: scope.clone(),
                    updated_at: now,
                })
                .map_err(|error| MemoryFailure::new("distill", error.to_string()))?;
            edges_written += 1;
        }
        self.observe(LayeredMemoryEvent::Distilled {
            scope: scope.clone(),
            entities: entities_written,
            edges: edges_written,
        });
        Ok(())
    }

    async fn resolve_canonical(
        &self,
        scope: &MemoryScope,
        name: &str,
        existing: &[L3MemoryGraphEntity],
        model: &Arc<dyn Model>,
    ) -> Result<String, MemoryFailure> {
        let lowered = name.to_lowercase();
        for entity in existing {
            if entity.canonical_name.to_lowercase() == lowered
                || entity
                    .aliases
                    .iter()
                    .any(|alias| alias.to_lowercase() == lowered)
            {
                return Ok(entity.canonical_name.clone());
            }
        }
        let vector = self.embedding.embed(name).await?;
        let candidates = self
            .vectors
            .search(scope, &vector, 3)
            .map_err(|error| MemoryFailure::new("distill", error.to_string()))?;
        for (candidate, similarity) in candidates {
            if similarity < self.config.l3_alias_similarity {
                continue;
            }
            let Some(existing) = self
                .graph
                .get_entity(scope, &candidate)
                .map_err(|error| MemoryFailure::new("distill", error.to_string()))?
            else {
                continue;
            };
            if self.same_entity(name, &existing, model).await? {
                return Ok(existing.canonical_name);
            }
        }
        Ok(name.to_string())
    }

    async fn same_entity(
        &self,
        name: &str,
        existing: &L3MemoryGraphEntity,
        model: &Arc<dyn Model>,
    ) -> Result<bool, MemoryFailure> {
        let prompt = format!(
            "Are these two names the same real-world entity?\nA: {name}\nB: {} ({}) — {}\n\
             Reply with exactly YES or NO.",
            existing.canonical_name, existing.entity_type, existing.description
        );
        let request = ModelRequest::new(vec![Message::user(prompt)], Vec::new());
        let (message, _usage) = complete(&**model, request)
            .await
            .map_err(|error| MemoryFailure::new("distill", error.to_string()))?;
        let text = message_text(&message);
        let verdict = text.trim().to_uppercase();
        if verdict.starts_with("YES") {
            Ok(true)
        } else if verdict.starts_with("NO") {
            Ok(false)
        } else {
            Err(MemoryFailure::new(
                "distill",
                format!("unparsable alias verdict: {text}"),
            ))
        }
    }

    fn touch(&self, conversation_id: &str, scope: &MemoryScope, canonical_name: &str) {
        let mut touched = self.touched.lock().unwrap_or_else(|p| p.into_inner());
        touched
            .entry(conversation_id.to_string())
            .or_default()
            .push((scope.clone(), canonical_name.to_string()));
    }

    fn observe(&self, event: LayeredMemoryEvent) {
        if let Some(observer) = &self.observer {
            observer.on_progress(&event);
        }
    }
}

/// One entity's readable text (recall rendering and management content).
pub(crate) fn entity_text(entity: &L3MemoryGraphEntity) -> String {
    if entity.description.is_empty() {
        format!("{}（{}）", entity.canonical_name, entity.entity_type)
    } else {
        format!(
            "{}（{}）— {}",
            entity.canonical_name, entity.entity_type, entity.description
        )
    }
}
