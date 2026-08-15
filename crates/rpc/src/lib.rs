mod doc;
mod path;
mod pid;
mod request;
mod transport;

use std::time::Duration;

use hyper_util::rt::TokioIo;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

pub use doc::{DocId, ParseDocIdError};
pub use path::{DrawingPath, DrawingPathError};
pub use pid::{ParseProcessIdError, ProcessId};
pub use protocol::doc_service_client::DocServiceClient;
pub use protocol::doc_service_server::{DocService, DocServiceServer};
pub use protocol::exec_service_client::ExecServiceClient;
pub use protocol::exec_service_server::{ExecService, ExecServiceServer};
pub use protocol::{
    CloseRequest, CloseResponse, Doc, DrawingOutcome, ExecAccepted, ExecCancelAcknowledgement,
    ExecCancelDisposition, ExecCancelRequest, ExecCancelled, ExecClientMessage, ExecFailure,
    ExecFinished, ExecMode, ExecOutcome, ExecOutput, ExecRequest, ExecServerEvent, ExecSuccess,
    HistoryRequest, HistoryResponse, ListRequest, ListResponse, OpenRequest, OpenResponse,
    SaveRequest, SaveResponse, SourceLocation, exec_client_message, exec_outcome,
    exec_server_event,
};
pub use transport::{Incoming, discover, incoming};

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
    process_id: ProcessId,
) -> Result<DocServiceClient<Channel>, tonic::transport::Error> {
    Ok(DocServiceClient::new(connect_channel(process_id).await?)
        .max_encoding_message_size(MAX_DOCUMENT_REQUEST_BYTES)
        .max_decoding_message_size(MAX_DOCUMENT_RESPONSE_BYTES))
}

pub async fn connect_execution(
    process_id: ProcessId,
) -> Result<ExecServiceClient<Channel>, tonic::transport::Error> {
    Ok(ExecServiceClient::new(connect_channel(process_id).await?)
        .max_encoding_message_size(MAX_EXECUTION_REQUEST_BYTES)
        .max_decoding_message_size(MAX_EXECUTION_RESPONSE_BYTES))
}

async fn connect_channel(process_id: ProcessId) -> Result<Channel, tonic::transport::Error> {
    let channel = Endpoint::from_static("http://acadctl")
        .connect_timeout(CONNECT_TIMEOUT)
        .connect_with_connector(service_fn(move |_| async move {
            transport::connect(process_id).await.map(TokioIo::new)
        }))
        .await?;

    Ok(channel)
}
