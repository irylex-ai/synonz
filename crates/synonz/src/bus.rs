//! The event bus: the dual-lane dispatch facility and its vocabulary.
//!
//! The bus is the *only* event-driven mechanism: emitters notify it, it
//! dispatches to its subscription slots. Two lanes with different delivery
//! contracts:
//!
//! - **Observation lane** — side queue, non-blocking (`try_send`), bounded,
//!   overflow drops counted and surfaced via [`Observer::on_lagged`],
//!   per-observer panic isolation, in-order delivery. A slow or panicking
//!   observer degrades observation, never any emitter.
//! - **Action lane** — legislated (synchronous reaction contract slot);
//!   no dispatch point in 0.3.0 (reserved for multi-agent orchestration).
//!
//! The product-narrative delivery ([`crate::ExecutionEvent`]) does *not*
//! travel the bus: delivery is the execution face's own point-to-point
//! pipeline with backpressure — it is not event-driven. The bus is notified
//! *before* the delivery hand-off, so an observer never lags behind the
//! product consumer.
//!
//! The dispatcher is resident for the runtime's lifetime: it delivers in
//! emission order, isolates observer panics (a panicking observer is
//! disabled for the remainder of the bus), reports lag, and exits only
//! after the queue is empty and closed — the terminal event is never lost.
//!
//! The task spawns lazily on the first emit that runs inside an async
//! context; until then events queue in the bounded channel (the eventual
//! lag is visible, never silent).

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::{mpsc, oneshot};

use crate::event::{MemoryFlowStage, TurnEvent};

/// The observation queue's capacity (fixed by design: no configuration
/// family; overflow is reported, not tuned away).
pub(crate) const OBSERVATION_QUEUE_CAP: usize = 256;

static EXECUTION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Allocates a process-unique execution id (one per run).
pub(crate) fn next_execution_id() -> u64 {
    EXECUTION_COUNTER.fetch_add(1, Ordering::Relaxed)
}

// ── Vocabulary: the conversation and memory entity families ──

/// Why a conversation ended.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationEndReason {
    /// The initiating side called the explicit end.
    Explicit,
    /// The idle-timeout fallback swept the conversation.
    IdleSwept,
    /// The runtime's shutdown (process teardown) ended the conversation.
    Shutdown,
}

/// The moment a memory-flow failure happened.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryFlowFailedMoment {
    /// The post-turn maintenance, synchronous segment.
    AfterTurn,
    /// The conversation-end maintenance.
    AtConversationEnd,
    /// The post-turn maintenance, background segment.
    Background,
    /// The lifecycle entry (the conversation's initial state save).
    Creation,
}

/// Conversation lifecycle facts.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ConversationEvent {
    /// A conversation was created (the three generic acts: construct,
    /// persist, notify).
    Created {
        /// The conversation's identity.
        conversation_id: String,
        /// The owning subject's id.
        subject_id: String,
    },
    /// A conversation ended (explicitly or by the idle-timeout sweep).
    Ended {
        /// The conversation's identity.
        conversation_id: String,
        /// The owning subject's id.
        subject_id: String,
        /// Which of the two end paths fired.
        reason: ConversationEndReason,
    },
    /// The conversation topic shifted (the detector's verdict, emitted by the
    /// state engine's maintenance).
    TopicShifted {
        /// The conversation's identity.
        conversation_id: String,
        /// The topic before the shift.
        from: String,
        /// The topic after the shift.
        to: String,
    },
}

/// Memory flow facts — the layered memory's lifecycle (archive, compaction,
/// distillation, promotion) and its failures.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MemoryEvent {
    /// A completed turn was archived into L1.
    TurnArchived {
        /// The conversation's identity.
        conversation_id: String,
        /// The owning subject's id.
        subject_id: String,
        /// The topic the turn was archived under.
        topic: String,
    },
    /// L1 overflow was compacted into an L2 summary.
    Compacted {
        /// The conversation's identity.
        conversation_id: String,
        /// How many L1 entries were compacted.
        count: usize,
    },
    /// L2 overflow was distilled into L3 knowledge.
    Distilled {
        /// The conversation's identity.
        conversation_id: String,
        /// How many L2 blocks were distilled.
        count: usize,
    },
    /// L2 blocks were promoted into L3 at conversation end.
    Promoted {
        /// The conversation's identity.
        conversation_id: String,
        /// How many L2 blocks were promoted.
        count: usize,
    },
    /// A memory-flow failure — visible, never silent; it does not abort
    /// whatever emitted it.
    FlowFailed {
        /// Which stage of the background lifecycle failed.
        stage: MemoryFlowStage,
        /// Human-readable detail of the failure.
        detail: String,
        /// When in the lifecycle the failure happened.
        moment: MemoryFlowFailedMoment,
    },
}

/// The bus vocabulary: the unified event envelope, tagged by entity family.
///
/// The publish/subscribe input is type-locked to this enum — non-framework
/// events cannot enter the bus (a compile-time guarantee).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SynonzEvent {
    /// A run's execution narrative.
    Turn(TurnEvent),
    /// A conversation lifecycle fact.
    Conversation(ConversationEvent),
    /// A memory flow fact.
    Memory(MemoryEvent),
}

// ── Observation slot ──

/// The context handed to an observer with every delivery: which execution
/// the event came from (concurrent runs interleave in one observer; run
/// external facts carry `None`).
#[non_exhaustive]
pub struct ObserverContext {
    /// The process-unique id of the run this event came from, when it has
    /// one.
    pub execution_id: Option<u64>,
}

impl ObserverContext {
    pub(crate) fn new(execution_id: Option<u64>) -> Self {
        Self { execution_id }
    }
}

/// An observer of the full event stream: the engineering-facing face of the
/// dual observability faces (the product narrative face is
/// [`crate::ExecutionEvent`]). Observers see **everything** the bus
/// carries — including the input-side payloads (`Started` / `Requested` /
/// `Responded`) — which is what debugging, recording, replay, and audit
/// need.
///
/// # Contract
///
/// - `on_event` **must return quickly**: it runs on the bus's resident
///   dispatcher, and heavy work (batching, network export) belongs in the
///   observer's own queue or worker.
/// - A panicking observer is disabled for the remainder of the bus — no
///   emitter is ever affected by observation failures.
/// - Queue overflow drops events (counted); the drop is surfaced via
///   [`Observer::on_lagged`] — visible, never silent.
pub trait Observer: Send + Sync + 'static {
    /// Delivers one event, in emission order.
    fn on_event(&self, ctx: &ObserverContext, event: &SynonzEvent);

    /// The observation queue overflowed: `dropped` events were dropped in
    /// total so far. Called when the dispatcher catches up after an
    /// overflow. The default does nothing; recording-type observers should
    /// override it to mark gaps.
    fn on_lagged(&self, ctx: &ObserverContext, dropped: u64) {
        let _ = (ctx, dropped);
    }
}

// ── The bus ──

/// One queued bus item: an event with its execution attribution, or the
/// internal flush barrier.
enum BusItem {
    Event {
        execution_id: Option<u64>,
        event: SynonzEvent,
    },
    Barrier(oneshot::Sender<()>),
}

struct BusCore {
    sender: mpsc::Sender<BusItem>,
    receiver: Mutex<Option<mpsc::Receiver<BusItem>>>,
    observers: Vec<Arc<dyn Observer>>,
    dropped: Arc<AtomicU64>,
    spawned: AtomicBool,
}

/// The dual-lane dispatch facility.
///
/// Held by the runtime; clones share the same bus. Emission is
/// non-blocking by legislation: a full observation queue counts the drop
/// and moves on. The dispatcher task spawns lazily on the first emit
/// inside an async context. The input is type-locked to
/// [`SynonzEvent`] — non-framework events cannot enter the bus.
#[derive(Clone)]
pub struct EventBus {
    core: Arc<BusCore>,
}

impl EventBus {
    /// Creates the bus and arms it with its observers. The dispatcher
    /// spawns on the first emit that runs inside an async context.
    pub fn new(observers: Vec<Arc<dyn Observer>>) -> Self {
        let (sender, receiver) = mpsc::channel(OBSERVATION_QUEUE_CAP);
        Self {
            core: Arc::new(BusCore {
                sender,
                receiver: Mutex::new(Some(receiver)),
                observers,
                dropped: Arc::new(AtomicU64::new(0)),
                spawned: AtomicBool::new(false),
            }),
        }
    }

    /// Emits a run-scoped fact (the attribution travels to observers).
    pub(crate) fn emit_for_run(&self, execution_id: u64, event: SynonzEvent) {
        self.emit_inner(Some(execution_id), event);
    }

    /// Emits a run-external fact (no execution attribution).
    ///
    /// The general emission path; the conversation lifecycle facts travel
    /// here.
    pub fn emit(&self, event: SynonzEvent) {
        self.emit_inner(None, event);
    }

    fn emit_inner(&self, execution_id: Option<u64>, event: SynonzEvent) {
        self.ensure_dispatcher();
        let item = BusItem::Event {
            execution_id,
            event,
        };
        if self.core.sender.try_send(item).is_err() {
            self.core.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Waits until everything emitted so far has been delivered to the
    /// observers — the internal shutdown barrier (not part of the public
    /// surface). Overflow-dropped events stay dropped; the barrier only
    /// orders what is still queued.
    pub(crate) async fn flush(&self) {
        self.ensure_dispatcher();
        let (ack, waited) = oneshot::channel();
        if self.core.sender.send(BusItem::Barrier(ack)).await.is_err() {
            return; // the dispatcher is gone; nothing to wait for
        }
        let _ = waited.await;
    }

    /// Spawns the resident dispatcher once an async context is available.
    /// Until then events queue in the bounded channel; the eventual lag is
    /// reported, never silent.
    fn ensure_dispatcher(&self) {
        if self.core.spawned.load(Ordering::Acquire) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.core.spawned.swap(true, Ordering::AcqRel) {
            return;
        }
        let receiver = self
            .core
            .receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(receiver) = receiver {
            spawn_dispatcher(
                handle,
                receiver,
                self.core.observers.clone(),
                Arc::clone(&self.core.dropped),
            );
        }
    }
}

/// Spawns the resident dispatcher: drains the queue in emission order,
/// delivers to every enabled observer (panic-isolated), reports lag, and
/// exits only after the queue is empty and closed.
fn spawn_dispatcher(
    handle: tokio::runtime::Handle,
    mut receiver: mpsc::Receiver<BusItem>,
    observers: Vec<Arc<dyn Observer>>,
    dropped: Arc<AtomicU64>,
) {
    handle.spawn(async move {
        // Per-observer circuit breaker: an observer that panicked is
        // disabled for the remainder of the bus.
        let mut disabled = vec![false; observers.len()];
        let mut reported_lag = 0u64;

        // The channel closes (and drains) when every sender is dropped, so
        // this loop exits only after delivering everything, terminal
        // included.
        while let Some(item) = receiver.recv().await {
            match item {
                BusItem::Event {
                    execution_id,
                    event,
                } => {
                    let ctx = ObserverContext::new(execution_id);
                    for (index, observer) in observers.iter().enumerate() {
                        if disabled[index] {
                            continue;
                        }
                        let delivery =
                            std::panic::AssertUnwindSafe(|| observer.on_event(&ctx, &event));
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
                BusItem::Barrier(ack) => {
                    let _ = ack.send(());
                }
            }
        }
    });
}

// ── The run's event outlet ──

/// The event outlet of one run: the explicit dual path out of the loop.
///
/// - [`EventSink::emit_turn`] — the Turn-family narration: the bus is
///   notified first (an observer never lags behind the product delivery),
///   then the delivery channel (backpressure). Returns whether the
///   consumer is still there.
/// - [`EventSink::emit_memory`] — memory facts: bus only, never on the
///   product narrative.
///
/// Clones share the same run attribution (background maintenance tasks
/// carry the outlet with them).
#[derive(Clone)]
pub struct EventSink {
    bus: EventBus,
    execution_id: u64,
    consumer: mpsc::Sender<TurnEvent>,
}

impl EventSink {
    /// Assembles the outlet (framework-internal; exposed for tests and
    /// custom orchestrators that must emit on an engine's behalf).
    pub fn new(bus: EventBus, execution_id: u64, consumer: mpsc::Sender<TurnEvent>) -> Self {
        Self {
            bus,
            execution_id,
            consumer,
        }
    }

    /// Emits one Turn-family event: bus first, then delivery. Returns
    /// whether the consumer is still there.
    pub async fn emit_turn(&self, event: TurnEvent) -> bool {
        self.bus
            .emit_for_run(self.execution_id, SynonzEvent::Turn(event.clone()));
        self.consumer.send(event).await.is_ok()
    }

    /// Emits one memory fact: bus only (never on the product narrative).
    pub fn emit_memory(&self, event: MemoryEvent) {
        self.bus
            .emit_for_run(self.execution_id, SynonzEvent::Memory(event));
    }

    /// Emits one conversation fact: bus only (never on the product
    /// narrative).
    pub fn emit_conversation(&self, event: ConversationEvent) {
        self.bus
            .emit_for_run(self.execution_id, SynonzEvent::Conversation(event));
    }
}
