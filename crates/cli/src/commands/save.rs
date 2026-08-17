use std::path::PathBuf;
use std::process::ExitCode;

use acadctl_rpc::{SavePath, SaveRequest};

use super::{RequestOperation, fail, request_error_message, target::Target};

pub async fn run(target: Target, path: Option<PathBuf>) -> ExitCode {
    let path = match path.map(SavePath::prepare).transpose() {
        Ok(path) => path,
        Err(error) => return fail(error.to_string()),
    };

    let mut client = match super::connect_drawings(target.instance_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };

    let saved = match client.save(SaveRequest::new(target.drawing_id, path)).await {
        Ok(response) => response.into_inner(),
        Err(status) => {
            return fail(request_error_message(
                RequestOperation::Save,
                Some(target),
                status,
            ));
        }
    };

    if saved.drawing.is_none() {
        return fail("Invalid response: saved drawing is missing.".into());
    }

    ExitCode::SUCCESS
}
