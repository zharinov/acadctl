use acadctl_rpc::{
    CloseRequest, CloseResponse, Drawing as RpcDrawing, DrawingError, DrawingErrorKind,
    DrawingPathError, DrawingService, HistoryRequest, HistoryResponse, ListRequest, ListResponse,
    OpenRequest, OpenResponse, SavePath, SaveRequest, SaveResponse, ScreenshotRequest,
    ScreenshotResponse, SwitchRequest, SwitchResponse,
};
use tonic::{Request, Response, Status};

use super::status::{parse_drawing_id, scheduler_error};

pub(super) struct DrawingRpc;

static SCREENSHOT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

    async fn switch(
        &self,
        request: Request<SwitchRequest>,
    ) -> Result<Response<SwitchResponse>, Status> {
        let drawing_id = parse_drawing_id(request.into_inner().drawing_id)?;
        let drawing = crate::scheduler::switch(drawing_id)
            .await
            .map_err(scheduler_error)?;
        Ok(Response::new(SwitchResponse {
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
        let request = request.into_inner();
        let drawing_id = parse_drawing_id(request.drawing_id)?;
        let drawing = crate::scheduler::undo(drawing_id, request.force)
            .await
            .map_err(history_error)?;
        Ok(Response::new(HistoryResponse {
            drawing: Some(drawing.into()),
        }))
    }

    async fn redo(
        &self,
        request: Request<HistoryRequest>,
    ) -> Result<Response<HistoryResponse>, Status> {
        let request = request.into_inner();
        let drawing_id = parse_drawing_id(request.drawing_id)?;
        let drawing = crate::scheduler::redo(drawing_id, request.force)
            .await
            .map_err(history_error)?;
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

    async fn screenshot(
        &self,
        request: Request<ScreenshotRequest>,
    ) -> Result<Response<ScreenshotResponse>, Status> {
        let request = request.into_inner();
        let drawing_id = parse_drawing_id(request.drawing_id)?;
        let region = request
            .region
            .ok_or_else(|| Status::invalid_argument("A model-space WCS region is required"))?;
        let region = crate::scheduler::CaptureRegion {
            min_x: region.min_x,
            min_y: region.min_y,
            max_x: region.max_x,
            max_y: region.max_y,
        };
        if [region.min_x, region.min_y, region.max_x, region.max_y]
            .iter()
            .any(|coordinate| !coordinate.is_finite())
            || region.min_x >= region.max_x
            || region.min_y >= region.max_y
        {
            return Err(Status::invalid_argument(
                "Region coordinates must be finite and strictly ordered",
            ));
        }
        let resolution = if request.wide {
            crate::screenshot::ResolutionPolicy::Wide
        } else {
            crate::screenshot::ResolutionPolicy::Default
        };
        let max_long_edge = if request.wide { 1024 } else { 512 };
        let screenshot = SCREENSHOT.lock().await;
        let capture = crate::scheduler::capture(drawing_id, region, max_long_edge)
            .await
            .map_err(scheduler_error)?;
        let realistic_style = capture.realistic_style;
        let bounds = capture.bounds;
        let encoded = tokio::task::spawn_blocking(move || {
            let _screenshot = screenshot;
            crate::screenshot::encode_png(capture.frame(), bounds, resolution)
        })
        .await
        .map_err(|_| Status::internal("The viewport image processor stopped unexpectedly"))?
        .map_err(|_| Status::internal("The viewport image could not be processed"))?;

        let warnings = if realistic_style {
            vec![
                "Realistic visual style may capture only the viewport background on some drawings."
                    .into(),
            ]
        } else {
            vec![]
        };

        Ok(Response::new(ScreenshotResponse {
            png: encoded.png,
            width: encoded.width,
            height: encoded.height,
            warnings,
        }))
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
            active: drawing.active,
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

fn history_error(error: crate::scheduler::Error) -> Status {
    if matches!(
        error,
        crate::scheduler::Error::DocumentContextRestoreFailed(_)
    ) {
        DrawingError {
            kind: DrawingErrorKind::HistoryOutcomeUnknown as i32,
            drawing_id: None,
            detail: String::new(),
        }
        .status(tonic::Code::Internal)
    } else {
        scheduler_error(error)
    }
}
