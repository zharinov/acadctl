use std::future::{Future, pending};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use acadctl_rpc::{
    ExecutionCancelDisposition, ExecutionCancelRequest, ExecutionClientMessage, ExecutionFailure,
    ExecutionMode, ExecutionRequest, ExecutionServerEvent, execution_client_message,
    execution_outcome, execution_server_event,
};
use futures_util::stream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tonic::Code;

use crate::instances::QueryError;

use super::{fail, query_error_message, request_error_message};

const EXECUTION_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const EXECUTION_RESPONSE_START_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run(id: String, file: Option<PathBuf>, mode: ExecutionMode) -> ExitCode {
    let source = match crate::source::read(file.as_deref(), mode == ExecutionMode::Eval) {
        Ok(source) => source,
        Err(error) => {
            error.report();
            return ExitCode::FAILURE;
        }
    };
    let process_id = match super::target::resolve_process_id(&id).await {
        Ok(process_id) => process_id,
        Err(error) => return fail(error),
    };
    let mut client = match tokio::time::timeout(
        EXECUTION_CONNECT_TIMEOUT,
        crate::instances::connect_execution(process_id),
    )
    .await
    {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => return fail(query_error_message(&error)),
        Err(_) => return fail(query_error_message(&QueryError::TimedOut)),
    };

    let source_name = source.name.clone();
    let request = ExecutionClientMessage {
        message: Some(execution_client_message::Message::Request(
            ExecutionRequest {
                document_id: id,
                mode: mode as i32,
                source_name: source.name,
                source: source.bytes,
            },
        )),
    };
    let diagnostics = match PipeWriter::stderr() {
        Ok(diagnostics) => diagnostics,
        Err(error) => return fail(format!("Could not start the stderr writer: {error}")),
    };
    let (mut interrupts, receiver) = match Interrupts::new(request, diagnostics) {
        Ok(interrupts) => interrupts,
        Err(error) => return fail(format!("Could not install the Ctrl+C handler: {error}")),
    };
    let outbound = stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|message| (message, receiver))
    });
    let response = match wait_for_response_start(client.execute(outbound), &mut interrupts).await {
        ResponseStartWait::Ready(Ok(response)) => response,
        ResponseStartWait::Ready(Err(status)) => {
            if interrupts.force_detach_requested() {
                return unconfirmed_detach_exit(&interrupts);
            }
            return diagnostic_failure(&mut interrupts, response_start_error(status)).await;
        }
        ResponseStartWait::TimedOut => {
            return diagnostic_failure(
                &mut interrupts,
                "Timed out waiting for AutoCAD to respond to the execution request. The request may still have been accepted; do not retry it blindly."
                    .into(),
            )
            .await;
        }
        ResponseStartWait::UnconfirmedDetach => return unconfirmed_detach_exit(&interrupts),
    };

    receive_response(response.into_inner(), &source_name, &mut interrupts).await
}

async fn receive_response(
    mut response: tonic::Streaming<ExecutionServerEvent>,
    source_name: &str,
    interrupts: &mut Interrupts,
) -> ExitCode {
    let mut accepted = false;
    let mut cancellation_acknowledged = false;
    let mut stdout = None;
    loop {
        let event =
            match wait_for_control(response.message(), interrupts, cancellation_acknowledged).await
            {
                ControlWait::Ready(Ok(Some(event))) => event,
                ControlWait::Ready(Ok(None)) => {
                    if interrupts.force_detach_requested() {
                        return unconfirmed_detach_exit(interrupts);
                    }
                    return lost_response(interrupts, accepted, "The execution stream ended").await;
                }
                ControlWait::Ready(Err(status)) => {
                    if interrupts.force_detach_requested() {
                        return unconfirmed_detach_exit(interrupts);
                    }
                    let detail = if status.message().is_empty() {
                        "The execution connection failed".into()
                    } else {
                        format!("The execution connection failed: {}", status.message())
                    };
                    return lost_response(interrupts, accepted, &detail).await;
                }
                ControlWait::ConfirmedDetach => return confirmed_detach_exit(interrupts),
                ControlWait::UnconfirmedDetach => return unconfirmed_detach_exit(interrupts),
            };

        match event.event {
            Some(execution_server_event::Event::Accepted(_)) if !accepted => accepted = true,
            Some(execution_server_event::Event::Accepted(_)) => {
                return invalid_response(
                    interrupts,
                    "AutoCAD accepted the execution more than once",
                )
                .await;
            }
            Some(execution_server_event::Event::Output(output)) if accepted => {
                if interrupts.cancellation_requested() {
                    continue;
                }
                let writer = match stdout.get_or_insert_with(PipeWriter::stdout).as_ref() {
                    Ok(writer) => writer,
                    Err(error) => {
                        drop(response);
                        return diagnostic_failure(
                            interrupts,
                            format!(
                                "Could not start the stdout writer: {error}. The accepted AutoCAD job may still be running; do not retry it blindly."
                            ),
                        )
                        .await;
                    }
                };
                match wait_for_stdout(
                    writer.write(output.text),
                    interrupts,
                    cancellation_acknowledged,
                )
                .await
                {
                    StdoutWait::Ready(Ok(())) => {}
                    StdoutWait::Ready(Err(error)) => {
                        drop(response);
                        return diagnostic_failure(
                            interrupts,
                            format!(
                                "Could not write stdout: {error}. The accepted AutoCAD job may still be running; do not retry it blindly."
                            ),
                        )
                        .await;
                    }
                    StdoutWait::Interrupted => continue,
                    StdoutWait::ConfirmedDetach => return confirmed_detach_exit(interrupts),
                    StdoutWait::UnconfirmedDetach => return unconfirmed_detach_exit(interrupts),
                }
            }
            Some(execution_server_event::Event::Output(_)) => {
                return invalid_response(
                    interrupts,
                    "AutoCAD sent output before accepting the execution",
                )
                .await;
            }
            Some(execution_server_event::Event::CancelAcknowledgement(acknowledgement))
                if accepted =>
            {
                if cancellation_acknowledged {
                    return invalid_response(
                        interrupts,
                        "AutoCAD acknowledged the same cancellation more than once",
                    )
                    .await;
                }
                match ExecutionCancelDisposition::try_from(acknowledgement.disposition) {
                    Ok(ExecutionCancelDisposition::Accepted) => {
                        cancellation_acknowledged = true;
                        if interrupts.detach_requested() {
                            return confirmed_detach_exit(interrupts);
                        }
                    }
                    Ok(ExecutionCancelDisposition::TooLate) => {
                        interrupts.notice(
                            "acadctl: cancellation was too late; detaching while AutoCAD finishes.",
                        );
                        return cancelled_exit();
                    }
                    Ok(ExecutionCancelDisposition::Unspecified) | Err(_) => {
                        return invalid_response(
                            interrupts,
                            "AutoCAD returned an invalid cancellation acknowledgement",
                        )
                        .await;
                    }
                }
            }
            Some(execution_server_event::Event::CancelAcknowledgement(_)) => {
                return invalid_response(
                    interrupts,
                    "AutoCAD acknowledged cancellation before accepting the execution",
                )
                .await;
            }
            Some(execution_server_event::Event::Finished(finished)) => {
                let Some(outcome) = finished.outcome.and_then(|outcome| outcome.outcome) else {
                    return invalid_response(
                        interrupts,
                        "AutoCAD returned an empty execution result",
                    )
                    .await;
                };
                return match outcome {
                    execution_outcome::Outcome::Success(_)
                        if accepted && interrupts.cancellation_requested() =>
                    {
                        cancelled_exit()
                    }
                    execution_outcome::Outcome::Success(_) if accepted => ExitCode::SUCCESS,
                    execution_outcome::Outcome::Cancelled(_) if accepted => cancelled_exit(),
                    execution_outcome::Outcome::Failure(failure) => {
                        report_failure(interrupts, failure, source_name).await
                    }
                    execution_outcome::Outcome::Success(_)
                    | execution_outcome::Outcome::Cancelled(_) => {
                        invalid_response(
                            interrupts,
                            "AutoCAD finished an execution that it did not accept",
                        )
                        .await
                    }
                };
            }
            None => {
                return invalid_response(interrupts, "AutoCAD returned an empty execution event")
                    .await;
            }
        }
    }
}

async fn report_failure(
    interrupts: &mut Interrupts,
    failure: ExecutionFailure,
    fallback_source_name: &str,
) -> ExitCode {
    let mut text = failure_lines(&failure, fallback_source_name).join("\n");
    text.push('\n');
    match write_diagnostic(interrupts, text).await {
        DiagnosticWait::Complete => ExitCode::FAILURE,
        DiagnosticWait::Interrupted => cancelled_exit(),
    }
}

fn failure_lines(failure: &ExecutionFailure, fallback_source_name: &str) -> Vec<String> {
    let message = if failure.message.is_empty() {
        "AutoCAD could not complete the execution."
    } else {
        &failure.message
    };
    match (&failure.location, failure.form_index) {
        (Some(location), Some(form_index)) => {
            let source_name = if location.source_name.is_empty() {
                fallback_source_name
            } else {
                &location.source_name
            };
            vec![
                format!(
                    "Execution error in {source_name}, form {form_index} (line {}).",
                    location.line
                ),
                message.into(),
            ]
        }
        (Some(location), None) => {
            let source_name = if location.source_name.is_empty() {
                fallback_source_name
            } else {
                &location.source_name
            };
            vec![
                format!(
                    "Read error in {source_name} (line {}, column {}).",
                    location.line, location.column
                ),
                message.into(),
            ]
        }
        (None, Some(form_index)) => vec![
            format!("Execution error in {fallback_source_name}, form {form_index}."),
            message.into(),
        ],
        (None, None) => vec![format!("acadctl: {message}")],
    }
}

fn response_start_error(status: tonic::Status) -> String {
    if status.code() == Code::Unimplemented {
        return request_error_message("start the AutoLISP execution", status);
    }
    let detail = if status.message().is_empty() {
        String::new()
    } else {
        format!(": {}", status.message())
    };
    format!(
        "Could not confirm whether AutoCAD accepted the execution{detail}. The request may still have been accepted; do not retry it blindly."
    )
}

async fn lost_response(interrupts: &mut Interrupts, accepted: bool, detail: &str) -> ExitCode {
    let message = if accepted {
        format!(
            "{detail} before a result was returned. The accepted AutoCAD job may still be running; do not retry it blindly."
        )
    } else {
        format!(
            "{detail} before acceptance was reported. The request may still have been accepted; do not retry it blindly."
        )
    };
    diagnostic_failure(interrupts, message).await
}

async fn invalid_response(interrupts: &mut Interrupts, detail: &str) -> ExitCode {
    diagnostic_failure(
        interrupts,
        format!(
            "Invalid execution response: {detail}. The execution outcome is unknown; do not retry it blindly."
        ),
    )
    .await
}

fn cancelled_exit() -> ExitCode {
    ExitCode::from(130)
}

fn confirmed_detach_exit(interrupts: &Interrupts) -> ExitCode {
    interrupts.notice("acadctl: detached; AutoCAD acknowledged the cancellation request.");
    cancelled_exit()
}

fn unconfirmed_detach_exit(interrupts: &Interrupts) -> ExitCode {
    interrupts.notice(
        "acadctl: detached without cancellation confirmation; the accepted AutoCAD job may still be running.",
    );
    cancelled_exit()
}

async fn diagnostic_failure(interrupts: &mut Interrupts, message: String) -> ExitCode {
    match write_diagnostic(interrupts, format!("acadctl: {message}\n")).await {
        DiagnosticWait::Complete => ExitCode::FAILURE,
        DiagnosticWait::Interrupted => cancelled_exit(),
    }
}

enum DiagnosticWait {
    Complete,
    Interrupted,
}

async fn write_diagnostic(interrupts: &mut Interrupts, text: String) -> DiagnosticWait {
    let diagnostics = interrupts.diagnostics.clone();
    let write = diagnostics.write(text);
    tokio::pin!(write);
    tokio::select! {
        biased;
        interrupt = interrupts.next() => {
            interrupts.note(interrupt);
            DiagnosticWait::Interrupted
        }
        _ = &mut write => DiagnosticWait::Complete,
    }
}

enum Interrupt {
    Cancel { queued: bool },
    Detach,
    ForceDetach,
}

struct Interrupts {
    receiver: Option<mpsc::Receiver<Interrupt>>,
    task: JoinHandle<()>,
    diagnostics: PipeWriter,
    cancellation_requested: bool,
    cancel_queued: bool,
    detach_requested: bool,
    force_detach_requested: bool,
}

impl Interrupts {
    fn new(
        request: ExecutionClientMessage,
        diagnostics: PipeWriter,
    ) -> io::Result<(Self, mpsc::Receiver<ExecutionClientMessage>)> {
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
            if event_sender.send(Interrupt::Detach).await.is_err() {
                return;
            }
            if signals.recv().await.is_none() {
                return;
            }
            let _ = event_sender.send(Interrupt::ForceDetach).await;
        });
        Ok((
            Self {
                receiver: Some(receiver),
                task,
                diagnostics,
                cancellation_requested: false,
                cancel_queued: false,
                detach_requested: false,
                force_detach_requested: false,
            },
            outbound,
        ))
    }

    async fn next(&mut self) -> Interrupt {
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

    fn note(&mut self, interrupt: Interrupt) {
        match interrupt {
            Interrupt::Cancel { queued } if !self.cancellation_requested => {
                self.cancellation_requested = true;
                self.cancel_queued = queued;
                if queued {
                    self.notice("acadctl: cancellation requested; press Ctrl+C again to detach.");
                } else {
                    self.notice(
                        "acadctl: cancellation could not be sent; press Ctrl+C again to request detachment.",
                    );
                }
            }
            Interrupt::Detach if !self.detach_requested => {
                self.detach_requested = true;
                if self.cancel_queued {
                    self.notice(
                        "acadctl: detach requested; waiting for AutoCAD to acknowledge cancellation. Press Ctrl+C again to detach without confirmation.",
                    );
                } else {
                    self.notice(
                        "acadctl: detach requested, but cancellation was not queued. Press Ctrl+C again to detach without confirmation.",
                    );
                }
            }
            Interrupt::ForceDetach => self.force_detach_requested = true,
            Interrupt::Cancel { .. } | Interrupt::Detach => {}
        }
    }

    fn cancellation_requested(&self) -> bool {
        self.cancellation_requested
    }

    fn detach_requested(&self) -> bool {
        self.detach_requested
    }

    fn force_detach_requested(&self) -> bool {
        self.force_detach_requested
    }

    fn notice(&self, message: &str) {
        self.diagnostic(&format!("{message}\n"));
    }

    fn diagnostic(&self, text: &str) {
        self.diagnostics.try_write(text.to_owned());
    }

    #[cfg(test)]
    fn test_pair() -> (Self, mpsc::Sender<Interrupt>) {
        let (sender, receiver) = mpsc::channel(3);
        let task = tokio::spawn(pending::<()>());
        let diagnostics = PipeWriter::spawn(io::sink(), 8, "acadctl-test-stderr").unwrap();
        (
            Self {
                receiver: Some(receiver),
                task,
                diagnostics,
                cancellation_requested: false,
                cancel_queued: false,
                detach_requested: false,
                force_detach_requested: false,
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

async fn publish_cancel(
    sender: &mpsc::Sender<ExecutionClientMessage>,
    event_sender: &mpsc::Sender<Interrupt>,
) -> bool {
    let queued = sender
        .send(ExecutionClientMessage {
            message: Some(execution_client_message::Message::Cancel(
                ExecutionCancelRequest {},
            )),
        })
        .await
        .is_ok();
    event_sender
        .send(Interrupt::Cancel { queued })
        .await
        .is_ok()
}

enum ControlWait<T> {
    Ready(T),
    ConfirmedDetach,
    UnconfirmedDetach,
}

enum ResponseStartWait<T> {
    Ready(T),
    TimedOut,
    UnconfirmedDetach,
}

async fn wait_for_response_start<F>(
    future: F,
    interrupts: &mut Interrupts,
) -> ResponseStartWait<F::Output>
where
    F: Future,
{
    wait_for_response_start_with_timeout(future, interrupts, EXECUTION_RESPONSE_START_TIMEOUT).await
}

async fn wait_for_response_start_with_timeout<F>(
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
                if interrupts.force_detach_requested() {
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

async fn wait_for_control<F>(
    future: F,
    interrupts: &mut Interrupts,
    cancellation_acknowledged: bool,
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
                if interrupts.force_detach_requested() {
                    return ControlWait::UnconfirmedDetach;
                }
                if cancellation_acknowledged && interrupts.detach_requested() {
                    return ControlWait::ConfirmedDetach;
                }
            }
            result = &mut future => return ControlWait::Ready(result),
        }
    }
}

enum StdoutWait<T> {
    Ready(T),
    Interrupted,
    ConfirmedDetach,
    UnconfirmedDetach,
}

async fn wait_for_stdout<F>(
    future: F,
    interrupts: &mut Interrupts,
    cancellation_acknowledged: bool,
) -> StdoutWait<F::Output>
where
    F: Future,
{
    tokio::pin!(future);
    tokio::select! {
        biased;
        interrupt = interrupts.next() => {
            interrupts.note(interrupt);
            if interrupts.force_detach_requested() {
                StdoutWait::UnconfirmedDetach
            } else if cancellation_acknowledged && interrupts.detach_requested() {
                StdoutWait::ConfirmedDetach
            } else {
                StdoutWait::Interrupted
            }
        }
        result = &mut future => StdoutWait::Ready(result),
    }
}

#[derive(Clone)]
struct PipeWriter {
    sender: mpsc::Sender<PipeWrite>,
}

struct PipeWrite {
    text: String,
    completion: oneshot::Sender<io::Result<()>>,
}

impl PipeWriter {
    fn stdout() -> io::Result<Self> {
        Self::spawn(io::stdout(), 1, "acadctl-stdout")
    }

    fn stderr() -> io::Result<Self> {
        Self::spawn(io::stderr(), 8, "acadctl-stderr")
    }

    fn spawn<W>(mut writer: W, capacity: usize, name: &str) -> io::Result<Self>
    where
        W: Write + Send + 'static,
    {
        let (sender, mut receiver) = mpsc::channel::<PipeWrite>(capacity);
        thread::Builder::new().name(name.into()).spawn(move || {
            while let Some(write) = receiver.blocking_recv() {
                let result = writer
                    .write_all(write.text.as_bytes())
                    .and_then(|()| writer.flush());
                let failed = result.is_err();
                let _ = write.completion.send(result);
                if failed {
                    return;
                }
            }
        })?;
        Ok(Self { sender })
    }

    async fn write(&self, text: String) -> io::Result<()> {
        let (completion, result) = oneshot::channel();
        self.sender
            .send(PipeWrite { text, completion })
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "pipe writer stopped"))?;
        result.await.map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "pipe writer stopped without reporting a result",
            )
        })?
    }

    fn try_write(&self, text: String) {
        let (completion, _result) = oneshot::channel();
        let _ = self.sender.try_send(PipeWrite { text, completion });
    }
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    use acadctl_rpc::{DrawingOutcome, SourceLocation};

    use super::*;

    struct BlockingWriter {
        started: Arc<AtomicBool>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Write for BlockingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.started.store(true, Ordering::Release);
            let (lock, condition) = &*self.release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = condition.wait(released).unwrap();
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    async fn wait_until_started(started: &AtomicBool) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !started.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    fn release_writers(release: &(Mutex<bool>, Condvar)) {
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
    }

    #[test]
    fn formats_runtime_and_reader_failures_with_honest_locations() {
        let runtime = ExecutionFailure {
            message: "bad argument type: numberp nil".into(),
            form_index: Some(3),
            location: Some(SourceLocation {
                source_name: "script.lsp".into(),
                line: 12,
                column: 1,
            }),
            drawing_outcome: DrawingOutcome::RolledBack as i32,
        };
        assert_eq!(
            failure_lines(&runtime, "<stdin>"),
            [
                "Execution error in script.lsp, form 3 (line 12).",
                "bad argument type: numberp nil",
            ]
        );

        let reader = ExecutionFailure {
            message: "unterminated string".into(),
            form_index: None,
            location: Some(SourceLocation {
                source_name: "script.lsp".into(),
                line: 12,
                column: 17,
            }),
            drawing_outcome: DrawingOutcome::NotStarted as i32,
        };
        assert_eq!(
            failure_lines(&reader, "<stdin>"),
            [
                "Read error in script.lsp (line 12, column 17).",
                "unterminated string",
            ]
        );
    }

    #[test]
    fn keeps_location_free_failures_concise() {
        let failure = ExecutionFailure {
            message: "The document is busy".into(),
            form_index: None,
            location: None,
            drawing_outcome: DrawingOutcome::NotStarted as i32,
        };

        assert_eq!(
            failure_lines(&failure, "script.lsp"),
            ["acadctl: The document is busy"]
        );
    }

    #[test]
    fn treats_transport_loss_after_request_exposure_as_an_unknown_handoff() {
        let message = response_start_error(tonic::Status::unavailable("connection reset"));
        assert!(message.contains("may still have been accepted"));
        assert!(message.contains("do not retry"));

        assert_eq!(
            response_start_error(tonic::Status::unimplemented("missing service")),
            "The acadctl plugin is outdated. Install the current version and restart AutoCAD."
        );
    }

    #[tokio::test]
    async fn safe_detach_waits_for_cancellation_acknowledgement() {
        let (mut interrupts, sender) = Interrupts::test_pair();
        sender
            .send(Interrupt::Cancel { queued: true })
            .await
            .unwrap();
        sender.send(Interrupt::Detach).await.unwrap();

        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                wait_for_control(pending::<()>(), &mut interrupts, false),
            )
            .await
            .is_err()
        );
        assert!(interrupts.cancellation_requested());
        assert!(interrupts.detach_requested());
    }

    #[tokio::test]
    async fn acknowledged_or_forced_detach_stops_waiting() {
        let (mut acknowledged, acknowledged_sender) = Interrupts::test_pair();
        acknowledged_sender
            .send(Interrupt::Cancel { queued: true })
            .await
            .unwrap();
        acknowledged_sender.send(Interrupt::Detach).await.unwrap();
        assert!(matches!(
            wait_for_control(pending::<()>(), &mut acknowledged, true).await,
            ControlWait::ConfirmedDetach
        ));

        let (mut forced, forced_sender) = Interrupts::test_pair();
        forced_sender
            .send(Interrupt::Cancel { queued: true })
            .await
            .unwrap();
        forced_sender.send(Interrupt::Detach).await.unwrap();
        forced_sender.send(Interrupt::ForceDetach).await.unwrap();
        assert!(matches!(
            wait_for_control(pending::<()>(), &mut forced, false).await,
            ControlWait::UnconfirmedDetach
        ));
    }

    #[tokio::test]
    async fn pre_accept_timeout_stops_after_cancellation() {
        let (mut ordinary, _ordinary_sender) = Interrupts::test_pair();
        assert!(matches!(
            wait_for_response_start_with_timeout(
                pending::<()>(),
                &mut ordinary,
                Duration::from_millis(1),
            )
            .await,
            ResponseStartWait::TimedOut
        ));

        let (mut cancelled, cancelled_sender) = Interrupts::test_pair();
        cancelled_sender
            .send(Interrupt::Cancel { queued: true })
            .await
            .unwrap();
        assert!(matches!(
            wait_for_response_start_with_timeout(
                async {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    7
                },
                &mut cancelled,
                Duration::from_millis(1),
            )
            .await,
            ResponseStartWait::Ready(7)
        ));
    }

    #[tokio::test]
    async fn closed_outbound_cancel_still_interrupts_a_blocked_diagnostic() {
        let stderr_started = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let diagnostics = PipeWriter::spawn(
            BlockingWriter {
                started: Arc::clone(&stderr_started),
                release: Arc::clone(&release),
            },
            8,
            "acadctl-test-closed-outbound-stderr",
        )
        .unwrap();
        let (event_sender, receiver) = mpsc::channel(3);
        let mut interrupts = Interrupts {
            receiver: Some(receiver),
            task: tokio::spawn(pending::<()>()),
            diagnostics,
            cancellation_requested: false,
            cancel_queued: false,
            detach_requested: false,
            force_detach_requested: false,
        };
        let diagnostic = tokio::spawn(async move {
            write_diagnostic(&mut interrupts, "blocked diagnostic".into()).await
        });
        wait_until_started(&stderr_started).await;

        let (outbound_sender, outbound_receiver) = mpsc::channel(2);
        drop(outbound_receiver);
        assert!(publish_cancel(&outbound_sender, &event_sender).await);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), diagnostic)
                .await
                .unwrap()
                .unwrap(),
            DiagnosticWait::Interrupted
        ));

        release_writers(&release);
    }

    #[tokio::test]
    async fn cancelling_stdout_wait_does_not_leave_tokio_blocking_work() {
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let writer = Arc::new(
            PipeWriter::spawn(
                BlockingWriter {
                    started: Arc::clone(&started),
                    release: Arc::clone(&release),
                },
                1,
                "acadctl-test-stdout",
            )
            .unwrap(),
        );
        let write = tokio::spawn({
            let writer = Arc::clone(&writer);
            async move { writer.write("blocked".into()).await }
        });
        wait_until_started(&started).await;

        write.abort();
        write.await.unwrap_err();
        drop(writer);
        release_writers(&release);
    }

    #[tokio::test]
    async fn blocked_stdout_and_stderr_do_not_block_forced_detach() {
        let stdout_started = Arc::new(AtomicBool::new(false));
        let stderr_started = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let stdout = Arc::new(
            PipeWriter::spawn(
                BlockingWriter {
                    started: Arc::clone(&stdout_started),
                    release: Arc::clone(&release),
                },
                1,
                "acadctl-test-combined-stdout",
            )
            .unwrap(),
        );
        let diagnostics = PipeWriter::spawn(
            BlockingWriter {
                started: Arc::clone(&stderr_started),
                release: Arc::clone(&release),
            },
            8,
            "acadctl-test-combined-stderr",
        )
        .unwrap();
        let stdout_write = tokio::spawn({
            let stdout = Arc::clone(&stdout);
            async move { stdout.write("blocked stdout".into()).await }
        });
        diagnostics.try_write("blocked stderr".into());
        wait_until_started(&stdout_started).await;
        wait_until_started(&stderr_started).await;

        let (sender, receiver) = mpsc::channel(3);
        let task = tokio::spawn(pending::<()>());
        let mut interrupts = Interrupts {
            receiver: Some(receiver),
            task,
            diagnostics,
            cancellation_requested: false,
            cancel_queued: false,
            detach_requested: false,
            force_detach_requested: false,
        };
        sender
            .send(Interrupt::Cancel { queued: true })
            .await
            .unwrap();
        sender.send(Interrupt::Detach).await.unwrap();
        sender.send(Interrupt::ForceDetach).await.unwrap();

        assert!(matches!(
            tokio::time::timeout(
                Duration::from_secs(1),
                wait_for_control(pending::<()>(), &mut interrupts, false),
            )
            .await
            .unwrap(),
            ControlWait::UnconfirmedDetach
        ));

        stdout_write.abort();
        stdout_write.await.unwrap_err();
        drop(stdout);
        drop(interrupts);
        release_writers(&release);
    }
}
