use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::Stream;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tonic::transport::server::Connected;

const MAX_CONNECTIONS: usize = 32;

pub type ClientStream = platform::ClientStream;
pub type Incoming = Pin<Box<dyn Stream<Item = io::Result<ServerStream>> + Send>>;

pub async fn connect(process_id: u32) -> io::Result<ClientStream> {
    platform::connect(process_id).await
}

pub fn incoming(process_id: u32) -> io::Result<Incoming> {
    let listener = platform::Listener::bind(process_id)?;
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
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
mod platform {
    use std::path::PathBuf;

    use objc2_foundation::NSTemporaryDirectory;
    use tokio::net::{UnixListener, UnixStream};

    use super::io;

    pub type ClientStream = UnixStream;
    pub type ServerStream = UnixStream;

    pub struct Listener {
        inner: UnixListener,
        path: PathBuf,
    }

    impl Listener {
        pub fn bind(process_id: u32) -> io::Result<Self> {
            let path = endpoint(process_id);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }

            let inner = UnixListener::bind(&path)?;
            Ok(Self { inner, path })
        }

        pub async fn accept(&mut self) -> io::Result<ServerStream> {
            self.inner.accept().await.map(|(stream, _)| stream)
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    pub async fn connect(process_id: u32) -> io::Result<ClientStream> {
        UnixStream::connect(endpoint(process_id)).await
    }

    fn endpoint(process_id: u32) -> PathBuf {
        user_temp_dir().join(format!("acadctl-{process_id}.sock"))
    }

    fn user_temp_dir() -> PathBuf {
        PathBuf::from(NSTemporaryDirectory().to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn endpoint_uses_the_system_user_temp_directory() {
            let endpoint = endpoint(123);
            assert_eq!(endpoint.parent(), Some(user_temp_dir().as_path()));
            assert_eq!(endpoint.file_name().unwrap(), "acadctl-123.sock");
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    use super::io;

    const ERROR_PIPE_BUSY: i32 = 231;

    pub type ClientStream = NamedPipeClient;
    pub type ServerStream = NamedPipeServer;

    pub struct Listener {
        name: String,
        next: NamedPipeServer,
    }

    impl Listener {
        pub fn bind(process_id: u32) -> io::Result<Self> {
            let name = endpoint(process_id);
            let next = server_options(true).create(&name)?;
            Ok(Self { name, next })
        }

        pub async fn accept(&mut self) -> io::Result<ServerStream> {
            self.next.connect().await?;
            let next = server_options(false).create(&self.name)?;
            Ok(std::mem::replace(&mut self.next, next))
        }
    }

    pub async fn connect(process_id: u32) -> io::Result<ClientStream> {
        let name = endpoint(process_id);
        loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => return Ok(client),
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn endpoint(process_id: u32) -> String {
        format!(r"\\.\pipe\acadctl-{process_id}")
    }

    fn server_options(first: bool) -> ServerOptions {
        let mut options = ServerOptions::new();
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true);
        options
    }
}
