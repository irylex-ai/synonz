//! Deterministic test doubles for agent testing (feature `test-util`).
//!
//! [`MockModel`] plays scripted model responses per call, records the
//! requests it received, and can hang forever (for cancellation tests).
//! Because the script drives the loop deterministically, agent behavior
//! tests need no network and no real provider.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

use futures::StreamExt;
use futures::future::BoxFuture;

use crate::error::ModelError;
use crate::event::TokenUsage;
use crate::message::Message;
use crate::model::{Model, ModelRequest, ModelStream, ModelStreamItem};

#[derive(Debug, Clone)]
enum Script {
    /// Yields the given items, then ends.
    Items(Vec<ModelStreamItem>),
    /// Never yields and never finishes (for cancellation tests).
    Hang,
}

/// A scripted [`Model`] for tests.
///
/// Each `stream` call consumes the next script entry in order. When the
/// scripts are exhausted, the model yields an empty stream (a premature
/// end, which the loop surfaces as an error) — tests should script exactly
/// the rounds they expect.
#[derive(Debug, Clone, Default)]
pub struct MockModel {
    scripts: Arc<Mutex<VecDeque<Script>>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl MockModel {
    /// Creates a model playing the given scripts, one per call.
    pub fn new(scripts: Vec<Vec<ModelStreamItem>>) -> Self {
        Self {
            scripts: Arc::new(Mutex::new(scripts.into_iter().map(Script::Items).collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A single-round model that answers with one text message.
    pub fn finishing_with_text(text: impl Into<String>) -> Self {
        Self::new(vec![vec![ModelStreamItem::Finish {
            message: Message::assistant_text(text),
            usage: TokenUsage::new(1, 1),
        }]])
    }

    /// A model whose stream never yields and never finishes.
    pub fn hanging() -> Self {
        Self {
            scripts: Arc::new(Mutex::new(VecDeque::from([Script::Hang]))),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// How many calls this model has received.
    pub fn calls(&self) -> usize {
        self.requests
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }

    /// Snapshots the requests this model has received (canonical messages),
    /// in call order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl Model for MockModel {
    fn stream(&self, request: ModelRequest) -> BoxFuture<'_, Result<ModelStream, ModelError>> {
        self.requests
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(request);
        let script = self
            .scripts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front();
        Box::pin(async move {
            match script {
                Some(Script::Items(items)) => Ok(futures::stream::iter(items).boxed()),
                Some(Script::Hang) => Ok(futures::stream::once(std::future::pending()).boxed()),
                None => Ok(futures::stream::empty().boxed()),
            }
        })
    }
}
