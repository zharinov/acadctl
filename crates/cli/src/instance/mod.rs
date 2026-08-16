use std::{fmt, time::Duration};

use acadctl_rpc::{Drawing, DrawingServiceClient, ExecServiceClient, InstanceId, ListRequest};
use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::time::timeout;
use tonic::Code;
use tonic::transport::Channel;

const LIST_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Instance {
    pub instance_id: InstanceId,
    pub drawings: Result<Vec<Drawing>, QueryError>,
}

impl Instance {
    async fn query(instance_id: InstanceId) -> Self {
        let drawings = match timeout(LIST_TIMEOUT, query_drawings(instance_id)).await {
            Ok(result) => result,
            Err(_) => Err(QueryError::TimedOut),
        };

        Self {
            instance_id,
            drawings,
        }
    }
}

pub enum QueryError {
    CannotConnect,
    TimedOut,
    OutdatedPlugin,
    RequestFailed(String),
}

pub struct InstanceSnapshot {
    instances: Vec<AutoCadInstance>,
}

impl InstanceSnapshot {
    pub fn discover() -> Self {
        Self {
            instances: process::discover(),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &AutoCadInstance> {
        self.instances.iter()
    }

    pub fn launch() -> Result<Option<InstanceId>, String> {
        process::launch()
    }

    pub fn select(
        &self,
        requested_instance_id: Option<InstanceId>,
    ) -> Result<&AutoCadInstance, InstanceSelectionError> {
        if let Some(instance_id) = requested_instance_id {
            return self
                .instances
                .iter()
                .find(|instance| instance.instance_id() == instance_id)
                .ok_or(InstanceSelectionError::NotRunning(instance_id));
        }

        match self.instances.as_slice() {
            [instance] => Ok(instance),
            [] => Err(InstanceSelectionError::NotRunningAny),
            instances => Err(InstanceSelectionError::Ambiguous(
                instances.iter().map(AutoCadInstance::instance_id).collect(),
            )),
        }
    }

    pub async fn query_instances(&self) -> Vec<Instance> {
        let mut pending = self
            .iter()
            .map(|instance| Instance::query(instance.instance_id()))
            .collect::<FuturesUnordered<_>>();
        let mut instances = Vec::new();

        while let Some(instance) = pending.next().await {
            instances.push(instance);
        }

        instances.sort_unstable_by_key(|instance| instance.instance_id);
        instances
    }
}

pub enum InstanceSelectionError {
    NotRunning(InstanceId),
    NotRunningAny,
    Ambiguous(Vec<InstanceId>),
}

impl fmt::Display for InstanceSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning(instance_id) => {
                write!(formatter, "AutoCAD instance {instance_id} is not running")
            }
            Self::NotRunningAny => formatter.write_str("AutoCAD is not running"),
            Self::Ambiguous(instance_ids) => write!(
                formatter,
                "More than one AutoCAD instance is running ({})",
                instance_ids
                    .iter()
                    .map(InstanceId::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

async fn query_drawings(instance_id: InstanceId) -> Result<Vec<Drawing>, QueryError> {
    let mut client = connect_drawings(instance_id).await?;

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

    Ok(listed.drawings)
}

pub async fn connect_drawings(
    instance_id: InstanceId,
) -> Result<DrawingServiceClient<Channel>, QueryError> {
    acadctl_rpc::connect_drawings(instance_id)
        .await
        .map_err(|_| QueryError::CannotConnect)
}

pub async fn connect_execution(
    instance_id: InstanceId,
) -> Result<ExecServiceClient<Channel>, QueryError> {
    acadctl_rpc::connect_execution(instance_id)
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

pub use process::AutoCadInstance;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preserves_an_instance_when_its_plugin_is_unavailable() {
        let instance_id = InstanceId::new(std::process::id()).unwrap();
        let instance = Instance::query(instance_id).await;

        assert_eq!(instance.instance_id, instance_id);
        assert!(matches!(instance.drawings, Err(QueryError::CannotConnect)));
    }

    #[test]
    fn instance_selection_errors_preserve_cli_guidance() {
        let first = InstanceId::new(123).unwrap();
        let second = InstanceId::new(456).unwrap();

        assert_eq!(
            InstanceSelectionError::NotRunningAny.to_string(),
            "AutoCAD is not running"
        );
        assert_eq!(
            InstanceSelectionError::NotRunning(second).to_string(),
            "AutoCAD instance 01C8 is not running"
        );
        assert_eq!(
            InstanceSelectionError::Ambiguous(vec![first, second]).to_string(),
            "More than one AutoCAD instance is running (007B, 01C8)"
        );
    }
}
