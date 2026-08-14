mod transport;

use std::time::Duration;

use hyper_util::rt::TokioIo;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

pub use protocol::acadctl_client::AcadctlClient;
pub use protocol::acadctl_server::{Acadctl, AcadctlServer};
pub use protocol::executor_client::ExecutorClient;
pub use protocol::executor_server::{Executor, ExecutorServer};
pub use protocol::{
    CloseRequest, CloseResponse, Document, DrawingOutcome, ExecuteClientMessage,
    ExecuteServerEvent, ExecutionAccepted, ExecutionCancel, ExecutionCancellation,
    ExecutionCancellationResult, ExecutionCancelled, ExecutionFailure, ExecutionFinished,
    ExecutionMode, ExecutionOutcome, ExecutionOutput, ExecutionRequest, ExecutionSuccess,
    ListRequest, ListResponse, OpenRequest, OpenResponse, SaveRequest, SaveResponse,
    SourceLocation, execute_client_message, execute_server_event, execution_outcome,
};
pub use transport::{Incoming, incoming};

mod protocol {
    tonic::include_proto!("acadctl");
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub const MAX_PATH_BYTES: usize = 32 * 1024;
pub const MAX_SOURCE_NAME_BYTES: usize = 4 * 1024;
pub const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
pub const MAX_CONTROL_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_CONTROL_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_EXECUTE_MESSAGE_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_EXECUTE_RESPONSE_BYTES: usize = 32 * 1024;
pub const MAX_SERVER_CONNECTIONS: usize = 9;
pub const MAX_STREAMS_PER_CONNECTION: u32 = 1;

pub async fn connect(process_id: u32) -> Result<AcadctlClient<Channel>, tonic::transport::Error> {
    Ok(AcadctlClient::new(connect_channel(process_id).await?)
        .max_encoding_message_size(MAX_CONTROL_MESSAGE_BYTES)
        .max_decoding_message_size(MAX_CONTROL_RESPONSE_BYTES))
}

pub async fn connect_executor(
    process_id: u32,
) -> Result<ExecutorClient<Channel>, tonic::transport::Error> {
    Ok(ExecutorClient::new(connect_channel(process_id).await?)
        .max_encoding_message_size(MAX_EXECUTE_MESSAGE_BYTES)
        .max_decoding_message_size(MAX_EXECUTE_RESPONSE_BYTES))
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
