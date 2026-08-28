//! (crate-internal) Cancellation engine: three entries, one signal.
//!
//! Per the cancellation ADR, the framework adopts the ecosystem primitive
//! ([`CancellationToken`]) and converges all cancellation entries onto a
//! single internal token:
//!
//! - **drop**: the run stream owns a drop guard; dropping it cancels;
//! - **token**: an external token is linked as the *parent* of the internal
//!   token, so its cancellation propagates;
//! - **timeout**: an armed sleeper cancels the token and marks the outcome
//!   as [`CancelOutcome::Timeout`].
//!
//! Awaiting every suspension point on `cancelled()` (via `select!`) is the
//! loop's responsibility; this module only owns the signal and the reason.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

/// What triggered the internal cancellation signal.
// Wired by the run loop (M3); the engine and its tests land in M2 per plan.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancelOutcome {
    /// The time budget armed on this run elapsed.
    Timeout,
    /// A consumer triggered the signal (drop, token, or upstream parent).
    Signal,
}

/// The run's internal cancellation signal.
#[allow(dead_code)]
pub(crate) struct CancelEngine {
    token: CancellationToken,
    timeout_fired: Arc<AtomicBool>,
    timeout_task: Option<JoinHandle<()>>,
    // Cancels the token when the engine (and therefore the run stream) is
    // dropped. Drops after `Drop::drop`, so explicit cleanup runs first.
    _drop_guard: DropGuard,
}

// Wired by the run loop (M3); the engine and its tests land in M2 per plan.
#[allow(dead_code)]
impl CancelEngine {
    /// Creates an engine for a run without an external token.
    pub(crate) fn new() -> Self {
        Self::from_token(CancellationToken::new())
    }

    /// Creates an engine whose signal fires whenever the parent token fires.
    pub(crate) fn child_of(parent: &CancellationToken) -> Self {
        Self::from_token(parent.child_token())
    }

    fn from_token(token: CancellationToken) -> Self {
        Self {
            _drop_guard: token.clone().drop_guard(),
            timeout_fired: Arc::new(AtomicBool::new(false)),
            timeout_task: None,
            token,
        }
    }

    /// The internal signal; suspension points select on its cancellation.
    pub(crate) fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Arms the time budget. Fires the signal with
    /// [`CancelOutcome::Timeout`] when `duration` elapses first.
    pub(crate) fn arm_timeout(&mut self, duration: Duration) {
        let token = self.token.clone();
        let fired = Arc::clone(&self.timeout_fired);
        self.timeout_task = Some(tokio::spawn(async move {
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

impl Drop for CancelEngine {
    fn drop(&mut self) {
        if let Some(task) = self.timeout_task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drop_of_engine_cancels_signal() {
        let engine = CancelEngine::new();
        let token = engine.token().clone();
        assert!(!token.is_cancelled());
        drop(engine);
        token.cancelled().await;
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn parent_token_propagates() {
        let parent = CancellationToken::new();
        let engine = CancelEngine::child_of(&parent);
        parent.cancel();
        assert_eq!(engine.cancelled().await, CancelOutcome::Signal);
    }

    #[tokio::test]
    async fn timeout_fires_with_timeout_outcome() {
        let mut engine = CancelEngine::new();
        engine.arm_timeout(Duration::from_millis(20));
        assert_eq!(engine.cancelled().await, CancelOutcome::Timeout);
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

        let engine = CancelEngine::new();
        let token = engine.token().clone();
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
            outcome = engine.cancelled() => {
                assert_eq!(outcome, CancelOutcome::Signal);
            }
            _ = inflight => unreachable!("pending future must not resolve"),
        }
        assert!(dropped.load(Ordering::Acquire));
    }
}
