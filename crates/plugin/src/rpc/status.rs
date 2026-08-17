use acadctl_rpc::{DrawingError, DrawingId};
use tonic::{Code, Status};

use crate::scheduler::Error as SchedulerError;

pub(super) fn parse_drawing_id(id: u32) -> Result<DrawingId, Status> {
    id.try_into()
        .map_err(|_| Status::invalid_argument("The drawing ID is invalid"))
}

pub(super) fn scheduler_error(error: SchedulerError) -> Status {
    let code = match &error {
        SchedulerError::DrawingNotFound(_) => Code::NotFound,
        SchedulerError::ReadinessTimedOut(_) => Code::DeadlineExceeded,
        _ if error.is_internal() => Code::Internal,
        _ => Code::FailedPrecondition,
    };

    if let Some(kind) = error.drawing_error_kind() {
        DrawingError {
            kind: kind as i32,
            drawing_id: error.drawing_id().map(Into::into),
        }
        .status(code)
    } else {
        Status::new(code, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use acadctl_rpc::DrawingErrorKind;

    use super::*;

    #[test]
    fn readiness_timeout_is_a_structured_deadline() {
        let id = "D0C0".parse().unwrap();
        let status = scheduler_error(SchedulerError::ReadinessTimedOut(Some(id)));

        assert_eq!(status.code(), Code::DeadlineExceeded);
        assert_eq!(status.message(), "drawing operation failed");
        assert_eq!(
            DrawingError::from_status(&status),
            Some(DrawingError {
                kind: DrawingErrorKind::ReadinessTimedOut as i32,
                drawing_id: Some(id.into()),
            })
        );
    }
}
