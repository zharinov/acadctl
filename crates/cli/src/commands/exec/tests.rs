use std::future::pending;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use tokio::sync::mpsc;

use acadctl_rpc::{DrawingOutcome, ExecCancelDisposition, SourceLocation};

use super::*;

fn target() -> Target {
    "6A84:36C8".parse().unwrap()
}

struct BlockingWriter {
    started: Arc<AtomicBool>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
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
    let runtime = ExecFailure {
        message: "bad argument type: numberp nil".into(),
        form_index: Some(3),
        location: Some(SourceLocation {
            source_name: "script.lsp".into(),
            line: 12,
            column: 1,
        }),
        drawing_outcome: DrawingOutcome::RolledBack as i32,
        drawing_error: acadctl_rpc::DrawingErrorKind::Unspecified as i32,
    };

    assert_eq!(
        failure_message(&runtime, "<stdin>", target()),
        "Code failed in script.lsp at form 3, line 12: bad argument type: numberp nil. Changes were rolled back. Other side effects may remain."
    );

    let reader = ExecFailure {
        message: "unterminated string".into(),
        form_index: None,
        location: Some(SourceLocation {
            source_name: "script.lsp".into(),
            line: 12,
            column: 17,
        }),
        drawing_outcome: DrawingOutcome::NotStarted as i32,
        drawing_error: acadctl_rpc::DrawingErrorKind::Unspecified as i32,
    };

    assert_eq!(
        failure_message(&reader, "<stdin>", target()),
        "Invalid code in script.lsp at line 12, column 17: unterminated string. Code was not run."
    );
}

#[test]
fn keeps_location_free_failures_concise() {
    let failure = ExecFailure {
        message: "The drawing is busy".into(),
        form_index: None,
        location: None,
        drawing_outcome: DrawingOutcome::NotStarted as i32,
        drawing_error: acadctl_rpc::DrawingErrorKind::Busy as i32,
    };

    assert_eq!(
        failure_message(&failure, "script.lsp", target()),
        "Drawing 6A84:36C8 is busy. Code was not run."
    );
}

#[test]
fn renders_readiness_timeout_as_one_line() {
    let failure = ExecFailure {
        message: "AutoCAD did not become ready within 60 seconds".into(),
        form_index: None,
        location: None,
        drawing_outcome: DrawingOutcome::NotStarted as i32,
        drawing_error: acadctl_rpc::DrawingErrorKind::Unspecified as i32,
    };

    assert_eq!(
        failure_message(&failure, "<command-line>", target()),
        "Timeout: code was not run."
    );
}

#[test]
fn reports_every_drawing_outcome() {
    assert_eq!(
        drawing_outcome_message(DrawingOutcome::NotStarted as i32),
        "Code was not run."
    );
    assert_eq!(
        drawing_outcome_message(DrawingOutcome::RolledBack as i32),
        "Changes were rolled back. Other side effects may remain."
    );
    assert_eq!(
        drawing_outcome_message(DrawingOutcome::Committed as i32),
        "Changes were committed before the failure."
    );
    assert_eq!(
        drawing_outcome_message(DrawingOutcome::Unknown as i32),
        "Outcome unknown: retrying may run code twice."
    );
}

#[tokio::test]
async fn ctrl_c_feedback_is_one_status_line_until_detachment() {
    let output = Arc::new(Mutex::new(Vec::new()));
    let diagnostics = PipeWriter::spawn(
        SharedWriter(Arc::clone(&output)),
        8,
        "acadctl-test-status-output",
    )
    .unwrap();
    let barrier = diagnostics.clone();
    let (mut interrupts, _) = Interrupts::test_with_diagnostics(diagnostics);

    interrupts.note(Interrupt::Cancel { queued: true });
    interrupts.finish_stopping("done.");
    barrier.write(String::new()).await.unwrap();

    assert_eq!(
        String::from_utf8(output.lock().unwrap().clone()).unwrap(),
        "Stopping... done.\n"
    );

    let output = Arc::new(Mutex::new(Vec::new()));
    let diagnostics = PipeWriter::spawn(
        SharedWriter(Arc::clone(&output)),
        8,
        "acadctl-test-detach-output",
    )
    .unwrap();
    let barrier = diagnostics.clone();
    let (mut interrupts, _) = Interrupts::test_with_diagnostics(diagnostics);

    interrupts.note(Interrupt::Cancel { queued: true });
    interrupts.note(Interrupt::Detach);
    assert_eq!(detach_exit(&interrupts), ExitCode::from(130));
    barrier.write(String::new()).await.unwrap();

    assert_eq!(
        String::from_utf8(output.lock().unwrap().clone()).unwrap(),
        "Stopping... \nDetached: code may still be running.\n"
    );

    let output = Arc::new(Mutex::new(Vec::new()));
    let diagnostics = PipeWriter::spawn(
        SharedWriter(Arc::clone(&output)),
        8,
        "acadctl-test-connection-lost-output",
    )
    .unwrap();
    let barrier = diagnostics.clone();
    let (mut interrupts, _) = Interrupts::test_with_diagnostics(diagnostics);

    interrupts.note(Interrupt::Cancel { queued: true });
    assert_eq!(stopping_connection_lost(&interrupts), ExitCode::from(130));
    barrier.write(String::new()).await.unwrap();

    assert_eq!(
        String::from_utf8(output.lock().unwrap().clone()).unwrap(),
        "Stopping... connection lost: code may still be running.\n"
    );
}

#[test]
fn treats_transport_loss_after_request_exposure_as_an_unknown_handoff() {
    let message = response_start_error(tonic::Status::unavailable("connection reset"));
    assert_eq!(message, "Outcome unknown: retrying may run code twice.");

    assert_eq!(
        response_start_error(tonic::Status::unimplemented("missing service")),
        "Plugin incompatible: code was not run."
    );
    assert_eq!(
        response_start_error(tonic::Status::unknown(
            "failed to decode Protobuf message: ExecRequest.drawing_id",
        )),
        "Plugin incompatible: code was not run."
    );
}

#[tokio::test]
async fn second_interrupt_detaches_without_waiting_for_an_acknowledgement() {
    let (mut interrupts, sender) = Interrupts::test_pair();
    sender
        .send(Interrupt::Cancel { queued: true })
        .await
        .unwrap();
    sender.send(Interrupt::Detach).await.unwrap();

    assert!(matches!(
        wait_for_control(pending::<()>(), &mut interrupts).await,
        ControlWait::UnconfirmedDetach
    ));
    assert!(interrupts.cancellation_requested());
    assert!(interrupts.detach_requested());
}

#[tokio::test]
async fn acknowledged_detach_stops_waiting() {
    let (mut acknowledged, acknowledged_sender) = Interrupts::test_pair();
    acknowledged_sender
        .send(Interrupt::Cancel { queued: true })
        .await
        .unwrap();

    let interrupt = acknowledged.next().await;
    acknowledged.note(interrupt);

    assert_eq!(
        acknowledged.record_cancellation_acknowledgement(ExecCancelDisposition::Accepted as i32),
        CancellationReceipt::Continue
    );

    acknowledged_sender.send(Interrupt::Detach).await.unwrap();

    assert!(matches!(
        wait_for_control(pending::<()>(), &mut acknowledged).await,
        ControlWait::UnconfirmedDetach
    ));
}

#[tokio::test]
async fn too_late_after_the_first_interrupt_remains_attached() {
    let (mut interrupts, sender) = Interrupts::test_pair();
    sender
        .send(Interrupt::Cancel { queued: true })
        .await
        .unwrap();

    let interrupt = interrupts.next().await;
    interrupts.note(interrupt);

    assert_eq!(
        interrupts.record_cancellation_acknowledgement(ExecCancelDisposition::TooLate as i32),
        CancellationReceipt::Continue
    );
    assert!(interrupts.cancellation_acknowledged());
    assert!(!interrupts.detach_requested());

    sender.send(Interrupt::Detach).await.unwrap();

    assert!(matches!(
        wait_for_control(pending::<()>(), &mut interrupts).await,
        ControlWait::UnconfirmedDetach
    ));
}

#[tokio::test]
async fn acknowledgement_may_arrive_before_the_local_interrupt_event() {
    let (mut interrupts, sender) = Interrupts::test_pair();

    assert_eq!(
        interrupts.record_cancellation_acknowledgement(ExecCancelDisposition::Accepted as i32),
        CancellationReceipt::Continue
    );

    sender
        .send(Interrupt::Cancel { queued: true })
        .await
        .unwrap();

    let interrupt = interrupts.next().await;
    interrupts.note(interrupt);

    assert!(interrupts.cancellation_requested());
    assert!(interrupts.cancellation_acknowledged());
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

    let (mut interrupts, event_sender) = Interrupts::test_with_diagnostics(diagnostics);

    let diagnostic = tokio::spawn(async move {
        interrupts
            .write_diagnostic("blocked diagnostic".into())
            .await
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
async fn blocked_stdout_and_stderr_do_not_block_detachment() {
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

    let (mut interrupts, sender) = Interrupts::test_with_diagnostics(diagnostics);

    sender
        .send(Interrupt::Cancel { queued: true })
        .await
        .unwrap();
    sender.send(Interrupt::Detach).await.unwrap();

    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_control(pending::<()>(), &mut interrupts),
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
