pub mod close;
pub mod ls;
pub mod open;
pub mod save;
mod target;

use std::path::Path;
use std::process::ExitCode;

use acadctl_rpc::Document;
use tonic::transport::Channel;
use tonic::{Code, Status};

use crate::instances::QueryError;

type Client = acadctl_rpc::AcadctlClient<Channel>;

fn fail(message: String) -> ExitCode {
    eprintln!("acadctl: {message}");
    ExitCode::FAILURE
}

fn document_line(document: &Document, long: bool) -> String {
    let modified = if document.modified { "*" } else { "-" };
    let mode = if document.read_only { "r" } else { "w" };
    let name = if long {
        document.path.as_str()
    } else {
        Path::new(&document.path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&document.path)
    };
    format!("{}  {modified}  {mode}  {name}", document.id)
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

async fn connect(process_id: u32) -> Result<Client, String> {
    crate::instances::connect(process_id)
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
