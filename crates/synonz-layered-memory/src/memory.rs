//! The management face and the component's actuator.
//!
//! The management face implements the core item contract over the L2 and L3
//! records (in-place edits, removal cascades, content-free facts). The
//! actuator is the component's rich handle: typed reads plus relation-level
//! removal (relations are structure, not items).

use std::sync::Arc;

use synonz::{
    EventBus, Memory, MemoryForgetFailure, MemoryForgetResult, MemoryItem, MemoryListCursor,
    MemoryPage, MemoryQuery, MemoryScope, MemorySource, MemoryStoreError, Subject, SynonzEvent,
};

use crate::config::{LayeredMemoryConfig, MemoryScopeResolver};
use crate::embedding::Embedding;
use crate::l2_memory::{L2Memory, L2MemoryEntry, L2MemoryStore};
use crate::l3_memory::{
    L3Memory, L3MemoryGraphEdge, L3MemoryGraphEntity, L3MemoryGraphStore, L3MemoryVectorStore,
    entity_text,
};
use crate::utils::{conversation_id_of, conversation_scope, now_epoch};

/// One located management record.
enum MemoryRecord {
    /// An L2 entry (a conversation's memory).
    Summary(L2MemoryEntry),
    /// An L3 entity (long-term knowledge).
    Entity(L3MemoryGraphEntity),
}

impl MemoryRecord {
    fn id(&self) -> &str {
        match self {
            Self::Summary(entry) => &entry.id,
            Self::Entity(entity) => &entity.id,
        }
    }

    fn scope(&self) -> &MemoryScope {
        match self {
            Self::Summary(entry) => &entry.scope,
            Self::Entity(entity) => &entity.scope,
        }
    }

    /// Maps the record onto the core item view.
    fn item(&self) -> MemoryItem {
        match self {
            Self::Summary(entry) => MemoryItem::new(
                entry.id.clone(),
                entry.content.clone(),
                MemorySource::new(
                    conversation_id_of(&entry.scope).unwrap_or_default(),
                    entry.topic.clone(),
                ),
                entry.scope.clone(),
                entry.created_at,
                entry.updated_at,
            ),
            Self::Entity(entity) => MemoryItem::new(
                entity.id.clone(),
                entity_text(entity),
                MemorySource::new(String::new(), String::new()),
                entity.scope.clone(),
                entity.created_at,
                entity.updated_at,
            ),
        }
    }

    /// Whether the record matches one query's filters (the scope filter is
    /// applied by the caller when picking partitions).
    fn matches(&self, query: &MemoryQuery) -> bool {
        match self {
            Self::Summary(entry) => {
                if query
                    .conversation_id
                    .as_deref()
                    .is_some_and(|id| conversation_id_of(&entry.scope) != Some(id))
                {
                    return false;
                }
                if query
                    .topic
                    .as_deref()
                    .is_some_and(|topic| entry.topic != topic)
                {
                    return false;
                }
                let stamp = entry.updated_at.max(entry.created_at);
                if query.from.is_some_and(|from| stamp < from)
                    || query.to.is_some_and(|to| stamp >= to)
                {
                    return false;
                }
                if let Some(keyword) = &query.keyword
                    && !entry
                        .content
                        .to_lowercase()
                        .contains(&keyword.to_lowercase())
                {
                    return false;
                }
                true
            }
            Self::Entity(entity) => {
                // Long-term entries are cross-conversation and carry no topic: a
                // conversation or topic filter excludes them.
                if query.conversation_id.is_some() || query.topic.is_some() {
                    return false;
                }
                let stamp = entity.updated_at.max(entity.created_at);
                if query.from.is_some_and(|from| stamp < from)
                    || query.to.is_some_and(|to| stamp >= to)
                {
                    return false;
                }
                if let Some(keyword) = &query.keyword
                    && !entity_text(entity)
                        .to_lowercase()
                        .contains(&keyword.to_lowercase())
                {
                    return false;
                }
                true
            }
        }
    }
}

/// The component's management face (internal): the core item contract over
/// the L2 and L3 records.
pub(crate) struct LayeredMemory {
    l2: Arc<L2Memory>,
    l2_store: Arc<dyn L2MemoryStore>,
    l3_graph: Arc<dyn L3MemoryGraphStore>,
    l3_vectors: Arc<dyn L3MemoryVectorStore>,
    embedding: Arc<dyn Embedding>,
    scope_resolver: Arc<dyn MemoryScopeResolver>,
    config: Arc<LayeredMemoryConfig>,
    bus: EventBus,
}

impl LayeredMemory {
    /// Builds the face over the component's state and the runtime's bus.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        l2: Arc<L2Memory>,
        l2_store: Arc<dyn L2MemoryStore>,
        l3_graph: Arc<dyn L3MemoryGraphStore>,
        l3_vectors: Arc<dyn L3MemoryVectorStore>,
        embedding: Arc<dyn Embedding>,
        scope_resolver: Arc<dyn MemoryScopeResolver>,
        config: Arc<LayeredMemoryConfig>,
        bus: EventBus,
    ) -> Self {
        Self {
            l2,
            l2_store,
            l3_graph,
            l3_vectors,
            embedding,
            scope_resolver,
            config,
            bus,
        }
    }

    /// The partitions a query addresses: an explicit scope wins; otherwise
    /// the conversation's partition (when one is named) or every known
    /// partition of the subject. Explicit partitions are validated against
    /// the subject — a partition that is not the subject's yields nothing.
    fn target_scopes(&self, subject: &Subject, query: &MemoryQuery) -> Vec<MemoryScope> {
        if let Some(scope) = &query.scope {
            if self.owns_scope(subject, scope) {
                return vec![scope.clone()];
            }
            return Vec::new();
        }
        if let Some(conversation_id) = &query.conversation_id {
            let scope = conversation_scope(conversation_id);
            if self.owns_scope(subject, &scope) {
                return vec![scope];
            }
            return Vec::new();
        }
        let mut scopes = self.l2.conversation_scopes(subject);
        scopes.extend(self.scope_resolver.resolve(subject, ""));
        scopes
    }

    /// Whether one partition belongs to the subject: a conversation
    /// partition must be registered to it; a long-term partition must be one
    /// the resolver returns for it.
    fn owns_scope(&self, subject: &Subject, scope: &MemoryScope) -> bool {
        if conversation_id_of(scope).is_some() {
            return owns_conversation(&self.l2, subject, scope);
        }
        owns_partition(&self.scope_resolver, subject, scope)
    }

    /// Collects the matching records, ranked by freshness descending, id
    /// ascending.
    fn collect(
        &self,
        subject: &Subject,
        query: &MemoryQuery,
    ) -> Result<Vec<MemoryRecord>, MemoryStoreError> {
        let mut records = Vec::new();
        for scope in self.target_scopes(subject, query) {
            if conversation_id_of(&scope).is_some() {
                for entry in self.l2_store.list(&scope)? {
                    let record = MemoryRecord::Summary(entry);
                    if record.matches(query) {
                        records.push(record);
                    }
                }
            } else {
                for entity in self.l3_graph.entities(&scope)? {
                    let record = MemoryRecord::Entity(entity);
                    if record.matches(query) {
                        records.push(record);
                    }
                }
            }
        }
        records.sort_by(|a, b| {
            let left = a.item();
            let right = b.item();
            freshness(&right)
                .cmp(&freshness(&left))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(records)
    }

    /// Locates one record by id within the subject's partitions (its
    /// conversation entries first, then its long-term entities).
    fn find(&self, subject: &Subject, id: &str) -> Result<Option<MemoryRecord>, MemoryStoreError> {
        for scope in self.l2.conversation_scopes(subject) {
            if let Some(entry) = self.l2_store.get(&scope, id)? {
                return Ok(Some(MemoryRecord::Summary(entry)));
            }
        }
        if let Some(entity) = self.l3_graph.get_entity_by_id(id)?
            && self.owns_scope(subject, &entity.scope)
        {
            return Ok(Some(MemoryRecord::Entity(entity)));
        }
        Ok(None)
    }

    fn emit_updated(&self, subject: &Subject, scope: &MemoryScope, id: &str) {
        self.bus
            .emit(SynonzEvent::Memory(synonz::MemoryEvent::Updated {
                subject_id: subject.to_string(),
                scope: scope.clone(),
                id: id.to_string(),
            }));
    }

    fn emit_removed(&self, subject: &Subject, scope: &MemoryScope, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        self.bus
            .emit(SynonzEvent::Memory(synonz::MemoryEvent::Removed {
                subject_id: subject.to_string(),
                scope: scope.clone(),
                ids,
            }));
    }

    /// Removes one located record (cascading vectors and edges).
    fn remove_record(&self, record: &MemoryRecord) -> Result<bool, MemoryStoreError> {
        match record {
            MemoryRecord::Summary(entry) => {
                self.l3_vectors.remove(&entry.scope, &entry.id)?;
                self.l2_store.remove(&entry.scope, &entry.id)
            }
            MemoryRecord::Entity(entity) => {
                self.l3_vectors
                    .remove(&entity.scope, &entity.canonical_name)?;
                self.l3_graph.remove_entity_by_id(&entity.id)
            }
        }
    }
}

impl Memory for LayeredMemory {
    fn list(
        &self,
        subject: &Subject,
        query: MemoryQuery,
    ) -> Result<MemoryPage<MemoryItem, MemoryListCursor>, MemoryStoreError> {
        let limit = query.limit;
        if limit == 0 {
            return Ok(MemoryPage::new(Vec::new(), None));
        }
        let records = self.collect(subject, &query)?;
        let items: Vec<MemoryItem> = records.iter().map(MemoryRecord::item).collect();
        let start = match &query.after {
            Some(cursor) => match &cursor.position {
                Some(position) => items
                    .iter()
                    .position(|item| {
                        freshness(item) < position.updated_at
                            || (freshness(item) == position.updated_at && item.id > position.id)
                    })
                    .unwrap_or(items.len()),
                None => 0,
            },
            None => 0,
        };
        let remaining = &items[start..];
        let take = remaining.len().min(limit);
        let page_items = remaining[..take].to_vec();
        let next = if remaining.len() > take {
            page_items.last().map(|item| {
                MemoryListCursor::new(synonz::MemoryCursor::new(freshness(item), item.id.clone()))
            })
        } else {
            None
        };
        Ok(MemoryPage::new(page_items, next))
    }

    fn get(&self, subject: &Subject, id: &str) -> Result<Option<MemoryItem>, MemoryStoreError> {
        Ok(self.find(subject, id)?.map(|record| record.item()))
    }

    fn edit(
        &self,
        subject: &Subject,
        id: &str,
        content: &str,
    ) -> Result<MemoryItem, MemoryStoreError> {
        let Some(record) = self.find(subject, id)? else {
            return Err(MemoryStoreError::EntryNotFound(id.to_string()));
        };
        match record {
            MemoryRecord::Summary(mut entry) => {
                entry.versions.insert(0, entry.content.clone());
                entry.versions.truncate(self.config.l2_versions.max(1));
                entry.content = content.to_string();
                entry.updated_at = now_epoch();
                // The embedding port's inline fast path refreshes the recall
                // vector synchronously; without it the vector refreshes when
                // the entry is next written back.
                if let Some(Ok(vector)) = self.embedding.embed_inline(&entry.content) {
                    entry.embedding = vector;
                }
                self.l2_store.upsert(entry.clone())?;
                self.emit_updated(subject, &entry.scope, &entry.id);
                Ok(MemoryRecord::Summary(entry).item())
            }
            MemoryRecord::Entity(mut entity) => {
                entity.description = content.to_string();
                entity.updated_at = now_epoch();
                self.l3_graph.upsert_entity(entity.clone())?;
                if let Some(Ok(vector)) = self.embedding.embed_inline(&entity_text(&entity)) {
                    self.l3_vectors
                        .upsert(&entity.scope, &entity.canonical_name, vector)?;
                }
                self.emit_updated(subject, &entity.scope, &entity.id);
                Ok(MemoryRecord::Entity(entity).item())
            }
        }
    }

    fn forget(&self, subject: &Subject, id: &str) -> Result<MemoryForgetResult, MemoryStoreError> {
        let Some(record) = self.find(subject, id)? else {
            return Ok(MemoryForgetResult::new(0, Vec::new()));
        };
        let removed = self.remove_record(&record)?;
        if removed {
            self.emit_removed(subject, record.scope(), vec![record.id().to_string()]);
        }
        Ok(MemoryForgetResult::new(usize::from(removed), Vec::new()))
    }

    fn forget_matching(
        &self,
        subject: &Subject,
        query: MemoryQuery,
    ) -> Result<MemoryForgetResult, MemoryStoreError> {
        let budget = query.limit;
        let mut result = MemoryForgetResult::new(0, Vec::new());
        if budget == 0 {
            return Ok(result);
        }
        let records = self.collect(subject, &query)?;
        let mut removed_by_scope: Vec<(MemoryScope, Vec<String>)> = Vec::new();
        for record in records.into_iter().take(budget) {
            match self.remove_record(&record) {
                Ok(true) => {
                    result.removed += 1;
                    match removed_by_scope
                        .iter_mut()
                        .find(|(scope, _)| scope == record.scope())
                    {
                        Some((_, ids)) => ids.push(record.id().to_string()),
                        None => removed_by_scope
                            .push((record.scope().clone(), vec![record.id().to_string()])),
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    result
                        .failures
                        .push(MemoryForgetFailure::new(record.id(), error.to_string()));
                }
            }
        }
        for (scope, ids) in removed_by_scope {
            self.emit_removed(subject, &scope, ids);
        }
        Ok(result)
    }
}

/// The component's actuator (public): typed reads plus relation-level
/// removal. Every read is scoped to the subject: an explicit partition must
/// belong to it (a conversation registered to the subject, or a long-term
/// partition its resolver returns), otherwise the read yields nothing.
pub struct LayeredMemoryActuator {
    l2: Arc<L2Memory>,
    l3: Arc<L3Memory>,
    scope_resolver: Arc<dyn MemoryScopeResolver>,
}

impl LayeredMemoryActuator {
    /// Builds the actuator over the component's state.
    pub(crate) fn new(
        l2: Arc<L2Memory>,
        l3: Arc<L3Memory>,
        scope_resolver: Arc<dyn MemoryScopeResolver>,
    ) -> Self {
        Self {
            l2,
            l3,
            scope_resolver,
        }
    }

    /// The subject's L2 entries (`scope: None` = every conversation of the
    /// subject).
    pub fn l2_memory_entries(
        &self,
        subject: &Subject,
        scope: Option<&MemoryScope>,
    ) -> Result<Vec<L2MemoryEntry>, MemoryStoreError> {
        match scope {
            Some(scope) => {
                if !owns_conversation(&self.l2, subject, scope) {
                    return Ok(Vec::new());
                }
                self.l2.entries(scope)
            }
            None => {
                let mut entries = Vec::new();
                for scope in self.l2.conversation_scopes(subject) {
                    entries.extend(self.l2.entries(&scope)?);
                }
                Ok(entries)
            }
        }
    }

    /// The subject's L3 entities (`scope: None` = every resolved partition).
    pub fn l3_memory_entities(
        &self,
        subject: &Subject,
        scope: Option<&MemoryScope>,
    ) -> Result<Vec<L3MemoryGraphEntity>, MemoryStoreError> {
        match scope {
            Some(scope) => {
                if !owns_partition(&self.scope_resolver, subject, scope) {
                    return Ok(Vec::new());
                }
                self.l3.entities(scope)
            }
            None => {
                let mut entities = Vec::new();
                for scope in self.scope_resolver.resolve(subject, "") {
                    entities.extend(self.l3.entities(&scope)?);
                }
                Ok(entities)
            }
        }
    }

    /// The subject's L3 relations touching one entity.
    pub fn l3_memory_relations(
        &self,
        subject: &Subject,
        scope: &MemoryScope,
        entity: &str,
    ) -> Result<Vec<L3MemoryGraphEdge>, MemoryStoreError> {
        if !owns_partition(&self.scope_resolver, subject, scope) {
            return Ok(Vec::new());
        }
        self.l3.relations_of(scope, entity)
    }

    /// Removes one L3 relation by its identity (the relation-management
    /// path).
    pub fn forget_l3_memory_relation(
        &self,
        subject: &Subject,
        scope: &MemoryScope,
        from: &str,
        relation_type: &str,
        to: &str,
    ) -> Result<bool, MemoryStoreError> {
        if !owns_partition(&self.scope_resolver, subject, scope) {
            return Ok(false);
        }
        self.l3.forget_relation(scope, from, relation_type, to)
    }
}

/// Whether one conversation partition is registered to the subject.
fn owns_conversation(l2: &L2Memory, subject: &Subject, scope: &MemoryScope) -> bool {
    l2.conversation_scopes(subject)
        .iter()
        .any(|owned| owned == scope)
}

/// Whether one long-term partition is resolved for the subject.
fn owns_partition(
    resolver: &Arc<dyn MemoryScopeResolver>,
    subject: &Subject,
    scope: &MemoryScope,
) -> bool {
    resolver
        .resolve(subject, "")
        .iter()
        .any(|owned| owned == scope)
}

/// The freshness stamp used for item ordering and time filters.
fn freshness(item: &MemoryItem) -> u64 {
    item.updated_at.max(item.created_at)
}
