//! The agent: a canned composition of model, tools, system prompt, and the
//! reasoning loop.
//!
//! An `Agent` is a *stateless, immutable configuration* (model + tools +
//! system prompt + round budget). All run state lives inside a run; one
//! agent can drive many concurrent runs without shared state.
//!
//! # Interaction
//!
//! The dual-layer API: [`Agent::run`] returns a [`Run`] of
//! [`AgentEvent`]s (the complete, observable narrative; dropping it
//! cancels the run), and [`Agent::ask`] returns a streaming-first
//! [`Answer`] (text deltas, then the final output on await).
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

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::Stream;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::CancellationToken;
use crate::cancel::{CancelCore, CancelHandle, CancelOutcome};
use crate::context::Context;
use crate::conversation::{Conversation, Turn, TurnInput};
use crate::error::{AgentError, ModelError};
use crate::event::{
    AgentEvent, CallPurpose, CancelReason, LifecycleEvent, ModelDelta, ModelEvent, TokenUsage,
    ToolEvent,
};
use crate::io::{AgentInput, AgentOutput};
use crate::message::{CallId, ContentBlock, Message, ToolCall, ToolResult};
use crate::model::{Model, ModelParams, ModelRequest, ModelStreamItem};
use crate::tool::{Tool, ToolContext, ToolSpec};

/// Default round budget: how many reasoning rounds a run may use before it
/// fails with [`AgentError::MaxRoundsExceeded`]. Explicitly configurable via
/// [`AgentBuilder::max_rounds`].
pub const DEFAULT_MAX_ROUNDS: u32 = 16;

/// Round budget recommended for the research pattern (multi-round search,
/// read, and synthesis). Documented, not enforced; overridable via
/// [`AgentBuilder::max_rounds`].
const RESEARCH_MAX_ROUNDS: u32 = 32;

/// The research pattern's system prompt (private: shown in the
/// [`Agent::research`] docs; compose further instructions via
/// [`AgentBuilder::extend_system_prompt`] instead of referencing this).
const RESEARCH_SYSTEM_PROMPT: &str = "You are a research agent. Investigate the user's question thoroughly using the available tools: search broadly first, then read the most promising sources. Verify important claims across independent sources before relying on them. Synthesize a complete, clearly structured answer and cite sources for factual claims. State uncertainty explicitly when evidence is thin or conflicting.";

/// The reflection pattern's system prompt (private: shown in the
/// [`Agent::reflection`] docs; compose further instructions via
/// [`AgentBuilder::extend_system_prompt`] instead of referencing this).
const REFLECTION_SYSTEM_PROMPT: &str = "You are a reflective agent. Work in three passes for every task: (1) draft — produce a first answer; (2) critique — examine your draft for errors, gaps, and unsupported claims; (3) revise — produce the improved final answer. Deliver only the final answer unless the user asks to see the intermediate passes.";

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
    /// is no hidden default prompt. Replaces any prompt set earlier; to
    /// compose additional instructions on top of an existing prompt (for
    /// example a preset's), use
    /// [`extend_system_prompt`][AgentBuilder::extend_system_prompt].
    pub fn system_prompt(mut self, text: impl Into<String>) -> Self {
        self.system_prompt = Some(text.into());
        self
    }

    /// Appends instructions to the current system prompt, creating it when
    /// absent.
    ///
    /// The natural partner of the pattern presets
    /// ([`Agent::research`], [`Agent::reflection`]): extend the preset
    /// prompt with domain instructions without replacing it. The appended
    /// text is separated from the existing prompt by a blank line.
    pub fn extend_system_prompt(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        match &mut self.system_prompt {
            Some(existing) => {
                existing.push_str("\n\n");
                existing.push_str(&text);
            }
            None => self.system_prompt = Some(text),
        }
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
            default_timeout: None,
            context: None,
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
    default_timeout: Option<Duration>,
    context: Option<Arc<Context>>,
}

impl Agent {
    /// Starts building an agent.
    pub fn builder() -> AgentBuilder {
        AgentBuilder::new()
    }

    /// The ReAct pattern as a named preset: the default reasoning loop,
    /// named for explicit intent.
    ///
    /// Injects nothing — the default loop *is* the reasoning-acting loop,
    /// and this constructor exists so developers state the pattern
    /// explicitly. Register tools via the returned builder as usual.
    pub fn react<M, I, T>(model: M, tools: I) -> AgentBuilder
    where
        M: Model + 'static,
        I: IntoIterator<Item = T>,
        T: Tool + 'static,
    {
        AgentBuilder::new().model(model).tools(tools)
    }

    /// The research pattern as a preset: multi-round search, verification,
    /// and synthesis.
    ///
    /// The preset sets a system prompt instructing broad search, source
    /// verification, and cited synthesis, and recommends a round budget of
    /// 32. Both are overridable on the returned
    /// builder ([`AgentBuilder::system_prompt`],
    /// [`AgentBuilder::max_rounds`]); compose domain instructions with
    /// [`AgentBuilder::extend_system_prompt`].
    ///
    /// Default system prompt (verbatim):
    ///
    /// ```text
    /// You are a research agent. Investigate the user's question thoroughly
    /// using the available tools: search broadly first, then read the most
    /// promising sources. Verify important claims across independent sources
    /// before relying on them. Synthesize a complete, clearly structured
    /// answer and cite sources for factual claims. State uncertainty
    /// explicitly when evidence is thin or conflicting.
    /// ```
    pub fn research<M, I, T>(model: M, tools: I) -> AgentBuilder
    where
        M: Model + 'static,
        I: IntoIterator<Item = T>,
        T: Tool + 'static,
    {
        AgentBuilder::new()
            .model(model)
            .tools(tools)
            .system_prompt(RESEARCH_SYSTEM_PROMPT)
            .max_rounds(RESEARCH_MAX_ROUNDS)
    }

    /// The reflection pattern as a preset: draft, critique, revise.
    ///
    /// The preset sets a system prompt instructing the three-pass
    /// draft-critique-revise discipline. Tools are optional for this
    /// pattern (self-critique needs none) — register them on the returned
    /// builder when wanted. The prompt is overridable via
    /// [`AgentBuilder::system_prompt`]; compose domain instructions with
    /// [`AgentBuilder::extend_system_prompt`].
    ///
    /// Default system prompt (verbatim):
    ///
    /// ```text
    /// You are a reflective agent. Work in three passes for every task:
    /// (1) draft — produce a first answer; (2) critique — examine your
    /// draft for errors, gaps, and unsupported claims; (3) revise —
    /// produce the improved final answer. Deliver only the final answer
    /// unless the user asks to see the intermediate passes.
    /// ```
    pub fn reflection<M: Model + 'static>(model: M) -> AgentBuilder {
        AgentBuilder::new()
            .model(model)
            .system_prompt(REFLECTION_SYSTEM_PROMPT)
    }

    /// Runs the agent and returns the run handle: the full event narrative.
    ///
    /// Await the handle for the final output ([`AgentOutput`]) or iterate it
    /// for the complete event stream. Dropping it (or calling
    /// [`Run::cancel`]) cancels the run — cooperative interruption at the
    /// loop's await points.
    pub fn run<'a>(&self, input: impl Into<TurnInput<'a>>) -> Run<'a> {
        let (input, conv) = input.into().into_parts();
        let core = CancelCore::new();
        let run = self.spawn_run(input, conv, core);
        self.apply_default_timeout(run)
    }

    /// Runs the agent with an externally owned cancellation token: when the
    /// token fires, the run cancels with [`CancelReason::UserRequested`].
    pub fn run_with<'a>(
        &self,
        input: impl Into<TurnInput<'a>>,
        token: CancellationToken,
    ) -> Run<'a> {
        let (input, conv) = input.into().into_parts();
        let core = CancelCore::child_of(&token);
        let run = self.spawn_run(input, conv, core);
        self.apply_default_timeout(run)
    }

    /// Asks the agent a question, returning a streaming-first [`Answer`].
    ///
    /// `Answer` yields text deltas via [`Answer::next`] and resolves to the
    /// final [`AgentOutput`] when awaited — so the one-shot spelling
    /// `agent.ask(input).await?` behaves exactly like the previous blocking
    /// `ask`. Cancellation: [`Answer::cancel`], or dropping the handle.
    pub fn ask<'a>(&self, input: impl Into<TurnInput<'a>>) -> Answer<'a> {
        let (input, conv) = input.into().into_parts();
        Answer {
            run: self.run_with_conv(input, conv),
        }
    }

    /// Internal shared path for `ask`: no default timeout handling here so
    /// `ask` mirrors `run` semantics through the same machinery.
    fn run_with_conv<'a>(&self, input: AgentInput, conv: Option<&'a Conversation>) -> Run<'a> {
        let core = CancelCore::new();
        let run = self.spawn_run(input, conv, core);
        self.apply_default_timeout(run)
    }

    /// Sets a default time budget applied to every run started afterwards.
    ///
    /// The budget is enforced as a [`CancelReason::Timeout`] cancellation;
    /// per-run [`Run::with_timeout`] overrides it.
    pub fn with_timeout(mut self, duration: Duration) -> Self {
        self.default_timeout = Some(duration);
        self
    }

    /// Attaches the narrative background (a conversation's
    /// [`Context`], from
    /// [`Conversation::context`][crate::Conversation::context]): the agent
    /// executes within it — the background makes the stateless agent
    /// stateful.
    ///
    /// With a context, every `ask`/`run` assembles the send view freshly
    /// through the context's assembly strategy (memory layers). Without
    /// one, the pre-memory behavior applies (the conversation's history,
    /// verbatim).
    pub fn with_context(mut self, context: Context) -> Self {
        self.context = Some(Arc::new(context));
        self
    }

    fn apply_default_timeout<'a>(&self, run: Run<'a>) -> Run<'a> {
        match self.default_timeout {
            Some(duration) => run.with_timeout(duration),
            None => run,
        }
    }

    fn spawn_run<'a>(
        &self,
        input: AgentInput,
        conv: Option<&'a Conversation>,
        core: Arc<CancelCore>,
    ) -> Run<'a> {
        let (sender, receiver) = mpsc::channel(1);
        let task = LoopTask {
            model: Arc::clone(&self.model),
            tools: Arc::clone(&self.tools),
            system_prompt: self.system_prompt.clone(),
            max_rounds: self.max_rounds,
            conversation: conv.cloned(),
            context: self.context.clone(),
        };
        tokio::spawn(task.execute(input, Arc::clone(&core), sender));
        Run {
            receiver,
            handle: CancelHandle::new(core),
            rounds_seen: 0,
            terminal: None,
            // Serialization guard: while the handle is alive the
            // conversation cannot start a competing turn (borrow checker).
            // The turn write itself happens inside the execution.
            _conv: conv,
        }
    }
}

/// Maps a terminal lifecycle event onto the run's final outcome.
fn map_terminal(event: &AgentEvent) -> Option<Result<AgentOutput, AgentError>> {
    match event {
        AgentEvent::Lifecycle(LifecycleEvent::Completed { response }) => Some(Ok(response.clone())),
        AgentEvent::Lifecycle(LifecycleEvent::Failed { error }) => Some(Err(error.clone())),
        AgentEvent::Lifecycle(LifecycleEvent::Cancelled { reason }) => {
            Some(Err(AgentError::Cancelled(*reason)))
        }
        _ => None,
    }
}

/// The handle to one in-flight run: the full event narrative.
///
/// Dual-faced: iterate it ([`Stream`] of [`AgentEvent`]) for the complete
/// observable narrative, or await it for the final [`AgentOutput`]. The
/// last event is always a terminal lifecycle event, and the stream closes
/// after it. Dropping the handle (or calling [`Run::cancel`]) cancels the
/// run.
pub struct Run<'a> {
    receiver: mpsc::Receiver<AgentEvent>,
    handle: CancelHandle,
    rounds_seen: usize,
    terminal: Option<Result<AgentOutput, AgentError>>,
    // See `spawn_run`: borrow guard only, never read.
    _conv: Option<&'a Conversation>,
}

impl Run<'_> {
    /// Receives the next event, or `None` after the stream closes.
    ///
    /// The terminal event's outcome is remembered, so awaiting the handle
    /// after full iteration still resolves.
    pub async fn next(&mut self) -> Option<AgentEvent> {
        let event = self.receiver.recv().await?;
        self.note(&event);
        Some(event)
    }

    fn note(&mut self, event: &AgentEvent) {
        if matches!(
            event,
            AgentEvent::Model(ModelEvent::Requested {
                purpose: CallPurpose::Reasoning,
                ..
            })
        ) {
            self.rounds_seen += 1;
        }
        if let Some(result) = map_terminal(event) {
            self.terminal = Some(result);
        }
    }

    /// Explicitly cancels the run. The event stream then terminates with
    /// [`LifecycleEvent::Cancelled`] ([`CancelReason::UserRequested`]);
    /// dropping the handle is the RAII backstop for the same behavior.
    pub fn cancel(&self) {
        self.handle.cancel();
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

impl Stream for Run<'_> {
    type Item = AgentEvent;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<AgentEvent>> {
        match self.receiver.poll_recv(cx) {
            std::task::Poll::Ready(Some(event)) => {
                self.note(&event);
                std::task::Poll::Ready(Some(event))
            }
            other => other,
        }
    }
}

impl Future for Run<'_> {
    type Output = Result<AgentOutput, AgentError>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let Some(result) = &self.terminal {
            return std::task::Poll::Ready(result.clone());
        }
        loop {
            match self.receiver.poll_recv(cx) {
                std::task::Poll::Ready(Some(event)) => {
                    if let Some(result) = map_terminal(&event) {
                        self.terminal = Some(result.clone());
                        return std::task::Poll::Ready(result);
                    }
                }
                std::task::Poll::Ready(None) => {
                    // Internal invariant: the loop always emits a terminal
                    // event before closing the stream.
                    unreachable!("run loop always emits a terminal event");
                }
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

/// A streaming-first answer handle.
///
/// Yields text deltas ([`ModelDelta`]) as the model streams, and resolves
/// to the final [`AgentOutput`] when awaited. Internally a filtered view of
/// a [`Run`]: non-delta events are consumed silently, and the run's outcome
/// is preserved for the eventual await.
pub struct Answer<'a> {
    run: Run<'a>,
}

impl Answer<'_> {
    /// Receives the next text delta, or `None` when the answer stream ends.
    pub async fn next(&mut self) -> Option<ModelDelta> {
        while let Some(event) = self.run.next().await {
            if let AgentEvent::Model(ModelEvent::StreamDelta { delta }) = event {
                return Some(delta);
            }
        }
        None
    }

    /// Explicitly cancels the answer's run (see [`Run::cancel`]).
    pub fn cancel(&self) {
        self.run.cancel();
    }

    /// Arms the answer's time budget (see [`Run::with_timeout`]).
    pub fn with_timeout(self, duration: Duration) -> Self {
        Self {
            run: self.run.with_timeout(duration),
        }
    }
}

impl Stream for Answer<'_> {
    type Item = ModelDelta;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<ModelDelta>> {
        loop {
            match Pin::new(&mut self.run).poll_next(cx) {
                std::task::Poll::Ready(Some(AgentEvent::Model(ModelEvent::StreamDelta {
                    delta,
                }))) => return std::task::Poll::Ready(Some(delta)),
                std::task::Poll::Ready(Some(_)) => continue,
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

impl Future for Answer<'_> {
    type Output = Result<AgentOutput, AgentError>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        Pin::new(&mut self.run).poll(cx)
    }
}

/// The per-run task: everything the loop needs, all owned.
struct LoopTask {
    model: Arc<dyn Model>,
    tools: Arc<[Arc<dyn Tool>]>,
    system_prompt: Option<String>,
    max_rounds: u32,
    conversation: Option<Conversation>,
    context: Option<Arc<Context>>,
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
        // Seed the narrative background: the attached context's assembly
        // (fresh per ask), or the pre-memory fallback (history verbatim).
        if let Some(context) = &self.context {
            match context.assemble(&input.text).await {
                Ok(assembled) => messages.extend(assembled),
                Err(error) => {
                    emit!(AgentEvent::Lifecycle(LifecycleEvent::Failed {
                        error: crate::AgentError::InvalidConfiguration {
                            message: format!("context assembly failed: {error}"),
                        },
                    }));
                    return;
                }
            }
        } else if let Some(conversation) = &self.conversation {
            messages.extend(conversation.messages());
        }
        // Everything from here on belongs to this turn (recorded on
        // completion; cancelled or failed runs record nothing).
        let base_len = messages.len();
        messages.push(Message::user(input.text.clone()));

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
                let output = AgentOutput::new(message, total_usage);
                if let Some(conversation) = &self.conversation {
                    conversation.push_turn(Turn::new(
                        input.clone(),
                        messages[base_len..].to_vec(),
                        output.clone(),
                    ));
                    // Post-turn memory flows (M11b): L1 write, topic
                    // tracking, mandatory floors, stacked policies. The
                    // summary call emits ContextManagement events before
                    // the terminal event — visible, not magic.
                    let runtime = conversation.runtime().clone();
                    let policies = runtime.memory_policies();
                    let detector = runtime.topic_detector();
                    let _soft = crate::trigger::run_post_turn_flows(
                        crate::trigger::PostTurn {
                            model: &*self.model,
                            conversation,
                            policies: &policies,
                            detector: &*detector,
                            input: &input.text,
                            messages: messages[base_len..].to_vec(),
                        },
                        &sender,
                    )
                    .await;
                }
                emit!(AgentEvent::Lifecycle(LifecycleEvent::Completed {
                    response: output,
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

#[cfg(test)]
mod preset_tests {
    use super::*;
    use crate::model::ModelStream;
    use crate::tool::ToolError;

    struct DummyModel;

    impl Model for DummyModel {
        fn stream(
            &self,
            _request: ModelRequest,
        ) -> futures::future::BoxFuture<'_, Result<ModelStream, ModelError>> {
            Box::pin(async { Ok(futures::stream::empty().boxed()) })
        }
    }

    fn dummy_tools() -> [StubTool; 2] {
        [StubTool, StubTool]
    }

    struct StubTool;

    impl Tool for StubTool {
        fn name(&self) -> &str {
            "stub"
        }
        fn description(&self) -> &str {
            "stub tool"
        }
        fn parameters_schema(&self) -> &serde_json::Value {
            static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
            SCHEMA.get_or_init(|| serde_json::json!({"type": "object"}))
        }
        fn execute<'a>(
            &'a self,
            _args: serde_json::Value,
            _ctx: ToolContext,
        ) -> futures::future::BoxFuture<'a, Result<ToolResult, ToolError>> {
            Box::pin(async {
                Ok(ToolResult::Ok {
                    content: crate::message::ToolContent::Text {
                        text: "stub".into(),
                    },
                })
            })
        }
    }

    #[test]
    fn react_is_the_bare_default() {
        let builder = Agent::react(DummyModel, dummy_tools());
        assert_eq!(builder.system_prompt, None);
        assert_eq!(builder.max_rounds, None);
        assert_eq!(builder.tools.len(), 2);
    }

    #[test]
    fn research_sets_prompt_and_round_budget() {
        let builder = Agent::research(DummyModel, dummy_tools());
        assert_eq!(
            builder.system_prompt.as_deref(),
            Some(RESEARCH_SYSTEM_PROMPT)
        );
        assert_eq!(builder.max_rounds, Some(RESEARCH_MAX_ROUNDS));
        assert_eq!(builder.tools.len(), 2);
    }

    #[test]
    fn reflection_sets_prompt_without_tools() {
        let builder = Agent::reflection(DummyModel);
        assert_eq!(
            builder.system_prompt.as_deref(),
            Some(REFLECTION_SYSTEM_PROMPT)
        );
        assert_eq!(builder.max_rounds, None);
        assert!(builder.tools.is_empty());
    }

    #[test]
    fn extend_system_prompt_composes_and_creates() {
        let builder = AgentBuilder::new().extend_system_prompt("first");
        assert_eq!(builder.system_prompt.as_deref(), Some("first"));
        let builder = builder.extend_system_prompt("second");
        assert_eq!(builder.system_prompt.as_deref(), Some("first\n\nsecond"));
    }

    #[test]
    fn extend_works_on_top_of_presets() {
        let builder = Agent::research(DummyModel, dummy_tools())
            .extend_system_prompt("prefer chinese sources");
        assert_eq!(
            builder.system_prompt.as_deref(),
            Some(&format!("{RESEARCH_SYSTEM_PROMPT}\n\nprefer chinese sources")[..])
        );
    }

    #[test]
    fn system_prompt_overrides_presets() {
        let builder = Agent::research(DummyModel, dummy_tools()).system_prompt("custom");
        assert_eq!(builder.system_prompt.as_deref(), Some("custom"));
    }

    #[cfg(feature = "test-util")]
    #[tokio::test]
    async fn research_preset_drives_a_full_run() {
        use crate::mock::MockModel;
        let model = MockModel::new(vec![vec![ModelStreamItem::Finish {
            message: Message::assistant_text("found the answer"),
            usage: TokenUsage::new(1, 1),
        }]]);
        let agent = Agent::research(model, dummy_tools()).build().unwrap();
        let output = agent.ask("research x").await.unwrap();
        assert_eq!(output.text(), Some("found the answer"));
    }
}
