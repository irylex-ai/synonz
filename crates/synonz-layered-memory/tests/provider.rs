//! The provider: bundled defaults, the prompt-driven strategies, and the
//! scope resolver.

#![allow(clippy::field_reassign_with_default)]

mod common;

use std::sync::Arc;

use synonz::{Agent, MemoryScope, Subject};
use synonz_layered_memory::{
    L1MemoryEntry, L2MemoryPromptSummarizer, L2MemorySummarizer, L2MemorySummaryInput,
    L3MemoryEntityExtractor, L3MemoryExtractionInput, L3MemoryGraphStore,
    L3MemoryPromptEntityExtractor, L3MemorySchema, LayeredMemoryConfig,
    LayeredMemoryContextRewriteInput, LayeredMemoryContextRewriter, LayeredMemoryEvent,
    LayeredMemoryPromptContextRewriter, MemoryScopeResolver,
};

#[tokio::test]
async fn the_bundled_defaults_run_multi_turn() {
    // The component's configured model is the same scripted model the agent
    // uses: turn 2's assembly recalls turn 1, so the default prompt rewriter
    // consumes one call before the reasoning call.
    let model = common::mock(&["first answer", "enhanced input", "second answer"]);
    let mut parts = common::Parts::default();
    parts.model = Some(Arc::new(model.clone()));
    let fixture = common::fixture(parts);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(model)
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();

    let first = agent
        .run(conversation.turn_input("first question"))
        .await
        .unwrap();
    assert_eq!(first.text(), Some("first answer"));
    let second = agent
        .run(conversation.turn_input("second question"))
        .await
        .unwrap();
    assert_eq!(second.text(), Some("second answer"));
    assert_eq!(conversation.turns().len(), 2);
}

fn model() -> Arc<dyn synonz::Model> {
    Arc::new(common::RecordingModel::new("rewritten"))
}

#[tokio::test]
async fn the_prompt_rewriter_folds_the_recalled_items() {
    let rewriter = LayeredMemoryPromptContextRewriter;
    let model = model();
    let recalled = vec!["the user likes coffee".to_string()];
    let output = rewriter
        .rewrite(LayeredMemoryContextRewriteInput::new(
            "what do I like?",
            &recalled,
            Arc::clone(&model),
        ))
        .await
        .unwrap();
    assert_eq!(output, "rewritten");
}

#[tokio::test]
async fn the_prompt_summarizer_parses_the_protocol() {
    let model: Arc<dyn synonz::Model> = Arc::new(common::mock(&[
        "CREATE|billing|0.9|the user asked about invoices",
    ]));
    let batch = vec![L1MemoryEntry::new(
        MemoryScope::new("conversation:c1"),
        "billing",
        "invoice?",
        "ok",
        1,
    )];
    let output = L2MemoryPromptSummarizer
        .summarize(L2MemorySummaryInput::new(&batch, &[], 30, model))
        .await
        .unwrap();
    assert_eq!(output.summaries.len(), 1);
    assert_eq!(output.summaries[0].topic, "billing");
    assert_eq!(output.summaries[0].importance, 0.9);

    // An over-long content violates the contract instead of truncating.
    let model: Arc<dyn synonz::Model> = Arc::new(common::mock(&[
        "CREATE|billing|0.5|this content is far too long for the configured limit",
    ]));
    let failure = L2MemoryPromptSummarizer
        .summarize(L2MemorySummaryInput::new(&batch, &[], 10, model))
        .await
        .expect_err("the contract violation fails");
    assert_eq!(failure.stage, "summarize");

    // NONE yields an empty output.
    let model: Arc<dyn synonz::Model> = Arc::new(common::mock(&["NONE"]));
    let output = L2MemoryPromptSummarizer
        .summarize(L2MemorySummaryInput::new(&batch, &[], 30, model))
        .await
        .unwrap();
    assert!(output.summaries.is_empty());
}

#[tokio::test]
async fn the_prompt_extractor_parses_records() {
    let model: Arc<dyn synonz::Model> = Arc::new(common::mock(&[
        "E|Alice|person|a colleague\nR|Alice|likes|coffee",
    ]));
    let batch = vec![L1MemoryEntry::new(
        MemoryScope::new("conversation:c1"),
        "billing",
        "I like coffee",
        "ok",
        1,
    )];
    let graph = L3MemoryPromptEntityExtractor
        .extract(L3MemoryExtractionInput::new(
            &batch,
            &[],
            &[],
            &L3MemorySchema::default(),
            model,
        ))
        .await
        .unwrap();
    assert_eq!(graph.entities.len(), 1);
    assert_eq!(graph.entities[0].canonical_name, "Alice");
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.edges[0].relation_type, "likes");

    let model: Arc<dyn synonz::Model> = Arc::new(common::mock(&["NONE"]));
    let graph = L3MemoryPromptEntityExtractor
        .extract(L3MemoryExtractionInput::new(
            &batch,
            &[],
            &[],
            &L3MemorySchema::default(),
            model,
        ))
        .await
        .unwrap();
    assert!(graph.entities.is_empty());
}

/// A resolver that declares no long-term partitions.
struct NoScopes;

impl MemoryScopeResolver for NoScopes {
    fn resolve(&self, _subject: &Subject, _conversation_id: &str) -> Vec<MemoryScope> {
        Vec::new()
    }
}

#[tokio::test]
async fn an_empty_scope_resolution_skips_long_term_memory() {
    let mut parts = common::Parts::default();
    parts.scope_resolver = Some(Arc::new(NoScopes));
    parts.extractor = Some(Arc::new(common::StubExtractor::new(vec![])));
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
        .topic_detector_provider(common::StubDetectorProvider { detector })
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();
    agent.run(conversation.turn_input("invoice")).await.unwrap();
    agent.run(conversation.turn_input("parcel")).await.unwrap();
    conversation.end(&fixture.runtime).await;

    assert!(
        fixture
            .progress
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(
                event,
                LayeredMemoryEvent::Skipped { reason, .. }
                    if reason.contains("no long-term partitions")
            )),
        "the skip is observable"
    );
    assert_eq!(
        fixture
            .graph
            .entities(&MemoryScope::new(format!("user:{}", fixture.subject)))
            .unwrap()
            .len(),
        0
    );
}

/// A resolver serving a project partition.
struct ProjectScope;

impl MemoryScopeResolver for ProjectScope {
    fn resolve(&self, _subject: &Subject, _conversation_id: &str) -> Vec<MemoryScope> {
        vec![MemoryScope::new("project:demo")]
    }
}

#[tokio::test]
async fn the_scope_resolver_selects_the_long_term_partition() {
    use synonz_layered_memory::{L3MemoryGraph, L3MemoryGraphEdge, L3MemoryGraphEntity};
    let mut parts = common::Parts::default();
    parts.scope_resolver = Some(Arc::new(ProjectScope));
    parts.summarizer = Some(Arc::new(common::StubSummarizer::new(vec![])));
    parts.extractor = Some(Arc::new(common::StubExtractor::new(vec![
        L3MemoryGraph::new(
            vec![L3MemoryGraphEntity::extracted(
                "Alice",
                "person",
                "a colleague",
            )],
            vec![L3MemoryGraphEdge::extracted("Alice", "likes", "coffee")],
        ),
    ])));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    let fixture = common::fixture(parts);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok"]))
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();
    agent.run(conversation.turn_input("hello")).await.unwrap();
    conversation.end(&fixture.runtime).await;

    let scope = MemoryScope::new("project:demo");
    assert_eq!(fixture.graph.entities(&scope).unwrap().len(), 1);
    assert!(
        fixture
            .graph
            .entities(&MemoryScope::new(format!("user:{}", fixture.subject)))
            .unwrap()
            .is_empty()
    );
}
