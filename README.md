# Synonz

**Explicit, controllable, observable agent engineering infrastructure in
pure Rust.**

Synonz is an open-source framework for building, running, and orchestrating
AI agents. It is designed around four commitments: restrained abstraction
(no framework magic), controllable behavior (you can always reason about
what an agent is doing), complete lifecycle semantics (cancellation is a
first-class citizen), and built-in observability (execution *is* an event
stream).

```rust
use synonz::Agent;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = Agent::builder()
        .model(synonz_openai::Client::from_env()?)
        .system_prompt("you are a helpful assistant")
        .build()?;

    let answer = agent.ask("hello!").await?;
    println!("{}", answer.text().unwrap_or_default());
    Ok(())
}
```

## Workspace

| Crate | Purpose |
|---|---|
| `synonz` | Core: `Agent`, `Tool` and `Model` contracts, the event model, canonical messages, the reasoning loop |
| `synonz-derive` | `#[derive(Tool)]` typed tool ergonomics (re-exported by `synonz`) |
| `synonz-openai` | OpenAI-compatible `Model` adapter |
| `synonz-anthropic` | Anthropic `Model` adapter |
| `synonz-mcp` | MCP tool bridge (official `rmcp` SDK) |

## Examples

Run them with `cargo run -p synonz-examples --bin <name>`:

| Example | Notes |
|---|---|
| `custom_tool` | `#[derive(Tool)]` + agent loop (offline) |
| `events` | Consuming the run's event narrative (offline) |
| `cancellation` | Token / timeout / drop cancellation entries (offline) |
| `mcp_tools` | Bridging an embedded MCP server (offline) |
| `openai_chat` | Real chat, needs `SYNONZ_OPENAI_API_KEY` |
| `anthropic_chat` | Real chat, needs `SYNONZ_ANTHROPIC_API_KEY` |

## Documentation

- Architecture decisions: `docs/adr/` (ADR-0001 and onward)
- Architecture overview: `docs/architecture/v1.zh-CN.md`
- Implementation plan: `docs/design/implementation-plan-v1.zh-CN.md`

## Status

Synonz is pre-1.0: APIs are stable in intent but not yet committed. The
project's architecture is documented via ADRs; the license is
Apache License 2.0 (the `LICENSE` file will be added separately).
