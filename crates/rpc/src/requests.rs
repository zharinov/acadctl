use bytes::Bytes;

use crate::{
    CloseRequest, DocumentId, DrawingPath, ExecutionMode, ExecutionRequest, HistoryRequest,
    OpenRequest, SaveRequest,
};

impl From<DrawingPath> for OpenRequest {
    fn from(path: DrawingPath) -> Self {
        Self {
            path: path.into_string(),
        }
    }
}

impl From<DocumentId> for SaveRequest {
    fn from(id: DocumentId) -> Self {
        Self { id: id.to_string() }
    }
}

impl From<DocumentId> for HistoryRequest {
    fn from(id: DocumentId) -> Self {
        Self { id: id.to_string() }
    }
}

impl CloseRequest {
    pub fn new(id: DocumentId, discard: bool) -> Self {
        Self {
            id: id.to_string(),
            discard,
        }
    }
}

impl ExecutionRequest {
    pub fn new(
        document_id: DocumentId,
        mode: ExecutionMode,
        source_name: String,
        source: Bytes,
    ) -> Self {
        Self {
            document_id: document_id.to_string(),
            mode: mode as i32,
            source_name,
            source,
        }
    }
}
