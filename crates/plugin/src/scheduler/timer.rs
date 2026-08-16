use std::time::{Duration, Instant};

use tokio::sync::Notify;

use super::native::schedule_native_actions;
use super::queue::SCHEDULER;

pub(super) const EXECUTION_START_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const BUSY_RETRY_INITIAL: Duration = Duration::from_millis(50);
pub(super) const BUSY_RETRY_MAX: Duration = Duration::from_millis(500);

static TIMERS_CHANGED: Notify = Notify::const_new();

pub(super) fn notify_changed() {
    TIMERS_CHANGED.notify_one();
}

pub(crate) async fn drive_timers() {
    loop {
        let changed = TIMERS_CHANGED.notified();

        match next_timer_deadline() {
            Some(deadline) => {
                tokio::select! {
                    _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                        process_due_timers(Instant::now());
                    }
                    _ = changed => {}
                }
            }
            None => changed.await,
        }
    }
}

fn next_timer_deadline() -> Option<Instant> {
    SCHEDULER.lock().ok()?.next_timer_deadline()
}

pub(super) fn process_due_timers(now: Instant) {
    let Some((expired, should_wake)) = SCHEDULER
        .lock()
        .ok()
        .map(|mut scheduler| scheduler.process_due_timers(now))
    else {
        return;
    };

    for (completion, outcome, output) in expired {
        if let Some(output) = output {
            output.finish();
        }

        let _ = completion.send(outcome);
    }

    if should_wake {
        schedule_native_actions();
    }
}

pub(crate) fn native_state_may_be_ready() {
    let should_wake = SCHEDULER
        .lock()
        .is_ok_and(|mut scheduler| scheduler.native_state_may_be_ready());

    if should_wake {
        schedule_native_actions();
    }

    notify_changed();
}
