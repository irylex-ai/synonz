//! The explicit context-compression command: resolve the read-phase
//! boundary from the visible working-memory window, compact the pending
//! turns through it (the normal batch path, drained), then advance the
//! durable compression boundary.

use std::sync::Arc;

use synonz::{MemoryFailure, Model, Subject};

use crate::config::LayeredMemoryConfig;
use crate::l1_memory::{L1Memory, L1MemoryEntry};
use crate::l2_memory::L2Memory;
use crate::l3_memory::L3Memory;
use crate::pipeline::MaintenanceState;
use crate::utils::{conversation_scope, now_epoch};

/// Compresses one conversation's visible context, keeping the configured
/// number of recent turns verbatim.
///
/// The compression boundary is resolved from the component's visible
/// working-memory turns (the last `l1_turns`). The pending turns up to and
/// including it are compacted in bounded chunks (the normal batch path,
/// advancing the durable cursor per chunk); the boundary is marked only
/// after every chunk succeeded, so a failure leaves the window unchanged
/// and the command can be retried (at-least-once).
///
/// The command waits for an in-flight background maintenance run of the
/// same conversation, then owns the maintenance slot for its duration.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn compact_context(
    l1: &L1Memory,
    l2: &L2Memory,
    l3: &L3Memory,
    config: &LayeredMemoryConfig,
    state: &Arc<MaintenanceState>,
    model: Option<Arc<dyn Model>>,
    subject: &Subject,
    conversation_id: &str,
) -> Result<(), MemoryFailure> {
    let partition = conversation_scope(conversation_id);

    // No visible window: there is no read-phase context to compress (and no
    // boundary to close).
    let window_cap = config.context_recent_turns.min(config.l1_turns);
    if window_cap == 0 {
        return Ok(());
    }

    // The boundary turn: keep the configured number of newer visible turns.
    let visible = l1
        .entries(&partition, config.l1_turns)
        .map_err(|error| MemoryFailure::new("compress", error.to_string()))?;
    let keep = config.context_compact_keep_turns;
    if keep >= visible.len() {
        return Ok(());
    }
    let boundary = visible[visible.len() - keep - 1].clone();

    // Serialize with the conversation's background maintenance: wait for an
    // in-flight run, then own the slot.
    let _guard = MaintenanceState::claim_wait(state, partition.as_str()).await;

    // Re-read the pending set after acquiring the slot: the run we waited
    // for may have advanced the durable cursor.
    let pending = l1
        .uncompacted(&partition)
        .map_err(|error| MemoryFailure::new("compress", error.to_string()))?;
    let through = pending
        .iter()
        .position(|turn| turn.id == boundary.id)
        .map(|index| index + 1)
        .unwrap_or(0);
    let batch: Vec<L1MemoryEntry> = pending.into_iter().take(through).collect();

    if !batch.is_empty() {
        let Some(model) = model else {
            return Err(MemoryFailure::new(
                "compress",
                "no context-management model configured; context compression skipped",
            ));
        };
        let cap = config.l1_turns.max(1);
        let mut rest = batch;
        while !rest.is_empty() {
            let chunk: Vec<L1MemoryEntry> = rest.iter().take(cap).cloned().collect();
            l2.compact(subject, &partition, &chunk, Arc::clone(&model))
                .await?;
            let Some(last) = chunk.last() else {
                break;
            };
            l1.mark_compacted(&partition, &last.id)
                .map_err(|error| MemoryFailure::new("compress", error.to_string()))?;
            // The normal settle window decides whether this chunk also
            // distills; the command adds no special rule.
            let aged = chunk.first().is_some_and(|turn| {
                now_epoch().saturating_sub(turn.created_at) >= config.l3_distill_min_age_seconds
            });
            if aged {
                let entries = l2
                    .entries(&partition)
                    .map_err(|error| MemoryFailure::new("compress", error.to_string()))?;
                l3.distill(
                    subject,
                    conversation_id,
                    &chunk,
                    &entries,
                    Arc::clone(&model),
                )
                .await?;
            }
            rest.drain(..chunk.len());
        }
    }

    // Everything through the boundary is consolidated: close the read-phase
    // window on it (the only write that moves the boundary).
    l1.mark_context_boundary(&partition, &boundary.id)
        .map_err(|error| MemoryFailure::new("compress", error.to_string()))?;
    Ok(())
}
