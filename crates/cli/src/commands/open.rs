use std::path::PathBuf;
use std::process::ExitCode;

use acadctl_rpc::{DrawingPath, InstanceId, OpenRequest};

use crate::instance::{Instance, InstanceSnapshot};

use super::{fail, parse_drawing_id, query_error_message, request_error_message};

pub async fn run(path: PathBuf, instance_id: Option<InstanceId>) -> ExitCode {
    let path = match DrawingPath::canonicalize(&path) {
        Ok(path) => path,
        Err(error) => return fail(error.to_string()),
    };

    let instance_id = match instance_id {
        Some(instance_id) => instance_id,
        None => {
            let snapshot = InstanceSnapshot::discover();
            let instances = snapshot.query_instances().await;

            match select_instance(&instances) {
                Ok(instance_id) => instance_id,
                Err(error) => return fail(error),
            }
        }
    };

    let mut client = match super::connect_drawings(instance_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };

    let opened = match client.open(OpenRequest::from(path)).await {
        Ok(response) => response.into_inner(),
        Err(status) => return fail(request_error_message("open the DWG file", None, status)),
    };

    let Some(drawing) = opened.drawing else {
        return fail("AutoCAD did not identify the opened drawing".into());
    };

    let drawing_id = match parse_drawing_id(drawing.id) {
        Ok(id) => id,
        Err(error) => return fail(error),
    };

    println!("{instance_id}:{drawing_id}");
    ExitCode::SUCCESS
}

fn select_instance(instances: &[Instance]) -> Result<InstanceId, String> {
    if instances.is_empty() {
        return Err("AutoCAD is not running".into());
    }

    let available = instances
        .iter()
        .filter(|instance| instance.drawings.is_ok())
        .collect::<Vec<_>>();

    match available.as_slice() {
        [instance] => Ok(instance.instance_id),
        [] => Err(instances
            .iter()
            .find_map(|instance| {
                instance
                    .drawings
                    .as_ref()
                    .err()
                    .map(|error| (instance.instance_id, error))
            })
            .map(|(instance, error)| query_error_message(instance, error))
            .unwrap_or_else(|| "No AutoCAD instance is available".into())),
        instances => {
            let instance_ids = instances
                .iter()
                .map(|instance| instance.instance_id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "More than one AutoCAD instance is running ({instance_ids})"
            ))
        }
    }
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
            select_instance(&instances).unwrap(),
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
            select_instance(&instances).unwrap_err(),
            "More than one AutoCAD instance is running (007B, 01C8)"
        );
    }
}
