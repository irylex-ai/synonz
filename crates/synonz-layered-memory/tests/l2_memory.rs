//! L2 event summaries: the store's capacities and batch compaction.

#![allow(clippy::field_reassign_with_default)]

mod common;

use std::sync::Arc;

use futures::future::BoxFuture;
use synonz::{Agent, MemoryScope};
use synonz_layered_memory::{
    L2MemoryEntry, L2MemoryStore, L2MemorySummarizer, L2MemorySummary, L2MemorySummaryInput,
    L2MemorySummaryOutput, LayeredMemoryConfig, LayeredMemoryEvent,
};

#[test]
fn the_store_enforces_its_capacities() {
    let store = synonz_layered_memory::InProcessL2MemoryStore::new(3, 2);
    let scope = MemoryScope::new("conversation:c1");
    for index in 0..5 {
        store
            .upsert(L2MemoryEntry::new(
                scope.clone(),
                "topic",
                format!("content {index}"),
                Vec::new(),
                0.5,
                index as u64,
            ))
            .unwrap();
    }
    let entries = store.list(&scope).unwrap();
    assert_eq!(entries.len(), 3, "the entry capacity holds");
    assert_eq!(entries[0].updated_at, 4, "the newest update is first");

    let mut entry = store.list(&scope).unwrap().remove(0);
    entry.versions = vec!["v1".into(), "v2".into(), "v3".into()];
    store.upsert(entry).unwrap();
    assert_eq!(
        store.list(&scope).unwrap()[0].versions.len(),
        2,
        "versions truncate"
    );

    assert!(
        store
            .remove(&scope, &store.list(&scope).unwrap()[0].id)
            .unwrap()
    );
    assert_eq!(store.count(&scope).unwrap(), 2);
}

/// A summarizer that creates on the first call and updates afterwards
/// (using the existing entry the input carries).
struct UpdateSummarizer;

impl L2MemorySummarizer for UpdateSummarizer {
    fn summarize<'a>(
        &'a self,
        input: L2MemorySummaryInput<'a>,
    ) -> BoxFuture<'a, Result<L2MemorySummaryOutput, synonz::MemoryFailure>> {
        let output = match input.existing.first() {
            None => L2MemorySummaryOutput::new(vec![L2MemorySummary::new(
                None,
                "billing",
                0.9,
                "the user asked about invoices",
            )]),
            Some(entry) => L2MemorySummaryOutput::new(vec![L2MemorySummary::new(
                Some(entry.id.clone()),
                "billing",
                0.9,
                "the user asked about invoices again",
            )]),
        };
        Box::pin(async move { Ok(output) })
    }
}

#[tokio::test]
async fn a_full_window_compacts_the_batch_into_an_entry() {
    let mut parts = common::Parts::default();
    parts.summarizer = Some(Arc::new(common::StubSummarizer::new(vec![
        L2MemorySummaryOutput::new(vec![L2MemorySummary::new(
            None,
            "billing",
            0.9,
            "the user asked about invoices",
        )]),
    ])));
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

    for index in 0..2 {
        agent
            .run(conversation.turn_input(format!("question {index}")))
            .await
            .unwrap();
    }
    conversation.end(&fixture.runtime).await;

    let scope = fixture.conversation_scope(&conversation);
    let entries = fixture.l2.list(&scope).unwrap();
    assert_eq!(entries.len(), 1, "the batch compacted into one entry");
    assert_eq!(entries[0].content, "the user asked about invoices");
    assert_eq!(entries[0].importance, 0.9);
    assert_eq!(entries[0].topic, "billing");
    assert!(
        fixture
            .progress
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(
                event,
                LayeredMemoryEvent::Compacted {
                    created: 1,
                    updated: 0,
                    ..
                }
            )),
        "the compaction is observable"
    );
}

#[tokio::test]
async fn updating_an_entry_pushes_the_old_content_into_versions() {
    let mut parts = common::Parts::default();
    parts.summarizer = Some(Arc::new(UpdateSummarizer));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.config = Some(LayeredMemoryConfig {
        l1_turns: 2,
        ..Default::default()
    });
    let fixture = common::fixture(parts);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok", "ok", "ok"]))
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();

    for index in 0..4 {
        agent
            .run(conversation.turn_input(format!("question {index}")))
            .await
            .unwrap();
    }
    conversation.end(&fixture.runtime).await;

    let scope = fixture.conversation_scope(&conversation);
    let entries = fixture.l2.list(&scope).unwrap();
    assert_eq!(entries.len(), 1, "the update reused the entry");
    assert_eq!(entries[0].content, "the user asked about invoices again");
    assert_eq!(
        entries[0].versions,
        vec!["the user asked about invoices".to_string()],
        "the previous content moved into the history"
    );
    assert!(
        fixture
            .progress
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(
                event,
                LayeredMemoryEvent::Compacted {
                    created: 0,
                    updated: 1,
                    ..
                }
            )),
    );
}
