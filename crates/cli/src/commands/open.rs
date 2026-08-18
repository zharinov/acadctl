use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use acadctl_rpc::{DrawingPath, InstanceId, OpenRequest};
use tokio::time::{sleep, timeout};

use crate::instance::{Instance, InstanceSnapshot};

use super::{RequestOperation, fail, parse_drawing_id, query_error_message, request_error_message};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(300);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub async fn run(path: PathBuf, instance_id: Option<InstanceId>) -> ExitCode {
    let path = match DrawingPath::canonicalize(&path) {
        Ok(path) => path,
        Err(error) => return fail(error.to_string()),
    };

    let instance_id = match resolve_instance(instance_id).await {
        Ok(instance) => instance,
        Err(error) => return fail(error),
    };

    let mut client = match super::connect_drawings(instance_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };

    let request = OpenRequest::from(path);
    let opened = match client.open(request).await {
        Ok(response) => response.into_inner(),
        Err(status) => return fail(request_error_message(RequestOperation::Open, None, status)),
    };

    let Some(drawing) = opened.drawing else {
        return fail("Invalid response: opened drawing is missing.".into());
    };

    let drawing_id = match parse_drawing_id(drawing.id) {
        Ok(id) => id,
        Err(error) => return fail(error),
    };

    println!("{instance_id}{drawing_id}");
    ExitCode::SUCCESS
}

async fn resolve_instance(instance_id: Option<InstanceId>) -> Result<InstanceId, String> {
    if let Some(instance_id) = instance_id {
        return Ok(instance_id);
    }

    let snapshot = InstanceSnapshot::discover();
    let running = snapshot
        .iter()
        .map(|instance| instance.instance_id())
        .collect::<Vec<_>>();

    if running.is_empty() {
        let launched = InstanceSnapshot::launch()?;

        return wait_for_launched_instance(launched).await;
    }

    let instances = snapshot.query_instances().await;
    select_instance(&instances, &running)
}

async fn wait_for_launched_instance(launched: Option<InstanceId>) -> Result<InstanceId, String> {
    timeout(STARTUP_TIMEOUT, async {
        loop {
            let instances = InstanceSnapshot::discover().query_instances().await;

            match select_launched_instance(&instances, launched)? {
                Some(instance_id) => return Ok(instance_id),
                None => sleep(STARTUP_POLL_INTERVAL).await,
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err("Timeout: AutoCAD did not start.".into()))
}

fn select_launched_instance(
    instances: &[Instance],
    launched: Option<InstanceId>,
) -> Result<Option<InstanceId>, String> {
    let Some(launched) = launched else {
        return select_available_instance(instances);
    };

    Ok(instances
        .iter()
        .find(|instance| instance.instance_id == launched && instance.drawings.is_ok())
        .map(|instance| instance.instance_id))
}

fn select_instance(instances: &[Instance], running: &[InstanceId]) -> Result<InstanceId, String> {
    if let Some(instance_id) = select_available_instance(instances)? {
        return Ok(instance_id);
    }

    if let Some(error) = instances
        .iter()
        .find_map(|instance| instance.drawings.as_ref().err())
    {
        return Err(query_error_message(error));
    }

    match running {
        [instance_id] => Ok(*instance_id),
        [] => Err("AutoCAD is not running".into()),
        instances => Err(ambiguous_instances(instances)),
    }
}

fn select_available_instance(instances: &[Instance]) -> Result<Option<InstanceId>, String> {
    let available = instances
        .iter()
        .filter(|instance| instance.drawings.is_ok())
        .map(|instance| instance.instance_id)
        .collect::<Vec<_>>();

    match available.as_slice() {
        [instance_id] => Ok(Some(*instance_id)),
        [] => Ok(None),
        instances => Err(ambiguous_instances(instances)),
    }
}

fn ambiguous_instances(instances: &[InstanceId]) -> String {
    let instance_ids = instances
        .iter()
        .map(InstanceId::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("More than one AutoCAD instance is running ({instance_ids})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance::{Instance, QueryError};

    #[test]
    fn selects_the_only_available_instance() {
        let instances = vec![
            Instance {
                instance_id: InstanceId::new(123).unwrap(),
                drawings: Err(QueryError::CannotConnect),
            },
            Instance {
                instance_id: InstanceId::new(456).unwrap(),
                drawings: Ok(vec![]),
            },
        ];

        assert_eq!(
            select_instance(
                &instances,
                &[InstanceId::new(123).unwrap(), InstanceId::new(456).unwrap()]
            )
            .unwrap(),
            InstanceId::new(456).unwrap()
        );
    }

    #[test]
    fn requires_an_instance_when_multiple_are_available() {
        let instances = vec![
            Instance {
                instance_id: InstanceId::new(123).unwrap(),
                drawings: Ok(vec![]),
            },
            Instance {
                instance_id: InstanceId::new(456).unwrap(),
                drawings: Ok(vec![]),
            },
        ];

        assert_eq!(
            select_instance(
                &instances,
                &[InstanceId::new(123).unwrap(), InstanceId::new(456).unwrap()]
            )
            .unwrap_err(),
            "More than one AutoCAD instance is running (007B, 01C8)"
        );
    }

    #[test]
    fn selects_a_running_instance_while_its_plugin_is_starting() {
        let instance_id = InstanceId::new(123).unwrap();

        assert_eq!(select_instance(&[], &[instance_id]).unwrap(), instance_id);
    }

    #[test]
    fn waits_for_the_instance_returned_by_the_launcher() {
        let launched = InstanceId::new(456).unwrap();
        let mut instances = vec![
            Instance {
                instance_id: InstanceId::new(123).unwrap(),
                drawings: Ok(vec![]),
            },
            Instance {
                instance_id: launched,
                drawings: Err(QueryError::CannotConnect),
            },
        ];

        assert_eq!(
            select_launched_instance(&instances, Some(launched)),
            Ok(None)
        );

        instances[1].drawings = Ok(vec![]);

        assert_eq!(
            select_launched_instance(&instances, Some(launched)),
            Ok(Some(launched))
        );
    }
}
