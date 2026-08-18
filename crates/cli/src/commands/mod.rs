pub mod close;
pub mod exec;
pub mod history;
pub mod list;
pub mod open;
pub mod quit;
pub mod save;
pub(crate) mod target;

use std::process::ExitCode;

use tonic::transport::Channel;
use tonic::{Code, Status};

use crate::instance::QueryError;
use target::Target;

type DrawingClient = acadctl_rpc::DrawingServiceClient<Channel>;

#[derive(Clone, Copy)]
enum RequestOperation {
    Open,
    Save,
    Close,
    Undo,
    Redo,
}

fn fail(message: String) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

fn query_error_message(error: &QueryError) -> String {
    match error {
        QueryError::CannotConnect => "Plugin unavailable: install it and restart AutoCAD.".into(),
        QueryError::TimedOut => "Timeout: plugin did not respond.".into(),
        QueryError::OutdatedPlugin => "Plugin incompatible: update it and restart AutoCAD.".into(),
        QueryError::RequestFailed(message) if incompatible_message(message) => {
            "Plugin incompatible: update it and restart AutoCAD.".into()
        }
        QueryError::RequestFailed(_) => "Unknown error.".into(),
    }
}

async fn connect_drawings(instance_id: acadctl_rpc::InstanceId) -> Result<DrawingClient, String> {
    crate::instance::connect_drawings(instance_id)
        .await
        .map_err(|error| query_error_message(&error))
}

fn request_error_message(
    operation: RequestOperation,
    target: Option<Target>,
    status: Status,
) -> String {
    if status.code() == Code::Unimplemented || incompatible_message(status.message()) {
        return "Plugin incompatible: update it and restart AutoCAD.".into();
    }

    if let Some(error) = acadctl_rpc::DrawingError::from_status(&status)
        && let Ok(kind) = acadctl_rpc::DrawingErrorKind::try_from(error.kind)
        && kind != acadctl_rpc::DrawingErrorKind::Unspecified
    {
        if kind == acadctl_rpc::DrawingErrorKind::ReadinessTimedOut {
            return operation.timeout_message(target);
        }

        if let Some(target) = target {
            return drawing_error_message(kind, target);
        }
    }

    operation.failure_message(target)
}

fn drawing_error_message(kind: acadctl_rpc::DrawingErrorKind, target: Target) -> String {
    use acadctl_rpc::DrawingErrorKind;

    match kind {
        DrawingErrorKind::NotOpen => format!("Drawing {target} is not open."),
        DrawingErrorKind::Replaced => {
            format!("Drawing {target} changed before the operation.")
        }
        DrawingErrorKind::ReadOnly => format!("Drawing {target} is read-only."),
        DrawingErrorKind::UnsavedChanges => format!("Drawing {target} has unsaved changes."),
        DrawingErrorKind::NoFileName => {
            format!("Drawing {target} has no filename: use --as FILE.")
        }
        DrawingErrorKind::Busy => format!("Drawing {target} is busy."),
        DrawingErrorKind::UndoDisabled => format!("Undo is disabled for drawing {target}."),
        DrawingErrorKind::DestinationExists => "Destination exists: choose another path.".into(),
        DrawingErrorKind::ReadinessTimedOut => {
            unreachable!("readiness timeouts require operation context")
        }
        DrawingErrorKind::Unspecified => {
            unreachable!("unspecified drawing errors are not rendered")
        }
    }
}

impl RequestOperation {
    fn failure_message(self, target: Option<Target>) -> String {
        match (self, target) {
            (Self::Open, _) => "Drawing was not opened.".into(),
            (Self::Save, Some(target)) => format!("Drawing {target} was not saved."),
            (Self::Close, Some(target)) => format!("Drawing {target} was not closed."),
            (Self::Undo, Some(target)) => format!("Undo was not run for drawing {target}."),
            (Self::Redo, Some(target)) => format!("Redo was not run for drawing {target}."),
            _ => "Operation failed.".into(),
        }
    }

    fn timeout_message(self, target: Option<Target>) -> String {
        match (self, target) {
            (Self::Open, _) => "Timeout: drawing was not opened.".into(),
            (Self::Save, Some(target)) => format!("Timeout: drawing {target} was not saved."),
            (Self::Close, Some(target)) => format!("Timeout: drawing {target} was not closed."),
            (Self::Undo, Some(target)) => {
                format!("Timeout: undo was not run for drawing {target}.")
            }
            (Self::Redo, Some(target)) => {
                format!("Timeout: redo was not run for drawing {target}.")
            }
            _ => "Timeout: operation failed.".into(),
        }
    }
}

fn parse_drawing_id(id: u32) -> Result<acadctl_rpc::DrawingId, String> {
    id.try_into()
        .map_err(|_| "AutoCAD returned an invalid drawing identifier".to_owned())
}

fn incompatible_message(message: &str) -> bool {
    message.contains("failed to decode Protobuf message")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_plugin_failures_with_the_complete_target() {
        let target = "6A84:36C8".parse().unwrap();

        for (kind, expected) in [
            (
                acadctl_rpc::DrawingErrorKind::NotOpen,
                "Drawing 6A84:36C8 is not open.",
            ),
            (
                acadctl_rpc::DrawingErrorKind::ReadOnly,
                "Drawing 6A84:36C8 is read-only.",
            ),
            (
                acadctl_rpc::DrawingErrorKind::UnsavedChanges,
                "Drawing 6A84:36C8 has unsaved changes.",
            ),
            (
                acadctl_rpc::DrawingErrorKind::Busy,
                "Drawing 6A84:36C8 is busy.",
            ),
            (
                acadctl_rpc::DrawingErrorKind::ReadinessTimedOut,
                "Timeout: drawing 6A84:36C8 was not saved.",
            ),
        ] {
            let status = acadctl_rpc::DrawingError {
                kind: kind as i32,
                drawing_id: Some(0x36C8),
            }
            .status(Code::FailedPrecondition);

            assert_eq!(
                request_error_message(RequestOperation::Save, Some(target), status),
                expected
            );
        }
    }

    #[test]
    fn translates_targetless_open_readiness_timeout() {
        let status = acadctl_rpc::DrawingError {
            kind: acadctl_rpc::DrawingErrorKind::ReadinessTimedOut as i32,
            drawing_id: None,
        }
        .status(Code::DeadlineExceeded);

        assert_eq!(
            request_error_message(RequestOperation::Open, None, status),
            "Timeout: drawing was not opened."
        );
    }

    #[test]
    fn hides_transport_decoder_details() {
        assert_eq!(
            request_error_message(
                RequestOperation::Save,
                None,
                Status::unknown(
                    "failed to decode Protobuf message: Drawing.id: invalid wire type: Varint",
                ),
            ),
            "Plugin incompatible: update it and restart AutoCAD."
        );
    }
}
