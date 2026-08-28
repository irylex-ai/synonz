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
