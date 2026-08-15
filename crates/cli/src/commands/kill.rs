use std::process::ExitCode;
use std::time::Duration;

use acadctl_rpc::ProcessId;
use tokio::time::{Instant, sleep};

use super::fail;
use crate::instance::{AutoCadProcess, autocad_processes};

const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub async fn run(requested_process_id: Option<ProcessId>, force: bool) -> ExitCode {
    let processes = autocad_processes();
    let process_ids = processes
        .iter()
        .map(AutoCadProcess::process_id)
        .collect::<Vec<_>>();
    let process_id = match select_process_id(&process_ids, requested_process_id) {
        Ok(process_id) => process_id,
        Err(error) => return fail(error),
    };

    let process = processes
        .into_iter()
        .find(|process| process.process_id() == process_id)
        .expect("the selected AutoCAD process came from this snapshot");

    if !process.request_termination(force) {
        let action = if force { "force" } else { "ask" };

        return fail(format!(
            "Could not {action} AutoCAD process {process_id} to quit."
        ));
    }

    if wait_until_stopped(&process).await {
        ExitCode::SUCCESS
    } else if force {
        fail(format!(
            "AutoCAD process {process_id} did not terminate within 5 seconds."
        ))
    } else {
        fail(format!(
            "AutoCAD process {process_id} did not exit within 5 seconds. Run `acadctl kill {process_id} --force` to terminate it immediately."
        ))
    }
}

fn select_process_id(
    process_ids: &[ProcessId],
    requested_process_id: Option<ProcessId>,
) -> Result<ProcessId, String> {
    match requested_process_id {
        Some(process_id) if process_ids.contains(&process_id) => Ok(process_id),
        Some(process_id) => Err(format!("AutoCAD process {process_id} is not running.")),
        None => match process_ids {
            [process_id] => Ok(*process_id),
            [] => Err("AutoCAD is not running.".into()),
            process_ids => Err(format!(
                "More than one AutoCAD instance is running ({}). Use `acadctl kill <pid>`.",
                process_ids
                    .iter()
                    .map(ProcessId::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
    }
}

async fn wait_until_stopped(process: &AutoCadProcess) -> bool {
    let deadline = Instant::now() + EXIT_TIMEOUT;

    loop {
        if process.has_exited() {
            return true;
        }

        if Instant::now() >= deadline {
            return false;
        }

        sleep(EXIT_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_the_only_autocad_process() {
        let first = ProcessId::new(123).unwrap();
        let second = ProcessId::new(456).unwrap();
        assert_eq!(select_process_id(&[first], None).unwrap(), first);
        assert_eq!(
            select_process_id(&[first, second], Some(second)).unwrap(),
            second
        );
    }

    #[test]
    fn requires_an_exact_process_when_selection_is_ambiguous() {
        let first = ProcessId::new(123).unwrap();
        let second = ProcessId::new(456).unwrap();
        assert_eq!(
            select_process_id(&[], None).unwrap_err(),
            "AutoCAD is not running."
        );
        assert_eq!(
            select_process_id(&[first, second], None).unwrap_err(),
            "More than one AutoCAD instance is running (007B, 01C8). Use `acadctl kill <pid>`."
        );
        assert_eq!(
            select_process_id(&[first], Some(second)).unwrap_err(),
            "AutoCAD process 01C8 is not running."
        );
    }
}
