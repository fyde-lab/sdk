use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::domains::sessions::{Service as SessionsService, SessionsClient};
use crate::{ErrorContext as _, Result};

/// Generated protobuf/gRPC bindings for the `changelog` service, compiled
/// from `../api-protos/changelog/v1/changelog.proto` by `build.rs`.
mod proto {
    tonic::include_proto!("changelog.v1");
}

pub(super) use proto::ConsumeSinceResponse as ChangelogEvent;

use proto::changelog_service_client::ChangelogServiceClient as ChangelogClient;
use proto::{ConsumeSinceRequest, RecordEventRequest};

/// A stream of raw changelog events as received from the server, opened by
/// [`FydeClient::consume_since`].
pub(super) type EventStream =
    BoxStream<'static, std::result::Result<ChangelogEvent, tonic::Status>>;

/// A gRPC transport for talking to the fyde server's changelog service.
/// Knows nothing about changelog events themselves beyond the raw proto
/// types; just sends/receives raw messages. Abstracted as a trait so
/// callers can be tested against [`MockFydeClient`] instead of a live
/// server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Submits a new, already-encrypted event, returning its assigned id.
    async fn record_event(&self, encrypted_content: Vec<u8>) -> Result<Uuid>;

    /// Streams every entry with an id strictly greater than `id`,
    /// oldest first: replays persisted history, then continues with the
    /// live tail. `None` means "from the beginning of the changelog".
    async fn consume_since(&self, id: Option<Uuid>) -> Result<EventStream>;
}

/// The production [`FydeClient`] implementation, backed by a tonic
/// [`Channel`] shared with every other domain's gRPC client (see
/// [`crate::Client::connect`]), so they all reuse the same underlying
/// connection instead of each opening one of their own. The channel is
/// typically opened lazily (via `Endpoint::connect_lazy`), so the SDK can be
/// used offline for anything that doesn't reach this client; a call that
/// does need the server surfaces a connection failure as
/// [`crate::Error::GrpcTransport`]. Cloning a [`Channel`] is cheap — it's
/// just a handle to the same underlying connection — so a fresh generated
/// client is created per call. Every call is authenticated by attaching the
/// session token currently persisted in the secrets store (if any) as a bearer
/// `authorization` header (see
/// [`crate::domains::sessions::Service::authenticated_request`]).
pub(super) struct GrpcClient {
    channel: Channel,
    sessions: Arc<SessionsClient>,
}

impl GrpcClient {
    /// Creates a client for the changelog service using the shared
    /// `channel` connection to the fyde server, authenticating every call
    /// via `sessions`.
    pub fn new(channel: Channel, sessions: Arc<SessionsClient>) -> Self {
        Self { channel, sessions }
    }

    /// Returns a generated client wrapping the shared connection.
    fn client(&self) -> ChangelogClient<Channel> {
        ChangelogClient::new(self.channel.clone())
    }
}

#[async_trait]
impl FydeClient for GrpcClient {
    async fn record_event(&self, encrypted_content: Vec<u8>) -> Result<Uuid> {
        let request = self
            .sessions
            .authenticated_request(RecordEventRequest { encrypted_content })
            .await?;

        let response = self
            .client()
            .record_event(request)
            .await
            .context("failed to record changelog event")?
            .into_inner();

        Uuid::parse_str(&response.id).with_context(|| {
            format!(
                "invalid changelog event id {:?} returned by server",
                response.id
            )
        })
    }

    async fn consume_since(&self, id: Option<Uuid>) -> Result<EventStream> {
        let request = self
            .sessions
            .authenticated_request(ConsumeSinceRequest {
                id: id.map(|id| id.to_string()).unwrap_or_default(),
            })
            .await?;

        let response = self
            .client()
            .consume_since(request)
            .await
            .with_context(|| format!("failed to consume changelog events since id {id:?}"))?;
        Ok(Box::pin(response.into_inner()))
    }
}

#[cfg(test)]
mod tests {
    use tonic::transport::Endpoint;

    use super::*;
    use crate::Error;
    use crate::domains::secrets::MockService as MockSecretsService;

    #[tokio::test]
    async fn new_does_not_connect_to_the_server() {
        // A syntactically valid but unreachable address, connected lazily:
        // if `client()` dialed eagerly at construction, this would fail
        // here rather than on first use below.
        let mut secrets = MockSecretsService::new();
        secrets.expect_get().returning(|_| Ok(None));
        let sessions = Arc::new(SessionsClient::new(Arc::new(secrets)));
        let channel = Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let grpc = GrpcClient::new(channel, sessions);

        let err = grpc.record_event(vec![1, 2, 3]).await.unwrap_err();

        assert!(matches!(err, Error::Context { .. }));
    }
}
