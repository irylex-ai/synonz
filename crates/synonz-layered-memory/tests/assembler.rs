//! The read phase: recall, the cold-start gate, and the frame.

#![allow(clippy::field_reassign_with_default)]

mod common;

use std::sync::Arc;

use synonz::{Agent, MemoryScope};
use synonz_layered_memory::{
    Embedding, HashEmbedding, L1MemoryEntry, L1MemoryStore, L2MemoryEntry, L2MemoryStore,
    L3MemoryGraphEntity, L3MemoryGraphStore, L3MemoryVectorStore, LayeredMemoryConfig,
    LayeredMemoryContextRewriter,
};

#[tokio::test]
async fn recalled_items_are_folded_into_the_frame() {
    let echo = Arc::new(common::EchoRewriter::default());
    let mut parts = common::Parts::default();
    parts.context_rewriter = Some(echo.clone() as Arc<dyn LayeredMemoryContextRewriter>);
    let fixture = common::fixture(parts);
    let mut conversation = fixture.conversation();
    let scope = fixture.conversation_scope(&conversation);
    fixture
        .l1
        .append(L1MemoryEntry::new(
            scope,
            "coffee",
            "I like coffee",
            "noted",
            1_700_000_000,
        ))
        .unwrap();

    let model = common::RecordingModel::new("the answer");
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(model.clone())
        .build()
        .unwrap();
    agent
        .run(conversation.turn_input("what do I like?"))
        .await
        .unwrap();

    let seen = model.last_user_text();
    assert!(seen.contains("[recalled:"), "{seen}");
    assert!(seen.contains("I like coffee"), "{seen}");
}

#[tokio::test]
async fn nothing_recalled_means_no_rewrite_call() {
    let echo = Arc::new(common::EchoRewriter::default());
    let mut parts = common::Parts::default();
    parts.context_rewriter = Some(echo.clone() as Arc<dyn LayeredMemoryContextRewriter>);
    let fixture = common::fixture(parts);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::RecordingModel::new("the answer"))
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();

    agent.run(conversation.turn_input("hello")).await.unwrap();

    assert!(
        echo.seen.lock().unwrap().is_empty(),
        "an empty recall skips the rewrite"
    );
}

#[tokio::test]
async fn the_cold_start_gate_keeps_long_term_memory_out() {
    let echo = Arc::new(common::EchoRewriter::default());
    let mut parts = common::Parts::default();
    parts.context_rewriter = Some(echo.clone() as Arc<dyn LayeredMemoryContextRewriter>);
    parts.config = Some(LayeredMemoryConfig {
        l3_anchor_similarity: 0.2,
        ..Default::default()
    });
    let fixture = common::fixture(parts);
    let scope = MemoryScope::new(format!("user:{}", fixture.subject));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut entity =
        L3MemoryGraphEntity::extracted("cappuccino", "preference", "the user likes cappuccino");
    entity.scope = scope.clone();
    entity.created_at = now;
    entity.updated_at = now;
    fixture.graph.upsert_entity(entity).unwrap();
    let embedding = HashEmbedding::default();
    let vector = Embedding::embed(
        &embedding,
        "cappuccino（preference）— the user likes cappuccino",
    )
    .await
    .unwrap();
    fixture
        .vectors
        .upsert(&scope, "cappuccino", vector)
        .unwrap();

    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok"]))
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();

    // The first turn: working memory is empty — the gate keeps L3 out.
    agent
        .run(conversation.turn_input("do I still like cappuccino"))
        .await
        .unwrap();
    assert!(
        !echo
            .seen
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .any(|text| text.contains("（preference）")),
        "the gate blocks long-term memory while working memory is empty"
    );

    // The second turn: working memory exists — the entity may be recalled.
    agent
        .run(conversation.turn_input("do I still like cappuccino?"))
        .await
        .unwrap();
    assert!(
        echo.seen
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .any(|text| text.contains("（preference）")),
        "the gate opens once working memory exists"
    );
}

/// An embedding keying billing texts onto one axis and everything else onto
/// the other (near-duplicates are controlled).
struct BillingEmbedding;

impl Embedding for BillingEmbedding {
    fn embed<'a>(
        &'a self,
        text: &'a str,
    ) -> futures::future::BoxFuture<'a, Result<Vec<f32>, synonz::MemoryFailure>> {
        let lowered = text.to_lowercase();
        let vector = if lowered.contains("invoice") || lowered.contains("billing") {
            vec![1.0, 0.0]
        } else {
            vec![0.0, 1.0]
        };
        Box::pin(async move { Ok(vector) })
    }
}

#[tokio::test]
async fn near_duplicate_l2_and_l3_candidates_dedupe_by_vector() {
    let echo = Arc::new(common::EchoRewriter::default());
    let mut parts = common::Parts::default();
    parts.context_rewriter = Some(echo.clone() as Arc<dyn LayeredMemoryContextRewriter>);
    parts.embedding = Some(Arc::new(BillingEmbedding));
    let fixture = common::fixture(parts);
    let mut conversation = fixture.conversation();
    let partition = fixture.conversation_scope(&conversation);

    // An L2 entry and an L3 entity stating the same fact in different words;
    // the embedding makes them near-identical.
    fixture
        .l2
        .upsert(L2MemoryEntry::new(
            partition,
            "billing",
            "asked about invoices",
            vec![1.0, 0.0],
            0.5,
            1,
        ))
        .unwrap();
    let scope = MemoryScope::new(format!("user:{}", fixture.subject));
    let mut entity = L3MemoryGraphEntity::extracted("Alice", "person", "handles billing");
    entity.scope = scope.clone();
    entity.created_at = 1;
    entity.updated_at = 1;
    fixture.graph.upsert_entity(entity).unwrap();
    fixture
        .vectors
        .upsert(&scope, "Alice", vec![1.0, 0.0])
        .unwrap();

    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok"]))
        .build()
        .unwrap();
    agent
        .run(conversation.turn_input("billing question"))
        .await
        .unwrap();

    let seen = echo.seen.lock().unwrap();
    let recalled = seen.last().expect("the recall happened");
    assert_eq!(
        recalled.len(),
        1,
        "the near-duplicates deduped by vector: {recalled:?}"
    );
}
