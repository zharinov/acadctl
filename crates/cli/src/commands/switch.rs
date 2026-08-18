use std::process::ExitCode;

use acadctl_rpc::SwitchRequest;

use super::{RequestOperation, fail, request_error_message, target::Target};

pub async fn run(target: Target) -> ExitCode {
    let mut client = match super::connect_drawings(target.instance_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };

    let switched = match client.switch(SwitchRequest::from(target.drawing_id)).await {
        Ok(response) => response.into_inner(),
        Err(status) => {
            return fail(request_error_message(
                RequestOperation::Switch,
                Some(target),
                status,
            ));
        }
    };

    let Some(drawing) = switched.drawing else {
        return fail("Invalid response: active drawing is missing.".into());
    };

    if !drawing.active {
        return fail("Invalid response: drawing is not active.".into());
    }

    ExitCode::SUCCESS
}
