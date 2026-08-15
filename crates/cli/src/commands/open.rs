use std::path::{Path, PathBuf};
use std::process::ExitCode;

use acadctl_rpc::{OpenRequest, ProcessId};

use crate::instances::Instance;

use super::{fail, query_error_message, request_error_message};

pub async fn run(path: PathBuf, process_id: Option<ProcessId>) -> ExitCode {
    let path = match drawing_path(&path) {
        Ok(path) => path,
        Err(error) => return fail(error),
    };
    let process_id = match process_id {
        Some(process_id) => process_id,
        None => {
            let instances = match crate::instances::list().await {
                Ok(instances) => instances,
                Err(_) => return fail("Could not inspect running AutoCAD instances.".into()),
            };
            match select_instance(&instances) {
                Ok(process_id) => process_id,
                Err(error) => return fail(error),
            }
        }
    };
    let mut client = match super::connect_documents(process_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };
    let opened = match client.open(OpenRequest { path }).await {
        Ok(response) => response.into_inner(),
        Err(status) => return fail(request_error_message("open the drawing", status)),
    };
    let Some(document) = opened.document else {
        return fail("AutoCAD did not identify the opened document.".into());
    };
    println!("{process_id}:{}", document.id);
    ExitCode::SUCCESS
}

fn drawing_path(path: &Path) -> Result<String, String> {
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dwg"))
    {
        return Err("Only DWG drawings can be opened.".into());
    }
    if !path.is_file() {
        return Err(format!("Drawing '{}' does not exist.", path.display()));
    }
    let path = std::fs::canonicalize(path)
        .map_err(|error| format!("Could not resolve '{}': {error}", path.display()))?;
    path.into_os_string().into_string().map_err(|path| {
        format!(
            "Drawing path '{}' is not valid UTF-8.",
            path.to_string_lossy()
        )
    })
}

fn select_instance(instances: &[Instance]) -> Result<ProcessId, String> {
    let available = instances
        .iter()
        .filter(|instance| instance.documents.is_ok())
        .collect::<Vec<_>>();
    match available.as_slice() {
        [instance] => Ok(instance.process_id),
        [] if instances.is_empty() => Err("AutoCAD is not running.".into()),
        [] => Err(instances
            .iter()
            .find_map(|instance| instance.documents.as_ref().err())
            .map(query_error_message)
            .unwrap_or_else(|| "No acadctl-enabled AutoCAD instance is available.".into())),
        instances => {
            let process_ids = instances
                .iter()
                .map(|instance| instance.process_id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "More than one acadctl-enabled AutoCAD instance is running ({process_ids}). Use `acadctl open <path> --pid <pid>`."
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::{Instance, QueryError};

    #[test]
    fn selects_the_only_available_instance() {
        let instances = vec![
            Instance {
                process_id: ProcessId::new(123).unwrap(),
                documents: Err(QueryError::CannotConnect),
            },
            Instance {
                process_id: ProcessId::new(456).unwrap(),
                documents: Ok(vec![]),
            },
        ];

        assert_eq!(
            select_instance(&instances).unwrap(),
            ProcessId::new(456).unwrap()
        );
    }

    #[test]
    fn requires_a_pid_when_multiple_instances_are_available() {
        let instances = vec![
            Instance {
                process_id: ProcessId::new(123).unwrap(),
                documents: Ok(vec![]),
            },
            Instance {
                process_id: ProcessId::new(456).unwrap(),
                documents: Ok(vec![]),
            },
        ];

        assert_eq!(
            select_instance(&instances).unwrap_err(),
            "More than one acadctl-enabled AutoCAD instance is running (007B, 01C8). Use `acadctl open <path> --pid <pid>`."
        );
    }
}
