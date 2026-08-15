use std::sync::{Mutex, mpsc as std_mpsc};
use std::thread::{self, JoinHandle as ThreadJoinHandle};
use std::time::Duration;

use acadctl_rpc::{DocServiceServer, ExecServiceServer};
use tokio::sync::oneshot;

use super::doc::DocRpc;
use super::exec::ExecRpc;

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
    let process_id =
        acadctl_rpc::ProcessId::new(std::process::id()).expect("the current process ID is nonzero");
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

    runtime.block_on(serve(process_id, stop, startup))
}

async fn serve(
    process_id: acadctl_rpc::ProcessId,
    stop: oneshot::Receiver<()>,
    startup: std_mpsc::SyncSender<Result<(), String>>,
) {
    let timer_driver = tokio::spawn(crate::scheduler::drive_timers());
    serve_until_stopped(process_id, stop, startup).await;
    timer_driver.abort();
}

async fn serve_until_stopped(
    process_id: acadctl_rpc::ProcessId,
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
                DocServiceServer::new(DocRpc)
                    .max_decoding_message_size(acadctl_rpc::MAX_DOCUMENT_REQUEST_BYTES)
                    .max_encoding_message_size(acadctl_rpc::MAX_DOCUMENT_RESPONSE_BYTES),
            )
            .add_service(
                ExecServiceServer::new(ExecRpc)
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
