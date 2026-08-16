use std::future::Future;
use std::time::Duration;

use super::EXECUTION_RESPONSE_START_TIMEOUT;
use super::interrupt::Interrupts;

pub(super) enum ControlWait<T> {
    Ready(T),
    UnconfirmedDetach,
}

pub(super) enum ResponseStartWait<T> {
    Ready(T),
    TimedOut,
    UnconfirmedDetach,
}

pub(super) async fn wait_for_response_start<F>(
    future: F,
    interrupts: &mut Interrupts,
) -> ResponseStartWait<F::Output>
where
    F: Future,
{
    wait_for_response_start_with_timeout(future, interrupts, EXECUTION_RESPONSE_START_TIMEOUT).await
}

pub(super) async fn wait_for_response_start_with_timeout<F>(
    future: F,
    interrupts: &mut Interrupts,
    timeout: Duration,
) -> ResponseStartWait<F::Output>
where
    F: Future,
{
    tokio::pin!(future);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            biased;
            interrupt = interrupts.next() => {
                interrupts.note(interrupt);

                if interrupts.detach_requested() {
                    return ResponseStartWait::UnconfirmedDetach;
                }
            }
            () = &mut deadline, if !interrupts.cancellation_requested() => {
                return ResponseStartWait::TimedOut;
            }
            result = &mut future => return ResponseStartWait::Ready(result),
        }
    }
}

pub(super) async fn wait_for_control<F>(
    future: F,
    interrupts: &mut Interrupts,
) -> ControlWait<F::Output>
where
    F: Future,
{
    tokio::pin!(future);

    loop {
        tokio::select! {
            biased;
            interrupt = interrupts.next() => {
                interrupts.note(interrupt);

                if interrupts.detach_requested() {
                    return ControlWait::UnconfirmedDetach;
                }
            }
            result = &mut future => return ControlWait::Ready(result),
        }
    }
}

pub(super) enum StdoutWait<T> {
    Ready(T),
    Interrupted,
    UnconfirmedDetach,
}

pub(super) async fn wait_for_stdout<F>(
    future: F,
    interrupts: &mut Interrupts,
) -> StdoutWait<F::Output>
where
    F: Future,
{
    tokio::pin!(future);
    tokio::select! {
        biased;
        interrupt = interrupts.next() => {
            interrupts.note(interrupt);

            if interrupts.detach_requested() {
                StdoutWait::UnconfirmedDetach
            } else {
                StdoutWait::Interrupted
            }
        }
        result = &mut future => StdoutWait::Ready(result),
    }
}
