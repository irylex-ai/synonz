//! Core memory contract acceptance: the bundled provider, the contract
//! family's extension points, and the write-phase template's semantics.
//!
//! Requires the `test-util` feature (the tests run against `MockModel`).
#![cfg(feature = "test-util")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;

use synonz::{
    Agent, Conversation, EventBus, Memory, MemoryContextAssembleInput, MemoryContextAssembleOutput,
    MemoryContextAssembler, MemoryFailure, MemoryForgetResult, MemoryItem, MemoryListCursor,
    MemoryPage, MemoryPipeline, MemoryProvider, MemoryQuery, MemoryStoreError, Message, MockModel,
    Model, ModelError, ModelRequest, ModelStream, ModelStreamItem, Observer, ObserverContext,
    PipelineTurnContext, RewriteInput, RewriterProvider, Role, Subject, SubjectType, SynonzEvent,
    TokenUsage, TopicDetectInput, TopicDetector, TopicDetectorProvider, TurnInputRewriter,
};

// ────────────────────────── test doubles ──────────────────────────

/// A model that records every request it receives and answers with a fixed
/// text (no script sequencing — auxiliary calls are visible too).
#[derive(Clone)]
struct RecordingModel {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    reply: String,
}

impl RecordingModel {
    fn new(reply: impl Into<String>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            reply: reply.into(),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Model for RecordingModel {
    fn stream(&self, request: ModelRequest) -> BoxFuture<'_, Result<ModelStream, ModelError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.reply.clone();
        Box::pin(async move {
            Ok(futures::stream::iter(vec![ModelStreamItem::Finish {
                message: Message::assistant_text(reply),
                usage: TokenUsage::new(1, 1),
            }])
            .boxed())
        })
    }
}

/// Records memory facts as readable strings.
#[derive(Default, Clone)]
struct FactObserver {
    facts: Arc<Mutex<Vec<String>>>,
}

impl Observer for FactObserver {
    fn on_event(&self, _ctx: &ObserverContext, event: &SynonzEvent) {
        let fact = match event {
            SynonzEvent::Memory(synonz::MemoryEvent::TurnArchived { topic, .. }) => {
                Some(format!("archived:{topic}"))
            }
            SynonzEvent::Memory(synonz::MemoryEvent::Failed {
                stage,
                detail,
                moment,
            }) => Some(format!("failed:{stage}:{moment:?}:{detail}")),
            SynonzEvent::Memory(synonz::MemoryEvent::Updated { scope, id, .. }) => {
                Some(format!("updated:{scope}:{id}"))
            }
            SynonzEvent::Memory(synonz::MemoryEvent::Removed { scope, ids, .. }) => {
                Some(format!("removed:{scope}:{}", ids.join(",")))
            }
            SynonzEvent::Conversation(synonz::ConversationEvent::TopicShifted {
                from, to, ..
            }) => Some(format!("shifted:{from}->{to}")),
            SynonzEvent::Turn(synonz::TurnEvent::Model(synonz::ModelEvent::Requested {
                purpose,
                round,
                ..
            })) => Some(format!("model-requested:{purpose:?}:{round:?}")),
            _ => None,
        };
        if let Some(fact) = fact {
            self.facts.lock().unwrap().push(fact);
        }
    }
}

impl FactObserver {
    fn facts(&self) -> Vec<String> {
        self.facts.lock().unwrap().clone()
    }

    fn has(&self, prefix: &str) -> bool {
        self.facts().iter().any(|fact| fact.starts_with(prefix))
    }
}

/// A memory implementation with no entries (the minimal management face).
struct NoMemory;

impl Memory for NoMemory {
    fn list(
        &self,
        _subject: &Subject,
        _query: MemoryQuery,
    ) -> Result<MemoryPage<MemoryItem, MemoryListCursor>, MemoryStoreError> {
        Ok(MemoryPage::new(Vec::new(), None))
    }
    fn get(&self, _subject: &Subject, _id: &str) -> Result<Option<MemoryItem>, MemoryStoreError> {
        Ok(None)
    }
    fn edit(
        &self,
        _subject: &Subject,
        id: &str,
        _content: &str,
    ) -> Result<MemoryItem, MemoryStoreError> {
        Err(MemoryStoreError::EntryNotFound(id.to_string()))
    }
    fn forget(
        &self,
        _subject: &Subject,
        _id: &str,
    ) -> Result<MemoryForgetResult, MemoryStoreError> {
        Ok(MemoryForgetResult::new(0, Vec::new()))
    }
    fn forget_matching(
        &self,
        _subject: &Subject,
        _query: MemoryQuery,
    ) -> Result<MemoryForgetResult, MemoryStoreError> {
        Ok(MemoryForgetResult::new(0, Vec::new()))
    }
}

/// An assembler returning a fixed frame (or an empty one).
struct FixedAssembler {
    frame: Vec<Message>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl FixedAssembler {
    fn new(frame: Vec<Message>) -> Self {
        Self {
            frame,
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl MemoryContextAssembler for FixedAssembler {
    fn assemble<'a>(
        &'a self,
        input: MemoryContextAssembleInput<'a>,
    ) -> BoxFuture<'a, MemoryContextAssembleOutput> {
        self.seen.lock().unwrap().push(format!(
            "topic={} rewritten={:?}",
            input.topic, input.rewritten_input
        ));
        let frame = self.frame.clone();
        Box::pin(async move { MemoryContextAssembleOutput::new(frame, Vec::new()) })
    }
}

/// A pipeline recording its calls and the turn data it received.
#[derive(Default)]
struct RecordingPipeline {
    calls: Mutex<Vec<String>>,
}

impl MemoryPipeline for RecordingPipeline {
    fn archive_turn<'a>(
        &'a self,
        ctx: &'a PipelineTurnContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
        self.calls.lock().unwrap().push(format!(
            "archive:input={}:topic={}:change={:?}:responses={}",
            ctx.input(),
            ctx.topic(),
            ctx.topic_change(),
            ctx.responses().len()
        ));
        Box::pin(async { Ok(()) })
    }

    fn spawn_task<'a>(
        &'a self,
        ctx: &'a PipelineTurnContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
        self.calls.lock().unwrap().push("spawn".to_string());
        if ctx.topic_change().is_some() {
            ctx.spawn(async { Err(MemoryFailure::new("summary", "background boom")) });
        }
        Box::pin(async { Ok(()) })
    }

    fn finalize_conversation<'a>(
        &'a self,
        _ctx: &'a synonz::PipelineConversationContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
        self.calls.lock().unwrap().push("finalize".to_string());
        Box::pin(async { Ok(()) })
    }
}

/// A provider wrapping the given faces.
struct StubProvider {
    assembler: Arc<dyn MemoryContextAssembler>,
    pipeline: Arc<dyn MemoryPipeline>,
    model: Option<Arc<dyn Model>>,
}

impl MemoryProvider for StubProvider {
    fn memory(&self, _bus: EventBus) -> Arc<dyn Memory> {
        Arc::new(NoMemory)
    }
    fn context_assembler(&self) -> Arc<dyn MemoryContextAssembler> {
        Arc::clone(&self.assembler)
    }
    fn pipeline(&self) -> Arc<dyn MemoryPipeline> {
        Arc::clone(&self.pipeline)
    }
    fn model(&self) -> Option<Arc<dyn Model>> {
        self.model.clone()
    }
}

/// A rewriter transforming the input, or failing.
struct StubRewriter {
    fail: bool,
}

impl TurnInputRewriter for StubRewriter {
    fn rewrite<'a>(
        &'a self,
        input: RewriteInput<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, MemoryFailure>> {
        let fail = self.fail;
        let rewritten = format!("rewritten({})", input.input);
        Box::pin(async move {
            if fail {
                Err(MemoryFailure::new("rewrite", "rewriter down"))
            } else {
                Ok(Some(rewritten))
            }
        })
    }
}

struct StubRewriterProvider {
    rewriter: Arc<dyn TurnInputRewriter>,
    model: Option<Arc<dyn Model>>,
}

impl RewriterProvider for StubRewriterProvider {
    fn turn_input_rewriter(&self) -> Arc<dyn TurnInputRewriter> {
        Arc::clone(&self.rewriter)
    }
    fn model(&self) -> Option<Arc<dyn Model>> {
        self.model.clone()
    }
}

/// A rewriter recording the history it receives.
#[derive(Default)]
struct HistoryRewriter {
    histories: Arc<Mutex<Vec<Vec<Message>>>>,
}

impl TurnInputRewriter for HistoryRewriter {
    fn rewrite<'a>(
        &'a self,
        input: RewriteInput<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, MemoryFailure>> {
        self.histories.lock().unwrap().push(input.history.to_vec());
        Box::pin(async { Ok(None) })
    }
}

/// A detector with a scripted verdict per call.
struct StubDetector {
    verdicts: Mutex<Vec<Result<Option<String>, MemoryFailure>>>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl TopicDetector for StubDetector {
    fn detect<'a>(
        &'a self,
        input: TopicDetectInput<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, MemoryFailure>> {
        self.seen.lock().unwrap().push(format!(
            "input={} topic={:?} history={}",
            input.input,
            input.topic,
            input.history.len()
        ));
        let verdict = if self.verdicts.lock().unwrap().is_empty() {
            Ok(None)
        } else {
            self.verdicts.lock().unwrap().remove(0)
        };
        Box::pin(async move { verdict })
    }
}

struct StubDetectorProvider {
    detector: Arc<dyn TopicDetector>,
    model: Option<Arc<dyn Model>>,
}

impl TopicDetectorProvider for StubDetectorProvider {
    fn topic_detector(&self) -> Arc<dyn TopicDetector> {
        Arc::clone(&self.detector)
    }
    fn model(&self) -> Option<Arc<dyn Model>> {
        self.model.clone()
    }
}

/// A detector that makes a narrated model call and reports what it saw.
struct CallingDetector;

impl TopicDetector for CallingDetector {
    fn detect<'a>(
        &'a self,
        input: TopicDetectInput<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, MemoryFailure>> {
        Box::pin(async move {
            let request = ModelRequest::new(vec![Message::user("classify the topic")], Vec::new());
            match synonz::complete(&*input.model, request).await {
                Ok((message, _usage)) => Ok(Some(
                    message
                        .blocks
                        .iter()
                        .filter_map(|block| match block {
                            synonz::ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<String>(),
                )),
                Err(error) => Err(MemoryFailure::new("topic", error.to_string())),
            }
        })
    }
}

// ────────────────────────── helpers ──────────────────────────

fn subject() -> Subject {
    Subject::of(SubjectType::User, "u-memory")
}

/// Runs one turn and returns the conversation and the execution outcome.
async fn run_turn(
    conversation: &mut Conversation,
    agent: &Agent,
    text: &str,
) -> Result<synonz::AgentOutput, synonz::AgentError> {
    agent.run(conversation.turn_input(text)).await
}

fn text_of(message: &Message) -> Option<String> {
    let text: String = message
        .blocks
        .iter()
        .filter_map(|block| match block {
            synonz::ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    (!text.is_empty()).then_some(text)
}

// ────────────────────────── the bundled provider ──────────────────────────

#[tokio::test]
async fn the_bundled_provider_carries_the_conversation_across_turns() {
    let runtime = synonz::SynonzRuntime::builder().build();
    let model = MockModel::new(vec![
        vec![ModelStreamItem::Finish {
            message: Message::assistant_text("first answer"),
            usage: TokenUsage::new(1, 1),
        }],
        vec![ModelStreamItem::Finish {
            message: Message::assistant_text("second answer"),
            usage: TokenUsage::new(1, 1),
        }],
    ]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "first question")
        .await
        .unwrap();
    let output = run_turn(&mut conversation, &agent, "second question")
        .await
        .unwrap();
    assert_eq!(output.text(), Some("second answer"));

    // The second run's frame replayed the first turn: the previous user
    // message and the previous answer precede the current input.
    let turns = conversation.turns();
    assert_eq!(turns.len(), 2);
    let frame = &turns[1].messages;
    assert_eq!(text_of(&frame[0]).as_deref(), Some("first question"));
    assert_eq!(frame[0].role, Role::User);
    assert_eq!(text_of(&frame[1]).as_deref(), Some("first answer"));
    assert_eq!(frame[1].role, Role::Assistant);
    assert_eq!(text_of(&frame[2]).as_deref(), Some("second question"));
    assert_eq!(text_of(&frame[3]).as_deref(), Some("second answer"));
    assert_eq!(turns[1].input.text, "second question");
}

#[tokio::test]
async fn the_bundled_management_face_is_minimal() {
    let runtime = synonz::SynonzRuntime::builder().build();
    let memory = runtime.memory();
    let subject = subject();

    let page = memory.list(&subject, MemoryQuery::new(10)).unwrap();
    assert!(page.items.is_empty());
    assert!(page.next.is_none());
    assert!(memory.get(&subject, "anything").unwrap().is_none());
    assert_eq!(memory.forget(&subject, "anything").unwrap().removed, 0);
    assert_eq!(
        memory
            .forget_matching(&subject, MemoryQuery::new(10))
            .unwrap()
            .removed,
        0
    );
}

// ────────────────────────── the contract family ──────────────────────────

#[tokio::test]
async fn the_reader_is_a_read_only_projection_of_the_memory() {
    let runtime = synonz::SynonzRuntime::builder().build();
    let subject = subject();
    let memory = runtime.memory();

    // The projection derives from the memory itself and exposes the
    // item-level reads only (no write verbs exist on it).
    let reader = memory.reader();
    let page = reader.list(&subject, MemoryQuery::new(10)).unwrap();
    assert!(page.items.is_empty());
    assert!(reader.get(&subject, "missing").unwrap().is_none());
}

#[tokio::test]
async fn a_replaced_provider_drives_the_model_visible_frame() {
    let runtime = synonz::SynonzRuntime::builder()
        .memory_provider(StubProvider {
            assembler: Arc::new(FixedAssembler::new(vec![Message::user(
                "from the provider",
            )])),
            pipeline: Arc::new(RecordingPipeline::default()),
            model: None,
        })
        .build();
    let model = RecordingModel::new("answer");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "original input")
        .await
        .unwrap();

    let request = model.requests().first().cloned().expect("a model call");
    assert_eq!(request.messages.len(), 1);
    assert_eq!(request.messages[0].role, Role::User);
    assert_eq!(
        text_of(&request.messages[0]).as_deref(),
        Some("from the provider")
    );
}

#[tokio::test]
async fn an_empty_frame_falls_back_to_the_original_input_and_reports() {
    let facts = FactObserver::default();
    let runtime = synonz::SynonzRuntime::builder()
        .observer(facts.clone())
        .memory_provider(StubProvider {
            assembler: Arc::new(FixedAssembler::new(Vec::new())),
            pipeline: Arc::new(RecordingPipeline::default()),
            model: None,
        })
        .build();
    let model = RecordingModel::new("answer");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "kept input")
        .await
        .unwrap();

    let request = model.requests().first().cloned().expect("a model call");
    assert_eq!(text_of(&request.messages[0]).as_deref(), Some("kept input"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        facts.has("failed:assemble"),
        "the fallback must be visible: {:?}",
        facts.facts()
    );
}

#[tokio::test]
async fn the_turn_archives_the_frame_and_the_responses() {
    let runtime = synonz::SynonzRuntime::builder()
        .memory_provider(StubProvider {
            assembler: Arc::new(FixedAssembler::new(vec![Message::user("framed input")])),
            pipeline: Arc::new(RecordingPipeline::default()),
            model: None,
        })
        .build();
    let model = RecordingModel::new("the answer");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "original input")
        .await
        .unwrap();

    let turn = &conversation.turns()[0];
    assert_eq!(turn.input.text, "original input");
    assert_eq!(turn.messages.len(), 2);
    assert_eq!(text_of(&turn.messages[0]).as_deref(), Some("framed input"));
    assert_eq!(turn.messages[1].role, Role::Assistant);
    assert_eq!(text_of(&turn.messages[1]).as_deref(), Some("the answer"));
}

#[tokio::test]
async fn the_write_phase_receives_the_turn_data() {
    let pipeline = Arc::new(RecordingPipeline::default());
    let runtime = synonz::SynonzRuntime::builder()
        .memory_provider(StubProvider {
            assembler: Arc::new(FixedAssembler::new(vec![Message::user("framed")])),
            pipeline: Arc::clone(&pipeline) as Arc<dyn MemoryPipeline>,
            model: None,
        })
        .build();
    let model = RecordingModel::new("the answer");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "hello there")
        .await
        .unwrap();

    let calls = pipeline.calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|call| call == "archive:input=hello there:topic=:change=None:responses=1"),
        "the archive hook receives the turn's input and responses: {calls:?}"
    );
    assert!(calls.iter().any(|call| call == "spawn"), "{calls:?}");
}

// ────────────────────────── read-side preprocessing ──────────────────────────

#[tokio::test]
async fn the_rewriter_output_flows_into_the_frame() {
    let runtime = synonz::SynonzRuntime::builder()
        .memory_provider(StubProvider {
            assembler: Arc::new(RewriteEchoAssembler),
            pipeline: Arc::new(RecordingPipeline::default()),
            model: None,
        })
        .build();
    let model = RecordingModel::new("answer");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .rewriter_provider(StubRewriterProvider {
            rewriter: Arc::new(StubRewriter { fail: false }),
            model: None,
        })
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "hello").await.unwrap();

    let request = model.requests().first().cloned().expect("a model call");
    assert_eq!(
        text_of(&request.messages[0]).as_deref(),
        Some("rewritten(hello)"),
        "the model sees the rewritten input"
    );
}

#[tokio::test]
async fn a_rewriter_failure_degrades_visibly_and_keeps_the_original() {
    let facts = FactObserver::default();
    let runtime = synonz::SynonzRuntime::builder()
        .observer(facts.clone())
        .memory_provider(StubProvider {
            assembler: Arc::new(RewriteEchoAssembler),
            pipeline: Arc::new(RecordingPipeline::default()),
            model: None,
        })
        .build();
    let model = RecordingModel::new("answer");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .rewriter_provider(StubRewriterProvider {
            rewriter: Arc::new(StubRewriter { fail: true }),
            model: None,
        })
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "kept").await.unwrap();

    let request = model.requests().first().cloned().expect("a model call");
    assert_eq!(text_of(&request.messages[0]).as_deref(), Some("kept"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(facts.has("failed:rewrite"), "{:?}", facts.facts());
}

/// An assembler that echoes the model-view input into the frame.
struct RewriteEchoAssembler;

impl MemoryContextAssembler for RewriteEchoAssembler {
    fn assemble<'a>(
        &'a self,
        input: MemoryContextAssembleInput<'a>,
    ) -> BoxFuture<'a, MemoryContextAssembleOutput> {
        let user_text = input
            .rewritten_input
            .map(str::to_string)
            .unwrap_or_else(|| input.input.to_string());
        Box::pin(async move {
            MemoryContextAssembleOutput::new(vec![Message::user(user_text)], Vec::new())
        })
    }
}

#[tokio::test]
async fn the_rewriter_history_comes_from_the_truth_domain() {
    let rewriter = Arc::new(HistoryRewriter::default());
    let runtime = synonz::SynonzRuntime::builder().build();
    let model = MockModel::new(vec![
        vec![ModelStreamItem::Finish {
            message: Message::assistant_text("ok"),
            usage: TokenUsage::new(1, 1),
        }],
        vec![ModelStreamItem::Finish {
            message: Message::assistant_text("ok"),
            usage: TokenUsage::new(1, 1),
        }],
    ]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .rewriter_provider(StubRewriterProvider {
            rewriter: Arc::clone(&rewriter) as Arc<dyn TurnInputRewriter>,
            model: None,
        })
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "first").await.unwrap();
    run_turn(&mut conversation, &agent, "second").await.unwrap();

    let histories = rewriter.histories.lock().unwrap();
    assert_eq!(histories.len(), 2);
    assert!(histories[0].is_empty(), "the first turn has no history");
    let second = &histories[1];
    assert_eq!(second.len(), 2, "one successful turn: user + assistant");
    assert_eq!(text_of(&second[0]).as_deref(), Some("first"));
    assert_eq!(second[1].role, Role::Assistant);
}

// ────────────────────────── topic detection ──────────────────────────

#[tokio::test]
async fn topic_detection_writes_back_and_reaches_the_hooks() {
    let facts = FactObserver::default();
    let pipeline = Arc::new(RecordingPipeline::default());
    let detector = Arc::new(StubDetector {
        verdicts: Mutex::new(vec![
            Ok(Some("billing".into())),
            Ok(Some("shipping".into())),
        ]),
        seen: Arc::new(Mutex::new(Vec::new())),
    });
    let runtime = synonz::SynonzRuntime::builder()
        .observer(facts.clone())
        .memory_provider(StubProvider {
            assembler: Arc::new(FixedAssembler::new(vec![Message::user("x")])),
            pipeline: Arc::clone(&pipeline) as Arc<dyn MemoryPipeline>,
            model: None,
        })
        .build();
    let model = MockModel::new(vec![
        vec![ModelStreamItem::Finish {
            message: Message::assistant_text("a"),
            usage: TokenUsage::new(1, 1),
        }],
        vec![ModelStreamItem::Finish {
            message: Message::assistant_text("b"),
            usage: TokenUsage::new(1, 1),
        }],
    ]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .topic_detector_provider(StubDetectorProvider {
            detector: Arc::clone(&detector) as Arc<dyn TopicDetector>,
            model: None,
        })
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "invoice?")
        .await
        .unwrap();
    assert_eq!(conversation.topic().as_deref(), Some("billing"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !facts.has("shifted:"),
        "the first topic is an establishment, not a shift: {:?}",
        facts.facts()
    );

    run_turn(&mut conversation, &agent, "where is my parcel?")
        .await
        .unwrap();
    assert_eq!(conversation.topic().as_deref(), Some("shipping"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        facts.has("shifted:billing->shipping"),
        "{:?}",
        facts.facts()
    );

    // The hooks saw the turn's topic change, and the change drove the
    // background work.
    let calls = pipeline.calls.lock().unwrap();
    assert!(
        calls.iter().any(|call| call.contains("topic=shipping")
            && call.contains("Some((\"billing\", \"shipping\"))")),
        "{calls:?}"
    );
    assert!(
        facts.has("failed:summary"),
        "the spawned failure is visible: {:?}",
        facts.facts()
    );
}

#[tokio::test]
async fn a_failing_detector_keeps_the_topic_and_reports() {
    let facts = FactObserver::default();
    let runtime = synonz::SynonzRuntime::builder()
        .observer(facts.clone())
        .memory_provider(StubProvider {
            assembler: Arc::new(FixedAssembler::new(vec![Message::user("x")])),
            pipeline: Arc::new(RecordingPipeline::default()),
            model: None,
        })
        .build();
    let model = MockModel::new(vec![vec![ModelStreamItem::Finish {
        message: Message::assistant_text("a"),
        usage: TokenUsage::new(1, 1),
    }]]);
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .topic_detector_provider(StubDetectorProvider {
            detector: Arc::new(StubDetector {
                verdicts: Mutex::new(vec![Err(MemoryFailure::new("topic", "detector down"))]),
                seen: Arc::new(Mutex::new(Vec::new())),
            }),
            model: None,
        })
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());
    conversation.set_topic("kept");

    run_turn(&mut conversation, &agent, "hello").await.unwrap();

    assert_eq!(conversation.topic().as_deref(), Some("kept"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let facts = facts.facts();
    assert!(
        facts.iter().any(|f| f.starts_with("failed:topic")),
        "{facts:?}"
    );
    assert!(facts.iter().any(|f| f == "archived:kept"), "{facts:?}");
}

#[tokio::test]
async fn detector_model_calls_are_narrated_by_the_framework() {
    let facts = FactObserver::default();
    let runtime = synonz::SynonzRuntime::builder()
        .observer(facts.clone())
        .memory_provider(StubProvider {
            assembler: Arc::new(FixedAssembler::new(vec![Message::user("x")])),
            pipeline: Arc::new(RecordingPipeline::default()),
            model: None,
        })
        .build();
    let model = RecordingModel::new("detected topic");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .topic_detector_provider(StubDetectorProvider {
            detector: Arc::new(CallingDetector),
            model: None,
        })
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "hello").await.unwrap();
    assert_eq!(conversation.topic().as_deref(), Some("detected topic"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        facts.has("model-requested:ContextManagement:None"),
        "the auxiliary call is narrated with the context-management purpose: {:?}",
        facts.facts()
    );
}

#[tokio::test]
async fn the_provider_model_wins_over_the_agent_model() {
    // The detector declares its own model; the framework hands it the
    // provider's model (narrated), not the agent's.
    let provider_model = RecordingModel::new("provider verdict");
    let runtime = synonz::SynonzRuntime::builder()
        .memory_provider(StubProvider {
            assembler: Arc::new(FixedAssembler::new(vec![Message::user("x")])),
            pipeline: Arc::new(RecordingPipeline::default()),
            model: None,
        })
        .build();
    let agent_model = MockModel::finishing_with_text("agent answer");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(agent_model)
        .topic_detector_provider(StubDetectorProvider {
            detector: Arc::new(CallingDetector),
            model: Some(Arc::new(provider_model.clone()) as Arc<dyn Model>),
        })
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "hello").await.unwrap();

    assert_eq!(conversation.topic().as_deref(), Some("provider verdict"));
    assert_eq!(
        provider_model.requests().len(),
        1,
        "the detector's call went to the provider model"
    );
}

// ────────────────────────── pipeline failure semantics ──────────────────────────

#[tokio::test]
async fn a_failing_archive_is_reported_and_the_background_segment_still_runs() {
    struct FailingArchive;

    impl MemoryPipeline for FailingArchive {
        fn archive_turn<'a>(
            &'a self,
            _ctx: &'a PipelineTurnContext<'a>,
        ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
            Box::pin(async { Err(MemoryFailure::new("archive", "boom")) })
        }
        fn spawn_task<'a>(
            &'a self,
            _ctx: &'a PipelineTurnContext<'a>,
        ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
            Box::pin(async { Ok(()) })
        }
        fn finalize_conversation<'a>(
            &'a self,
            _ctx: &'a synonz::PipelineConversationContext<'a>,
        ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
            Box::pin(async { Ok(()) })
        }
    }

    let facts = FactObserver::default();
    let runtime = synonz::SynonzRuntime::builder()
        .observer(facts.clone())
        .memory_provider(StubProvider {
            assembler: Arc::new(FixedAssembler::new(vec![Message::user("x")])),
            pipeline: Arc::new(FailingArchive),
            model: None,
        })
        .build();
    let model = MockModel::finishing_with_text("answer");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .build()
        .unwrap();
    let mut conversation = Conversation::new(&runtime, &subject());

    run_turn(&mut conversation, &agent, "hello").await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    let facts = facts.facts();
    assert!(
        facts.iter().any(|f| f.starts_with("failed:archive")),
        "{facts:?}"
    );
    assert!(
        !facts.iter().any(|f| f.starts_with("archived:")),
        "{facts:?}"
    );
}

#[tokio::test]
async fn finalize_runs_at_conversation_end() {
    let pipeline = Arc::new(RecordingPipeline::default());
    let runtime = synonz::SynonzRuntime::builder()
        .memory_provider(StubProvider {
            assembler: Arc::new(FixedAssembler::new(vec![Message::user("x")])),
            pipeline: Arc::clone(&pipeline) as Arc<dyn MemoryPipeline>,
            model: None,
        })
        .build();
    let conversation = Conversation::new(&runtime, &subject());

    conversation.end(&runtime).await;

    assert!(
        pipeline
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "finalize"),
        "the finalize hook must run at conversation end"
    );
}
