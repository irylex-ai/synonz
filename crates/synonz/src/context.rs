//! The context engine: the narrative background of a conversation
//! (ADR-0012 决策一/八/九).
//!
//! `Context` is the third persistent object — the session-scoped runtime
//! that lets the stateless agent execute with state. Created from a
//! conversation ([`Conversation::context`]), it holds the memory handles
//! and the assembly strategy. Assembly is fresh per ask (never once at
//! creation): the strategy composes what is *sent*, nothing else.

use std::sync::Arc;

use futures::future::BoxFuture;

use crate::conversation::Conversation;
use crate::message::Message;

/// The inputs an assembly strategy consumes: the conversation (history,
/// memory handles, subject) and the turn's user input.
#[non_exhaustive]
pub struct AssemblyRequest<'a> {
    /// The conversation of this turn.
    pub conversation: &'a Conversation,
    /// The turn's user input text.
    pub input: &'a str,
}

/// Assembly failures.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AssemblyError {
    /// The strategy failed to produce a context.
    #[error("context assembly failed: {0}")]
    Failed(String),
}

/// The context assembly strategy contract.
///
/// Strategies decide **what is sent** (the composition of L1/L2/L3/history
/// and their formatting); timing, turn recording, and write flows stay in
/// the framework. Everything is a strategy: built-in [`LayeredMemory`]
/// (default) or [`ConversationHistory`] (the pre-memory behavior), or a
/// developer's own.
pub trait ContextAssembly: Send + Sync + 'static {
    /// Assembles the context messages for one turn.
    fn assemble<'a>(
        &'a self,
        request: AssemblyRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<Message>, AssemblyError>>;
}

/// The session-scoped runtime (narrative background engine).
///
/// Cloning shares the same background. Created by
/// [`Conversation::context`].
#[derive(Clone)]
pub struct Context {
    conversation: Conversation,
    assembly: Arc<dyn ContextAssembly>,
}

impl Context {
    pub(crate) fn for_conversation(conversation: &Conversation) -> Self {
        Self {
            conversation: conversation.clone(),
            assembly: conversation.assembly(),
        }
    }

    /// Assembles the context for one turn (fresh — the memory layers are
    /// read at assembly time, so just-completed turns are included).
    pub async fn assemble(&self, input: &str) -> Result<Vec<Message>, AssemblyError> {
        self.assembly
            .assemble(AssemblyRequest {
                conversation: &self.conversation,
                input,
            })
            .await
    }
}

/// The L3 retrieval budget for the default strategy.
pub const DEFAULT_L3_BUDGET: usize = 3;

/// The default strategy: layered assembly
/// (L3 recall → L2 summaries → L1 window). Memory is the sole source;
/// the conversation is not read directly.
#[derive(Default)]
pub struct LayeredMemory;

impl ContextAssembly for LayeredMemory {
    fn assemble<'a>(
        &'a self,
        request: AssemblyRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<Message>, AssemblyError>> {
        Box::pin(async move {
            let conversation = request.conversation;
            let memory = conversation.memory();
            let subject = conversation.subject();
            let conversation_id = conversation.id();
            let topic = conversation.topic().unwrap_or_default();
            let mut messages = Vec::new();

            // L3 recall: independent System message (persona/memory
            // separation, P2).
            let l3 = memory
                .l3_retrieve(subject, request.input, &topic, DEFAULT_L3_BUDGET)
                .unwrap_or_default();
            if !l3.is_empty() {
                let recall = l3
                    .iter()
                    .map(|fragment| format!("- {}", fragment.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                messages.push(Message::system(format!("Memory recall:\n{recall}")));
            }

            // L2: this conversation's earlier turns, summarized.
            let l2 = memory.l2_read(subject, conversation_id).unwrap_or_default();
            for block in l2 {
                messages.push(Message::system(format!(
                    "Earlier in this conversation:\n{}",
                    block.content
                )));
            }

            // L1: this conversation's recent turns, verbatim, in order.
            let l1 = memory
                .l1_window(subject, conversation_id)
                .unwrap_or_default();
            for entry in l1 {
                messages.extend(entry.messages);
            }

            Ok(messages)
        })
    }
}

/// The pre-memory behavior as a built-in strategy: the conversation's
/// full history, verbatim (v1 behavior, ADR-0012 决策八收编).
#[derive(Default)]
pub struct ConversationHistory;

impl ContextAssembly for ConversationHistory {
    fn assemble<'a>(
        &'a self,
        request: AssemblyRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<Message>, AssemblyError>> {
        // The loop appends the user input itself; strategies return the
        // context only.
        let _ = request.input;
        Box::pin(async move { Ok(request.conversation.messages()) })
    }
}
