//! The Context state engine: the agent's context — its state.
//!
//! The context of an agent is *its state*: the layered memory (the
//! persistent沉淀 part), the environment perception (the run's model and
//! tool results), and — in future material slots — runtime-derived
//! elements. The state engine owns the two moments of that state's
//! lifecycle:
//!
//! - **materialization** ([`Context::assemble`]): the state → the working
//!   payload (the background messages a run starts from);
//! - **maintenance** ([`Context::on_turn_completed`]): new perception
//!   settles into the state (L1 archive, topic advance) and the state is
//!   curated over time (L1→L2 compaction, L2→L3 distillation).
//!
//! The contract is **doubly neutral**: neither the layer philosophy
//! (L1/L2/L3, floors, promotion paths) nor the timing engineering
//! (backgrounding, ordering, latency) leaks into it. It anchors only the
//! two lifecycle facts. Replace the whole philosophy with a custom
//! `impl Context`; tune one axis through the three strategy slots.
//!
//! The engine is **pure behavior**: it holds strategy slots and floor
//! parameters, no instance state — background maintenance tasks register
//! with the runtime's conversation table (the system schedules
//! what the application's engine produces), and memory arrives through
//! the payload at every call (behavior and data meet at the payload, the
//! runtime orchestrates).

use std::sync::Arc;

use futures::future::BoxFuture;

use crate::bus::{EventSink, MemoryEvent, MemoryFlowFailedMoment};
use crate::conversation::Conversation;
use crate::event::{CallPurpose, MemoryFlowStage, ModelEvent, TurnEvent};
use crate::memory::{L1Entry, Memory, SummaryBlock, Topic};
use crate::message::Message;
use crate::model::Model;
use crate::runtime::ConversationTaskSpawner;
use crate::subject::Subject;

// ── Failure type ──

/// One memory-flow failure, typed by stage (the contract's soft-failure
/// return). The framework surfaces these as `FlowFailed` memory facts —
/// degraded, never silent.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryFlowError {
    /// Which background stage failed.
    pub stage: MemoryFlowStage,
    /// Human-readable detail of the failure.
    pub detail: String,
}

impl MemoryFlowError {
    pub(crate) fn new(stage: MemoryFlowStage, detail: impl Into<String>) -> Self {
        Self {
            stage,
            detail: detail.into(),
        }
    }
}

// ── The maintenance payloads ──

/// The turn-completed payload: everything the engine's maintenance needs.
///
/// `messages` are owned (background tasks carry them forward); `memory`
/// and `model` are the runtime's (borrowed for the synchronous segment,
/// cloned into background jobs); `events` and `task_spawner` are the
/// framework outlets (the event sink and this conversation's background
/// spawner).
#[non_exhaustive]
pub struct TurnContext<'a> {
    /// The conversation the turn completed in.
    pub conversation: &'a Conversation,
    /// The turn's user input text.
    pub input: &'a str,
    /// The turn's canonical messages (the L1 entry's content).
    pub messages: Vec<Message>,
    /// The layered memory (from the runtime — the single authority).
    pub memory: &'a Memory,
    /// The run's model (the maintenance's summarization calls it; clone
    /// it into background jobs).
    pub model: Arc<dyn Model>,
    /// The event outlet (bus facts; clones keep the run attribution).
    pub events: EventSink,
    /// This conversation's background-task spawner (submitted jobs attach
    /// to the conversation; the runtime drains them at conversation end).
    pub task_spawner: ConversationTaskSpawner,
}

/// The assembly payload: what the assembler consumes — the minimal
/// sufficient set. Deliberately narrow: a strategy reads the **memory**
/// plus the identity, topic, and input needed to query it; reading the
/// conversation entity is not expressible (the assembly never reads the
/// truth domain).
///
/// The material domain is open for growth (`non_exhaustive`): future
/// perception/runtime materials (environment snapshots, tool catalogs,
/// in-run messages) join as new fields without breaking strategies.
#[non_exhaustive]
pub struct ContextAssemblerInput<'a> {
    /// The layered memory (the primary material — the state's persistent
    /// part).
    pub memory: &'a Memory,
    /// The subject owning the memory.
    pub subject: &'a Subject,
    /// The conversation whose layers are assembled.
    pub conversation_id: &'a str,
    /// The conversation's current topic.
    pub topic: &'a str,
    /// The turn's user input text.
    pub input: &'a str,
}

impl<'a> ContextAssemblerInput<'a> {
    /// Assembles the input (the struct-literal route stays closed for
    /// field growth; tests and strategies construct through here).
    pub fn new(
        memory: &'a Memory,
        subject: &'a Subject,
        conversation_id: &'a str,
        topic: &'a str,
        input: &'a str,
    ) -> Self {
        Self {
            memory,
            subject,
            conversation_id,
            topic,
            input,
        }
    }
}

/// One degraded read: the stage that failed and why. Surfaced through
/// [`ContextAssemblerOutput::failures`] — never silent.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct AssemblyFailure {
    /// Which background stage failed.
    pub stage: MemoryFlowStage,
    /// Human-readable detail of the failure.
    pub detail: String,
}

/// What an assembly produced: the composed background plus the reads
/// that failed along the way (degraded but visible).
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct ContextAssemblerOutput {
    /// The composed background messages.
    pub messages: Vec<Message>,
    /// The memory reads that failed while composing (the corresponding
    /// layers are absent from `messages`).
    pub failures: Vec<AssemblyFailure>,
}

// ── The strategy slots ──

/// The assembly strategy: how the context state materializes into the
/// working payload (what the model sees).
///
/// Strategies decide **what is sent** (the composition and formatting of
/// the memory layers); timing, turn recording, and maintenance stay in
/// the engine. Strategies read memory through
/// [`ContextAssemblerInput`]; the type makes reading the conversation
/// entity unexpressible.
pub trait ContextAssembler: Send + Sync + 'static {
    /// Assembles the background for one turn (fresh — the layers are
    /// read at assembly time). Read failures degrade visibly through
    /// [`ContextAssemblerOutput::failures`].
    fn assemble<'a>(
        &'a self,
        input: ContextAssemblerInput<'a>,
    ) -> BoxFuture<'a, ContextAssemblerOutput>;
}

/// The summarization strategy: how L1 turns become an L2 summary (the
/// content transform of compaction).
///
/// The summarizer owns the narration of its model calls (it emits the
/// `ContextManagement` events through the sink, with `round: None` —
/// off-loop maintenance). Returning `Err` hands the lossless-degradation
/// decision to the engine (the raw transcript becomes the summary, and
/// the failure is surfaced — never silent).
pub trait MemorySummarizer: Send + Sync + 'static {
    /// Summarizes the given L1 entries into one short summary text.
    fn summarize<'a>(
        &'a self,
        entries: &'a [L1Entry],
        model: &'a dyn Model,
        events: &'a EventSink,
    ) -> BoxFuture<'a, Result<String, String>>;
}

/// The topic strategy: the conversation topic state machine.
///
/// Detects the topic of a turn and whether it shifted from the current
/// one. A shift's consequences are structural: the pre-shift turns are
/// compacted into L2 (the compaction semantics of the conversation context)
/// and a `TopicShifted` fact is emitted.
pub trait ConversationTopicDetector: Send + Sync + 'static {
    /// Returns the topic for `input` given the `current` topic; `shifted`
    /// signals a topic change.
    fn detect(&self, input: &str, current: Option<&str>) -> TopicDecision;
}

/// The topic state machine's decision on one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicDecision {
    /// The current topic after processing this turn.
    pub topic: String,
    /// Whether the topic shifted at this turn.
    pub shifted: bool,
}

// ── The engine contract ──

/// The agent's state engine: materialization + maintenance of the
/// context state.
///
/// Registered per agent (each application carries its own engine);
/// pure behavior — no instance state. The two methods anchor the two
/// lifecycle facts; everything else (when flows fire, how failures
/// surface, what runs in the background) is implementation freedom.
pub trait Context: Send + Sync + 'static {
    /// Materializes the state: the full context (memory + perception
    /// materials) → the working payload's background. Called once per
    /// run, before the reasoning loop.
    fn assemble<'a>(
        &'a self,
        input: ContextAssemblerInput<'a>,
    ) -> BoxFuture<'a, ContextAssemblerOutput>;

    /// Maintains the state on turn completion: archive the turn's
    /// perception into memory, advance the topic, and curate the layers.
    /// Called after a **completed** turn, before the terminal event —
    /// the synchronous segment should return promptly; heavy maintenance
    /// (compaction, distillation) belongs in the background (spawn
    /// through [`TurnContext::task_spawner`]; the runtime drains them at
    /// conversation end).
    ///
    /// Failed and cancelled turns never reach this method — the truth
    /// archive takes them, the memory layers stay unpolluted.
    ///
    /// Synchronous-segment failures are returned (the framework surfaces
    /// them as `FlowFailed { moment: AfterTurn }` facts); background
    /// failures surface themselves through the event sink (`FlowFailed {
    /// moment: Background }`).
    fn on_turn_completed<'a>(
        &'a self,
        ctx: &'a TurnContext<'a>,
    ) -> BoxFuture<'a, Vec<MemoryFlowError>>;
}

// ── The default engine ──

/// The default state engine: layered maintenance with three strategy
/// slots and floor parameters.
///
/// Maintenance orchestration (the default philosophy): on turn
/// completion — advance the topic (a shift compacts the pre-shift turns
/// and emits `TopicShifted`), archive the turn into L1 (emits
/// `TurnArchived`), then background-spawn the compaction (L1 overflow →
/// L2 summary, emits `Compacted`) and distillation (L2 overflow → L3,
/// emits `Distilled`). Compaction order is summarize → append → pop, so
/// a concurrent assembly never observes a memory hole.
pub struct DefaultContext {
    assembler: Arc<dyn ContextAssembler>,
    summarizer: Arc<dyn MemorySummarizer>,
    topic_detector: Arc<dyn ConversationTopicDetector>,
    l1_window: usize,
    l2_cap: usize,
}

/// The L3 recall budget of the built-in assembler.
pub(crate) const DEFAULT_L3_BUDGET: usize = 3;

/// The built-in summarization prompt (verbatim).
const DEFAULT_SUMMARY_PROMPT: &str = "Summarize the following conversation turns into one short paragraph, preserving key facts, decisions, and preferences:";

impl DefaultContext {
    /// Creates the engine with everything defaulted (floors 20/8,
    /// first-segment topic detection, prompt summarization, layered
    /// assembly).
    pub fn new() -> Self {
        Self {
            assembler: Arc::new(LayeredMemoryContextAssembler),
            summarizer: Arc::new(PromptMemorySummarizer::default()),
            topic_detector: Arc::new(FirstSegmentTopicDetector),
            l1_window: 20,
            l2_cap: 8,
        }
    }

    /// Replaces the assembly strategy.
    pub fn with_assembler(mut self, assembler: impl ContextAssembler + 'static) -> Self {
        self.assembler = Arc::new(assembler);
        self
    }

    /// Replaces the summarization strategy.
    pub fn with_summarizer(mut self, summarizer: impl MemorySummarizer + 'static) -> Self {
        self.summarizer = Arc::new(summarizer);
        self
    }

    /// Replaces the topic strategy.
    pub fn with_topic_detector(
        mut self,
        detector: impl ConversationTopicDetector + 'static,
    ) -> Self {
        self.topic_detector = Arc::new(detector);
        self
    }

    /// Configures the built-in summarizer's prompt (a shallow-customization
    /// convenience; a full `with_summarizer` replacement overrides it).
    pub fn with_summary_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.summarizer = Arc::new(PromptMemorySummarizer::new(prompt.into(), None));
        self
    }

    /// Gives the built-in summarizer a dedicated model (the background
    /// model: summarization budgets against it instead of the run's
    /// model). A shallow-customization convenience.
    pub fn with_summary_model(mut self, model: Arc<dyn Model>) -> Self {
        self.summarizer = Arc::new(PromptMemorySummarizer::new(
            DEFAULT_SUMMARY_PROMPT.to_string(),
            Some(model),
        ));
        self
    }

    /// Sets the L1 window: how many of the conversation's recent turns
    /// stay verbatim (mandatory floor; overflow compacts into L2).
    pub fn l1_window(mut self, n: usize) -> Self {
        self.l1_window = n;
        self
    }

    /// Sets the L2 cap: how many summary blocks the conversation keeps
    /// (mandatory floor; overflow distills into L3).
    pub fn l2_cap(mut self, n: usize) -> Self {
        self.l2_cap = n;
        self
    }
}

impl Default for DefaultContext {
    fn default() -> Self {
        Self::new()
    }
}

impl Context for DefaultContext {
    fn assemble<'a>(
        &'a self,
        input: ContextAssemblerInput<'a>,
    ) -> BoxFuture<'a, ContextAssemblerOutput> {
        self.assembler.assemble(input)
    }

    fn on_turn_completed<'a>(
        &'a self,
        ctx: &'a TurnContext<'a>,
    ) -> BoxFuture<'a, Vec<MemoryFlowError>> {
        Box::pin(async move {
            let mut errors = Vec::new();
            let subject = ctx.conversation.subject();
            let conversation_id = ctx.conversation.id();

            // 1. Topic state machine (synchronous segment: cheap and
            //    next-turn-visible).
            let decision = self
                .topic_detector
                .detect(ctx.input, ctx.conversation.topic().as_deref());
            let previous = ctx.conversation.topic().unwrap_or_default();
            ctx.conversation.set_topic(&decision.topic);
            if decision.shifted {
                ctx.events
                    .emit_conversation(crate::ConversationEvent::TopicShifted {
                        conversation_id: conversation_id.to_string(),
                        from: previous,
                        to: decision.topic.clone(),
                    });
            }

            // 2. L1 archive (synchronous segment).
            if let Err(error) = ctx.memory.l1_append(
                subject,
                conversation_id,
                &decision.topic,
                ctx.messages.clone(),
            ) {
                errors.push(MemoryFlowError::new(
                    MemoryFlowStage::Archive,
                    format!("l1 append: {error}"),
                ));
            } else {
                ctx.events.emit_memory(MemoryEvent::TurnArchived {
                    conversation_id: conversation_id.to_string(),
                    subject_id: subject.to_string(),
                    topic: decision.topic.clone(),
                });
            }

            // 3. Background segment: compaction (topic-shift flush + L1
            //    floor) and distillation (L2 floor) — one background
            //    maintenance task per turn, the flows in sequence (the
            //    distillation reads the L2 the compaction just wrote).
            //    Spawned through the runtime's task registry — the system
            //    schedules what the engine produces; the conversation-end
            //    teardown drains them.
            let shift_flush = decision.shifted;
            let l1_window = self.l1_window;
            let jobs = BackgroundMaintenanceTask {
                summarizer: Arc::clone(&self.summarizer),
                memory: ctx.memory.clone(),
                sink: ctx.events.clone(),
                model: Arc::clone(&ctx.model),
                subject: subject.clone(),
                conversation_id: conversation_id.to_string(),
                topic: decision.topic.clone(),
                l2_cap: self.l2_cap,
            };
            ctx.task_spawner.spawn(async move {
                if shift_flush {
                    jobs.compact(true, l1_window).await;
                }
                jobs.compact(false, l1_window).await;
                jobs.distill().await;
            });

            errors
        })
    }
}

/// The default engine's background maintenance task: the owned inputs and
/// steps of one per-turn maintenance run (a spawned task carries it;
/// clones share nothing — every clone is its own task).
#[derive(Clone)]
struct BackgroundMaintenanceTask {
    summarizer: Arc<dyn MemorySummarizer>,
    memory: Memory,
    sink: EventSink,
    model: Arc<dyn Model>,
    subject: Subject,
    conversation_id: String,
    topic: Topic,
    l2_cap: usize,
}

impl BackgroundMaintenanceTask {
    /// Compacts L1 overflow into an L2 summary: summarize the oldest
    /// entries (a shift flush compacts the whole window; the floor
    /// compacts the overflow), append the summary, then pop the entries —
    /// summarize → append → pop, so a concurrent assembly never sees a
    /// hole (the L1 over-window during summarization is the layered
    /// design's perceptual lag, by intent).
    async fn compact(&self, flush: bool, l1_window: usize) {
        let l1_len = match self.memory.l1_len(&self.subject, &self.conversation_id) {
            Ok(len) => len,
            Err(error) => {
                self.sink.emit_memory(MemoryEvent::FlowFailed {
                    stage: MemoryFlowStage::Summarize,
                    detail: format!("compaction read: {error}"),
                    moment: MemoryFlowFailedMoment::Background,
                });
                return;
            }
        };
        let overflow = if flush {
            l1_len
        } else {
            l1_len.saturating_sub(l1_window)
        };
        if overflow == 0 {
            return;
        }

        // Summarize first (peek the oldest entries through the window).
        let entries = match self.memory.l1_window(&self.subject, &self.conversation_id) {
            Ok(entries) if !entries.is_empty() => {
                let take = overflow.min(entries.len());
                entries[..take].to_vec()
            }
            Ok(_) => return,
            Err(error) => {
                self.sink.emit_memory(MemoryEvent::FlowFailed {
                    stage: MemoryFlowStage::Summarize,
                    detail: format!("compaction read: {error}"),
                    moment: MemoryFlowFailedMoment::Background,
                });
                return;
            }
        };
        let summary = match self
            .summarizer
            .summarize(&entries, self.model.as_ref(), &self.sink)
            .await
        {
            Ok(summary) => summary,
            Err(error) => {
                // Lossless degradation: the raw transcript becomes the L2
                // content — and the degradation is VISIBLE.
                self.sink.emit_memory(MemoryEvent::FlowFailed {
                    stage: MemoryFlowStage::Summarize,
                    detail: format!(
                        "summarization failed; the raw transcript was archived as the summary: {error}"
                    ),
                    moment: MemoryFlowFailedMoment::Background,
                });
                transcript_of(&entries)
            }
        };

        // Append the summary, then pop the entries (atomic-enough
        // handoff: the pop takes the oldest N of this conversation).
        if let Err(error) = self.memory.l2_append(
            &self.subject,
            SummaryBlock {
                conversation_id: self.conversation_id.clone(),
                content: summary,
                index: 0,
            },
        ) {
            self.sink.emit_memory(MemoryEvent::FlowFailed {
                stage: MemoryFlowStage::Summarize,
                detail: format!("l2 append: {error}"),
                moment: MemoryFlowFailedMoment::Background,
            });
            return;
        }
        if let Err(error) =
            self.memory
                .l1_pop_oldest(&self.subject, &self.conversation_id, overflow)
        {
            self.sink.emit_memory(MemoryEvent::FlowFailed {
                stage: MemoryFlowStage::Summarize,
                detail: format!("l1 pop: {error}"),
                moment: MemoryFlowFailedMoment::Background,
            });
            return;
        }
        self.sink.emit_memory(MemoryEvent::Compacted {
            conversation_id: self.conversation_id.clone(),
            count: overflow,
        });
    }

    /// Distills L2 overflow into L3 knowledge (mechanical: the summary
    /// becomes long-term knowledge under the conversation's topic — no
    /// model call; LLM-based extraction is a future pluggable
    /// improvement).
    async fn distill(&self) {
        let l2_len = match self.memory.l2_len(&self.subject, &self.conversation_id) {
            Ok(len) => len,
            Err(error) => {
                self.sink.emit_memory(MemoryEvent::FlowFailed {
                    stage: MemoryFlowStage::Distill,
                    detail: format!("distill read: {error}"),
                    moment: MemoryFlowFailedMoment::Background,
                });
                return;
            }
        };
        let overflow = l2_len.saturating_sub(self.l2_cap);
        if overflow == 0 {
            return;
        }
        let popped = match self
            .memory
            .l2_pop_oldest(&self.subject, &self.conversation_id, overflow)
        {
            Ok(popped) => popped,
            Err(error) => {
                self.sink.emit_memory(MemoryEvent::FlowFailed {
                    stage: MemoryFlowStage::Distill,
                    detail: format!("l2 pop: {error}"),
                    moment: MemoryFlowFailedMoment::Background,
                });
                return;
            }
        };
        let mut distilled = 0usize;
        for block in popped {
            let fragment = crate::memory::KnowledgeFragment {
                identity: crate::memory::FragmentIdentity {
                    subject_id: self.subject.to_string(),
                    conversation_id: block.conversation_id,
                    topic: self.topic.clone(),
                },
                content: block.content,
                created_at: now_epoch(),
            };
            if let Err(error) = self.memory.l3_upsert(&self.subject, fragment) {
                self.sink.emit_memory(MemoryEvent::FlowFailed {
                    stage: MemoryFlowStage::Distill,
                    detail: format!("l3 upsert: {error}"),
                    moment: MemoryFlowFailedMoment::Background,
                });
            } else {
                distilled += 1;
            }
        }
        if distilled > 0 {
            self.sink.emit_memory(MemoryEvent::Distilled {
                conversation_id: self.conversation_id.clone(),
                count: distilled,
            });
        }
    }
}

// ── The internalized default implementations ──

/// The built-in assembler: layered assembly (L3 recall → L2 summaries →
/// L1 window). Memory is the sole source; per-layer read failures degrade
/// the background visibly ([`ContextAssemblerOutput::failures`]), never
/// silently.
pub(crate) struct LayeredMemoryContextAssembler;

impl ContextAssembler for LayeredMemoryContextAssembler {
    fn assemble<'a>(
        &'a self,
        input: ContextAssemblerInput<'a>,
    ) -> BoxFuture<'a, ContextAssemblerOutput> {
        Box::pin(async move {
            let mut output = ContextAssemblerOutput::default();

            // L3 recall: independent System message (persona/memory
            // separation).
            let topic = input.topic.to_string();
            match input
                .memory
                .l3_query(input.subject, input.input, &topic, DEFAULT_L3_BUDGET)
            {
                Ok(l3) if !l3.is_empty() => {
                    let recall = l3
                        .iter()
                        .map(|fragment| format!("- {}", fragment.content))
                        .collect::<Vec<_>>()
                        .join("\n");
                    output
                        .messages
                        .push(Message::system(format!("Memory recall:\n{recall}")));
                }
                Ok(_) => {}
                Err(error) => output.failures.push(AssemblyFailure {
                    stage: MemoryFlowStage::AssembleRead,
                    detail: format!("l3 query: {error}"),
                }),
            }

            // L2: this conversation's earlier turns, summarized.
            match input.memory.l2_read(input.subject, input.conversation_id) {
                Ok(l2) => {
                    for block in l2 {
                        output.messages.push(Message::system(format!(
                            "Earlier in this conversation:\n{}",
                            block.content
                        )));
                    }
                }
                Err(error) => output.failures.push(AssemblyFailure {
                    stage: MemoryFlowStage::AssembleRead,
                    detail: format!("l2 read: {error}"),
                }),
            }

            // L1: this conversation's recent turns, verbatim, in order.
            match input.memory.l1_window(input.subject, input.conversation_id) {
                Ok(l1) => {
                    for entry in l1 {
                        output.messages.extend(entry.messages);
                    }
                }
                Err(error) => output.failures.push(AssemblyFailure {
                    stage: MemoryFlowStage::AssembleRead,
                    detail: format!("l1 window: {error}"),
                }),
            }

            output
        })
    }
}

/// The built-in summarizer: prompt-driven summarization through the model,
/// with an optional dedicated background model.
pub(crate) struct PromptMemorySummarizer {
    prompt: String,
    model: Option<Arc<dyn Model>>,
}

impl PromptMemorySummarizer {
    fn new(prompt: String, model: Option<Arc<dyn Model>>) -> Self {
        Self { prompt, model }
    }
}

impl Default for PromptMemorySummarizer {
    fn default() -> Self {
        Self::new(DEFAULT_SUMMARY_PROMPT.to_string(), None)
    }
}

impl MemorySummarizer for PromptMemorySummarizer {
    fn summarize<'a>(
        &'a self,
        entries: &'a [L1Entry],
        model: &'a dyn Model,
        events: &'a EventSink,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            if entries.is_empty() {
                return Ok(String::new());
            }
            let transcript = transcript_of(entries);
            let request = Message::user(format!("{}\n\n{transcript}", self.prompt));
            let _ = events
                .emit_turn(TurnEvent::Model(ModelEvent::Requested {
                    purpose: CallPurpose::ContextManagement,
                    messages: vec![request.clone()],
                    round: None,
                }))
                .await;
            let effective: &dyn Model = self.model.as_deref().unwrap_or(model);
            let call = crate::model::ModelRequest::new(vec![request], Vec::new());
            match crate::model::complete(effective, call).await {
                Ok((message, usage)) => {
                    let _ = events
                        .emit_turn(TurnEvent::Model(ModelEvent::Responded {
                            message: message.clone(),
                            usage,
                            round: None,
                        }))
                        .await;
                    Ok(text_of(&message).unwrap_or_default())
                }
                Err(error) => Err(error.to_string()),
            }
        })
    }
}

/// The built-in topic detector: the first meaningful segment of the input
/// as the topic, and no shift detection (a heuristic, not a
/// classification — documented as approximate).
pub(crate) struct FirstSegmentTopicDetector;

impl ConversationTopicDetector for FirstSegmentTopicDetector {
    fn detect(&self, input: &str, current: Option<&str>) -> TopicDecision {
        let topic: String = input
            .split_whitespace()
            .take(4)
            .collect::<Vec<_>>()
            .join(" ");
        match current {
            None | Some("") => TopicDecision {
                topic,
                shifted: false,
            },
            Some(existing) => TopicDecision {
                topic: existing.to_string(),
                shifted: false,
            },
        }
    }
}

fn transcript_of(entries: &[L1Entry]) -> String {
    let mut transcript = String::new();
    for entry in entries {
        for message in &entry.messages {
            match message.role {
                crate::message::Role::User => {
                    if let Some(text) = text_of(message) {
                        transcript.push_str("User: ");
                        transcript.push_str(&text);
                        transcript.push('\n');
                    }
                }
                crate::message::Role::Assistant => {
                    if let Some(text) = text_of(message) {
                        transcript.push_str("Assistant: ");
                        transcript.push_str(&text);
                        transcript.push('\n');
                    }
                }
                _ => {}
            }
        }
    }
    transcript
}

fn text_of(message: &Message) -> Option<String> {
    let mut text = String::new();
    for block in &message.blocks {
        if let crate::message::ContentBlock::Text { text: fragment } = block {
            text.push_str(fragment);
        }
    }
    (!text.is_empty()).then_some(text)
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
