//! Internal helpers shared across the component's files.

use synonz::{ContentBlock, MemoryScope, Message, Role};

/// The concatenated text of one message.
pub(crate) fn message_text(message: &Message) -> String {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The final answer of a turn: the last assistant message's text (the only
/// part of the agent side that becomes memory; tool results and
/// intermediate messages stay in the truth archive).
pub(crate) fn final_answer(responses: &[Message]) -> String {
    responses
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .map(message_text)
        .unwrap_or_default()
}

/// The leading segment of a text: the first sentence (or the whole text
/// when it has no sentence break).
pub(crate) fn first_segment(text: &str) -> String {
    let trimmed = text.trim();
    let end = trimmed
        .find(['。', '！', '？', '!', '?', '\n'])
        .or_else(|| trimmed.find(". "))
        .unwrap_or(trimmed.len());
    trimmed[..end].trim().to_string()
}

/// Whether an input is too trivial to shift the conversation topic: very
/// short messages and bare confirmations.
pub(crate) fn is_trivial(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.chars().count() < 4 {
        return true;
    }
    const CONFIRMATIONS: [&str; 12] = [
        "yes",
        "yep",
        "yeah",
        "ok",
        "okay",
        "sure",
        "thanks",
        "thank you",
        "好的",
        "嗯",
        "是的",
        "收到",
    ];
    let lowered = trimmed.to_lowercase();
    CONFIRMATIONS.contains(&lowered.as_str())
}

/// Cosine similarity (0 when either vector is empty or their norms are
/// zero).
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

/// The current epoch in seconds.
pub(crate) fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// The conversation partition of one conversation (the component's
/// convention).
pub(crate) fn conversation_scope(conversation_id: &str) -> MemoryScope {
    MemoryScope::new(format!("conversation:{conversation_id}"))
}

/// The conversation id behind a conversation partition, when the scope
/// follows the component's convention.
pub(crate) fn conversation_id_of(scope: &MemoryScope) -> Option<&str> {
    scope.as_str().strip_prefix("conversation:")
}
