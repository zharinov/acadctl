use std::sync::{Mutex, mpsc};
use std::thread::{self, JoinHandle};

use acadctl_rpc::{Acadctl, AcadctlServer, Document, ListRequest, ListResponse};
use tokio::sync::oneshot;
use tonic::{Request, Response, Status};

const MAX_CONCURRENT_STREAMS: u32 = 32;

static SERVER: Mutex<Option<Server>> = Mutex::new(None);
static DOCUMENTS: Mutex<Vec<Document>> = Mutex::new(Vec::new());

struct Server {
    stop: oneshot::Sender<()>,
    thread: JoinHandle<()>,
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
    if active.is_some() {
        return Ok(());
    }

    let (stop, stop_receiver) = oneshot::channel();
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    let thread = thread::spawn(move || run(stop_receiver, ready_sender));

    match ready_receiver.recv() {
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
        let _ = server.stop.send(());
        let _ = server.thread.join();
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

fn run(stop: oneshot::Receiver<()>, ready: mpsc::SyncSender<Result<(), String>>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(format!("could not create the async runtime: {error}")));
            return;
        }
    };

    runtime.block_on(async move {
        let incoming = match acadctl_rpc::incoming(std::process::id()) {
            Ok(incoming) => incoming,
            Err(error) => {
                let _ = ready.send(Err(format!("could not create the RPC endpoint: {error}")));
                return;
            }
        };
        if ready.send(Ok(())).is_err() {
            return;
        }

        let serving = tonic::transport::Server::builder()
            .max_concurrent_streams(MAX_CONCURRENT_STREAMS)
            .add_service(AcadctlServer::new(Service))
            .serve_with_incoming(incoming);
        tokio::pin!(serving);
        tokio::select! {
            _ = &mut serving => {}
            _ = stop => {}
        }
    });
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
