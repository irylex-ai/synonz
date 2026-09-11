//! `scheduler`: the time-driven task scheduler and the system lifecycle.
//!
//! Part 1 — a developer-built scheduler runs an "auto save" task with the
//! `Queue` policy (saves never overlap; missed triggers merge into one
//! catch-up).
//! Part 2 — a runtime with an idle timeout owns the Monitor (the system
//! idle sweep) and shuts down explicitly.
//!
//! Run: `cargo run -p synonz-examples --bin scheduler`

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use synonz::{OverlapPolicy, Schedule, Scheduler};

#[tokio::main]
async fn main() {
    // ── Part 1: the developer-facing Scheduler component ──
    // The timing thread only triggers and submits; the save body runs on
    // this runtime's worker pool.
    let scheduler = Scheduler::new(tokio::runtime::Handle::current());
    let saves = Arc::new(AtomicUsize::new(0));
    let handle = scheduler.schedule_named(
        "auto-save",
        Schedule::every(Duration::from_millis(200)),
        OverlapPolicy::Queue,
        {
            let saves = Arc::clone(&saves);
            move || {
                let saves = Arc::clone(&saves);
                async move {
                    // A save that sometimes overruns its period: the next
                    // trigger merges into one deferred run.
                    tokio::time::sleep(Duration::from_millis(350)).await;
                    let count = saves.fetch_add(1, Ordering::SeqCst) + 1;
                    println!("auto-save run #{count}");
                }
            }
        },
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    for info in scheduler.tasks() {
        println!(
            "task {:?}: period {:?}, policy {:?}, running: {}",
            info.name,
            info.schedule.period(),
            info.policy,
            info.running
        );
    }
    handle.cancel();
    drop(scheduler); // the timing thread stops with the scheduler

    // ── Part 2: the system lifecycle (Monitor + explicit shutdown) ──
    let runtime = synonz::SynonzRuntime::builder()
        .conversation_idle_timeout(Duration::from_secs(300))
        .build();
    let subject = synonz::Subject::of(synonz::SubjectType::User, "demo");
    let conversation = synonz::Conversation::new(&runtime, &subject);

    // The system scheduler is read-only for developers (no registration).
    for info in runtime.scheduler_snapshot() {
        println!(
            "system task {:?}: period {:?}, policy {:?}",
            info.name,
            info.schedule.period(),
            info.policy
        );
    }

    runtime.shutdown().await;
    println!(
        "after shutdown: the conversation is ended = {}",
        conversation.is_ended()
    );
}
