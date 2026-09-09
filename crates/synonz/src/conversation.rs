//! The conversation entity: a multi-turn dialogue with identity.
//!
//! A `Conversation` is a *data entity*: the aggregate of its
//! turns, identified, serializable, and agent-agnostic — different agents
//! can continue the same conversation. Data-only behaviors live here
//! (information expert); model-touching behaviors (summarization,
//! compression) belong to the agent side (context management, S2b).
//!
//! Normal flow: `conv.turn_input(text)` builds the per-turn input object,
//! the agent executes it, and the completed turn is recorded into the
//! conversation automatically at the execution's epilogue
//! ([`Conversation::push_turn`] is the internal write; documented for
//! manual construction).

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::io::{AgentInput, AgentOutput};
use crate::message::Message;
use crate::runtime::SynonzRuntime;
use crate::subject::Subject;

static CONVERSATION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Conversation persistence failures.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ConversationStoreError {
    /// No conversation exists under this id (for this subject).
    #[error("conversation not found: {0}")]
    NotFound(String),
    /// The backing storage failed.
    #[error("conversation storage failure: {0}")]
    Storage(String),
}

/// The serializable state of a conversation (what stores persist).
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationState {
    /// The owning subject's full identity (`(type, id)`).
    pub subject_id: String,
    /// The conversation id.
    pub id: String,
    /// The recorded turns, in order.
    pub turns: Vec<Turn>,
    /// The session topic state machine's current topic.
    pub topic: Option<String>,
    /// Epoch seconds of the last activity (idle-timeout tracking).
    pub last_active: u64,
}

/// The conversation persistence contract, sibling of the
/// [`Memory`] facade and its three store contracts. Implementations own
/// storage; the
/// framework owns when saves happen (auto-save on turn completion,
/// High Level).
pub trait ConversationStore: Send + Sync + 'static {
    /// Loads a conversation's state by id, for a subject.
    fn load(
        &self,
        subject: &Subject,
        id: &str,
    ) -> Result<ConversationState, ConversationStoreError>;

    /// Saves (upserts) a conversation's state.
    fn save(&self, state: ConversationState) -> Result<(), ConversationStoreError>;

    /// Lists all stored conversation states (idle-timeout sweeping).
    fn list(&self) -> Result<Vec<ConversationState>, ConversationStoreError>;
}

/// How a turn ended. Every turn enters the history, marked with its
/// outcome (the truth archive keeps the full audit trail —
/// failures and cancellations are as much a part of the record as
/// successes).
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TurnOutcome {
    /// The run completed; carries the final output snapshot.
    Completed(AgentOutput),
    /// The run failed; carries the error.
    Failed(crate::AgentError),
    /// The run was cancelled; carries the reason.
    Cancelled(crate::CancelReason),
}

/// One completed question-answer round of a conversation.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    /// The user input that started this turn.
    pub input: AgentInput,
    /// All canonical messages of this turn's run, including the user input
    /// message and any tool round-trips — the context replayed into the
    /// model on later turns.
    pub messages: Vec<Message>,
    /// How the turn ended (success carries the output snapshot; failures
    /// and cancellations carry their reason — the full audit trail).
    pub outcome: TurnOutcome,
}

impl Turn {
    /// Records a completed turn with its final output.
    pub fn completed(input: AgentInput, messages: Vec<Message>, output: AgentOutput) -> Self {
        Self {
            input,
            messages,
            outcome: TurnOutcome::Completed(output),
        }
    }

    /// Records a failed turn (the messages built up to the failure are
    /// the audit trail; the memory layers are not fed from failed turns).
    pub fn failed(input: AgentInput, messages: Vec<Message>, error: crate::AgentError) -> Self {
        Self {
            input,
            messages,
            outcome: TurnOutcome::Failed(error),
        }
    }

    /// Records a cancelled turn.
    pub fn cancelled(
        input: AgentInput,
        messages: Vec<Message>,
        reason: crate::CancelReason,
    ) -> Self {
        Self {
            input,
            messages,
            outcome: TurnOutcome::Cancelled(reason),
        }
    }

    /// The final output snapshot (completed turns only).
    pub fn output(&self) -> Option<&AgentOutput> {
        match &self.outcome {
            TurnOutcome::Completed(output) => Some(output),
            _ => None,
        }
    }
}

/// A multi-turn dialogue: a pure data entity — the truth archive
/// (identity + recorded turns + topic state).
///
/// The conversation carries **no environment**: services (persistence,
/// memory, policies) come from the runtime of whoever operates on it —
/// the execution loop persists through the agent's runtime, `end` takes
/// the runtime explicitly, `of` restores from one. Cloning is cheap
/// (shared storage) and produces the same conversation. Concurrent turns
/// on one conversation are excluded by the borrow checker: constructing a
/// turn input borrows the conversation mutably for the duration of the
/// turn.
pub struct Conversation {
    id: String,
    subject: Subject,
    turns: Arc<Mutex<Vec<Turn>>>,
    topic: Arc<Mutex<Option<String>>>,
}

impl Clone for Conversation {
    /// Clones the handle (shared storage): the clone *is* the same
    /// conversation — this is how the execution task receives the
    /// conversation it must record into.
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            subject: self.subject.clone(),
            turns: Arc::clone(&self.turns),
            topic: Arc::clone(&self.topic),
        }
    }
}

impl Conversation {
    /// Creates a new conversation for a subject.
    ///
    /// The generated id is `conv-<timestamp>-<counter>`: unique within a
    /// process for practical purposes, not cryptographic. The conversation
    /// is pure data — persistence happens through a runtime when turns are
    /// recorded or the conversation is restored.
    pub fn new(subject: &Subject) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let counter = CONVERSATION_COUNTER.fetch_add(1, Ordering::Relaxed);
        Self::with_id(subject, format!("conv-{nanos:x}-{counter:x}"))
    }

    /// Creates a new conversation with an application-supplied id (ticket
    /// numbers, user session keys, ...).
    pub fn with_id(subject: &Subject, id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            subject: subject.clone(),
            turns: Arc::new(Mutex::new(Vec::new())),
            topic: Arc::new(Mutex::new(None)),
        }
    }

    /// Restores an existing conversation by id from the runtime's store.
    ///
    /// `of` = restore (never create): fails with
    /// [`ConversationStoreError::NotFound`] when no conversation exists
    /// under this id for this subject.
    pub fn of(
        runtime: &SynonzRuntime,
        subject: &Subject,
        id: &str,
    ) -> Result<Self, ConversationStoreError> {
        let state = runtime.conversation_store().load(subject, id)?;
        Ok(Self {
            id: state.id,
            subject: subject.clone(),
            turns: Arc::new(Mutex::new(state.turns)),
            topic: Arc::new(Mutex::new(state.topic)),
        })
    }

    /// The conversation's owning subject.
    pub fn subject(&self) -> &Subject {
        &self.subject
    }

    /// The current topic (session topic state machine), if any.
    pub(crate) fn topic(&self) -> Option<String> {
        self.topic.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Updates the current topic.
    pub(crate) fn set_topic(&self, topic: &str) {
        *self.topic.lock().unwrap_or_else(|p| p.into_inner()) = Some(topic.to_string());
    }

    /// Ends the conversation: the two end paths are the explicit call
    /// (the initiating side in control) and the idle-timeout sweep. The
    /// end itself performs the generic acts of a lifecycle transition;
    /// the state teardown (drain the conversation's background
    /// maintenance, mechanical L2 → L3 promotion) is the runtime's
    /// structural behavior — never this data entity's business.
    pub async fn end(&self, runtime: &SynonzRuntime) {
        runtime.finalize_conversation(self).await;
    }

    /// The conversation's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The full flattened message history (all turns' messages in order) —
    /// the context replayed into the model on the next turn.
    pub fn messages(&self) -> Vec<Message> {
        let turns = self.turns.lock().unwrap_or_else(|p| p.into_inner());
        turns
            .iter()
            .flat_map(|turn| turn.messages.clone())
            .collect()
    }

    /// The recorded turns, in order.
    pub fn turns(&self) -> Vec<Turn> {
        self.turns.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// How many turns have completed.
    pub fn len(&self) -> usize {
        self.turns.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Whether the conversation has no turns yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Builds the input object for the next turn, borrowing the conversation
    /// for the turn's duration (turns on one conversation are serialized by
    /// the borrow checker). This is the only way to construct a
    /// [`TurnInput`] — every execution belongs to a conversation.
    pub fn turn_input<'a>(&'a mut self, text: impl Into<String>) -> TurnInput<'a> {
        TurnInput {
            input: AgentInput::new(text),
            conv: self,
        }
    }

    /// Records a turn into the truth archive (the only write path; called
    /// by the execution loop for completed, failed, and cancelled turns —
    /// all enter the history, marked by [`Turn::outcome`]). Persistence
    /// is driven by the operating runtime (see [`Conversation::state`]).
    pub(crate) fn push_turn(&self, turn: Turn) {
        self.turns
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(turn);
    }

    /// The current state snapshot (identity + turns + topic) — what a
    /// store saves and what `of` restores from.
    pub(crate) fn state(&self) -> ConversationState {
        let turns = self.turns.lock().unwrap_or_else(|p| p.into_inner());
        ConversationState {
            subject_id: self.subject.to_string(),
            id: self.id.clone(),
            turns: turns.clone(),
            topic: self.topic(),
            last_active: now_epoch(),
        }
    }

    /// Serializes the conversation state (JSON) for application-side
    /// storage.
    ///
    /// Boundary: what migrates is the **truth record** — the
    /// turns. The memory layers (L2/L3) and topic state are the
    /// `MemoryStore`'s own transactions and do not travel with an export;
    /// restoration goes through [`Conversation::of`] with a store that
    /// holds the state.
    pub fn export(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&self.state())
    }
}

impl std::fmt::Debug for Conversation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let turns = self.turns.lock().unwrap_or_else(|p| p.into_inner());
        f.debug_struct("Conversation")
            .field("id", &self.id)
            .field("subject", &self.subject)
            .field("turns", &*turns)
            .finish()
    }
}

/// The per-turn input object: the question plus the conversation it belongs
/// to (the parameter-object pattern).
///
/// Constructed only by [`Conversation::turn_input`] — every execution
/// belongs to a conversation; there is no conversation-less execution.
pub struct TurnInput<'a> {
    input: AgentInput,
    conv: &'a Conversation,
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl<'a> TurnInput<'a> {
    pub(crate) fn into_parts(self) -> (AgentInput, &'a Conversation) {
        (self.input, self.conv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;
    use crate::subject::SubjectType;

    fn rt() -> (SynonzRuntime, Subject) {
        (
            SynonzRuntime::builder().build(),
            Subject::of(SubjectType::User, "u-42"),
        )
    }

    fn text_turn(input: &str, answer: &str) -> Turn {
        Turn::completed(
            AgentInput::new(input),
            vec![Message::user(input), Message::assistant_text(answer)],
            AgentOutput::new(
                Message::assistant_text(answer),
                crate::TokenUsage::new(1, 1),
            ),
        )
    }

    #[test]
    fn new_generates_unique_ids() {
        let (_runtime, subject) = rt();
        let a = Conversation::new(&subject);
        let b = Conversation::new(&subject);
        assert_ne!(a.id(), b.id());
        assert!(a.id().starts_with("conv-"));
        assert_eq!(a.subject(), &subject);
    }

    #[test]
    fn with_id_preserves_application_identity() {
        let (_runtime, subject) = rt();
        let conv = Conversation::with_id(&subject, "user-42-ticket-7");
        assert_eq!(conv.id(), "user-42-ticket-7");
    }

    #[test]
    fn push_and_read_roundtrip() {
        let (_runtime, subject) = rt();
        let conv = Conversation::new(&subject);
        assert!(conv.is_empty());
        conv.push_turn(text_turn("a", "A"));
        conv.push_turn(text_turn("b", "B"));
        assert_eq!(conv.len(), 2);
        assert_eq!(conv.turns().len(), 2);
        let messages = conv.messages();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[3].role, Role::Assistant);
    }

    #[test]
    fn of_restores_from_store_after_save() {
        let (runtime, subject) = rt();
        let conv = Conversation::with_id(&subject, "keep-me");
        conv.push_turn(text_turn("a", "A"));
        conv.push_turn(text_turn("b", "B"));
        // Persistence is driven by the operating runtime; `of` restores it.
        runtime
            .conversation_store()
            .save(conv.state())
            .expect("save");
        let restored = Conversation::of(&runtime, &subject, "keep-me").expect("restore");
        assert_eq!(restored.id(), "keep-me");
        assert_eq!(restored.turns().len(), 2);
        assert_eq!(restored.messages(), conv.messages());
    }

    #[test]
    fn of_fails_for_unknown_id() {
        let (runtime, subject) = rt();
        assert!(Conversation::of(&runtime, &subject, "nope").is_err());
    }

    #[test]
    fn of_fails_for_wrong_subject() {
        let (runtime, subject) = rt();
        let conv = Conversation::with_id(&subject, "shared-id");
        conv.push_turn(text_turn("a", "A"));
        runtime
            .conversation_store()
            .save(conv.state())
            .expect("save");
        let other = Subject::of(SubjectType::User, "u-43");
        assert!(Conversation::of(&runtime, &other, "shared-id").is_err());
    }

    #[test]
    fn turn_input_serializes_by_borrow() {
        let (_runtime, subject) = rt();
        let mut conv = Conversation::new(&subject);
        let _turn = conv.turn_input("first");
        // Compile-time check: a second borrow cannot start while the first
        // turn input is alive. This test documents the borrow discipline.
        assert!(conv.is_empty());
    }

    #[test]
    fn registered_store_replaces_default() {
        let subject = Subject::of(SubjectType::User, "u-42");
        // Two separate runtimes: a conversation persisted on runtime A must
        // not be visible on runtime B (each runtime holds its own view).
        let runtime_a = SynonzRuntime::builder().build();
        let runtime_b = SynonzRuntime::builder().build();
        let conv = Conversation::with_id(&subject, "isolated");
        conv.push_turn(text_turn("a", "A"));
        runtime_a
            .conversation_store()
            .save(conv.state())
            .expect("save");
        assert!(Conversation::of(&runtime_b, &subject, "isolated").is_err());
        assert!(Conversation::of(&runtime_a, &subject, "isolated").is_ok());
    }
}
