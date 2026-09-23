# Synonz

**Explicit, controllable, observable agent engineering infrastructure in
pure Rust.**

Synonz is an open-source framework for building, running, and orchestrating
AI agents. It is designed around four commitments: restrained abstraction
(no framework magic), controllable behavior (you can always reason about
what an agent is doing), complete lifecycle semantics (cancellation is a
first-class citizen), and built-in observability (an event bus carries
every run's full story — the hot path pays one non-blocking `try_send`).

```rust
use synonz::{Subject, SubjectType, SynonzRuntime};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = SynonzRuntime::builder().build();
    let mut conv = synonz::Conversation::new(
        &runtime,
        &Subject::of(SubjectType::User, "demo"),
    );
    let agent = synonz::Agent::builder()
        .runtime(&runtime)
        .model(synonz_openai::Client::from_env()?)
        .system_prompt("you are a helpful assistant")
        .build()?;

    let output = agent.run(conv.turn_input("hello!")).await?;
    println!("{}", output.text().unwrap_or_default());
    Ok(())
}
```

## Workspace

| Crate | Purpose |
|---|---|
| `synonz` | Core: `Agent`, the built-in context engine, the event bus and `SynonzEvent` vocabulary, the memory contract family (`Memory` / `MemoryContextAssembler` / `MemoryPipeline` + providers) with a bundled in-process default, the time-driven scheduler, canonical messages, the reasoning loop |
| `synonz-layered-memory` | Official layered memory component: L1 turn-level entries, L2 batch compaction, L3 entity graph with vectors (distillation), four replaceable storage contracts, a configurable schema, semantic strategies, and its own observation face |
| `synonz-derive` | `#[derive(Tool)]` typed tool ergonomics (re-exported by `synonz`) |
| `synonz-openai` | OpenAI-compatible `Model` adapter |
| `synonz-anthropic` | Anthropic `Model` adapter |
| `synonz-mcp` | MCP tool bridge (official `rmcp` SDK) |

## Examples

Run them with `cargo run -p synonz-examples --bin <name>`:

| Example | Notes |
|---|---|
| `custom_tool` | `#[derive(Tool)]` + agent loop (offline) |
| `layered_memory` | The layered memory component end to end (offline) |
| `events` | Observing the full event stream on the bus (offline) |
| `cancellation` | External cancel signal / timeout / drop entries (offline) |
| `scheduler` | Custom scheduled tasks + the system lifecycle: Monitor and `shutdown()` (offline) |
| `mcp_tools` | Bridging an embedded MCP server (offline) |
| `openai_chat` | Real chat, needs `SYNONZ_OPENAI_API_KEY` |
| `anthropic_chat` | Real chat, needs `SYNONZ_ANTHROPIC_API_KEY` |
| `chat_tui` | Interactive TUI chat; configure the endpoint on first screen |

## Documentation

- Architecture decisions: `docs/adr/` (ADR-0001 and onward)
- Architecture overview: `docs/architecture/v8.zh-CN.md` (the 0.7.0
  target-state view; v7 and the earlier documents are retained as era
  snapshots)
- Implementation plan: `docs/design/implementation-plan-0.7.0.zh-CN.md`
  (the 0.6.0 and earlier plans are retained for their stage records)
- API migration: `docs/design/migration-0.7.0.zh-CN.md` (0.6.0 → 0.7.0;
  the earlier migration guides are retained alongside their plans)
- Changelog: `CHANGELOG.md` (migration tables inside for the breaking
  waves)

## Status

Synonz is pre-1.0: APIs are stable in intent but not yet committed. The
project's architecture is documented via ADRs; the license is
Apache License 2.0 (the `LICENSE` file will be added separately).
