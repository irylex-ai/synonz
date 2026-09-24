//! The management face and the actuator.

mod common;

use synonz::{MemoryQuery, MemoryScope};
use synonz_layered_memory::{
    L2MemoryEntry, L2MemoryStore, L3MemoryGraphEdge, L3MemoryGraphEntity, L3MemoryGraphStore,
    L3MemoryVectorStore,
};

/// The subject's default long-term partition (the bundled resolver).
fn subject_scope(fixture: &common::Fixture) -> MemoryScope {
    MemoryScope::new(format!("user:{}", fixture.subject))
}

/// Seeds the records the management tests work with: an L2 entry and an L3
/// entity of the fixture's subject. The entry carries its subject, which is
/// what the management face enumerates and validates through.
fn seed(fixture: &common::Fixture) -> (MemoryScope, MemoryScope) {
    let partition = MemoryScope::new("conversation:c-managed");
    let scope = subject_scope(fixture);
    fixture
        .l2
        .upsert(L2MemoryEntry::new(
            fixture.subject.clone(),
            partition.clone(),
            "billing",
            "asked about invoices",
            Vec::new(),
            0.5,
            1,
        ))
        .unwrap();
    let mut entity = L3MemoryGraphEntity::new("Alice", "person", "a colleague", scope.clone(), 1);
    entity.updated_at = 1;
    fixture.graph.upsert_entity(entity).unwrap();
    fixture
        .graph
        .upsert_edge(L3MemoryGraphEdge::new(
            "Alice",
            "likes",
            "coffee",
            scope.clone(),
            1,
        ))
        .unwrap();
    (partition, scope)
}

#[tokio::test]
async fn the_management_face_lists_edits_and_forgets() {
    let fixture = common::fixture(common::Parts::default());
    // The entry's subject (not an archived turn) is what the face
    // enumerates and validates through.
    let (partition, scope) = seed(&fixture);
    let memory = fixture.runtime.memory();

    let entries = memory
        .list(
            &fixture.subject,
            MemoryQuery::new(10).with_conversation("c-managed"),
        )
        .unwrap();
    assert_eq!(entries.items.len(), 1);
    assert!(entries.items[0].content.contains("invoices"));
    let entities = memory
        .list(
            &fixture.subject,
            MemoryQuery::new(10).with_scope(scope.clone()),
        )
        .unwrap();
    assert_eq!(entities.items.len(), 1);
    assert!(entities.items[0].content.contains("Alice"));

    // Edit the entry: in place, the old content moves into the history.
    let entry_id = entries.items[0].id.clone();
    let edited = memory
        .edit(&fixture.subject, &entry_id, "asked about invoices twice")
        .unwrap();
    assert!(edited.content.contains("twice"));
    assert_eq!(
        fixture.l2.list(&partition).unwrap()[0].versions,
        vec!["asked about invoices".to_string()]
    );

    // Forget the entity: cascades its edge.
    let entity_id = entities.items[0].id.clone();
    assert_eq!(
        memory.forget(&fixture.subject, &entity_id).unwrap().removed,
        1
    );
    assert!(fixture.graph.entities(&scope).unwrap().is_empty());
    assert!(fixture.graph.edges_of(&scope, "Alice").unwrap().is_empty());

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let facts = fixture.facts.facts.lock().unwrap();
    assert!(
        facts.iter().any(|fact| fact.contains("updated:")),
        "{facts:?}"
    );
    assert!(
        facts.iter().any(|fact| fact.contains("removed:")),
        "{facts:?}"
    );
}

#[tokio::test]
async fn forget_matching_batches_by_scope() {
    let fixture = common::fixture(common::Parts::default());
    let (partition, _) = seed(&fixture);
    fixture
        .l2
        .upsert(L2MemoryEntry::new(
            fixture.subject.clone(),
            partition.clone(),
            "shipping",
            "asked about parcels",
            Vec::new(),
            0.5,
            2,
        ))
        .unwrap();

    let result = fixture
        .runtime
        .memory()
        .forget_matching(
            &fixture.subject,
            MemoryQuery::new(10).with_conversation("c-managed"),
        )
        .unwrap();
    assert_eq!(result.removed, 2);
    assert_eq!(fixture.l2.count(&partition).unwrap(), 0);

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let facts = fixture.facts.facts.lock().unwrap();
    assert!(
        facts
            .iter()
            .any(|fact| fact.starts_with("removed:conversation:c-managed:")),
        "{facts:?}"
    );
}

#[tokio::test]
async fn the_actuator_exposes_documents_and_forgets_relations() {
    let fixture = common::fixture(common::Parts::default());
    let (partition, scope) = seed(&fixture);
    let actuator = fixture.actuator;

    assert_eq!(
        actuator
            .l2_memory_entries(&fixture.subject, Some(&partition))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        actuator
            .l3_memory_entities(&fixture.subject, Some(&scope))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        actuator
            .l3_memory_relations(&fixture.subject, &scope, "Alice")
            .unwrap()
            .len(),
        1
    );
    assert!(
        actuator
            .forget_l3_memory_relation(&fixture.subject, &scope, "Alice", "likes", "coffee")
            .unwrap()
    );
    assert!(
        actuator
            .l3_memory_relations(&fixture.subject, &scope, "Alice")
            .unwrap()
            .is_empty()
    );
    assert!(
        !actuator
            .forget_l3_memory_relation(&fixture.subject, &scope, "Alice", "likes", "coffee")
            .unwrap()
    );
}

#[tokio::test]
async fn the_management_face_is_isolated_by_subject() {
    let fixture = common::fixture(common::Parts::default());
    seed(&fixture);
    let alice = fixture.subject.clone();
    let bob = synonz::Subject::of(synonz::SubjectType::User, "u-bob");

    // Bob's entry (carrying his subject) and one entity of his own.
    let bob_partition = MemoryScope::new("conversation:c-bob");
    fixture
        .l2
        .upsert(L2MemoryEntry::new(
            bob.clone(),
            bob_partition.clone(),
            "billing",
            "bob's invoice note",
            Vec::new(),
            0.5,
            2,
        ))
        .unwrap();
    let bob_scope = MemoryScope::new(format!("user:{bob}"));
    let mut entity =
        L3MemoryGraphEntity::new("Bob", "person", "another colleague", bob_scope.clone(), 2);
    entity.updated_at = 2;
    fixture.graph.upsert_entity(entity.clone()).unwrap();

    let memory = fixture.runtime.memory();
    // Listing alice's memory never includes bob's entries.
    let all = memory.list(&alice, MemoryQuery::new(50)).unwrap();
    assert!(
        all.items.iter().all(|item| !item.content.contains("bob's")),
        "{:?}",
        all.items
    );
    // Explicit filters naming bob's partitions yield nothing for alice.
    assert!(
        memory
            .list(&alice, MemoryQuery::new(10).with_conversation("c-bob"))
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        memory
            .list(
                &alice,
                MemoryQuery::new(10).with_scope(bob_partition.clone())
            )
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        memory
            .list(&alice, MemoryQuery::new(10).with_scope(bob_scope.clone()))
            .unwrap()
            .items
            .is_empty()
    );
    // Id-addressed verbs cannot reach bob's records.
    let bob_entry_id = memory
        .list(&bob, MemoryQuery::new(10).with_conversation("c-bob"))
        .unwrap()
        .items[0]
        .id
        .clone();
    assert!(memory.get(&alice, &bob_entry_id).unwrap().is_none());
    assert!(memory.edit(&alice, &bob_entry_id, "rewritten").is_err());
    assert_eq!(memory.forget(&alice, &bob_entry_id).unwrap().removed, 0);
    assert!(memory.get(&alice, &entity.id).unwrap().is_none());
    assert_eq!(
        memory
            .forget_matching(&alice, MemoryQuery::new(10).with_conversation("c-bob"))
            .unwrap()
            .removed,
        0
    );
    // Bob's records are untouched.
    let stored = fixture
        .l2
        .get(&bob_partition, &bob_entry_id)
        .unwrap()
        .unwrap();
    assert!(stored.content.contains("bob's"));
    assert!(memory.get(&bob, &bob_entry_id).unwrap().is_some());
    // The actuator is subject-scoped too.
    assert!(
        fixture
            .actuator
            .l2_memory_entries(&alice, Some(&bob_partition))
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .actuator
            .l3_memory_entities(&alice, Some(&bob_scope))
            .unwrap()
            .is_empty()
    );
    assert!(
        !fixture
            .actuator
            .forget_l3_memory_relation(&alice, &bob_scope, "Bob", "likes", "coffee")
            .unwrap()
    );
    assert_eq!(
        fixture
            .actuator
            .l2_memory_entries(&bob, None)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn direct_store_writes_carry_their_owner_into_the_face() {
    let fixture = common::fixture(common::Parts::default());
    // A framework-external import: an entry written straight into the store
    // with its subject (no turn archived through the component).
    let partition = MemoryScope::new("conversation:c-imported");
    fixture
        .l2
        .upsert(L2MemoryEntry::new(
            fixture.subject.clone(),
            partition.clone(),
            "billing",
            "imported invoice note",
            Vec::new(),
            0.5,
            1,
        ))
        .unwrap();

    let memory = fixture.runtime.memory();
    let entries = memory
        .list(
            &fixture.subject,
            MemoryQuery::new(10).with_conversation("c-imported"),
        )
        .unwrap();
    assert_eq!(entries.items.len(), 1, "the import is visible to the face");
    assert!(entries.items[0].content.contains("imported"));
    assert_eq!(
        memory
            .get(&fixture.subject, &entries.items[0].id)
            .unwrap()
            .map(|item| item.content),
        Some("imported invoice note".to_string())
    );

    // Ownership is part of the data: another subject sees nothing.
    let other = synonz::Subject::of(synonz::SubjectType::User, "u-other");
    assert!(
        memory
            .list(&other, MemoryQuery::new(10).with_conversation("c-imported"))
            .unwrap()
            .items
            .is_empty()
    );
    assert!(memory.get(&other, &entries.items[0].id).unwrap().is_none());
}

/// The entity's vector as the recall path sees it: the similarity between
/// the stored vector and the embedding of `text`.
async fn vector_similarity(fixture: &common::Fixture, scope: &MemoryScope, text: &str) -> f32 {
    use synonz_layered_memory::Embedding;
    let embedding = synonz_layered_memory::HashEmbedding::default();
    let vector = embedding.embed(text).await.unwrap();
    fixture
        .vectors
        .search(scope, &vector, 1)
        .unwrap()
        .first()
        .map(|(_, similarity)| *similarity)
        .unwrap_or(0.0)
}

#[tokio::test]
async fn editing_an_entity_refreshes_its_vector_immediately() {
    use synonz_layered_memory::{
        Embedding, HashEmbedding, L3MemoryGraphStore, L3MemoryVectorStore,
    };
    let fixture = common::fixture(common::Parts::default());
    let scope = subject_scope(&fixture);
    let entity = L3MemoryGraphEntity::new("Alice", "person", "likes coffee", scope.clone(), 1);
    fixture.graph.upsert_entity(entity.clone()).unwrap();
    let embedding = HashEmbedding::default();
    let old_text = "Alice（person）— likes coffee";
    fixture
        .vectors
        .upsert(&scope, "Alice", embedding.embed(old_text).await.unwrap())
        .unwrap();

    fixture
        .runtime
        .memory()
        .edit(&fixture.subject, &entity.id, "dislikes rain")
        .unwrap();

    let new_text = "Alice（person）— dislikes rain";
    assert!(
        vector_similarity(&fixture, &scope, new_text).await > 0.999,
        "the inline path refreshed the vector with the new description"
    );
}
