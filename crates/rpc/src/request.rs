use bytes::Bytes;
use prost::Message;
use tonic::{Code, Status};

use crate::{
    CloseRequest, DrawingError, DrawingId, DrawingPath, ExecMode, ExecRequest, HistoryRequest,
    OpenRequest, SaveRequest, SourceName,
};

impl From<DrawingPath> for OpenRequest {
    fn from(path: DrawingPath) -> Self {
        Self {
            path: path.into_string(),
        }
    }
}

impl From<DrawingId> for SaveRequest {
    fn from(drawing_id: DrawingId) -> Self {
        Self {
            drawing_id: drawing_id.into(),
        }
    }
}

impl From<DrawingId> for HistoryRequest {
    fn from(drawing_id: DrawingId) -> Self {
        Self {
            drawing_id: drawing_id.into(),
        }
    }
}

impl CloseRequest {
    pub fn new(drawing_id: DrawingId, discard: bool) -> Self {
        Self {
            drawing_id: drawing_id.into(),
            discard,
        }
    }
}

impl ExecRequest {
    pub fn new(
        drawing_id: DrawingId,
        mode: ExecMode,
        source_name: SourceName,
        source: Bytes,
    ) -> Self {
        Self {
            drawing_id: drawing_id.into(),
            mode: mode as i32,
            source_name: source_name.into_string(),
            source,
        }
    }
}

impl DrawingError {
    pub fn status(&self, code: Code) -> Status {
        Status::with_details(
            code,
            "drawing operation failed",
            self.encode_to_vec().into(),
        )
    }

    pub fn from_status(status: &Status) -> Option<Self> {
        (!status.details().is_empty())
            .then(|| Self::decode(status.details()).ok())
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DrawingErrorKind;

    #[test]
    fn drawing_errors_round_trip_through_status_details() {
        let error = DrawingError {
            kind: DrawingErrorKind::ReadOnly as i32,
            drawing_id: Some(0x36C8),
        };
        let status = error.status(Code::FailedPrecondition);

        assert_eq!(DrawingError::from_status(&status), Some(error));
        assert_eq!(DrawingError::from_status(&Status::internal("failed")), None);
    }
}
