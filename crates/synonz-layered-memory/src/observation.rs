//! The component's observation face: layered progress facts reported to
//! component configuration (never on the core event bus). Failures keep
//! flowing through the framework as `Failed` facts.

use synonz::MemoryScope;

/// The component's observation face: layered progress facts (compaction,
/// distillation, normalization, skips). Implementations must return
/// quickly. It can be double-registered on the same object as a runtime
/// observer — the two contracts carry different event types.
pub trait LayeredMemoryObserver: Send + Sync + 'static {
    /// Delivers one progress fact.
    fn on_progress(&self, event: &LayeredMemoryEvent);
}

/// One layered progress fact.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum LayeredMemoryEvent {
    /// One batch compaction finished: how many entries it created and how
    /// many existing entries it updated.
    Compacted {
        /// The conversation partition.
        scope: MemoryScope,
        /// How many entries were created.
        created: usize,
        /// How many entries were updated.
        updated: usize,
    },
    /// One distillation finished: how many entities and edges reached one
    /// long-term partition (one event per partition).
    Distilled {
        /// The long-term partition.
        scope: MemoryScope,
        /// How many entities were written.
        entities: usize,
        /// How many edges were written.
        edges: usize,
    },
    /// An extracted name was merged into an existing entity as an alias.
    Normalized {
        /// The partition the merge happened in.
        scope: MemoryScope,
        /// The name that was merged.
        from: String,
        /// The canonical name it merged into.
        into: String,
    },
    /// A batch produced nothing to remember (considered and skipped — not a
    /// failure).
    Skipped {
        /// The partition.
        scope: MemoryScope,
        /// Why the work was skipped.
        reason: String,
    },
}
