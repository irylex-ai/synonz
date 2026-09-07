//! The observation bypass: the `Observer` contract and its dispatcher
//! (ADR-0016).
//!
//! Observability is a property of the execution, not of a consumption
//! face: every event emitted by a run's loop is tapped into a bounded
//! side queue and delivered — in emission order — to the registered
//! observers by a per-run background task. The hot path pays one
//! non-blocking `try_send` and never waits for an observer; a slow or
//! panicking observer degrades observation, never the run.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;

use crate::event::AgentEvent;

/// The observation queue's capacity (fixed by design: no configuration
/// family; overflow is reported, not tuned away).
pub(crate) const OBSERVATION_QUEUE_CAP: usize = 256;

static EXECUTION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Allocates a process-unique execution id (one per run).
pub(crate) fn next_execution_id() -> u64 {
    EXECUTION_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// The context handed to an observer with every delivery: which execution
/// the event belongs to (concurrent runs interleave in one observer).
#[non_exhaustive]
pub struct ObserverContext {
    /// The process-unique id of the run this event came from.
    pub execution_id: u64,
}

impl ObserverContext {
    pub(crate) fn new(execution_id: u64) -> Self {
        Self { execution_id }
    }
}

/// An observer of the full event stream: the engineering-facing side of
/// the dual observability faces (the product narrative face is
/// [`crate::ExecutionEvent`]). Observers see **everything** — including
/// the input-side payloads (`Started` / `Requested` / `Responded`) —
/// which is what debugging, recording, replay, and audit need.
///
/// # Contract
///
/// - `on_event` **must return quickly**: it runs on the run's dispatcher,
///   and heavy work (batching, network export) belongs in the observer's
///   own queue or worker.
/// - A panicking observer is disabled for the remainder of that run —
///   the execution is never affected by observation failures.
/// - Queue overflow drops events (counted); the drop is surfaced via
///   [`Observer::on_lagged`] — visible, never silent (ADR-0015).
pub trait Observer: Send + Sync + 'static {
    /// Delivers one event, in emission order.
    fn on_event(&self, ctx: &ObserverContext, event: &AgentEvent);

    /// The observation queue overflowed: `dropped` events of this run
    /// were dropped in total so far. Called when the dispatcher catches
    /// up after an overflow. The default does nothing; recording-type
    /// observers should override it to mark gaps.
    fn on_lagged(&self, ctx: &ObserverContext, dropped: u64) {
        let _ = (ctx, dropped);
    }
}

/// The side queue tapped at the loop's emission point.
pub(crate) struct ObservationQueue {
    sender: mpsc::Sender<AgentEvent>,
    dropped: Arc<AtomicU64>,
}

impl ObservationQueue {
    /// Creates a queue wired to a freshly spawned dispatcher.
    pub(crate) fn spawn(observers: Vec<Arc<dyn Observer>>, execution_id: u64) -> Self {
        let (sender, receiver) = mpsc::channel(OBSERVATION_QUEUE_CAP);
        let dropped = Arc::new(AtomicU64::new(0));
        spawn_dispatcher(receiver, observers, execution_id, Arc::clone(&dropped));
        Self { sender, dropped }
    }

    /// Taps one event (non-blocking: a full queue counts the drop).
    fn tap(&self, event: &AgentEvent) {
        if self.sender.try_send(event.clone()).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The event sink the loop emits into: the consumer channel (the
/// narrative the handle reads) plus, when observability is on, the
/// side queue feeding the dispatcher.
pub(crate) struct EventTap {
    consumer: mpsc::Sender<AgentEvent>,
    observation: Option<ObservationQueue>,
}

impl EventTap {
    pub(crate) fn consumer_only(consumer: mpsc::Sender<AgentEvent>) -> Self {
        Self {
            consumer,
            observation: None,
        }
    }

    pub(crate) fn with_observation(
        consumer: mpsc::Sender<AgentEvent>,
        observation: ObservationQueue,
    ) -> Self {
        Self {
            consumer,
            observation: Some(observation),
        }
    }

    /// Emits one event: taps the observation queue first (the bypass sees
    /// the full stream regardless of the consumer's fate), then delivers
    /// to the consumer. Returns whether the consumer is still there.
    pub(crate) async fn emit(&self, event: AgentEvent) -> bool {
        if let Some(queue) = &self.observation {
            queue.tap(&event);
        }
        self.consumer.send(event).await.is_ok()
    }
}

/// Spawns the per-run dispatcher: drains the side queue in emission
/// order, delivers to every enabled observer (panic-isolated), reports
/// lag, and exits only after the queue is empty and closed — the
/// terminal event is never lost to a lingering observer.
pub(crate) fn spawn_dispatcher(
    receiver: mpsc::Receiver<AgentEvent>,
    observers: Vec<Arc<dyn Observer>>,
    execution_id: u64,
    dropped: Arc<AtomicU64>,
) {
    tokio::spawn(async move {
        let ctx = ObserverContext::new(execution_id);
        let mut receiver = receiver;
        // Per-observer circuit breaker: an observer that panicked is
        // disabled for the remainder of this run.
        let mut disabled = vec![false; observers.len()];
        let mut reported_lag = 0u64;

        // The channel closes (and drains) when every sender is dropped —
        // the loop's tap and the consumer-side handle together — so this
        // loop exits only after delivering everything, terminal included.
        while let Some(event) = receiver.recv().await {
            for (index, observer) in observers.iter().enumerate() {
                if disabled[index] {
                    continue;
                }
                let delivery = std::panic::AssertUnwindSafe(|| observer.on_event(&ctx, &event));
                if std::panic::catch_unwind(delivery).is_err() {
                    disabled[index] = true;
                }
            }
            let total_dropped = dropped.load(Ordering::Relaxed);
            if total_dropped > reported_lag {
                for (index, observer) in observers.iter().enumerate() {
                    if disabled[index] {
                        continue;
                    }
                    let report = std::panic::AssertUnwindSafe(|| {
                        observer.on_lagged(&ctx, total_dropped);
                    });
                    if std::panic::catch_unwind(report).is_err() {
                        disabled[index] = true;
                    }
                }
                reported_lag = total_dropped;
            }
        }
    });
}
