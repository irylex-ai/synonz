//! `layered_memory`: the official layered memory component end to end —
//! turn-level L1 working memory, L2 batch compaction, and the L3 entity
//! graph and vectors, with deterministic strategies so the example runs
//! fully offline.
//!
//! Run: `cargo run -p synonz-examples --bin layered_memory`

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::future::BoxFuture;
use synonz::{
    Agent, MockModel, ModelStreamItem, Subject, SubjectType, SynonzRuntime, TokenUsage,
    TopicDetectInput, TopicDetector, TopicDetectorProvider,
};
use synonz_layered_memory::{
    L2MemorySummarizer, L2MemorySummary, L2MemorySummaryInput, L2MemorySummaryOutput,
    L3MemoryEntityExtractor, L3MemoryExtractionInput, L3MemoryGraph, L3MemoryGraphEdge,
    L3MemoryGraphEntity, LayeredMemoryContextRewriteInput, LayeredMemoryContextRewriter,
    LayeredMemoryProvider,
};

/// A deterministic batch summarizer: one entry per batch (a real strategy
/// would call the model).
struct OneEntry;

impl L2MemorySummarizer for OneEntry {
    fn summarize<'a>(
        &'a self,
        input: L2MemorySummaryInput<'a>,
    ) -> BoxFuture<'a, Result<L2MemorySummaryOutput, synonz::MemoryFailure>> {
        let turn = input.batch.last();
        let topic = turn.map(|turn| turn.topic.clone()).unwrap_or_default();
        let content = turn
            .map(|turn| turn.input.chars().take(input.max_chars).collect::<String>())
            .unwrap_or_default();
        Box::pin(async move {
            Ok(L2MemorySummaryOutput::new(vec![L2MemorySummary::new(
                None, topic, 0.8, content,
            )]))
        })
    }
}

/// A deterministic entity extractor: one person with one preference.
struct OneEntity;

impl L3MemoryEntityExtractor for OneEntity {
    fn extract<'a>(
        &'a self,
        _input: L3MemoryExtractionInput<'a>,
    ) -> BoxFuture<'a, Result<L3MemoryGraph, synonz::MemoryFailure>> {
        Box::pin(async move {
            Ok(L3MemoryGraph::new(
                vec![L3MemoryGraphEntity::extracted(
                    "Alice",
                    "person",
                    "the user's colleague",
                )],
                vec![L3MemoryGraphEdge::extracted("Alice", "likes", "coffee")],
            ))
        })
    }
}

/// A deterministic context rewriter: folds the recalled items into the
/// user message (a real strategy would call the model).
struct FoldContext;

impl LayeredMemoryContextRewriter for FoldContext {
    fn rewrite<'a>(
        &'a self,
        input: LayeredMemoryContextRewriteInput<'a>,
    ) -> BoxFuture<'a, Result<String, synonz::MemoryFailure>> {
        let recalled = input.recalled.join("; ");
        let enhanced = if recalled.is_empty() {
            input.input.to_string()
        } else {
            format!("{} (context: {recalled})", input.input)
        };
        Box::pin(async move { Ok(enhanced) })
    }
}

/// A deterministic detector: establishes a topic, then shifts once.
struct ShiftOnce {
    calls: AtomicUsize,
}

impl TopicDetector for ShiftOnce {
    fn detect<'a>(
        &'a self,
        _input: TopicDetectInput<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, synonz::MemoryFailure>> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let verdict = if call == 0 {
            Some("billing".to_string())
        } else {
            Some("shipping".to_string())
        };
        Box::pin(async move { Ok(verdict) })
    }
}

/// The detector's factory (Agent-level registration).
struct ShiftOnceProvider {
    detector: Arc<ShiftOnce>,
}

impl TopicDetectorProvider for ShiftOnceProvider {
    fn topic_detector(&self) -> Arc<dyn TopicDetector> {
        Arc::clone(&self.detector) as Arc<dyn TopicDetector>
    }
}

#[tokio::main]
async fn main() {
    let model = MockModel::new(vec![
        vec![ModelStreamItem::Finish {
            message: synonz::Message::assistant_text("noted"),
            usage: TokenUsage::new(1, 1),
        }],
        vec![ModelStreamItem::Finish {
            message: synonz::Message::assistant_text("noted"),
            usage: TokenUsage::new(1, 1),
        }],
    ]);

    // The component: the bundled in-process stores and deterministic
    // strategies; the long-term partition defaults to `user:<subject>`.
    // The component's model feeds the framework's conversation-end
    // maintenance (these deterministic strategies make no model calls).
    let provider = LayeredMemoryProvider::builder()
        .summarizer(OneEntry)
        .entity_extractor(OneEntity)
        .context_rewriter(FoldContext)
        .model(model.clone())
        .build();
    let actuator = provider.actuator();

    let runtime = SynonzRuntime::builder().memory_provider(provider).build();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .topic_detector_provider(ShiftOnceProvider {
            detector: Arc::new(ShiftOnce {
                calls: AtomicUsize::new(0),
            }),
        })
        .build()
        .expect("model and runtime are set");

    let subject = Subject::of(SubjectType::User, "demo");
    let mut conversation = synonz::Conversation::new(&runtime, &subject);

    let first = agent
        .run(conversation.turn_input("please send me the invoice"))
        .await
        .expect("run completes");
    println!("turn 1: {:?}", first.text());

    let second = agent
        .run(conversation.turn_input("where is my parcel?"))
        .await
        .expect("run completes");
    println!("turn 2: {:?}", second.text());

    // Ending the conversation drains the background maintenance and runs
    // the conversation-end compaction and distillation.
    conversation.end(&runtime).await;

    // The core management face: item-level listing and editing.
    let items = runtime
        .memory()
        .list(&subject, synonz::MemoryQuery::new(10))
        .expect("the management face lists");
    println!("core items: {}", items.items.len());

    // The component's actuator: typed reads and relation removal.
    let entries = actuator
        .l2_memory_entries(&subject, None)
        .expect("entries list");
    println!(
        "L2 entries: {:?}",
        entries
            .iter()
            .map(|entry| &entry.content)
            .collect::<Vec<_>>()
    );
    let scope = synonz::MemoryScope::new(format!("user:{subject}"));
    let entities = actuator
        .l3_memory_entities(&subject, Some(&scope))
        .expect("entities list");
    println!(
        "entities: {:?}",
        entities
            .iter()
            .map(|entity| (entity.canonical_name.as_str(), entity.entity_type.as_str()))
            .collect::<Vec<_>>()
    );
    let relations = actuator
        .l3_memory_relations(&subject, &scope, "Alice")
        .expect("relations");
    println!(
        "relations: {:?}",
        relations
            .iter()
            .map(|edge| format!("{} {} {}", edge.from, edge.relation_type, edge.to))
            .collect::<Vec<_>>()
    );
}
