//! The agent: a canned composition of model, tools, system prompt, and the
//! reasoning loop.
//!
//! An `Agent` is a *stateless, immutable configuration* (model + tools +
//! system prompt + round budget). All run state lives inside a run; one
//! agent can drive many concurrent runs without shared state.
//!
//! # Interaction
//!
//! The single execution face (ADR-0014): [`Agent::run`] returns an
//! [`Execution`] — a three-in-one handle (narrative stream of
//! [`ExecutionEvent`]s, final-output Future, controller). Dropping it
//! cancels the run.
//!
//! ```no_run
//! use futures::StreamExt;
//! use synonz::{
//!     Agent, ExecutionEvent, Model, ModelRequest, Subject, SubjectType, SynonzRuntime,
//! };
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
//! let runtime = SynonzRuntime::builder().build();
//! let mut conv = synonz::Conversation::new(
//!     &runtime,
//!     &Subject::of(SubjectType::User, "demo"),
//! );
//! let agent = Agent::builder()
//!     .runtime(&runtime)
//!     .model(EchoModel)
//!     .system_prompt("be friendly")
//!     .build()
//!     .expect("model is set");
//!
//! let mut execution = agent.run(conv.turn_input("hi"));
//! while let Some(event) = execution.next().await {
//!     if let ExecutionEvent::Completed(output) = event {
//!         assert_eq!(output.text(), Some("hello!"));
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
use crate::conversation::{Conversation, Turn, TurnInput};
use crate::error::{AgentError, ModelError};
use crate::event::{
    AgentEvent, CallPurpose, CancelReason, ExecutionEvent, LifecycleEvent, MemoryFlowStage,
    ModelEvent, TokenUsage, ToolEvent,
};
use crate::io::{AgentInput, AgentOutput};
use crate::message::{CallId, ContentBlock, Message, ToolCall, ToolResult};
use crate::model::{Model, ModelRequest, ModelStreamItem};
use crate::runtime::SynonzRuntime;
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
    runtime: Option<SynonzRuntime>,
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

    /// Sets the runtime (required): the environment the agent lives in.
    /// The agent and every conversation it executes against must come from
    /// the same runtime (enforced at the execution entry).
    pub fn runtime(mut self, runtime: &SynonzRuntime) -> Self {
        self.runtime = Some(runtime.clone());
        self
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
    /// Fails with [`AgentError::InvalidConfiguration`] when no model or no
    /// runtime was set.
    pub fn build(self) -> Result<Agent, AgentError> {
        let model = self.model.ok_or_else(|| AgentError::InvalidConfiguration {
            message: "a model is required".into(),
        })?;
        let runtime = self
            .runtime
            .ok_or_else(|| AgentError::InvalidConfiguration {
                message: "a runtime is required".into(),
            })?;
        Ok(Agent {
            runtime,
            model,
            tools: self.tools.into(),
            system_prompt: self.system_prompt,
            max_rounds: self.max_rounds.unwrap_or(DEFAULT_MAX_ROUNDS),
            default_timeout: None,
        })
    }
}

/// The agent's configuration: model + tools + system prompt + budget, plus
/// the runtime it lives in (ADR-0015: the agent knows its container; it
/// holds no run state — every run's state lives inside the run, and the
/// same agent can drive many concurrent runs independently).
#[derive(Clone)]
pub struct Agent {
    runtime: SynonzRuntime,
    model: Arc<dyn Model>,
    tools: Arc<[Arc<dyn Tool>]>,
    system_prompt: Option<String>,
    max_rounds: u32,
    default_timeout: Option<Duration>,
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
    pub fn react<M, I, T>(runtime: &SynonzRuntime, model: M, tools: I) -> AgentBuilder
    where
        M: Model + 'static,
        I: IntoIterator<Item = T>,
        T: Tool + 'static,
    {
        AgentBuilder::new()
            .runtime(runtime)
            .model(model)
            .tools(tools)
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
    pub fn research<M, I, T>(runtime: &SynonzRuntime, model: M, tools: I) -> AgentBuilder
    where
        M: Model + 'static,
        I: IntoIterator<Item = T>,
        T: Tool + 'static,
    {
        AgentBuilder::new()
            .runtime(runtime)
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
    pub fn reflection<M: Model + 'static>(runtime: &SynonzRuntime, model: M) -> AgentBuilder {
        AgentBuilder::new()
            .runtime(runtime)
            .model(model)
            .system_prompt(REFLECTION_SYSTEM_PROMPT)
    }

    /// Runs the agent and returns the run handle: the full event narrative.
    ///
    /// Runs the agent: the single execution face (ADR-0014).
    ///
    /// Returns an [`Execution`] — a three-in-one handle (narrative
    /// stream of [`ExecutionEvent`]s, final-output Future, controller).
    /// Dropping it (or calling [`Execution::cancel`]) cancels the run —
    /// cooperative interruption at the loop's await points.
    ///
    /// # Panics
    ///
    /// Panics when the conversation belongs to a different runtime than
    /// the agent — cross-runtime mixing is a programmer error; build both
    /// from the same [`SynonzRuntime`].
    pub fn run<'a>(&self, input: impl Into<TurnInput<'a>>) -> Execution<'a> {
        let (input, conv) = input.into().into_parts();
        self.check_same_runtime(conv);
        let runner = self.spawn_runner(input, conv, CancelCore::new());
        Execution {
            runner,
            // Serialization guard: while the handle is alive the
            // conversation cannot start a competing turn (borrow checker).
            // The turn write itself happens inside the execution.
            _conv: conv,
        }
    }

    /// Runs the agent with an externally owned cancellation token: when the
    /// token fires, the run cancels with [`CancelReason::UserRequested`].
    ///
    /// Panics on cross-runtime mixing, like [`Agent::run`].
    pub fn run_with<'a>(
        &self,
        input: impl Into<TurnInput<'a>>,
        token: CancellationToken,
    ) -> Execution<'a> {
        let (input, conv) = input.into().into_parts();
        self.check_same_runtime(conv);
        let runner = self.spawn_runner(input, conv, CancelCore::child_of(&token));
        Execution {
            runner,
            _conv: conv,
        }
    }

    /// Sets a default time budget applied to every run started afterwards.
    ///
    /// The budget is enforced as a [`CancelReason::Timeout`] cancellation;
    /// per-run [`Execution::with_timeout`] overrides it.
    pub fn with_timeout(mut self, duration: Duration) -> Self {
        self.default_timeout = Some(duration);
        self
    }

    /// Rejects cross-runtime mixing loudly (a programmer error): the agent
    /// and the conversation must come from the same [`SynonzRuntime`]
    /// (ADR-0015 — "fatal and silent" became "mismatch reports loudly").
    fn check_same_runtime(&self, conversation: &Conversation) {
        assert_eq!(
            self.runtime.id(),
            conversation.runtime().id(),
            "conversation '{}' belongs to a different runtime than the agent; \
             build both from the same SynonzRuntime",
            conversation.id()
        );
    }

    /// Spawns the loop task and wraps its shared execution state in an
    /// [`AgentRunner`] — the machinery both `Execution` (now) and the
    /// Observer bypass (ADR-0016) hang off. Applies the agent's default
    /// time budget when one is set.
    fn spawn_runner(
        &self,
        input: AgentInput,
        conv: &Conversation,
        core: Arc<CancelCore>,
    ) -> AgentRunner {
        let (sender, receiver) = mpsc::channel(1);
        let task = AgentLoopTask {
            model: Arc::clone(&self.model),
            tools: Arc::clone(&self.tools),
            system_prompt: self.system_prompt.clone(),
            max_rounds: self.max_rounds,
            conversation: conv.clone(),
        };
        tokio::spawn(task.execute(input, Arc::clone(&core), sender));
        let runner = AgentRunner {
            receiver,
            handle: CancelHandle::new(core),
            rounds_seen: 0,
            terminal: None,
        };
        if let Some(duration) = self.default_timeout {
            runner.arm_timeout(duration);
        }
        runner
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

/// The shared execution state of one in-flight run.
///
/// [`Answer`] and [`Run`] are peers that each wrap an `AgentRunner` —
/// neither wraps the other. The runner owns the event receiver, the
/// cancellation handle, and the terminal outcome; the two handles differ
/// only in how they consume the event stream.
pub(crate) struct AgentRunner {
    receiver: mpsc::Receiver<AgentEvent>,
    handle: CancelHandle,
    rounds_seen: usize,
    terminal: Option<Result<AgentOutput, AgentError>>,
}

impl AgentRunner {
    /// Receives the next event, or `None` after the stream closes. The
    /// terminal event's outcome is remembered, so awaiting the runner
    /// after full iteration still resolves.
    async fn next(&mut self) -> Option<AgentEvent> {
        let event = self.receiver.recv().await?;
        self.note(&event);
        Some(event)
    }

    /// Records round count and the terminal outcome from an event.
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
    fn cancel(&self) {
        self.handle.cancel();
    }

    /// Arms the run's time budget. When it elapses first, the run cancels
    /// with [`CancelReason::Timeout`].
    fn arm_timeout(&self, duration: Duration) {
        self.handle.arm_timeout(duration);
    }

    /// The number of reasoning rounds observed so far (derived from consumed
    /// events — rounds are never stored as events).
    fn rounds(&self) -> usize {
        self.rounds_seen
    }

    /// `Stream::poll_next` over the full event narrative.
    fn poll_event(
        &mut self,
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

    /// `Future::poll` until the terminal outcome. Replays the stashed
    /// outcome when the stream was already fully consumed.
    fn poll_result(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<AgentOutput, AgentError>> {
        if let Some(result) = &self.terminal {
            return std::task::Poll::Ready(result.clone());
        }
        loop {
            match self.receiver.poll_recv(cx) {
                std::task::Poll::Ready(Some(event)) => {
                    self.note(&event);
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

/// Projects an internal event onto the execution face (ADR-0014):
/// input-side payloads (`Started` / `Requested` / `Responded`) stay on
/// the observation bypass; everything a product consumer renders
/// surfaces as an [`ExecutionEvent`].
fn execution_event(event: AgentEvent) -> Option<ExecutionEvent> {
    match event {
        AgentEvent::Model(ModelEvent::StreamDelta { delta }) => Some(ExecutionEvent::Delta(delta)),
        AgentEvent::Tool(ToolEvent::CallRequested { call }) => {
            Some(ExecutionEvent::ToolRequested(call))
        }
        AgentEvent::Tool(ToolEvent::CallCompleted { call_id, result }) => {
            Some(ExecutionEvent::ToolCompleted { call_id, result })
        }
        AgentEvent::Lifecycle(LifecycleEvent::Completed { response }) => {
            Some(ExecutionEvent::Completed(response))
        }
        AgentEvent::Lifecycle(LifecycleEvent::Failed { error }) => {
            Some(ExecutionEvent::Failed(error))
        }
        AgentEvent::Lifecycle(LifecycleEvent::Cancelled { reason }) => {
            Some(ExecutionEvent::Cancelled(reason))
        }
        AgentEvent::Lifecycle(LifecycleEvent::Started { .. })
        | AgentEvent::Lifecycle(LifecycleEvent::MemoryFlowFailed { .. })
        | AgentEvent::Model(ModelEvent::Requested { .. })
        | AgentEvent::Model(ModelEvent::Responded { .. }) => None,
    }
}

/// The handle to one in-flight execution: the product-narrative face.
///
/// Three-in-one (ADR-0014): iterate it ([`Stream`] of
/// [`ExecutionEvent`]) for the narrative — text deltas, tool cards,
/// status, terminal outcome — await it for the final [`AgentOutput`]
/// (stream self-sufficiency: the terminal `Completed` event already
/// carries it), and control the run (cancel / timeout / rounds). The
/// terminal invariant holds: a terminal variant is always the last
/// item, and the stream closes after it. Dropping the handle (or
/// calling [`Execution::cancel`]) cancels the run.
pub struct Execution<'a> {
    runner: AgentRunner,
    // Serialization guard: while the handle is alive the conversation
    // cannot start a competing turn (borrow checker). The turn write
    // itself happens inside the execution.
    _conv: &'a Conversation,
}

impl Execution<'_> {
    /// Receives the next narrative event, or `None` after the stream
    /// closes. The terminal outcome is remembered, so awaiting the
    /// handle after full iteration still resolves.
    pub async fn next(&mut self) -> Option<ExecutionEvent> {
        loop {
            let event = self.runner.next().await?;
            if let Some(narrative) = execution_event(event) {
                return Some(narrative);
            }
        }
    }

    /// Explicitly cancels the run. The stream then terminates with
    /// [`ExecutionEvent::Cancelled`]; dropping the handle is the RAII
    /// backstop for the same behavior.
    pub fn cancel(&self) {
        self.runner.cancel();
    }

    /// Arms the run's time budget. When it elapses first, the run cancels
    /// with [`CancelReason::Timeout`].
    pub fn with_timeout(self, duration: Duration) -> Self {
        self.runner.arm_timeout(duration);
        self
    }

    /// The number of reasoning rounds observed so far (derived from consumed
    /// events — rounds are never stored as events).
    pub fn rounds(&self) -> usize {
        self.runner.rounds()
    }
}

impl Stream for Execution<'_> {
    type Item = ExecutionEvent;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<ExecutionEvent>> {
        loop {
            match self.runner.poll_event(cx) {
                std::task::Poll::Ready(Some(event)) => {
                    if let Some(narrative) = execution_event(event) {
                        return std::task::Poll::Ready(Some(narrative));
                    }
                }
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

impl Future for Execution<'_> {
    type Output = Result<AgentOutput, AgentError>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.runner.poll_result(cx)
    }
}

/// The per-run task: everything the loop needs, all owned.
struct AgentLoopTask {
    model: Arc<dyn Model>,
    tools: Arc<[Arc<dyn Tool>]>,
    system_prompt: Option<String>,
    max_rounds: u32,
    conversation: Conversation,
}

impl AgentLoopTask {
    async fn execute(
        self,
        input: AgentInput,
        core: Arc<CancelCore>,
        sender: mpsc::Sender<AgentEvent>,
    ) {
        let mut total_usage = TokenUsage::new(0, 0);

        // Started: a plain send. A consumer already gone here is caught by
        // the cancelled check right after the turn's frame is built below.
        let _ = sender
            .send(AgentEvent::Lifecycle(LifecycleEvent::Started {
                input: input.clone(),
            }))
            .await;

        // The background engine, derived from the (mandatory) conversation.
        let context = self.conversation.context();

        // Moment 1: assemble. Memory reads that fail degrade the background
        // visibly — the run continues with the layers that succeeded. (The
        // failure events are plain sends: the turn's frame is not built yet;
        // a consumer gone at this point is caught below.)
        let assembled = context.assemble(&input.text).await;
        for failure in &assembled.failures {
            let _ = sender
                .send(AgentEvent::Lifecycle(LifecycleEvent::MemoryFlowFailed {
                    stage: failure.stage.clone(),
                    detail: failure.detail.clone(),
                }))
                .await;
        }
        let mut messages = Vec::new();
        if let Some(prompt) = &self.system_prompt {
            messages.push(Message::system(prompt.clone()));
        }
        messages.extend(assembled.messages);
        // Everything from here on belongs to this turn (all outcomes enter
        // the truth archive, marked; only success feeds the memory layers).
        let base_len = messages.len();
        messages.push(Message::user(input.text.clone()));

        // Truth-archive helper: every outcome enters the history, marked
        // (ADR-0015). Persistence failures surface as MemoryFlowFailed
        // events — never silent.
        macro_rules! record {
            ($turn:expr) => {
                if let Err(error) = self.conversation.push_turn($turn) {
                    let _ = sender
                        .send(AgentEvent::Lifecycle(LifecycleEvent::MemoryFlowFailed {
                            stage: MemoryFlowStage::Archive,
                            detail: format!("conversation auto-save failed: {error}"),
                        }))
                        .await;
                }
            };
        }

        // Cancelled before the loop (token/drop races with startup): the
        // frame exists now, so the turn is archived and the terminal event
        // still closes the stream (the terminal invariant holds even for a
        // run that never reached the reasoning loop).
        if core.is_cancelled() {
            let outcome = core.cancelled().await;
            record!(Turn::cancelled(
                input.clone(),
                messages[base_len..].to_vec(),
                cancel_reason(outcome),
            ));
            let _ = sender.send(cancelled_event(outcome)).await;
            return;
        }

        // Emit helper: when the consumer is gone (drop-cancel), the turn is
        // archived before the run ends — then the loop simply stops.
        macro_rules! emit {
            ($event:expr) => {
                if sender.send($event).await.is_err() {
                    record!(Turn::cancelled(
                        input.clone(),
                        messages[base_len..].to_vec(),
                        CancelReason::UserRequested,
                    ));
                    return; // consumer dropped; archive then stop narrating
                }
            };
        }

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

            let request = ModelRequest::new(messages.clone(), tool_specs.clone());

            // Suspension point 1: starting the model call.
            let mut stream = tokio::select! {
                outcome = core.cancelled() => {
                    record!(Turn::cancelled(
                        input.clone(),
                        messages[base_len..].to_vec(),
                        cancel_reason(outcome),
                    ));
                    emit!(cancelled_event(outcome));
                    return;
                }
                result = self.model.stream(request) => match result {
                    Ok(stream) => stream,
                    Err(error) => {
                        let error = AgentError::Model(error);
                        record!(Turn::failed(
                            input.clone(),
                            messages[base_len..].to_vec(),
                            error.clone(),
                        ));
                        emit!(AgentEvent::Lifecycle(LifecycleEvent::Failed { error }));
                        return;
                    }
                }
            };

            // Suspension point 2: consuming the response stream.
            let (message, usage) = loop {
                let item = tokio::select! {
                    outcome = core.cancelled() => {
                        record!(Turn::cancelled(
                            input.clone(),
                            messages[base_len..].to_vec(),
                            cancel_reason(outcome),
                        ));
                        emit!(cancelled_event(outcome));
                        return;
                    }
                    item = stream.next() => match item {
                        Some(ModelStreamItem::Delta(delta)) => {
                            emit!(AgentEvent::Model(ModelEvent::StreamDelta { delta }));
                            continue;
                        }
                        Some(ModelStreamItem::Failed(error)) => {
                            let error = AgentError::Model(error);
                            record!(Turn::failed(
                                input.clone(),
                                messages[base_len..].to_vec(),
                                error.clone(),
                            ));
                            emit!(AgentEvent::Lifecycle(LifecycleEvent::Failed { error }));
                            return;
                        }
                        Some(ModelStreamItem::Finish { message, usage }) => (message, usage),
                        None => {
                            let error = AgentError::Model(ModelError::Api {
                                message: "model stream ended without a finish item".into(),
                            });
                            record!(Turn::failed(
                                input.clone(),
                                messages[base_len..].to_vec(),
                                error.clone(),
                            ));
                            emit!(AgentEvent::Lifecycle(LifecycleEvent::Failed { error }));
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
                // Truth archive: the completed turn.
                record!(Turn::completed(
                    input.clone(),
                    messages[base_len..].to_vec(),
                    output.clone(),
                ));
                // Moments 2 + 3: archive + compress the background. The
                // summary call emits ContextManagement events before the
                // terminal — visible, not magic.
                context
                    .on_turn_completed(
                        &*self.model,
                        &input.text,
                        messages[base_len..].to_vec(),
                        &sender,
                    )
                    .await;
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
                Ok(results) => results,
                Err(outcome) => {
                    record!(Turn::cancelled(
                        input.clone(),
                        messages[base_len..].to_vec(),
                        cancel_reason(outcome),
                    ));
                    emit!(cancelled_event(outcome));
                    return;
                }
            };
            for call in &calls {
                let result = results
                    .get(&call.call_id)
                    .expect("every issued call has a result");
                messages.push(Message::tool_result(call.call_id.clone(), result.clone()));
            }
        }

        record!(Turn::failed(
            input.clone(),
            messages[base_len..].to_vec(),
            AgentError::MaxRoundsExceeded,
        ));
        emit!(AgentEvent::Lifecycle(LifecycleEvent::Failed {
            error: AgentError::MaxRoundsExceeded,
        }));
    }

    /// Executes all calls in parallel; emits `CallCompleted` in completion
    /// order; returns results keyed by call id. Returns `Err(outcome)` when
    /// cancelled while tools ran (the caller records the turn and emits the
    /// terminal event).
    async fn run_tools_parallel(
        &self,
        calls: &[ToolCall],
        core: &Arc<CancelCore>,
        sender: &mpsc::Sender<AgentEvent>,
    ) -> Result<std::collections::HashMap<CallId, ToolResult>, CancelOutcome> {
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
                    return Err(outcome);
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
                        // The consumer is gone: dropping the handle fired the
                        // cancel signal, so this resolves as user-requested.
                        return Err(CancelOutcome::Signal);
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
                            return Err(CancelOutcome::Signal);
                        }
                        results.insert(call.call_id.clone(), result);
                    }
                }
                None => break, // set drained
            }
        }
        Ok(results)
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

fn cancel_reason(outcome: CancelOutcome) -> CancelReason {
    match outcome {
        CancelOutcome::Timeout => CancelReason::Timeout,
        CancelOutcome::Signal => CancelReason::UserRequested,
    }
}

fn cancelled_event(outcome: CancelOutcome) -> AgentEvent {
    AgentEvent::Lifecycle(LifecycleEvent::Cancelled {
        reason: cancel_reason(outcome),
    })
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
        let runtime = crate::runtime::SynonzRuntime::builder().build();
        let builder = Agent::react(&runtime, DummyModel, dummy_tools());
        assert_eq!(builder.system_prompt, None);
        assert_eq!(builder.max_rounds, None);
        assert_eq!(builder.tools.len(), 2);
    }

    #[test]
    fn research_sets_prompt_and_round_budget() {
        let runtime = crate::runtime::SynonzRuntime::builder().build();
        let builder = Agent::research(&runtime, DummyModel, dummy_tools());
        assert_eq!(
            builder.system_prompt.as_deref(),
            Some(RESEARCH_SYSTEM_PROMPT)
        );
        assert_eq!(builder.max_rounds, Some(RESEARCH_MAX_ROUNDS));
        assert_eq!(builder.tools.len(), 2);
    }

    #[test]
    fn reflection_sets_prompt_without_tools() {
        let runtime = crate::runtime::SynonzRuntime::builder().build();
        let builder = Agent::reflection(&runtime, DummyModel);
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
        let runtime = crate::runtime::SynonzRuntime::builder().build();
        let builder = Agent::research(&runtime, DummyModel, dummy_tools())
            .extend_system_prompt("prefer chinese sources");
        assert_eq!(
            builder.system_prompt.as_deref(),
            Some(&format!("{RESEARCH_SYSTEM_PROMPT}\n\nprefer chinese sources")[..])
        );
    }

    #[test]
    fn system_prompt_overrides_presets() {
        let runtime = crate::runtime::SynonzRuntime::builder().build();
        let builder = Agent::research(&runtime, DummyModel, dummy_tools()).system_prompt("custom");
        assert_eq!(builder.system_prompt.as_deref(), Some("custom"));
    }

    #[cfg(feature = "test-util")]
    #[tokio::test]
    async fn research_preset_drives_a_full_run() {
        use crate::mock::MockModel;
        let runtime = crate::runtime::SynonzRuntime::builder().build();
        let mut conv =
            crate::Conversation::new(&runtime, &crate::Subject::of(crate::SubjectType::User, "u"));
        let model = MockModel::new(vec![vec![ModelStreamItem::Finish {
            message: Message::assistant_text("found the answer"),
            usage: TokenUsage::new(1, 1),
        }]]);
        let agent = Agent::research(&runtime, model, dummy_tools())
            .build()
            .unwrap();
        let output = agent.run(conv.turn_input("research x")).await.unwrap();
        assert_eq!(output.text(), Some("found the answer"));
    }
}
