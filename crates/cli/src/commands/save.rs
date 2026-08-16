use std::process::ExitCode;

use acadctl_rpc::SaveRequest;

use super::{fail, request_error_message, target::Target};

pub async fn run(target: Target) -> ExitCode {
    let mut client = match super::connect_drawings(target.instance_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };

    let saved = match client.save(SaveRequest::from(target.drawing_id)).await {
        Ok(response) => response.into_inner(),
        Err(status) => {
            return fail(request_error_message(
                &format!("save drawing {target}"),
                Some(target),
                status,
            ));
        }
    };

    if saved.drawing.is_none() {
        return fail("AutoCAD did not identify the saved drawing".into());
    }

    ExitCode::SUCCESS
}
