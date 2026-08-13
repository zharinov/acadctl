mod transport;

use std::time::Duration;

use hyper_util::rt::TokioIo;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

pub use protocol::acadctl_client::AcadctlClient;
pub use protocol::acadctl_server::{Acadctl, AcadctlServer};
pub use protocol::{Document, ListRequest, ListResponse};
pub use transport::{Incoming, incoming};

mod protocol {
    tonic::include_proto!("acadctl");
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn connect(process_id: u32) -> Result<AcadctlClient<Channel>, tonic::transport::Error> {
    let channel = Endpoint::from_static("http://acadctl")
        .connect_timeout(CONNECT_TIMEOUT)
        .connect_with_connector(service_fn(move |_| async move {
            transport::connect(process_id).await.map(TokioIo::new)
        }))
        .await?;

    Ok(AcadctlClient::new(channel))
}
