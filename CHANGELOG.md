# Changelog

All notable changes to Synonz are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Multi-agent orchestration (S3) — planned.

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

- 0.1.0 — first release (S1 + S2 complete; M0–M11 milestones).
