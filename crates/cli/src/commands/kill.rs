use std::process::ExitCode;
use std::time::Duration;

use acadctl_rpc::ProcessId;
use tokio::time::{Instant, sleep};

use super::fail;
use crate::instance::{AutoCadProcess, ProcessSnapshot};

const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub async fn run(requested_process_id: Option<ProcessId>, force: bool) -> ExitCode {
    let processes = ProcessSnapshot::discover();
    let process = match processes.select(requested_process_id) {
        Ok(process) => process,
        Err(error) => return fail(error.to_string()),
    };
    let process_id = process.process_id();

    if !process.request_termination(force) {
        let action = if force { "force" } else { "ask" };

        return fail(format!(
            "Could not {action} AutoCAD process {process_id} to quit."
        ));
    }

    if wait_until_stopped(process).await {
        return ExitCode::SUCCESS;
    }

    if force {
        return fail(format!(
            "AutoCAD process {process_id} did not terminate within 5 seconds."
        ));
    }

    fail(format!(
        "AutoCAD process {process_id} did not exit within 5 seconds. Run `acadctl kill {process_id} --force` to terminate it immediately."
    ))
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
