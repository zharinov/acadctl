pub mod close;
pub mod exec;
pub mod history;
pub mod kill;
pub mod open;
pub mod ps;
pub mod save;
pub(crate) mod target;

use std::process::ExitCode;

use tonic::transport::Channel;
use tonic::{Code, Status};

use crate::instance::QueryError;

type DocClient = acadctl_rpc::DocServiceClient<Channel>;

fn fail(message: String) -> ExitCode {
    eprintln!("acadctl: {message}");
    ExitCode::FAILURE
}

fn query_error_message(error: &QueryError) -> String {
    match error {
        QueryError::CannotConnect => {
            "Could not connect to the acadctl plugin. Install it and restart AutoCAD.".into()
        }
        QueryError::TimedOut => {
            "AutoCAD did not respond within 5 seconds. Try again when it is idle.".into()
        }
        QueryError::OutdatedPlugin => {
            "The acadctl plugin is outdated. Install the current version and restart AutoCAD."
                .into()
        }
        QueryError::RequestFailed(message) if message.is_empty() => {
            "The acadctl plugin could not list documents.".into()
        }
        QueryError::RequestFailed(message) => {
            format!("Could not list AutoCAD documents: {message}")
        }
    }
}

async fn connect_documents(process_id: acadctl_rpc::ProcessId) -> Result<DocClient, String> {
    crate::instance::connect_documents(process_id)
        .await
        .map_err(|error| query_error_message(&error))
}

fn request_error_message(operation: &str, status: Status) -> String {
    if status.code() == Code::Unimplemented {
        return "The acadctl plugin is outdated. Install the current version and restart AutoCAD."
            .into();
    }

    if status.message().is_empty() {
        format!("AutoCAD could not {operation}.")
    } else {
        status.message().to_owned()
    }
}

fn parse_document_id(id: u32) -> Result<acadctl_rpc::DocId, String> {
    id.try_into()
        .map_err(|_| "AutoCAD returned an invalid document ID.".to_owned())
}
