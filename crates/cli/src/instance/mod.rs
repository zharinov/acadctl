use std::{fmt, time::Duration};

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

impl Instance {
    async fn query(process_id: ProcessId) -> Option<Self> {
        let documents = match timeout(LIST_TIMEOUT, query_documents(process_id)).await {
            Ok(result) => result,
            Err(_) => Err(QueryError::TimedOut),
        };

        if matches!(documents, Err(QueryError::CannotConnect)) {
            return None;
        }

        Some(Self {
            process_id,
            documents,
        })
    }
}

pub enum QueryError {
    CannotConnect,
    TimedOut,
    OutdatedPlugin,
    RequestFailed(String),
}

pub struct ProcessSnapshot {
    processes: Vec<AutoCadProcess>,
}

impl ProcessSnapshot {
    pub fn discover() -> Self {
        Self {
            processes: process::discover(),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &AutoCadProcess> {
        self.processes.iter()
    }

    pub fn select(
        &self,
        requested_process_id: Option<ProcessId>,
    ) -> Result<&AutoCadProcess, ProcessSelectionError> {
        if let Some(process_id) = requested_process_id {
            return self
                .processes
                .iter()
                .find(|process| process.process_id() == process_id)
                .ok_or(ProcessSelectionError::NotRunning(process_id));
        }

        match self.processes.as_slice() {
            [process] => Ok(process),
            [] => Err(ProcessSelectionError::NotRunningAny),
            processes => Err(ProcessSelectionError::Ambiguous(
                processes.iter().map(AutoCadProcess::process_id).collect(),
            )),
        }
    }

    pub async fn query_instances(&self) -> Vec<Instance> {
        let mut pending = self
            .iter()
            .map(|process| Instance::query(process.process_id()))
            .collect::<FuturesUnordered<_>>();
        let mut instances = Vec::new();

        while let Some(result) = pending.next().await {
            let Some(instance) = result else {
                continue;
            };

            instances.push(instance);
        }

        instances.sort_unstable_by_key(|instance| instance.process_id);
        instances
    }
}

pub enum ProcessSelectionError {
    NotRunning(ProcessId),
    NotRunningAny,
    Ambiguous(Vec<ProcessId>),
}

impl fmt::Display for ProcessSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning(process_id) => {
                write!(formatter, "AutoCAD process {process_id} is not running.")
            }
            Self::NotRunningAny => formatter.write_str("AutoCAD is not running."),
            Self::Ambiguous(process_ids) => write!(
                formatter,
                "More than one AutoCAD instance is running ({}). Use `acadctl kill <pid>`.",
                process_ids
                    .iter()
                    .map(ProcessId::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
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

pub use process::AutoCadProcess;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_selection_errors_preserve_cli_guidance() {
        let first = ProcessId::new(123).unwrap();
        let second = ProcessId::new(456).unwrap();

        assert_eq!(
            ProcessSelectionError::NotRunningAny.to_string(),
            "AutoCAD is not running."
        );
        assert_eq!(
            ProcessSelectionError::NotRunning(second).to_string(),
            "AutoCAD process 01C8 is not running."
        );
        assert_eq!(
            ProcessSelectionError::Ambiguous(vec![first, second]).to_string(),
            "More than one AutoCAD instance is running (007B, 01C8). Use `acadctl kill <pid>`."
        );
    }
}
