use bytes::Bytes;
use prost::Message;
use tonic::{Code, Status};

use crate::{
    CloseRequest, DrawingError, DrawingId, DrawingPath, ExecMode, ExecRequest, HistoryRequest,
    OpenRequest, SavePath, SaveRequest, ScreenshotRegion, ScreenshotRequest, SourceName,
    SwitchRequest,
};

impl From<DrawingPath> for OpenRequest {
    fn from(path: DrawingPath) -> Self {
        Self {
            path: path.into_string(),
        }
    }
}

impl SaveRequest {
    pub fn new(drawing_id: DrawingId, path: Option<SavePath>) -> Self {
        Self {
            drawing_id: drawing_id.into(),
            path: path.map(SavePath::into_string),
        }
    }
}

impl HistoryRequest {
    pub fn new(drawing_id: DrawingId, force: bool) -> Self {
        Self {
            drawing_id: drawing_id.into(),
            force,
        }
    }
}

impl From<DrawingId> for SwitchRequest {
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

impl ScreenshotRequest {
    pub fn new(drawing_id: DrawingId, region: ScreenshotRegion, wide: bool) -> Self {
        Self {
            drawing_id: drawing_id.into(),
            region: Some(region),
            wide,
        }
    }
}

impl ExecRequest {
    pub fn new(
        drawing_id: DrawingId,
        mode: ExecMode,
        source_name: SourceName,
        source: Bytes,
        force: bool,
    ) -> Self {
        Self {
            drawing_id: drawing_id.into(),
            mode: mode as i32,
            source_name: source_name.into_string(),
            source,
            force,
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
            detail: String::new(),
        };
        let status = error.status(Code::FailedPrecondition);

        assert_eq!(DrawingError::from_status(&status), Some(error));
        assert_eq!(DrawingError::from_status(&Status::internal("failed")), None);
    }

    #[test]
    fn screenshot_request_contains_the_required_region_and_size_policy() {
        let drawing_id = "36C8".parse().unwrap();
        let region = ScreenshotRegion {
            min_x: -100.0,
            min_y: -25.0,
            max_x: 10.0,
            max_y: 20.0,
        };

        assert_eq!(
            ScreenshotRequest::new(drawing_id, region, true),
            ScreenshotRequest {
                drawing_id: drawing_id.into(),
                region: Some(region),
                wide: true,
            }
        );
    }
}
