use acadctl_rpc::{
    CloseRequest, CloseResponse, Doc as RpcDoc, DocService, DrawingPathError, HistoryRequest,
    HistoryResponse, ListRequest, ListResponse, OpenRequest, OpenResponse, SaveRequest,
    SaveResponse,
};
use tonic::{Request, Response, Status};

use super::status::{parse_document_id, scheduler_error};

pub(super) struct DocRpc;

#[tonic::async_trait]
impl DocService for DocRpc {
    async fn list(&self, _request: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let documents = crate::scheduler::list()
            .map_err(scheduler_error)?
            .into_iter()
            .map(Into::into)
            .collect();
        Ok(Response::new(ListResponse { documents }))
    }

    async fn open(&self, request: Request<OpenRequest>) -> Result<Response<OpenResponse>, Status> {
        let path = request.into_inner().path;
        let path = path.parse().map_err(drawing_path_status)?;
        let document = crate::scheduler::open(path)
            .await
            .map_err(scheduler_error)?;
        Ok(Response::new(OpenResponse {
            document: Some(document.into()),
        }))
    }

    async fn save(&self, request: Request<SaveRequest>) -> Result<Response<SaveResponse>, Status> {
        let id = request.into_inner().id;
        let id = parse_document_id(id)?;
        let document = crate::scheduler::save(id).await.map_err(scheduler_error)?;
        Ok(Response::new(SaveResponse {
            document: Some(document.into()),
        }))
    }

    async fn undo(
        &self,
        request: Request<HistoryRequest>,
    ) -> Result<Response<HistoryResponse>, Status> {
        let id = request.into_inner().id;
        let id = parse_document_id(id)?;
        let document = crate::scheduler::undo(id).await.map_err(scheduler_error)?;
        Ok(Response::new(HistoryResponse {
            document: Some(document.into()),
        }))
    }

    async fn redo(
        &self,
        request: Request<HistoryRequest>,
    ) -> Result<Response<HistoryResponse>, Status> {
        let id = request.into_inner().id;
        let id = parse_document_id(id)?;
        let document = crate::scheduler::redo(id).await.map_err(scheduler_error)?;
        Ok(Response::new(HistoryResponse {
            document: Some(document.into()),
        }))
    }

    async fn close(
        &self,
        request: Request<CloseRequest>,
    ) -> Result<Response<CloseResponse>, Status> {
        let request = request.into_inner();
        let id = parse_document_id(request.id)?;
        crate::scheduler::close(id, request.discard)
            .await
            .map_err(scheduler_error)?;
        Ok(Response::new(CloseResponse {}))
    }
}

impl From<crate::doc::Doc> for RpcDoc {
    fn from(document: crate::doc::Doc) -> Self {
        Self {
            id: document.id.into(),
            display_name: document.display_name().to_owned(),
            file_path: document.file_path().map(|path| path.as_str().to_owned()),
            modified: document.modified,
            read_only: document.read_only,
        }
    }
}

fn drawing_path_status(error: DrawingPathError) -> Status {
    let message = match error {
        DrawingPathError::NotDwg => "Only DWG drawings can be opened",
        DrawingPathError::NotAbsolute => "The drawing path must be absolute",
        DrawingPathError::TooLong => "The drawing path exceeds the 32 KiB limit",
        DrawingPathError::NotFile(_)
        | DrawingPathError::Resolve { .. }
        | DrawingPathError::InvalidUtf8(_) => "The drawing path is invalid",
    };
    Status::invalid_argument(message)
}
