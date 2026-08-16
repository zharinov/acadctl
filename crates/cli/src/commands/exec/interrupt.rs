use std::future::pending;
use std::io;

use acadctl_rpc::{
    ExecCancelDisposition, ExecCancelRequest, ExecClientMessage, exec_client_message,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::writer::PipeWriter;

pub(super) enum Interrupt {
    Cancel { queued: bool },
    Detach,
}

pub(super) struct Interrupts {
    receiver: Option<mpsc::Receiver<Interrupt>>,
    task: JoinHandle<()>,
    diagnostics: PipeWriter,
    phase: InterruptPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InterruptPhase {
    Attached,
    CancelRequested(CancellationDisposition),
    DetachRequested(CancellationDisposition),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CancellationDisposition {
    NotQueued,
    Queued,
    Accepted,
    TooLate,
}

enum AcknowledgementResult {
    Recorded,
    RecordedBeforeInterrupt,
    Duplicate,
    Invalid,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CancellationReceipt {
    Continue,
    Detach,
    Duplicate,
    Invalid,
}

pub(super) enum DiagnosticWait {
    Complete,
    Interrupted,
}

impl Interrupts {
    pub(super) fn new(
        request: ExecClientMessage,
        diagnostics: PipeWriter,
    ) -> io::Result<(Self, mpsc::Receiver<ExecClientMessage>)> {
        let mut signals = interrupt_signal_stream()?;

        let (sender, outbound) = mpsc::channel(2);
        sender
            .try_send(request)
            .map_err(|_| io::Error::other("could not queue the execution request"))?;

        let (event_sender, receiver) = mpsc::channel(3);
        let task = tokio::spawn(async move {
            if signals.recv().await.is_none() {
                return;
            }

            if !publish_cancel(&sender, &event_sender).await {
                return;
            }

            if signals.recv().await.is_none() {
                return;
            }

            let _ = event_sender.send(Interrupt::Detach).await;
        });

        Ok((
            Self {
                receiver: Some(receiver),
                task,
                diagnostics,
                phase: InterruptPhase::Attached,
            },
            outbound,
        ))
    }

    pub(super) async fn next(&mut self) -> Interrupt {
        loop {
            let Some(receiver) = self.receiver.as_mut() else {
                return pending().await;
            };

            match receiver.recv().await {
                Some(interrupt) => return interrupt,
                None => self.receiver = None,
            }
        }
    }

    pub(super) fn note(&mut self, interrupt: Interrupt) {
        match (self.phase, interrupt) {
            (InterruptPhase::Attached, Interrupt::Cancel { queued }) => {
                let disposition = if queued {
                    CancellationDisposition::Queued
                } else {
                    CancellationDisposition::NotQueued
                };
                self.phase = InterruptPhase::CancelRequested(disposition);
                self.diagnostic("Stopping... ");
            }
            (InterruptPhase::CancelRequested(disposition), Interrupt::Detach) => {
                self.phase = InterruptPhase::DetachRequested(disposition);
            }
            (InterruptPhase::Attached, Interrupt::Detach)
            | (InterruptPhase::CancelRequested(_), Interrupt::Cancel { .. })
            | (InterruptPhase::DetachRequested(_), Interrupt::Cancel { .. } | Interrupt::Detach) => {
            }
        }
    }

    pub(super) fn cancellation_requested(&self) -> bool {
        !matches!(self.phase, InterruptPhase::Attached)
    }

    pub(super) fn detach_requested(&self) -> bool {
        matches!(self.phase, InterruptPhase::DetachRequested(_))
    }

    #[cfg(test)]
    pub(super) fn cancellation_acknowledged(&self) -> bool {
        matches!(
            self.cancellation_disposition(),
            Some(CancellationDisposition::Accepted | CancellationDisposition::TooLate)
        )
    }

    fn cancellation_disposition(&self) -> Option<CancellationDisposition> {
        match self.phase {
            InterruptPhase::Attached => None,
            InterruptPhase::CancelRequested(disposition)
            | InterruptPhase::DetachRequested(disposition) => Some(disposition),
        }
    }

    fn acknowledge_cancellation(
        &mut self,
        disposition: CancellationDisposition,
    ) -> AcknowledgementResult {
        let current = match self.cancellation_disposition() {
            Some(current) => current,
            None => {
                self.phase = InterruptPhase::CancelRequested(disposition);
                return AcknowledgementResult::RecordedBeforeInterrupt;
            }
        };

        match current {
            CancellationDisposition::Queued => {}
            CancellationDisposition::Accepted | CancellationDisposition::TooLate => {
                return AcknowledgementResult::Duplicate;
            }
            CancellationDisposition::NotQueued => return AcknowledgementResult::Invalid,
        }

        self.phase = match self.phase {
            InterruptPhase::CancelRequested(_) => InterruptPhase::CancelRequested(disposition),
            InterruptPhase::DetachRequested(_) => InterruptPhase::DetachRequested(disposition),
            InterruptPhase::Attached => return AcknowledgementResult::Invalid,
        };

        AcknowledgementResult::Recorded
    }

    pub(super) fn record_cancellation_acknowledgement(
        &mut self,
        disposition: i32,
    ) -> CancellationReceipt {
        let disposition = match ExecCancelDisposition::try_from(disposition) {
            Ok(ExecCancelDisposition::Accepted) => CancellationDisposition::Accepted,
            Ok(ExecCancelDisposition::TooLate) => CancellationDisposition::TooLate,
            Ok(ExecCancelDisposition::Unspecified) | Err(_) => {
                return CancellationReceipt::Invalid;
            }
        };

        match self.acknowledge_cancellation(disposition) {
            AcknowledgementResult::Recorded => {}
            AcknowledgementResult::RecordedBeforeInterrupt => {}
            AcknowledgementResult::Duplicate => return CancellationReceipt::Duplicate,
            AcknowledgementResult::Invalid => return CancellationReceipt::Invalid,
        }

        if self.detach_requested() {
            return CancellationReceipt::Detach;
        }

        CancellationReceipt::Continue
    }

    pub(super) async fn write_diagnostic(&mut self, text: String) -> DiagnosticWait {
        let diagnostics = self.diagnostics.clone();
        let write = diagnostics.write(text);
        tokio::pin!(write);

        tokio::select! {
            biased;
            interrupt = self.next() => {
                self.note(interrupt);
                DiagnosticWait::Interrupted
            }
            _ = &mut write => DiagnosticWait::Complete,
        }
    }

    pub(super) fn finish_stopping(&self, message: &str) {
        if self.cancellation_requested() {
            self.diagnostic(&format!("{message}\n"));
        }
    }

    pub(super) fn diagnostic(&self, text: &str) {
        self.diagnostics.try_write(text.to_owned());
    }

    #[cfg(test)]
    pub(super) fn test_pair() -> (Self, mpsc::Sender<Interrupt>) {
        let diagnostics = PipeWriter::spawn(io::sink(), 8, "acadctl-test-stderr").unwrap();
        Self::test_with_diagnostics(diagnostics)
    }

    #[cfg(test)]
    pub(super) fn test_with_diagnostics(
        diagnostics: PipeWriter,
    ) -> (Self, mpsc::Sender<Interrupt>) {
        let (sender, receiver) = mpsc::channel(3);
        let task = tokio::spawn(pending::<()>());
        (
            Self {
                receiver: Some(receiver),
                task,
                diagnostics,
                phase: InterruptPhase::Attached,
            },
            sender,
        )
    }
}

impl Drop for Interrupts {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) async fn publish_cancel(
    sender: &mpsc::Sender<ExecClientMessage>,
    event_sender: &mpsc::Sender<Interrupt>,
) -> bool {
    let queued = sender
        .send(ExecClientMessage {
            message: Some(exec_client_message::Message::Cancel(ExecCancelRequest {})),
        })
        .await
        .is_ok();

    event_sender
        .send(Interrupt::Cancel { queued })
        .await
        .is_ok()
}

#[cfg(unix)]
type InterruptSignalStream = tokio::signal::unix::Signal;

#[cfg(windows)]
type InterruptSignalStream = tokio::signal::windows::CtrlC;

#[cfg(unix)]
fn interrupt_signal_stream() -> io::Result<InterruptSignalStream> {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
}

#[cfg(windows)]
fn interrupt_signal_stream() -> io::Result<InterruptSignalStream> {
    tokio::signal::windows::ctrl_c()
}
