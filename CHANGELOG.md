# Changelog

All notable changes to Synonz are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Multi-agent orchestration (S3) — planned.

## [0.3.0] - unreleased

The reaction-architecture release: the event bus, the Agent-level
Context state engine, the three-slot memory storage, and the complete
conversation lifecycle. **Single-wave publication** — this version ships
everything since 0.1.2, including all work originally staged for the
unpublished 0.2.0 (its full record follows below).

### Highlights

- **Event bus (ADR-0017)**: one dual-lane dispatch facility on the
  runtime. The **observation lane** (bounded queue, non-blocking
  try_send, drop+lag reporting, panic isolation, in-order delivery)
  carries the unified [`SynonzEvent`] vocabulary — three entity families
  (`Turn` / `Conversation` / `Memory`). Registered observers see every
  run unconditionally; the delivery (product narrative) is a separate
  point-to-point pipeline with backpressure — never event-driven.
- **Agent-level Context state engine**: the context is the agent's
  state — `trait Context` with two methods (materialization +
  maintenance), registered per agent. Three strategy slots
  (`ContextAssembler` / `MemorySummarizer` / `ConversationTopicDetector`)
  carry the extension axes; floor parameters (`l1_window` / `l2_cap`)
  are builder values; the whole philosophy replaces via `impl Context`.
  Maintenance runs as one background job per turn (compaction in the
  race-free order summarize → append → pop, then distillation), facts
  ride the bus, and the conversation-end teardown (drain + mechanical
  L2→L3 promotion) is runtime structural behavior.
- **Three-slot memory storage**: `MemoryL1Store` / `MemoryL2Store` /
  `MemoryL3Store` — heterogeneous backends per layer (in-process
  memory / Redis / vector stores), each slot replaced independently.
  [`Memory`] is the first-class domain object: the three slots behind
  one coherent facade, assembled by the runtime (`runtime.memory()` is
  the single authority).
- **Complete conversation lifecycle**: `new` / `with_id` / `of` / `end`
  all take the environment and share one shape (mark, persist, notify).
  `new` persists the initial state from birth (no directory hole) and
  emits `Created`; `end` is idempotent, persists the `ended` state, and
  emits `Ended { reason }` (`Explicit` / `IdleSwept`); the sweep
  structurally skips ended conversations.

### Breaking Changes (event-bus wave)

- `AgentEvent` → `TurnEvent`, delivered inside the `SynonzEvent`
  envelope (`Turn` / `Conversation` / `Memory` entity families;
  three-level tags `type` / `kind` / `event`).
- `Observer::on_event` takes `&SynonzEvent`;
  `ObserverContext.execution_id` is `Option<u64>`; observation is
  unconditional — `AgentBuilder::observability` retired.
- `MemoryStore` (unified three-layer contract) retired → the three
  per-layer contracts + the `Memory` facade; `RuntimeBuilder::memory_store`
  → `memory_l1_store` / `memory_l2_store` / `memory_l3_store`;
  `runtime.memory_store()` → `runtime.memory()`.
- `ContextAssembly` / `AssemblyRequest` / `AssemblyOutput` →
  `ContextAssembler` / `ContextAssemblerInput` / `ContextAssemblerOutput`;
  assembly is an engine slot (`DefaultContext::with_assembler`), not a
  runtime slot; `Conversation::context()` removed.
- `EventPolicy` / `MemoryPolicies` retired: compaction-on-shift and
  end-promotion are structural behaviors; the floors are engine builder
  values.
- `LifecycleEvent::MemoryFlowFailed` → `MemoryEvent::FlowFailed`
  (stage + detail + moment: `AfterTurn` / `AtConversationEnd` /
  `Background` / `Creation`).
- Model/tool events carry `round: Option<usize>` (1-based reasoning
  round; `None` = off-loop maintenance calls).
- `Conversation::new` / `with_id` take the runtime again (the lifecycle
  entry persists the initial state); `end` is `async` and idempotent;
  `ConversationState` gained `ended`.
- `RuntimeBuilder::idle_timeout` → `conversation_idle_timeout`
  (transitional — the automatic monitoring story lands in the Monitor
  ADR).
- Naming clarity: `Agent::run_with`'s parameter renamed to
  `cancel_token` (matches the `CancellationToken` type; the bare
  `token` was referentially unclear). Non-breaking (positional).

### Migration Guidance (event-bus wave)

| 0.2.0 (internal stage) | 0.3.0 |
|---|---|
| `AgentEvent` (match `Lifecycle`/`Model`/`Tool`) | `SynonzEvent::Turn(TurnEvent)`; conversation/memory facts in `Conversation` / `Memory` |
| `Observer::on_event(&AgentEvent)` | `on_event(&SynonzEvent)`; run attribution via `ctx.execution_id` |
| `.observability(true)` on the agent | delete — registered observers see every run |
| `runtime.memory_store()` / `RuntimeBuilder::memory_store` | `runtime.memory()` (facade) / `memory_l1_store` + `memory_l2_store` + `memory_l3_store` |
| `ContextAssembly` impl + `RuntimeBuilder::context_assembly` | `ContextAssembler` impl + `Agent::builder().context(DefaultContext::new().with_assembler(..))` |
| `Conversation::new(&subject)` / `with_id(&subject, id)` | `Conversation::new(&runtime, &subject)` / `with_id(&runtime, &subject, id)` |
| `conv.context(&runtime)` | delete — assembly lives in the agent's engine |
| `MemoryPolicies::new(w, c)` / `EventPolicy` | `DefaultContext::new().l1_window(w).l2_cap(c)`; the end promotion is structural |
| `LifecycleEvent::MemoryFlowFailed { stage, detail }` | `MemoryEvent::FlowFailed { stage, detail, moment }` |
| `conv.end(&runtime)` (sync) | `conv.end(&runtime).await` (idempotent; emits `Ended`) |

## [0.2.0] - not published (all changes ship in 0.3.0)

Historical record of the internal implementation stage (ADR-0014/0015/
0016). Some items were superseded by the 0.3.0 event-bus wave above —
this section is retained as the stage's record.

The contract-convergence release: a single execution face, closed
execution contracts, the full background engine, the complete truth
archive, and the observation bypass. Breaking changes are concentrated
(pre-1.0); every change is recorded below with its migration.

### Summary

0.2.0 implements ADR-0014 (single execution face), ADR-0015 (execution
contract closure and S2 convergence), and ADR-0016 (Observer contract).
The dual ask/run API is replaced by one `run` returning an `Execution`
handle; every execution belongs to a conversation; the agent knows its
runtime; the Context engine owns assemble/archive/compress; all turns —
including failures and cancellations — enter the truth archive; and the
full event stream is available to community observers on a bypass that
never touches the hot path.

### Highlights

- **Single execution face (ADR-0014)**: `agent.run(...)` returns
  `Execution` — a three-in-one handle (narrative stream of
  `ExecutionEvent`, final-output Future, controller). The terminal
  `Completed` event carries the output (stream self-sufficiency).
- **Observation bypass (ADR-0016)**: implement `Observer`, register it
  on the runtime, open the per-agent switch — and receive the **full**
  event stream (including input-side payloads) in emission order,
  panic-isolated, with overflow reported. The hot path pays one
  non-blocking `try_send`.
- **Truth archive (ADR-0015)**: every turn enters the conversation
  history marked with its outcome (`Completed` / `Failed` /
  `Cancelled`) — failures keep their audit trail; only success turns
  feed the memory layers.
- **Background engine**: `Context` owns assemble / archive / compress;
  assembly strategies read **memory only** (type-locked request);
  memory-flow failures are events, never silent.

### Breaking Changes

- `Agent::ask` / `Answer` removed — use `agent.run(...)` (await
  semantics identical).
- The old event-consumption `Run` removed — iterate `ExecutionEvent` on
  `Execution`; the full `AgentEvent` stream moved to the Observer
  bypass.
- Bare-string executions removed — every execution is
  `agent.run(conv.turn_input(...))`; there is no conversation-less run.
- `Conversation` is a pure data entity (no runtime field):
  `Conversation::new(&subject)` / `with_id(&subject, id)` no longer take
  the runtime; `end(&runtime)` and `context(&runtime)` take it
  explicitly; persistence is driven by the operating runtime. Cross-
  runtime mixing is structurally impossible instead of rejected.
- `Agent::builder().runtime(&runtime)` required; presets take the
  runtime first (`Agent::react(&runtime, model, tools)`, etc.).
- `Agent::with_context` removed — the background is derived from the
  conversation.
- `Turn` gained `outcome` (`Completed` / `Failed` / `Cancelled`);
  failed and cancelled turns are recorded (previously they were not).
- `Conversation::truncate_last` / `clear` / `fork` removed;
  `push_turn` is internal.
- `ConversationHistory` strategy removed — memory is the sole assembly
  source; the default is `LayeredMemory`.
- `ModelRequest` lost `params` — bind inference parameters on the
  adapter (`Client::new(...).params(...)`, `temperature` / `max_tokens`).
- `Conversation::end` returns typed failures
  (`Vec<(MemoryFlowStage, String)>`).
- `LifecycleEvent::MemoryFlowFailed` added (non-terminal; memory-flow
  failures surface as events).

### Migration Guidance

| 0.1.x | 0.2.0 |
|---|---|
| `agent.ask(x).await?` | `agent.run(conv.turn_input(x)).await?` |
| `agent.run("text")` | `agent.run(conv.turn_input("text"))` |
| iterate `AgentEvent` on `Run` | iterate `ExecutionEvent` on `Execution` (full stream → `Observer`) |
| `agent.run_with(x, token)` | `agent.run_with(conv.turn_input(x), token)` |
| `with_context(conv.context())` | delete — the background is derived |
| register `ConversationHistory` | default `LayeredMemory` (or a custom strategy) |
| `truncate_last` / `clear` / `fork` | removed (window management lives in the background engine) |
| `ModelRequest { .., params }` | `ModelRequest { messages, tools }`; params on the adapter |

### Fixed

- `sweep_stale` never worked: the persisted subject identity was
  reconstructed with a second wrapping (every conversation was silently
  skipped) and the subject type was hardcoded. Fixed with symmetric
  encode/decode covering both subject types.

## [0.1.2] - 2026-09-07

Internal quality release. No public API changes and no behavior changes;
`ask` and `run` resolve to identical results before and after.

### Changed

- `Answer` and `Run` are now peer handles, each wrapping its own internal
  `AgentRunner` (ADR-0013) — `Answer` is no longer a filtered view over
  `Run`. Public API signatures are unchanged.
- The agent's default time budget is applied uniformly inside the shared
  spawn path (`run` / `run_with` / `ask` behave identically); `Answer`
  holds the conversation serialization guard directly instead of
  inheriting it through `Run`.

## [0.1.1] - 2026-08-29

### Fixed

- Crates now ship their README, so crates.io pages render it instead of
  reporting "no README.md file" (packaging-only fix; no code changes).

## [0.1.0] - 2026-08-29

Initial release. Pre-1.0: APIs are stable in intent but not yet committed;
expect breaking changes between 0.x versions.

### Added

#### Core agent framework (S1)

- `Agent` (stateless configuration) with builder, presets
  (`react` / `research` / `reflection`), explicit system prompt, and a
  round budget (`max_rounds`, default 16, exceeding fails explicitly).
- Reasoning loop: model-stream consumption, parallel tool execution with
  call-id pairing, soft tool-failure feedback, and the canonical message
  form (role + content blocks) with validated invariants.
- Two-level event model (`Lifecycle` / `Model` / `Tool`) — a run's single
  ordered narrative, serializable for record/replay; `CallPurpose`
  distinguishes reasoning from auxiliary calls.
- Converged handle family: `Answer` (streaming-first) and `Run` (event
  stream + awaitable result), with `cancel()`, `with_timeout`, and
  drop-based cancellation converging on one signal.
- `Tool` contract (dynamic core + `#[derive(Tool)]` typed ergonomics) and
  `Model` contract (single stream method, non-streaming as a degenerate
  stream, no hidden retries).
- Adapters: `synonz-openai`, `synonz-anthropic`, `synonz-mcp` (official
  `rmcp` SDK), plus a `MockModel` test utility (`test-util` feature).

#### Conversation, memory, and context (S2)

- `Conversation` entity (identity, turns, auto-save, fork) and the
  `TurnInput` parameter object — the single input model for `ask`/`run`.
- `Subject` (identity = `(SubjectType, id)`), `SynonzRuntime` (explicit
  bootstrap, startup registry, in-process defaults).
- Layered memory: L1 (session turns), L2 (summaries), L3 (cross-session
  knowledge) behind the `MemoryStore` contract, with trigger policies
  (`TurnCount` / `L2Overflow` mandatory floors, stackable event policies).
- `Context` (session-scoped runtime) and the `ContextAssembly` contract
  (`LayeredMemory` default, `ConversationHistory` built-in): fresh
  assembly per ask, persona and memory recall as distinguishable
  messages, summarization visible as `ContextManagement` events.

### Changed

- `Conversation::new()` now takes `(&runtime, &subject)`; conversations
  require an explicit runtime and subject (breaking, pre-1.0).

### Removed

- `Conversation::import` (restoration goes through
  `Conversation::of(&runtime, &subject, id)`).
- `RunStream` renamed to `Run`.

## Version history

- 0.2.0 — contract convergence: single execution face, closed execution
  contracts, truth archive with outcomes, Observer bypass (M12–M15).
- 0.1.2 — internal refactor: `Answer`/`Run` as peer handles (ADR-0013).
- 0.1.1 — README packaging fix.
- 0.1.0 — first release (S1 + S2 complete; M0–M11 milestones).
