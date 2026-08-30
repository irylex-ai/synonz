//! The memory behavior engine: topic tracking and layer-flow triggers
//! The mandatory floors fire on their resource conditions.
//!
//! Orchestration lives in the framework; storage lives in the plugin.
//! After every completed turn the execution calls
//! `run_post_turn_flows`: the topic state machine updates, the
//! mandatory resource floors fire (L1 window / L2 cap), and stacked
//! event policies fire on their conditions. Summarization (L1 → L2)
//! uses the agent's model and is visible in the event stream under
//! [`crate::CallPurpose::ContextManagement`].

use tokio::sync::mpsc;

use crate::conversation::Conversation;
use crate::event::{AgentEvent, CallPurpose, ModelEvent};
use crate::memory::{KnowledgeFragment, SummaryBlock};
use crate::message::{Message, Role};
use crate::model::{Model, ModelRequest};

/// The resource floors (mandatory, deterministic — can never be removed)
/// and the stacked event policies (opt-in).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct MemoryPolicies {
    /// L1 window: how many of the current conversation's turns are kept
    /// verbatim (mandatory floor; overflow demotes to L2).
    pub l1_window: usize,
    /// L2 cap: how many summary blocks this conversation keeps (mandatory
    /// floor; overflow distills into L3).
    pub l2_cap: usize,
    /// Stacked event policies (empty by default; the floors always apply).
    pub extra: Vec<EventPolicy>,
}

impl Default for MemoryPolicies {
    fn default() -> Self {
        Self {
            l1_window: 20,
            l2_cap: 8,
            extra: Vec::new(),
        }
    }
}

impl MemoryPolicies {
    /// Creates policies with explicit resource floors and no stacked
    /// events.
    pub fn new(l1_window: usize, l2_cap: usize) -> Self {
        Self {
            l1_window,
            l2_cap,
            extra: Vec::new(),
        }
    }

    /// Stacks event policies on top of the mandatory floors.
    pub fn with_extra(mut self, policies: impl IntoIterator<Item = EventPolicy>) -> Self {
        self.extra.extend(policies);
        self
    }
}

/// An opt-in, event-driven policy. The resource floors are not listed
/// here: they always apply.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventPolicy {
    /// Flush pre-shift turns into L2 when the topic detector reports a
    /// topic shift.
    TopicShift,
    /// Promote L2 summaries into L3 when the conversation ends.
    ConversationEnd,
}

/// The topic state machine's decision on one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicDecision {
    /// The current topic after processing this turn.
    pub topic: String,
    /// Whether the topic shifted at this turn.
    pub shifted: bool,
}

/// Detects the topic of a turn and whether it shifted from the current
/// one. Pluggable (default: a deterministic first-segment heuristic —
/// zero cost, documented as approximate).
pub trait TopicDetector: Send + Sync + 'static {
    /// Returns the topic for `input` given the `current` topic; `shifted`
    /// signals a topic change.
    fn detect(&self, input: &str, current: Option<&str>) -> TopicDecision;
}

/// The default detector: the first meaningful segment of the input as the
/// topic, and no shift detection (a heuristic, not a classification).
#[derive(Default)]
pub struct FirstSegmentDetector;

impl TopicDetector for FirstSegmentDetector {
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

/// The post-turn flow context: everything the engine needs.
pub(crate) struct PostTurn<'a> {
    pub model: &'a dyn Model,
    pub conversation: &'a Conversation,
    pub policies: &'a MemoryPolicies,
    pub detector: &'a dyn TopicDetector,
    /// The turn's user input (topic detection).
    pub input: &'a str,
    /// The turn's messages (the L1 entry's content).
    pub messages: Vec<Message>,
}

/// Runs the post-turn memory flows: L1 write, topic update, resource
/// floors, and stacked event policies. Emits `ContextManagement` events
/// for summarization calls into `emit`; returns the current topic.
///
/// Failures of the *flows* are non-fatal (memory is auxiliary): they are
/// logged through the returned `Vec` of soft errors and do not fail the
/// run (the run's own terminal event is emitted by the caller).
pub(crate) async fn run_post_turn_flows(
    ctx: PostTurn<'_>,
    sender: &mpsc::Sender<AgentEvent>,
) -> (String, Vec<String>) {
    let PostTurn {
        model,
        conversation,
        policies,
        detector,
        input,
        messages,
    } = ctx;

    let mut soft_errors = Vec::new();

    // 1. Topic state machine.
    let decision = detector.detect(input, conversation.topic().as_deref());
    conversation.set_topic(&decision.topic);

    let memory = conversation.memory();

    // 2. L1 write: the turn's messages, tagged with the topic.
    if let Err(e) = memory.l1_append(
        conversation.subject(),
        conversation.id(),
        &decision.topic,
        messages,
    ) {
        soft_errors.push(format!("l1 append: {e}"));
    }

    // 3. TopicShift policy: flush pre-shift turns into L2.
    let topic_shift_enabled = policies.extra.contains(&EventPolicy::TopicShift);
    if topic_shift_enabled && decision.shifted {
        let l1_len = memory
            .l1_len(conversation.subject(), conversation.id())
            .unwrap_or(0);
        if l1_len > 0 {
            let popped = memory
                .l1_pop_oldest(conversation.subject(), conversation.id(), l1_len)
                .unwrap_or_default();
            let summary = summarize_l1(model, &popped, sender).await;
            if let Err(e) = memory.l2_append(
                conversation.subject(),
                SummaryBlock {
                    conversation_id: conversation.id().to_string(),
                    content: summary,
                    index: 0,
                },
            ) {
                soft_errors.push(format!("l2 append (topic shift): {e}"));
            }
        }
    }

    // 4. TurnCount floor: L1 overflow demotes oldest turns into L2.
    let l1_len = memory
        .l1_len(conversation.subject(), conversation.id())
        .unwrap_or(0);
    if l1_len > policies.l1_window {
        let overflow = l1_len - policies.l1_window;
        let popped = memory
            .l1_pop_oldest(conversation.subject(), conversation.id(), overflow)
            .unwrap_or_default();
        let summary = summarize_l1(model, &popped, sender).await;
        if let Err(e) = memory.l2_append(
            conversation.subject(),
            SummaryBlock {
                conversation_id: conversation.id().to_string(),
                content: summary,
                index: 0,
            },
        ) {
            soft_errors.push(format!("l2 append (turn count): {e}"));
        }
    }

    // 5. L2Overflow floor: distill oldest summary blocks into L3.
    let l2_len = memory
        .l2_len(conversation.subject(), conversation.id())
        .unwrap_or(0);
    if l2_len > policies.l2_cap {
        let overflow = l2_len - policies.l2_cap;
        let popped = memory
            .l2_pop_oldest(conversation.subject(), conversation.id(), overflow)
            .unwrap_or_default();
        for block in popped {
            // Mechanical distillation: the summary becomes long-term
            // knowledge under the conversation's topic (no extra LLM call;
            // LLM-based extraction is a future pluggable improvement).
            let fragment = KnowledgeFragment {
                identity: crate::memory::FragmentIdentity {
                    subject_id: conversation.subject().to_string(),
                    conversation_id: block.conversation_id,
                    topic: decision.topic.clone(),
                },
                content: block.content,
                created_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            };
            if let Err(e) = memory.l3_upsert(conversation.subject(), fragment) {
                soft_errors.push(format!("l3 upsert: {e}"));
            }
        }
    }

    (decision.topic, soft_errors)
}

/// Runs the conversation-end flow: mechanical promotion of all L2 blocks
/// into L3 (the explicit `Conversation::end` path and the idle-timeout
/// fallback both land here). No model call is required.
pub(crate) fn run_end_flows(conversation: &Conversation, policies: &MemoryPolicies) -> Vec<String> {
    if !policies.extra.contains(&EventPolicy::ConversationEnd) {
        return Vec::new();
    }
    let memory = conversation.memory();
    let mut soft_errors = Vec::new();
    let l2_len = memory
        .l2_len(conversation.subject(), conversation.id())
        .unwrap_or(0);
    let blocks = memory
        .l2_pop_oldest(conversation.subject(), conversation.id(), l2_len)
        .unwrap_or_default();
    let topic = conversation.topic().unwrap_or_default();
    for block in blocks {
        let fragment = KnowledgeFragment {
            identity: crate::memory::FragmentIdentity {
                subject_id: conversation.subject().to_string(),
                conversation_id: block.conversation_id,
                topic: topic.clone(),
            },
            content: block.content,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        if let Err(e) = memory.l3_upsert(conversation.subject(), fragment) {
            soft_errors.push(format!("l3 upsert (end): {e}"));
        }
    }
    soft_errors
}

/// Summarizes demoted L1 turns into one summary block via the model,
/// emitting the call under `ContextManagement` (visible, not magic).
async fn summarize_l1(
    model: &dyn Model,
    entries: &[crate::memory::L1Entry],
    sender: &mpsc::Sender<AgentEvent>,
) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut transcript = String::new();
    for entry in entries {
        for message in &entry.messages {
            match message.role {
                Role::User => {
                    if let Some(text) = message_text(message) {
                        transcript.push_str("User: ");
                        transcript.push_str(&text);
                        transcript.push('\n');
                    }
                }
                Role::Assistant => {
                    if let Some(text) = message_text(message) {
                        transcript.push_str("Assistant: ");
                        transcript.push_str(&text);
                        transcript.push('\n');
                    }
                }
                _ => {}
            }
        }
    }
    let request = Message::user(format!(
        "Summarize the following conversation turns into one short paragraph, preserving key facts, decisions, and preferences:\n\n{transcript}"
    ));
    let emit_requested = AgentEvent::Model(ModelEvent::Requested {
        purpose: CallPurpose::ContextManagement,
        messages: vec![request.clone()],
    });
    let _ = sender.send(emit_requested).await;

    let request = ModelRequest::new(
        vec![request],
        Vec::new(),
        crate::model::ModelParams::default().with_max_tokens(256),
    );
    match crate::model::complete(model, request).await {
        Ok((message, usage)) => {
            let _ = sender
                .send(AgentEvent::Model(ModelEvent::Responded {
                    message: message.clone(),
                    usage,
                }))
                .await;
            message_text(&message).unwrap_or_default()
        }
        Err(_error) => {
            // Fallback: mechanical join so L2 never loses the content.
            transcript
        }
    }
}

fn message_text(message: &Message) -> Option<String> {
    let mut text = String::new();
    for block in &message.blocks {
        if let crate::ContentBlock::Text { text: fragment } = block {
            text.push_str(fragment);
        }
    }
    (!text.is_empty()).then_some(text)
}
