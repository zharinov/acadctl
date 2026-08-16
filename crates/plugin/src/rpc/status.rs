use acadctl_rpc::{DrawingError, DrawingId};
use tonic::{Code, Status};

use crate::scheduler::Error as SchedulerError;

pub(super) fn parse_drawing_id(id: u32) -> Result<DrawingId, Status> {
    id.try_into()
        .map_err(|_| Status::invalid_argument("The drawing ID is invalid"))
}

pub(super) fn scheduler_error(error: SchedulerError) -> Status {
    let code = if matches!(&error, SchedulerError::DrawingNotFound(_)) {
        Code::NotFound
    } else if error.is_internal() {
        Code::Internal
    } else {
        Code::FailedPrecondition
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
