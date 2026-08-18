use std::process::ExitCode;
use std::time::Duration;

use acadctl_rpc::InstanceId;
use tokio::time::{Instant, sleep};

use super::fail;
use crate::instance::{AutoCadInstance, InstanceSnapshot};

const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub async fn run(requested_instance: Option<InstanceId>, force: bool) -> ExitCode {
    let instances = InstanceSnapshot::discover();
    let instance = match instances.select(requested_instance) {
        Ok(instance) => instance,
        Err(error) => return fail(error.to_string()),
    };
    let instance_id = instance.instance_id();

    if !instance.request_termination(force) {
        return fail(format!("Stop failed for AutoCAD instance {instance_id}."));
    }

    if wait_until_stopped(instance).await {
        return ExitCode::SUCCESS;
    }

    if force {
        return fail(format!(
            "Timeout: AutoCAD instance {instance_id} did not stop."
        ));
    }

    fail(format!(
        "Timeout: AutoCAD instance {instance_id} did not stop. Use --force."
    ))
}

async fn wait_until_stopped(instance: &AutoCadInstance) -> bool {
    let deadline = Instant::now() + EXIT_TIMEOUT;

    loop {
        if instance.has_exited() {
            return true;
        }

        if Instant::now() >= deadline {
            return false;
        }

        sleep(EXIT_POLL_INTERVAL).await;
    }
}
