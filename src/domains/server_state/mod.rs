mod grpc_client;
mod service;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;

use crate::Result;

pub use service::ServerStateClient;

/// Reachability checking against the fyde server, over the standard gRPC
/// health checking protocol (`grpc.health.v1.Health`,
/// https://github.com/grpc/grpc/blob/master/doc/health-checking.md) rather
/// than anything defined in `../api-protos`.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Returns whether the fyde server currently responds as reachable to
    /// a standard gRPC health check against its overall status (the
    /// well-known empty service name). Never fails: a transport failure
    /// (server down, unreachable, etc.) is reported as `Ok(false)` rather
    /// than propagated, since the whole point of this method is to test
    /// reachability without erroring the caller.
    async fn is_server_reachable(&self) -> Result<bool>;
}

/// Initializes the server_state service: checks reachability over the
/// shared `channel` connection to the fyde server.
pub(crate) fn init(channel: Channel) -> Arc<dyn Service> {
    Arc::new(ServerStateClient::new(channel))
}
