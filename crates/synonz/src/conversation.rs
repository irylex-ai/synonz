//! The conversation entity: a multi-turn dialogue with identity.
//!
//! A `Conversation` is a *data entity* (ADR-0011): the aggregate of its
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

use crate::io::{AgentInput, AgentOutput};
use crate::message::Message;

static CONVERSATION_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    /// The final result snapshot (`output.message` is the last assistant
    /// message of `messages`; kept as a snapshot for O(1) access and type
    /// consistency across `Answer::await` / `Run::await` / `Completed`).
    pub output: AgentOutput,
}

impl Turn {
    /// Creates a turn from its parts.
    pub fn new(input: AgentInput, messages: Vec<Message>, output: AgentOutput) -> Self {
        Self {
            input,
            messages,
            output,
        }
    }
}

/// A multi-turn dialogue: a data entity with identity.
///
/// Cloning is cheap (shared storage) and produces an independent copy of
/// the turn history (useful for branching). Concurrent turns on the same
/// conversation are excluded by the borrow checker: constructing a turn
/// input borrows the conversation mutably for the duration of the turn.
pub struct Conversation {
    id: String,
    turns: Arc<Mutex<Vec<Turn>>>,
}

impl Clone for Conversation {
    /// Clones the handle (shared storage): the clone *is* the same
    /// conversation — this is how the execution task receives the
    /// conversation it must record into. For an independent copy, see
    /// [`Conversation::fork`].
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            turns: Arc::clone(&self.turns),
        }
    }
}

impl Conversation {
    /// Starts a new conversation with a generated id.
    ///
    /// The generated id is `conv-<timestamp>-<counter>`: unique within a
    /// process for practical purposes, not cryptographic.
    pub fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let counter = CONVERSATION_COUNTER.fetch_add(1, Ordering::Relaxed);
        Self::with_id(format!("conv-{nanos:x}-{counter:x}"))
    }

    /// Starts a conversation with an application-supplied id (ticket
    /// numbers, user session keys, ...).
    pub fn with_id(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            turns: Arc::new(Mutex::new(Vec::new())),
        }
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
    /// the borrow checker).
    pub fn turn_input<'a>(&'a mut self, text: impl Into<String>) -> TurnInput<'a> {
        TurnInput {
            input: AgentInput::new(text),
            conv: Some(self),
        }
    }

    /// Records a completed turn.
    ///
    /// In the normal flow this is called internally at the execution's
    /// epilogue (only completed turns are recorded; cancelled or failed
    /// runs leave the history untouched). Manual calls serve conversation
    /// construction (tests, imports, memory injection in S2c).
    pub fn push_turn(&self, turn: Turn) {
        self.turns
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(turn);
    }

    /// Forks an independent copy of the conversation (deep copy of the
    /// turn history at this moment) — the branching operation.
    pub fn fork(&self) -> Conversation {
        let turns = self.turns.lock().unwrap_or_else(|p| p.into_inner());
        Conversation {
            id: self.id.clone(),
            turns: Arc::new(Mutex::new(turns.clone())),
        }
    }

    /// Drops the last `n` completed turns.
    pub fn truncate_last(&self, n: usize) {
        let mut turns = self.turns.lock().unwrap_or_else(|p| p.into_inner());
        let new_len = turns.len().saturating_sub(n);
        turns.truncate(new_len);
    }

    /// Drops all turns.
    pub fn clear(&self) {
        self.turns.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// Serializes the conversation (JSON) for application-side storage.
    pub fn export(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Restores a conversation serialized with [`Conversation::export`].
    pub fn import(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Conversation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let turns = self.turns.lock().unwrap_or_else(|p| p.into_inner());
        f.debug_struct("Conversation")
            .field("id", &self.id)
            .field("turns", &*turns)
            .finish()
    }
}

impl Serialize for Conversation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Conversation", 2)?;
        state.serialize_field("id", &self.id)?;
        let turns = self.turns.lock().unwrap_or_else(|p| p.into_inner());
        state.serialize_field("turns", &*turns)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Conversation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Shape {
            id: String,
            turns: Vec<Turn>,
        }
        let shape = Shape::deserialize(deserializer)?;
        Ok(Self {
            id: shape.id,
            turns: Arc::new(Mutex::new(shape.turns)),
        })
    }
}

/// The per-turn input object: the question plus the conversation it belongs
/// to (ADR-0011 parameter object).
///
/// Constructed by [`Conversation::turn_input`]; `&str`, `String`, and
/// [`AgentInput`] convert into a conversation-less one-shot input.
pub struct TurnInput<'a> {
    input: AgentInput,
    conv: Option<&'a Conversation>,
}

impl<'a> TurnInput<'a> {
    pub(crate) fn into_parts(self) -> (AgentInput, Option<&'a Conversation>) {
        (self.input, self.conv)
    }
}

impl From<&str> for TurnInput<'static> {
    fn from(text: &str) -> Self {
        TurnInput {
            input: AgentInput::new(text),
            conv: None,
        }
    }
}

impl From<String> for TurnInput<'static> {
    fn from(text: String) -> Self {
        TurnInput {
            input: AgentInput::new(text),
            conv: None,
        }
    }
}

impl From<AgentInput> for TurnInput<'static> {
    fn from(input: AgentInput) -> Self {
        TurnInput { input, conv: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;

    fn text_turn(input: &str, answer: &str) -> Turn {
        Turn::new(
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
        let a = Conversation::new();
        let b = Conversation::new();
        assert_ne!(a.id(), b.id());
        assert!(a.id().starts_with("conv-"));
    }

    #[test]
    fn with_id_preserves_application_identity() {
        let conv = Conversation::with_id("user-42-ticket-7");
        assert_eq!(conv.id(), "user-42-ticket-7");
    }

    #[test]
    fn push_and_read_roundtrip() {
        let conv = Conversation::new();
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
    fn truncate_last_drops_whole_turns() {
        let conv = Conversation::new();
        conv.push_turn(text_turn("a", "A"));
        conv.push_turn(text_turn("b", "B"));
        conv.push_turn(text_turn("c", "C"));
        conv.truncate_last(2);
        assert_eq!(conv.len(), 1);
        assert_eq!(conv.turns()[0].input.text, "a");
    }

    #[test]
    fn export_import_roundtrip() {
        let conv = Conversation::with_id("keep-me");
        conv.push_turn(text_turn("a", "A"));
        conv.push_turn(text_turn("b", "B"));
        let bytes = conv.export().expect("export");
        let restored = Conversation::import(&bytes).expect("import");
        assert_eq!(restored.id(), "keep-me");
        assert_eq!(restored.turns().len(), 2);
        assert_eq!(restored.messages(), conv.messages());
    }

    #[test]
    fn turn_input_serializes_by_borrow() {
        let mut conv = Conversation::new();
        let _turn = conv.turn_input("first");
        // Compile-time check: a second borrow cannot start while the first
        // turn input is alive. This test documents the borrow discipline.
        assert!(conv.is_empty());
    }
}
