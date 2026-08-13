use std::ffi::OsStr;
use std::time::Duration;

use acadctl_rpc::{Document, StatusRequest};
use sysinfo::System;
use tokio::task::JoinSet;
use tokio::time::timeout;

const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Instance {
    pub process_id: u32,
    pub documents: Vec<Document>,
}

pub struct StatusReport {
    pub process_count: usize,
    pub instances: Vec<Instance>,
}

pub async fn status() -> StatusReport {
    let process_ids = autocad_process_ids();
    let process_count = process_ids.len();
    let mut pending = JoinSet::new();
    for process_id in process_ids {
        pending.spawn(query(process_id));
    }

    let mut instances = Vec::new();
    while let Some(result) = pending.join_next().await {
        if let Ok(Some(instance)) = result {
            instances.push(instance);
        }
    }
    instances.sort_unstable_by_key(|instance| instance.process_id);

    StatusReport {
        process_count,
        instances,
    }
}

async fn query(process_id: u32) -> Option<Instance> {
    timeout(STATUS_TIMEOUT, async move {
        let mut client = acadctl_rpc::connect(process_id).await.ok()?;
        let status = client.status(StatusRequest {}).await.ok()?.into_inner();
        Some(Instance {
            process_id,
            documents: status.documents,
        })
    })
    .await
    .ok()
    .flatten()
}

fn autocad_process_ids() -> Vec<u32> {
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
