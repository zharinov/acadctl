use bytes::Bytes;

use crate::{
    CloseRequest, DocId, DrawingPath, ExecMode, ExecRequest, HistoryRequest, OpenRequest,
    SaveRequest, SourceName,
};

impl From<DrawingPath> for OpenRequest {
    fn from(path: DrawingPath) -> Self {
        Self {
            path: path.into_string(),
        }
    }
}

impl From<DocId> for SaveRequest {
    fn from(id: DocId) -> Self {
        Self { id: id.into() }
    }
}

impl From<DocId> for HistoryRequest {
    fn from(id: DocId) -> Self {
        Self { id: id.into() }
    }
}

impl CloseRequest {
    pub fn new(id: DocId, discard: bool) -> Self {
        Self {
            id: id.into(),
            discard,
        }
    }
}

impl ExecRequest {
    pub fn new(document_id: DocId, mode: ExecMode, source_name: SourceName, source: Bytes) -> Self {
        Self {
            document_id: document_id.into(),
            mode: mode as i32,
            source_name: source_name.into_string(),
            source,
        }
    }
}
