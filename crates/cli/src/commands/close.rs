use std::process::ExitCode;

use acadctl_rpc::CloseRequest;

use super::{fail, request_error_message, target::Target};

pub async fn run(target: Target, discard: bool) -> ExitCode {
    let mut client = match super::connect_documents(target.process_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };

    if let Err(status) = client
        .close(CloseRequest::new(target.document_id, discard))
        .await
    {
        return fail(request_error_message("close the document", status));
    }

    ExitCode::SUCCESS
}
