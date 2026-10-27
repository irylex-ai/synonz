//! The write phase: the framework's three hooks.
//!
//! The synchronous segment archives the completed turn into working memory
//! and keeps the pending-turn hint. The background segment compacts the
//! durable pending batch when the window fills or the topic shifts, and
//! distills on topic shifts — or with a compaction batch once it has aged
//! past the settle window. Conversation end drains the pending set in
//! bounded chunks, distills them, repairs the touched vectors, and clears
//! the conversation's hints.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use synonz::{
    MemoryFailure, MemoryPipeline, MemoryScope, Model, PipelineConversationContext,
    PipelineTurnContext, Subject,
};

use crate::config::LayeredMemoryConfig;
use crate::l1_memory::{L1Memory, L1MemoryEntry};
use crate::l2_memory::L2Memory;
use crate::l3_memory::L3Memory;
use crate::utils::{conversation_scope, final_answer, now_epoch};

/// The component's pipeline: the framework's write-phase template calls its
/// three hooks.
pub(crate) struct LayeredMemoryPipeline {
    l1: Arc<L1Memory>,
    l2: Arc<L2Memory>,
    l3: Arc<L3Memory>,
    config: Arc<LayeredMemoryConfig>,
    state: Arc<MaintenanceState>,
}

impl LayeredMemoryPipeline {
    /// Builds the pipeline over its collaborators.
    pub(crate) fn new(
        l1: Arc<L1Memory>,
        l2: Arc<L2Memory>,
        l3: Arc<L3Memory>,
        config: Arc<LayeredMemoryConfig>,
    ) -> Self {
        Self {
            l1,
            l2,
            l3,
            config,
            state: Arc::new(MaintenanceState::default()),
        }
    }
}

/// Per-process maintenance hints. The store's compaction cursor is
/// authoritative; the pending count is only a trigger hint, refreshed from
/// the store whenever a maintenance run finishes. The busy set serializes
/// maintenance per conversation.
#[derive(Default)]
struct MaintenanceState {
    pending: Mutex<HashMap<String, usize>>,
    busy: Mutex<HashSet<String>>,
}

impl MaintenanceState {
    /// The cached pending count, or `None` before this process's first turn
    /// for the partition.
    fn pending(&self, scope: &MemoryScope) -> Option<usize> {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(scope.as_str())
            .copied()
    }

    /// Replaces the cached pending count.
    fn set_pending(&self, scope: &MemoryScope, count: usize) {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(scope.to_string(), count);
    }

    /// Counts one archived turn while the cache exists (otherwise the first
    /// spawn initializes it from the store).
    fn count_archived(&self, scope: &MemoryScope) {
        if let Some(count) = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(scope.as_str())
        {
            *count += 1;
        }
    }

    /// Drops the cached hint (the next trigger re-reads the store).
    fn forget_pending(&self, scope: &MemoryScope) {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(scope.as_str());
    }

    /// Claims the per-partition maintenance slot, or `None` when a run is
    /// already in flight.
    fn claim(state: &Arc<Self>, key: &str) -> Option<BusyGuard> {
        if !state
            .busy
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.to_string())
        {
            return None;
        }
        Some(BusyGuard {
            state: Arc::clone(state),
            key: key.to_string(),
        })
    }
}

/// Releases the per-partition maintenance slot on drop (task completion or
/// cancellation).
struct BusyGuard {
    state: Arc<MaintenanceState>,
    key: String,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.state
            .busy
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.key);
    }
}

impl MemoryPipeline for LayeredMemoryPipeline {
    fn archive_turn<'a>(
        &'a self,
        ctx: &'a PipelineTurnContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
        Box::pin(async move {
            let partition = conversation_scope(ctx.conversation_id());
            let response = final_answer(ctx.responses());
            self.l1
                .archive_turn(&partition, ctx.topic(), ctx.input(), &response)
                .map_err(|error| MemoryFailure::new("archive", error.to_string()))?;
            self.state.count_archived(&partition);
            Ok(())
        })
    }

    fn spawn_task<'a>(
        &'a self,
        ctx: &'a PipelineTurnContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
        let partition = conversation_scope(ctx.conversation_id());
        let topic_change = ctx.topic_change().is_some();
        if self
            .state
            .pending(&partition)
            .is_some_and(|count| count < self.config.l1_turns)
            && !topic_change
        {
            return Box::pin(async { Ok(()) });
        }
        // The store's cursor is the authoritative pending set; the cached
        // count only decides whether it is worth reading this turn.
        let pending = match self.l1.uncompacted(&partition) {
            Ok(pending) => pending,
            Err(error) => {
                self.state.forget_pending(&partition);
                return Box::pin(
                    async move { Err(MemoryFailure::new("spawn", error.to_string())) },
                );
            }
        };
        self.state.set_pending(&partition, pending.len());
        if pending.len() < self.config.l1_turns && !topic_change {
            return Box::pin(async { Ok(()) });
        }
        // One run processes at most a window of pending turns; further
        // triggers drain the backlog.
        let batch: Vec<L1MemoryEntry> = pending
            .into_iter()
            .take(self.config.l1_turns.max(1))
            .collect();
        if batch.is_empty() {
            return Box::pin(async { Ok(()) });
        }
        let compact = batch.len() >= self.config.l1_turns;
        let aged = batch.first().is_some_and(|turn| {
            now_epoch().saturating_sub(turn.created_at) >= self.config.l3_distill_min_age_seconds
        });
        let distill = topic_change || (compact && aged);
        if !compact && !distill {
            return Box::pin(async { Ok(()) });
        }
        let Some(guard) = MaintenanceState::claim(&self.state, partition.as_str()) else {
            // A maintenance run is in flight for this conversation; the
            // next turn retries (the cursor means nothing is lost).
            return Box::pin(async { Ok(()) });
        };
        let task = TurnBackgroundTask {
            l1: Arc::clone(&self.l1),
            l2: Arc::clone(&self.l2),
            l3: Arc::clone(&self.l3),
            state: Arc::clone(&self.state),
            subject: ctx.subject().clone(),
            conversation_id: ctx.conversation_id().to_string(),
            partition,
            batch,
            compact,
            distill,
            model: Arc::clone(ctx.model()),
        };
        ctx.spawn(async move {
            let _guard = guard;
            task.run().await
        });
        Box::pin(async { Ok(()) })
    }

    fn finalize_conversation<'a>(
        &'a self,
        ctx: &'a PipelineConversationContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
        Box::pin(async move {
            let partition = conversation_scope(ctx.conversation_id());
            let pending = self
                .l1
                .uncompacted(&partition)
                .map_err(|error| MemoryFailure::new("finalize", error.to_string()))?;
            let entries = self
                .l2
                .entries(&partition)
                .map_err(|error| MemoryFailure::new("finalize", error.to_string()))?;
            let has_work = !pending.is_empty() || !entries.is_empty();
            let Some(model) = ctx.model().cloned() else {
                if has_work {
                    return Err(MemoryFailure::new(
                        "finalize",
                        "no context-management model configured; conversation-end maintenance skipped",
                    ));
                }
                return Ok(());
            };
            let subject = ctx.subject().clone();
            let conversation_id = ctx.conversation_id().to_string();
            if pending.is_empty() {
                // Nothing pending: the distillation still sees the committed
                // entries as its context.
                self.l3
                    .distill(&subject, &conversation_id, &[], &entries, model)
                    .await?;
            } else {
                // No turn will trigger the remaining backlog afterwards:
                // drain it in bounded chunks, advancing the cursor per chunk.
                let mut rest = pending;
                let cap = self.config.l1_turns.max(1);
                while !rest.is_empty() {
                    let chunk: Vec<L1MemoryEntry> = rest.iter().take(cap).cloned().collect();
                    self.l2
                        .compact(&subject, &partition, &chunk, Arc::clone(&model))
                        .await?;
                    let Some(through) = chunk.last().map(|turn| turn.id.as_str()) else {
                        break;
                    };
                    self.l1
                        .mark_compacted(&partition, through)
                        .map_err(|error| MemoryFailure::new("finalize", error.to_string()))?;
                    let entries = self
                        .l2
                        .entries(&partition)
                        .map_err(|error| MemoryFailure::new("finalize", error.to_string()))?;
                    self.l3
                        .distill(
                            &subject,
                            &conversation_id,
                            &chunk,
                            &entries,
                            Arc::clone(&model),
                        )
                        .await?;
                    rest.drain(..chunk.len());
                }
            }
            self.l3.repair_touched(&conversation_id).await?;
            self.l3.forget_touched(&conversation_id);
            self.state.forget_pending(&partition);
            Ok(())
        })
    }
}

/// The background write job: compact the captured batch (advancing the
/// durable cursor on success), then distill it when requested. Failures
/// surface as `Failed` facts through the spawn wrapper.
struct TurnBackgroundTask {
    l1: Arc<L1Memory>,
    l2: Arc<L2Memory>,
    l3: Arc<L3Memory>,
    state: Arc<MaintenanceState>,
    subject: Subject,
    conversation_id: String,
    partition: MemoryScope,
    batch: Vec<L1MemoryEntry>,
    compact: bool,
    distill: bool,
    model: Arc<dyn Model>,
}

impl TurnBackgroundTask {
    async fn run(self) -> Result<(), MemoryFailure> {
        let outcome = self.execute().await;
        // Refresh the trigger hint from the store; the cursor is
        // authoritative and the next turn re-evaluates from it.
        match self.l1.uncompacted(&self.partition) {
            Ok(pending) => self.state.set_pending(&self.partition, pending.len()),
            Err(error) => {
                self.state.forget_pending(&self.partition);
                if outcome.is_ok() {
                    return Err(MemoryFailure::new("compact", error.to_string()));
                }
            }
        }
        outcome
    }

    async fn execute(&self) -> Result<(), MemoryFailure> {
        if self.compact {
            self.l2
                .compact(
                    &self.subject,
                    &self.partition,
                    &self.batch,
                    Arc::clone(&self.model),
                )
                .await?;
            let Some(through) = self.batch.last().map(|turn| turn.id.as_str()) else {
                return Err(MemoryFailure::new("compact", "empty compaction batch"));
            };
            self.l1
                .mark_compacted(&self.partition, through)
                .map_err(|error| MemoryFailure::new("compact", error.to_string()))?;
        }
        if self.distill {
            let entries = self
                .l2
                .entries(&self.partition)
                .map_err(|error| MemoryFailure::new("distill", error.to_string()))?;
            self.l3
                .distill(
                    &self.subject,
                    &self.conversation_id,
                    &self.batch,
                    &entries,
                    Arc::clone(&self.model),
                )
                .await?;
        }
        Ok(())
    }
}
