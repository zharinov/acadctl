use std::process::ExitCode;

use acadctl_rpc::SaveRequest;

use super::{fail, request_error_message};

pub async fn run(id: String) -> ExitCode {
    let target = match super::target::resolve(&id) {
        Ok(target) => target,
        Err(error) => return fail(error),
    };

    let mut client = match super::connect_documents(target.process_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };

    let saved = match client.save(SaveRequest::from(target.document_id)).await {
        Ok(response) => response.into_inner(),
        Err(status) => return fail(request_error_message("save the document", status)),
    };

    if saved.document.is_none() {
        return fail("AutoCAD did not identify the saved document.".into());
    }

    ExitCode::SUCCESS
}
