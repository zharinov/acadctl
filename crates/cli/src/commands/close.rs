use std::process::ExitCode;

use acadctl_rpc::CloseRequest;

use super::{fail, request_error_message};

pub async fn run(id: String, discard: bool) -> ExitCode {
    let process_id = match super::target::resolve_process_id(&id).await {
        Ok(process_id) => process_id,
        Err(error) => return fail(error),
    };
    let mut client = match super::connect_documents(process_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };
    if let Err(status) = client.close(CloseRequest { id, discard }).await {
        return fail(request_error_message("close the document", status));
    }
    ExitCode::SUCCESS
}
