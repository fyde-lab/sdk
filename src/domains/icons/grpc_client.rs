use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::domains::sessions::{Service as SessionsService, SessionsClient};
use crate::{ErrorContext as _, ErrorKind, Result};

use super::Icon;

/// Generated protobuf/gRPC bindings for the `icons` service, compiled by
/// `build.rs` from `icons/v1/icons.proto` in the `buf.build/fyde-lab/api`
/// BSR module (sourced from `../server/protos`).
mod proto {
    tonic::include_proto!("icons.v1");
}

use proto::{
    CreateIconRequest, FetchIconRequest,
    icons_admin_service_client::IconsAdminServiceClient as GeneratedIconsAdminClient,
    icons_service_client::IconsServiceClient as GeneratedIconsClient,
};

/// A gRPC transport for the fyde server's icons service. Knows nothing
/// beyond translating between raw proto messages and the domain [`Icon`].
/// Abstracted as a trait so callers can be tested against
/// [`MockFydeClient`] instead of a live server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Stores `content` as an icon, or returns the existing icon with the
    /// same content, through the admin-only `IconsAdminService`.
    async fn create_icon(&self, content: Vec<u8>) -> Result<Icon>;

    /// Fetches the icon matching `id`.
    async fn fetch_icon(&self, id: Uuid) -> Result<Icon>;
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
    /// Creates a client for the icons service using the shared `channel`
    /// connection to the fyde server, authenticating every call via
    /// `sessions`.
    pub fn new(channel: Channel, sessions: Arc<SessionsClient>) -> Self {
        Self { channel, sessions }
    }

    /// Returns a generated client wrapping the shared connection.
    fn client(&self) -> GeneratedIconsClient<Channel> {
        GeneratedIconsClient::new(self.channel.clone())
    }

    /// Returns a generated client for the admin-only icons API, wrapping
    /// the shared connection.
    fn admin_client(&self) -> GeneratedIconsAdminClient<Channel> {
        GeneratedIconsAdminClient::new(self.channel.clone())
    }
}

#[async_trait]
impl FydeClient for GrpcClient {
    async fn create_icon(&self, content: Vec<u8>) -> Result<Icon> {
        let request = self
            .sessions
            .authenticated_request(CreateIconRequest { content })
            .await?;

        let response = self
            .sessions
            .check_response(self.admin_client().create_icon(request).await)
            .await
            .context("failed to create icon")?
            .into_inner();

        into_domain(response.icon.ok_or_else(|| {
            ErrorKind::InvalidResponse("create icon response had no icon".to_string())
        })?)
    }

    async fn fetch_icon(&self, id: Uuid) -> Result<Icon> {
        let request = self
            .sessions
            .authenticated_request(FetchIconRequest { id: id.to_string() })
            .await?;

        let response = self
            .sessions
            .check_response(self.client().fetch_icon(request).await)
            .await
            .with_context(|| format!("failed to fetch icon {id}"))?
            .into_inner();

        into_domain(response.icon.ok_or_else(|| {
            ErrorKind::InvalidResponse(format!("fetch icon {id} response had no icon"))
        })?)
    }
}

/// Converts a generated `proto::Icon` into the domain [`Icon`], parsing its
/// canonical UUID string id.
fn into_domain(icon: proto::Icon) -> Result<Icon> {
    Ok(Icon {
        id: icon
            .id
            .parse()
            .context("failed to parse icon id returned by the server")?,
        content: icon.content,
    })
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

        let err = grpc.fetch_icon(Uuid::new_v4()).await.unwrap_err();

        assert!(err.has_context());
    }

    #[test]
    fn into_domain_rejects_a_malformed_id() {
        let err = into_domain(proto::Icon {
            id: "not-a-uuid".to_string(),
            content: vec![1, 2, 3],
        })
        .unwrap_err();

        assert!(err.has_context());
    }
}
