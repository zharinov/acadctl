use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use acadctl_rpc::{
    DrawingId, DrawingOutcome as RpcDrawingOutcome, ExecAccepted, ExecCancelAcknowledgement,
    ExecCancelDisposition, ExecCancelled, ExecClientMessage, ExecFailure as RpcExecFailure,
    ExecFinished, ExecMode as RpcExecMode, ExecOutcome as RpcExecOutcome, ExecOutput, ExecRequest,
    ExecServerEvent, ExecService, ExecSuccess, SourceLocation as RpcSourceLocation, SourceName,
    SourceNameError, exec_client_message, exec_outcome, exec_server_event,
};
use futures_util::{Stream, stream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle as TokioJoinHandle;
use tonic::{Request, Response, Status};

use crate::exec::{
    DrawingOutcome, Exec, ExecFailure, ExecMode, ExecOutcome, SourceValidationError,
    bounded_diagnostic,
};
use crate::scheduler::{CancelResult, Error as SchedulerError};

use super::status::scheduler_error;

const FIRST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct ExecRpc;

type ExecuteResponse =
    Pin<Box<dyn Stream<Item = Result<ExecServerEvent, Status>> + Send + 'static>>;

type CompletionFuture =
    Pin<Box<dyn Future<Output = Result<ExecOutcome, SchedulerError>> + Send + 'static>>;

struct ExecuteResponseState {
    output: crate::exec::output::OutputStream,
    completion: CompletionFuture,
    control: Option<mpsc::Receiver<Result<ExecCancelDisposition, Status>>>,
    control_task: TokioJoinHandle<()>,
    phase: ExecuteResponsePhase,
}

enum ExecuteResponsePhase {
    SendAccepted,
    RelayOutput,
    AwaitCompletion,
    Done,
}

#[tonic::async_trait]
impl ExecService for ExecRpc {
    type ExecuteStream = ExecuteResponse;

    async fn execute(
        &self,
        request: Request<tonic::Streaming<ExecClientMessage>>,
    ) -> Result<Response<Self::ExecuteStream>, Status> {
        let reservation = crate::scheduler::try_reserve_execution()
            .ok_or_else(|| Status::resource_exhausted("Too many live execution streams"))?;

        let mut inbound = request.into_inner();
        let first = tokio::time::timeout(FIRST_MESSAGE_TIMEOUT, inbound.message())
            .await
            .map_err(|_| Status::deadline_exceeded("The execution request was not received"))??
            .ok_or_else(|| Status::invalid_argument("The first execution message is required"))?;
        let Some(exec_client_message::Message::Request(request)) = first.message else {
            return Err(Status::invalid_argument(
                "The first execution message must be a request",
            ));
        };

        let request = match ValidatedExecRequest::try_from(request) {
            Ok(request) => request,
            Err(failure) => return Ok(Response::new(terminal_response(reservation, failure))),
        };

        let ValidatedExecRequest {
            drawing_id,
            mode,
            source_name,
            source,
        } = request;

        let validation_source_name = source_name.clone();
        let validated = tokio::task::spawn_blocking(move || {
            (reservation, Exec::new(mode, source_name, source))
        })
        .await
        .map_err(|error| Status::internal(format!("Source validation failed: {error}")))?;
        let (reservation, execution) = validated;
        let (execution, output) = match execution {
            Ok(execution) => execution,
            Err(error) => {
                return Ok(Response::new(terminal_response(
                    reservation,
                    validation_failure(error, validation_source_name),
                )));
            }
        };

        let admission = match crate::scheduler::admit_execution(
            drawing_id,
            execution,
            output,
            reservation.clone(),
        ) {
            Ok(admission) => admission,
            Err(error) if error.is_internal() => return Err(scheduler_error(error)),
            Err(error) => {
                return Ok(Response::new(terminal_response(
                    reservation,
                    scheduler_failure(error),
                )));
            }
        };

        let (job_id, output, completion) = admission.into_parts();
        drop(reservation);

        let (control, control_task) = spawn_control_reader(inbound, job_id);
        let state = ExecuteResponseState {
            output,
            completion: Box::pin(completion.wait()),
            control: Some(control),
            control_task,
            phase: ExecuteResponsePhase::SendAccepted,
        };

        Ok(Response::new(execution_response(state)))
    }
}

fn execution_response(state: ExecuteResponseState) -> ExecuteResponse {
    Box::pin(stream::try_unfold(state, |mut state| async move {
        match state.next_event().await? {
            Some(event) => Ok(Some((event, state))),
            None => Ok(None),
        }
    }))
}

impl ExecuteResponseState {
    async fn next_event(&mut self) -> Result<Option<ExecServerEvent>, Status> {
        loop {
            match self.phase {
                ExecuteResponsePhase::SendAccepted => {
                    self.phase = ExecuteResponsePhase::RelayOutput;

                    return Ok(Some(server_event(exec_server_event::Event::Accepted(
                        ExecAccepted {},
                    ))));
                }
                ExecuteResponsePhase::RelayOutput => {
                    let Some(control) = self.control.as_mut() else {
                        match self.output.next_chunk().await {
                            Some(chunk) => {
                                return Ok(Some(server_event(exec_server_event::Event::Output(
                                    ExecOutput { chunk },
                                ))));
                            }
                            None => self.phase = ExecuteResponsePhase::AwaitCompletion,
                        }

                        continue;
                    };

                    tokio::select! {
                        biased;
                        control = control.recv() => {
                            if let Some(event) = self.handle_control(control)? {
                                return Ok(Some(event));
                            }
                        }
                        output = self.output.next_chunk() => {
                            match output {
                                Some(chunk) => return Ok(Some(server_event(
                                    exec_server_event::Event::Output(ExecOutput { chunk }),
                                ))),
                                None => self.phase = ExecuteResponsePhase::AwaitCompletion,
                            }
                        }
                    }
                }
                ExecuteResponsePhase::AwaitCompletion => {
                    let outcome = if let Some(control) = self.control.as_mut() {
                        tokio::select! {
                            biased;
                            control = control.recv() => {
                                if let Some(event) = self.handle_control(control)? {
                                    return Ok(Some(event));
                                }

                                continue;
                            }
                            outcome = self.completion.as_mut() => outcome,
                        }
                    } else {
                        self.completion.as_mut().await
                    };

                    self.control_task.abort();
                    self.phase = ExecuteResponsePhase::Done;

                    return Ok(Some(finished_event(match outcome {
                        Ok(outcome) => outcome,
                        Err(error) => ExecOutcome::Failure(scheduler_failure(error)),
                    })));
                }
                ExecuteResponsePhase::Done => return Ok(None),
            }
        }
    }

    fn handle_control(
        &mut self,
        control: Option<Result<ExecCancelDisposition, Status>>,
    ) -> Result<Option<ExecServerEvent>, Status> {
        let Some(control) = control else {
            self.control = None;

            return Ok(None);
        };

        match control {
            Ok(result) => Ok(Some(server_event(
                exec_server_event::Event::CancelAcknowledgement(ExecCancelAcknowledgement {
                    disposition: result as i32,
                }),
            ))),
            Err(status) => Err(status),
        }
    }
}

impl Drop for ExecuteResponseState {
    fn drop(&mut self) {
        self.control_task.abort();
    }
}

fn spawn_control_reader(
    mut inbound: tonic::Streaming<ExecClientMessage>,
    job_id: crate::scheduler::MutationJobId,
) -> (
    mpsc::Receiver<Result<ExecCancelDisposition, Status>>,
    TokioJoinHandle<()>,
) {
    let (sender, receiver) = mpsc::channel(1);
    let task = tokio::spawn(async move {
        let mut cancel_disposition = None;

        loop {
            let message = match inbound.message().await {
                Ok(Some(message)) => message,
                Ok(None) => return,
                Err(status) => {
                    let _ = sender.send(Err(status)).await;

                    return;
                }
            };

            if !matches!(
                message.message,
                Some(exec_client_message::Message::Cancel(_))
            ) {
                let _ = sender
                    .send(Err(Status::invalid_argument(
                        "Only Cancel is valid after the execution request",
                    )))
                    .await;

                return;
            }

            if cancel_disposition.is_some() {
                continue;
            }

            let result = match crate::scheduler::cancel_execution(job_id) {
                CancelResult::Accepted => ExecCancelDisposition::Accepted,
                CancelResult::TooLate | CancelResult::NotFound => ExecCancelDisposition::TooLate,
                CancelResult::Unavailable => {
                    let _ = sender
                        .send(Err(Status::internal(
                            "Execution cancellation state is unavailable",
                        )))
                        .await;

                    return;
                }
            };

            cancel_disposition = Some(result);

            if sender.send(Ok(result)).await.is_err() {
                return;
            }
        }
    });
    (receiver, task)
}

pub(super) fn terminal_response(
    reservation: crate::scheduler::ExecReservation,
    failure: ExecFailure,
) -> ExecuteResponse {
    let event = finished_event(ExecOutcome::Failure(failure));

    Box::pin(stream::once(async move {
        let _reservation = reservation;
        Ok(event)
    }))
}

struct ValidatedExecRequest {
    drawing_id: DrawingId,
    mode: ExecMode,
    source_name: SourceName,
    source: bytes::Bytes,
}

impl TryFrom<ExecRequest> for ValidatedExecRequest {
    type Error = ExecFailure;

    fn try_from(request: ExecRequest) -> Result<Self, Self::Error> {
        let drawing_id = DrawingId::try_from(request.drawing_id)
            .map_err(|_| failure("The drawing ID is invalid"))?;

        let source_name = SourceName::new(request.source_name).map_err(|error| match error {
            SourceNameError::Empty => failure("The source name is required"),
            SourceNameError::TooLong => failure("The source name exceeds the 4 KiB limit"),
        })?;

        let mode = match RpcExecMode::try_from(request.mode) {
            Ok(RpcExecMode::Eval) => ExecMode::Eval,
            Ok(RpcExecMode::Exec) => ExecMode::Exec,
            Ok(RpcExecMode::Unspecified) | Err(_) => {
                return Err(failure("The execution mode must be eval or exec"));
            }
        };

        Ok(Self {
            drawing_id,
            mode,
            source_name,
            source: request.source,
        })
    }
}

fn validation_failure(error: SourceValidationError, source_name: SourceName) -> ExecFailure {
    match error {
        SourceValidationError::SourceTooLarge => failure("The source exceeds the 4 MiB limit"),
        SourceValidationError::InvalidUtf8 => failure("The source is not valid UTF-8"),
        SourceValidationError::NullCharacter => {
            failure("The source contains U+0000, which AutoLISP cannot represent")
        }
        SourceValidationError::ExpectedOneForm { actual } => failure(format!(
            "eval requires exactly one top-level form; found {actual}"
        )),
        SourceValidationError::Scan(error) => ExecFailure {
            message: error.kind.message().to_owned(),
            form_index: None,
            location: Some(crate::exec::SourceLocation::from_scan_error(
                source_name,
                &error,
            )),
            drawing_outcome: DrawingOutcome::NotStarted,
            drawing_error: None,
        },
    }
}

fn failure(message: impl Into<String>) -> ExecFailure {
    ExecFailure::not_started(bounded_diagnostic(message.into()))
}

fn scheduler_failure(error: SchedulerError) -> ExecFailure {
    let drawing_outcome = error.drawing_outcome();
    let drawing_error = error.drawing_error_kind();

    ExecFailure {
        message: bounded_diagnostic(error.to_string()),
        form_index: None,
        location: None,
        drawing_outcome,
        drawing_error,
    }
}

fn finished_event(outcome: ExecOutcome) -> ExecServerEvent {
    let outcome = match outcome {
        ExecOutcome::Success => exec_outcome::Outcome::Success(ExecSuccess {}),
        ExecOutcome::Cancelled => exec_outcome::Outcome::Cancelled(ExecCancelled {}),
        ExecOutcome::Failure(failure) => exec_outcome::Outcome::Failure(rpc_failure(failure)),
    };

    server_event(exec_server_event::Event::Finished(ExecFinished {
        outcome: Some(RpcExecOutcome {
            outcome: Some(outcome),
        }),
    }))
}

fn rpc_failure(failure: ExecFailure) -> RpcExecFailure {
    let drawing_outcome = match failure.drawing_outcome {
        DrawingOutcome::NotStarted => RpcDrawingOutcome::NotStarted,
        DrawingOutcome::RolledBack => RpcDrawingOutcome::RolledBack,
        DrawingOutcome::Committed => RpcDrawingOutcome::Committed,
        DrawingOutcome::Unknown => RpcDrawingOutcome::Unknown,
    };

    RpcExecFailure {
        message: bounded_diagnostic(failure.message),
        form_index: failure.form_index.map(|index| index as u64),
        location: failure.location.map(|location| RpcSourceLocation {
            source_name: location.source_name.into_string(),
            line: location.line as u64,
            column: location.column as u64,
        }),
        drawing_outcome: drawing_outcome as i32,
        drawing_error: failure
            .drawing_error
            .unwrap_or(acadctl_rpc::DrawingErrorKind::Unspecified) as i32,
    }
}

fn server_event(event: exec_server_event::Event) -> ExecServerEvent {
    ExecServerEvent { event: Some(event) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_failures_keep_their_typed_drawing_error() {
        let failure = scheduler_failure(SchedulerError::DrawingNotFound(
            DrawingId::new(0x36C8).unwrap(),
        ));
        let failure = rpc_failure(failure);

        assert_eq!(
            failure.drawing_error,
            acadctl_rpc::DrawingErrorKind::NotOpen as i32
        );
        assert_eq!(
            failure.drawing_outcome,
            RpcDrawingOutcome::NotStarted as i32
        );
    }
}
