//! The read-side preprocessing extension: coreference resolution against
//! the conversation history the framework provides (the truth domain's
//! recent successful turns).

use std::sync::Arc;

use futures::future::BoxFuture;
use synonz::{
    MemoryFailure, Message, Model, ModelRequest, RewriteInput, RewriterProvider, Role,
    TurnInputRewriter, complete,
};

use crate::utils::message_text;

/// The component's input rewriter.
pub(crate) struct CoreferenceInputRewriter;

impl TurnInputRewriter for CoreferenceInputRewriter {
    fn rewrite<'a>(
        &'a self,
        input: RewriteInput<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, MemoryFailure>> {
        Box::pin(async move {
            if input.history.is_empty() {
                return Ok(None);
            }
            let mut history = String::new();
            for message in input.history {
                let role = match message.role {
                    Role::User => "User",
                    Role::Assistant => "Assistant",
                    _ => continue,
                };
                let text = message_text(message);
                if !text.is_empty() {
                    history.push_str(&format!("{role}: {text}\n"));
                }
            }
            let prompt = format!(
                "Resolve pronouns and references in the user's new message using the \
                 conversation history. Keep the message's language and intent. If nothing \
                 needs resolving, repeat the message unchanged.\n\nConversation history:\n\
                 {history}\nNew message:\n{message}\n\nReply with the resolved message only.",
                message = input.input
            );
            let request = ModelRequest::new(vec![Message::user(prompt)], Vec::new());
            let (message, _usage) = complete(&*input.model, request)
                .await
                .map_err(|error| MemoryFailure::new("rewrite", error.to_string()))?;
            let text = message_text(&message);
            if text.trim().is_empty() {
                return Err(MemoryFailure::new(
                    "rewrite",
                    "the model reply carried no rewritten input",
                ));
            }
            if text.trim() == input.input.trim() {
                return Ok(None);
            }
            Ok(Some(text))
        })
    }
}

/// The component's read-side preprocessing provider (register it on an
/// agent). Its model is the component's configured model; when the
/// component has none, the framework falls back to the agent model.
pub struct CoreferenceInputRewriterProvider {
    model: Option<Arc<dyn Model>>,
}

impl CoreferenceInputRewriterProvider {
    /// Builds the provider.
    pub(crate) fn new(model: Option<Arc<dyn Model>>) -> Self {
        Self { model }
    }
}

impl RewriterProvider for CoreferenceInputRewriterProvider {
    fn turn_input_rewriter(&self) -> Arc<dyn TurnInputRewriter> {
        Arc::new(CoreferenceInputRewriter)
    }

    fn model(&self) -> Option<Arc<dyn Model>> {
        self.model.clone()
    }
}
