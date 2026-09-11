//! The time-driven task scheduler.
//!
//! A [`Scheduler`] owns one lightweight timing thread that computes
//! deadlines and submits executions; the executions themselves run on the
//! executor given at construction (the host runtime's worker pool). The
//! separation is deliberate: timing never blocks on execution, and a slow
//! or panicking task cannot stall the clock.
//!
//! Tasks are periodic: registration triggers immediately once, then every
//! period. Triggers that arrive while a previous execution is still
//! running follow the task's [`OverlapPolicy`].
//!
//! The runtime keeps an internal scheduler for system tasks (the idle
//! sweep); developers build their own instance with [`Scheduler::new`].

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;

use crate::BoxFuture;

/// How a periodic task handles a trigger that arrives while its previous
/// execution is still running.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverlapPolicy {
    /// Drop the trigger (the next one comes at the next period).
    Skip,
    /// Submit every trigger; executions may overlap.
    Concurrent,
    /// Keep at most one deferred run: when an execution completes, it
    /// runs once more if any trigger arrived meanwhile (missed triggers
    /// merge into a single catch-up; the backlog is bounded at one).
    Queue,
}

/// A task's recurrence rule (today: a fixed period).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    period: Duration,
}

impl Schedule {
    /// Fires immediately on registration, then every `period`.
    ///
    /// # Panics
    ///
    /// Panics when `period` is zero.
    pub fn every(period: Duration) -> Self {
        assert!(!period.is_zero(), "schedule period must be non-zero");
        Self { period }
    }

    /// The recurrence period.
    pub fn period(&self) -> Duration {
        self.period
    }
}

/// Read-only information about one registered task.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct TaskInfo {
    /// The registration id (unique within the scheduler).
    pub id: u64,
    /// The task's name, when it has one (system tasks are named).
    pub name: Option<String>,
    /// The recurrence rule.
    pub schedule: Schedule,
    /// The overlap policy.
    pub policy: OverlapPolicy,
    /// Time until the next trigger.
    pub next_in: Duration,
    /// Whether an execution is currently in flight.
    pub running: bool,
}

/// A handle to one scheduled task: identity and cancellation.
#[derive(Clone)]
pub struct TaskHandle {
    id: u64,
    shared: Arc<Shared>,
}

impl TaskHandle {
    /// The task's registration id.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Cancels future triggers (an in-flight execution is not stopped).
    pub fn cancel(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.tasks.retain(|task| task.id != self.id);
        self.shared.cond.notify_all();
    }
}

/// The time-driven task scheduler (see the module docs).
pub struct Scheduler {
    shared: Arc<Shared>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Scheduler {
    /// Creates a scheduler whose executions run on `executor`.
    ///
    /// The timing thread starts with the first task registration (a
    /// scheduler with no tasks costs no thread). Dropping the scheduler
    /// stops the timing thread; in-flight executions are left to finish.
    pub fn new(executor: Handle) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                cond: Condvar::new(),
                stop: AtomicBool::new(false),
                executor,
            }),
            thread: Mutex::new(None),
        }
    }

    /// Registers a periodic task and returns its handle.
    ///
    /// The task fires immediately once, then every period. The body is
    /// called on the executor for every execution (it produces the future
    /// to run).
    pub fn schedule<F, Fut>(&self, schedule: Schedule, policy: OverlapPolicy, task: F) -> TaskHandle
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.schedule_inner(None, schedule, policy, task)
    }

    /// Registers a named periodic task (the name appears in
    /// [`Scheduler::tasks`]; system tasks are named by the framework).
    pub fn schedule_named<F, Fut>(
        &self,
        name: impl Into<String>,
        schedule: Schedule,
        policy: OverlapPolicy,
        task: F,
    ) -> TaskHandle
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.schedule_inner(Some(name.into()), schedule, policy, task)
    }

    fn schedule_inner<F, Fut>(
        &self,
        name: Option<String>,
        schedule: Schedule,
        policy: OverlapPolicy,
        task: F,
    ) -> TaskHandle
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let body: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync> =
            Arc::new(move || Box::pin(task()));
        let inflight = Arc::new(AtomicUsize::new(0));
        let pending = Arc::new(AtomicBool::new(false));

        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.next_id += 1;
        let id = state.next_id;
        state.tasks.push(TaskEntry {
            id,
            name,
            period: schedule.period,
            next_deadline: Instant::now(),
            policy,
            inflight: Arc::clone(&inflight),
            pending: Arc::clone(&pending),
            body: Arc::clone(&body),
        });
        self.ensure_thread();
        // Notify while holding the state lock (no lost wakeups).
        self.shared.cond.notify_all();
        drop(state);

        TaskHandle {
            id,
            shared: Arc::clone(&self.shared),
        }
    }

    /// A read-only snapshot of the registered tasks.
    pub fn tasks(&self) -> Vec<TaskInfo> {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        state
            .tasks
            .iter()
            .map(|task| TaskInfo {
                id: task.id,
                name: task.name.clone(),
                schedule: Schedule {
                    period: task.period,
                },
                policy: task.policy,
                next_in: task.next_deadline.saturating_duration_since(now),
                running: task.inflight.load(Ordering::Acquire) > 0,
            })
            .collect()
    }

    /// Stops the timing thread (idempotent); in-flight executions are
    /// not waited for.
    pub(crate) fn stop(&self) {
        {
            let _state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.shared.stop.store(true, Ordering::Release);
            self.shared.cond.notify_all();
        }
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }

    /// Starts the timing thread on the first task registration.
    fn ensure_thread(&self) {
        let mut thread = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if thread.is_some() {
            return;
        }
        let shared = Arc::clone(&self.shared);
        *thread = Some(std::thread::spawn(move || run_loop(shared)));
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The scheduler's shared execution state.
struct Shared {
    state: Mutex<State>,
    cond: Condvar,
    stop: AtomicBool,
    executor: Handle,
}

/// The task table (guarded by [`Shared::state`]).
#[derive(Default)]
struct State {
    tasks: Vec<TaskEntry>,
    next_id: u64,
}

/// One registered task.
struct TaskEntry {
    id: u64,
    name: Option<String>,
    period: Duration,
    next_deadline: Instant,
    policy: OverlapPolicy,
    /// Currently in-flight executions of this task.
    inflight: Arc<AtomicUsize>,
    /// A trigger arrived while running (Queue's merged catch-up slot).
    pending: Arc<AtomicBool>,
    /// Produces the future of one execution (called on the executor).
    body: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
}

/// The timing loop: wait for the nearest deadline, submit due tasks, and
/// repeat until stopped. It never executes a task body itself.
fn run_loop(shared: Arc<Shared>) {
    const IDLE_WAIT: Duration = Duration::from_secs(3600);
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let now = Instant::now();
        let mut next_wake: Option<Instant> = None;
        for task in state.tasks.iter_mut() {
            if task.next_deadline <= now {
                task.next_deadline = advance(task.next_deadline, task.period, now);
                let due = match task.policy {
                    OverlapPolicy::Concurrent => {
                        task.inflight.fetch_add(1, Ordering::AcqRel);
                        true
                    }
                    OverlapPolicy::Skip => {
                        if task.inflight.load(Ordering::Acquire) == 0 {
                            task.inflight.fetch_add(1, Ordering::AcqRel);
                            true
                        } else {
                            false
                        }
                    }
                    OverlapPolicy::Queue => {
                        if task.inflight.load(Ordering::Acquire) == 0 {
                            task.inflight.fetch_add(1, Ordering::AcqRel);
                            true
                        } else {
                            task.pending.store(true, Ordering::Release);
                            false
                        }
                    }
                };
                if due {
                    spawn_execution(&shared, task);
                }
            }
            next_wake = Some(match next_wake {
                Some(wake) => wake.min(task.next_deadline),
                None => task.next_deadline,
            });
        }
        let wait = next_wake
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(IDLE_WAIT);
        // Registrations, cancellations, and stops notify this condvar;
        // spurious wakeups just recompute.
        let (guard, _) = shared
            .cond
            .wait_timeout(state, wait)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state = guard;
    }
}

/// Submits one execution of `task` to the executor.
fn spawn_execution(shared: &Shared, task: &TaskEntry) {
    let body = Arc::clone(&task.body);
    let inflight = Arc::clone(&task.inflight);
    let pending = Arc::clone(&task.pending);
    let policy = task.policy;
    shared.executor.spawn(async move {
        loop {
            let keep = AtomicBool::new(false);
            {
                // The guard releases the run slot on completion or panic.
                let _slot = SlotGuard {
                    inflight: &inflight,
                    keep: &keep,
                };
                (body)().await;
                if matches!(policy, OverlapPolicy::Queue) && pending.swap(false, Ordering::AcqRel) {
                    // A trigger arrived while running: run once more,
                    // keeping the slot occupied.
                    keep.store(true, Ordering::Release);
                }
            }
            if !keep.load(Ordering::Acquire) {
                break;
            }
        }
    });
}

/// Releases the run slot unless the queue continuation kept it.
struct SlotGuard<'a> {
    inflight: &'a AtomicUsize,
    keep: &'a AtomicBool,
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        if !self.keep.load(Ordering::Acquire) {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Advances a due deadline at a fixed rate with catch-up protection:
/// after a long stall the deadline jumps past `now` instead of firing
/// every missed period.
fn advance(deadline: Instant, period: Duration, now: Instant) -> Instant {
    if deadline > now {
        return deadline;
    }
    let behind = now.saturating_duration_since(deadline);
    let steps = behind.as_nanos() / period.as_nanos().max(1) + 1;
    let jump = period.saturating_mul(u32::try_from(steps).unwrap_or(u32::MAX));
    deadline + jump
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fires_immediately_then_periodically() {
        let counter = Arc::new(AtomicUsize::new(0));
        let scheduler = Scheduler::new(Handle::current());
        let body = {
            let counter = Arc::clone(&counter);
            move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            }
        };
        let handle = scheduler.schedule(
            Schedule::every(Duration::from_secs(3600)),
            OverlapPolicy::Skip,
            body,
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "registration fires immediately"
        );
        handle.cancel();
    }

    #[tokio::test]
    async fn skip_policy_drops_triggers_while_running() {
        let counter = Arc::new(AtomicUsize::new(0));
        let scheduler = Scheduler::new(Handle::current());
        let body = {
            let counter = Arc::clone(&counter);
            move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(120)).await;
                }
            }
        };
        let handle = scheduler.schedule(
            Schedule::every(Duration::from_millis(50)),
            OverlapPolicy::Skip,
            body,
        );
        tokio::time::sleep(Duration::from_millis(360)).await;
        handle.cancel();
        let count = counter.load(Ordering::SeqCst);
        assert!(
            (2..=4).contains(&count),
            "skip policy must drop overlapping triggers (ran {count} times)"
        );
    }

    #[tokio::test]
    async fn queue_policy_runs_back_to_back() {
        let counter = Arc::new(AtomicUsize::new(0));
        let scheduler = Scheduler::new(Handle::current());
        let body = {
            let counter = Arc::clone(&counter);
            move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
            }
        };
        let handle = scheduler.schedule(
            Schedule::every(Duration::from_millis(30)),
            OverlapPolicy::Queue,
            body,
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        handle.cancel();
        let count = counter.load(Ordering::SeqCst);
        assert!(
            (3..=5).contains(&count),
            "queue policy runs back-to-back under overload (ran {count} times)"
        );
    }

    #[tokio::test]
    async fn concurrent_policy_allows_overlap() {
        let current = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let scheduler = Scheduler::new(Handle::current());
        let body = {
            let current = Arc::clone(&current);
            let max_seen = Arc::clone(&max_seen);
            move || {
                let current = Arc::clone(&current);
                let max_seen = Arc::clone(&max_seen);
                async move {
                    let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    current.fetch_sub(1, Ordering::SeqCst);
                }
            }
        };
        let handle = scheduler.schedule(
            Schedule::every(Duration::from_millis(30)),
            OverlapPolicy::Concurrent,
            body,
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.cancel();
        assert!(
            max_seen.load(Ordering::SeqCst) >= 2,
            "concurrent policy must overlap executions"
        );
    }

    #[tokio::test]
    async fn long_task_does_not_block_other_triggers() {
        let slow = Arc::new(AtomicUsize::new(0));
        let fast = Arc::new(AtomicUsize::new(0));
        let scheduler = Scheduler::new(Handle::current());
        let slow_body = {
            let slow = Arc::clone(&slow);
            move || {
                let slow = Arc::clone(&slow);
                async move {
                    slow.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        };
        let fast_body = {
            let fast = Arc::clone(&fast);
            move || {
                let fast = Arc::clone(&fast);
                async move {
                    fast.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        };
        let slow_handle = scheduler.schedule(
            Schedule::every(Duration::from_millis(20)),
            OverlapPolicy::Skip,
            slow_body,
        );
        let fast_handle = scheduler.schedule(
            Schedule::every(Duration::from_millis(20)),
            OverlapPolicy::Skip,
            fast_body,
        );
        tokio::time::sleep(Duration::from_millis(220)).await;
        slow_handle.cancel();
        fast_handle.cancel();
        assert!(
            fast.load(Ordering::SeqCst) >= 5,
            "the fast task keeps triggering while the slow one runs"
        );
    }

    #[tokio::test]
    async fn panicking_task_does_not_kill_the_loop() {
        let counter = Arc::new(AtomicUsize::new(0));
        let scheduler = Scheduler::new(Handle::current());
        let body = {
            let counter = Arc::clone(&counter);
            move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    panic!("scheduled task boom");
                }
            }
        };
        let handle = scheduler.schedule(
            Schedule::every(Duration::from_millis(30)),
            OverlapPolicy::Skip,
            body,
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
        handle.cancel();
        assert!(
            counter.load(Ordering::SeqCst) >= 2,
            "the loop must keep firing after a task panic"
        );
    }

    #[tokio::test]
    async fn tasks_reports_registered_tasks() {
        let scheduler = Scheduler::new(Handle::current());
        let handle = scheduler.schedule(
            Schedule::every(Duration::from_secs(60)),
            OverlapPolicy::Queue,
            || async { tokio::time::sleep(Duration::from_millis(200)).await },
        );
        let named = scheduler.schedule_named(
            "system-heartbeat",
            Schedule::every(Duration::from_secs(30)),
            OverlapPolicy::Skip,
            || async { tokio::time::sleep(Duration::from_millis(200)).await },
        );

        // The immediate first runs start soon after registration (poll:
        // the exact start time is not asserted).
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let infos = scheduler.tasks();
            if infos.len() == 2 && infos.iter().all(|info| info.running) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the immediate runs must start"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let infos = scheduler.tasks();
        let info = infos
            .iter()
            .find(|info| info.id == handle.id())
            .expect("the unnamed task is reported");
        assert_eq!(info.policy, OverlapPolicy::Queue);
        assert_eq!(info.schedule.period(), Duration::from_secs(60));
        assert!(info.name.is_none());
        assert!(info.running);
        let named_info = infos
            .iter()
            .find(|info| info.id == named.id())
            .expect("the named task is reported");
        assert_eq!(named_info.name.as_deref(), Some("system-heartbeat"));

        handle.cancel();
        named.cancel();
        assert!(scheduler.tasks().is_empty());
    }
}
