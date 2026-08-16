use std::time::{Duration, Instant};

use tokio::sync::Notify;

use super::native::schedule_native_actions;
use super::operation::finalize;
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
    let scheduler = SCHEDULER.lock().ok()?;
    let execution_start = scheduler
        .pending
        .iter()
        .chain(scheduler.active.iter())
        .filter(|job| job.execution_start_pending())
        .filter_map(|job| job.start_deadline)
        .min();
    let retry = scheduler
        .pending
        .front()
        .filter(|job| job.waiting_for_readiness)
        .and_then(|job| job.retry_at);
    execution_start.into_iter().chain(retry).min()
}

pub(super) fn process_due_timers(now: Instant) {
    let Some((expired, should_wake)) = SCHEDULER.lock().ok().map(|mut scheduler| {
        if let Some(active) = scheduler.active.as_mut() {
            active.expire_if_due(now);
        }

        let mut expired = Vec::new();
        let mut index = 0;

        while index < scheduler.pending.len() {
            let should_expire = scheduler.pending[index].deadline_is_due(now);

            if !should_expire {
                index += 1;
                continue;
            }

            let mut job = scheduler.pending.remove(index).expect("queued job exists");

            if job.expire_if_due(now) {
                let output = job.operation.output_sink();
                let outcome = finalize(&mut job.operation, &scheduler.documents, None);
                expired.push((job.completion, outcome, output));
            } else {
                scheduler.pending.insert(index, job);
                index += 1;
            }
        }

        if let Some(head) = scheduler.pending.front_mut()
            && head.waiting_for_readiness
            && head.retry_at.is_some_and(|retry_at| now >= retry_at)
        {
            head.waiting_for_readiness = false;
            head.retry_at = None;
        }

        let should_wake = scheduler.request_wake();
        (expired, should_wake)
    }) else {
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
    let should_wake = SCHEDULER.lock().is_ok_and(|mut scheduler| {
        if let Some(job) = scheduler.pending.front_mut() {
            job.waiting_for_readiness = false;
            job.retry_at = None;
            job.retry_delay = BUSY_RETRY_INITIAL;
        }

        scheduler.request_wake()
    });

    if should_wake {
        schedule_native_actions();
    }

    notify_changed();
}
