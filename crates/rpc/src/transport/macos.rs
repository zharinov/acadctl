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

pub fn discover() -> io::Result<Vec<ProcessId>> {
    let entries = match std::fs::read_dir(user_temp_dir()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };

    let mut process_ids = entries
        .filter_map(Result::ok)
        .filter_map(|entry| process_id_from_file_name(entry.file_name().to_str()?))
        .collect::<Vec<_>>();
    process_ids.sort_unstable();
    process_ids.dedup();
    Ok(process_ids)
}

fn process_id_from_file_name(file_name: &str) -> Option<ProcessId> {
    let process_id = file_name
        .strip_prefix("acadctl-")?
        .strip_suffix(".sock")?
        .parse()
        .ok()?;
    (file_name == format!("acadctl-{process_id}.sock")).then_some(process_id)
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

    #[test]
    fn recognizes_only_canonical_socket_names() {
        assert_eq!(
            process_id_from_file_name("acadctl-0FA5.sock"),
            ProcessId::new(0xFA5)
        );
        assert_eq!(process_id_from_file_name("acadctl-0fa5.sock"), None);
        assert_eq!(process_id_from_file_name("acadctl-00FA5.sock"), None);
        assert_eq!(process_id_from_file_name("acadctl-123.sock"), None);
        assert_eq!(process_id_from_file_name("acadctl-0FA5"), None);
        assert_eq!(process_id_from_file_name("other-0FA5.sock"), None);
    }
}
