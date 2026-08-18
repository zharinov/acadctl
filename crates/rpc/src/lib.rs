mod drawing;
mod instance;
mod path;
mod request;
mod source;
mod transport;

use std::time::Duration;

use hyper_util::rt::TokioIo;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

pub use drawing::{DrawingId, ParseDrawingIdError};
pub use instance::{InstanceId, ParseInstanceIdError};
pub use path::{DrawingPath, DrawingPathError, SavePath};
pub use protocol::drawing_service_client::DrawingServiceClient;
pub use protocol::drawing_service_server::{DrawingService, DrawingServiceServer};
pub use protocol::exec_service_client::ExecServiceClient;
pub use protocol::exec_service_server::{ExecService, ExecServiceServer};
pub use protocol::{
    CloseRequest, CloseResponse, Drawing, DrawingError, DrawingErrorKind, DrawingOutcome,
    ExecAccepted, ExecCancelAcknowledgement, ExecCancelDisposition, ExecCancelRequest,
    ExecCancelled, ExecClientMessage, ExecFailure, ExecFinished, ExecMode, ExecOutcome, ExecOutput,
    ExecRequest, ExecServerEvent, ExecSuccess, HistoryRequest, HistoryResponse, ListRequest,
    ListResponse, OpenRequest, OpenResponse, SaveRequest, SaveResponse, SourceLocation,
    SwitchRequest, SwitchResponse, exec_client_message, exec_outcome, exec_server_event,
};
pub use source::{SourceName, SourceNameError};
pub use transport::{Incoming, incoming};

#[allow(
    clippy::all,
    clippy::allow_attributes_without_reason,
    reason = "tonic and prost generate this module"
)]
mod protocol {
    tonic::include_proto!("acadctl");
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub const MAX_DRAWING_PATH_BYTES: usize = 32 * 1024;
pub const MAX_EXECUTION_SOURCE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SOURCE_NAME_BYTES: usize = 4 * 1024;
pub const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
pub const MAX_DRAWING_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_DRAWING_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_EXECUTION_REQUEST_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_EXECUTION_RESPONSE_BYTES: usize = 32 * 1024;
pub const MAX_SERVER_CONNECTIONS: usize = 9;
pub const MAX_STREAMS_PER_CONNECTION: u32 = 1;

pub async fn connect_drawings(
    instance_id: InstanceId,
) -> Result<DrawingServiceClient<Channel>, tonic::transport::Error> {
    Ok(
        DrawingServiceClient::new(connect_channel(instance_id).await?)
            .max_encoding_message_size(MAX_DRAWING_REQUEST_BYTES)
            .max_decoding_message_size(MAX_DRAWING_RESPONSE_BYTES),
    )
}

pub async fn connect_execution(
    instance_id: InstanceId,
) -> Result<ExecServiceClient<Channel>, tonic::transport::Error> {
    Ok(ExecServiceClient::new(connect_channel(instance_id).await?)
        .max_encoding_message_size(MAX_EXECUTION_REQUEST_BYTES)
        .max_decoding_message_size(MAX_EXECUTION_RESPONSE_BYTES))
}

async fn connect_channel(instance_id: InstanceId) -> Result<Channel, tonic::transport::Error> {
    let channel = Endpoint::from_static("http://acadctl")
        .connect_timeout(CONNECT_TIMEOUT)
        .connect_with_connector(service_fn(move |_| async move {
            transport::connect(instance_id).await.map(TokioIo::new)
        }))
        .await?;

    Ok(channel)
}
