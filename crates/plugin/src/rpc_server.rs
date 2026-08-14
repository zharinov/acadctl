use std::sync::{Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use acadctl_rpc::{
    Acadctl, AcadctlServer, CloseRequest, CloseResponse, ListRequest, ListResponse, OpenRequest,
    OpenResponse, SaveRequest, SaveResponse,
};
use tokio::sync::oneshot;
use tonic::{Request, Response, Status};

use crate::scheduler::Error as SchedulerError;

const MAX_CONCURRENT_STREAMS: u32 = 32;
const RESTART_BACKOFF: Duration = Duration::from_millis(100);

static SERVER: Mutex<Option<Server>> = Mutex::new(None);

struct Server {
    stop: oneshot::Sender<()>,
    thread: JoinHandle<()>,
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

struct Service;

#[tonic::async_trait]
impl Acadctl for Service {
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

fn validate_open_path(path: &str) -> Result<(), Status> {
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
    if id.is_empty() {
        Err(Status::invalid_argument("The document ID is required"))
    } else {
        Ok(())
    }
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
    let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
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

pub fn replace_documents(documents: Vec<crate::ffi::NativeDocumentState>) {
    crate::scheduler::replace_documents(documents);
}

fn run(stop: oneshot::Receiver<()>, startup: mpsc::SyncSender<Result<(), String>>) {
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
    mut stop: oneshot::Receiver<()>,
    startup: mpsc::SyncSender<Result<(), String>>,
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
            .max_concurrent_streams(MAX_CONCURRENT_STREAMS)
            .add_service(AcadctlServer::new(Service))
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

    #[test]
    fn reports_documents_and_stops_promptly() {
        let _test = crate::scheduler::TEST_LOCK.blocking_lock();
        replace_documents(vec![
            crate::ffi::NativeDocumentState {
                token: 1,
                database_token: 101,
                name: "/tmp/house.dwg".into(),
                named: true,
                modified: false,
                read_only: false,
            },
            crate::ffi::NativeDocumentState {
                token: 2,
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
            let mut client = acadctl_rpc::connect(std::process::id()).await.unwrap();
            let listed = client.list(ListRequest {}).await.unwrap().into_inner();
            assert_eq!(listed.documents.len(), 2);
            assert_eq!(listed.documents[0].id.len(), 6);
            assert_eq!(listed.documents[0].path, "/tmp/house.dwg");
            assert!(!listed.documents[0].modified);
            assert!(!listed.documents[0].read_only);
            assert_eq!(listed.documents[1].id.len(), 6);
            assert_ne!(listed.documents[0].id, listed.documents[1].id);
            assert_eq!(listed.documents[1].path, "/tmp/site.dwg");
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
}
