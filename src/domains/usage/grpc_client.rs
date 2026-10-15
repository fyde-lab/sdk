use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;

use crate::domains::sessions::{Service as SessionsService, SessionsClient};
use crate::{ErrorContext as _, ErrorKind, Result};

use super::Usage;

/// Generated protobuf/gRPC bindings for the `usage` service, compiled by
/// `build.rs` from `usage/v1/usage.proto` in the `buf.build/fyde-lab/api`
/// BSR module (sourced from `../api-protos`).
mod proto {
    tonic::include_proto!("usage.v1");
}

use proto::{GetUsageRequest, usage_service_client::UsageServiceClient as GeneratedUsageClient};

/// A gRPC transport for the fyde server's usage service. Knows nothing
/// beyond translating the raw proto response into the domain [`Usage`].
/// Abstracted as a trait so callers can be tested against
/// [`MockFydeClient`] instead of a live server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Fetches the authenticated account's plan, limits and usage.
    async fn get_usage(&self) -> Result<Usage>;
}

/// The production [`FydeClient`] implementation, backed by a tonic
/// [`Channel`] shared with every other domain's gRPC client (see
/// [`crate::Client::init`]). Every call is authenticated by attaching the
/// persisted session token (see
/// [`crate::domains::sessions::Service::authenticated_request`]), and its
/// outcome checked for a session the server rejected (see
/// [`crate::domains::sessions::Service::check_response`]).
pub(super) struct GrpcClient {
    channel: Channel,
    sessions: Arc<SessionsClient>,
}

impl GrpcClient {
    /// Creates a client for the usage service using the shared `channel`
    /// connection to the fyde server, authenticating every call via
    /// `sessions`.
    pub fn new(channel: Channel, sessions: Arc<SessionsClient>) -> Self {
        Self { channel, sessions }
    }

    /// Returns a generated client wrapping the shared connection.
    fn client(&self) -> GeneratedUsageClient<Channel> {
        GeneratedUsageClient::new(self.channel.clone())
    }
}

#[async_trait]
impl FydeClient for GrpcClient {
    async fn get_usage(&self) -> Result<Usage> {
        let request = self
            .sessions
            .authenticated_request(GetUsageRequest {})
            .await?;

        let response = self
            .sessions
            .check_response(self.client().get_usage(request).await)
            .await
            .context("failed to get account usage")?
            .into_inner();

        let limits = response.limits.ok_or_else(|| {
            ErrorKind::InvalidResponse("usage response is missing the plan limits".to_string())
        })?;
        let usage = response.usage.ok_or_else(|| {
            ErrorKind::InvalidResponse("usage response is missing the account usage".to_string())
        })?;

        Ok(Usage {
            plan_id: response.plan_id,
            max_scripts: limits.max_scripts,
            max_sessions: limits.max_sessions,
            max_size: limits.max_size,
            enabled_scripts: usage.enabled_scripts,
            open_sessions: usage.open_sessions,
            total_size: usage.total_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use tonic::transport::Endpoint;

    use super::*;
    use crate::domains::secrets::MockService as MockSecretsService;

    fn sessions() -> Arc<SessionsClient> {
        let mut secrets = MockSecretsService::new();
        secrets.expect_get().returning(|_| Ok(None));
        Arc::new(SessionsClient::new(Arc::new(secrets)))
    }

    #[tokio::test]
    async fn new_does_not_connect_to_the_server() {
        // A syntactically valid but unreachable address, connected lazily:
        // if `client()` dialed eagerly at construction, this would fail
        // here rather than on first use below.
        let channel = Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let grpc = GrpcClient::new(channel, sessions());

        let err = grpc.get_usage().await.unwrap_err();

        assert!(err.has_context());
    }
}
