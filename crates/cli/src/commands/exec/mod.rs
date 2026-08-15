use std::process::ExitCode;
use std::time::Duration;

use acadctl_rpc::{
    ExecCancelDisposition, ExecClientMessage, ExecFailure, ExecMode, ExecRequest, ExecServerEvent,
    exec_client_message, exec_outcome, exec_server_event,
};
use futures_util::stream as futures_stream;
use tonic::Code;

use crate::instance::QueryError;

mod interrupt;
mod stream;
mod writer;

use interrupt::{AcknowledgementResult, CancellationDisposition, Interrupts};
#[cfg(test)]
use interrupt::{Interrupt, InterruptPhase, publish_cancel};
#[cfg(test)]
use stream::wait_for_response_start_with_timeout;
use stream::{
    ControlWait, ResponseStartWait, StdoutWait, wait_for_control, wait_for_response_start,
    wait_for_stdout,
};
use writer::PipeWriter;

use super::{fail, query_error_message, request_error_message, target::Target};

const EXECUTION_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const EXECUTION_RESPONSE_START_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run(target: Target, source: crate::source::SourceSpec, mode: ExecMode) -> ExitCode {
    let source = match crate::source::read(source, mode == ExecMode::Eval) {
        Ok(source) => source,
        Err(error) => {
            error.report();

            return ExitCode::FAILURE;
        }
    };

    let mut client = match tokio::time::timeout(
        EXECUTION_CONNECT_TIMEOUT,
        crate::instance::connect_execution(target.process_id),
    )
    .await
    {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => return fail(query_error_message(&error)),
        Err(_) => return fail(query_error_message(&QueryError::TimedOut)),
    };

    let source_name = source.name.to_string();
    let request = ExecClientMessage {
        message: Some(exec_client_message::Message::Request(ExecRequest::new(
            target.document_id,
            mode,
            source.name,
            source.bytes,
        ))),
    };

    let diagnostics = match PipeWriter::stderr() {
        Ok(diagnostics) => diagnostics,
        Err(error) => return fail(format!("Could not start the stderr writer: {error}")),
    };

    let (mut interrupts, receiver) = match Interrupts::new(request, diagnostics) {
        Ok(interrupts) => interrupts,
        Err(error) => return fail(format!("Could not install the Ctrl+C handler: {error}")),
    };

    let outbound = futures_stream::unfold(receiver, |mut receiver| async move {
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
    mut response: tonic::Streaming<ExecServerEvent>,
    source_name: &str,
    interrupts: &mut Interrupts,
) -> ExitCode {
    let mut accepted = false;
    let mut stdout = None;

    loop {
        let event = match wait_for_control(response.message(), interrupts).await {
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
            Some(exec_server_event::Event::Accepted(_)) if !accepted => accepted = true,
            Some(exec_server_event::Event::Accepted(_)) => {
                return invalid_response(
                    interrupts,
                    "AutoCAD accepted the execution more than once",
                )
                .await;
            }
            Some(exec_server_event::Event::Output(output)) if accepted => {
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
                                "Could not start the stdout writer: {error}. The accepted execution request may still be running; do not retry it blindly."
                            ),
                        )
                        .await;
                    }
                };

                match wait_for_stdout(writer.write(output.chunk), interrupts).await {
                    StdoutWait::Ready(Ok(())) => {}
                    StdoutWait::Ready(Err(error)) => {
                        drop(response);

                        return diagnostic_failure(
                            interrupts,
                            format!(
                                "Could not write stdout: {error}. The accepted execution request may still be running; do not retry it blindly."
                            ),
                        )
                        .await;
                    }
                    StdoutWait::Interrupted => continue,
                    StdoutWait::ConfirmedDetach => return confirmed_detach_exit(interrupts),
                    StdoutWait::UnconfirmedDetach => return unconfirmed_detach_exit(interrupts),
                }
            }
            Some(exec_server_event::Event::Output(_)) => {
                return invalid_response(
                    interrupts,
                    "AutoCAD sent output before accepting the execution",
                )
                .await;
            }

            Some(exec_server_event::Event::CancelAcknowledgement(acknowledgement)) if accepted => {
                match record_cancellation_acknowledgement(interrupts, acknowledgement.disposition) {
                    CancellationReceipt::Continue => {}
                    CancellationReceipt::Detach => return confirmed_detach_exit(interrupts),
                    CancellationReceipt::Duplicate => {
                        return invalid_response(
                            interrupts,
                            "AutoCAD acknowledged the same cancellation more than once",
                        )
                        .await;
                    }
                    CancellationReceipt::Invalid => {
                        return invalid_response(
                            interrupts,
                            "AutoCAD returned an invalid cancellation acknowledgement",
                        )
                        .await;
                    }
                }
            }
            Some(exec_server_event::Event::CancelAcknowledgement(_)) => {
                return invalid_response(
                    interrupts,
                    "AutoCAD acknowledged cancellation before accepting the execution",
                )
                .await;
            }
            Some(exec_server_event::Event::Finished(finished)) => {
                let Some(outcome) = finished.outcome.and_then(|outcome| outcome.outcome) else {
                    return invalid_response(
                        interrupts,
                        "AutoCAD returned an empty execution result",
                    )
                    .await;
                };

                return match outcome {
                    exec_outcome::Outcome::Success(_)
                        if accepted && interrupts.cancellation_requested() =>
                    {
                        cancelled_exit()
                    }
                    exec_outcome::Outcome::Success(_) if accepted => ExitCode::SUCCESS,
                    exec_outcome::Outcome::Cancelled(_) if accepted => cancelled_exit(),
                    exec_outcome::Outcome::Failure(failure) => {
                        report_failure(interrupts, failure, source_name).await
                    }

                    exec_outcome::Outcome::Success(_) | exec_outcome::Outcome::Cancelled(_) => {
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
    failure: ExecFailure,
    fallback_source_name: &str,
) -> ExitCode {
    let mut text = failure_lines(&failure, fallback_source_name).join("\n");
    text.push('\n');

    match write_diagnostic(interrupts, text).await {
        DiagnosticWait::Complete => ExitCode::FAILURE,
        DiagnosticWait::Interrupted => cancelled_exit(),
    }
}

fn failure_lines(failure: &ExecFailure, fallback_source_name: &str) -> Vec<String> {
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
            "{detail} before a result was returned. The accepted execution request may still be running; do not retry it blindly."
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

#[derive(Debug, PartialEq, Eq)]
enum CancellationReceipt {
    Continue,
    Detach,
    Duplicate,
    Invalid,
}

fn record_cancellation_acknowledgement(
    interrupts: &mut Interrupts,
    disposition: i32,
) -> CancellationReceipt {
    let disposition = match ExecCancelDisposition::try_from(disposition) {
        Ok(ExecCancelDisposition::Accepted) => CancellationDisposition::Accepted,
        Ok(ExecCancelDisposition::TooLate) => {
            interrupts.notice(
                "acadctl: cancellation was too late; execution will continue. Press Ctrl+C again to detach.",
            );
            CancellationDisposition::TooLate
        }
        Ok(ExecCancelDisposition::Unspecified) | Err(_) => {
            return CancellationReceipt::Invalid;
        }
    };

    match interrupts.acknowledge_cancellation(disposition) {
        AcknowledgementResult::Recorded => {}
        AcknowledgementResult::RecordedBeforeInterrupt => {
            if disposition == CancellationDisposition::Accepted {
                interrupts.notice("acadctl: cancellation requested; press Ctrl+C again to detach.");
            }
        }
        AcknowledgementResult::Duplicate => return CancellationReceipt::Duplicate,
        AcknowledgementResult::Invalid => return CancellationReceipt::Invalid,
    }

    if interrupts.detach_requested() {
        CancellationReceipt::Detach
    } else {
        CancellationReceipt::Continue
    }
}

fn confirmed_detach_exit(interrupts: &Interrupts) -> ExitCode {
    interrupts.notice("acadctl: detached; AutoCAD acknowledged the cancellation request.");
    cancelled_exit()
}

fn unconfirmed_detach_exit(interrupts: &Interrupts) -> ExitCode {
    interrupts.notice(
        "acadctl: detached without cancellation confirmation; the accepted execution request may still be running.",
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

#[cfg(test)]
mod tests;
