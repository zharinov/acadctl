mod transport;

use std::time::Duration;

use hyper_util::rt::TokioIo;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

pub use protocol::document_service_client::DocumentServiceClient;
pub use protocol::document_service_server::{DocumentService, DocumentServiceServer};
pub use protocol::execution_service_client::ExecutionServiceClient;
pub use protocol::execution_service_server::{ExecutionService, ExecutionServiceServer};
pub use protocol::{
    CloseRequest, CloseResponse, Document, DrawingOutcome, ExecutionAccepted,
    ExecutionCancelAcknowledgement, ExecutionCancelDisposition, ExecutionCancelRequest,
    ExecutionCancelled, ExecutionClientMessage, ExecutionFailure, ExecutionFinished, ExecutionMode,
    ExecutionOutcome, ExecutionOutput, ExecutionRequest, ExecutionServerEvent, ExecutionSuccess,
    HistoryRequest, HistoryResponse, ListRequest, ListResponse, OpenRequest, OpenResponse,
    SaveRequest, SaveResponse, SourceLocation, execution_client_message, execution_outcome,
    execution_server_event,
};
pub use transport::{Incoming, incoming};

mod protocol {
    tonic::include_proto!("acadctl");
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub const MAX_DRAWING_PATH_BYTES: usize = 32 * 1024;
pub const MAX_EXECUTION_SOURCE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SOURCE_NAME_BYTES: usize = 4 * 1024;
pub const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
pub const MAX_DOCUMENT_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_DOCUMENT_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_EXECUTION_REQUEST_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_EXECUTION_RESPONSE_BYTES: usize = 32 * 1024;
pub const MAX_SERVER_CONNECTIONS: usize = 9;
pub const MAX_STREAMS_PER_CONNECTION: u32 = 1;

pub async fn connect_documents(
    process_id: u32,
) -> Result<DocumentServiceClient<Channel>, tonic::transport::Error> {
    Ok(
        DocumentServiceClient::new(connect_channel(process_id).await?)
            .max_encoding_message_size(MAX_DOCUMENT_REQUEST_BYTES)
            .max_decoding_message_size(MAX_DOCUMENT_RESPONSE_BYTES),
    )
}

pub async fn connect_execution(
    process_id: u32,
) -> Result<ExecutionServiceClient<Channel>, tonic::transport::Error> {
    Ok(
        ExecutionServiceClient::new(connect_channel(process_id).await?)
            .max_encoding_message_size(MAX_EXECUTION_REQUEST_BYTES)
            .max_decoding_message_size(MAX_EXECUTION_RESPONSE_BYTES),
    )
}

async fn connect_channel(process_id: u32) -> Result<Channel, tonic::transport::Error> {
    let channel = Endpoint::from_static("http://acadctl")
        .connect_timeout(CONNECT_TIMEOUT)
        .connect_with_connector(service_fn(move |_| async move {
            transport::connect(process_id).await.map(TokioIo::new)
        }))
        .await?;

    Ok(channel)
}
