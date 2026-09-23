//! The write phase: conversation-end compaction and distillation, and the
//! failure path.

#![allow(clippy::field_reassign_with_default)]

mod common;

use std::sync::Arc;

use futures::future::BoxFuture;
use synonz::Agent;
use synonz_layered_memory::{
    L2MemoryStore, L2MemorySummarizer, L2MemorySummaryInput, L2MemorySummaryOutput, L3MemoryGraph,
    L3MemoryGraphEdge, L3MemoryGraphEntity, L3MemoryGraphStore, LayeredMemoryConfig,
};

#[tokio::test]
async fn conversation_end_compacts_the_batch_and_distills_it() {
    let graph = L3MemoryGraph::new(
        vec![L3MemoryGraphEntity::extracted(
            "Alice",
            "person",
            "a colleague",
        )],
        vec![L3MemoryGraphEdge::extracted("Alice", "likes", "coffee")],
    );
    let mut parts = common::Parts::default();
    parts.extractor = Some(Arc::new(common::StubExtractor::new(vec![graph])));
    parts.summarizer = Some(Arc::new(common::StubSummarizer::new(vec![
        synonz_layered_memory::L2MemorySummaryOutput::new(vec![
            synonz_layered_memory::L2MemorySummary::new(
                None,
                "billing",
                0.8,
                "the user asked about invoices",
            ),
        ]),
    ])));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    let fixture = common::fixture(parts);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok"]))
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();

    agent.run(conversation.turn_input("one")).await.unwrap();
    agent.run(conversation.turn_input("two")).await.unwrap();
    conversation.end(&fixture.runtime).await;

    let partition = fixture.conversation_scope(&conversation);
    assert_eq!(
        fixture.l2.count(&partition).unwrap(),
        1,
        "compacted at the end"
    );
    let scope = synonz::MemoryScope::new(format!("user:{}", fixture.subject));
    assert_eq!(
        fixture.graph.entities(&scope).unwrap().len(),
        1,
        "distilled"
    );
}

/// A summarizer that always fails.
struct FailingSummarizer;

impl L2MemorySummarizer for FailingSummarizer {
    fn summarize<'a>(
        &'a self,
        _input: L2MemorySummaryInput<'a>,
    ) -> BoxFuture<'a, Result<L2MemorySummaryOutput, synonz::MemoryFailure>> {
        Box::pin(async { Err(synonz::MemoryFailure::new("summarize", "down")) })
    }
}

#[tokio::test]
async fn a_failing_compaction_surfaces_as_a_fact() {
    let mut parts = common::Parts::default();
    parts.summarizer = Some(Arc::new(FailingSummarizer));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.config = Some(LayeredMemoryConfig {
        l1_turns: 2,
        ..Default::default()
    });
    let fixture = common::fixture(parts);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok"]))
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();

    agent.run(conversation.turn_input("one")).await.unwrap();
    agent.run(conversation.turn_input("two")).await.unwrap();
    conversation.end(&fixture.runtime).await;

    let facts = fixture.facts.facts.lock().unwrap();
    assert!(
        facts.iter().any(|fact| fact == "failed:summarize"),
        "the background failure is visible: {facts:?}"
    );
}

#[tokio::test]
async fn conversation_end_without_a_model_fails_explicitly() {
    let mut parts = common::Parts::default();
    parts.without_model = true;
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    let fixture = common::fixture(parts);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok"]))
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();

    // The turn itself runs on the agent's model; the conversation-end
    // maintenance has no configured component model.
    agent.run(conversation.turn_input("one")).await.unwrap();
    conversation.end(&fixture.runtime).await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let facts = fixture.facts.facts.lock().unwrap();
    assert!(
        facts.iter().any(|fact| fact == "failed:finalize"),
        "the missing model surfaces as an explicit failure: {facts:?}"
    );
}
