use std::sync::{Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use acadctl_rpc::{Acadctl, AcadctlServer, Document, ListRequest, ListResponse};
use tokio::sync::oneshot;
use tonic::{Request, Response, Status};

const MAX_CONCURRENT_STREAMS: u32 = 32;
const RESTART_BACKOFF: Duration = Duration::from_millis(100);

static SERVER: Mutex<Option<Server>> = Mutex::new(None);
static DOCUMENTS: Mutex<Vec<Document>> = Mutex::new(Vec::new());

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
        let documents = DOCUMENTS
            .lock()
            .map_err(|_| Status::internal("document state is unavailable"))?
            .clone();
        Ok(Response::new(ListResponse { documents }))
    }
}

pub fn start() -> Result<(), String> {
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
    let server = SERVER.lock().ok().and_then(|mut active| active.take());
    if let Some(server) = server {
        server.shutdown();
    }
}

pub fn set_documents(documents: Vec<crate::ffi::DocumentState>) {
    if let Ok(mut active) = DOCUMENTS.lock() {
        *active = documents
            .into_iter()
            .map(|document| Document {
                id: document.id,
                path: document.path,
                modified: document.modified,
                read_only: document.read_only,
            })
            .collect();
    }
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
        set_documents(vec![
            crate::ffi::DocumentState {
                id: "k7m2qx".into(),
                path: "/tmp/house.dwg".into(),
                modified: false,
                read_only: false,
            },
            crate::ffi::DocumentState {
                id: "p8z4cw".into(),
                path: "/tmp/site.dwg".into(),
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
            assert_eq!(listed.documents[0].id, "k7m2qx");
            assert_eq!(listed.documents[0].path, "/tmp/house.dwg");
            assert!(!listed.documents[0].modified);
            assert!(!listed.documents[0].read_only);
            assert_eq!(listed.documents[1].id, "p8z4cw");
            assert_eq!(listed.documents[1].path, "/tmp/site.dwg");
            assert!(listed.documents[1].modified);
            assert!(listed.documents[1].read_only);
            client
        });

        let started = Instant::now();
        stop();
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(client);
    }
}
