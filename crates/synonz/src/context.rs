//! The Context engine: the agent's context — its state.
//!
//! The context of an agent is *its state*: the layered memory (the
//! persistent settling part), the environment perception (the run's model
//! and tool results), and — in future material slots — runtime-derived
//! elements. The engine owns the two moments of that state's lifecycle:
//!
//! - **materialization** (the engine's `assemble` entry, framework-internal):
//!   the state → the working payload (the background messages a run starts
//!   from), preceded by the read-side input preprocessing when a rewriter
//!   strategy is configured;
//! - **maintenance** (the engine's `on_turn_completed` entry,
//!   framework-internal): new perception settles into the state (L1
//!   archive, topic advance) and the state is curated over time (L1→L2
//!   compaction, L2→L3 distillation).
//!
//! The engine is a **concrete type** (the open engine trait was retired):
//! the read phase is fully replaceable through its strategy slot
//! ([`ContextAssembler`], reading through a read-only memory view), while
//! the write phase is framework-owned — its customization points are the
//! narrow sub-hooks ([`ConversationTopicDetector`], [`MemorySummarizer`],
//! and the planned `MemoryDistiller`).
//!
//! The engine is **pure behavior**: it holds strategy slots and floor
//! parameters, no instance state — background maintenance tasks register
//! with the runtime's conversation table (the system schedules
//! what the application's engine produces), and memory arrives through
//! the payload at every call (behavior and data meet at the payload, the
//! runtime orchestrates).

use std::sync::Arc;

use futures::StreamExt;
use futures::future::BoxFuture;

use crate::bus::{EventSink, MemoryEvent, MemoryFlowFailedMoment};
use crate::conversation::Conversation;
use crate::error::ModelError;
use crate::event::{CallPurpose, MemoryFlowStage, ModelEvent, TurnEvent};
use crate::memory::{L1Entry, L2Entry, MemoryLayerStore, MemoryReader, Topic};
use crate::message::Message;
use crate::model::{Model, ModelRequest, ModelStream, ModelStreamItem};
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

// ── The assembly payload ──

/// The assembly payload: what the assembler consumes — the minimal
/// sufficient set. Deliberately narrow: a strategy reads the **memory**
/// through a read-only [`MemoryReader`] plus the identity, topic, and
/// input needed to query it; reading the conversation entity is not
/// expressible (the assembly never reads the truth domain).
///
/// `input` is the turn's original text (the truth domain);
/// `rewritten_input` is the model view the read-side preprocessing
/// ([`TurnInputRewriter`]) produced — the engine fills it before the
/// assembler runs. The material domain is open for growth
/// (`non_exhaustive`): future perception/runtime materials (environment
/// snapshots, tool catalogs, in-run messages) join as new fields without
/// breaking strategies.
#[non_exhaustive]
pub struct ContextAssemblerInput<'a> {
    /// The read-only memory view (the primary material — the state's
    /// persistent part).
    pub reader: MemoryReader<'a>,
    /// The subject owning the memory.
    pub subject: &'a Subject,
    /// The conversation whose layers are assembled.
    pub conversation_id: &'a str,
    /// The conversation's current topic.
    pub topic: &'a str,
    /// The turn's original user input text (truth domain).
    pub input: &'a str,
    /// The model-view input (the rewriter's output); `None` = no rewrite.
    pub rewritten_input: Option<&'a str>,
}

impl<'a> ContextAssemblerInput<'a> {
    /// Assembles the input (the struct-literal route stays closed for
    /// field growth; tests and strategies construct through here).
    /// `rewritten_input` is engine-filled and starts as `None`.
    pub fn new(
        reader: MemoryReader<'a>,
        subject: &'a Subject,
        conversation_id: &'a str,
        topic: &'a str,
        input: &'a str,
    ) -> Self {
        Self {
            reader,
            subject,
            conversation_id,
            topic,
            input,
            rewritten_input: None,
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

/// The read-side input preprocessing strategy: rewrites the turn's user
/// input into the model view used for assembly (for example coreference
/// resolution against the recent conversation).
///
/// Runs **before** assembly; the rewritten text flows into
/// [`ContextAssemblerInput::rewritten_input`] and the assembler may use it
/// for recall and composition. The model-visible current user message
/// stays the original text. `None` = keep the original; `Err` = visible
/// degradation (the original is kept and the failure is reported through
/// [`ContextAssemblerOutput::failures`]).
pub trait TurnInputRewriter: Send + Sync + 'static {
    /// Produces the model-view input; `input` is the original user text,
    /// `history` the recent conversation messages the engine provides.
    fn rewrite<'a>(
        &'a self,
        input: &'a str,
        history: &'a [Message],
    ) -> BoxFuture<'a, Result<Option<String>, String>>;
}

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
/// The framework narrates the strategy's model calls (the engine hands it
/// a framed handle); the strategy must not emit events itself. Returning
/// `Err` hands the lossless-degradation decision to the engine (the raw
/// transcript becomes the summary, and the failure is surfaced — never
/// silent).
pub trait MemorySummarizer: Send + Sync + 'static {
    /// Summarizes the given L1 entries into one short summary text.
    fn summarize<'a>(
        &'a self,
        entries: &'a [L1Entry],
        model: &'a dyn Model,
    ) -> BoxFuture<'a, Result<String, String>>;
}

/// The distillation strategy: how L2 overflow becomes L3 knowledge (the
/// content transform of distillation).
///
/// Reads across layers through the read-only view (L1 anchoring, L2
/// subject, existing L3 normalization). Returns knowledge texts only; the
/// engine stamps identity and time, performs the L3 upsert, and pops the
/// processed L2 entries **after the transform succeeds** (a failed
/// transform keeps L2 — never lose data). The framework narrates the
/// strategy's model calls.
pub trait MemoryDistiller: Send + Sync + 'static {
    /// Distills the given L2 entries into L3 knowledge texts.
    fn distill<'a>(
        &'a self,
        blocks: &'a [L2Entry],
        conversation_id: &'a str,
        topic: &'a str,
        reader: MemoryReader<'a>,
        model: &'a dyn Model,
    ) -> BoxFuture<'a, Result<Vec<String>, String>>;
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

// ── The engine ──

/// The state engine (concrete type — the open engine trait was retired):
/// layered maintenance with strategy slots and floor parameters.
///
/// The engine owns the two state-lifecycle moments as framework-internal
/// entries: materialization (`assemble`) and maintenance
/// (`on_turn_completed`). The read phase is fully replaceable through
/// [`Context::with_assembler`] (reading through a read-only memory view);
/// the write phase is framework-owned — advance the topic (a shift
/// compacts the pre-shift turns and emits `TopicShifted`), archive the
/// turn into L1 (emits `TurnArchived`), then background-spawn the
/// compaction (L1 overflow → L2 summary, emits `Compacted`) and
/// distillation (L2 overflow → L3, emits `Distilled`). Compaction order
/// is summarize → append → pop, so a concurrent assembly never observes a
/// memory hole; distillation transforms first and pops after, so a failed
/// transform never loses L2.
pub struct Context {
    assembler: Arc<dyn ContextAssembler>,
    rewriter: Option<Arc<dyn TurnInputRewriter>>,
    summarizer: Arc<dyn MemorySummarizer>,
    distiller: Arc<dyn MemoryDistiller>,
    topic_detector: Arc<dyn ConversationTopicDetector>,
    model: Option<Arc<dyn Model>>,
    l1_window: usize,
    l2_cap: usize,
}

/// The L3 recall budget of the built-in assembler.
pub(crate) const DEFAULT_L3_BUDGET: usize = 3;

/// The built-in summarization prompt (verbatim).
const DEFAULT_SUMMARY_PROMPT: &str = "Summarize the following conversation turns into one short paragraph, preserving key facts, decisions, and preferences:";

impl Context {
    /// Creates the engine with everything defaulted (floors 20/8,
    /// first-segment topic detection, prompt summarization, mechanical
    /// distillation, layered assembly, no input rewriting; the engine
    /// model defaults to the agent's model).
    pub fn new() -> Self {
        Self {
            assembler: Arc::new(LayeredMemoryContextAssembler),
            rewriter: None,
            summarizer: Arc::new(DefaultMemorySummarizer),
            distiller: Arc::new(MechanicalMemoryDistiller),
            topic_detector: Arc::new(FirstSegmentTopicDetector),
            model: None,
            l1_window: 20,
            l2_cap: 8,
        }
    }

    /// Replaces the assembly strategy.
    pub fn with_assembler(mut self, assembler: impl ContextAssembler + 'static) -> Self {
        self.assembler = Arc::new(assembler);
        self
    }

    /// Replaces the read-side input preprocessing strategy (default:
    /// none — the turn input reaches assembly verbatim).
    pub fn with_rewriter(mut self, rewriter: impl TurnInputRewriter + 'static) -> Self {
        self.rewriter = Some(Arc::new(rewriter));
        self
    }

    /// Replaces the summarization strategy.
    pub fn with_summarizer(mut self, summarizer: impl MemorySummarizer + 'static) -> Self {
        self.summarizer = Arc::new(summarizer);
        self
    }

    /// Replaces the distillation strategy.
    pub fn with_distiller(mut self, distiller: impl MemoryDistiller + 'static) -> Self {
        self.distiller = Arc::new(distiller);
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

    /// Sets the engine model: the model the engine's maintenance calls
    /// (summarization, distillation) resolve to. Default: the agent's
    /// model. Memory follows the context — there is no separate memory
    /// model.
    pub fn with_model(mut self, model: Arc<dyn Model>) -> Self {
        self.model = Some(model);
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

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    /// Materializes the state (framework-internal): the turn input is
    /// preprocessed into its model view (when a [`TurnInputRewriter`] is
    /// configured), then the assembler composes the working payload's
    /// background. Called once per run, before the reasoning loop.
    pub(crate) async fn assemble<'a>(
        &'a self,
        input: ContextAssemblerInput<'a>,
    ) -> ContextAssemblerOutput {
        let Some(rewriter) = &self.rewriter else {
            return self.assembler.assemble(input).await;
        };

        // The rewriter's context: this conversation's L1 window (the
        // engine provides it; the strategy decides how much to use).
        let history: Vec<Message> = input
            .reader
            .l1_window(input.conversation_id)
            .unwrap_or_default()
            .into_iter()
            .flat_map(|entry| entry.messages)
            .collect();

        match rewriter.rewrite(input.input, &history).await {
            Ok(Some(rewritten)) => {
                let rebuilt = ContextAssemblerInput {
                    rewritten_input: Some(&rewritten),
                    ..input
                };
                self.assembler.assemble(rebuilt).await
            }
            Ok(None) => self.assembler.assemble(input).await,
            Err(error) => {
                // Visible degradation: the original input stands, and the
                // failure rides the assembly output's failure list.
                let mut output = self.assembler.assemble(input).await;
                output.failures.push(AssemblyFailure {
                    stage: MemoryFlowStage::Rewrite,
                    detail: format!("input rewrite: {error}"),
                });
                output
            }
        }
    }

    /// Maintains the state on turn completion (framework-internal): archive
    /// the turn's perception into memory, advance the topic, and curate the
    /// layers. Called after a **completed** turn, before the terminal event;
    /// heavy maintenance (compaction, distillation) runs in the background
    /// (spawned through `task_spawner`; the runtime drains it at
    /// conversation end).
    ///
    /// Failed and cancelled turns never reach this method — the truth
    /// archive takes them, the memory layers stay unpolluted.
    ///
    /// Synchronous-segment failures are returned (the framework surfaces
    /// them as `FlowFailed { moment: AfterTurn }` facts); background
    /// failures surface through the event sink.
    // The parameter list is the minimal sufficient set (turn facts +
    // runtime outlets + the agent model); it is a framework-internal entry
    // point, not a public ergonomics surface.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn on_turn_completed(
        &self,
        conversation: &Conversation,
        input: &str,
        messages: Vec<Message>,
        memory: &MemoryLayerStore,
        agent_model: Arc<dyn Model>,
        events: EventSink,
        task_spawner: ConversationTaskSpawner,
    ) -> Vec<MemoryFlowError> {
        let mut errors = Vec::new();
        let subject = conversation.subject();
        let conversation_id = conversation.id();

        // 1. Topic state machine (synchronous segment: cheap and
        //    next-turn-visible).
        let decision = self
            .topic_detector
            .detect(input, conversation.topic().as_deref());
        let previous = conversation.topic().unwrap_or_default();
        conversation.set_topic(&decision.topic);
        if decision.shifted {
            events.emit_conversation(crate::ConversationEvent::TopicShifted {
                conversation_id: conversation_id.to_string(),
                from: previous,
                to: decision.topic.clone(),
            });
        }

        // 2. L1 archive (synchronous segment).
        if let Err(error) = memory.l1_append(subject, conversation_id, &decision.topic, messages) {
            errors.push(MemoryFlowError::new(
                MemoryFlowStage::Archive,
                format!("l1 append: {error}"),
            ));
        } else {
            events.emit_memory(MemoryEvent::TurnArchived {
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
        // Model resolution: the engine model when configured, otherwise
        // the agent's model. The strategy handle is framed so the
        // framework narrates every auxiliary call.
        let resolved = self.model.clone().unwrap_or(agent_model);
        let narrated: Arc<dyn Model> = Arc::new(NarratedModel {
            inner: resolved,
            sink: events.clone(),
        });
        let jobs = BackgroundMaintenanceTask {
            summarizer: Arc::clone(&self.summarizer),
            distiller: Arc::clone(&self.distiller),
            memory: memory.clone(),
            sink: events.clone(),
            model: narrated,
            subject: subject.clone(),
            conversation_id: conversation_id.to_string(),
            topic: decision.topic.clone(),
            l2_cap: self.l2_cap,
        };
        task_spawner.spawn(async move {
            if shift_flush {
                jobs.compact(true, l1_window).await;
            }
            jobs.compact(false, l1_window).await;
            jobs.distill().await;
        });

        errors
    }
}

/// The default engine's background maintenance task: the owned inputs and
/// steps of one per-turn maintenance run (a spawned task carries it;
/// clones share nothing — every clone is its own task).
#[derive(Clone)]
struct BackgroundMaintenanceTask {
    summarizer: Arc<dyn MemorySummarizer>,
    distiller: Arc<dyn MemoryDistiller>,
    memory: MemoryLayerStore,
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
            .summarize(&entries, self.model.as_ref())
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
            L2Entry::new(self.conversation_id.clone(), summary, 0).with_topic(self.topic.clone()),
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

    /// Distills L2 overflow into L3 knowledge through the configured
    /// strategy (the default is mechanical: each summary becomes
    /// long-term knowledge under the conversation's topic). Transform
    /// first, pop after — a failed transform keeps L2.
    ///
    /// Concurrency discipline (ADR-0020): sources are re-validated by id
    /// before the transform (already-forgotten ones are skipped) and
    /// again before the write; if the source set changes during the
    /// transform, the produced output is discarded (it may contain
    /// forgotten content) and the survivors stay for a later pass.
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
        // Peek the processed prefix.
        let peeked: Vec<L2Entry> = match self.memory.l2_read(&self.subject, &self.conversation_id) {
            Ok(blocks) => blocks.into_iter().take(overflow).collect(),
            Err(error) => {
                self.sink.emit_memory(MemoryEvent::FlowFailed {
                    stage: MemoryFlowStage::Distill,
                    detail: format!("distill read: {error}"),
                    moment: MemoryFlowFailedMoment::Background,
                });
                return;
            }
        };
        // Pre-transform validation: only sources that still exist reach
        // the strategy (entries forgotten meanwhile are skipped).
        let mut survivors: Vec<L2Entry> = Vec::with_capacity(peeked.len());
        for block in &peeked {
            match self.memory.l2_get(&self.subject, &block.id) {
                Ok(Some(current)) => survivors.push(current),
                Ok(None) => {}
                Err(error) => {
                    self.sink.emit_memory(MemoryEvent::FlowFailed {
                        stage: MemoryFlowStage::Distill,
                        detail: format!("distill validate: {error}"),
                        moment: MemoryFlowFailedMoment::Background,
                    });
                    return;
                }
            }
        }
        if survivors.is_empty() {
            return;
        }
        if survivors.len() < peeked.len() {
            self.sink.emit_memory(MemoryEvent::FlowFailed {
                stage: MemoryFlowStage::Distill,
                detail: format!(
                    "{} source(s) were forgotten before distillation; skipped",
                    peeked.len() - survivors.len()
                ),
                moment: MemoryFlowFailedMoment::Background,
            });
        }
        let reader = self.memory.reader(&self.subject);
        let contents = match self
            .distiller
            .distill(
                &survivors,
                &self.conversation_id,
                &self.topic,
                reader,
                self.model.as_ref(),
            )
            .await
        {
            Ok(contents) => contents,
            Err(error) => {
                // Lossless degradation: the L2 entries stay.
                self.sink.emit_memory(MemoryEvent::FlowFailed {
                    stage: MemoryFlowStage::Distill,
                    detail: format!("distillation failed (L2 kept): {error}"),
                    moment: MemoryFlowFailedMoment::Background,
                });
                return;
            }
        };
        // Post-transform validation and claim: if a source vanished
        // during the transform (or between validation and claim), the
        // output may contain forgotten content — discard it.
        for block in &survivors {
            match self.memory.l2_remove(&self.subject, &block.id) {
                Ok(true) => {}
                Ok(false) => {
                    self.sink.emit_memory(MemoryEvent::FlowFailed {
                        stage: MemoryFlowStage::Distill,
                        detail: "sources changed during distillation; output discarded".to_string(),
                        moment: MemoryFlowFailedMoment::Background,
                    });
                    return;
                }
                Err(error) => {
                    self.sink.emit_memory(MemoryEvent::FlowFailed {
                        stage: MemoryFlowStage::Distill,
                        detail: format!("l2 claim: {error}"),
                        moment: MemoryFlowFailedMoment::Background,
                    });
                    return;
                }
            }
        }
        let mut distilled = 0usize;
        for content in contents {
            let entry = crate::memory::L3Entry::new(
                crate::memory::L3Identity {
                    subject_id: self.subject.to_string(),
                    conversation_id: self.conversation_id.clone(),
                    topic: self.topic.clone(),
                },
                content,
            );
            if let Err(error) = self.memory.l3_upsert(&self.subject, entry) {
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
            // separation). Recall runs on the model-view input.
            let topic = input.topic.to_string();
            match input.reader.l3_query(
                input.rewritten_input.unwrap_or(input.input),
                &topic,
                DEFAULT_L3_BUDGET,
            ) {
                Ok(l3) if !l3.is_empty() => {
                    let recall = l3
                        .iter()
                        .map(|entry| format!("- {}", entry.content))
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
            match input.reader.l2_read(input.conversation_id) {
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
            match input.reader.l1_window(input.conversation_id) {
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

/// The built-in summarizer: prompt-driven summarization through the model
/// (the framework narrates the call; the prompt is strategy content —
/// replace the strategy to change it).
pub(crate) struct DefaultMemorySummarizer;

impl MemorySummarizer for DefaultMemorySummarizer {
    fn summarize<'a>(
        &'a self,
        entries: &'a [L1Entry],
        model: &'a dyn Model,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            if entries.is_empty() {
                return Ok(String::new());
            }
            let transcript = transcript_of(entries);
            let request = Message::user(format!("{DEFAULT_SUMMARY_PROMPT}\n\n{transcript}"));
            let call = crate::model::ModelRequest::new(vec![request], Vec::new());
            match crate::model::complete(model, call).await {
                Ok((message, _usage)) => Ok(text_of(&message).unwrap_or_default()),
                Err(error) => Err(error.to_string()),
            }
        })
    }
}

/// The built-in distiller: mechanical promotion — each summary becomes one
/// knowledge text under the conversation's topic (no model call; replace
/// the strategy for LLM-based extraction).
pub(crate) struct MechanicalMemoryDistiller;

impl MemoryDistiller for MechanicalMemoryDistiller {
    fn distill<'a>(
        &'a self,
        blocks: &'a [L2Entry],
        _conversation_id: &'a str,
        _topic: &'a str,
        _reader: MemoryReader<'a>,
        _model: &'a dyn Model,
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        Box::pin(async move { Ok(blocks.iter().map(|block| block.content.clone()).collect()) })
    }
}

/// The framework's narration wrapper for engine-issued auxiliary model
/// calls: emits `Requested` before the call and `Responded` on its finish
/// item (bus + delivery), and never emits `StreamDelta` (auxiliary calls
/// consume the final result — there is no product stream to narrate).
struct NarratedModel {
    inner: Arc<dyn Model>,
    sink: EventSink,
}

impl Model for NarratedModel {
    fn stream(&self, request: ModelRequest) -> BoxFuture<'_, Result<ModelStream, ModelError>> {
        Box::pin(async move {
            let _ = self
                .sink
                .emit_turn(TurnEvent::Model(ModelEvent::Requested {
                    purpose: CallPurpose::ContextManagement,
                    messages: request.messages.clone(),
                    round: None,
                }))
                .await;
            let inner = self.inner.stream(request).await?;
            let sink = self.sink.clone();
            let stream = futures::stream::unfold(Some(inner), move |state| {
                let sink = sink.clone();
                async move {
                    let mut inner = state?;
                    match inner.next().await {
                        Some(ModelStreamItem::Finish { message, usage }) => {
                            let _ = sink
                                .emit_turn(TurnEvent::Model(ModelEvent::Responded {
                                    message: message.clone(),
                                    usage,
                                    round: None,
                                }))
                                .await;
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
