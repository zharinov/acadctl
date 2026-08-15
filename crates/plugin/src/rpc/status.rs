use acadctl_rpc::DocId;
use tonic::Status;

use crate::scheduler::Error as SchedulerError;

pub(super) fn parse_document_id(id: &str) -> Result<DocId, Status> {
    id.parse()
        .map_err(|_| Status::invalid_argument("The document ID is invalid"))
}

pub(super) fn scheduler_error(error: SchedulerError) -> Status {
    if matches!(&error, SchedulerError::DocNotFound(_)) {
        Status::not_found(error.to_string())
    } else if error.is_internal() {
        Status::internal(error.to_string())
    } else {
        Status::failed_precondition(error.to_string())
    }
}
