//! The memory contract family: the read abstraction, the write pipeline,
//! the extension factories, and the crate-internal context engine.
//!
//! The framework owns the *mechanism* — when the read phase runs, when the
//! write phase runs, how model access is resolved and narrated, how
//! failures become facts — while the *semantics* belong to the provider
//! behind [`MemoryProvider`]:
//!
//! - [`MemoryContextAssembler`] (read): materializes the context state into
//!   the model-visible message frame of one run;
//! - [`MemoryPipeline`] (write): the three business hooks the framework's
//!   write-phase template calls — `archive_turn`, `spawn_task`,
//!   `finalize_conversation`;
//! - [`TurnInputRewriter`] (read-side preprocessing, Agent level) and
//!   [`TopicDetector`] (write-side preprocessing, Agent level): optional,
//!   independently replaceable extension points.
//!
//! Model access is uniform: implementations declare an optional model on
//! their factory ([`MemoryProvider`] / [`RewriterProvider`] /
//! [`TopicDetectorProvider`]); the framework resolves it at use time as
//! *provider model, falling back to the agent model*, and hands the
//! implementation a narrated handle (auxiliary calls are reported on the
//! bus; no stream deltas are emitted for them).

use std::future::Future;
use std::sync::Arc;

use futures::StreamExt;
use futures::future::BoxFuture;

use crate::bus::{ConversationEvent, EventBus, FactOutlet, MemoryEvent, MemoryFailedMoment};
use crate::conversation::Conversation;
use crate::error::ModelError;
use crate::event::TurnEvent;
use crate::event::{CallPurpose, ModelEvent};
use crate::memory::{Memory, MemoryReader};
use crate::message::Message;
use crate::model::{Model, ModelRequest, ModelStream, ModelStreamItem};
use crate::runtime::ConversationTaskSpawner;
use crate::subject::Subject;

/// How many recent successful turns the framework hands to the read-side
/// preprocessing materials (input rewrite, topic detection) as the
/// conversation history — the truth-domain window.
pub(crate) const HISTORY_TURNS: usize = 3;

// ── Failure representation ──

/// One memory-flow failure: the stage that failed and why.
///
/// Stages are implementation-defined strings; the framework does not
/// interpret them. Failures travel as [`MemoryFailure`] values (read
/// outputs, hook results) and surface as `Failed` memory facts — degraded,
/// never silent.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryFailure {
    /// Which stage failed (implementation-defined).
    pub stage: String,
    /// Human-readable detail of the failure.
    pub detail: String,
}

impl MemoryFailure {
    /// Creates a failure record.
    pub fn new(stage: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            detail: detail.into(),
        }
    }
}

// ── Read abstraction: context assembly ──

/// The read abstraction: how the context state materializes into the
/// model-visible message frame of one run.
///
/// The framework runs the assembler once per run, before the reasoning
/// loop, and places its frame directly after the agent's system message
/// (when one is configured). The returned frame is the **complete
/// model-visible message list after the system message** — including the
/// current turn's user message; the framework appends nothing.
///
/// Contract: an assembler must return a usable frame containing the
/// current turn's user message. When it cannot (a read failed), it degrades
/// by itself — placing the original input as the user message and reporting
/// the failure through [`MemoryContextAssembleOutput::failures`]. If the
/// frame comes back empty, the framework substitutes the original input and
/// reports a failure — the model never silently loses the input.
///
/// Role convention: system messages carry agent instructions only; memory
/// context and enhanced input belong on the user side.
pub trait MemoryContextAssembler: Send + Sync + 'static {
    /// Assembles the frame for one turn (fresh — the state is read at
    /// assembly time).
    fn assemble<'a>(
        &'a self,
        input: MemoryContextAssembleInput<'a>,
    ) -> BoxFuture<'a, MemoryContextAssembleOutput>;
}

/// The assembly payload: what the assembler consumes — the minimal
/// sufficient set. The material domain is open for growth
/// (`non_exhaustive`): future perception or runtime materials join as new
/// fields without breaking implementations.
#[non_exhaustive]
pub struct MemoryContextAssembleInput<'a> {
    /// The read-only projection of the memory implementation (the state's
    /// persistent part).
    pub reader: MemoryReader<'a>,
    /// The subject owning the memory.
    pub subject: &'a Subject,
    /// The conversation whose state is assembled.
    pub conversation_id: &'a str,
    /// The conversation's current topic.
    pub topic: &'a str,
    /// The turn's original user input text (the truth).
    pub input: &'a str,
    /// The model-view input the read-side preprocessing produced; `None` =
    /// no rewrite (use the original input).
    pub rewritten_input: Option<&'a str>,
    /// The narrated model handle for the assembler's own model calls (the
    /// context-management model, resolved by the framework).
    pub model: Arc<dyn Model>,
}

impl<'a> MemoryContextAssembleInput<'a> {
    /// Assembles the payload (the struct-literal route stays closed for
    /// field growth; the framework and tests construct through here).
    pub fn new(
        reader: MemoryReader<'a>,
        subject: &'a Subject,
        conversation_id: &'a str,
        topic: &'a str,
        input: &'a str,
        rewritten_input: Option<&'a str>,
        model: Arc<dyn Model>,
    ) -> Self {
        Self {
            reader,
            subject,
            conversation_id,
            topic,
            input,
            rewritten_input,
            model,
        }
    }
}

/// What an assembly produced: the model-visible frame plus the reads that
/// failed along the way (degraded but visible).
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct MemoryContextAssembleOutput {
    /// The complete message frame after the agent's system message (the
    /// current turn's user message included).
    pub messages: Vec<Message>,
    /// The failures observed while composing (the frame is degraded
    /// accordingly).
    pub failures: Vec<MemoryFailure>,
}

impl MemoryContextAssembleOutput {
    /// Creates an output from its parts.
    pub fn new(messages: Vec<Message>, failures: Vec<MemoryFailure>) -> Self {
        Self { messages, failures }
    }
}

// ── Read-side preprocessing: input rewrite ──

/// The read-side input preprocessing contract: rewrites the turn's user
/// input into the model view used for recall and assembly (for example
/// coreference resolution against the recent conversation).
///
/// Runs before assembly; the result flows into
/// [`MemoryContextAssembleInput::rewritten_input`]. `Ok(None)` = keep the
/// original; `Err` = visible degradation (the original stands and the
/// failure is reported through the assembly output).
pub trait TurnInputRewriter: Send + Sync + 'static {
    /// Produces the model-view input.
    fn rewrite<'a>(
        &'a self,
        input: RewriteInput<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, MemoryFailure>>;
}

/// The rewrite payload: the turn's original input, the conversation
/// history (recent successful turns, the truth domain), and the narrated
/// model handle.
#[non_exhaustive]
pub struct RewriteInput<'a> {
    /// The turn's original user input text.
    pub input: &'a str,
    /// The recent successful turns' messages, oldest first.
    pub history: &'a [Message],
    /// The narrated model handle for the rewriter's own model calls.
    pub model: Arc<dyn Model>,
}

impl<'a> RewriteInput<'a> {
    /// Assembles the payload.
    pub fn new(input: &'a str, history: &'a [Message], model: Arc<dyn Model>) -> Self {
        Self {
            input,
            history,
            model,
        }
    }
}

// ── Write-side preprocessing: topic detection ──

/// The write-side topic detection contract (Agent level): decides the
/// conversation's topic at the end of a turn.
///
/// The framework calls the detector at the end of every completed turn,
/// before the write-phase hooks; it writes the verdict back to the
/// conversation, emits a shift fact when the topic changed, and exposes
/// the change to the hooks through their context. `Ok(None)` = keep the
/// current topic; `Err` = keep the current topic and report a failure.
pub trait TopicDetector: Send + Sync + 'static {
    /// Detects the conversation's topic for this turn.
    fn detect<'a>(
        &'a self,
        input: TopicDetectInput<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, MemoryFailure>>;
}

/// The topic detection payload: the turn's input, the conversation history
/// (recent successful turns), the current topic, and the narrated model
/// handle.
#[non_exhaustive]
pub struct TopicDetectInput<'a> {
    /// The turn's original user input text.
    pub input: &'a str,
    /// The recent successful turns' messages, oldest first.
    pub history: &'a [Message],
    /// The conversation's current topic (`None` when none is established).
    pub topic: Option<&'a str>,
    /// The narrated model handle for the detector's own model calls.
    pub model: Arc<dyn Model>,
}

impl<'a> TopicDetectInput<'a> {
    /// Assembles the payload.
    pub fn new(
        input: &'a str,
        history: &'a [Message],
        topic: Option<&'a str>,
        model: Arc<dyn Model>,
    ) -> Self {
        Self {
            input,
            history,
            topic,
            model,
        }
    }
}

// ── Write abstraction: the memory pipeline ──

/// The write abstraction: the business hooks the framework's write-phase
/// template calls.
///
/// The template is framework-owned: it detects the topic (through the
/// agent's [`TopicDetector`]), writes it back to the conversation, emits
/// the lifecycle facts (`TurnArchived`, `TopicShifted`), derives the
/// background segment, and drains the conversation's tasks before
/// finalization. Implementations describe what happened in business terms
/// through the three hooks; they write only through the context's semantic
/// entries ([`PipelineTurnContext::spawn`], `report_failure`, `set_topic`).
///
/// Failure semantics are best-effort: each hook runs independently; a hook
/// returning `Err` surfaces as a `Failed` fact and the template continues
/// with the remaining steps (an archive failure still derives the
/// background segment).
pub trait MemoryPipeline: Send + Sync + 'static {
    /// Archives the completed turn (the synchronous segment, before the
    /// terminal event).
    fn archive_turn<'a>(
        &'a self,
        ctx: &'a PipelineTurnContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>>;

    /// Derives the turn's background maintenance work (registered through
    /// [`PipelineTurnContext::spawn`]; the runtime drains it at conversation
    /// end).
    fn spawn_task<'a>(
        &'a self,
        ctx: &'a PipelineTurnContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>>;

    /// Finalizes the conversation (after the framework drained its
    /// background tasks).
    fn finalize_conversation<'a>(
        &'a self,
        ctx: &'a PipelineConversationContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>>;
}

/// The turn-level context the pipeline hooks receive: data reads (including
/// the turn's topic change) plus the semantic entries the framework
/// supports (`set_topic`, `spawn`, `report_failure`) and the narrated model
/// handle for the hooks' own model calls.
pub struct PipelineTurnContext<'a> {
    conversation: &'a Conversation,
    topic: &'a str,
    topic_change: Option<(&'a str, &'a str)>,
    input: &'a str,
    frame: &'a [Message],
    responses: &'a [Message],
    reader: MemoryReader<'a>,
    model: Arc<dyn Model>,
    spawner: ConversationTaskSpawner,
    outlet: FactOutlet,
}

impl<'a> PipelineTurnContext<'a> {
    /// Builds the context (framework-internal: the write-phase template is
    /// the sole construction site).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        conversation: &'a Conversation,
        topic: &'a str,
        topic_change: Option<(&'a str, &'a str)>,
        input: &'a str,
        frame: &'a [Message],
        responses: &'a [Message],
        reader: MemoryReader<'a>,
        model: Arc<dyn Model>,
        spawner: ConversationTaskSpawner,
        outlet: FactOutlet,
    ) -> Self {
        Self {
            conversation,
            topic,
            topic_change,
            input,
            frame,
            responses,
            reader,
            model,
            spawner,
            outlet,
        }
    }

    /// The conversation's owning subject.
    pub fn subject(&self) -> &'a Subject {
        self.conversation.subject()
    }

    /// The conversation's identity.
    pub fn conversation_id(&self) -> &'a str {
        self.conversation.id()
    }

    /// The conversation's topic (after this turn's detection).
    pub fn topic(&self) -> &'a str {
        self.topic
    }

    /// This turn's topic change (`from`, `to`), when the topic shifted.
    /// The first topic's establishment is not a shift.
    pub fn topic_change(&self) -> Option<(&'a str, &'a str)> {
        self.topic_change
    }

    /// The turn's original user input text.
    pub fn input(&self) -> &'a str {
        self.input
    }

    /// The complete model-visible frame the turn started from (the current
    /// turn's user message included; assembled by the read phase).
    pub fn frame(&self) -> &'a [Message] {
        self.frame
    }

    /// The turn's subsequent messages: the model's responses and the tool
    /// round-trips produced during this turn (the messages after the
    /// frame).
    pub fn responses(&self) -> &'a [Message] {
        self.responses
    }

    /// The read-only projection of the memory implementation.
    pub fn reader(&self) -> MemoryReader<'a> {
        self.reader
    }

    /// The narrated model handle for this hook's own model calls.
    pub fn model(&self) -> &Arc<dyn Model> {
        &self.model
    }

    /// Writes a topic back to the conversation.
    pub fn set_topic(&self, topic: &str) {
        self.conversation.set_topic(topic);
    }

    /// Registers background maintenance work for this conversation: the
    /// runtime schedules it and drains it at conversation end. A task
    /// returning `Err` surfaces as a `Failed` fact with the background
    /// moment.
    pub fn spawn(&self, task: impl Future<Output = Result<(), MemoryFailure>> + Send + 'static) {
        spawn_failing(self.spawner.clone(), self.outlet.clone(), task);
    }

    /// Reports a failure as a `Failed` fact (never silent) without
    /// interrupting the template.
    pub fn report_failure(&self, failure: MemoryFailure) {
        self.outlet.emit_memory(MemoryEvent::Failed {
            stage: failure.stage,
            detail: failure.detail,
            moment: MemoryFailedMoment::AfterTurn,
        });
    }
}

/// The conversation-level context the finalize hook receives: data reads
/// plus the semantic entries (`set_topic`, `spawn`, `report_failure`) and
/// the narrated model handle.
pub struct PipelineConversationContext<'a> {
    conversation: &'a Conversation,
    topic: &'a str,
    reader: MemoryReader<'a>,
    model: Option<Arc<dyn Model>>,
    spawner: ConversationTaskSpawner,
    outlet: FactOutlet,
}

impl<'a> PipelineConversationContext<'a> {
    /// Builds the context (framework-internal: the runtime's teardown is
    /// the sole construction site).
    pub(crate) fn new(
        conversation: &'a Conversation,
        topic: &'a str,
        reader: MemoryReader<'a>,
        model: Option<Arc<dyn Model>>,
        spawner: ConversationTaskSpawner,
        outlet: FactOutlet,
    ) -> Self {
        Self {
            conversation,
            topic,
            reader,
            model,
            spawner,
            outlet,
        }
    }

    /// The conversation's owning subject.
    pub fn subject(&self) -> &'a Subject {
        self.conversation.subject()
    }

    /// The conversation's identity.
    pub fn conversation_id(&self) -> &'a str {
        self.conversation.id()
    }

    /// The conversation's final topic.
    pub fn topic(&self) -> &'a str {
        self.topic
    }

    /// The read-only projection of the memory implementation.
    pub fn reader(&self) -> MemoryReader<'a> {
        self.reader
    }

    /// The narrated model handle for the hook's own model calls.
    ///
    /// At conversation end no agent is in scope, so the handle is the
    /// memory provider's configured model — `None` when the provider
    /// configured none.
    pub fn model(&self) -> Option<&Arc<dyn Model>> {
        self.model.as_ref()
    }

    /// Writes a final topic back to the conversation.
    pub fn set_topic(&self, topic: &str) {
        self.conversation.set_topic(topic);
    }

    /// Registers background work for this conversation (the runtime drains
    /// it before finalization completes).
    pub fn spawn(&self, task: impl Future<Output = Result<(), MemoryFailure>> + Send + 'static) {
        spawn_failing(self.spawner.clone(), self.outlet.clone(), task);
    }

    /// Reports a failure as a `Failed` fact (never silent).
    pub fn report_failure(&self, failure: MemoryFailure) {
        self.outlet.emit_memory(MemoryEvent::Failed {
            stage: failure.stage,
            detail: failure.detail,
            moment: MemoryFailedMoment::AtConversationEnd,
        });
    }
}

/// Wraps a background task so a failure surfaces as a `Failed` fact with
/// the background moment.
fn spawn_failing(
    spawner: ConversationTaskSpawner,
    outlet: FactOutlet,
    task: impl Future<Output = Result<(), MemoryFailure>> + Send + 'static,
) {
    spawner.spawn(async move {
        if let Err(failure) = task.await {
            outlet.emit_memory(MemoryEvent::Failed {
                stage: failure.stage,
                detail: failure.detail,
                moment: MemoryFailedMoment::Background,
            });
        }
    });
}

// ── Factories ──

/// The memory capability factory: the single registration point for a
/// memory implementation's three faces plus its optional context-management
/// model.
pub trait MemoryProvider: Send + Sync + 'static {
    /// The item-level management face, bound to the runtime's fact bus
    /// (management actions are reported through it).
    fn memory(&self, bus: EventBus) -> Arc<dyn Memory>;

    /// The read face: context assembly.
    fn context_assembler(&self) -> Arc<dyn MemoryContextAssembler>;

    /// The write face: the pipeline hooks.
    fn pipeline(&self) -> Arc<dyn MemoryPipeline>;

    /// The optional context-management model; resolved at use time as
    /// *provider model, falling back to the agent model*.
    fn model(&self) -> Option<Arc<dyn Model>> {
        None
    }
}

/// The read-side preprocessing factory (Agent level).
pub trait RewriterProvider: Send + Sync + 'static {
    /// The input rewriter.
    fn turn_input_rewriter(&self) -> Arc<dyn TurnInputRewriter>;

    /// The optional rewriter model; resolved at use time as *provider
    /// model, falling back to the agent model*.
    fn model(&self) -> Option<Arc<dyn Model>> {
        None
    }
}

/// The topic detection factory (Agent level).
pub trait TopicDetectorProvider: Send + Sync + 'static {
    /// The topic detector.
    fn topic_detector(&self) -> Arc<dyn TopicDetector>;

    /// The optional detector model; resolved at use time as *provider
    /// model, falling back to the agent model*.
    fn model(&self) -> Option<Arc<dyn Model>> {
        None
    }
}

// ── The crate-internal engine ──

/// The context engine: the framework-internal orchestration of the read
/// phase (input rewrite → assembly) and the write phase (topic detection →
/// archive → background segment).
///
/// The engine is pure behavior: it holds the runtime's memory capability,
/// the agent's optional read-side extensions, and the model resolution
/// rules — no instance state, no storage. Every call receives its data
/// through the payload.
pub(crate) struct Context {
    memory: Arc<dyn Memory>,
    assembler: Arc<dyn MemoryContextAssembler>,
    pipeline: Arc<dyn MemoryPipeline>,
    memory_model: Option<Arc<dyn Model>>,
    rewriter: Option<Arc<dyn TurnInputRewriter>>,
    rewriter_model: Option<Arc<dyn Model>>,
    detector: Option<Arc<dyn TopicDetector>>,
    detector_model: Option<Arc<dyn Model>>,
}

impl Context {
    /// Assembles the engine from the runtime's memory capability and the
    /// agent's optional read-side extensions (framework-internal; the
    /// agent's build is the sole construction site).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        memory: Arc<dyn Memory>,
        assembler: Arc<dyn MemoryContextAssembler>,
        pipeline: Arc<dyn MemoryPipeline>,
        memory_model: Option<Arc<dyn Model>>,
        rewriter: Option<Arc<dyn TurnInputRewriter>>,
        rewriter_model: Option<Arc<dyn Model>>,
        detector: Option<Arc<dyn TopicDetector>>,
        detector_model: Option<Arc<dyn Model>>,
    ) -> Self {
        Self {
            memory,
            assembler,
            pipeline,
            memory_model,
            rewriter,
            rewriter_model,
            detector,
            detector_model,
        }
    }

    /// Materializes the state (framework-internal): the turn input is
    /// preprocessed into its model view (when a rewriter is configured),
    /// then the assembler composes the model-visible frame. Called once per
    /// run, before the reasoning loop.
    ///
    /// Failures degrade visibly: they are reported as `Failed` facts and
    /// returned with the frame. An empty frame is substituted with the
    /// original input — the model never silently loses the input.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn assemble<'a>(
        &'a self,
        subject: &'a Subject,
        conversation_id: &'a str,
        topic: &'a str,
        input: &'a str,
        history: &'a [Message],
        agent_model: &Arc<dyn Model>,
        outlet: &FactOutlet,
    ) -> MemoryContextAssembleOutput {
        let mut failures = Vec::new();

        // Read-side preprocessing: resolve and narrate the model, run the
        // rewriter, degrade visibly on failure.
        let rewritten = match &self.rewriter {
            None => None,
            Some(rewriter) => {
                let model = narrate_model(
                    self.rewriter_model
                        .clone()
                        .unwrap_or_else(|| Arc::clone(agent_model)),
                    outlet,
                );
                match rewriter
                    .rewrite(RewriteInput::new(input, history, model))
                    .await
                {
                    Ok(text) => text,
                    Err(failure) => {
                        failures.push(failure);
                        None
                    }
                }
            }
        };

        let model = narrate_model(
            self.memory_model
                .clone()
                .unwrap_or_else(|| Arc::clone(agent_model)),
            outlet,
        );
        let assembled = self
            .assembler
            .assemble(MemoryContextAssembleInput::new(
                self.memory.reader(),
                subject,
                conversation_id,
                topic,
                input,
                rewritten.as_deref(),
                model,
            ))
            .await;

        let MemoryContextAssembleOutput {
            mut messages,
            failures: read_failures,
        } = assembled;
        failures.extend(read_failures);
        if messages.is_empty() {
            messages.push(Message::user(input));
            failures.push(MemoryFailure::new(
                "assemble",
                "the assembler returned an empty frame; the original input was used",
            ));
        }

        for failure in &failures {
            outlet.emit_memory(MemoryEvent::Failed {
                stage: failure.stage.clone(),
                detail: failure.detail.clone(),
                moment: MemoryFailedMoment::AfterTurn,
            });
        }
        MemoryContextAssembleOutput::new(messages, failures)
    }

    /// Maintains the state on turn completion (framework-internal): detect
    /// the topic, then run the write-phase template (archive → background
    /// segment). Called after a completed turn, before the terminal event.
    ///
    /// Failed and cancelled turns never reach this method — the truth
    /// archive takes them, the memory stays unpolluted.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn on_turn_completed<'a>(
        &'a self,
        conversation: &'a Conversation,
        input: &'a str,
        frame: &'a [Message],
        responses: &'a [Message],
        history: &'a [Message],
        agent_model: &Arc<dyn Model>,
        outlet: &FactOutlet,
        spawner: ConversationTaskSpawner,
    ) {
        let subject = conversation.subject();
        let conversation_id = conversation.id();
        let previous = conversation.topic().unwrap_or_default();

        // 1. Topic detection (synchronous segment): the verdict is written
        //    back, a change emits the shift fact, and the change travels to
        //    the hooks through their context. A failure keeps the previous
        //    topic and is reported.
        let mut topic = previous.clone();
        let mut topic_change: Option<(String, String)> = None;
        if let Some(detector) = &self.detector {
            let model = narrate_model(
                self.detector_model
                    .clone()
                    .unwrap_or_else(|| Arc::clone(agent_model)),
                outlet,
            );
            let detected = detector
                .detect(TopicDetectInput::new(
                    input,
                    history,
                    conversation.topic().as_deref(),
                    model,
                ))
                .await;
            match detected {
                Ok(Some(next)) if next != previous => {
                    topic = next.clone();
                    conversation.set_topic(&next);
                    if previous.is_empty() {
                        // The first topic is an establishment, not a shift.
                    } else {
                        topic_change = Some((previous.clone(), next.clone()));
                        outlet.emit_conversation(ConversationEvent::TopicShifted {
                            conversation_id: conversation_id.to_string(),
                            from: previous.clone(),
                            to: next,
                        });
                    }
                }
                Ok(_) => {}
                Err(failure) => {
                    outlet.emit_memory(MemoryEvent::Failed {
                        stage: failure.stage,
                        detail: failure.detail,
                        moment: MemoryFailedMoment::AfterTurn,
                    });
                }
            }
        }

        // 2. The write-phase template: archive (the synchronous segment),
        //    then the background segment. Each step is independent; a
        //    failure is reported and the template continues.
        let model = narrate_model(
            self.memory_model
                .clone()
                .unwrap_or_else(|| Arc::clone(agent_model)),
            outlet,
        );
        let turn_ctx = PipelineTurnContext::new(
            conversation,
            &topic,
            topic_change
                .as_ref()
                .map(|(from, to)| (from.as_str(), to.as_str())),
            input,
            frame,
            responses,
            self.memory.reader(),
            model,
            spawner,
            outlet.clone(),
        );

        match self.pipeline.archive_turn(&turn_ctx).await {
            Ok(()) => outlet.emit_memory(MemoryEvent::TurnArchived {
                conversation_id: conversation_id.to_string(),
                subject_id: subject.to_string(),
                topic: topic.clone(),
            }),
            Err(failure) => outlet.emit_memory(MemoryEvent::Failed {
                stage: failure.stage,
                detail: failure.detail,
                moment: MemoryFailedMoment::AfterTurn,
            }),
        }

        if let Err(failure) = self.pipeline.spawn_task(&turn_ctx).await {
            outlet.emit_memory(MemoryEvent::Failed {
                stage: failure.stage,
                detail: failure.detail,
                moment: MemoryFailedMoment::AfterTurn,
            });
        }
    }
}

// ── Model narration ──

/// Wraps a model so every auxiliary call the framework issues on an
/// implementation's behalf is narrated: `Requested` before the call and
/// `Responded` on its finish item (bus and delivery), never a
/// `StreamDelta` — auxiliary calls consume the final result, there is no
/// product stream to narrate.
pub(crate) fn narrate_model(inner: Arc<dyn Model>, outlet: &FactOutlet) -> Arc<dyn Model> {
    Arc::new(NarratedModel {
        inner,
        outlet: outlet.clone(),
    })
}

struct NarratedModel {
    inner: Arc<dyn Model>,
    outlet: FactOutlet,
}

impl Model for NarratedModel {
    fn stream(&self, request: ModelRequest) -> BoxFuture<'_, Result<ModelStream, ModelError>> {
        Box::pin(async move {
            self.outlet
                .emit_turn(TurnEvent::Model(ModelEvent::Requested {
                    purpose: CallPurpose::ContextManagement,
                    messages: request.messages.clone(),
                    round: None,
                }));
            let inner = self.inner.stream(request).await?;
            let outlet = self.outlet.clone();
            let stream = futures::stream::unfold(Some(inner), move |state| {
                let outlet = outlet.clone();
                async move {
                    let mut inner = state?;
                    match inner.next().await {
                        Some(ModelStreamItem::Finish { message, usage }) => {
                            outlet.emit_turn(TurnEvent::Model(ModelEvent::Responded {
                                message: message.clone(),
                                usage,
                                round: None,
                            }));
                            Some((ModelStreamItem::Finish { message, usage }, None))
                        }
                        Some(item) => Some((item, Some(inner))),
                        None => None,
                    }
                }
            });
            Ok(stream.boxed())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{EventBus, MemoryEvent, Observer, ObserverContext, SynonzEvent};
    use crate::memory::{
        MemoryForgetResult, MemoryItem, MemoryListCursor, MemoryPage, MemoryQuery, MemoryScope,
        MemoryStoreError,
    };
    use crate::message::Role;
    use crate::subject::SubjectType;
    use std::sync::Mutex;

    fn subject() -> Subject {
        Subject::of(SubjectType::User, "u-engine")
    }

    #[derive(Default, Clone)]
    struct FactLog {
        facts: Arc<Mutex<Vec<String>>>,
    }

    impl Observer for FactLog {
        fn on_event(&self, _ctx: &ObserverContext, event: &SynonzEvent) {
            let entry = match event {
                SynonzEvent::Memory(MemoryEvent::TurnArchived { topic, .. }) => {
                    Some(format!("archived:{topic}"))
                }
                SynonzEvent::Memory(MemoryEvent::Failed { stage, .. }) => {
                    Some(format!("failed:{stage}"))
                }
                SynonzEvent::Conversation(ConversationEvent::TopicShifted { from, to, .. }) => {
                    Some(format!("shifted:{from}->{to}"))
                }
                _ => None,
            };
            if let Some(entry) = entry {
                self.facts.lock().unwrap().push(entry);
            }
        }
    }

    struct EmptyMemory;

    impl Memory for EmptyMemory {
        fn list(
            &self,
            _subject: &Subject,
            _query: MemoryQuery,
        ) -> Result<MemoryPage<MemoryItem, MemoryListCursor>, MemoryStoreError> {
            Ok(MemoryPage::new(Vec::new(), None))
        }
        fn get(
            &self,
            _subject: &Subject,
            _id: &str,
        ) -> Result<Option<MemoryItem>, MemoryStoreError> {
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
            Ok(MemoryForgetResult {
                removed: 0,
                failures: Vec::new(),
            })
        }
        fn forget_matching(
            &self,
            _subject: &Subject,
            _query: MemoryQuery,
        ) -> Result<MemoryForgetResult, MemoryStoreError> {
            Ok(MemoryForgetResult {
                removed: 0,
                failures: Vec::new(),
            })
        }
    }

    /// An assembler that returns the given frame (or an empty one).
    struct StubAssembler {
        frame: Vec<Message>,
    }

    impl MemoryContextAssembler for StubAssembler {
        fn assemble<'a>(
            &'a self,
            _input: MemoryContextAssembleInput<'a>,
        ) -> BoxFuture<'a, MemoryContextAssembleOutput> {
            let frame = self.frame.clone();
            Box::pin(async move { MemoryContextAssembleOutput::new(frame, Vec::new()) })
        }
    }

    /// A pipeline recording its calls; optionally failing archive.
    #[derive(Default)]
    struct StubPipeline {
        calls: Mutex<Vec<&'static str>>,
        fail_archive: bool,
    }

    impl MemoryPipeline for StubPipeline {
        fn archive_turn<'a>(
            &'a self,
            _ctx: &'a PipelineTurnContext<'a>,
        ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
            self.calls.lock().unwrap().push("archive");
            let fail = self.fail_archive;
            Box::pin(async move {
                if fail {
                    Err(MemoryFailure::new("archive", "boom"))
                } else {
                    Ok(())
                }
            })
        }
        fn spawn_task<'a>(
            &'a self,
            _ctx: &'a PipelineTurnContext<'a>,
        ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
            self.calls.lock().unwrap().push("spawn");
            Box::pin(async { Ok(()) })
        }
        fn finalize_conversation<'a>(
            &'a self,
            _ctx: &'a PipelineConversationContext<'a>,
        ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
            self.calls.lock().unwrap().push("finalize");
            Box::pin(async { Ok(()) })
        }
    }

    fn engine(
        assembler: StubAssembler,
        pipeline: Arc<StubPipeline>,
        detector: Option<Arc<dyn TopicDetector>>,
    ) -> Context {
        Context::new(
            Arc::new(EmptyMemory),
            Arc::new(assembler),
            pipeline,
            None,
            None,
            None,
            detector,
            None,
        )
    }

    fn model() -> Arc<dyn Model> {
        struct StubModel;

        impl Model for StubModel {
            fn stream(
                &self,
                _request: ModelRequest,
            ) -> BoxFuture<'_, Result<ModelStream, ModelError>> {
                Box::pin(async {
                    Ok(futures::stream::iter(vec![ModelStreamItem::Finish {
                        message: Message::assistant_text("ok"),
                        usage: crate::event::TokenUsage::new(1, 1),
                    }])
                    .boxed())
                })
            }
        }

        Arc::new(StubModel)
    }

    #[tokio::test]
    async fn empty_frame_falls_back_to_the_original_input_and_reports() {
        let log = FactLog::default();
        let bus = EventBus::new(vec![Arc::new(log.clone())]);
        let outlet = FactOutlet::bus_only(&bus);
        let engine = engine(StubAssembler { frame: vec![] }, Arc::default(), None);
        let subject = subject();
        let output = engine
            .assemble(&subject, "c1", "", "hello", &[], &model(), &outlet)
            .await;
        assert_eq!(output.messages.len(), 1);
        assert_eq!(output.messages[0].role, Role::User);
        assert_eq!(output.failures.len(), 1);
        assert!(output.failures[0].detail.contains("empty frame"));
        bus.flush().await;
        assert!(
            log.facts
                .lock()
                .unwrap()
                .iter()
                .any(|f| f.starts_with("failed:assemble")),
            "the fallback must be visible"
        );
    }

    #[tokio::test]
    async fn a_non_empty_frame_is_passed_through_unchanged() {
        let log = FactLog::default();
        let bus = EventBus::new(vec![Arc::new(log.clone())]);
        let outlet = FactOutlet::bus_only(&bus);
        let engine = engine(
            StubAssembler {
                frame: vec![Message::user("rewritten")],
            },
            Arc::default(),
            None,
        );
        let subject = subject();
        let output = engine
            .assemble(&subject, "c1", "", "hello", &[], &model(), &outlet)
            .await;
        assert_eq!(output.messages, vec![Message::user("rewritten")]);
        assert!(output.failures.is_empty());
    }

    #[tokio::test]
    async fn the_write_template_archives_and_derives_background_work() {
        let log = FactLog::default();
        let bus = EventBus::new(vec![Arc::new(log.clone())]);
        let outlet = FactOutlet::bus_only(&bus);
        let pipeline = Arc::new(StubPipeline::default());
        let engine = engine(StubAssembler { frame: vec![] }, Arc::clone(&pipeline), None);
        let subject = subject();
        let runtime = crate::SynonzRuntime::builder().build();
        let conversation = crate::Conversation::with_id(&runtime, &subject, "c-engine");
        conversation.set_topic("billing");
        let spawner = ConversationTaskSpawner::new(&runtime, conversation.id());

        engine
            .on_turn_completed(
                &conversation,
                "invoice?",
                &[Message::user("invoice?")],
                &[],
                &[],
                &model(),
                &outlet,
                spawner,
            )
            .await;

        assert_eq!(
            pipeline.calls.lock().unwrap().as_slice(),
            ["archive", "spawn"]
        );
        bus.flush().await;
        let facts = log.facts.lock().unwrap();
        assert!(facts.iter().any(|f| f == "archived:billing"), "{facts:?}");
    }

    #[tokio::test]
    async fn a_failing_archive_is_reported_and_the_background_segment_still_runs() {
        let log = FactLog::default();
        let bus = EventBus::new(vec![Arc::new(log.clone())]);
        let outlet = FactOutlet::bus_only(&bus);
        let pipeline = Arc::new(StubPipeline {
            fail_archive: true,
            ..Default::default()
        });
        let engine = engine(StubAssembler { frame: vec![] }, Arc::clone(&pipeline), None);
        let subject = subject();
        let runtime = crate::SynonzRuntime::builder().build();
        let conversation = crate::Conversation::with_id(&runtime, &subject, "c-fail");
        let spawner = ConversationTaskSpawner::new(&runtime, conversation.id());

        engine
            .on_turn_completed(
                &conversation,
                "hi",
                &[Message::user("hi")],
                &[],
                &[],
                &model(),
                &outlet,
                spawner,
            )
            .await;

        assert_eq!(
            pipeline.calls.lock().unwrap().as_slice(),
            ["archive", "spawn"]
        );
        bus.flush().await;
        let facts = log.facts.lock().unwrap();
        assert!(facts.iter().any(|f| f == "failed:archive"), "{facts:?}");
        assert!(
            !facts.iter().any(|f| f.starts_with("archived")),
            "{facts:?}"
        );
    }

    /// A detector returning a fixed verdict.
    struct StubDetector {
        verdict: Result<Option<String>, MemoryFailure>,
    }

    impl TopicDetector for StubDetector {
        fn detect<'a>(
            &'a self,
            _input: TopicDetectInput<'a>,
        ) -> BoxFuture<'a, Result<Option<String>, MemoryFailure>> {
            let verdict = self.verdict.clone();
            Box::pin(async move { verdict })
        }
    }

    #[tokio::test]
    async fn a_topic_shift_is_written_back_and_visible_to_the_hooks() {
        let log = FactLog::default();
        let bus = EventBus::new(vec![Arc::new(log.clone())]);
        let outlet = FactOutlet::bus_only(&bus);
        let runtime = crate::SynonzRuntime::builder().build();
        let subject = subject();
        let conversation = crate::Conversation::with_id(&runtime, &subject, "c-shift");
        conversation.set_topic("billing");

        let engine = engine(
            StubAssembler { frame: vec![] },
            Arc::new(StubPipeline::default()),
            Some(Arc::new(StubDetector {
                verdict: Ok(Some("shipping".into())),
            })),
        );
        engine
            .on_turn_completed(
                &conversation,
                "where is my parcel?",
                &[Message::user("where is my parcel?")],
                &[],
                &[],
                &model(),
                &outlet,
                ConversationTaskSpawner::new(&runtime, conversation.id()),
            )
            .await;

        assert_eq!(conversation.topic().as_deref(), Some("shipping"));
        bus.flush().await;
        let facts = log.facts.lock().unwrap();
        assert!(
            facts.iter().any(|f| f == "shifted:billing->shipping"),
            "{facts:?}"
        );
    }

    #[tokio::test]
    async fn the_first_topic_is_an_establishment_not_a_shift() {
        let log = FactLog::default();
        let bus = EventBus::new(vec![Arc::new(log.clone())]);
        let outlet = FactOutlet::bus_only(&bus);
        let runtime = crate::SynonzRuntime::builder().build();
        let subject = subject();
        let conversation = crate::Conversation::with_id(&runtime, &subject, "c-first");

        let engine = engine(
            StubAssembler { frame: vec![] },
            Arc::new(StubPipeline::default()),
            Some(Arc::new(StubDetector {
                verdict: Ok(Some("billing".into())),
            })),
        );
        engine
            .on_turn_completed(
                &conversation,
                "invoice?",
                &[Message::user("invoice?")],
                &[],
                &[],
                &model(),
                &outlet,
                ConversationTaskSpawner::new(&runtime, conversation.id()),
            )
            .await;

        assert_eq!(conversation.topic().as_deref(), Some("billing"));
        bus.flush().await;
        assert!(
            !log.facts
                .lock()
                .unwrap()
                .iter()
                .any(|f| f.starts_with("shifted:")),
            "an establishment is not a shift"
        );
    }

    #[tokio::test]
    async fn a_failing_detector_keeps_the_previous_topic_and_reports() {
        let log = FactLog::default();
        let bus = EventBus::new(vec![Arc::new(log.clone())]);
        let outlet = FactOutlet::bus_only(&bus);
        let runtime = crate::SynonzRuntime::builder().build();
        let subject = subject();
        let conversation = crate::Conversation::with_id(&runtime, &subject, "c-detect-fail");
        conversation.set_topic("billing");

        let engine = engine(
            StubAssembler { frame: vec![] },
            Arc::new(StubPipeline::default()),
            Some(Arc::new(StubDetector {
                verdict: Err(MemoryFailure::new("topic", "model down")),
            })),
        );
        engine
            .on_turn_completed(
                &conversation,
                "hi",
                &[Message::user("hi")],
                &[],
                &[],
                &model(),
                &outlet,
                ConversationTaskSpawner::new(&runtime, conversation.id()),
            )
            .await;

        assert_eq!(conversation.topic().as_deref(), Some("billing"));
        bus.flush().await;
        let facts = log.facts.lock().unwrap();
        assert!(facts.iter().any(|f| f == "failed:topic"), "{facts:?}");
        assert!(
            facts.iter().any(|f| f == "archived:billing"),
            "the archive still runs: {facts:?}"
        );
    }

    #[test]
    fn memory_failure_is_constructible_and_plain() {
        let failure = MemoryFailure::new("stage", "detail");
        assert_eq!(failure.stage, "stage");
        assert_eq!(failure.detail, "detail");
    }

    #[test]
    fn memory_scope_is_used_by_the_contracts() {
        let _scope = MemoryScope::new("project:x");
    }
}
