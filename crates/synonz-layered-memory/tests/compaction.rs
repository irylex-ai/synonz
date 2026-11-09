//! The explicit context-compression command: boundary resolution, the
//! configured keep, the L1 recall exclusion, failure and restart semantics,
//! and the maintenance serialization.

#![allow(clippy::field_reassign_with_default)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use synonz::{Agent, Conversation, Message};
use synonz_layered_memory::{
    L1MemoryStore, L2MemoryStore, L2MemorySummarizer, L2MemorySummary, L2MemorySummaryInput,
    L2MemorySummaryOutput, LayeredMemoryConfig, LayeredMemoryEvent,
};
use tokio::sync::Notify;

/// A scripted summary for every compaction call.
fn summary(content: &str) -> L2MemorySummaryOutput {
    L2MemorySummaryOutput::new(vec![L2MemorySummary::new(None, "billing", 0.8, content)])
}

fn parts_with(recent: usize, keep: usize, summaries: Vec<L2MemorySummaryOutput>) -> common::Parts {
    let mut parts = common::Parts::default();
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.summarizer = Some(Arc::new(common::StubSummarizer::new(summaries)));
    parts.config = Some(LayeredMemoryConfig {
        context_recent_turns: recent,
        context_compact_keep_turns: keep,
        ..Default::default()
    });
    parts
}

fn supplement(frame: &[Message]) -> String {
    frame
        .last()
        .map(|message| {
            message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    synonz::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn compacting_keeps_the_configured_recent_turns() {
    let fixture = common::fixture(parts_with(
        5,
        2,
        vec![summary("the user asked about invoices")],
    ));
    let mut conversation = fixture.conversation();
    let partition = fixture.conversation_scope(&conversation);
    let model = common::RecordingModel::new("ok");
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(model.clone())
        .build()
        .unwrap();
    for input in ["one", "two", "three", "four", "five", "six"] {
        agent.run(conversation.turn_input(input)).await.unwrap();
    }

    fixture
        .actuator
        .compact_context(&fixture.subject, conversation.id())
        .await
        .unwrap();

    // The boundary is the newest visible turn minus the configured keep.
    let visible = fixture.l1.recent(&partition, 10).unwrap();
    assert_eq!(visible.len(), 6);
    assert_eq!(
        fixture.l1.context_boundary(&partition).unwrap(),
        Some(visible[3].id.clone()),
        "keep 2 closes the window on turn four"
    );
    let pending = fixture.l1.uncompacted(&partition).unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|entry| entry.input.as_str())
            .collect::<Vec<_>>(),
        vec!["five", "six"],
        "everything through the boundary is consolidated"
    );
    assert_eq!(fixture.l2.count(&partition).unwrap(), 1);

    // The next turn's frame replays only the kept turns; the compressed
    // turns are not recalled verbatim.
    agent.run(conversation.turn_input("seven")).await.unwrap();
    let requests = model.requests();
    let frame = &requests.last().unwrap().messages;
    assert_eq!(
        frame,
        &vec![
            Message::user("five"),
            Message::assistant_text("ok"),
            Message::user("six"),
            Message::assistant_text("ok"),
            Message::user("seven [recalled: the user asked about invoices]"),
        ],
        "the kept turns are replayed; the compressed turns leave the window"
    );

    // The assembled fact reports the compressed window size.
    let events = fixture.progress.events.lock().unwrap();
    let assembled = events
        .iter()
        .rev()
        .find_map(|event| match event {
            LayeredMemoryEvent::Assembled { verbatim_turns, .. } => Some(*verbatim_turns),
            _ => None,
        })
        .expect("an assembled fact");
    assert_eq!(assembled, 2);
}

#[tokio::test]
async fn the_window_grows_back_after_a_compression() {
    let fixture = common::fixture(parts_with(5, 2, vec![summary("invoices")]));
    let mut conversation = fixture.conversation();
    let model = common::RecordingModel::new("ok");
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(model.clone())
        .build()
        .unwrap();
    for input in ["one", "two", "three", "four", "five", "six"] {
        agent.run(conversation.turn_input(input)).await.unwrap();
    }
    fixture
        .actuator
        .compact_context(&fixture.subject, conversation.id())
        .await
        .unwrap();

    for input in ["seven", "eight", "nine", "ten"] {
        agent.run(conversation.turn_input(input)).await.unwrap();
    }

    let requests = model.requests();
    let frame = &requests.last().unwrap().messages;
    assert_eq!(
        &frame[..10],
        &[
            Message::user("five"),
            Message::assistant_text("ok"),
            Message::user("six"),
            Message::assistant_text("ok"),
            Message::user("seven"),
            Message::assistant_text("ok"),
            Message::user("eight"),
            Message::assistant_text("ok"),
            Message::user("nine"),
            Message::assistant_text("ok"),
        ],
        "the window grows back from the kept turns to the configured size"
    );
    assert!(supplement(frame).starts_with("ten [recalled: "));
}

#[tokio::test]
async fn a_zero_keep_clears_the_visible_window() {
    let fixture = common::fixture(parts_with(3, 0, vec![summary("invoices")]));
    let mut conversation = fixture.conversation();
    let partition = fixture.conversation_scope(&conversation);
    let model = common::RecordingModel::new("ok");
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(model.clone())
        .build()
        .unwrap();
    for input in ["one", "two", "three"] {
        agent.run(conversation.turn_input(input)).await.unwrap();
    }

    fixture
        .actuator
        .compact_context(&fixture.subject, conversation.id())
        .await
        .unwrap();

    let visible = fixture.l1.recent(&partition, 10).unwrap();
    assert_eq!(
        fixture.l1.context_boundary(&partition).unwrap(),
        Some(visible[2].id.clone()),
        "keep 0 closes the window on the newest turn"
    );

    agent.run(conversation.turn_input("four")).await.unwrap();
    let requests = model.requests();
    let frame = &requests.last().unwrap().messages;
    assert_eq!(frame.len(), 1, "no replayed turns: {frame:?}");
    assert!(supplement(frame).starts_with("four [recalled: "));
    assert!(
        !supplement(frame).contains("用户：three"),
        "the cleared turns are not recalled verbatim"
    );
}

#[tokio::test]
async fn the_command_is_a_no_op_without_a_visible_window() {
    let fixture = common::fixture(parts_with(0, 2, Vec::new()));
    let mut conversation = fixture.conversation();
    let partition = fixture.conversation_scope(&conversation);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok"]))
        .build()
        .unwrap();
    agent.run(conversation.turn_input("one")).await.unwrap();
    agent.run(conversation.turn_input("two")).await.unwrap();

    fixture
        .actuator
        .compact_context(&fixture.subject, conversation.id())
        .await
        .unwrap();
    assert_eq!(fixture.l1.context_boundary(&partition).unwrap(), None);
    assert_eq!(fixture.l2.count(&partition).unwrap(), 0);
}

#[tokio::test]
async fn the_command_is_a_no_op_when_the_keep_covers_every_turn() {
    let fixture = common::fixture(parts_with(3, 5, Vec::new()));
    let mut conversation = fixture.conversation();
    let partition = fixture.conversation_scope(&conversation);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok", "ok"]))
        .build()
        .unwrap();
    for input in ["one", "two", "three"] {
        agent.run(conversation.turn_input(input)).await.unwrap();
    }

    fixture
        .actuator
        .compact_context(&fixture.subject, conversation.id())
        .await
        .unwrap();
    assert_eq!(fixture.l1.context_boundary(&partition).unwrap(), None);
    assert_eq!(fixture.l2.count(&partition).unwrap(), 0);
    assert_eq!(fixture.l1.uncompacted(&partition).unwrap().len(), 3);
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
async fn a_failing_compression_leaves_the_boundary_unadvanced() {
    let mut parts = parts_with(3, 1, Vec::new());
    parts.summarizer = Some(Arc::new(FailingSummarizer));
    let fixture = common::fixture(parts);
    let mut conversation = fixture.conversation();
    let partition = fixture.conversation_scope(&conversation);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok", "ok"]))
        .build()
        .unwrap();
    for input in ["one", "two", "three"] {
        agent.run(conversation.turn_input(input)).await.unwrap();
    }

    let failure = fixture
        .actuator
        .compact_context(&fixture.subject, conversation.id())
        .await
        .expect_err("the compaction fails");
    assert_eq!(failure.stage, "summarize");
    assert_eq!(
        fixture.l1.context_boundary(&partition).unwrap(),
        None,
        "the boundary only advances after consolidation"
    );
    assert_eq!(
        fixture.l1.uncompacted(&partition).unwrap().len(),
        3,
        "the pending set is intact for the retry"
    );
}

#[tokio::test]
async fn the_command_without_a_model_fails_explicitly() {
    let mut parts = parts_with(3, 1, Vec::new());
    parts.without_model = true;
    let fixture = common::fixture(parts);
    let mut conversation = fixture.conversation();
    let partition = fixture.conversation_scope(&conversation);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok", "ok"]))
        .build()
        .unwrap();
    for input in ["one", "two", "three"] {
        agent.run(conversation.turn_input(input)).await.unwrap();
    }

    let failure = fixture
        .actuator
        .compact_context(&fixture.subject, conversation.id())
        .await
        .expect_err("a pending compression needs the model");
    assert_eq!(failure.stage, "compress");
    assert!(failure.detail.contains("no context-management model"));
    assert_eq!(fixture.l1.context_boundary(&partition).unwrap(), None);
}

#[tokio::test]
async fn the_compression_boundary_survives_a_restart() {
    let parts = parts_with(3, 1, vec![summary("invoices")]);
    let fixture = common::fixture(parts.clone());
    let mut conversation =
        Conversation::with_id(&fixture.runtime, &fixture.subject, "compress-restart");
    let partition = fixture.conversation_scope(&conversation);
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok", "ok"]))
        .build()
        .unwrap();
    for input in ["one", "two", "three"] {
        agent.run(conversation.turn_input(input)).await.unwrap();
    }
    fixture
        .actuator
        .compact_context(&fixture.subject, conversation.id())
        .await
        .unwrap();
    let boundary = fixture.l1.context_boundary(&partition).unwrap();
    let boundary_id = boundary.clone().expect("the boundary is set");
    let l1 = Arc::clone(&fixture.l1);
    let l2 = Arc::clone(&fixture.l2);
    let graph = Arc::clone(&fixture.graph);
    let vectors = Arc::clone(&fixture.vectors);
    drop(fixture);

    // A rebuilt component reads the persisted boundary from the same
    // stores; the compressed turns never return to the recall.
    let fixture = common::fixture_over(parts, l1, l2, graph, vectors);
    assert_eq!(
        fixture.l1.context_boundary(&partition).unwrap(),
        Some(boundary_id),
        "the boundary outlives the process"
    );
    let model = common::RecordingModel::new("ok");
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(model.clone())
        .build()
        .unwrap();
    let mut conversation =
        Conversation::with_id(&fixture.runtime, &fixture.subject, "compress-restart");
    agent.run(conversation.turn_input("four")).await.unwrap();

    let requests = model.requests();
    let frame = &requests.last().unwrap().messages;
    let recalled = supplement(frame);
    assert!(
        recalled.contains("three"),
        "the post-boundary turn is recallable: {recalled}"
    );
    assert!(
        !recalled.contains("one") && !recalled.contains("two"),
        "the compressed turns stay out of the recall: {recalled}"
    );
}

/// A summarizer that blocks its first call until the test releases it, then
/// answers normally.
struct BlockingOnceSummarizer {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

impl L2MemorySummarizer for BlockingOnceSummarizer {
    fn summarize<'a>(
        &'a self,
        _input: L2MemorySummaryInput<'a>,
    ) -> BoxFuture<'a, Result<L2MemorySummaryOutput, synonz::MemoryFailure>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(summary("invoices"))
        })
    }
}

#[tokio::test]
async fn the_command_waits_for_the_background_maintenance() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut parts = common::Parts::default();
    parts.context_rewriter = Some(Arc::new(common::EchoRewriter::default()));
    parts.summarizer = Some(Arc::new(BlockingOnceSummarizer {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        calls: Arc::clone(&calls),
    }));
    parts.config = Some(LayeredMemoryConfig {
        l1_turns: 2,
        context_recent_turns: 3,
        context_compact_keep_turns: 1,
        ..Default::default()
    });
    let fixture = common::fixture(parts);
    let mut conversation = fixture.conversation();
    let conversation_id = conversation.id().to_string();
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["ok", "ok"]))
        .build()
        .unwrap();
    for input in ["one", "two"] {
        agent.run(conversation.turn_input(input)).await.unwrap();
    }

    // The window-filled background run starts and blocks inside its
    // summarizer.
    entered.notified().await;

    let partition = fixture.conversation_scope(&conversation);
    let actuator = fixture.actuator;
    let subject = fixture.subject.clone();
    let command =
        tokio::spawn(async move { actuator.compact_context(&subject, &conversation_id).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !command.is_finished(),
        "the command waits for the in-flight maintenance"
    );

    release.notify_one();
    command.await.unwrap().unwrap();

    assert!(
        fixture.l1.context_boundary(&partition).unwrap().is_some(),
        "the command ran after the background run"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the command found the material consolidated and only closed the window"
    );
}
