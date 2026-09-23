# synonz-layered-memory

**The official layered memory component for
[Synonz](https://crates.io/crates/synonz) — turn-level working memory,
batch-compacted conversation memory, and a cross-conversation entity graph
with vectors.**

The component implements Synonz's memory contract family with a
three-layer model:

- **L1 working memory**: one entry per completed turn (the user's input
  plus the final answer), conversation-scoped and bounded to the ten most
  recent turns — a recall hit brings back the whole turn;
- **L2 event summaries**: conversation entries produced by **batch
  compaction** of L1 (when the window fills, the topic shifts, or the
  conversation ends); each entry keeps its current content, the history
  of previous contents, and an importance used by recall ranking;
- **L3 long-term memory**: a cross-conversation entity graph plus entity
  vectors, **distilled** on topic shifts and conversation ends. Entity
  and relation types are constrained by the configured
  [`L3MemorySchema`] — the component hardcodes no domain vocabulary.

## Registration

```rust
use synonz::SynonzRuntime;
use synonz_layered_memory::LayeredMemoryProvider;

// Long-term partitions resolve at use time (the application can consult
// its own project store); without a resolver the component derives
// `user:<subject>`.
let provider = LayeredMemoryProvider::builder()
    .scope_resolver(my_resolver)
    .build();
let actuator = provider.actuator();
let rewriter = provider.rewriter_provider();
let detector = provider.topic_detector_provider();

let runtime = SynonzRuntime::builder().memory_provider(provider).build();
let agent = synonz::Agent::builder()
    .runtime(&runtime)
    .model(model)
    .rewriter_provider(rewriter)
    .topic_detector_provider(detector)
    .build()?;
```

## What you can replace

- **Storage contracts** (`L1MemoryStore` / `L2MemoryStore` /
  `L3MemoryGraphStore` / `L3MemoryVectorStore`) — each ships an
  in-process default; real backends (Redis, MongoDB, Neo4j, Milvus) can
  live in their own packages.
- **Embedding**: the `Embedding` port (the bundled `HashEmbedding` is a
  deterministic development default).
- **Semantic strategies**: `L2MemorySummarizer` (batch compaction),
  `L3MemoryEntityExtractor` (distillation), and
  `LayeredMemoryContextRewriter` (the enhanced input) — the
  prompt-driven defaults are replaceable, and the framework narrates
  every model call they make.
- **Long-term partitions**: `MemoryScopeResolver` resolves a
  conversation's partitions at use time; an empty result means no
  long-term memory for that conversation.
- **Observation**: `LayeredMemoryObserver` reports layered progress
  (`Compacted` / `Distilled` / `Normalized` / `Skipped`) without
  touching the core event bus; failures keep flowing through the
  framework as `Failed` facts.
- **Tunables**: `LayeredMemoryConfig` (capacities, recall weights,
  thresholds, hop decay, the schema) and `L3MemorySchema` (the
  entity/relation vocabulary and its fallbacks).

## Management

`runtime.memory()` gives the core item-level face (list / get / edit /
forget, with scope addressing): L2 entries and L3 entities map onto the
core item view. Every read and lookup is scoped to the subject — a
conversation must be one the component archived into, and a long-term
partition must be one the resolver returns for the subject. The actuator
adds the typed view and relation-level removal:

```rust
let handle = provider.actuator();
let entries = handle.l2_memory_entries(&subject, None)?;        // L2 documents
let entities = handle.l3_memory_entities(&subject, None)?;      // L3 documents
let relations = handle.l3_memory_relations(&subject, &scope, "Alice")?;
handle.forget_l3_memory_relation(&subject, &scope, "Alice", "likes", "coffee")?;
```

The management face creates no entries: new memory comes from
conversations (the pipeline), never from the application. The storage
contracts are the component's persistence ports — applications implement
them and the component calls them; bulk import or migration writes them
directly outside the framework and reads them back through the store
handles.

## Model

The component's context-management model is optional: without one, the
framework falls back to the agent's model for per-turn work, and the
conversation-end maintenance reports an explicit `Failed` fact instead of
running.

## Documentation

- Architecture decisions: `docs/adr/` (the component's model is
  ADR-0026, redefined by ADR-0028)
- Architecture overview: `docs/architecture/v8.zh-CN.md`
- API migration: `docs/design/migration-0.7.0.zh-CN.md`
- Changelog: `CHANGELOG.md`

## Status

Synonz is pre-1.0: APIs are stable in intent but not yet committed. The
license is Apache License 2.0 (the `LICENSE` file will be added
separately).
