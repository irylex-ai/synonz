//! The agent: a canned composition of model, tools, system prompt, and the
//! reasoning loop.
//!
//! An `Agent` is a *stateless, immutable configuration* (model + tools +
//! system prompt + round budget). All run state lives inside a run; one
//! agent can drive many concurrent runs without shared state.
//!
//! # Interaction
//!
//! The dual-layer API: [`Agent::run`] returns a [`RunStream`] of
//! [`AgentEvent`]s (the complete, observable narrative; dropping the stream
//! cancels the run), and [`Agent::ask`] is the convenience shell built on
//! that stream.
//!
//! ```no_run
//! use futures::StreamExt;
//! use synonz::{Agent, AgentEvent, LifecycleEvent, Model, ModelRequest};
//!
//! struct EchoModel;
//!
//! impl Model for EchoModel {
//!     fn stream(&self, _request: ModelRequest)
//!         -> futures::future::BoxFuture<'_, Result<synonz::ModelStream, synonz::ModelError>>
//!     {
//!         Box::pin(async move {
//!             let finish = synonz::ModelStreamItem::Finish {
//!                 message: synonz::Message::assistant_text("hello!"),
//!                 usage: synonz::TokenUsage::new(1, 1),
//!             };
//!             Ok(futures::stream::iter(vec![finish]).boxed())
//!         })
//!     }
//! }
//!
//! # async fn demo() {
//! let agent = Agent::builder()
//!     .model(EchoModel)
//!     .system_prompt("be friendly")
//!     .build()
//!     .expect("model is set");
//!
//! let mut run = agent.run("hi");
//! while let Some(event) = run.next().await {
//!     if let AgentEvent::Lifecycle(LifecycleEvent::Completed { response }) = event {
//!         assert_eq!(response.text(), Some("hello!"));
//!     }
//! }
//! # }
//! ```

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::Stream;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::CancellationToken;
use crate::cancel::{CancelCore, CancelHandle, CancelOutcome};
use crate::error::{AgentError, ModelError};
use crate::event::{
    AgentEvent, CallPurpose, CancelReason, LifecycleEvent, ModelEvent, TokenUsage, ToolEvent,
};
use crate::io::{AgentInput, AgentOutput};
use crate::message::{CallId, ContentBlock, Message, ToolCall, ToolResult};
use crate::model::{Model, ModelParams, ModelRequest, ModelStreamItem};
use crate::tool::{Tool, ToolContext, ToolSpec};

/// Default round budget: how many reasoning rounds a run may use before it
/// fails with [`AgentError::MaxRoundsExceeded`]. Explicitly configurable via
/// [`AgentBuilder::max_rounds`].
pub const DEFAULT_MAX_ROUNDS: u32 = 16;

/// Builder for [`Agent`].
///
/// All configuration is explicit; nothing is defaulted silently except the
/// documented [`DEFAULT_MAX_ROUNDS`] budget.
#[derive(Default)]
pub struct AgentBuilder {
    model: Option<Arc<dyn Model>>,
    tools: Vec<Arc<dyn Tool>>,
    system_prompt: Option<String>,
    max_rounds: Option<u32>,
}

impl AgentBuilder {
    /// Starts building an agent.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the model (required). Accepts a concrete model or an
    /// `Arc<dyn Model>`.
    pub fn model<M: Model + 'static>(mut self, model: M) -> Self {
        self.model = Some(Arc::new(model));
        self
    }

    /// Sets the system prompt.
    ///
    /// When set, every run of this agent starts with this message as its
    /// first system message. When unset, no system message is sent — there
    /// is no hidden default prompt.
    pub fn system_prompt(mut self, text: impl Into<String>) -> Self {
        self.system_prompt = Some(text.into());
        self
    }

    /// Registers one tool.
    pub fn tool<T: Tool + 'static>(mut self, tool: T) -> Self {
        self.tools.push(Arc::new(tool));
        self
    }

    /// Registers multiple tools.
    pub fn tools<I, T>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Tool + 'static,
    {
        self.tools
            .extend(tools.into_iter().map(|t| Arc::new(t) as Arc<dyn Tool>));
        self
    }

    /// Sets the round budget (default: [`DEFAULT_MAX_ROUNDS`]). A run that
    /// exceeds the budget fails with [`AgentError::MaxRoundsExceeded`] —
    /// budget exhaustion is an explicit failure, never a silent truncation.
    pub fn max_rounds(mut self, max_rounds: u32) -> Self {
        self.max_rounds = Some(max_rounds);
        self
    }

    /// Assembles the agent.
    ///
    /// Fails with [`AgentError::InvalidConfiguration`] when no model was
    /// set.
    pub fn build(self) -> Result<Agent, AgentError> {
        let model = self.model.ok_or_else(|| AgentError::InvalidConfiguration {
            message: "a model is required".into(),
        })?;
        Ok(Agent {
            model,
            tools: self.tools.into(),
            system_prompt: self.system_prompt,
            max_rounds: self.max_rounds.unwrap_or(DEFAULT_MAX_ROUNDS),
        })
    }
}

/// A stateless agent configuration: model + tools + system prompt + budget.
///
/// Cloning is cheap (shared arcs). All run state lives inside a run; the
/// same agent can drive many concurrent runs independently.
#[derive(Clone)]
pub struct Agent {
    model: Arc<dyn Model>,
    tools: Arc<[Arc<dyn Tool>]>,
    system_prompt: Option<String>,
    max_rounds: u32,
}

impl Agent {
    /// Starts building an agent.
    pub fn builder() -> AgentBuilder {
        AgentBuilder::new()
    }

    /// Runs the agent and returns the event stream.
    ///
    /// Dropping the returned stream cancels the run (cooperative
    /// interruption at the loop's await points).
    pub fn run(&self, input: impl Into<AgentInput>) -> RunStream {
        let core = CancelCore::new();
        self.spawn_run(input.into(), core)
    }

    /// Runs the agent with an externally owned cancellation token: when the
    /// token fires, the run cancels with [`CancelReason::UserRequested`].
    pub fn run_with(&self, input: impl Into<AgentInput>, token: CancellationToken) -> RunStream {
        let core = CancelCore::child_of(&token);
        self.spawn_run(input.into(), core)
    }

    /// Convenience: runs to completion and returns the final output.
    ///
    /// Maps the terminal event: `Completed` to `Ok`, `Failed` and
    /// `Cancelled` to `Err`.
    pub async fn ask(&self, input: impl Into<AgentInput>) -> Result<AgentOutput, AgentError> {
        let mut stream = self.run(input);
        while let Some(event) = stream.next().await {
            match event {
                AgentEvent::Lifecycle(LifecycleEvent::Completed { response }) => {
                    return Ok(response);
                }
                AgentEvent::Lifecycle(LifecycleEvent::Failed { error }) => return Err(error),
                AgentEvent::Lifecycle(LifecycleEvent::Cancelled { reason }) => {
                    return Err(AgentError::Cancelled(reason));
                }
                _ => {}
            }
        }
        // Internal invariant: the loop always emits a terminal event before
        // closing the stream. Reaching this point means that invariant is
        // broken (a programmer error, not a runtime failure).
        unreachable!("run loop always emits a terminal event");
    }

    fn spawn_run(&self, input: AgentInput, core: Arc<CancelCore>) -> RunStream {
        let (sender, receiver) = mpsc::channel(1);
        let task = LoopTask {
            model: Arc::clone(&self.model),
            tools: Arc::clone(&self.tools),
            system_prompt: self.system_prompt.clone(),
            max_rounds: self.max_rounds,
        };
        tokio::spawn(task.execute(input, Arc::clone(&core), sender));
        RunStream {
            receiver,
            handle: CancelHandle::new(core),
            rounds_seen: 0,
        }
    }
}

/// The observable narrative of one run.
///
/// Yields [`AgentEvent`]s in order; the last event is always a terminal
/// lifecycle event, and the stream closes after it. Dropping the stream
/// cancels the run.
pub struct RunStream {
    receiver: mpsc::Receiver<AgentEvent>,
    handle: CancelHandle,
    rounds_seen: usize,
}

impl RunStream {
    /// Receives the next event, or `None` after the stream closes.
    pub async fn next(&mut self) -> Option<AgentEvent> {
        let event = self.receiver.recv().await?;
        if matches!(
            &event,
            AgentEvent::Model(ModelEvent::Requested {
                purpose: CallPurpose::Reasoning,
                ..
            })
        ) {
            self.rounds_seen += 1;
        }
        Some(event)
    }

    /// Arms the run's time budget. When it elapses first, the run cancels
    /// with [`CancelReason::Timeout`].
    pub fn with_timeout(self, duration: Duration) -> Self {
        self.handle.arm_timeout(duration);
        self
    }

    /// The number of reasoning rounds observed so far (derived from consumed
    /// events — rounds are never stored as events).
    pub fn rounds(&self) -> usize {
        self.rounds_seen
    }
}

impl Stream for RunStream {
    type Item = AgentEvent;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<AgentEvent>> {
        use std::task::Poll;
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(event)) => {
                if matches!(
                    &event,
                    AgentEvent::Model(ModelEvent::Requested {
                        purpose: CallPurpose::Reasoning,
                        ..
                    })
                ) {
                    self.rounds_seen += 1;
                }
                Poll::Ready(Some(event))
            }
            other => other,
        }
    }
}

/// The per-run task: everything the loop needs, all owned.
struct LoopTask {
    model: Arc<dyn Model>,
    tools: Arc<[Arc<dyn Tool>]>,
    system_prompt: Option<String>,
    max_rounds: u32,
}

impl LoopTask {
    async fn execute(
        self,
        input: AgentInput,
        core: Arc<CancelCore>,
        sender: mpsc::Sender<AgentEvent>,
    ) {
        // Emit helper: when the consumer is gone the run simply ends.
        macro_rules! emit {
            ($event:expr) => {
                if sender.send($event).await.is_err() {
                    return; // consumer dropped; nothing left to narrate
                }
            };
        }

        let mut total_usage = TokenUsage::new(0, 0);

        emit!(AgentEvent::Lifecycle(LifecycleEvent::Started {
            input: input.clone(),
        }));

        let mut messages = Vec::new();
        if let Some(prompt) = &self.system_prompt {
            messages.push(Message::system(prompt.clone()));
        }
        messages.push(Message::user(input.text));

        let tool_specs: Vec<ToolSpec> = self
            .tools
            .iter()
            .map(|t| ToolSpec::for_tool(&**t))
            .collect();

        for _round in 1..=self.max_rounds {
            emit!(AgentEvent::Model(ModelEvent::Requested {
                purpose: CallPurpose::Reasoning,
                messages: messages.clone(),
            }));

            let request =
                ModelRequest::new(messages.clone(), tool_specs.clone(), ModelParams::default());

            // Suspension point 1: starting the model call.
            let mut stream = tokio::select! {
                outcome = core.cancelled() => {
                    emit!(cancelled_event(outcome));
                    return;
                }
                result = self.model.stream(request) => match result {
                    Ok(stream) => stream,
                    Err(error) => {
                        emit!(AgentEvent::Lifecycle(LifecycleEvent::Failed {
                            error: AgentError::Model(error),
                        }));
                        return;
                    }
                }
            };

            // Suspension point 2: consuming the response stream.
            let (message, usage) = loop {
                let item = tokio::select! {
                    outcome = core.cancelled() => {
                        emit!(cancelled_event(outcome));
                        return;
                    }
                    item = stream.next() => match item {
                        Some(ModelStreamItem::Delta(delta)) => {
                            emit!(AgentEvent::Model(ModelEvent::StreamDelta { delta }));
                            continue;
                        }
                        Some(ModelStreamItem::Failed(error)) => {
                            emit!(AgentEvent::Lifecycle(LifecycleEvent::Failed {
                                error: AgentError::Model(error),
                            }));
                            return;
                        }
                        Some(ModelStreamItem::Finish { message, usage }) => (message, usage),
                        None => {
                            emit!(AgentEvent::Lifecycle(LifecycleEvent::Failed {
                                error: AgentError::Model(ModelError::Api {
                                    message: "model stream ended without a finish item".into(),
                                }),
                            }));
                            return;
                        }
                    }
                };
                break item;
            };

            total_usage = TokenUsage::new(
                total_usage.input_tokens + usage.input_tokens,
                total_usage.output_tokens + usage.output_tokens,
            );

            let calls = tool_calls_of(&message);
            emit!(AgentEvent::Model(ModelEvent::Responded {
                message: message.clone(),
                usage,
            }));

            if calls.is_empty() {
                emit!(AgentEvent::Lifecycle(LifecycleEvent::Completed {
                    response: AgentOutput::new(message, total_usage),
                }));
                return;
            }

            // Canonical order: the assistant message (with its tool calls)
            // precedes the tool result messages.
            messages.push(message);

            // Suspension point 3: parallel tool execution — completion-order
            // events, deterministic call-order conversation.
            emit_all_requested(&sender, &calls).await;
            let results = match self.run_tools_parallel(&calls, &core, &sender).await {
                Some(results) => results,
                None => return, // cancelled while tools ran
            };
            for call in &calls {
                let result = results
                    .get(&call.call_id)
                    .expect("every issued call has a result");
                messages.push(Message::tool_result(call.call_id.clone(), result.clone()));
            }
        }

        emit!(AgentEvent::Lifecycle(LifecycleEvent::Failed {
            error: AgentError::MaxRoundsExceeded,
        }));
    }

    /// Executes all calls in parallel; emits `CallCompleted` in completion
    /// order; returns results keyed by call id. Returns `None` when
    /// cancelled mid-execution.
    async fn run_tools_parallel(
        &self,
        calls: &[ToolCall],
        core: &Arc<CancelCore>,
        sender: &mpsc::Sender<AgentEvent>,
    ) -> Option<std::collections::HashMap<CallId, ToolResult>> {
        let mut set = tokio::task::JoinSet::new();
        for call in calls {
            let tool = self.tools.iter().find(|t| t.name() == call.name).cloned();
            let call = call.clone();
            let token = core.token().clone();
            set.spawn(async move {
                let result = match tool {
                    None => ToolResult::Err {
                        message: format!("unknown tool: {}", call.name),
                    },
                    Some(tool) => match tool
                        .execute(call.arguments.clone(), ToolContext::new(token))
                        .await
                    {
                        Ok(result) => result,
                        Err(error) => ToolResult::Err {
                            message: error.to_string(),
                        },
                    },
                };
                (call.call_id.clone(), result)
            });
        }

        let mut results = std::collections::HashMap::new();
        while results.len() < calls.len() {
            let joined = tokio::select! {
                outcome = core.cancelled() => {
                    set.abort_all();
                    // Best effort: the consumer may already be gone.
                    let _ = sender.send(cancelled_event(outcome)).await;
                    return None;
                }
                joined = set.join_next() => joined,
            };
            match joined {
                Some(Ok((call_id, result))) => {
                    if sender
                        .send(AgentEvent::Tool(ToolEvent::CallCompleted {
                            call_id: call_id.clone(),
                            result: result.clone(),
                        }))
                        .await
                        .is_err()
                    {
                        return None; // consumer gone
                    }
                    results.insert(call_id, result);
                }
                Some(Err(join_error)) => {
                    // A tool task panicked (aborts are handled by the
                    // cancellation branch). Find the outstanding call and
                    // report it as a soft failure.
                    if join_error.is_cancelled() {
                        continue;
                    }
                    if let Some(call) = calls.iter().find(|c| !results.contains_key(&c.call_id)) {
                        let result = ToolResult::Err {
                            message: format!("tool task failed: {join_error}"),
                        };
                        if sender
                            .send(AgentEvent::Tool(ToolEvent::CallCompleted {
                                call_id: call.call_id.clone(),
                                result: result.clone(),
                            }))
                            .await
                            .is_err()
                        {
                            return None;
                        }
                        results.insert(call.call_id.clone(), result);
                    }
                }
                None => break, // set drained
            }
        }
        Some(results)
    }
}

async fn emit_all_requested(sender: &mpsc::Sender<AgentEvent>, calls: &[ToolCall]) {
    for call in calls {
        if sender
            .send(AgentEvent::Tool(ToolEvent::CallRequested {
                call: call.clone(),
            }))
            .await
            .is_err()
        {
            return; // consumer gone; the loop notices on the next emit
        }
    }
}

fn tool_calls_of(message: &Message) -> Vec<ToolCall> {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .collect()
}

fn cancelled_event(outcome: CancelOutcome) -> AgentEvent {
    let reason = match outcome {
        CancelOutcome::Timeout => CancelReason::Timeout,
        CancelOutcome::Signal => CancelReason::UserRequested,
    };
    AgentEvent::Lifecycle(LifecycleEvent::Cancelled { reason })
}
