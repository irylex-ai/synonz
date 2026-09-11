//! The process-level runtime: explicit bootstrap, startup registry, and
//! default implementations.
//!
//! `SynonzRuntime` is the single source of the process's environment
//! services: conversation persistence, layered memory, and context
//! assembly. There is no implicit default runtime — entities that touch
//! environment services are created through an explicit runtime (the
//! static factory functions on `Conversation` take `&runtime`), so a
//! conversation and the memory it resolves can never split across
//! environments.
//!
//! The runtime also owns the **system scheduler** and its first task, the
//! Monitor (the idle sweep): configuring `conversation_idle_timeout`
//! registers the Monitor at build time, and the timing thread starts with
//! that registration. The system scheduler is not reachable for
//! registration; a read-only snapshot is exposed instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::runtime::Handle;

use crate::bus::{
    ConversationEndReason, EventBus, MemoryEvent, MemoryFlowFailedMoment, SynonzEvent,
};
use crate::conversation::{
    Conversation, ConversationPage, ConversationQuery, ConversationStore, ConversationStoreError,
};
use crate::event::MemoryFlowStage;
use crate::inprocess::{
    InProcessConversationStore, InProcessMemoryL1Store, InProcessMemoryL2Store,
    InProcessMemoryL3Store,
};
use crate::memory::{Memory, MemoryL1Store, MemoryL2Store, MemoryL3Store};
use crate::scheduler::{OverlapPolicy, Schedule, Scheduler, TaskInfo};

/// The per-conversation drain budget: how long conversation-end teardown
/// waits for background maintenance tasks before proceeding without them.
const MAINTENANCE_DRAIN_TIMEOUT: Duration = Duration::from_secs(60);

/// How many stale conversations one sweep page holds (fixed by design:
/// the sweep pages through the store instead of loading everything).
const SWEEP_PAGE_SIZE: usize = 128;

/// The Monitor task's name (shown by the read-only scheduler snapshot).
const MONITOR_TASK_NAME: &str = "monitor";

/// The Monitor's tick: derived from the idle timeout, clamped so cleanup
/// stays prompt for small timeouts and crash recovery stays within a
/// minute for large ones.
fn monitor_tick(idle_timeout: Duration) -> Duration {
    const FLOOR: Duration = Duration::from_millis(100);
    const CAP: Duration = Duration::from_secs(60);
    (idle_timeout / 4).max(FLOOR).min(CAP)
}

/// The startup registry: every service has an in-process default;
/// registration replaces it. Resolution can never fail.
#[derive(Default)]
pub struct RuntimeBuilder {
    conversation_store: Option<Arc<dyn ConversationStore>>,
    memory_l1_store: Option<Arc<dyn MemoryL1Store>>,
    memory_l2_store: Option<Arc<dyn MemoryL2Store>>,
    memory_l3_store: Option<Arc<dyn MemoryL3Store>>,
    observers: Vec<Arc<dyn crate::bus::Observer>>,
    conversation_idle_timeout: Option<Duration>,
    executor: Option<Handle>,
}

impl RuntimeBuilder {
    /// Starts building a runtime.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the conversation store (default: in-process).
    pub fn conversation_store(mut self, store: impl ConversationStore) -> Self {
        self.conversation_store = Some(Arc::new(store));
        self
    }

    /// Sets the L1 working memory store (default: built-in in-process
    /// memory — the freshness layer expects memory-grade latency).
    pub fn memory_l1_store(mut self, store: impl MemoryL1Store) -> Self {
        self.memory_l1_store = Some(Arc::new(store));
        self
    }

    /// Sets the L2 summary store (default: in-process).
    pub fn memory_l2_store(mut self, store: impl MemoryL2Store) -> Self {
        self.memory_l2_store = Some(Arc::new(store));
        self
    }

    /// Sets the L3 knowledge store (default: in-process).
    pub fn memory_l3_store(mut self, store: impl MemoryL3Store) -> Self {
        self.memory_l3_store = Some(Arc::new(store));
        self
    }

    /// Registers an observer of the full event stream. Unlike the other
    /// services there is **no default** — with no observer registered, the
    /// observation lane delivers to nobody (the bus itself still runs).
    pub fn observer(mut self, observer: impl crate::bus::Observer) -> Self {
        self.observers.push(Arc::new(observer));
        self
    }

    /// Sets the conversation idle timeout: the Monitor (a system scheduler
    /// task) ends conversations with no activity for this long
    /// (`ConversationEndReason::IdleSwept`); explicit `Conversation::end`
    /// remains the primary trigger. Configuring this starts the system
    /// scheduler's timing thread at build.
    pub fn conversation_idle_timeout(mut self, timeout: Duration) -> Self {
        self.conversation_idle_timeout = Some(timeout);
        self
    }

    /// Sets the host execution environment explicitly: the system
    /// scheduler submits its executions to this runtime's worker pool.
    ///
    /// When omitted, the handle is captured from the calling async context
    /// at build time. Configuring [`conversation_idle_timeout`] without an
    /// available execution environment (neither ambient nor injected)
    /// panics at build — a configuration error.
    ///
    /// [`conversation_idle_timeout`]: RuntimeBuilder::conversation_idle_timeout
    pub fn executor(mut self, handle: Handle) -> Self {
        self.executor = Some(handle);
        self
    }

    /// Builds the runtime with registered or default implementations.
    ///
    /// The system scheduler is created and the Monitor registered when
    /// `conversation_idle_timeout` is configured; a runtime without the
    /// configuration costs no scheduling thread.
    ///
    /// # Panics
    ///
    /// Panics when `conversation_idle_timeout` is configured but no
    /// execution environment is available: call build inside an async
    /// context or inject the host handle via [`RuntimeBuilder::executor`].
    pub fn build(self) -> SynonzRuntime {
        let executor = self.executor.or_else(|| Handle::try_current().ok());
        if self.conversation_idle_timeout.is_some() && executor.is_none() {
            panic!(
                "SynonzRuntime: `conversation_idle_timeout` is configured, so the system \
                 scheduler needs an execution environment; call build() inside an async \
                 context or inject the host handle via RuntimeBuilder::executor"
            );
        }
        let inner = Arc::new(RuntimeInner {
            conversation_store: self
                .conversation_store
                .unwrap_or_else(|| Arc::new(InProcessConversationStore::default())),
            memory: Memory::new(
                self.memory_l1_store
                    .unwrap_or_else(|| Arc::new(InProcessMemoryL1Store::default())),
                self.memory_l2_store
                    .unwrap_or_else(|| Arc::new(InProcessMemoryL2Store::default())),
                self.memory_l3_store
                    .unwrap_or_else(|| Arc::new(InProcessMemoryL3Store::default())),
            ),
            event_bus: EventBus::new(self.observers),
            conversations: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            conversation_idle_timeout: self.conversation_idle_timeout,
            maintenance_drain_timeout: MAINTENANCE_DRAIN_TIMEOUT,
            scheduler: executor.map(Scheduler::new),
            shutdown_lock: tokio::sync::Mutex::new(()),
            shutdown_done: AtomicBool::new(false),
        });
        let runtime = SynonzRuntime { inner };
        runtime.start_monitor();
        runtime
    }
}

/// One owned conversation in the conversation table: the handle plus its
/// spawned background maintenance tasks (drained at conversation end).
pub(crate) struct ConversationEntry {
    pub(crate) conversation: Conversation,
    pub(crate) tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// The conversation table: conversation id → owned conversation — the
/// runtime's ownership registry (shutdown's teardown list) and the
/// background-task bookkeeping in one structure.
pub(crate) type ConversationTable =
    Arc<std::sync::Mutex<std::collections::HashMap<String, ConversationEntry>>>;

/// The maintenance-task registry handle: how an engine spawns background
/// work the runtime schedules (and drains at conversation end).
///
/// The type is public (the engine payload carries it); construction stays
/// with the runtime. Clones are scoped to the same conversation; spawned
/// tasks attach to that conversation's entry in the runtime's table.
#[derive(Clone)]
pub struct TaskRegistry {
    table: ConversationTable,
    conversation_id: String,
}

impl TaskRegistry {
    /// The sole construction site is [`SynonzRuntime::task_registry`].
    fn new(table: ConversationTable, conversation_id: impl Into<String>) -> Self {
        Self {
            table,
            conversation_id: conversation_id.into(),
        }
    }

    /// Spawns a background maintenance task and registers it under this
    /// handle's conversation (the conversation-end teardown drains them).
    ///
    /// A conversation the runtime does not own has no table entry; the
    /// task then runs untracked.
    pub fn spawn(&self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        let handle = tokio::spawn(task);
        let mut table = self
            .table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = table.get_mut(&self.conversation_id) {
            entry.tasks.push(handle);
        }
    }
}

/// The runtime's shared state (one `Arc` per runtime; clones are cheap).
struct RuntimeInner {
    conversation_store: Arc<dyn ConversationStore>,
    /// The layered memory as one domain object (assembled from the three
    /// storage slots at build time; the runtime is its single authority).
    memory: Memory,
    event_bus: EventBus,
    /// The conversation table: owned conversations and their background
    /// maintenance tasks (the Monitor and shutdown both consume it).
    conversations: ConversationTable,
    conversation_idle_timeout: Option<Duration>,
    /// The per-conversation budget for draining background maintenance
    /// tasks at conversation end (bounded teardown).
    maintenance_drain_timeout: Duration,
    /// The system scheduler (created when an execution environment is
    /// available; its first task is the Monitor).
    scheduler: Option<Scheduler>,
    /// Serializes shutdown calls (idempotency).
    shutdown_lock: tokio::sync::Mutex<()>,
    /// Whether shutdown completed.
    shutdown_done: AtomicBool,
}

/// The process-level environment handle.
///
/// Cheap to clone (shared state); every entity created with it shares the
/// same environment — the explicit same-source guarantee. Entities are
/// created via the static factory functions on their own types
/// (`Conversation::new(&runtime, &subject)`), not through the runtime
/// itself (factory attribution: the product type owns its construction).
#[derive(Clone)]
pub struct SynonzRuntime {
    inner: Arc<RuntimeInner>,
}

impl SynonzRuntime {
    /// Starts building a runtime.
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }

    /// Registers the Monitor (the idle sweep) when a timeout is
    /// configured: Skip policy, immediate first trigger (reconciling
    /// stale stored conversations such as crash leftovers), derived tick.
    fn start_monitor(&self) {
        let (Some(scheduler), Some(timeout)) = (
            self.inner.scheduler.as_ref(),
            self.inner.conversation_idle_timeout,
        ) else {
            return;
        };
        let weak = Arc::downgrade(&self.inner);
        scheduler.schedule_named(
            MONITOR_TASK_NAME,
            Schedule::every(monitor_tick(timeout)),
            OverlapPolicy::Skip,
            move || {
                let weak = weak.clone();
                async move {
                    if let Some(inner) = weak.upgrade() {
                        SynonzRuntime { inner }.sweep_idle().await;
                    }
                }
            },
        );
    }

    /// The registered (or default) conversation store.
    pub(crate) fn conversation_store(&self) -> Arc<dyn ConversationStore> {
        Arc::clone(&self.inner.conversation_store)
    }

    /// The layered memory (the three storage slots behind one facade).
    ///
    /// Public for the Low Level track: direct memory access (diagnostics,
    /// custom flows, explicit operations) alongside the framework's own
    /// orchestration. This is the object's single authority — nobody
    /// constructs a `Memory` by hand.
    pub fn memory(&self) -> Memory {
        self.inner.memory.clone()
    }

    /// The event bus (the resident dual-lane dispatch facility).
    pub(crate) fn event_bus(&self) -> &EventBus {
        &self.inner.event_bus
    }

    /// A maintenance-task registry handle scoped to one conversation.
    pub(crate) fn task_registry(&self, conversation_id: &str) -> TaskRegistry {
        TaskRegistry::new(Arc::clone(&self.inner.conversations), conversation_id)
    }

    /// Registers an owned conversation in the conversation table (entry =
    /// conversation handle + its background tasks).
    pub(crate) fn register_conversation(&self, conversation: &Conversation) {
        let mut table = self
            .inner
            .conversations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        table
            .entry(conversation.id().to_string())
            .or_insert_with(|| ConversationEntry {
                conversation: conversation.clone(),
                tasks: Vec::new(),
            });
    }

    /// Stops the system scheduler, ends every conversation this runtime
    /// owns (`ConversationEndReason::Shutdown`), and flushes the
    /// observation queue — the explicit end-of-life teardown.
    ///
    /// Idempotent: the first call performs the teardown; concurrent and
    /// later calls wait for its completion. In-flight turns are not
    /// awaited (quiesce before calling); conversations left open by a
    /// hard kill are reconciled by the next runtime's Monitor.
    pub async fn shutdown(&self) {
        let _guard = self.inner.shutdown_lock.lock().await;
        if self.inner.shutdown_done.load(Ordering::Acquire) {
            return;
        }
        // 1. Stop the system scheduler: no new sweeps compete with the
        //    teardown (in-flight executions are not awaited).
        if let Some(scheduler) = &self.inner.scheduler {
            scheduler.stop();
        }
        // 2. End every owned conversation (end_with is idempotent).
        let conversations: Vec<Conversation> = {
            let table = self
                .inner
                .conversations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            table
                .values()
                .map(|entry| entry.conversation.clone())
                .collect()
        };
        for conversation in conversations {
            conversation
                .end_with(self, ConversationEndReason::Shutdown)
                .await;
        }
        // 3. Flush: the facts emitted above have reached their observers.
        self.inner.event_bus.flush().await;
        self.inner.shutdown_done.store(true, Ordering::Release);
    }

    /// Test-only override for the drain budget (kept off the public
    /// configuration surface by design).
    #[cfg(test)]
    pub(crate) fn set_maintenance_drain_timeout(&mut self, timeout: Duration) {
        let inner = Arc::get_mut(&mut self.inner)
            .expect("test-only override requires a uniquely owned runtime");
        inner.maintenance_drain_timeout = timeout;
    }

    /// A read-only snapshot of the system scheduler's tasks — the
    /// developer-facing view (/proc style: name, period, policy, time to
    /// the next trigger, running). The system scheduler itself is not
    /// reachable for registration.
    pub fn scheduler_snapshot(&self) -> Vec<TaskInfo> {
        self.inner
            .scheduler
            .as_ref()
            .map(Scheduler::tasks)
            .unwrap_or_default()
    }

    /// The conversation-end teardown (the system's structural behavior):
    /// drains the conversation's background maintenance tasks, then
    /// mechanically promotes all L2 blocks into L3 (no model call, no
    /// strategy). Failures surface as `FlowFailed { moment:
    /// AtConversationEnd }` memory facts; success surfaces as `Promoted`.
    pub(crate) async fn finalize_conversation(&self, conversation: &Conversation) {
        // 1. Drain: the conversation's in-flight maintenance completes
        //    before the promotion reads L2 — no unfinished blocks. The
        //    drain is bounded per conversation: a stuck task delays
        //    teardown by at most the configured budget; stuck tasks and
        //    panicked tasks surface as FlowFailed facts.
        let entry = self
            .inner
            .conversations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(conversation.id());
        if let Some(entry) = entry {
            let handles = entry.tasks;
            let deadline = tokio::time::Instant::now() + self.inner.maintenance_drain_timeout;
            let total = handles.len();
            let mut timed_out = 0usize;
            for handle in handles {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    timed_out += 1;
                    continue;
                }
                match tokio::time::timeout(remaining, handle).await {
                    Ok(Ok(())) => {}
                    Ok(Err(join_error)) => {
                        self.inner
                            .event_bus
                            .emit(SynonzEvent::Memory(MemoryEvent::FlowFailed {
                                stage: MemoryFlowStage::Drain,
                                detail: format!("maintenance task failed: {join_error}"),
                                moment: MemoryFlowFailedMoment::AtConversationEnd,
                            }));
                    }
                    Err(_) => timed_out += 1,
                }
            }
            if timed_out > 0 {
                self.inner
                    .event_bus
                    .emit(SynonzEvent::Memory(MemoryEvent::FlowFailed {
                        stage: MemoryFlowStage::Drain,
                        detail: format!(
                            "drain timed out: {timed_out} of {total} maintenance task(s) still running"
                        ),
                        moment: MemoryFlowFailedMoment::AtConversationEnd,
                    }));
            }
        }

        // 2. Mechanical promotion: every L2 block becomes L3 knowledge
        //    under the conversation's topic.
        let subject = conversation.subject();
        let memory = &self.inner.memory;
        let l2_len = match memory.l2_len(subject, conversation.id()) {
            Ok(len) => len,
            Err(error) => {
                self.inner
                    .event_bus
                    .emit(SynonzEvent::Memory(MemoryEvent::FlowFailed {
                        stage: MemoryFlowStage::Distill,
                        detail: format!("promotion read: {error}"),
                        moment: MemoryFlowFailedMoment::AtConversationEnd,
                    }));
                return;
            }
        };
        if l2_len == 0 {
            return;
        }
        let blocks = match memory.l2_pop_oldest(subject, conversation.id(), l2_len) {
            Ok(blocks) => blocks,
            Err(error) => {
                self.inner
                    .event_bus
                    .emit(SynonzEvent::Memory(MemoryEvent::FlowFailed {
                        stage: MemoryFlowStage::Distill,
                        detail: format!("promotion pop: {error}"),
                        moment: MemoryFlowFailedMoment::AtConversationEnd,
                    }));
                return;
            }
        };
        let topic = conversation.topic().unwrap_or_default();
        let mut promoted = 0usize;
        for block in blocks {
            let fragment = crate::memory::KnowledgeFragment {
                identity: crate::memory::FragmentIdentity {
                    subject_id: subject.to_string(),
                    conversation_id: block.conversation_id,
                    topic: topic.clone(),
                },
                content: block.content,
                created_at: now_epoch(),
            };
            if let Err(error) = memory.l3_upsert(subject, fragment) {
                self.inner
                    .event_bus
                    .emit(SynonzEvent::Memory(MemoryEvent::FlowFailed {
                        stage: MemoryFlowStage::Distill,
                        detail: format!("promotion upsert: {error}"),
                        moment: MemoryFlowFailedMoment::AtConversationEnd,
                    }));
            } else {
                promoted += 1;
            }
        }
        if promoted > 0 {
            self.inner
                .event_bus
                .emit(SynonzEvent::Memory(MemoryEvent::Promoted {
                    conversation_id: conversation.id().to_string(),
                    count: promoted,
                }));
        }
    }

    /// Lists conversations matching a metadata keyword with keyset
    /// pagination — the Low Level track (conversation management, diagnostics).
    ///
    /// Ordering is `last_active` descending, `id` ascending; pass a
    /// previous page's `next` back through [`ConversationQuery::with_after`]
    /// to continue.
    pub fn list_conversations(
        &self,
        query: ConversationQuery,
    ) -> Result<ConversationPage, ConversationStoreError> {
        self.inner.conversation_store.list(query)
    }

    /// The idle sweep — the Monitor's task body. Paging and failure
    /// handling are shared with [`SynonzRuntime::sweep_idle_before`].
    pub(crate) async fn sweep_idle(&self) -> usize {
        let Some(timeout) = self.inner.conversation_idle_timeout else {
            return 0;
        };
        let threshold = now_epoch().saturating_sub(timeout.as_secs().max(1));
        self.sweep_idle_before(threshold).await
    }

    /// Sweeps conversations idle since `before` (epoch seconds), running
    /// their ConversationEnd flows. Returns how many were ended.
    ///
    /// Pages through the store's stale query: the cursor advances past
    /// failed entries (retried on the next sweep), so one bad conversation
    /// never blocks the rest.
    pub(crate) async fn sweep_idle_before(&self, before: u64) -> usize {
        let mut cursor = None;
        let mut ended = 0;
        while let Ok(page) =
            self.inner
                .conversation_store
                .list_stale(before, cursor, SWEEP_PAGE_SIZE)
        {
            if page.items.is_empty() {
                break;
            }
            for summary in &page.items {
                let Some(subject) = crate::Subject::parse(&summary.subject_id) else {
                    continue;
                };
                if let Ok(conversation) = Conversation::of(self, &subject, &summary.id) {
                    conversation
                        .end_with(self, ConversationEndReason::IdleSwept)
                        .await;
                    ended += 1;
                }
            }
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        ended
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{ConversationEvent, Observer, ObserverContext};
    use crate::conversation::Conversation;
    use crate::subject::{Subject, SubjectType};
    use std::sync::{Arc, Mutex};

    /// Records memory-flow failure facts for assertions.
    #[derive(Default, Clone)]
    struct FactRecorder {
        facts: Arc<Mutex<Vec<String>>>,
    }

    impl Observer for FactRecorder {
        fn on_event(&self, _ctx: &ObserverContext, event: &SynonzEvent) {
            if let SynonzEvent::Memory(MemoryEvent::FlowFailed { detail, .. }) = event {
                self.facts.lock().unwrap().push(detail.clone());
            }
        }
    }

    /// Records conversation-end facts for assertions.
    #[derive(Default, Clone)]
    struct EndRecorder {
        ends: Arc<Mutex<Vec<String>>>,
    }

    impl Observer for EndRecorder {
        fn on_event(&self, _ctx: &ObserverContext, event: &SynonzEvent) {
            if let SynonzEvent::Conversation(ConversationEvent::Ended {
                conversation_id,
                reason,
                ..
            }) = event
            {
                self.ends
                    .lock()
                    .unwrap()
                    .push(format!("{conversation_id}:{reason:?}"));
            }
        }
    }

    async fn wait_until_ended(runtime: &SynonzRuntime, subject: &Subject, id: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let conversation = Conversation::of(runtime, subject, id).unwrap();
            if conversation.is_ended() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the Monitor must end the idle conversation within the deadline"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn finalize_drain_is_bounded_and_visible() {
        let recorder = FactRecorder::default();
        let mut runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
        runtime.set_maintenance_drain_timeout(Duration::from_millis(50));
        let subject = Subject::of(SubjectType::User, "u-drain");
        let conversation = Conversation::with_id(&runtime, &subject, "drain-bounded");

        runtime
            .task_registry(conversation.id())
            .spawn(std::future::pending::<()>());

        let started = std::time::Instant::now();
        conversation.end(&runtime).await;
        assert!(conversation.is_ended());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the drain must be bounded (took {:?})",
            started.elapsed()
        );

        tokio::time::sleep(Duration::from_millis(50)).await;
        let facts = recorder.facts.lock().unwrap();
        assert!(
            facts.iter().any(|fact| fact.contains("drain timed out")),
            "the timeout must be visible: {facts:?}"
        );
    }

    #[tokio::test]
    async fn finalize_drain_surfaces_task_panics() {
        let recorder = FactRecorder::default();
        let runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
        let subject = Subject::of(SubjectType::User, "u-panic");
        let conversation = Conversation::with_id(&runtime, &subject, "drain-panic");

        runtime
            .task_registry(conversation.id())
            .spawn(async { panic!("maintenance boom") });
        // Let the task panic before teardown awaits it.
        tokio::time::sleep(Duration::from_millis(20)).await;

        conversation.end(&runtime).await;
        assert!(conversation.is_ended());

        tokio::time::sleep(Duration::from_millis(50)).await;
        let facts = recorder.facts.lock().unwrap();
        assert!(
            facts
                .iter()
                .any(|fact| fact.contains("maintenance task failed")),
            "the task panic must be visible: {facts:?}"
        );
    }

    #[tokio::test]
    async fn sweep_pages_through_many_stale_conversations() {
        let runtime = SynonzRuntime::builder().build();
        let subject = Subject::of(SubjectType::User, "u-sweep");
        let stale_stamp = now_epoch().saturating_sub(3600);
        let total = 130usize;
        for index in 0..total {
            runtime
                .inner
                .conversation_store
                .save(crate::conversation::ConversationState {
                    subject_id: subject.to_string(),
                    id: format!("sweep-{index:03}"),
                    turns: Vec::new(),
                    topic: None,
                    last_active: stale_stamp,
                    ended: false,
                })
                .unwrap();
        }

        let ended = runtime.sweep_idle_before(now_epoch()).await;
        assert_eq!(ended, total, "the sweep must page past one page size");
    }

    #[tokio::test]
    async fn monitor_ends_idle_conversations_automatically() {
        let recorder = EndRecorder::default();
        let runtime = SynonzRuntime::builder()
            .observer(recorder.clone())
            .conversation_idle_timeout(Duration::from_secs(1))
            .build();
        let subject = Subject::of(SubjectType::User, "u-monitor");
        let conversation = Conversation::with_id(&runtime, &subject, "idle-auto");
        assert!(!conversation.is_ended());

        wait_until_ended(&runtime, &subject, "idle-auto").await;

        tokio::time::sleep(Duration::from_millis(50)).await;
        let ends = recorder.ends.lock().unwrap();
        assert!(
            ends.iter().any(|end| end == "idle-auto:IdleSwept"),
            "the automatic sweep must be visible: {ends:?}"
        );
    }

    #[tokio::test]
    async fn monitor_reconciles_previously_stored_stale_conversations() {
        // A conversation left open by a previous process (simulated by a
        // pre-populated store) is reconciled by the immediate first sweep.
        let store = crate::inprocess::InProcessConversationStore::default();
        let subject = Subject::of(SubjectType::User, "u-orphan");
        store
            .save(crate::conversation::ConversationState {
                subject_id: subject.to_string(),
                id: "orphan".to_string(),
                turns: Vec::new(),
                topic: None,
                last_active: now_epoch().saturating_sub(3600),
                ended: false,
            })
            .unwrap();
        let runtime = SynonzRuntime::builder()
            .conversation_store(store)
            .conversation_idle_timeout(Duration::from_secs(1))
            .build();

        wait_until_ended(&runtime, &subject, "orphan").await;
    }

    #[tokio::test]
    async fn scheduler_snapshot_exposes_the_monitor() {
        let runtime = SynonzRuntime::builder()
            .conversation_idle_timeout(Duration::from_secs(1))
            .build();
        let snapshot = runtime.scheduler_snapshot();
        assert_eq!(snapshot.len(), 1);
        let monitor = &snapshot[0];
        assert_eq!(monitor.name.as_deref(), Some("monitor"));
        assert_eq!(monitor.policy, OverlapPolicy::Skip);
        assert_eq!(
            monitor.schedule.period(),
            monitor_tick(Duration::from_secs(1))
        );
    }

    #[tokio::test]
    async fn scheduler_snapshot_is_empty_without_configuration() {
        let runtime = SynonzRuntime::builder().build();
        assert!(runtime.scheduler_snapshot().is_empty());
    }

    #[test]
    fn build_panics_when_idle_timeout_has_no_execution_environment() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            SynonzRuntime::builder()
                .conversation_idle_timeout(Duration::from_secs(1))
                .build();
        }));
        assert!(
            result.is_err(),
            "build must reject a configured Monitor without an execution environment"
        );
    }

    #[test]
    fn executor_injection_allows_a_sync_build() {
        let host = tokio::runtime::Runtime::new().expect("host runtime");
        let runtime = SynonzRuntime::builder()
            .conversation_idle_timeout(Duration::from_secs(60))
            .executor(host.handle().clone())
            .build();
        assert_eq!(runtime.scheduler_snapshot().len(), 1);
        drop(runtime);
    }

    #[tokio::test]
    async fn shutdown_ends_owned_open_conversations() {
        let recorder = EndRecorder::default();
        let runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
        let subject = Subject::of(SubjectType::User, "u-shutdown");
        let open = Conversation::with_id(&runtime, &subject, "shutdown-open");
        let ended = Conversation::with_id(&runtime, &subject, "shutdown-ended");
        ended.end(&runtime).await;

        runtime.shutdown().await;

        assert!(open.is_ended());
        assert!(
            Conversation::of(&runtime, &subject, "shutdown-open")
                .unwrap()
                .is_ended()
        );
        let ends = recorder.ends.lock().unwrap();
        assert!(
            ends.iter().any(|end| end == "shutdown-open:Shutdown"),
            "the shutdown end reason must be visible: {ends:?}"
        );
        assert_eq!(
            ends.iter()
                .filter(|end| end.starts_with("shutdown-ended:"))
                .count(),
            1,
            "an explicitly ended conversation is not ended again: {ends:?}"
        );
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let recorder = EndRecorder::default();
        let runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
        let subject = Subject::of(SubjectType::User, "u-idem");
        let _conversation = Conversation::with_id(&runtime, &subject, "idem-1");

        runtime.shutdown().await;
        runtime.shutdown().await;

        let ends = recorder.ends.lock().unwrap();
        assert_eq!(
            ends.iter().filter(|end| end.starts_with("idem-1:")).count(),
            1,
            "shutdown ends each owned conversation exactly once: {ends:?}"
        );
    }

    #[tokio::test]
    async fn shutdown_only_ends_conversations_the_runtime_owns() {
        let store = crate::inprocess::InProcessConversationStore::default();
        let subject = Subject::of(SubjectType::User, "u-owner");
        store
            .save(crate::conversation::ConversationState {
                subject_id: subject.to_string(),
                id: "not-owned".to_string(),
                turns: Vec::new(),
                topic: None,
                last_active: now_epoch(),
                ended: false,
            })
            .unwrap();
        let runtime = SynonzRuntime::builder().conversation_store(store).build();

        runtime.shutdown().await;

        assert!(
            !Conversation::of(&runtime, &subject, "not-owned")
                .unwrap()
                .is_ended(),
            "an unowned stored conversation is not touched"
        );
    }

    #[tokio::test]
    async fn shutdown_flushes_observation_before_returning() {
        let recorder = EndRecorder::default();
        let runtime = SynonzRuntime::builder().observer(recorder.clone()).build();
        let subject = Subject::of(SubjectType::User, "u-flush");
        let _conversation = Conversation::with_id(&runtime, &subject, "flush-1");

        runtime.shutdown().await;

        // No sleep: the flush guarantees delivery before return.
        let ends = recorder.ends.lock().unwrap();
        assert!(
            ends.iter().any(|end| end == "flush-1:Shutdown"),
            "shutdown must flush observation: {ends:?}"
        );
    }
}
