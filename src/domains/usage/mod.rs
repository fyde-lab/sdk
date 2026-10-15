mod grpc_client;
mod models;
mod service;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;

use crate::Result;
use crate::domains::sessions::SessionsClient;

#[cfg(test)]
pub(crate) use models::FakeUsage;
pub use models::Usage;
pub use service::UsageClient;

/// The plan the signed-in account is on, and how much of each of its
/// limits the account currently uses — over the server's
/// `usage.v1.UsageService`.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Fetches the signed-in account's plan, limits and current usage.
    /// Always asks the server: nothing is cached locally, since every
    /// device of the account changes these figures.
    async fn get_usage(&self) -> Result<Usage>;
}

/// Initializes the usage service over the shared `channel` connection to
/// the fyde server, authenticating every call via `sessions`.
pub(crate) fn init(channel: Channel, sessions: Arc<SessionsClient>) -> Arc<dyn Service> {
    Arc::new(UsageClient::new(grpc_client::GrpcClient::new(
        channel, sessions,
    )))
}
