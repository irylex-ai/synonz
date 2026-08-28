//! `cancellation`: the three cancellation entries — token, timeout, drop.
//!
//! Run: `cargo run -p synonz-examples --bin cancellation`

use std::time::Duration;

use futures::StreamExt;
use synonz::CancellationToken;
use synonz::{Agent, CancelReason, LifecycleEvent};

/// A model whose stream never finishes (to make cancellation observable).
struct HangingModel;

impl synonz::Model for HangingModel {
    fn stream(
        &self,
        _request: synonz::ModelRequest,
    ) -> synonz::BoxFuture<'_, Result<synonz::ModelStream, synonz::ModelError>> {
        Box::pin(async move { Ok(futures::stream::once(std::future::pending()).boxed()) })
    }
}

#[tokio::main]
async fn main() {
    let agent = Agent::builder()
        .model(HangingModel)
        .build()
        .expect("model is set");

    // Entry 1: an external token cancels with `UserRequested`.
    let token = CancellationToken::new();
    let mut run = agent.run_with("go", token.clone());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });
    while let Some(event) = run.next().await {
        if let Some(LifecycleEvent::Cancelled { reason }) = terminal(&event) {
            println!("token entry cancelled the run: {reason:?}");
            assert_eq!(reason, CancelReason::UserRequested);
            break;
        }
    }

    // Entry 2: the time budget cancels with `Timeout`.
    let mut run = agent.run("go").with_timeout(Duration::from_millis(50));
    while let Some(event) = run.next().await {
        if let Some(LifecycleEvent::Cancelled { reason }) = terminal(&event) {
            println!("timeout entry cancelled the run: {reason:?}");
            assert_eq!(reason, CancelReason::Timeout);
            break;
        }
    }

    // Entry 3: dropping the stream cancels the run (cooperative teardown).
    let mut run = agent.run("go");
    let _started = run.next().await; // Started
    let _requested = run.next().await; // Requested
    drop(run);
    println!("drop entry: the run stream was dropped and torn down");
}

fn terminal(event: &synonz::AgentEvent) -> Option<LifecycleEvent> {
    match event {
        synonz::AgentEvent::Lifecycle(lifecycle @ LifecycleEvent::Cancelled { .. }) => {
            Some(lifecycle.clone())
        }
        _ => None,
    }
}
