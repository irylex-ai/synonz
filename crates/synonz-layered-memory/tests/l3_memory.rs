//! L3 long-term memory: schema normalization, the graph store's identity
//! guarantees, distillation, and alias merging.

#![allow(clippy::field_reassign_with_default)]

mod common;

use std::sync::Arc;

use futures::future::BoxFuture;
use synonz::{Agent, MemoryFailure, MemoryScope};
use synonz_layered_memory::{
    Embedding, InProcessL3MemoryGraphStore, L3MemoryGraph, L3MemoryGraphEdge, L3MemoryGraphEntity,
    L3MemoryGraphStore, L3MemorySchema, L3MemoryVectorStore, LayeredMemoryConfig,
    LayeredMemoryEvent,
};

#[test]
fn the_schema_normalizes_case_and_falls_back() {
    let schema = L3MemorySchema::default();
    assert_eq!(schema.normalize_entity_type("PERSON"), "person");
    assert_eq!(schema.normalize_entity_type(" Alien "), "other");
    assert_eq!(schema.normalize_relation_type("Likes"), "likes");
    assert_eq!(schema.normalize_relation_type("hates"), "related_to");

    let unconstrained = L3MemorySchema {
        entity_types: Vec::new(),
        relation_types: Vec::new(),
        ..Default::default()
    };
    assert_eq!(unconstrained.normalize_entity_type("Anything"), "Anything");
}

#[test]
fn the_graph_store_preserves_identity_and_cascades_removal() {
    let store = InProcessL3MemoryGraphStore::default();
    let scope = MemoryScope::new("user:u1");
    let mut alice = L3MemoryGraphEntity::extracted("Alice", "person", "a colleague");
    alice.scope = scope.clone();
    alice.created_at = 10;
    alice.updated_at = 10;
    store.upsert_entity(alice).unwrap();
    let original_id = store.get_entity(&scope, "Alice").unwrap().unwrap().id;

    let mut replacement = L3MemoryGraphEntity::extracted("Alice", "person", "updated");
    replacement.scope = scope.clone();
    replacement.created_at = 99;
    replacement.updated_at = 99;
    store.upsert_entity(replacement).unwrap();
    let stored = store.get_entity(&scope, "Alice").unwrap().unwrap();
    assert_eq!(stored.id, original_id, "the store keeps the record id");
    assert_eq!(stored.created_at, 10, "the store keeps the creation time");
    assert_eq!(stored.description, "updated");

    store
        .upsert_edge(L3MemoryGraphEdge::new(
            "Alice",
            "likes",
            "coffee",
            scope.clone(),
            1,
        ))
        .unwrap();
    assert_eq!(store.edges_of(&scope, "Alice").unwrap().len(), 1);

    assert!(store.get_entity_by_id(&original_id).unwrap().is_some());
    assert!(store.remove_entity_by_id(&original_id).unwrap());
    assert!(store.edges_of(&scope, "Alice").unwrap().is_empty());
    assert!(!store.remove_entity_by_id(&original_id).unwrap());

    store
        .upsert_edge(L3MemoryGraphEdge::new(
            "Alice",
            "likes",
            "coffee",
            scope.clone(),
            1,
        ))
        .unwrap();
    assert!(
        store
            .remove_edge(&scope, "Alice", "likes", "coffee")
            .unwrap()
    );
    assert!(store.edges_of(&scope, "Alice").unwrap().is_empty());
}

fn alice_graph(name: &str) -> L3MemoryGraph {
    L3MemoryGraph::new(
        vec![L3MemoryGraphEntity::extracted(
            name,
            "person",
            "a colleague",
        )],
        vec![L3MemoryGraphEdge::extracted(name, "likes", "coffee")],
    )
}

#[tokio::test]
async fn a_topic_shift_distills_the_batch_into_the_graph() {
    let mut parts = common::Parts::default();
    parts.extractor = Some(Arc::new(common::StubExtractor::new(vec![alice_graph(
        "Alice",
    )])));
    parts.summarizer = Some(Arc::new(common::StubSummarizer::new(vec![])));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.config = Some(LayeredMemoryConfig {
        l1_turns: 2,
        ..Default::default()
    });
    let fixture = common::fixture(parts);
    let detector = Arc::new(common::StubDetector::new(vec![
        Some("billing".into()),
        Some("shipping".into()),
    ]));
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok"]))
        .topic_detector_provider(common::StubDetectorProvider {
            detector: detector.clone(),
        })
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();

    agent.run(conversation.turn_input("invoice")).await.unwrap();
    agent.run(conversation.turn_input("parcel")).await.unwrap();
    conversation.end(&fixture.runtime).await;

    let scope = MemoryScope::new(format!("user:{}", fixture.subject));
    let entities = fixture.graph.entities(&scope).unwrap();
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].canonical_name, "Alice");
    assert_eq!(entities[0].entity_type, "person");
    assert_eq!(fixture.graph.edges_of(&scope, "Alice").unwrap().len(), 1);
    assert!(!fixture.vectors.search(&scope, &[], 10).unwrap().is_empty());
    assert!(
        fixture
            .progress
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(
                event,
                LayeredMemoryEvent::Distilled {
                    entities: 1,
                    edges: 1,
                    ..
                }
            )),
    );
}

#[tokio::test]
async fn alias_merges_reuse_the_existing_canonical_name() {
    let mut parts = common::Parts::default();
    parts.extractor = Some(Arc::new(common::StubExtractor::new(vec![
        alice_graph("Alice"),
        alice_graph("alice"),
    ])));
    parts.summarizer = Some(Arc::new(common::StubSummarizer::new(vec![])));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.config = Some(LayeredMemoryConfig {
        l1_turns: 2,
        ..Default::default()
    });
    let fixture = common::fixture(parts);
    let detector = Arc::new(common::StubDetector::new(vec![
        Some("billing".into()),
        Some("shipping".into()),
        Some("billing".into()),
    ]));
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok", "ok"]))
        .topic_detector_provider(common::StubDetectorProvider { detector })
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();
    for index in 0..3 {
        agent
            .run(conversation.turn_input(format!("turn {index}")))
            .await
            .unwrap();
    }
    conversation.end(&fixture.runtime).await;

    let scope = MemoryScope::new(format!("user:{}", fixture.subject));
    let entities = fixture.graph.entities(&scope).unwrap();
    assert_eq!(entities.len(), 1, "the alias merged into one entity");
    assert_eq!(entities[0].canonical_name, "Alice");
    assert!(entities[0].aliases.iter().any(|alias| alias == "alice"));
}

/// An embedding that makes every pair maximally similar (to exercise the
/// LLM alias judgment).
struct ConstantEmbedding;

impl Embedding for ConstantEmbedding {
    fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, MemoryFailure>> {
        Box::pin(async { Ok(vec![1.0]) })
    }
}

#[tokio::test]
async fn the_llm_judges_near_duplicate_names() {
    let mut parts = common::Parts::default();
    parts.extractor = Some(Arc::new(common::StubExtractor::new(vec![
        alice_graph("Alice"),
        alice_graph("Alicia"),
    ])));
    parts.summarizer = Some(Arc::new(common::StubSummarizer::new(vec![])));
    parts.embedding = Some(Arc::new(ConstantEmbedding));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.config = Some(LayeredMemoryConfig {
        l1_turns: 2,
        ..Default::default()
    });
    // The provider model answers the alias judgment.
    parts.model =
        Some(Arc::new(common::mock(&["YES", "YES", "YES", "YES"])) as Arc<dyn synonz::Model>);
    let fixture = common::fixture(parts);
    let detector = Arc::new(common::StubDetector::new(vec![
        Some("billing".into()),
        Some("shipping".into()),
        Some("billing".into()),
    ]));
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok", "ok"]))
        .topic_detector_provider(common::StubDetectorProvider { detector })
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();
    for index in 0..3 {
        agent
            .run(conversation.turn_input(format!("turn {index}")))
            .await
            .unwrap();
    }
    conversation.end(&fixture.runtime).await;

    let scope = MemoryScope::new(format!("user:{}", fixture.subject));
    let entities = fixture.graph.entities(&scope).unwrap();
    assert_eq!(entities.len(), 1, "the LLM-merged alias left one entity");
    assert!(entities[0].aliases.iter().any(|alias| alias == "Alicia"));
}
