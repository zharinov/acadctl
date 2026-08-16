use acadctl_rpc::{
    CloseRequest, CloseResponse, Drawing as RpcDrawing, DrawingPathError, DrawingService,
    HistoryRequest, HistoryResponse, ListRequest, ListResponse, OpenRequest, OpenResponse,
    SavePath, SaveRequest, SaveResponse,
};
use tonic::{Request, Response, Status};

use super::status::{parse_drawing_id, scheduler_error};

pub(super) struct DrawingRpc;

#[tonic::async_trait]
impl DrawingService for DrawingRpc {
    async fn list(&self, _request: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let drawings = crate::scheduler::list()
            .map_err(scheduler_error)?
            .into_iter()
            .map(Into::into)
            .collect();
        Ok(Response::new(ListResponse { drawings }))
    }

    async fn open(&self, request: Request<OpenRequest>) -> Result<Response<OpenResponse>, Status> {
        let path = request.into_inner().path;
        let path = path.parse().map_err(drawing_path_status)?;
        let drawing = crate::scheduler::open(path)
            .await
            .map_err(scheduler_error)?;
        Ok(Response::new(OpenResponse {
            drawing: Some(drawing.into()),
        }))
    }

    async fn save(&self, request: Request<SaveRequest>) -> Result<Response<SaveResponse>, Status> {
        let request = request.into_inner();
        let drawing_id = parse_drawing_id(request.drawing_id)?;
        let path = request
            .path
            .map(|path| path.parse::<SavePath>())
            .transpose()
            .map_err(drawing_path_status)?;
        let drawing = crate::scheduler::save(drawing_id, path)
            .await
            .map_err(scheduler_error)?;
        Ok(Response::new(SaveResponse {
            drawing: Some(drawing.into()),
        }))
    }

    async fn undo(
        &self,
        request: Request<HistoryRequest>,
    ) -> Result<Response<HistoryResponse>, Status> {
        let drawing_id = parse_drawing_id(request.into_inner().drawing_id)?;
        let drawing = crate::scheduler::undo(drawing_id)
            .await
            .map_err(scheduler_error)?;
        Ok(Response::new(HistoryResponse {
            drawing: Some(drawing.into()),
        }))
    }

    async fn redo(
        &self,
        request: Request<HistoryRequest>,
    ) -> Result<Response<HistoryResponse>, Status> {
        let drawing_id = parse_drawing_id(request.into_inner().drawing_id)?;
        let drawing = crate::scheduler::redo(drawing_id)
            .await
            .map_err(scheduler_error)?;
        Ok(Response::new(HistoryResponse {
            drawing: Some(drawing.into()),
        }))
    }

    async fn close(
        &self,
        request: Request<CloseRequest>,
    ) -> Result<Response<CloseResponse>, Status> {
        let request = request.into_inner();
        let drawing_id = parse_drawing_id(request.drawing_id)?;
        crate::scheduler::close(drawing_id, request.discard)
            .await
            .map_err(scheduler_error)?;
        Ok(Response::new(CloseResponse {}))
    }
}

impl From<crate::drawing::Drawing> for RpcDrawing {
    fn from(drawing: crate::drawing::Drawing) -> Self {
        Self {
            id: drawing.id.into(),
            display_name: drawing.display_name().to_owned(),
            file_path: drawing.file_path().map(|path| path.as_str().to_owned()),
            modified: drawing.modified,
            read_only: drawing.read_only,
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
        | DrawingPathError::InvalidUtf8(_)
        | DrawingPathError::AlreadyExists(_) => "The drawing path is invalid",
    };
    Status::invalid_argument(message)
}
