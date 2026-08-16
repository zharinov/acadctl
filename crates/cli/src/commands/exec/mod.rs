use std::process::ExitCode;
use std::time::Duration;

use acadctl_rpc::{
    DrawingOutcome, ExecClientMessage, ExecFailure, ExecMode, ExecRequest, ExecServerEvent,
    exec_client_message, exec_outcome, exec_server_event,
};
use futures_util::stream as futures_stream;
use tonic::Code;

use crate::instance::QueryError;

mod interrupt;
mod stream;
mod writer;

use interrupt::{CancellationReceipt, DiagnosticWait, Interrupts};
#[cfg(test)]
use interrupt::{Interrupt, publish_cancel};
#[cfg(test)]
use stream::wait_for_response_start_with_timeout;
use stream::{
    ControlWait, ResponseStartWait, StdoutWait, wait_for_control, wait_for_response_start,
    wait_for_stdout,
};
use writer::PipeWriter;

use super::{fail, incompatible_message, query_error_message, target::Target};

const EXECUTION_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const EXECUTION_RESPONSE_START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResponsePhase {
    AwaitingAcceptance,
    Accepted,
}

struct ResponseSession {
    response: Option<tonic::Streaming<ExecServerEvent>>,
    source_name: String,
    target: Target,
    phase: ResponsePhase,
    stdout: Option<std::io::Result<PipeWriter>>,
    interrupts: Interrupts,
}

pub async fn run(target: Target, source: crate::source::SourceSpec, mode: ExecMode) -> ExitCode {
    let source_mode = if mode == ExecMode::Eval {
        crate::source::SourceMode::Eval
    } else {
        crate::source::SourceMode::Exec
    };

    let source = match source.load(source_mode) {
        Ok(source) => source,
        Err(error) => {
            error.report();

            return ExitCode::FAILURE;
        }
    };

    let mut client = match tokio::time::timeout(
        EXECUTION_CONNECT_TIMEOUT,
        crate::instance::connect_execution(target.instance_id),
    )
    .await
    {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => return fail(query_error_message(target.instance_id, &error)),
        Err(_) => {
            return fail(query_error_message(
                target.instance_id,
                &QueryError::TimedOut,
            ));
        }
    };

    let (source_name, source_bytes) = source.into_parts();
    let diagnostic_source_name = source_name.to_string();
    let request = ExecClientMessage {
        message: Some(exec_client_message::Message::Request(ExecRequest::new(
            target.drawing_id,
            mode,
            source_name,
            source_bytes,
        ))),
    };

    let diagnostics = match PipeWriter::stderr() {
        Ok(diagnostics) => diagnostics,
        Err(_) => return fail("Could not start diagnostic output".into()),
    };

    let (mut interrupts, receiver) = match Interrupts::new(request, diagnostics) {
        Ok(interrupts) => interrupts,
        Err(_) => return fail("Could not listen for Ctrl+C".into()),
    };

    let outbound = futures_stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|message| (message, receiver))
    });

    let response = match wait_for_response_start(client.execute(outbound), &mut interrupts).await {
        ResponseStartWait::Ready(Ok(response)) => response,
        ResponseStartWait::Ready(Err(status)) => {
            if interrupts.detach_requested() {
                return detach_exit(&interrupts);
            }

            if interrupts.cancellation_requested() {
                return stopping_connection_lost(&interrupts);
            }

            return diagnostic_failure(&mut interrupts, response_start_error(status)).await;
        }
        ResponseStartWait::TimedOut => {
            return diagnostic_failure(
                &mut interrupts,
                "AutoCAD did not report whether it started the AutoLISP (running it again may execute it twice)"
                    .into(),
            )
            .await;
        }
        ResponseStartWait::UnconfirmedDetach => return detach_exit(&interrupts),
    };

    ResponseSession::new(
        response.into_inner(),
        diagnostic_source_name,
        target,
        interrupts,
    )
    .drive()
    .await
}

impl ResponseSession {
    fn new(
        response: tonic::Streaming<ExecServerEvent>,
        source_name: String,
        target: Target,
        interrupts: Interrupts,
    ) -> Self {
        Self {
            response: Some(response),
            source_name,
            target,
            phase: ResponsePhase::AwaitingAcceptance,
            stdout: None,
            interrupts,
        }
    }

    fn accepted(&self) -> bool {
        self.phase == ResponsePhase::Accepted
    }

    async fn drive(mut self) -> ExitCode {
        loop {
            let event = match wait_for_control(
                self.response
                    .as_mut()
                    .expect("response session is active")
                    .message(),
                &mut self.interrupts,
            )
            .await
            {
                ControlWait::Ready(Ok(Some(event))) => event,
                ControlWait::Ready(Ok(None)) => {
                    if self.interrupts.detach_requested() {
                        return detach_exit(&self.interrupts);
                    }

                    return self.lost_response().await;
                }
                ControlWait::Ready(Err(_status)) => {
                    if self.interrupts.detach_requested() {
                        return detach_exit(&self.interrupts);
                    }

                    return self.lost_response().await;
                }
                ControlWait::UnconfirmedDetach => {
                    return detach_exit(&self.interrupts);
                }
            };

            match event.event {
                Some(exec_server_event::Event::Accepted(_)) => {
                    if self.accepted() {
                        return self
                            .invalid_response("AutoCAD accepted the execution more than once")
                            .await;
                    }

                    self.phase = ResponsePhase::Accepted;
                }
                Some(exec_server_event::Event::Output(output)) => {
                    if !self.accepted() {
                        return self
                            .invalid_response("AutoCAD sent output before accepting the execution")
                            .await;
                    }

                    if self.interrupts.cancellation_requested() {
                        continue;
                    }

                    let writer = match self.stdout.get_or_insert_with(PipeWriter::stdout).as_ref() {
                        Ok(writer) => writer,
                        Err(_) => {
                            self.response.take();

                            return diagnostic_failure(
                                &mut self.interrupts,
                                "Could not write AutoLISP output (AutoLISP may still be running)"
                                    .into(),
                            )
                            .await;
                        }
                    };

                    match wait_for_stdout(writer.write(output.chunk), &mut self.interrupts).await {
                        StdoutWait::Ready(Ok(())) => {}
                        StdoutWait::Ready(Err(_)) => {
                            self.response.take();

                            return diagnostic_failure(
                                &mut self.interrupts,
                                "Could not write AutoLISP output (AutoLISP may still be running)"
                                    .into(),
                            )
                            .await;
                        }
                        StdoutWait::Interrupted => continue,
                        StdoutWait::UnconfirmedDetach => {
                            return detach_exit(&self.interrupts);
                        }
                    }
                }
                Some(exec_server_event::Event::CancelAcknowledgement(acknowledgement)) => {
                    if !self.accepted() {
                        return self
                            .invalid_response(
                                "AutoCAD acknowledged cancellation before accepting the execution",
                            )
                            .await;
                    }

                    match self
                        .interrupts
                        .record_cancellation_acknowledgement(acknowledgement.disposition)
                    {
                        CancellationReceipt::Continue => {}
                        CancellationReceipt::Detach => {
                            return detach_exit(&self.interrupts);
                        }
                        CancellationReceipt::Duplicate => {
                            return self
                                .invalid_response(
                                    "AutoCAD acknowledged the same cancellation more than once",
                                )
                                .await;
                        }
                        CancellationReceipt::Invalid => {
                            return self
                                .invalid_response(
                                    "AutoCAD returned an invalid cancellation acknowledgement",
                                )
                                .await;
                        }
                    }
                }
                Some(exec_server_event::Event::Finished(finished)) => {
                    let Some(outcome) = finished.outcome.and_then(|outcome| outcome.outcome) else {
                        return self
                            .invalid_response("AutoCAD returned an empty execution result")
                            .await;
                    };

                    if !self.accepted()
                        && matches!(
                            &outcome,
                            exec_outcome::Outcome::Success(_) | exec_outcome::Outcome::Cancelled(_)
                        )
                    {
                        return self
                            .invalid_response(
                                "AutoCAD finished an execution that it did not accept",
                            )
                            .await;
                    }

                    return match outcome {
                        exec_outcome::Outcome::Success(_)
                            if self.interrupts.cancellation_requested() =>
                        {
                            stopped_exit(&self.interrupts)
                        }
                        exec_outcome::Outcome::Success(_) => ExitCode::SUCCESS,
                        exec_outcome::Outcome::Cancelled(_) => stopped_exit(&self.interrupts),
                        exec_outcome::Outcome::Failure(failure) => {
                            self.report_failure(failure).await
                        }
                    };
                }
                None => {
                    return self
                        .invalid_response("AutoCAD returned an empty execution event")
                        .await;
                }
            }
        }
    }

    async fn lost_response(&mut self) -> ExitCode {
        if self.interrupts.cancellation_requested() {
            return stopping_connection_lost(&self.interrupts);
        }

        diagnostic_failure(
            &mut self.interrupts,
            "Connection lost (AutoLISP may still be running)".into(),
        )
        .await
    }

    async fn invalid_response(&mut self, _detail: &str) -> ExitCode {
        diagnostic_failure(
            &mut self.interrupts,
            "Invalid response from AutoCAD (execution outcome unknown)".into(),
        )
        .await
    }

    async fn report_failure(&mut self, failure: ExecFailure) -> ExitCode {
        self.interrupts.finish_stopping("done.");
        let mut text = failure_lines(&failure, &self.source_name, self.target).join("\n");
        text.push('\n');

        match self.interrupts.write_diagnostic(text).await {
            DiagnosticWait::Complete => ExitCode::FAILURE,
            DiagnosticWait::Interrupted => cancelled_exit(),
        }
    }
}

fn failure_lines(failure: &ExecFailure, fallback_source_name: &str, target: Target) -> Vec<String> {
    let message = if failure.message.is_empty() {
        format!("Could not execute AutoLISP in drawing {target}")
    } else if failure.location.is_some() || failure.form_index.is_some() {
        failure.message.clone()
    } else {
        location_free_failure_message(failure, target)
    };

    let mut lines = match (&failure.location, failure.form_index) {
        (Some(location), Some(form_index)) => {
            let source_name = if location.source_name.is_empty() {
                fallback_source_name
            } else {
                &location.source_name
            };

            vec![
                format!(
                    "AutoLISP failed in {source_name} at form {form_index}, line {}",
                    location.line
                ),
                message,
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
                    "Could not read AutoLISP in {source_name} at line {}, column {}",
                    location.line, location.column
                ),
                message,
            ]
        }
        (None, Some(form_index)) => vec![
            format!("AutoLISP failed in {fallback_source_name} at form {form_index}"),
            message,
        ],
        (None, None) => vec![message],
    };

    lines.push(drawing_outcome_message(failure.drawing_outcome).into());
    lines
}

fn location_free_failure_message(failure: &ExecFailure, target: Target) -> String {
    if let Ok(kind) = acadctl_rpc::DrawingErrorKind::try_from(failure.drawing_error)
        && kind != acadctl_rpc::DrawingErrorKind::Unspecified
    {
        super::drawing_error_message(kind, target)
    } else if failure.message.starts_with("The source ") || failure.message.starts_with("eval ") {
        failure.message.trim_end_matches('.').to_owned()
    } else {
        format!("Could not execute AutoLISP in drawing {target}")
    }
}

fn drawing_outcome_message(outcome: i32) -> &'static str {
    match DrawingOutcome::try_from(outcome) {
        Ok(DrawingOutcome::NotStarted) => "AutoLISP was not run",
        Ok(DrawingOutcome::RolledBack) => {
            "Drawing changes were rolled back (other side effects may remain)"
        }
        Ok(DrawingOutcome::Committed) => "Drawing changes were committed before the failure",
        Ok(DrawingOutcome::Unknown | DrawingOutcome::Unspecified) | Err(_) => {
            "Drawing outcome is unknown (running it again may repeat the operation)"
        }
    }
}

fn response_start_error(status: tonic::Status) -> String {
    if status.code() == Code::Unimplemented || incompatible_message(status.message()) {
        return "CLI and AutoCAD plugin are incompatible (AutoLISP was not run)".into();
    }

    "AutoCAD did not report whether it started the AutoLISP (running it again may execute it twice)"
        .into()
}

fn cancelled_exit() -> ExitCode {
    ExitCode::from(130)
}

fn stopped_exit(interrupts: &Interrupts) -> ExitCode {
    interrupts.finish_stopping("done.");
    cancelled_exit()
}

fn detach_exit(interrupts: &Interrupts) -> ExitCode {
    interrupts.diagnostic("\nDetached (AutoLISP may still be running)\n");
    cancelled_exit()
}

fn stopping_connection_lost(interrupts: &Interrupts) -> ExitCode {
    interrupts.finish_stopping("connection lost (AutoLISP may still be running)");
    cancelled_exit()
}

async fn diagnostic_failure(interrupts: &mut Interrupts, message: String) -> ExitCode {
    match interrupts.write_diagnostic(format!("{message}\n")).await {
        DiagnosticWait::Complete => ExitCode::FAILURE,
        DiagnosticWait::Interrupted => cancelled_exit(),
    }
}

#[cfg(test)]
mod tests;
