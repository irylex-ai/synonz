//! The context engine: the narrative background of a conversation.
//!
//! `Context` is the background engine — the third persistent object. It
//! owns the **three behaviors** of the conversation's narrative background:
//! **assemble** (what the model sees before a call), **archive**
//! (what a completed turn leaves in the L1 layer and the topic state), and
//! **compress** (the L1→L2 / L2→L3 flows fired by the trigger policies).
//!
//! Its data source is **memory only** — the conversation is where the
//! engine comes from (identity, handles, timing), never what it reads. The
//! strategy decides what is sent; the framework owns when.

use crate::conversation::Conversation;
use crate::event::MemoryFlowStage;
use crate::memory::MemoryStore;
use crate::message::Message;
use crate::model::Model;
use crate::runtime::SynonzRuntime;
use crate::subject::Subject;
use futures::future::BoxFuture;

/// The inputs an assembly strategy consumes — the minimal sufficient set.
/// Deliberately narrow: a strategy reads **memory**, plus the
/// identity and topic needed to query it; reading the conversation entity
/// is not expressible.
#[non_exhaustive]
pub struct AssemblyRequest<'a> {
    /// The memory store (the sole data source).
    pub memory: &'a dyn MemoryStore,
    /// The subject owning the memory.
    pub subject: &'a Subject,
    /// The conversation whose layers are assembled.
    pub conversation_id: &'a str,
    /// The session's current topic.
    pub topic: &'a str,
    /// The turn's user input text.
    pub input: &'a str,
}

/// One degraded read: the stage that failed and why. Surfaced as a
/// `MemoryFlowFailed` event by the caller — never silent.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct AssemblyFailure {
    /// Which background stage failed.
    pub stage: MemoryFlowStage,
    /// Human-readable detail of the failure.
    pub detail: String,
}

/// What an assembly produced: the composed background plus the reads that
/// failed along the way (degraded but visible).
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct AssemblyOutput {
    /// The composed background messages.
    pub messages: Vec<Message>,
    /// The memory reads that failed while composing (the corresponding
    /// layers are absent from `messages`).
    pub failures: Vec<AssemblyFailure>,
}

/// Strategy-level failure: the strategy itself could not produce anything.
/// Memory-read degradation is *not* this — it travels in
/// [`AssemblyOutput::failures`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AssemblyError {
    /// The strategy failed to produce a context.
    #[error("context assembly failed: {0}")]
    Failed(String),
}

/// The context assembly strategy contract.
///
/// Strategies decide **what is sent** (the composition of L1/L2/L3 and
/// their formatting); timing, turn recording, and write flows stay in the
/// framework. Strategies read memory through [`AssemblyRequest`]; the
/// type makes reading the conversation entity unexpressible.
pub trait ContextAssembly: Send + Sync + 'static {
    /// Assembles the context messages for one turn.
    fn assemble<'a>(
        &'a self,
        request: AssemblyRequest<'a>,
    ) -> BoxFuture<'a, Result<AssemblyOutput, AssemblyError>>;
}

/// The session-scoped background engine (the third persistent object).
///
/// Cloning shares the same background. Constructed from the conversation
/// (identity) plus the operating runtime (services) at execution time —
/// there is no manual mounting (derivation kills the background/turn
/// split).
#[derive(Clone)]
pub struct Context {
    conversation: Conversation,
    runtime: SynonzRuntime,
}

impl Context {
    pub(crate) fn for_conversation(conversation: &Conversation, runtime: &SynonzRuntime) -> Self {
        Self {
            conversation: conversation.clone(),
            runtime: runtime.clone(),
        }
    }

    /// Assembles the background for one turn (fresh — the memory layers
    /// are read at assembly time, so just-completed turns are included).
    ///
    /// Memory reads that fail are reported in
    /// [`AssemblyOutput::failures`] and the run continues with the layers
    /// that succeeded — degraded, never silent.
    pub async fn assemble(&self, input: &str) -> AssemblyOutput {
        let topic = self.conversation.topic().unwrap_or_default();
        let memory = self.runtime.memory_store();
        let request = AssemblyRequest {
            memory: &*memory,
            subject: self.conversation.subject(),
            conversation_id: self.conversation.id(),
            topic: &topic,
            input,
        };
        match self.runtime.context_assembly().assemble(request).await {
            Ok(output) => output,
            Err(error) => AssemblyOutput {
                messages: Vec::new(),
                failures: vec![AssemblyFailure {
                    stage: MemoryFlowStage::AssembleRead,
                    detail: error.to_string(),
                }],
            },
        }
    }

    /// The archive + compress moment (the second and third behaviors).
    ///
    /// Called by the execution loop after a **completed** turn: the L1
    /// archive is written, the topic state advances, and the trigger
    /// policies (mandatory floors + stacked events) run. Failed and
    /// cancelled turns enter the truth archive only — they never feed the
    /// memory layers.
    ///
    /// Flow failures are emitted as `FlowFailed` memory facts through the
    /// event sink — visible, never silent; they do
    /// not abort the run.
    pub(crate) async fn on_turn_completed(
        &self,
        model: &dyn Model,
        input: &str,
        messages: Vec<Message>,
        sink: &crate::bus::EventSink,
    ) {
        let memory_policies = self.runtime.memory_policies();
        let topic_detector = self.runtime.topic_detector();
        let memory = self.runtime.memory_store();
        let (topic, soft_errors) = crate::trigger::run_post_turn_flows(
            crate::trigger::PostTurn {
                model,
                conversation: &self.conversation,
                memory: &*memory,
                memory_policies: &memory_policies,
                topic_detector: &*topic_detector,
                input,
                messages,
            },
            sink,
        )
        .await;
        for (stage, detail) in soft_errors {
            sink.emit_memory(crate::MemoryEvent::FlowFailed {
                stage,
                detail,
                moment: crate::MemoryFlowFailedMoment::AfterTurn,
            });
        }
        let _ = topic;
    }
}

/// The L3 retrieval budget for the default strategy.
pub const DEFAULT_L3_BUDGET: usize = 3;

/// The default strategy: layered assembly (L3 recall → L2 summaries →
/// L1 window). Memory is the sole source; per-layer read failures degrade
/// the background visibly (`AssemblyOutput::failures`), never silently.
#[derive(Default)]
pub struct LayeredMemory;

impl ContextAssembly for LayeredMemory {
    fn assemble<'a>(
        &'a self,
        request: AssemblyRequest<'a>,
    ) -> BoxFuture<'a, Result<AssemblyOutput, AssemblyError>> {
        Box::pin(async move {
            let mut output = AssemblyOutput::default();

            // L3 recall: independent System message (persona/memory
            // separation, P2).
            let topic = request.topic.to_string();
            match request.memory.l3_retrieve(
                request.subject,
                request.input,
                &topic,
                DEFAULT_L3_BUDGET,
            ) {
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
                    detail: format!("l3 retrieve: {error}"),
                }),
            }

            // L2: this conversation's earlier turns, summarized.
            match request
                .memory
                .l2_read(request.subject, request.conversation_id)
            {
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
            match request
                .memory
                .l1_window(request.subject, request.conversation_id)
            {
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

            Ok(output)
        })
    }
}
