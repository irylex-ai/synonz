# Changelog

All notable changes to Synonz are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versions follow [Semantic Versioning](https://semver.org/).

## [0.7.0] - 2026-09-21

The memory-componentization release: the framework keeps the memory
contracts, the built-in engine and a bundled in-process default, while
the layered memory model (L1 double-track messages, L2 event summaries,
L3 entity graph and vectors) becomes the official component
`synonz-layered-memory`. Topic detection returns to an independent
Agent-level extension point, the read phase owns the model-visible
message frame, and management gains opaque scope addressing.
**Breaking** — migrate with the table below and
`docs/design/migration-0.7.0.zh-CN.md`.

### Highlights

- **Memory contract family**: `Memory` (item management),
  `MemoryContextAssembler` (read), `MemoryPipeline` (write: the
  `archive_turn` / `spawn_task` / `finalize_conversation` hooks) plus
  the factories `MemoryProvider` / `RewriterProvider` /
  `TopicDetectorProvider`. The `Context` type is internalized (crate
  engine), never public.
- **Bundled default**: an in-process, non-layered provider ships in the
  crate — zero configuration, `runtime.memory()` always available;
  registering a provider replaces it.
- **Complete message frame**: the assembler returns the model-visible
  frame after the agent's system message (the current turn's user
  message included); the core appends nothing. The contract requires a
  usable frame, and the core substitutes the original input when the
  frame comes back empty (never silent).
- **Narrated model access**: implementations declare optional models on
  their factories; the framework resolves `provider model ?? agent
  model` at use time and hands over a narrated handle (auxiliary calls
  are reported; no stream deltas).
- **Opaque scope addressing**: `MemoryScope` (an opaque string value)
  partitions memory; `MemoryItem.scope` exposes it, `MemoryQuery.scope`
  filters it (`None` merges every partition), `forget_matching` batches
  by it, and the management facts (`Updated` / `Removed`) carry it.
- **Independent topic detection**: `TopicDetectorProvider` (Agent
  level, optional model, same resolution rule as the rewriter); the
  core writes the verdict back, emits `TopicShifted` on change, and
  exposes the change to the pipeline hooks through their context. The
  pipeline has three hooks (no `detect_topic`).
- **Official layered component** (`synonz-layered-memory`): L1
  turn-level entries (one entry per completed turn, ten turns,
  conversation-scoped), L2 batch compaction (one summarizer call per
  batch producing 1..N entries with content history and importance), L3
  entity graph plus vectors distilled on topic shifts and conversation
  ends (cross-conversation, configurable schema, alias merging with an
  LLM judgment, anchor-first walk, cold-start gate), the four-phase
  pipeline, four replaceable storage contracts, a replaceable embedding
  port, semantic strategies, and its own observation face. The
  component's model was redefined by ADR-0028.

### Added

- **Contracts** (`synonz`): `Memory` (trait), `MemoryContextAssembler`,
  `MemoryContextAssembleInput` / `MemoryContextAssembleOutput`,
  `MemoryPipeline`, `PipelineTurnContext` /
  `PipelineConversationContext`, `MemoryFailure`, `RewriteInput`,
  `TopicDetector` / `TopicDetectInput`, `MemoryProvider`,
  `RewriterProvider`, `TopicDetectorProvider`, `MemoryScope`.
- **Runtime** (`synonz`): `RuntimeBuilder::memory_provider(...)`;
  `SynonzRuntime::memory() -> Arc<dyn Memory>`;
  `AgentBuilder::rewriter_provider(...)` /
  `AgentBuilder::topic_detector_provider(...)`.
- **Events** (`synonz`): `MemoryEvent::Failed { stage: String, detail,
  moment }`; `Updated` / `Removed` carry `scope`;
  `MemoryFailedMoment` (renamed).
- **Views** (`synonz`): `MemoryItem::scope`, `MemoryQuery.topic` /
  `MemoryQuery.scope`, flat `MemoryListCursor`.
- **Component** (`synonz-layered-memory`, new crate): the layered model
  (`L1MemoryEntry` / `L2MemoryEntry` / `L3MemoryGraph` /
  `L3MemoryGraphEntity` / `L3MemoryGraphEdge`), four storage contracts
  (`L1MemoryStore` / `L2MemoryStore` / `L3MemoryGraphStore` /
  `L3MemoryVectorStore`) with in-process defaults, `Embedding` (with the
  deterministic `HashEmbedding`), the semantic strategies
  (`L2MemorySummarizer` / `L3MemoryEntityExtractor` /
  `LayeredMemoryContextRewriter` with prompt-driven defaults),
  `LayeredMemoryObserver` / `LayeredMemoryEvent` (ADR-0028 variants),
  `LayeredMemoryConfig`, `L3MemorySchema`, `MemoryScopeResolver`, the
  provider (builder), the actuator (`LayeredMemoryActuator`:
  `l2_memory_entries` / `l3_memory_entities` / `l3_memory_relations` /
  `forget_l3_memory_relation`, every read scoped to the subject), and the
  read-side providers (`CoreferenceInputRewriterProvider` /
  `SimilarityTopicDetectorProvider`).

### Changed

- The read phase owns the model-visible input: the assembler's frame
  goes straight to the model; the agent's system prompt precedes it.
- `Turn.messages` records the complete frame plus the turn's subsequent
  messages; `Turn.input` stays the original text. The system prompt is
  agent configuration and is not archived.
- Rewriter history comes from the truth domain (recent successful
  turns) instead of a memory layer window.
- The conversation-end teardown drains background tasks and then calls
  the pipeline's `finalize_conversation` hook (the mechanical L2→L3
  promotion is gone).
- The component's management face is subject-isolated: conversations
  register their owner at archive time; `list` / `get` / `edit` /
  `forget` / `forget_matching` (explicit `scope` / `conversation_id`
  filters included) only see the subject's own partitions, and the
  actuator reads follow the same rule. The management face creates no
  entries: new memory comes from conversations, and direct store writes
  are out-of-framework imports the face does not index.
- `L3MemoryGraphEdge` uses `from` / `to` (was `subject` / `object`); the
  component's vocabulary is conversation, not session.

### Breaking Changes

- Removed from the public surface (no aliases): `Context`,
  `ContextAssembler` / `ContextAssemblerInput` /
  `ContextAssemblerOutput`, `AssemblyFailure`, `MemorySummarizer`,
  `MemoryDistiller`, `ConversationTopicDetector` / `TopicDecision`,
  `MemoryFlowError`, `MemoryFlowStage`, `MemoryType`,
  `MemoryStoreQuery`, `L1Entry` / `L2Entry` / `L3Entry` / `L3Identity`,
  `MemoryL1Store` / `MemoryL2Store` / `MemoryL3Store`,
  `MemoryFlowFailedMoment`, `MemoryEvent::{Compacted, Distilled,
  Promoted, FlowFailed}`.
- `RuntimeBuilder::memory_l1_store` / `memory_l2_store` /
  `memory_l3_store` are gone; use `memory_provider(...)`.
- `Memory` is a trait; `Memory::edit` takes `&str`; `MemoryItem` gains
  `scope` and loses `memory_type`; `MemoryQuery` loses `memory_type`.
- `MemoryEvent::Updated` / `Removed` gain `scope`.
- `EventSink` and `ConversationTaskSpawner` leave the public surface
  (implementation-side hooks use the pipeline context instead).
- 0.6.0 serialized data is not supported (no data migration).

### Migration

| Before (0.6.0) | After (0.7.0) |
|---|---|
| `AgentBuilder::context(...)` / `Context` | removed; use `rewriter_provider(...)` and the runtime's memory provider |
| `ContextAssembler*` | `MemoryContextAssembler` / `MemoryContextAssembleInput` / `MemoryContextAssembleOutput` (the frame is the complete model-visible input) |
| `TurnInputRewriter::rewrite(input, history)` | `rewrite(RewriteInput { input, history, model })` |
| `ConversationTopicDetector` | `TopicDetector` + `AgentBuilder::topic_detector_provider(...)` |
| `MemorySummarizer` / `MemoryDistiller` | the component's strategies or your own pipeline hooks |
| `MemoryFlowStage` / `MemoryFlowError` / `AssemblyFailure` | `MemoryFailure { stage: String, detail: String }` |
| `MemoryEvent::FlowFailed` | `MemoryEvent::Failed { stage, detail, moment }` |
| `MemoryEvent::Compacted` / `Distilled` / `Promoted` | component observation face (`LayeredMemoryObserver`) |
| `RuntimeBuilder::memory_l1/l2/l3_store(...)` | `RuntimeBuilder::memory_provider(...)` |
| layered types (`L1Entry` / `L2Entry` / `L3Entry`) | component documents (`L1MemoryEntry` / `L2MemoryEntry` / `L3MemoryGraphEntity` / `L3MemoryGraphEdge`; ADR-0028) |
| `MemoryItem.memory_type` / `MemoryQuery::with_memory_type` | the component's `LayeredMemoryActuator` documents (the Summary/Knowledge mapping is documentation, not a type) |
| `memory.reader(&subject)` | `memory.reader()` — the read-only projection of the memory (item-level `list` / `get` with the subject per call; construction internal) |
| rewriter history from the L1 window | truth-domain recent successful turns |

### Notes

- Boundaries (no current requirement): scope conversion (`move` /
  `promote` / `demote`), scope acquisition (application layer), L1
  retention and session cleanup, semantic search and contradiction
  resolution (component strategy layer), real storage backends
  (Redis / MongoDB / Neo4j / Milvus) as separate packages.

## [0.6.0] - 2026-09-19

The memory-management release: memory entries become addressable items
with an application management surface, equal-power in-place updates,
effective forgetting and a layer-agnostic application face.
**Breaking** — migrate with the table below and
`docs/design/migration-0.6.0.zh-CN.md`.

### Highlights

- **Memory management surface**: `Memory` is the application face —
  `list` / `get` / `edit` / `forget` / `forget_matching` over
  `MemoryItem` (L2 `Summary` + L3 `Knowledge` items), with keyset
  pagination (`MemoryQuery` / `MemoryListCursor` / `MemoryPage`) and
  content-free `Updated` / `Removed` facts.
- **Stable ids and freshness**: entries carry framework-generated ids
  and `updated_at`; listing, cursors and recall rank by
  `(updated_at desc, id asc)`.
- **Equal-power updates**: user edits and system distillation share one
  in-place path (same identity slot, id preserved, last writer wins);
  content quality belongs to the strategies, not to the kernel.
- **Effective forgetting**: reads go through the management face; the
  in-flight maintenance re-validates its sources by id and claims them
  by id before writing — forgotten sources are never written back.
  Re-learning the fact from new evidence is allowed.
- **Application face / internal separation**: layer primitives and the
  three store slots move into the crate-internal `MemoryLayerStore`;
  `Memory::reader`'s constructor is internalized (`MemoryReader` stays
  public for strategy slots).

### Added

- **Memory management** (`synonz`): `MemoryType`, `MemorySource`,
  `MemoryItem`, `MemoryQuery`, `MemoryCursor`, `MemoryListCursor`,
  `MemoryPage`, `MemoryForgetFailure`, `MemoryForgetResult`,
  `MemoryStoreQuery`; `Memory::list` / `get` / `edit` / `forget` /
  `forget_matching`.
- **Store contracts** (`synonz`): `MemoryL2Store` / `MemoryL3Store`
  gain required `get` / `update` / `remove` / `list`;
  `MemoryStoreError::EntryNotFound`.
- **Events** (`synonz`): `MemoryEvent::Updated` / `Removed`
  (content-free).
- **Entries** (`synonz`): `id` (UUID v4) and `updated_at`;
  `L2Entry::topic` and `L2Entry::with_topic`.
- **Test utilities** (`test-util`): `Memory::seed_l1` / `seed_l2` /
  `seed_l3`, `Memory::reader_for_tests`,
  `Memory::l1_len_for_tests` / `l2_len_for_tests` / `l3_len_for_tests`.

### Changed

- Recall ranking uses `(updated_at desc, id asc)`.
- Distillation and conversation-end promotion claim sources by id
  (forgotten sources are skipped; a source set that changes during a
  transform discards that output with a visible `FlowFailed` fact).
- `MemoryL3Store::upsert` preserves the existing id on same-identity
  replace.

### Breaking Changes

- `Memory` no longer exposes the layer primitives (`l1_append` /
  `l1_window` / `l1_pop_oldest` / layer lengths, `l2_append` / `l2_read`
  / `l2_pop_oldest`, `l3_upsert` / `l3_query` / `l3_len`); use the
  management surface (applications) or the crate-internal mechanism
  (framework).
- `Memory::reader` is no longer publicly constructible (`MemoryReader`
  the type is unchanged and still handed to strategy slots through
  their payloads).
- `MemoryL2Store` / `MemoryL3Store` implementations must add the four
  by-id methods (compile-time requirement).
- `L2Entry` / `L3Entry` gain fields (`id`, `updated_at`; L2 `topic`) —
  downstream struct literals must move to the constructors.
- Recall ordering changes from `created_at` to `updated_at`.

### Migration

| Before (0.5.0) | After (planned 0.6.0) |
|---|---|
| `memory.l1_append(...)` (application seeding) | `test-util` `memory.seed_l1(...)` (tests/fixtures) |
| `memory.l1_len` / `l2_len` / `l3_len` (application reads) | `memory.list(...)` (management) or `test-util` `*_len_for_tests` |
| `memory.l3_upsert(entry)` (application write) | no application write path: conversations distill; imports run out-of-band; tests seed |
| `memory.reader(&subject)` | internal; strategy slots receive `MemoryReader` through payloads; tests use `reader_for_tests` |
| custom `MemoryL2Store` / `MemoryL3Store` | implement `get` / `update` / `remove` / `list`; keep ids on same-identity upsert |
| recall by `created_at` | by `(updated_at desc, id asc)` |

### Notes

- Boundaries (no current requirement): application `add`, out-of-band
  import guarantees, strict global time order, semantic search,
  fact aggregation (strategy-level), physical/backup erasure, L1
  retention/session cleanup.

## [0.5.0] - 2026-09-14

The context-engine extension release: the engine becomes a concrete
type with a read/write phase asymmetry, a read-only memory view, input
rewriting, a distillation slot, and engine-model narration. **Breaking**
— migrate with the table below.

### Highlights

- **Concrete engine**: `DefaultContext` is now `Context`, a concrete
  type (the open `trait Context` is gone). The agent holds
  `Arc<Context>`; `assemble` / `on_turn_completed` are framework-internal
  entries, and engine behavior is a configuration of narrow strategies.
- **Read/write asymmetry**: the read phase is fully replaceable
  (`ContextAssembler` + `TurnInputRewriter`), reading through a
  read-only `MemoryReader`; the write phase is framework-owned, with
  `ConversationTopicDetector` / `MemorySummarizer` / `MemoryDistiller`
  as its customization points.
- **Input rewriting**: `TurnInputRewriter` runs before assembly
  (coreference resolution and similar preprocessing); the model view
  flows into `ContextAssemblerInput::rewritten_input`, while the truth
  domain (canonical messages / turns / L1) keeps the original text.
- **Engine model**: `Context::with_model` sets the model the engine's
  maintenance calls resolve to (default: the agent's model). Auxiliary
  calls are narrated by the framework (`Requested` / `Responded`, never
  `StreamDelta`).
- **Distillation slot**: `MemoryDistiller` (default: mechanical
  promotion); the engine transforms first and pops after — a failed
  transform keeps L2.

### Added

- **Input rewriting** (`synonz`): `TurnInputRewriter`,
  `ContextAssemblerInput::rewritten_input`, `Context::with_rewriter`,
  and the `MemoryFlowStage::Rewrite` flow-failure stage.
- **Read-only memory view** (`synonz`): `MemoryReader` (subject-scoped;
  no write or curation methods) and `Memory::reader`.
- **Distillation strategy** (`synonz`): `MemoryDistiller` and
  `Context::with_distiller` (mechanical default).
- **Engine model** (`synonz`): `Context::with_model`.
- **Memory entry API** (`synonz`): `L1Entry::new` and
  `L1Entry::created_at`; `L2Entry::created_at`.
- **Topic accessors** (`synonz`): `Conversation::topic` /
  `Conversation::set_topic` are now public.

### Changed

- Memory entry types are named by layer: `SummaryBlock` → `L2Entry`,
  `KnowledgeFragment` → `L3Entry`, `FragmentIdentity` → `L3Identity`.
- `MemorySummarizer::summarize` no longer receives the event sink — the
  framework narrates model calls.
- `with_summary_model` → `with_model`.

### Breaking Changes

- **`trait Context` removed**: custom engines migrate to the concrete
  `Context` configuration (read strategy/rewriter; write sub-hooks).
  `DefaultContext` is now `Context`; the agent holds `Arc<Context>`.
- **`TurnContext` removed**: the public maintenance payload is gone;
  `on_turn_completed` is framework-internal with an explicit parameter
  set.
- **`ContextAssemblerInput.memory` → `reader`** (`&Memory` →
  `MemoryReader`); `ContextAssemblerInput::new` signature updated.
- **`MemorySummarizer::summarize(entries, model, events)` →
  `summarize(entries, model)`**.
- **`with_summary_prompt` removed** (prompt is strategy content);
  **`with_summary_model` renamed to `with_model`**.
- **Memory entry renames** (see Changed).
- `MemoryFlowStage::Rewrite` added (additive).

### Migration

| Before (0.4.0) | After (0.5.0) |
|---|---|
| `impl Context for MyEngine` | configure `Context` (`with_assembler` / `with_rewriter` / `with_topic_detector` / `with_summarizer` / `with_distiller`); whole write-phase replacement is retired |
| `DefaultContext` | `Context` |
| `Agent` holds `Arc<dyn Context>` | `Arc<Context>`; `AgentBuilder::context(Context)` |
| `ContextAssemblerInput.memory` | `ContextAssemblerInput.reader` |
| — | `ContextAssemblerInput.rewritten_input` (engine-filled) |
| `summarize(entries, model, events)` | `summarize(entries, model)` |
| hard-coded mechanical distillation | `MemoryDistiller` (default mechanical; transform-then-pop) |
| `with_summary_model` | `with_model` |
| `with_summary_prompt` | removed — implement `MemorySummarizer` |
| `SummaryBlock` / `KnowledgeFragment` / `FragmentIdentity` | `L2Entry` / `L3Entry` / `L3Identity` |
| `L1Entry` without constructor | `L1Entry::new` + `created_at`; `L2Entry::created_at` |
| `topic()` / `set_topic()` crate-private | public |
| `TurnContext` | removed (framework-internal) |

### Notes

- Without a rewriter the read phase is behavior-identical to 0.4.0.
- The engine model resolves as `with_model ?? agent_model`; auxiliary
  model calls emit `Requested` / `Responded` with `round: None` and
  never `StreamDelta`.
- Boundaries (no current requirement): whole write-phase replacement,
  conversation-end flush hooks, remote storage in the default memory
  pipeline, streaming-narration switches.

## [0.4.0] - 2026-09-13

The scheduling-and-lifecycle release: the time-driven Scheduler
component, the automatic conversation lifecycle (Monitor), explicit
shutdown, and the conversation query surface. **Breaking** — migrate
with the table below.

### Highlights

- **Scheduler component**: the public `Scheduler` — periodic tasks with
  an immediate first fire, three overlap policies (`Skip` /
  `Concurrent` / `Queue`, the coalescing catch-up), a read-only
  `tasks()` snapshot, and per-task cancellation. One lightweight timing
  thread per scheduler; executions run on the host executor (the worker
  pool), never on the timing thread.
- **Automatic lifecycle (Monitor)**: configuring
  `conversation_idle_timeout` registers the Monitor at build; the
  runtime automatically ends idle conversations (`IdleSwept`) and
  reconciles conversations left open by a previous process (the
  immediate first sweep — crash recovery).
- **Explicit shutdown**: `runtime.shutdown().await` stops the system
  scheduler, ends every conversation the runtime owns
  (`ConversationEndReason::Shutdown`), and flushes the observation queue
  before returning. Idempotent.
- **Conversation query surface**: `ConversationStore::list_stale`
  (sweep pushdown) and `list` (metadata keyword + keyset pagination)
  with `ConversationSummary` / `ConversationCursor` / `ConversationPage`
  / `ConversationQuery`; `runtime.list_conversations` is the access
  path.
- **Read-only system snapshot**: `runtime.scheduler_snapshot()`
  (name / period / policy / next trigger / running) — the system
  scheduler is not reachable for registration.

### Added

- **Runtime-adjustable model options** (`synonz-openai` /
  `synonz-anthropic`): `Client::options` / `set_options` / `set_model` —
  changes take effect on the next request, no rebuild. Each adapter
  exposes its provider-specific `ModelOptions` (serde; usable in
  application config records): `reasoning_effort` (OpenAI) and `effort`
  + `thinking` (Anthropic).
- **Reasoning deltas** (`synonz` / `synonz-openai` / `synonz-anthropic`):
  `ModelDelta::Reasoning` carries the model's streamed thinking when the
  provider exposes it (OpenAI-compatible `reasoning_content` /
  `reasoning` / `reasoning_text`; Anthropic `thinking_delta`). Reasoning
  is narration only — it never enters the canonical assistant message.
- **Model listing** (`synonz-openai`): `Client::list_models()` calls the
  standard `GET /models` route (sorted, deduplicated ids).
- **Custom request headers** (`synonz-openai`): `Client::header(name,
  value)` adds default headers to every request — a dedicated
  `User-Agent` and/or a session header such as OpenCode Go's
  `x-opencode-session`; `USER_AGENT`, `HeaderName`, and `HeaderValue`
  are re-exported by the adapter.

### Changed

- Adapter `Client` clones share the same live options and model name
  (previously clones carried independent copies); connection and
  credentials stay construction-bound.

### Fixed

- **Agent**: the final assistant message of a completed turn is archived
  into the turn record and the L1 memory window (it was dropped for
  text-only turns), so the next request's assembled context contains the
  previous answer instead of the bare question.
- `synonz-openai` streamed tool calls: the streaming path now parses
  `tool_calls` at all (it previously yielded text only), tolerates
  providers that repeat empty `id` / `name` fragments, treats an empty
  argument string as `{}`, and fails clearly when a tool call arrives
  without a name.
- `synonz-openai` tool-result requests: tool messages are spliced into
  the `messages` array instead of nested as arrays (every tool
  round-trip was rejected by providers); tool-call turns keep the
  canonical `content: null`, and providers that omit call ids get a
  local fallback id.

### Breaking Changes

- **`ConversationStore::list()` removed**: replaced by
  `list_stale(before, after, limit)` and `list(query)` (both keyset
  paginated, returning `ConversationSummary` entries without turns).
  Trait implementations must be updated.
- **`SynonzRuntime::sweep_stale()` removed**: the Monitor is the only
  sweep path. Rely on the automatic Monitor (configure
  `conversation_idle_timeout`) or call `shutdown()` for teardown.
- **Build contract**: configuring `conversation_idle_timeout` requires
  an execution environment — call `build()` inside an async context or
  inject a handle via `RuntimeBuilder::executor`; otherwise `build()`
  panics (a configuration error).
- **`TaskRegistry` renamed to `ConversationTaskSpawner`**: the type now
  lives in `synonz::runtime` (root-exported), and the payload field is
  `TurnContext::task_spawner` (was `tasks`).
- `ConversationEndReason::Shutdown` and `MemoryFlowStage::Drain` added
  (additive variants).

### Migration

| Before (0.3.x) | After (0.4.0) |
|---|---|
| `store.list()` | `store.list_stale(before, cursor, limit)` / `store.list(ConversationQuery::new(limit)…)` |
| full `ConversationState` in listings | `ConversationSummary` (turns stay on `load` / `Conversation::of`) |
| `runtime.sweep_stale().await` | automatic Monitor; `runtime.shutdown().await` for end-of-life teardown |
| `runtime` built anywhere | `build()` inside an async context (or `.executor(handle)`) when `conversation_idle_timeout` is set |
| maintenance table (internal) | conversation table (internal): `new` / `with_id` / `of` register, any end removes |
| `synonz::TaskRegistry` / `TurnContext::tasks` | `synonz::ConversationTaskSpawner` / `TurnContext::task_spawner` |

### Notes

- The Monitor tick derives from the idle timeout:
  `clamp(timeout / 4, 100ms, 60s)`.
- The conversation-end drain is bounded (60s per conversation); stuck or
  panicked maintenance tasks surface as `FlowFailed { stage: Drain }`.

## [0.3.1] - 2026-09-10

### Fixed

- Documentation packaging: the five crate-level `README.md` files
  (what crates.io renders per crate) were 0.1.1-era snapshots pointing
  at the s2 architecture and the v1 plan — synced to the current
  repository README (0.3.0 reality: v4 architecture, the 0.3.0
  implementation plan, the bus examples table). Docs-only release; no
  code changes.

## [0.3.0] - 2026-09-09

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
