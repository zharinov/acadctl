use std::process::ExitCode;

use acadctl_rpc::CloseRequest;

use super::{fail, request_error_message};

pub async fn run(id: String, discard: bool) -> ExitCode {
    let target = match super::target::resolve(&id) {
        Ok(target) => target,
        Err(error) => return fail(error),
    };

    let mut client = match super::connect_documents(target.process_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };

    if let Err(status) = client
        .close(CloseRequest {
            id: target.document_id,
            discard,
        })
        .await
    {
        return fail(request_error_message("close the document", status));
    }

    ExitCode::SUCCESS
}
