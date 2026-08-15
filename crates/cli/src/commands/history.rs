use std::process::ExitCode;

use acadctl_rpc::HistoryRequest;

use super::{fail, request_error_message};

#[derive(Clone, Copy)]
pub enum Direction {
    Undo,
    Redo,
}

pub async fn run(id: String, direction: Direction) -> ExitCode {
    let process_id = match super::target::resolve_process_id(&id).await {
        Ok(process_id) => process_id,
        Err(error) => return fail(error),
    };
    let mut client = match super::connect_documents(process_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };
    let response = match direction {
        Direction::Undo => client.undo(HistoryRequest { id }).await,
        Direction::Redo => client.redo(HistoryRequest { id }).await,
    };
    let response = match response {
        Ok(response) => response.into_inner(),
        Err(status) => {
            let operation = match direction {
                Direction::Undo => "undo the drawing's last history step",
                Direction::Redo => "redo the drawing's next history step",
            };
            return fail(request_error_message(operation, status));
        }
    };
    if response.document.is_none() {
        return fail("AutoCAD did not identify the updated document.".into());
    }
    ExitCode::SUCCESS
}
