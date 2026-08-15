use std::time::Duration;

use acadctl_rpc::{Doc, DocServiceClient, ExecServiceClient, ListRequest, ProcessId};
use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::time::timeout;
use tonic::Code;
use tonic::transport::Channel;

const LIST_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Instance {
    pub process_id: ProcessId,
    pub documents: Result<Vec<Doc>, QueryError>,
}

pub enum QueryError {
    CannotConnect,
    TimedOut,
    OutdatedPlugin,
    RequestFailed(String),
}

pub async fn list() -> std::io::Result<Vec<Instance>> {
    let mut pending = acadctl_rpc::discover()?
        .into_iter()
        .map(query)
        .collect::<FuturesUnordered<_>>();

    let mut instances = Vec::new();

    while let Some(result) = pending.next().await {
        let Some(instance) = result else {
            continue;
        };

        instances.push(instance);
    }

    instances.sort_unstable_by_key(|instance| instance.process_id);

    Ok(instances)
}

async fn query(process_id: ProcessId) -> Option<Instance> {
    let documents = match timeout(LIST_TIMEOUT, query_documents(process_id)).await {
        Ok(result) => result,
        Err(_) => Err(QueryError::TimedOut),
    };

    if matches!(documents, Err(QueryError::CannotConnect)) {
        return None;
    }

    Some(Instance {
        process_id,
        documents,
    })
}

async fn query_documents(process_id: ProcessId) -> Result<Vec<Doc>, QueryError> {
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
    process_id: ProcessId,
) -> Result<DocServiceClient<Channel>, QueryError> {
    acadctl_rpc::connect_documents(process_id)
        .await
        .map_err(|_| QueryError::CannotConnect)
}

pub async fn connect_execution(
    process_id: ProcessId,
) -> Result<ExecServiceClient<Channel>, QueryError> {
    acadctl_rpc::connect_execution(process_id)
        .await
        .map_err(|_| QueryError::CannotConnect)
}

#[cfg(target_os = "macos")]
#[path = "../process/macos.rs"]
mod process;
#[cfg(windows)]
#[path = "../process/windows.rs"]
mod process;
#[cfg(not(any(target_os = "macos", windows)))]
#[path = "../process/unsupported.rs"]
mod process;

pub use process::{AutoCadProcess, autocad_processes};
