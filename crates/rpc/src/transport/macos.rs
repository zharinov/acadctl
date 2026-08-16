use std::io;
use std::path::PathBuf;

use objc2_foundation::NSTemporaryDirectory;
use tokio::net::{UnixListener, UnixStream};

use crate::ProcessId;

pub type ClientStream = UnixStream;
pub type ServerStream = UnixStream;

pub struct Listener {
    inner: UnixListener,
    path: PathBuf,
}

impl Listener {
    pub fn bind(process_id: ProcessId) -> io::Result<Self> {
        let path = endpoint(process_id);

        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let inner = UnixListener::bind(&path)?;

        Ok(Self { inner, path })
    }

    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "the shared transport loop also serves the mutable Windows listener"
    )]
    pub async fn accept(&mut self) -> io::Result<ServerStream> {
        self.inner.accept().await.map(|(stream, _)| stream)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub async fn connect(process_id: ProcessId) -> io::Result<ClientStream> {
    UnixStream::connect(endpoint(process_id)).await
}

fn endpoint(process_id: ProcessId) -> PathBuf {
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
        let endpoint = endpoint(ProcessId::new(123).unwrap());

        assert_eq!(endpoint.parent(), Some(user_temp_dir().as_path()));
        assert_eq!(endpoint.file_name().unwrap(), "acadctl-007B.sock");
    }
}
