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
use target::Target;

type DrawingClient = acadctl_rpc::DrawingServiceClient<Channel>;

fn fail(message: String) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

fn query_error_message(error: &QueryError) -> String {
    match error {
        QueryError::CannotConnect => "Plugin unavailable. Install it and restart AutoCAD.".into(),
        QueryError::TimedOut => "Plugin does not respond within 5 seconds.".into(),
        QueryError::OutdatedPlugin => "Plugin incompatible. Update it and restart AutoCAD.".into(),
        QueryError::RequestFailed(message) if incompatible_message(message) => {
            "Plugin incompatible. Update it and restart AutoCAD.".into()
        }
        QueryError::RequestFailed(_) => "Unknown error.".into(),
    }
}

async fn connect_drawings(instance_id: acadctl_rpc::InstanceId) -> Result<DrawingClient, String> {
    crate::instance::connect_drawings(instance_id)
        .await
        .map_err(|error| query_error_message(&error))
}

fn request_error_message(operation: &str, target: Option<Target>, status: Status) -> String {
    if status.code() == Code::Unimplemented || incompatible_message(status.message()) {
        return "CLI and AutoCAD plugin are incompatible".into();
    }

    if let Some(error) = acadctl_rpc::DrawingError::from_status(&status)
        && let Ok(kind) = acadctl_rpc::DrawingErrorKind::try_from(error.kind)
        && kind != acadctl_rpc::DrawingErrorKind::Unspecified
    {
        if kind == acadctl_rpc::DrawingErrorKind::ReadinessTimedOut {
            return readiness_timeout_message(target);
        }

        if let Some(target) = target {
            return drawing_error_message(kind, target);
        }
    }

    format!("Could not {operation}")
}

fn drawing_error_message(kind: acadctl_rpc::DrawingErrorKind, target: Target) -> String {
    use acadctl_rpc::DrawingErrorKind;

    match kind {
        DrawingErrorKind::NotOpen => format!("Drawing {target} is not open"),
        DrawingErrorKind::Replaced => {
            format!("Drawing {target} changed before AutoCAD could perform the operation")
        }
        DrawingErrorKind::ReadOnly => format!("Drawing {target} is read-only"),
        DrawingErrorKind::UnsavedChanges => format!("Drawing {target} has unsaved changes"),
        DrawingErrorKind::NoFileName => {
            format!("Drawing {target} has no file name; use --as FILE")
        }
        DrawingErrorKind::Busy => format!("Drawing {target} is busy"),
        DrawingErrorKind::UndoDisabled => format!("Undo is disabled for drawing {target}"),
        DrawingErrorKind::DestinationExists => {
            "Destination already exists; use another path or omit --as".into()
        }
        DrawingErrorKind::ReadinessTimedOut => readiness_timeout_message(Some(target)),
        DrawingErrorKind::Unspecified => {
            unreachable!("unspecified drawing errors are not rendered")
        }
    }
}

fn readiness_timeout_message(target: Option<Target>) -> String {
    target.map_or_else(
        || "AutoCAD did not become ready within 60 seconds".into(),
        |target| format!("AutoCAD did not become ready for drawing {target} within 60 seconds"),
    )
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
                "Drawing 6A84:36C8 is not open",
            ),
            (
                acadctl_rpc::DrawingErrorKind::ReadOnly,
                "Drawing 6A84:36C8 is read-only",
            ),
            (
                acadctl_rpc::DrawingErrorKind::UnsavedChanges,
                "Drawing 6A84:36C8 has unsaved changes",
            ),
            (
                acadctl_rpc::DrawingErrorKind::Busy,
                "Drawing 6A84:36C8 is busy",
            ),
            (
                acadctl_rpc::DrawingErrorKind::ReadinessTimedOut,
                "AutoCAD did not become ready for drawing 6A84:36C8 within 60 seconds",
            ),
        ] {
            let status = acadctl_rpc::DrawingError {
                kind: kind as i32,
                drawing_id: Some(0x36C8),
            }
            .status(Code::FailedPrecondition);

            assert_eq!(
                request_error_message("save drawing 6A84:36C8", Some(target), status),
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
            request_error_message("open drawing", None, status),
            "AutoCAD did not become ready within 60 seconds"
        );
    }

    #[test]
    fn hides_transport_decoder_details() {
        assert_eq!(
            request_error_message(
                "save drawing 6A84:36C8",
                None,
                Status::unknown(
                    "failed to decode Protobuf message: Drawing.id: invalid wire type: Varint",
                ),
            ),
            "CLI and AutoCAD plugin are incompatible"
        );
    }
}
