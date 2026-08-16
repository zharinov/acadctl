use std::process::ExitCode;

use acadctl_rpc::HistoryRequest;

use super::{fail, request_error_message, target::Target};

#[derive(Clone, Copy)]
pub enum Direction {
    Undo,
    Redo,
}

pub async fn run(target: Target, direction: Direction) -> ExitCode {
    let mut client = match super::connect_drawings(target.instance_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };

    let response = match direction {
        Direction::Undo => client.undo(HistoryRequest::from(target.drawing_id)).await,
        Direction::Redo => client.redo(HistoryRequest::from(target.drawing_id)).await,
    };

    let response = match response {
        Ok(response) => response.into_inner(),
        Err(status) => {
            let operation = match direction {
                Direction::Undo => format!("undo the previous action in drawing {target}"),
                Direction::Redo => format!("redo the previous action in drawing {target}"),
            };

            return fail(request_error_message(&operation, Some(target), status));
        }
    };

    if response.drawing.is_none() {
        return fail("AutoCAD did not identify the updated drawing".into());
    }

    ExitCode::SUCCESS
}
