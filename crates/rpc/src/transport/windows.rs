use std::io;
use std::time::Duration;

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};

use crate::ProcessId;

const ERROR_PIPE_BUSY: i32 = 231;

pub type ClientStream = NamedPipeClient;
pub type ServerStream = NamedPipeServer;

pub struct Listener {
    name: String,
    next: NamedPipeServer,
}

impl Listener {
    pub fn bind(process_id: ProcessId) -> io::Result<Self> {
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

pub async fn connect(process_id: ProcessId) -> io::Result<ClientStream> {
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

fn endpoint(process_id: ProcessId) -> String {
    format!(r"\\.\pipe\acadctl-{process_id}")
}

fn server_options(first: bool) -> ServerOptions {
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    options
}
