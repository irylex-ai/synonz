//! The write phase: the framework's three hooks.
//!
//! The synchronous segment archives the completed turn into working memory
//! (and notes it for compaction). The background segment compacts the batch
//! when the window fills or the topic shifts, and distills into long-term
//! memory on topic shifts. Conversation end compacts whatever remains,
//! distills the conversation, repairs the touched vectors, and clears the
//! per-conversation bookkeeping.

use std::sync::Arc;

use futures::future::BoxFuture;
use synonz::{
    MemoryFailure, MemoryPipeline, Model, PipelineConversationContext, PipelineTurnContext, Subject,
};

use crate::config::LayeredMemoryConfig;
use crate::l1_memory::{L1Memory, L1MemoryEntry};
use crate::l2_memory::L2Memory;
use crate::l3_memory::L3Memory;
use crate::utils::{conversation_scope, final_answer};

/// The component's pipeline: the framework's write-phase template calls its
/// three hooks.
pub(crate) struct LayeredMemoryPipeline {
    l1: Arc<L1Memory>,
    l2: Arc<L2Memory>,
    l3: Arc<L3Memory>,
    config: Arc<LayeredMemoryConfig>,
}

impl LayeredMemoryPipeline {
    /// Builds the pipeline over its collaborators.
    pub(crate) fn new(
        l1: Arc<L1Memory>,
        l2: Arc<L2Memory>,
        l3: Arc<L3Memory>,
        config: Arc<LayeredMemoryConfig>,
    ) -> Self {
        Self { l1, l2, l3, config }
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
            self.l2.note_turn(&partition);
            Ok(())
        })
    }

    fn spawn_task<'a>(
        &'a self,
        ctx: &'a PipelineTurnContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
        let partition = conversation_scope(ctx.conversation_id());
        let uncompacted = self.l2.uncompacted_turns(&partition);
        let compact = uncompacted >= self.config.l1_turns;
        let distill = ctx.topic_change().is_some();
        if !compact && !distill {
            return Box::pin(async { Ok(()) });
        }
        // The batch is captured synchronously: the next turns may append and
        // evict before the background task runs.
        let batch = match self.l1.entries(&partition, uncompacted) {
            Ok(batch) => batch,
            Err(error) => {
                return Box::pin(
                    async move { Err(MemoryFailure::new("spawn", error.to_string())) },
                );
            }
        };
        let task = TurnBackgroundTask {
            l2: Arc::clone(&self.l2),
            l3: Arc::clone(&self.l3),
            subject: ctx.subject().clone(),
            conversation_id: ctx.conversation_id().to_string(),
            partition,
            batch,
            compact,
            distill,
            model: Arc::clone(ctx.model()),
        };
        ctx.spawn(async move { task.run().await });
        Box::pin(async { Ok(()) })
    }

    fn finalize_conversation<'a>(
        &'a self,
        ctx: &'a PipelineConversationContext<'a>,
    ) -> BoxFuture<'a, Result<(), MemoryFailure>> {
        Box::pin(async move {
            let partition = conversation_scope(ctx.conversation_id());
            let uncompacted = self.l2.uncompacted_turns(&partition);
            let batch = self
                .l1
                .entries(&partition, uncompacted)
                .map_err(|error| MemoryFailure::new("finalize", error.to_string()))?;
            let entries = self
                .l2
                .entries(&partition)
                .map_err(|error| MemoryFailure::new("finalize", error.to_string()))?;
            let has_work = uncompacted > 0 || !entries.is_empty();
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
            if uncompacted > 0 {
                self.l2
                    .compact(&subject, &partition, &batch, Arc::clone(&model))
                    .await?;
            }
            let entries = self
                .l2
                .entries(&partition)
                .map_err(|error| MemoryFailure::new("finalize", error.to_string()))?;
            self.l3
                .distill(&subject, &conversation_id, &batch, &entries, model)
                .await?;
            self.l3.repair_touched(&conversation_id).await?;
            self.l3.forget_touched(&conversation_id);
            self.l2.clear_counter(&partition);
            Ok(())
        })
    }
}

/// The background write job: compact the captured batch, then distill on
/// topic shifts. Failures surface as `Failed` facts through the spawn
/// wrapper.
struct TurnBackgroundTask {
    l2: Arc<L2Memory>,
    l3: Arc<L3Memory>,
    subject: Subject,
    conversation_id: String,
    partition: synonz::MemoryScope,
    batch: Vec<L1MemoryEntry>,
    compact: bool,
    distill: bool,
    model: Arc<dyn Model>,
}

impl TurnBackgroundTask {
    async fn run(self) -> Result<(), MemoryFailure> {
        if self.compact {
            self.l2
                .compact(
                    &self.subject,
                    &self.partition,
                    &self.batch,
                    Arc::clone(&self.model),
                )
                .await?;
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
                    self.model,
                )
                .await?;
        }
        Ok(())
    }
}
