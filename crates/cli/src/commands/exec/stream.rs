use std::future::Future;

use super::interrupt::Interrupts;

pub(super) enum ControlWait<T> {
    Ready(T),
    UnconfirmedDetach,
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
