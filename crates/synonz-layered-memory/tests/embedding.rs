//! The embedding port's retrieval roles.

#![allow(clippy::field_reassign_with_default)]

mod common;

use std::sync::Arc;

use futures::future::BoxFuture;
use synonz::{
    Agent, Conversation, MemoryFailure, MemoryScope, Subject, SubjectType, SynonzRuntime,
};
use synonz_layered_memory::{
    Embedding, HashEmbedding, L1MemoryEntry, L1MemoryStore, L2MemoryEntry, L2MemoryStore,
    L2MemorySummary, L2MemorySummaryOutput, L3MemoryGraph, L3MemoryGraphEntity, L3MemoryGraphStore,
    L3MemoryVectorStore, LayeredMemoryConfig, LayeredMemoryProvider,
};

/// A symmetric implementation: it overrides only the baseline methods, so
/// the role methods must delegate.
struct SymmetricEmbedding;

impl Embedding for SymmetricEmbedding {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, MemoryFailure>> {
        let vector = vec![text.chars().count() as f32, 1.0];
        Box::pin(async move { Ok(vector) })
    }

    fn embed_inline(&self, text: &str) -> Option<Result<Vec<f32>, MemoryFailure>> {
        Some(Ok(vec![text.chars().count() as f32, 1.0]))
    }
}

#[tokio::test]
async fn role_methods_delegate_to_the_symmetric_baseline() {
    let embedding = SymmetricEmbedding;
    let symmetric = embedding.embed("hello").await.unwrap();
    assert_eq!(embedding.embed_query("hello").await.unwrap(), symmetric);
    assert_eq!(embedding.embed_passage("hello").await.unwrap(), symmetric);
    let inline = embedding.embed_inline("hello").unwrap().unwrap();
    assert_eq!(
        embedding.embed_inline_query("hello").unwrap().unwrap(),
        inline
    );
    assert_eq!(
        embedding.embed_inline_passage("hello").unwrap().unwrap(),
        inline
    );
}

#[tokio::test]
async fn component_sites_use_the_role_methods() {
    let recording = Arc::new(common::RoleRecordingEmbedding::default());
    let mut parts = common::Parts::default();
    parts.embedding = Some(recording.clone());
    parts.summarizer = Some(Arc::new(common::StubSummarizer::new(vec![
        L2MemorySummaryOutput::new(vec![L2MemorySummary::new(
            None,
            "coffee",
            0.8,
            "the user likes coffee",
        )]),
    ])));
    parts.extractor = Some(Arc::new(common::StubExtractor::new(vec![
        L3MemoryGraph::new(
            vec![L3MemoryGraphEntity::extracted(
                "Alicia",
                "person",
                "likes coffee",
            )],
            vec![],
        ),
    ])));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.config = Some(LayeredMemoryConfig {
        l3_anchor_similarity: 0.15,
        ..Default::default()
    });
    let fixture = common::fixture(parts);
    let mut conversation = fixture.conversation();
    let partition = fixture.conversation_scope(&conversation);
    let long_term = MemoryScope::new(format!("user:{}", fixture.subject));

    // Working memory: one archived turn (L1 passages on the next recall).
    fixture
        .l1
        .append(L1MemoryEntry::new(
            partition.clone(),
            "coffee",
            "I like coffee",
            "noted",
            1_700_000_000,
        ))
        .unwrap();
    // Long-term memory: one entity (an anchor, and an alias candidate for
    // the distilled name).
    let entity = L3MemoryGraphEntity::new("Alice", "person", "likes coffee", long_term.clone(), 1);
    fixture.graph.upsert_entity(entity.clone()).unwrap();
    let embedding = HashEmbedding::default();
    fixture
        .vectors
        .upsert(
            &long_term,
            "Alice",
            embedding
                .embed("Alice（person）— likes coffee")
                .await
                .unwrap(),
        )
        .unwrap();

    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok"]))
        .build()
        .unwrap();
    agent
        .run(conversation.turn_input("do I still like coffee"))
        .await
        .unwrap();
    conversation.end(&fixture.runtime).await;

    // Retrieval queries: recall, the distillation batch, the alias lookup.
    assert!(recording.has(common::CallRole::Query, "do I still like coffee"));
    assert!(recording.has(common::CallRole::Query, "Alicia"));
    // Stored passages: L1 turn sides, the anchor re-embedding, the summary,
    // the distilled entity text.
    assert!(recording.has(common::CallRole::Passage, "I like coffee"));
    assert!(recording.has(common::CallRole::Passage, "noted"));
    assert!(recording.has(common::CallRole::Passage, "Alice（person）"));
    assert!(
        recording.has(common::CallRole::Passage, "the user likes coffee"),
        "the compacted summary is embedded as a passage"
    );
    assert!(recording.has(common::CallRole::Passage, "Alicia（person）"));

    // The management edit refreshes through the inline passage path.
    let alicia = fixture
        .actuator
        .l3_memory_entities(&fixture.subject, Some(&long_term))
        .unwrap()
        .into_iter()
        .find(|entity| entity.canonical_name == "Alicia")
        .expect("the distilled entity exists");
    fixture
        .runtime
        .memory()
        .edit(&fixture.subject, &alicia.id, "now likes tea")
        .unwrap();
    assert!(recording.has(common::CallRole::InlinePassage, "now likes tea"));

    let calls = recording.calls.lock().unwrap();
    assert!(
        !calls
            .iter()
            .any(|(role, _)| *role == common::CallRole::Symmetric),
        "the component must use the role methods only: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|(role, _)| *role == common::CallRole::InlineQuery),
        "no inline query call site exists: {calls:?}"
    );
}

#[tokio::test]
async fn topic_detection_embeds_both_sides_as_passages() {
    let recording = Arc::new(common::RoleRecordingEmbedding::default());
    let provider = LayeredMemoryProvider::builder()
        .embedding(common::SharedEmbedding(recording.clone()))
        .build();
    let detector = provider.topic_detector_provider();
    let runtime = SynonzRuntime::builder().memory_provider(provider).build();
    let subject = Subject::of(SubjectType::User, "u-detector");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(common::mock(&["ok"]))
        .topic_detector_provider(detector)
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject);
    conversation.set_topic("coffee");

    agent
        .run(conversation.turn_input("coffee beans are good"))
        .await
        .unwrap();

    assert!(recording.has(common::CallRole::Passage, "coffee"));
    assert!(recording.has(common::CallRole::Passage, "beans"));
    assert!(
        !recording
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(role, _)| *role == common::CallRole::Symmetric),
        "topic detection uses the role methods"
    );
}

/// An embedding whose synchronous refresh path always fails.
struct FailingInlineEmbedding;

impl Embedding for FailingInlineEmbedding {
    fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, MemoryFailure>> {
        Box::pin(async { Ok(vec![1.0, 0.0]) })
    }

    fn embed_inline_passage(&self, _text: &str) -> Option<Result<Vec<f32>, MemoryFailure>> {
        Some(Err(MemoryFailure::new("embed", "sync path down")))
    }
}

#[tokio::test]
async fn a_failed_inline_refresh_surfaces_as_a_fact() {
    let mut parts = common::Parts::default();
    parts.embedding = Some(Arc::new(FailingInlineEmbedding));
    let fixture = common::fixture(parts);
    let long_term = MemoryScope::new(format!("user:{}", fixture.subject));
    let entity = L3MemoryGraphEntity::new("Alice", "person", "likes coffee", long_term.clone(), 1);
    fixture.graph.upsert_entity(entity.clone()).unwrap();
    let partition = MemoryScope::new("conversation:c-managed");
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
    let entry_id = fixture
        .l2
        .list(&partition)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();

    let memory = fixture.runtime.memory();
    let edited = memory
        .edit(&fixture.subject, &entity.id, "likes tea")
        .unwrap();
    assert_eq!(edited.content, "Alice（person）— likes tea");
    let edited = memory
        .edit(&fixture.subject, &entry_id, "asked about refunds")
        .unwrap();
    assert_eq!(edited.content, "asked about refunds");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let facts = fixture.facts.facts.lock().unwrap();
    let failures = facts.iter().filter(|fact| *fact == "failed:embed").count();
    assert_eq!(failures, 2, "both failed refreshes are visible: {facts:?}");
    assert!(
        facts.iter().any(|fact| fact.starts_with("updated:")),
        "the edits still apply: {facts:?}"
    );
}
