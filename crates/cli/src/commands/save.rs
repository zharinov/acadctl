use std::process::ExitCode;

use acadctl_rpc::SaveRequest;

use super::{fail, request_error_message};

pub async fn run(id: String) -> ExitCode {
    let mut client = match super::target::connect(&id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };
    let saved = match client.save(SaveRequest { id }).await {
        Ok(response) => response.into_inner(),
        Err(status) => return fail(request_error_message("save the document", status)),
    };
    if saved.document.is_none() {
        return fail("AutoCAD did not identify the saved document.".into());
    }
    ExitCode::SUCCESS
}
