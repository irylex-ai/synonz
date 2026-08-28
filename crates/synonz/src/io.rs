//! Run boundary types: what goes into a run and what comes out.

use crate::event::TokenUsage;
use crate::message::{ContentBlock, Message};
use serde::{Deserialize, Serialize};

/// The input that initiates a run.
///
/// v1 carries the user message text. The type is extensible so future
/// scenarios can add structured payload without breaking consumers.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInput {
    /// The user message text.
    pub text: String,
}

impl AgentInput {
    /// Creates an input from text.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl From<&str> for AgentInput {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for AgentInput {
    fn from(text: String) -> Self {
        Self { text }
    }
}

/// The final output of a completed run.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentOutput {
    /// The final assistant message, with complete content blocks.
    pub message: Message,
    /// Token usage accumulated over the whole run.
    pub usage: TokenUsage,
}

impl AgentOutput {
    /// Creates an output from the final message and accumulated usage.
    pub fn new(message: Message, usage: TokenUsage) -> Self {
        Self { message, usage }
    }

    /// Convenience view of the final text.
    ///
    /// Returns the text of the first [`ContentBlock::Text`] in the final
    /// message, or `None` when the message contains no text block (for
    /// example, when it only carries tool calls).
    pub fn text(&self) -> Option<&str> {
        self.message.blocks.iter().find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_returns_first_text_block() {
        let output = AgentOutput {
            message: Message::assistant_text("done."),
            usage: TokenUsage::new(10, 5),
        };
        assert_eq!(output.text(), Some("done."));
    }

    #[test]
    fn text_is_none_without_text_blocks() {
        let output = AgentOutput {
            message: Message::new(crate::message::Role::Assistant, vec![]),
            usage: TokenUsage::new(0, 0),
        };
        assert_eq!(output.text(), None);
    }
}
