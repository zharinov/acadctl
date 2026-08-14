use std::ffi::OsStr;
use std::time::Duration;

use acadctl_rpc::{Document, DocumentServiceClient, ListRequest};
use sysinfo::System;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tonic::Code;
use tonic::transport::Channel;

const LIST_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Instance {
    pub process_id: u32,
    pub documents: Result<Vec<Document>, QueryError>,
}

pub struct ListReport {
    pub instances: Vec<Instance>,
}

pub enum QueryError {
    CannotConnect,
    TimedOut,
    OutdatedPlugin,
    RequestFailed(String),
}

pub enum ListError {
    QueryTaskFailed,
}

pub async fn list() -> Result<ListReport, ListError> {
    let process_ids = autocad_process_ids();
    let mut pending = JoinSet::new();
    for process_id in process_ids {
        pending.spawn(query(process_id));
    }

    let mut instances = Vec::new();
    while let Some(result) = pending.join_next().await {
        instances.push(result.map_err(|_| ListError::QueryTaskFailed)?);
    }
    instances.sort_unstable_by_key(|instance| instance.process_id);

    Ok(ListReport { instances })
}

async fn query(process_id: u32) -> Instance {
    let documents = match timeout(LIST_TIMEOUT, query_documents(process_id)).await {
        Ok(result) => result,
        Err(_) => Err(QueryError::TimedOut),
    };

    Instance {
        process_id,
        documents,
    }
}

async fn query_documents(process_id: u32) -> Result<Vec<Document>, QueryError> {
    let mut client = connect_documents(process_id).await?;
    let listed = client
        .list(ListRequest {})
        .await
        .map_err(|status| {
            if status.code() == Code::Unimplemented {
                QueryError::OutdatedPlugin
            } else {
                QueryError::RequestFailed(status.message().to_owned())
            }
        })?
        .into_inner();
    Ok(listed.documents)
}

pub async fn connect_documents(
    process_id: u32,
) -> Result<DocumentServiceClient<Channel>, QueryError> {
    acadctl_rpc::connect_documents(process_id)
        .await
        .map_err(|_| QueryError::CannotConnect)
}

pub fn autocad_process_ids() -> Vec<u32> {
    let system = System::new_all();
    let mut pids = system
        .processes()
        .values()
        .filter(|process| is_autocad_process(process.name(), process.exe()))
        .map(|process| process.pid().as_u32())
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids
}

fn is_autocad_process(name: &OsStr, executable: Option<&std::path::Path>) -> bool {
    is_autocad_name(name)
        || executable
            .and_then(std::path::Path::file_name)
            .is_some_and(is_autocad_name)
}

fn is_autocad_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.eq_ignore_ascii_case("autocad") || name.eq_ignore_ascii_case("acad.exe")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn recognizes_macos_and_windows_processes() {
        assert!(is_autocad_process(
            OsStr::new("AutoCAD"),
            Some(Path::new("/Applications/AutoCAD"))
        ));
        assert!(is_autocad_process(
            OsStr::new("acad.exe"),
            Some(Path::new(r"C:\Program Files\Autodesk\AutoCAD\acad.exe"))
        ));
        assert!(!is_autocad_process(
            OsStr::new("acadctl"),
            Some(Path::new("/usr/local/bin/acadctl"))
        ));
    }
}
