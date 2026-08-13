use std::process::ExitCode;

use acadctl_rpc::CloseRequest;

use super::{fail, request_error_message};

pub async fn run(id: String, discard: bool) -> ExitCode {
    let mut client = match super::target::connect(&id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };
    if let Err(status) = client
        .close(CloseRequest {
            id: id.clone(),
            discard,
        })
        .await
    {
        return fail(request_error_message("close the document", status));
    }
    println!("closed  {id}");
    ExitCode::SUCCESS
}
