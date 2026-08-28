//! (crate-internal) Cancellation engine: three entries, one signal.
//!
//! Per the cancellation ADR, the framework adopts the ecosystem primitive
//! ([`CancellationToken`]) and converges all cancellation entries onto a
//! single internal token:
//!
//! - **drop**: the run stream owns a [`CancelHandle`] whose drop guard
//!   cancels the token when the consumer walks away;
//! - **token**: an external token is linked as the *parent* of the internal
//!   token, so its cancellation propagates;
//! - **timeout**: an armed sleeper cancels the token and marks the outcome
//!   as [`CancelOutcome::Timeout`].
//!
//! Ownership is split: the [`CancelCore`] (signal + reason) is shared with
//! the loop; the [`CancelHandle`] (drop guard + arming) stays with the run
//! stream. Awaiting every suspension point on `cancelled()` (via `select!`)
//! is the loop's responsibility; this module only owns the signal and the
//! reason.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

/// What triggered the internal cancellation signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancelOutcome {
    /// The time budget armed on this run elapsed.
    Timeout,
    /// A consumer triggered the signal (drop, token, or upstream parent).
    Signal,
}

/// The shared cancellation signal and reason tracking.
pub(crate) struct CancelCore {
    token: CancellationToken,
    timeout_fired: Arc<AtomicBool>,
    // Poisoning is tolerated (`unwrap_or_else(into_inner)`): the guarded
    // section only swaps a task handle, so recovery is always sound.
    timeout_task: Mutex<Option<JoinHandle<()>>>,
}

// `is_cancelled`/`core` are part of the engine's contract for downstream
// milestones (S2/S3 consumers); the loop itself currently reads the signal
// only through `cancelled()`.
#[allow(dead_code)]
impl CancelCore {
    /// Creates a core for a run without an external token.
    pub(crate) fn new() -> Arc<Self> {
        Self::from_token(CancellationToken::new())
    }

    /// Creates a core whose signal fires whenever the parent token fires.
    pub(crate) fn child_of(parent: &CancellationToken) -> Arc<Self> {
        Self::from_token(parent.child_token())
    }

    fn from_token(token: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            timeout_fired: Arc::new(AtomicBool::new(false)),
            timeout_task: Mutex::new(None),
            token,
        })
    }

    /// The internal signal; suspension points select on its cancellation.
    pub(crate) fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Arms the time budget (once per run). Fires the signal with
    /// [`CancelOutcome::Timeout`] when `duration` elapses first.
    pub(crate) fn arm_timeout(self: &Arc<Self>, duration: Duration) {
        let mut task = self.timeout_task.lock().unwrap_or_else(|p| p.into_inner());
        if task.is_some() {
            return; // armed once; later calls are no-ops
        }
        let token = self.token.clone();
        let fired = Arc::clone(&self.timeout_fired);
        *task = Some(tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {
                    // Cancelled by another entry before the budget elapsed.
                }
                _ = tokio::time::sleep(duration) => {
                    fired.store(true, Ordering::Release);
                    token.cancel();
                }
            }
        }));
    }

    /// Resolves when the signal fires, reporting what triggered it.
    pub(crate) async fn cancelled(&self) -> CancelOutcome {
        self.token.cancelled().await;
        if self.timeout_fired.load(Ordering::Acquire) {
            CancelOutcome::Timeout
        } else {
            CancelOutcome::Signal
        }
    }

    /// Whether the signal has already fired.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

impl Drop for CancelCore {
    fn drop(&mut self) {
        if let Some(task) = self
            .timeout_task
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            task.abort();
        }
    }
}

/// The consumer-side cancellation handle: owns the drop entry.
///
/// Dropping the handle cancels the signal — this is how dropping a run
/// stream cancels the run. The handle also arms the time budget.
pub(crate) struct CancelHandle {
    core: Arc<CancelCore>,
    _drop_guard: DropGuard,
}

#[allow(dead_code)]
impl CancelHandle {
    /// Wraps a core with a drop guard.
    pub(crate) fn new(core: Arc<CancelCore>) -> Self {
        Self {
            _drop_guard: core.token.clone().drop_guard(),
            core,
        }
    }

    /// The shared signal core (used by the loop before the first yield).
    pub(crate) fn core(&self) -> &CancelCore {
        &self.core
    }

    /// Arms the time budget.
    pub(crate) fn arm_timeout(&self, duration: Duration) {
        self.core.arm_timeout(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drop_of_handle_cancels_signal() {
        let handle = CancelHandle::new(CancelCore::new());
        let token = handle.core().token().clone();
        assert!(!token.is_cancelled());
        drop(handle);
        token.cancelled().await;
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn parent_token_propagates() {
        let parent = CancellationToken::new();
        let handle = CancelHandle::new(CancelCore::child_of(&parent));
        parent.cancel();
        assert_eq!(handle.core().cancelled().await, CancelOutcome::Signal);
    }

    #[tokio::test]
    async fn timeout_fires_with_timeout_outcome() {
        let handle = CancelHandle::new(CancelCore::new());
        handle.arm_timeout(Duration::from_millis(20));
        assert_eq!(handle.core().cancelled().await, CancelOutcome::Timeout);
    }

    #[tokio::test]
    async fn inflight_future_is_dropped_at_cancellation() {
        // The mock in-flight work holds a sentinel; cancellation must drop it
        // (cooperative interruption at the await point).
        struct Sentinel(Arc<AtomicBool>);
        impl Drop for Sentinel {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let handle = CancelHandle::new(CancelCore::new());
        let token = handle.core().token().clone();
        let dropped = Arc::new(AtomicBool::new(false));
        let sentinel = Sentinel(Arc::clone(&dropped));

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            token.cancel();
        });

        let inflight = async move {
            let _held = sentinel;
            std::future::pending::<()>().await;
        };

        tokio::select! {
            outcome = handle.core().cancelled() => {
                assert_eq!(outcome, CancelOutcome::Signal);
            }
            _ = inflight => unreachable!("pending future must not resolve"),
        }
        assert!(dropped.load(Ordering::Acquire));
    }
}
