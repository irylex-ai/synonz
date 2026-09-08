//! `cancellation`: the three cancellation entries — token, timeout, drop.
//!
//! Run: `cargo run -p synonz-examples --bin cancellation`

use std::time::Duration;

use futures::StreamExt;
use synonz::CancellationToken;
use synonz::{Agent, CancelReason, ExecutionEvent};

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
    let runtime = synonz::SynonzRuntime::builder().build();
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(HangingModel)
        .build()
        .expect("model is set");
    let subject = synonz::Subject::of(synonz::SubjectType::User, "demo");

    // Entry 1: an external token cancels with `UserRequested`.
    let token = CancellationToken::new();
    let mut conv = synonz::Conversation::new(&subject);
    let mut run = agent.run_with(conv.turn_input("go"), token.clone());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });
    while let Some(event) = run.next().await {
        if let Some(reason) = terminal(&event) {
            println!("token entry cancelled the run: {reason:?}");
            assert_eq!(reason, CancelReason::UserRequested);
            break;
        }
    }

    // Entry 2: the time budget cancels with `Timeout`.
    let mut conv2 = synonz::Conversation::new(&subject);
    let mut run = agent
        .run(conv2.turn_input("go"))
        .with_timeout(Duration::from_millis(50));
    while let Some(event) = run.next().await {
        if let Some(reason) = terminal(&event) {
            println!("timeout entry cancelled the run: {reason:?}");
            assert_eq!(reason, CancelReason::Timeout);
            break;
        }
    }

    // Entry 3: dropping the handle cancels the run (cooperative teardown).
    let mut conv3 = synonz::Conversation::new(&subject);
    let run = agent.run(conv3.turn_input("go"));
    drop(run);
    println!("drop entry: the run handle was dropped and torn down");
}

fn terminal(event: &synonz::ExecutionEvent) -> Option<CancelReason> {
    match event {
        ExecutionEvent::Cancelled(reason) => Some(*reason),
        _ => None,
    }
}
