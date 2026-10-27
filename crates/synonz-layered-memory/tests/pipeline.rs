//! The write phase: conversation-end compaction and distillation, and the
//! failure path.

#![allow(clippy::field_reassign_with_default)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use synonz::{
    Agent, Conversation, MemoryScope, MemoryStoreError, Subject, SubjectType, SynonzRuntime,
};
use synonz_layered_memory::{
    InProcessL1MemoryStore, InProcessL2MemoryStore, InProcessL3MemoryGraphStore,
    InProcessL3MemoryVectorStore, L1MemoryEntry, L1MemoryStore, L2MemoryEntry, L2MemoryStore,
    L2MemorySummarizer, L2MemorySummary, L2MemorySummaryInput, L2MemorySummaryOutput,
    L3MemoryGraph, L3MemoryGraphEdge, L3MemoryGraphEntity, L3MemoryGraphStore, LayeredMemoryConfig,
    LayeredMemoryEvent, LayeredMemoryProvider,
};
use tokio::sync::Notify;

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

/// The standard stub summary (one entry, "billing").
fn summary() -> L2MemorySummaryOutput {
    L2MemorySummaryOutput::new(vec![L2MemorySummary::new(
        None,
        "billing",
        0.8,
        "the user asked about invoices",
    )])
}

/// The standard stub graph (one entity, "Alice").
fn graph() -> L3MemoryGraph {
    L3MemoryGraph::new(
        vec![L3MemoryGraphEntity::extracted(
            "Alice",
            "person",
            "a colleague",
        )],
        vec![],
    )
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn wait_for_compaction(summarizer: &common::StubSummarizer) {
    for _ in 0..100 {
        if !summarizer.seen.lock().unwrap().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn maintenance_resumes_across_provider_restarts() {
    let summarizer = Arc::new(common::StubSummarizer::new(vec![summary(), summary()]));
    let mut parts = common::Parts::default();
    parts.summarizer = Some(summarizer.clone());
    parts.extractor = Some(Arc::new(common::StubExtractor::new(vec![graph(), graph()])));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.config = Some(LayeredMemoryConfig {
        l3_distill_min_age_seconds: 0,
        ..Default::default()
    });
    let config = parts.config.clone().unwrap();
    let l1 = Arc::new(InProcessL1MemoryStore::new(config.l1_turns));
    let l2 = Arc::new(InProcessL2MemoryStore::new(
        config.l2_entries,
        config.l2_versions,
    ));
    let graph_store = Arc::new(InProcessL3MemoryGraphStore::default());
    let vectors = Arc::new(InProcessL3MemoryVectorStore::default());
    let partition = MemoryScope::new("conversation:restart");
    let long_term = MemoryScope::new(format!(
        "user:{}",
        Subject::of(SubjectType::User, "u-layered")
    ));

    // Process A: six turns, below the window — nothing is compacted yet.
    {
        let fixture = common::fixture_over(
            parts.clone(),
            Arc::clone(&l1),
            Arc::clone(&l2),
            Arc::clone(&graph_store),
            Arc::clone(&vectors),
        );
        let agent = Agent::builder()
            .runtime(&fixture.runtime)
            .model(common::mock(&["a", "b", "c", "d", "e", "f"]))
            .build()
            .unwrap();
        let mut conversation = Conversation::with_id(&fixture.runtime, &fixture.subject, "restart");
        for input in ["one", "two", "three", "four", "five", "six"] {
            agent.run(conversation.turn_input(input)).await.unwrap();
        }
        assert_eq!(l1.count(&partition).unwrap(), 6);
        assert_eq!(l2.count(&partition).unwrap(), 0);
    }

    // Process B: a rebuilt provider resumes from the same stores.
    let fixture = common::fixture_over(parts, l1, l2, graph_store, vectors);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["g", "h", "i", "j"]))
        .build()
        .unwrap();
    let mut conversation = Conversation::with_id(&fixture.runtime, &fixture.subject, "restart");
    for input in ["seven", "eight", "nine", "ten"] {
        agent.run(conversation.turn_input(input)).await.unwrap();
    }
    wait_for_compaction(&summarizer).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let seen = summarizer.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "one cross-restart compaction");
    assert_eq!(seen[0].len(), 10, "the batch is the durable pending set");
    assert_eq!(
        &seen[0][..6],
        &["one", "two", "three", "four", "five", "six"],
        "the pre-restart turns are in the batch"
    );
    assert!(
        fixture.l1.uncompacted(&partition).unwrap().is_empty(),
        "the cursor advanced past the whole batch"
    );
    assert_eq!(fixture.l2.count(&partition).unwrap(), 1);
    assert_eq!(
        fixture.graph.entities(&long_term).unwrap().len(),
        1,
        "the aged batch (window 0) distilled"
    );
}

#[tokio::test]
async fn a_fresh_batch_compacts_without_distilling_by_default() {
    let mut parts = common::Parts::default();
    parts.summarizer = Some(Arc::new(common::StubSummarizer::new(vec![summary()])));
    parts.extractor = Some(Arc::new(common::StubExtractor::new(vec![graph()])));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.config = Some(LayeredMemoryConfig {
        l1_turns: 2,
        ..Default::default()
    });
    let fixture = common::fixture(parts);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["a", "b"]))
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();
    agent.run(conversation.turn_input("one")).await.unwrap();
    agent.run(conversation.turn_input("two")).await.unwrap();

    let partition = fixture.conversation_scope(&conversation);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fixture.l2.count(&partition).unwrap(), 1, "compacted");
    let progress = fixture.progress.events.lock().unwrap();
    assert!(
        progress
            .iter()
            .any(|event| matches!(event, LayeredMemoryEvent::Compacted { .. }))
    );
    assert!(
        !progress
            .iter()
            .any(|event| matches!(event, LayeredMemoryEvent::Distilled { .. })),
        "the fresh batch is below the one-day settle window"
    );
    let long_term = MemoryScope::new(format!("user:{}", fixture.subject));
    assert!(fixture.graph.entities(&long_term).unwrap().is_empty());
}

#[tokio::test]
async fn an_aged_backlog_distills_on_compaction() {
    let mut parts = common::Parts::default();
    parts.summarizer = Some(Arc::new(common::StubSummarizer::new(vec![summary()])));
    parts.extractor = Some(Arc::new(common::StubExtractor::new(vec![graph()])));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.config = Some(LayeredMemoryConfig {
        l1_turns: 2,
        ..Default::default()
    });
    let config = parts.config.clone().unwrap();
    let l1 = Arc::new(InProcessL1MemoryStore::new(10));
    let l2 = Arc::new(InProcessL2MemoryStore::new(
        config.l2_entries,
        config.l2_versions,
    ));
    let graph_store = Arc::new(InProcessL3MemoryGraphStore::default());
    let vectors = Arc::new(InProcessL3MemoryVectorStore::default());
    let fixture = common::fixture_over(parts, l1, l2, graph_store, vectors);
    let mut conversation = fixture.conversation();
    let partition = fixture.conversation_scope(&conversation);
    let old = now_seconds() - 2 * 86_400;
    fixture
        .l1
        .append(L1MemoryEntry::new(
            partition.clone(),
            "t",
            "old one",
            "r1",
            old,
        ))
        .unwrap();
    fixture
        .l1
        .append(L1MemoryEntry::new(
            partition.clone(),
            "t",
            "old two",
            "r2",
            old + 60,
        ))
        .unwrap();

    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["a"]))
        .build()
        .unwrap();
    agent.run(conversation.turn_input("three")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(fixture.l2.count(&partition).unwrap(), 1);
    assert_eq!(
        fixture.l1.uncompacted(&partition).unwrap().len(),
        1,
        "the aged chunk advanced; the fresh turn stays pending"
    );
    let progress = fixture.progress.events.lock().unwrap();
    assert!(
        progress
            .iter()
            .any(|event| matches!(event, LayeredMemoryEvent::Distilled { .. }))
    );
    drop(progress);
    let long_term = MemoryScope::new(format!("user:{}", fixture.subject));
    assert_eq!(fixture.graph.entities(&long_term).unwrap().len(), 1);
}

#[tokio::test]
async fn conversation_end_drains_the_pending_backlog_in_chunks() {
    let mut parts = common::Parts::default();
    parts.summarizer = Some(Arc::new(common::StubSummarizer::new(vec![
        summary(),
        summary(),
        summary(),
    ])));
    parts.extractor = Some(Arc::new(common::StubExtractor::new(vec![
        graph(),
        graph(),
        graph(),
    ])));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.config = Some(LayeredMemoryConfig {
        l1_turns: 2,
        ..Default::default()
    });
    let config = parts.config.clone().unwrap();
    let l1 = Arc::new(InProcessL1MemoryStore::new(10));
    let l2 = Arc::new(InProcessL2MemoryStore::new(
        config.l2_entries,
        config.l2_versions,
    ));
    let graph_store = Arc::new(InProcessL3MemoryGraphStore::default());
    let vectors = Arc::new(InProcessL3MemoryVectorStore::default());
    let fixture = common::fixture_over(parts, l1, l2, graph_store, vectors);
    let conversation = fixture.conversation();
    let partition = fixture.conversation_scope(&conversation);
    for index in 0..5 {
        fixture
            .l1
            .append(L1MemoryEntry::new(
                partition.clone(),
                "t",
                format!("q{index}"),
                format!("a{index}"),
                1_700_000_000 + index,
            ))
            .unwrap();
    }

    conversation.end(&fixture.runtime).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        fixture.l1.uncompacted(&partition).unwrap().is_empty(),
        "the end path drains the backlog"
    );
    assert_eq!(fixture.l2.count(&partition).unwrap(), 3, "2 + 2 + 1 chunks");
    let progress = fixture.progress.events.lock().unwrap();
    let distilled = progress
        .iter()
        .filter(|event| matches!(event, LayeredMemoryEvent::Distilled { .. }))
        .count();
    assert_eq!(distilled, 3, "each chunk distilled");
}

/// A summarizer that blocks until the test releases it.
struct BlockingSummarizer {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

impl L2MemorySummarizer for BlockingSummarizer {
    fn summarize<'a>(
        &'a self,
        _input: L2MemorySummaryInput<'a>,
    ) -> BoxFuture<'a, Result<L2MemorySummaryOutput, synonz::MemoryFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            self.release.notified().await;
            Ok(summary())
        })
    }
}

#[tokio::test]
async fn overlapping_triggers_serialize_per_conversation() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut parts = common::Parts::default();
    parts.summarizer = Some(Arc::new(BlockingSummarizer {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        calls: Arc::clone(&calls),
    }));
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.config = Some(LayeredMemoryConfig {
        l1_turns: 2,
        ..Default::default()
    });
    let fixture = common::fixture(parts);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["a", "b", "c"]))
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();

    agent.run(conversation.turn_input("one")).await.unwrap();
    agent.run(conversation.turn_input("two")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the compaction starts");
    // The third turn triggers while the first run is still in flight; the
    // busy slot skips it.
    agent.run(conversation.turn_input("three")).await.unwrap();
    release.notify_one();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the overlapping compaction was skipped"
    );
    let partition = fixture.conversation_scope(&conversation);
    assert_eq!(fixture.l2.count(&partition).unwrap(), 1);
    assert_eq!(
        fixture.l1.uncompacted(&partition).unwrap().len(),
        1,
        "the third turn stays pending for the next window"
    );
}

/// An L2 store whose writes fail while the flag is set.
struct FailingL2Store {
    inner: Arc<InProcessL2MemoryStore>,
    fail: Arc<AtomicBool>,
}

impl L2MemoryStore for FailingL2Store {
    fn upsert(&self, entry: L2MemoryEntry) -> Result<(), MemoryStoreError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(MemoryStoreError::Storage("down".into()));
        }
        self.inner.upsert(entry)
    }

    fn get(
        &self,
        scope: &MemoryScope,
        id: &str,
    ) -> Result<Option<L2MemoryEntry>, MemoryStoreError> {
        self.inner.get(scope, id)
    }

    fn list(&self, scope: &MemoryScope) -> Result<Vec<L2MemoryEntry>, MemoryStoreError> {
        self.inner.list(scope)
    }

    fn scopes(&self, subject: &Subject) -> Result<Vec<MemoryScope>, MemoryStoreError> {
        self.inner.scopes(subject)
    }

    fn remove(&self, scope: &MemoryScope, id: &str) -> Result<bool, MemoryStoreError> {
        self.inner.remove(scope, id)
    }

    fn count(&self, scope: &MemoryScope) -> Result<usize, MemoryStoreError> {
        self.inner.count(scope)
    }
}

#[tokio::test]
async fn a_failed_compaction_leaves_the_cursor_and_retries() {
    let l1 = Arc::new(InProcessL1MemoryStore::new(10));
    let l2 = Arc::new(InProcessL2MemoryStore::new(5, 5));
    let fail = Arc::new(AtomicBool::new(true));
    let provider = LayeredMemoryProvider::builder()
        .l1_store(common::InProcessL1MemoryStoreView(Arc::clone(&l1)))
        .l2_store(FailingL2Store {
            inner: Arc::clone(&l2),
            fail: Arc::clone(&fail),
        })
        .config(LayeredMemoryConfig {
            l1_turns: 2,
            ..Default::default()
        })
        .summarizer(common::SharedSummarizer(Arc::new(
            common::StubSummarizer::new(vec![summary(), summary(), summary()]),
        )))
        .context_rewriter(common::SharedRewriter(Arc::new(
            common::EchoRewriter::default(),
        )))
        .build();
    let facts = common::FactLog::default();
    let runtime = SynonzRuntime::builder()
        .memory_provider(provider)
        .observer(facts.clone())
        .build();
    let subject = Subject::of(SubjectType::User, "u-crash");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(common::mock(&["a", "b", "c", "d"]))
        .build()
        .unwrap();
    let mut conversation = Conversation::with_id(&runtime, &subject, "crash");
    let partition = MemoryScope::new("conversation:crash");

    agent.run(conversation.turn_input("one")).await.unwrap();
    agent.run(conversation.turn_input("two")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        l1.uncompacted(&partition).unwrap().len(),
        2,
        "the failed write did not advance the cursor"
    );
    assert!(
        facts
            .facts
            .lock()
            .unwrap()
            .iter()
            .any(|fact| fact == "failed:compact"),
        "the failure is visible"
    );

    fail.store(false, Ordering::SeqCst);
    agent.run(conversation.turn_input("three")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        l1.uncompacted(&partition).unwrap().len(),
        1,
        "the retried batch advanced; the third turn stays pending"
    );
    assert_eq!(l2.count(&partition).unwrap(), 1);

    agent.run(conversation.turn_input("four")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(l1.uncompacted(&partition).unwrap().is_empty());
}
