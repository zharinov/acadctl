use std::future::Future;
use std::pin::Pin;
use std::sync::{Mutex, mpsc as std_mpsc};
use std::thread::{self, JoinHandle as ThreadJoinHandle};
use std::time::Duration;

use acadctl_rpc::{
    CloseRequest, CloseResponse, DocumentService, DocumentServiceServer,
    DrawingOutcome as RpcDrawingOutcome, ExecutionAccepted, ExecutionCancelAcknowledgement,
    ExecutionCancelDisposition, ExecutionCancelled, ExecutionClientMessage,
    ExecutionFailure as RpcExecutionFailure, ExecutionFinished, ExecutionMode as RpcExecutionMode,
    ExecutionOutcome as RpcExecutionOutcome, ExecutionOutput, ExecutionRequest,
    ExecutionServerEvent, ExecutionService, ExecutionServiceServer, ExecutionSuccess,
    HistoryRequest, HistoryResponse, ListRequest, ListResponse, OpenRequest, OpenResponse,
    SaveRequest, SaveResponse, SourceLocation as RpcSourceLocation, execution_client_message,
    execution_outcome, execution_server_event,
};
use futures_util::{Stream, stream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle as TokioJoinHandle;
use tonic::{Request, Response, Status};

use crate::execution::{
    DrawingOutcome, Execution, ExecutionFailure, ExecutionMode, ExecutionOutcome,
    SourceValidationError, bounded_diagnostic,
};
use crate::scheduler::{CancelResult, Error as SchedulerError};

const FIRST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(5);
const RESTART_BACKOFF: Duration = Duration::from_millis(100);

static SERVER: Mutex<Option<Server>> = Mutex::new(None);

struct Server {
    stop: oneshot::Sender<()>,
    thread: ThreadJoinHandle<()>,
}

impl Server {
    fn is_running(&self) -> bool {
        !self.thread.is_finished()
    }

    fn shutdown(self) {
        let _ = self.stop.send(());
        let _ = self.thread.join();
    }
}

struct DocumentRpc;

struct ExecutionRpc;

type ExecuteResponse =
    Pin<Box<dyn Stream<Item = Result<ExecutionServerEvent, Status>> + Send + 'static>>;

type CompletionFuture =
    Pin<Box<dyn Future<Output = Result<ExecutionOutcome, SchedulerError>> + Send + 'static>>;

struct ExecuteResponseState {
    output: crate::execution::output::OutputStream,
    output_done: bool,
    completion: CompletionFuture,
    control: mpsc::Receiver<Result<ExecutionCancelDisposition, Status>>,
    control_open: bool,
    control_task: TokioJoinHandle<()>,
    accepted_sent: bool,
    finished: bool,
    _reservation: crate::scheduler::ExecutionReservation,
}

#[tonic::async_trait]
impl DocumentService for DocumentRpc {
    async fn list(&self, _request: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let documents = crate::scheduler::list().map_err(scheduler_error)?;
        Ok(Response::new(ListResponse { documents }))
    }

    async fn open(&self, request: Request<OpenRequest>) -> Result<Response<OpenResponse>, Status> {
        let path = request.into_inner().path;
        validate_open_path(&path)?;
        let document = crate::scheduler::open(path)
            .await
            .map_err(scheduler_error)?;
        Ok(Response::new(OpenResponse {
            document: Some(document),
        }))
    }

    async fn save(&self, request: Request<SaveRequest>) -> Result<Response<SaveResponse>, Status> {
        let id = request.into_inner().id;
        validate_document_id(&id)?;
        let document = crate::scheduler::save(id).await.map_err(scheduler_error)?;
        Ok(Response::new(SaveResponse {
            document: Some(document),
        }))
    }

    async fn undo(
        &self,
        request: Request<HistoryRequest>,
    ) -> Result<Response<HistoryResponse>, Status> {
        let id = request.into_inner().id;
        validate_document_id(&id)?;
        let document = crate::scheduler::undo(id).await.map_err(scheduler_error)?;
        Ok(Response::new(HistoryResponse {
            document: Some(document),
        }))
    }

    async fn redo(
        &self,
        request: Request<HistoryRequest>,
    ) -> Result<Response<HistoryResponse>, Status> {
        let id = request.into_inner().id;
        validate_document_id(&id)?;
        let document = crate::scheduler::redo(id).await.map_err(scheduler_error)?;
        Ok(Response::new(HistoryResponse {
            document: Some(document),
        }))
    }

    async fn close(
        &self,
        request: Request<CloseRequest>,
    ) -> Result<Response<CloseResponse>, Status> {
        let request = request.into_inner();
        validate_document_id(&request.id)?;
        crate::scheduler::close(request.id, request.discard)
            .await
            .map_err(scheduler_error)?;
        Ok(Response::new(CloseResponse {}))
    }
}

#[tonic::async_trait]
impl ExecutionService for ExecutionRpc {
    type ExecuteStream = ExecuteResponse;

    async fn execute(
        &self,
        request: Request<tonic::Streaming<ExecutionClientMessage>>,
    ) -> Result<Response<Self::ExecuteStream>, Status> {
        let reservation = crate::scheduler::try_reserve_execution()
            .ok_or_else(|| Status::resource_exhausted("Too many live execution streams"))?;
        let mut inbound = request.into_inner();
        let first = tokio::time::timeout(FIRST_MESSAGE_TIMEOUT, inbound.message())
            .await
            .map_err(|_| Status::deadline_exceeded("The execution request was not received"))??
            .ok_or_else(|| Status::invalid_argument("The first execution message is required"))?;
        let request = match first.message {
            Some(execution_client_message::Message::Request(request)) => request,
            Some(execution_client_message::Message::Cancel(_)) | None => {
                return Err(Status::invalid_argument(
                    "The first execution message must be a request",
                ));
            }
        };

        let request = match validate_execution_request(request) {
            Ok(request) => request,
            Err(failure) => return Ok(Response::new(terminal_response(reservation, failure))),
        };
        let ExecutionRequest {
            document_id,
            mode,
            source_name,
            source,
        } = request;
        let mode = match RpcExecutionMode::try_from(mode) {
            Ok(RpcExecutionMode::Eval) => ExecutionMode::Eval,
            Ok(RpcExecutionMode::Exec) => ExecutionMode::Exec,
            Ok(RpcExecutionMode::Unspecified) | Err(_) => {
                return Ok(Response::new(terminal_response(
                    reservation,
                    failure("The execution mode must be eval or exec"),
                )));
            }
        };
        let validation_source_name = source_name.clone();
        let validated = tokio::task::spawn_blocking(move || {
            (reservation, Execution::new(mode, source_name, source))
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
            document_id,
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
        let (control, control_task) = spawn_control_reader(inbound, job_id);
        let state = ExecuteResponseState {
            output,
            output_done: false,
            completion: Box::pin(completion.wait()),
            control,
            control_open: true,
            control_task,
            accepted_sent: false,
            finished: false,
            _reservation: reservation,
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
    async fn next_event(&mut self) -> Result<Option<ExecutionServerEvent>, Status> {
        if self.finished {
            return Ok(None);
        }
        if !self.accepted_sent {
            self.accepted_sent = true;
            return Ok(Some(server_event(execution_server_event::Event::Accepted(
                ExecutionAccepted {},
            ))));
        }

        while !self.output_done {
            if self.control_open {
                tokio::select! {
                    biased;
                    control = self.control.recv() => {
                        if let Some(event) = self.handle_control(control)? {
                            return Ok(Some(event));
                        }
                    }
                    output = self.output.next_chunk() => {
                        match output {
                            Some(chunk) => return Ok(Some(server_event(
                                execution_server_event::Event::Output(ExecutionOutput { chunk }),
                            ))),
                            None => self.output_done = true,
                        }
                    }
                }
            } else {
                match self.output.next_chunk().await {
                    Some(chunk) => {
                        return Ok(Some(server_event(execution_server_event::Event::Output(
                            ExecutionOutput { chunk },
                        ))));
                    }
                    None => self.output_done = true,
                }
            }
        }

        loop {
            let outcome = if self.control_open {
                tokio::select! {
                    biased;
                    control = self.control.recv() => {
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
            self.finished = true;
            return Ok(Some(finished_event(match outcome {
                Ok(outcome) => outcome,
                Err(error) => ExecutionOutcome::Failure(scheduler_failure(error)),
            })));
        }
    }

    fn handle_control(
        &mut self,
        control: Option<Result<ExecutionCancelDisposition, Status>>,
    ) -> Result<Option<ExecutionServerEvent>, Status> {
        match control {
            Some(Ok(result)) => Ok(Some(server_event(
                execution_server_event::Event::CancelAcknowledgement(
                    ExecutionCancelAcknowledgement {
                        disposition: result as i32,
                    },
                ),
            ))),
            Some(Err(status)) => Err(status),
            None => {
                self.control_open = false;
                Ok(None)
            }
        }
    }
}

impl Drop for ExecuteResponseState {
    fn drop(&mut self) {
        self.control_task.abort();
    }
}

fn spawn_control_reader(
    mut inbound: tonic::Streaming<ExecutionClientMessage>,
    job_id: crate::scheduler::MutationJobId,
) -> (
    mpsc::Receiver<Result<ExecutionCancelDisposition, Status>>,
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
                Some(execution_client_message::Message::Cancel(_))
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
                CancelResult::Accepted => ExecutionCancelDisposition::Accepted,
                CancelResult::TooLate | CancelResult::NotFound => {
                    ExecutionCancelDisposition::TooLate
                }
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

fn terminal_response(
    reservation: crate::scheduler::ExecutionReservation,
    failure: ExecutionFailure,
) -> ExecuteResponse {
    let event = finished_event(ExecutionOutcome::Failure(failure));
    Box::pin(stream::unfold(
        (reservation, Some(event)),
        |(reservation, event)| async move { event.map(|event| (Ok(event), (reservation, None))) },
    ))
}

fn validate_execution_request(
    request: ExecutionRequest,
) -> Result<ExecutionRequest, ExecutionFailure> {
    if !crate::documents::valid_document_id(&request.document_id) {
        return Err(failure("The document ID is invalid"));
    }
    if request.source_name.is_empty() {
        return Err(failure("The source name is required"));
    }
    if request.source_name.len() > acadctl_rpc::MAX_SOURCE_NAME_BYTES {
        return Err(failure("The source name exceeds the 4 KiB limit"));
    }
    Ok(request)
}

fn validation_failure(error: SourceValidationError, source_name: String) -> ExecutionFailure {
    match error {
        SourceValidationError::SourceTooLarge => failure("The source exceeds the 4 MiB limit"),
        SourceValidationError::InvalidUtf8 => failure("The source is not valid UTF-8"),
        SourceValidationError::NullCharacter => {
            failure("The source contains U+0000, which AutoLISP cannot represent")
        }
        SourceValidationError::ExpectedOneForm { actual } => failure(format!(
            "eval requires exactly one top-level form; found {actual}"
        )),
        SourceValidationError::Scan(error) => ExecutionFailure {
            message: error.kind.message().to_owned(),
            form_index: None,
            location: Some(crate::execution::SourceLocation {
                source_name,
                line: error.line,
                column: error.column,
            }),
            drawing_outcome: DrawingOutcome::NotStarted,
        },
    }
}

fn failure(message: impl Into<String>) -> ExecutionFailure {
    ExecutionFailure {
        message: bounded_diagnostic(message.into()),
        form_index: None,
        location: None,
        drawing_outcome: DrawingOutcome::NotStarted,
    }
}

fn scheduler_failure(error: SchedulerError) -> ExecutionFailure {
    let drawing_outcome = match &error {
        SchedulerError::DocumentContextRestoreFailed(_)
        | SchedulerError::ExecutionBridgeFinalizationFailed(_)
        | SchedulerError::ExecutionBridgeSymbolsClearFailed(_)
        | SchedulerError::ExecutionBridgeFailed(_)
        | SchedulerError::ExecutionNotFinished
        | SchedulerError::NativeMutationStateUnknown
        | SchedulerError::Stopped
        | SchedulerError::UnknownResult(_) => DrawingOutcome::Unknown,
        SchedulerError::SchedulerStateUnavailable
        | SchedulerError::ScheduleFailed(_)
        | SchedulerError::PluginStopping
        | SchedulerError::DocumentNotFound(_)
        | SchedulerError::DocumentGone
        | SchedulerError::DocumentGenerationChanged
        | SchedulerError::Unnamed(_)
        | SchedulerError::ReadOnly(_)
        | SchedulerError::Dirty(_)
        | SchedulerError::NotDwg
        | SchedulerError::OpenFailed(_)
        | SchedulerError::LockFailed(_)
        | SchedulerError::SaveFailed(_)
        | SchedulerError::CloseFailed(_)
        | SchedulerError::HistoryFailed { .. }
        | SchedulerError::OpenNotPublished
        | SchedulerError::SaveNotPublished
        | SchedulerError::CloseNotPublished
        | SchedulerError::NotQuiescent
        | SchedulerError::UndoDisabled
        | SchedulerError::DocumentContextFailed(_)
        | SchedulerError::MutationCapacity
        | SchedulerError::ExecutionCapacity => DrawingOutcome::NotStarted,
    };
    ExecutionFailure {
        message: bounded_diagnostic(error.to_string()),
        form_index: None,
        location: None,
        drawing_outcome,
    }
}

fn finished_event(outcome: ExecutionOutcome) -> ExecutionServerEvent {
    let outcome = match outcome {
        ExecutionOutcome::Success => execution_outcome::Outcome::Success(ExecutionSuccess {}),
        ExecutionOutcome::Cancelled => execution_outcome::Outcome::Cancelled(ExecutionCancelled {}),
        ExecutionOutcome::Failure(failure) => {
            execution_outcome::Outcome::Failure(rpc_failure(failure))
        }
    };
    server_event(execution_server_event::Event::Finished(ExecutionFinished {
        outcome: Some(RpcExecutionOutcome {
            outcome: Some(outcome),
        }),
    }))
}

fn rpc_failure(failure: ExecutionFailure) -> RpcExecutionFailure {
    let drawing_outcome = match failure.drawing_outcome {
        DrawingOutcome::NotStarted => RpcDrawingOutcome::NotStarted,
        DrawingOutcome::RolledBack => RpcDrawingOutcome::RolledBack,
        DrawingOutcome::Committed => RpcDrawingOutcome::Committed,
        DrawingOutcome::Unknown => RpcDrawingOutcome::Unknown,
    };
    RpcExecutionFailure {
        message: bounded_diagnostic(failure.message),
        form_index: failure.form_index.map(|index| index as u64),
        location: failure.location.map(|location| RpcSourceLocation {
            source_name: location.source_name,
            line: location.line as u64,
            column: location.column as u64,
        }),
        drawing_outcome: drawing_outcome as i32,
    }
}

fn server_event(event: execution_server_event::Event) -> ExecutionServerEvent {
    ExecutionServerEvent { event: Some(event) }
}

fn validate_open_path(path: &str) -> Result<(), Status> {
    if path.len() > acadctl_rpc::MAX_DRAWING_PATH_BYTES {
        return Err(Status::invalid_argument(
            "The drawing path exceeds the 32 KiB limit",
        ));
    }
    let path = std::path::Path::new(path);

    if !path.is_absolute() {
        return Err(Status::invalid_argument(
            "The drawing path must be absolute",
        ));
    }

    if !is_dwg(path) {
        return Err(Status::invalid_argument("Only DWG drawings can be opened"));
    }

    Ok(())
}

fn validate_document_id(id: &str) -> Result<(), Status> {
    crate::documents::valid_document_id(id)
        .then_some(())
        .ok_or_else(|| Status::invalid_argument("The document ID is invalid"))
}

fn is_dwg(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dwg"))
}

fn scheduler_error(error: SchedulerError) -> Status {
    if matches!(&error, SchedulerError::DocumentNotFound(_)) {
        Status::not_found(error.to_string())
    } else if error.is_internal() {
        Status::internal(error.to_string())
    } else {
        Status::failed_precondition(error.to_string())
    }
}

pub fn start() -> Result<(), String> {
    crate::scheduler::start();
    let mut active = SERVER
        .lock()
        .map_err(|_| "server state is unavailable".to_owned())?;
    if active.as_ref().is_some_and(Server::is_running) {
        return Ok(());
    }
    if let Some(server) = active.take() {
        server.shutdown();
    }

    let (stop, stop_receiver) = oneshot::channel();
    let (startup_sender, startup_receiver) = std_mpsc::sync_channel(1);
    let thread = thread::spawn(move || run(stop_receiver, startup_sender));

    match startup_receiver.recv() {
        Ok(Ok(())) => {
            *active = Some(Server { stop, thread });
            Ok(())
        }
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        Err(_) => {
            let _ = thread.join();
            Err("server thread stopped during startup".to_owned())
        }
    }
}

pub fn stop() {
    crate::scheduler::stop();
    let server = SERVER.lock().ok().and_then(|mut active| active.take());
    if let Some(server) = server {
        server.shutdown();
    }
}

fn run(stop: oneshot::Receiver<()>, startup: std_mpsc::SyncSender<Result<(), String>>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let error = format!("could not create the async runtime: {error}");
            let _ = startup.send(Err(error));
            return;
        }
    };

    runtime.block_on(serve(std::process::id(), stop, startup))
}

async fn serve(
    process_id: u32,
    stop: oneshot::Receiver<()>,
    startup: std_mpsc::SyncSender<Result<(), String>>,
) {
    let timer_driver = tokio::spawn(crate::scheduler::drive_timers());
    serve_until_stopped(process_id, stop, startup).await;
    timer_driver.abort();
}

async fn serve_until_stopped(
    process_id: u32,
    mut stop: oneshot::Receiver<()>,
    startup: std_mpsc::SyncSender<Result<(), String>>,
) {
    let mut startup = Some(startup);
    loop {
        let connections = match acadctl_rpc::incoming(process_id) {
            Ok(incoming) => incoming,
            Err(error) => {
                let error = format!("could not create the RPC endpoint: {error}");
                if let Some(startup) = startup.take() {
                    let _ = startup.send(Err(error));
                    return;
                }
                if stopped_during_restart_backoff(&mut stop).await {
                    return;
                }
                continue;
            }
        };
        if startup
            .take()
            .is_some_and(|startup| startup.send(Ok(())).is_err())
        {
            return;
        }

        let serving = tonic::transport::Server::builder()
            .max_concurrent_streams(acadctl_rpc::MAX_STREAMS_PER_CONNECTION)
            .add_service(
                DocumentServiceServer::new(DocumentRpc)
                    .max_decoding_message_size(acadctl_rpc::MAX_DOCUMENT_REQUEST_BYTES)
                    .max_encoding_message_size(acadctl_rpc::MAX_DOCUMENT_RESPONSE_BYTES),
            )
            .add_service(
                ExecutionServiceServer::new(ExecutionRpc)
                    .max_decoding_message_size(acadctl_rpc::MAX_EXECUTION_REQUEST_BYTES)
                    .max_encoding_message_size(acadctl_rpc::MAX_EXECUTION_RESPONSE_BYTES),
            )
            .serve_with_incoming(connections);
        tokio::pin!(serving);
        tokio::select! {
            _ = &mut serving => {}
            _ = &mut stop => return,
        }
        if stopped_during_restart_backoff(&mut stop).await {
            return;
        }
    }
}

async fn stopped_during_restart_backoff(stop: &mut oneshot::Receiver<()>) -> bool {
    tokio::select! {
        _ = stop => true,
        _ = tokio::time::sleep(RESTART_BACKOFF) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use bytes::Bytes;

    #[test]
    fn reports_documents_and_stops_promptly() {
        let _test = crate::scheduler::TEST_LOCK.blocking_lock();
        crate::scheduler::replace_document_snapshot(vec![
            crate::ffi::NativeDocumentSnapshot {
                document_token: 1,
                database_token: 101,
                name: "/tmp/house.dwg".into(),
                named: true,
                modified: false,
                read_only: false,
            },
            crate::ffi::NativeDocumentSnapshot {
                document_token: 2,
                database_token: 102,
                name: "/tmp/site.dwg".into(),
                named: true,
                modified: true,
                read_only: true,
            },
        ]);
        start().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let client = runtime.block_on(async {
            let mut client = acadctl_rpc::connect_documents(std::process::id())
                .await
                .unwrap();
            let listed = client.list(ListRequest {}).await.unwrap().into_inner();
            assert_eq!(listed.documents.len(), 2);
            assert_eq!(listed.documents[0].id.len(), 6);
            assert_eq!(listed.documents[0].display_name, "house.dwg");
            assert_eq!(
                listed.documents[0].file_path.as_deref(),
                Some("/tmp/house.dwg")
            );
            assert!(!listed.documents[0].modified);
            assert!(!listed.documents[0].read_only);
            assert_eq!(listed.documents[1].id.len(), 6);
            assert_ne!(listed.documents[0].id, listed.documents[1].id);
            assert_eq!(listed.documents[1].display_name, "site.dwg");
            assert_eq!(
                listed.documents[1].file_path.as_deref(),
                Some("/tmp/site.dwg")
            );
            assert!(listed.documents[1].modified);
            assert!(listed.documents[1].read_only);

            let opened = client
                .open(OpenRequest {
                    path: "/tmp/house.dwg".into(),
                })
                .await
                .unwrap()
                .into_inner()
                .document
                .unwrap();
            assert_eq!(opened.id, listed.documents[0].id);

            let saved = client
                .save(SaveRequest {
                    id: opened.id.clone(),
                })
                .await
                .unwrap()
                .into_inner()
                .document
                .unwrap();
            assert_eq!(saved.id, opened.id);
            assert!(!saved.modified);

            let mut undo_client = client.clone();
            let undo_id = opened.id.clone();
            let undo_response =
                tokio::spawn(async move { undo_client.undo(HistoryRequest { id: undo_id }).await });
            let undo_action = next_native_action().await;
            assert_eq!(undo_action.kind, crate::ffi::NativeActionKind::Undo);
            crate::scheduler::replace_document_snapshot(vec![
                crate::ffi::NativeDocumentSnapshot {
                    document_token: 1,
                    database_token: 101,
                    name: "/tmp/house.dwg".into(),
                    named: true,
                    modified: true,
                    read_only: false,
                },
                crate::ffi::NativeDocumentSnapshot {
                    document_token: 2,
                    database_token: 102,
                    name: "/tmp/site.dwg".into(),
                    named: true,
                    modified: true,
                    read_only: true,
                },
            ]);
            crate::scheduler::complete_native_action(
                undo_action.job_id,
                crate::ffi::NativeActionResult {
                    kind: crate::ffi::NativeActionResultKind::Success,
                    native_status: 0,
                    native_detail: String::new(),
                },
            );
            let undone = undo_response
                .await
                .unwrap()
                .unwrap()
                .into_inner()
                .document
                .unwrap();
            assert_eq!(undone.id, opened.id);
            assert!(undone.modified);

            let mut redo_client = client.clone();
            let redo_id = opened.id.clone();
            let redo_response =
                tokio::spawn(async move { redo_client.redo(HistoryRequest { id: redo_id }).await });
            let redo_action = next_native_action().await;
            assert_eq!(redo_action.kind, crate::ffi::NativeActionKind::Redo);
            crate::scheduler::replace_document_snapshot(vec![
                crate::ffi::NativeDocumentSnapshot {
                    document_token: 1,
                    database_token: 101,
                    name: "/tmp/house.dwg".into(),
                    named: true,
                    modified: false,
                    read_only: false,
                },
                crate::ffi::NativeDocumentSnapshot {
                    document_token: 2,
                    database_token: 102,
                    name: "/tmp/site.dwg".into(),
                    named: true,
                    modified: true,
                    read_only: true,
                },
            ]);
            crate::scheduler::complete_native_action(
                redo_action.job_id,
                crate::ffi::NativeActionResult {
                    kind: crate::ffi::NativeActionResultKind::Success,
                    native_status: 0,
                    native_detail: String::new(),
                },
            );
            let redone = redo_response
                .await
                .unwrap()
                .unwrap()
                .into_inner()
                .document
                .unwrap();
            assert_eq!(redone.id, opened.id);
            assert!(!redone.modified);

            let close_error = client
                .close(CloseRequest {
                    id: listed.documents[1].id.clone(),
                    discard: false,
                })
                .await
                .unwrap_err();
            assert_eq!(close_error.code(), tonic::Code::FailedPrecondition);
            assert!(close_error.message().contains("has unsaved changes"));
            client
        });

        let started = Instant::now();
        stop();
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(client);
    }

    async fn next_native_action() -> crate::ffi::NativeAction {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let action = crate::scheduler::take_native_action();
            if action.kind != crate::ffi::NativeActionKind::None {
                return action;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "RPC did not enqueue a native action"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[test]
    fn execute_transport_preserves_the_four_mib_source_boundary() {
        let _test = crate::scheduler::TEST_LOCK.blocking_lock();
        crate::scheduler::replace_document_snapshot(vec![crate::ffi::NativeDocumentSnapshot {
            document_token: 1,
            database_token: 101,
            name: "/tmp/house.dwg".into(),
            named: true,
            modified: false,
            read_only: false,
        }]);
        let document_id = crate::scheduler::list().unwrap()[0].id.clone();
        start().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let mut client = acadctl_rpc::connect_execution(std::process::id())
                .await
                .unwrap();
            execute_and_cancel(
                &mut client,
                &document_id,
                Bytes::from(vec![b'x'; acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES]),
            )
            .await;

            let mut with_bom = Vec::with_capacity(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 3);
            with_bom.extend_from_slice(&[0xef, 0xbb, 0xbf]);
            with_bom.resize(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 3, b'x');
            execute_and_cancel(&mut client, &document_id, Bytes::from(with_bom)).await;

            let request = execution_request(
                &document_id,
                Bytes::from(vec![b'x'; acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 1]),
            );
            let mut response = client
                .execute(stream::iter([request]))
                .await
                .unwrap()
                .into_inner();
            let event = response.message().await.unwrap().unwrap();
            let Some(execution_server_event::Event::Finished(finished)) = event.event else {
                panic!("oversized source must fail before acceptance");
            };
            let Some(execution_outcome::Outcome::Failure(failure)) =
                finished.outcome.unwrap().outcome
            else {
                panic!("oversized source must produce a structured failure");
            };
            assert!(failure.message.contains("4 MiB"));
            assert_eq!(
                failure.drawing_outcome,
                RpcDrawingOutcome::NotStarted as i32
            );
            assert!(response.message().await.unwrap().is_none());
        });

        stop();
    }

    #[test]
    fn diagnostic_truncation_preserves_utf8_and_the_wire_bound() {
        let message = bounded_diagnostic("é".repeat(acadctl_rpc::MAX_DIAGNOSTIC_BYTES));
        assert!(message.len() <= acadctl_rpc::MAX_DIAGNOSTIC_BYTES);
        assert!(message.len() >= acadctl_rpc::MAX_DIAGNOSTIC_BYTES - 3);
        assert!(message.ends_with("... [truncated]"));
        assert!(std::str::from_utf8(message.as_bytes()).is_ok());
    }

    #[test]
    fn dropping_the_rpc_stream_detaches_without_cancelling_the_job() {
        let _test = crate::scheduler::TEST_LOCK.blocking_lock();
        crate::scheduler::replace_document_snapshot(vec![crate::ffi::NativeDocumentSnapshot {
            document_token: 1,
            database_token: 101,
            name: "/tmp/house.dwg".into(),
            named: true,
            modified: false,
            read_only: false,
        }]);
        let document_id = crate::scheduler::list().unwrap()[0].id.clone();
        start().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let mut client = acadctl_rpc::connect_execution(std::process::id())
                .await
                .unwrap();
            let (sender, receiver) = mpsc::channel(1);
            sender
                .send(execution_request(&document_id, Bytes::from_static(b"form")))
                .await
                .unwrap();
            let outbound = stream::unfold(receiver, |mut receiver| async move {
                receiver.recv().await.map(|message| (message, receiver))
            });
            let mut response = client.execute(outbound).await.unwrap().into_inner();
            assert!(matches!(
                response.message().await.unwrap().unwrap().event,
                Some(execution_server_event::Event::Accepted(_))
            ));
            drop(response);
            drop(sender);
            tokio::time::sleep(Duration::from_millis(20)).await;

            let action = crate::scheduler::take_native_action();
            assert_eq!(
                action.kind,
                crate::ffi::NativeActionKind::QueueExecutionDriver
            );
            assert_eq!(
                crate::scheduler::take_execution_step(action.job_id).kind(),
                crate::execution::ExecutionStepKind::BeginUndoGroup
            );
            assert!(crate::scheduler::complete_execution_step(
                action.job_id,
                successful_step()
            ));
            assert_eq!(
                crate::scheduler::take_execution_step(action.job_id).kind(),
                crate::execution::ExecutionStepKind::EvaluateForm
            );
            let mut writer = crate::scheduler::begin_println(1, 101);
            assert_eq!(
                writer.write(crate::execution::value_bridge::ValueEvent::Integer(1)),
                crate::execution::value_bridge::WriteResult::Disconnected
            );
            assert_eq!(
                writer.finish(),
                crate::execution::value_bridge::WriteResult::Disconnected
            );
            assert_eq!(
                crate::scheduler::cancel_execution(action.job_id),
                CancelResult::Accepted
            );
            assert!(crate::scheduler::complete_execution_step(
                action.job_id,
                successful_step()
            ));
            assert_eq!(
                crate::scheduler::take_execution_step(action.job_id).kind(),
                crate::execution::ExecutionStepKind::RollbackUndoGroup
            );
            assert!(crate::scheduler::complete_execution_step(
                action.job_id,
                successful_step()
            ));
            assert_eq!(
                crate::scheduler::take_execution_step(action.job_id).kind(),
                crate::execution::ExecutionStepKind::Done
            );
            crate::scheduler::complete_native_action(
                action.job_id,
                crate::ffi::NativeActionResult {
                    kind: crate::ffi::NativeActionResultKind::Success,
                    native_status: 0,
                    native_detail: String::new(),
                },
            );
        });

        stop();
    }

    async fn execute_and_cancel(
        client: &mut acadctl_rpc::ExecutionServiceClient<tonic::transport::Channel>,
        document_id: &str,
        source: Bytes,
    ) {
        let (sender, receiver) = mpsc::channel(2);
        sender
            .send(execution_request(document_id, source))
            .await
            .unwrap();
        let outbound = stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|message| (message, receiver))
        });
        let mut response = client.execute(outbound).await.unwrap().into_inner();
        let accepted = response.message().await.unwrap().unwrap();
        assert!(matches!(
            accepted.event,
            Some(execution_server_event::Event::Accepted(_))
        ));

        for _ in 0..2 {
            sender
                .send(ExecutionClientMessage {
                    message: Some(execution_client_message::Message::Cancel(
                        acadctl_rpc::ExecutionCancelRequest {},
                    )),
                })
                .await
                .unwrap();
        }
        let mut cancel_acknowledgement_count = 0;
        let mut finished_seen = false;
        while let Some(event) = response.message().await.unwrap() {
            match event.event {
                Some(execution_server_event::Event::CancelAcknowledgement(acknowledgement)) => {
                    assert_eq!(
                        acknowledgement.disposition,
                        ExecutionCancelDisposition::Accepted as i32
                    );
                    cancel_acknowledgement_count += 1;
                }
                Some(execution_server_event::Event::Finished(finished)) => {
                    assert!(matches!(
                        finished.outcome.unwrap().outcome,
                        Some(execution_outcome::Outcome::Cancelled(_))
                    ));
                    finished_seen = true;
                }
                Some(execution_server_event::Event::Accepted(_))
                | Some(execution_server_event::Event::Output(_))
                | None => panic!("unexpected execution event"),
            }
        }
        assert_eq!(cancel_acknowledgement_count, 1);
        assert!(finished_seen);
    }

    fn execution_request(document_id: &str, source: Bytes) -> ExecutionClientMessage {
        ExecutionClientMessage {
            message: Some(execution_client_message::Message::Request(
                ExecutionRequest {
                    document_id: document_id.into(),
                    mode: RpcExecutionMode::Exec as i32,
                    source_name: "<stdin>".into(),
                    source,
                },
            )),
        }
    }

    fn successful_step() -> crate::execution::ExecutionStepResult {
        crate::execution::ExecutionStepResult {
            kind: crate::execution::ExecutionStepResultKind::Success,
            native_status: 0,
            lisp_errno: 0,
            detail: String::new(),
            bridge_symbols_clear_status: 0,
        }
    }
}
