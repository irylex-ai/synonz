//! Acceptance tests for the memory & context system (0.3.0 form).
//!
//! Verifies: layered assembly (L3 recall / L2 summaries / L1 window),
//! the engine's maintenance (L1 write with `TurnArchived`, floor
//! compaction with visible `ContextManagement` calls, L2 distillation),
//! conversation-end teardown (drain + mechanical promotion), topic
//! tracking with `TopicShifted` facts, strategy-slot replacement, and
//! the never-silent memory-failure rule (synchronous returns and
//! background facts both visible).

#![cfg(feature = "test-util")]

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use synonz::{
    Agent, Context, ContextAssembler, ContextAssemblerInput, ContextAssemblerOutput, Conversation,
    ConversationTopicDetector, MockModel, ModelStreamItem, Subject, SubjectType, SynonzEvent,
    SynonzRuntime,
};

fn env() -> (SynonzRuntime, Subject) {
    (
        SynonzRuntime::builder().build(),
        Subject::of(SubjectType::User, "test-user"),
    )
}

fn text_model(replies: &[&str]) -> MockModel {
    MockModel::new(
        replies
            .iter()
            .map(|text| {
                vec![ModelStreamItem::Finish {
                    message: synonz::Message::assistant_text(*text),
                    usage: synonz::TokenUsage::new(1, 1),
                }]
            })
            .collect(),
    )
}

/// A model that routes by request shape: summarization calls (the
/// built-in summarizer's prompt) consume the summary script; everything
/// else consumes the reasoning script. Deterministic despite background
/// compaction racing the next turn.
struct RoutingModel {
    reasoning: MockModel,
    summarization: MockModel,
}

impl RoutingModel {
    fn new(reasoning: &[&str], summarization: &[&str]) -> Self {
        Self {
            reasoning: text_model(reasoning),
            summarization: text_model(summarization),
        }
    }
}

impl synonz::Model for RoutingModel {
    fn stream(
        &self,
        request: synonz::ModelRequest,
    ) -> synonz::BoxFuture<'_, Result<synonz::ModelStream, synonz::ModelError>> {
        let is_summary = request.messages.iter().rev().any(|m| {
            m.role == synonz::Role::User
                && m.blocks.iter().any(|b| {
                    matches!(b, synonz::ContentBlock::Text { text }
                        if text.starts_with("Summarize the following"))
                })
        });
        if is_summary {
            self.summarization.stream(request)
        } else {
            self.reasoning.stream(request)
        }
    }
}

/// Polls until the condition holds (background maintenance is eventual —
/// the drain points are deterministic; this poller covers in-between).
async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..100 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("background maintenance never reached the expected state");
}

#[tokio::test]
async fn post_turn_flow_writes_l1_and_demotes_on_turn_count() {
    let (_unused, subject) = env();
    // Small window: overflow after the first turn, so compaction fires
    // and L2 receives a block.
    let runtime = SynonzRuntime::builder().build();
    let model = RoutingModel::new(&["answer one", "answer two"], &["summary"]);
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-1");
    let agent = Agent::builder()
        .runtime(&runtime)
        .context(Context::new().l1_window(1).l2_cap(4))
        .model(model)
        .build()
        .unwrap();

    let _ = agent.run(conv.turn_input("one")).await.unwrap();
    assert_eq!(conv.len(), 1);

    // After turn one, L1 holds one entry (within window).
    let memory = runtime.memory();
    assert_eq!(memory.l1_len(&subject, "conv-1").unwrap(), 1);

    // Turn two overflows: the background flow demotes the oldest into L2
    // (the summarization lane consumes the "summary" script).
    let _ = agent.run(conv.turn_input("two")).await.unwrap();
    eventually(|| memory.l2_len(&subject, "conv-1").unwrap() >= 1).await;
    assert_eq!(
        memory.l1_len(&subject, "conv-1").unwrap(),
        1,
        "window enforced"
    );
    assert_eq!(
        memory.l2_len(&subject, "conv-1").unwrap(),
        1,
        "demoted into L2"
    );
}

#[tokio::test]
async fn l2_overflow_distills_into_l3() {
    let (_unused, subject) = env();
    let runtime = SynonzRuntime::builder().build();
    // L2 capped at 1: any second summary block distills into L3.
    let model = RoutingModel::new(&["a1", "a2", "a3"], &["sum1", "sum2"]);
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-2");
    let agent = Agent::builder()
        .runtime(&runtime)
        .context(Context::new().l1_window(1).l2_cap(1))
        .model(model)
        .build()
        .unwrap();

    for text in ["one", "two", "three"] {
        let _ = agent.run(conv.turn_input(text)).await.unwrap();
    }
    let memory = runtime.memory();
    // Poll the STABLE end state: the last turn's job ran to completion
    // (distillation happened) — L1 windowed, L2 capped, L3 fed.
    eventually(|| memory.l3_len(&subject).unwrap() >= 1).await;
    assert_eq!(memory.l1_len(&subject, "conv-2").unwrap(), 1);
    // L2 capped at 1; the rest distilled into L3.
    assert_eq!(memory.l2_len(&subject, "conv-2").unwrap(), 1);
    assert!(memory.l3_len(&subject).unwrap() >= 1, "distilled into L3");
}

#[tokio::test]
async fn conversation_end_drains_and_promotes_l2_into_l3() {
    let (_unused, subject) = env();
    let runtime = SynonzRuntime::builder().build();
    // Promotion is now structural (no policy gate): conversation end
    // drains the background maintenance, then mechanically promotes.
    let model = RoutingModel::new(&["a1", "a2"], &["sum1"]);
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-3");
    let agent = Agent::builder()
        .runtime(&runtime)
        .context(Context::new().l1_window(1).l2_cap(4))
        .model(model)
        .build()
        .unwrap();

    let _ = agent.run(conv.turn_input("one")).await.unwrap();
    let _ = agent.run(conv.turn_input("two")).await.unwrap();
    let memory = runtime.memory();
    eventually(|| memory.l2_len(&subject, "conv-3").unwrap() >= 1).await;

    // The end drains the background first — after it returns, the L2
    // block is promoted into L3 (deterministic, no polling needed).
    conv.end(&runtime).await;
    assert_eq!(
        memory.l2_len(&subject, "conv-3").unwrap(),
        0,
        "promoted away"
    );
    assert!(memory.l3_len(&subject).unwrap() >= 1);
}

#[tokio::test]
async fn layered_assembly_reads_memory_layers() {
    let (runtime, subject) = env();
    // Seed L1/L2/L3 directly through the memory facade.
    let memory = runtime.memory();
    memory
        .l1_append(
            &subject,
            "conv-4",
            &"weather".to_string(),
            vec![
                synonz::Message::user("what about beijing?"),
                synonz::Message::assistant_text("sunny"),
            ],
        )
        .unwrap();
    memory
        .l2_append(
            &subject,
            synonz::L2Entry::new("conv-4", "earlier we discussed travel plans", 0),
        )
        .unwrap();
    memory
        .l3_upsert(
            &subject,
            synonz::L3Entry::new(
                synonz::L3Identity {
                    subject_id: subject.to_string(),
                    conversation_id: "old-conv".into(),
                    topic: "weather".into(),
                },
                "the user prefers celsius",
            ),
        )
        .unwrap();

    // Drive a run: the assembled background reaches the model request.
    let model = text_model(&["ok"]);
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-4");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .build()
        .unwrap();
    let _ = agent
        .run(conv.turn_input("what does the user prefer?"))
        .await
        .unwrap();

    let request = &model.requests()[0];
    // L3 recall first (independent System message).
    assert!(request.messages[0].blocks.iter().any(|b| {
        matches!(b, synonz::ContentBlock::Text { text } if text.contains("Memory recall")
            && text.contains("prefers celsius"))
    }));
    // L2 summaries present.
    assert!(request.messages.iter().any(|m| m.blocks.iter().any(
        |b| matches!(b, synonz::ContentBlock::Text { text } if text.contains("travel plans"))
    )));
    // L1 turns present (verbatim).
    assert!(request.messages.iter().any(|m| m.blocks.iter().any(
        |b| matches!(b, synonz::ContentBlock::Text { text } if text.contains("what about beijing?"))
    )));
}

/// A custom assembler: the slot contract survives the narrowing — it
/// reads memory through the input and composes its own background.
struct PrependStrategy;

impl ContextAssembler for PrependStrategy {
    fn assemble<'a>(
        &'a self,
        input: ContextAssemblerInput<'a>,
    ) -> synonz::BoxFuture<'a, ContextAssemblerOutput> {
        Box::pin(async move {
            let mut output = ContextAssemblerOutput::default();
            output
                .messages
                .push(synonz::Message::system("custom strategy was here"));
            for entry in input
                .reader
                .l1_window(input.conversation_id)
                .unwrap_or_default()
            {
                output.messages.extend(entry.messages);
            }
            output
        })
    }
}

#[tokio::test]
async fn custom_assembler_slot_drives_assembly() {
    let (_first, subject) = env();
    let runtime = SynonzRuntime::builder().build();
    // Seed L1 through the memory facade (the strategy reads memory).
    runtime
        .memory()
        .l1_append(
            &subject,
            "conv-5",
            &"chat".to_string(),
            vec![synonz::Message::user("hi")],
        )
        .unwrap();

    let model = text_model(&["ok"]);
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-5");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .context(Context::new().with_assembler(PrependStrategy))
        .build()
        .unwrap();
    let _ = agent.run(conv.turn_input("follow up")).await.unwrap();

    // The request starts with the custom marker, then the seeded L1
    // (verbatim), then the current input.
    let request = &model.requests()[0];
    assert!(request.messages[0].blocks.iter().any(
        |b| matches!(b, synonz::ContentBlock::Text { text } if text == "custom strategy was here")
    ));
    assert!(
        request.messages[1]
            .blocks
            .iter()
            .any(|b| matches!(b, synonz::ContentBlock::Text { text } if text == "hi"))
    );
}

/// Memory stores whose reads fail — the degradation must be visible in
/// the assembly output, never silent. One per layer: all three layers
/// must report independently.
struct BrokenL1;
struct BrokenL2;
struct BrokenL3;

impl synonz::MemoryL1Store for BrokenL1 {
    fn append(
        &self,
        _: &Subject,
        _: &str,
        _: &String,
        _: Vec<synonz::Message>,
    ) -> Result<(), synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn window(
        &self,
        _: &Subject,
        _: &str,
    ) -> Result<Vec<synonz::L1Entry>, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn pop_oldest(
        &self,
        _: &Subject,
        _: &str,
        _: usize,
    ) -> Result<Vec<synonz::L1Entry>, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn len(&self, _: &Subject, _: &str) -> Result<usize, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
}

impl synonz::MemoryL2Store for BrokenL2 {
    fn append(&self, _: &Subject, _: synonz::L2Entry) -> Result<(), synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn read(&self, _: &Subject, _: &str) -> Result<Vec<synonz::L2Entry>, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn len(&self, _: &Subject, _: &str) -> Result<usize, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn pop_oldest(
        &self,
        _: &Subject,
        _: &str,
        _: usize,
    ) -> Result<Vec<synonz::L2Entry>, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
}

impl synonz::MemoryL3Store for BrokenL3 {
    fn upsert(&self, _: &Subject, _: synonz::L3Entry) -> Result<(), synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn query(
        &self,
        _: &Subject,
        _: &str,
        _: &String,
        _: usize,
    ) -> Result<Vec<synonz::L3Entry>, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn len(&self, _: &Subject) -> Result<usize, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
}

/// Records the stages of `FlowFailed` memory facts (bus-facing).
#[derive(Default, Clone)]
struct StageRecorder {
    stages: Arc<Mutex<Vec<synonz::MemoryFlowStage>>>,
}

impl synonz::Observer for StageRecorder {
    fn on_event(&self, _ctx: &synonz::ObserverContext, event: &SynonzEvent) {
        if let SynonzEvent::Memory(synonz::MemoryEvent::FlowFailed { stage, .. }) = event {
            self.stages.lock().unwrap().push(stage.clone());
        }
    }
}

#[tokio::test]
async fn assembly_memory_failures_are_visible_not_silent() {
    // Override all three layers with failing stores: the degradation
    // must be visible as facts, never silent.
    let recorder = StageRecorder::default();
    let runtime = SynonzRuntime::builder()
        .memory_l1_store(BrokenL1)
        .memory_l2_store(BrokenL2)
        .memory_l3_store(BrokenL3)
        .observer(recorder.clone())
        .build();
    let subject = Subject::of(SubjectType::User, "u");
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-6");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(text_model(&["answer"]))
        .build()
        .unwrap();
    let output = agent.run(conv.turn_input("anything")).await.unwrap();
    assert_eq!(output.text(), Some("answer"));

    // Every layer failed and every failure is reported — no silent
    // "no memory" degradation.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let stages = recorder.stages.lock().unwrap();
    let assemble_reads = stages
        .iter()
        .filter(|stage| **stage == synonz::MemoryFlowStage::AssembleRead)
        .count();
    assert_eq!(assemble_reads, 3, "l1, l2, l3 all report: {stages:?}");
}

/// A rewriter that marks the input as resolved (the model view).
struct MarkResolved;

impl synonz::TurnInputRewriter for MarkResolved {
    fn rewrite<'a>(
        &'a self,
        input: &'a str,
        _history: &'a [synonz::Message],
    ) -> synonz::BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move { Ok(Some(format!("resolved: {input}"))) })
    }
}

/// An assembler that exposes the model-view input it received.
struct ExposeView;

impl ContextAssembler for ExposeView {
    fn assemble<'a>(
        &'a self,
        input: ContextAssemblerInput<'a>,
    ) -> synonz::BoxFuture<'a, ContextAssemblerOutput> {
        Box::pin(async move {
            let mut output = ContextAssemblerOutput::default();
            if let Some(view) = input.rewritten_input {
                output.messages.push(synonz::Message::system(view));
            }
            output
        })
    }
}

#[tokio::test]
async fn rewriter_feeds_the_model_view_into_assembly() {
    let (runtime, subject) = env();
    let model = text_model(&["ok"]);
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-rw");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .context(
            Context::new()
                .with_rewriter(MarkResolved)
                .with_assembler(ExposeView),
        )
        .build()
        .unwrap();
    let _ = agent.run(conv.turn_input("go")).await.unwrap();

    let request = &model.requests()[0];
    // The assembler saw the model view...
    assert!(request.messages.iter().any(|m| {
        m.blocks
            .iter()
            .any(|b| matches!(b, synonz::ContentBlock::Text { text } if text == "resolved: go"))
    }));
    // ...while the current user message stays the original text.
    let last = request.messages.last().unwrap();
    assert!(
        last.blocks
            .iter()
            .any(|b| matches!(b, synonz::ContentBlock::Text { text } if text == "go"))
    );
}

/// A rewriter that always fails.
struct BrokenRewriter;

impl synonz::TurnInputRewriter for BrokenRewriter {
    fn rewrite<'a>(
        &'a self,
        _input: &'a str,
        _history: &'a [synonz::Message],
    ) -> synonz::BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async { Err("rewriter down".into()) })
    }
}

#[tokio::test]
async fn rewriter_failure_is_visible_and_degraded() {
    let recorder = StageRecorder::default();
    let runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
    let (_unused, subject) = env();
    let model = text_model(&["ok"]);
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-rw-fail");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .context(Context::new().with_rewriter(BrokenRewriter))
        .build()
        .unwrap();
    let output = agent.run(conv.turn_input("go")).await.unwrap();
    assert_eq!(output.text(), Some("ok"));

    tokio::time::sleep(Duration::from_millis(100)).await;
    let stages = recorder.stages.lock().unwrap();
    assert!(
        stages.contains(&synonz::MemoryFlowStage::Rewrite),
        "rewrite failure visible: {stages:?}"
    );
    // Degraded: the original input reached the model.
    let request = &model.requests()[0];
    let last = request.messages.last().unwrap();
    assert!(
        last.blocks
            .iter()
            .any(|b| matches!(b, synonz::ContentBlock::Text { text } if text == "go"))
    );
}

#[tokio::test]
async fn background_engine_drives_a_full_turn() {
    let (runtime, subject) = env();
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-7");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(text_model(&["the answer"]))
        .build()
        .unwrap();

    // The completed turn lands in the truth archive AND in L1.
    let output = agent.run(conv.turn_input("question")).await.unwrap();
    assert_eq!(output.text(), Some("the answer"));
    assert_eq!(conv.len(), 1);
    assert_eq!(runtime.memory().l1_len(&subject, "conv-7").unwrap(), 1);
}

/// An observer collecting memory and conversation facts (the bus-facing
/// acceptance checks).
#[derive(Default, Clone)]
struct FactRecorder {
    facts: Arc<Mutex<Vec<String>>>,
}

impl synonz::Observer for FactRecorder {
    fn on_event(&self, _ctx: &synonz::ObserverContext, event: &SynonzEvent) {
        let line = match event {
            SynonzEvent::Memory(fact) => match fact {
                synonz::MemoryEvent::TurnArchived { .. } => "archived".to_string(),
                synonz::MemoryEvent::Compacted { count, .. } => format!("compacted:{count}"),
                synonz::MemoryEvent::Distilled { count, .. } => format!("distilled:{count}"),
                synonz::MemoryEvent::Promoted { count, .. } => format!("promoted:{count}"),
                synonz::MemoryEvent::FlowFailed { moment, .. } => {
                    format!("flow-failed:{moment:?}")
                }
                _ => "memory-other".to_string(),
            },
            SynonzEvent::Conversation(fact) => match fact {
                synonz::ConversationEvent::TopicShifted { .. } => "shifted".to_string(),
                synonz::ConversationEvent::Ended { .. } => "ended".to_string(),
                _ => "conversation-other".to_string(),
            },
            _ => "other".to_string(),
        };
        self.facts.lock().unwrap().push(line);
    }
}

#[tokio::test]
async fn flow_facts_are_visible_on_the_bus() {
    let recorder = FactRecorder::default();
    let runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
    let (_unused, subject) = ((), Subject::of(SubjectType::User, "facts"));
    let model = RoutingModel::new(&["a1", "a2"], &["sum1"]);
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-8");
    let agent = Agent::builder()
        .runtime(&runtime)
        .context(Context::new().l1_window(1).l2_cap(4))
        .model(model)
        .build()
        .unwrap();

    let _ = agent.run(conv.turn_input("one")).await.unwrap();
    let _ = agent.run(conv.turn_input("two")).await.unwrap();
    conv.end(&runtime).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let facts = recorder.facts.lock().unwrap();
    assert!(
        facts
            .iter()
            .any(|f| f == "turn-archived" || f.starts_with("compacted")),
        "archive and compaction facts visible: {facts:?}"
    );
    assert!(
        facts.iter().any(|f| f.starts_with("promoted")),
        "promotion fact visible: {facts:?}"
    );
}

#[tokio::test]
async fn summarization_failure_is_a_visible_background_fact() {
    /// A summarizer that always fails: the engine must degrade losslessly
    /// and surface the failure — never silent.
    struct BrokenSummarizer;
    impl synonz::MemorySummarizer for BrokenSummarizer {
        fn summarize<'a>(
            &'a self,
            _entries: &'a [synonz::L1Entry],
            _model: &'a dyn synonz::Model,
            _events: &'a synonz::EventSink,
        ) -> synonz::BoxFuture<'a, Result<String, String>> {
            Box::pin(async { Err("summarizer down".into()) })
        }
    }

    let recorder = FactRecorder::default();
    let runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
    let (_unused, subject) = env();
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-9");
    let agent = Agent::builder()
        .runtime(&runtime)
        .context(
            Context::new()
                .l1_window(1)
                .with_summarizer(BrokenSummarizer),
        )
        .model(text_model(&["a1", "a2"]))
        .build()
        .unwrap();

    let _ = agent.run(conv.turn_input("one")).await.unwrap();
    let _ = agent.run(conv.turn_input("two")).await.unwrap();
    conv.end(&runtime).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let facts = recorder.facts.lock().unwrap();
    assert!(
        facts.iter().any(|f| f
            == &format!(
                "flow-failed:{:?}",
                synonz::MemoryFlowFailedMoment::Background
            )),
        "background failure visible, never silent: {facts:?}"
    );
}

#[tokio::test]
async fn topic_shift_compacts_and_emits_the_fact() {
    /// Shifts whenever a previous topic exists.
    struct AlwaysShifts;
    impl ConversationTopicDetector for AlwaysShifts {
        fn detect(&self, input: &str, current: Option<&str>) -> synonz::TopicDecision {
            synonz::TopicDecision {
                topic: input.to_string(),
                shifted: current.is_some() && !current.unwrap().is_empty(),
            }
        }
    }

    let recorder = FactRecorder::default();
    let runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
    let (_unused, subject) = env();
    let model = RoutingModel::new(&["a1", "a2"], &["sum1"]);
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-10");
    let agent = Agent::builder()
        .runtime(&runtime)
        .context(
            Context::new()
                .l1_window(8)
                .with_topic_detector(AlwaysShifts),
        )
        .model(model)
        .build()
        .unwrap();

    let _ = agent.run(conv.turn_input("weather talk")).await.unwrap();
    let _ = agent.run(conv.turn_input("new topic now")).await.unwrap();
    conv.end(&runtime).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let facts = recorder.facts.lock().unwrap();
    assert!(
        facts.iter().any(|f| f == "shifted"),
        "TopicShifted visible on the shift: {facts:?}"
    );
    // The pre-shift turns were flushed (compacted) as the shift's
    // consequence.
    assert!(
        facts.iter().any(|f| f.starts_with("compacted")),
        "the shift's compaction consequence fired: {facts:?}"
    );
}
