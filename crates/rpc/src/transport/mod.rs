use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::Stream;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tonic::transport::server::Connected;

use crate::ProcessId;

pub type ClientStream = platform::ClientStream;
pub type Incoming = Pin<Box<dyn Stream<Item = io::Result<ServerStream>> + Send>>;

pub async fn connect(process_id: ProcessId) -> io::Result<ClientStream> {
    platform::connect(process_id).await
}

pub fn incoming(process_id: ProcessId) -> io::Result<Incoming> {
    let listener = platform::Listener::bind(process_id)?;
    let permits = Arc::new(Semaphore::new(super::MAX_SERVER_CONNECTIONS));
    let connections = futures_util::stream::try_unfold(
        (listener, permits),
        |(mut listener, permits)| async move {
            let permit = Arc::clone(&permits)
                .acquire_owned()
                .await
                .map_err(io::Error::other)?;
            let stream = listener.accept().await?;

            Ok(Some((
                ServerStream {
                    inner: stream,
                    _permit: permit,
                },
                (listener, permits),
            )))
        },
    );

    Ok(Box::pin(connections))
}

pub struct ServerStream {
    inner: platform::ServerStream,
    _permit: OwnedSemaphorePermit,
}

impl AsyncRead for ServerStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(context, buffer)
    }
}

impl AsyncWrite for ServerStream {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_write(context, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(context)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
    }
}

impl Connected for ServerStream {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod platform;

#[cfg(windows)]
#[path = "windows.rs"]
mod platform;
