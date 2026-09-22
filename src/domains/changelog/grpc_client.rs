use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;

use crate::domains::sessions::{Service as SessionsService, SessionsClient};
use crate::{Error, ErrorContext as _, Result};

/// Generated protobuf/gRPC bindings for the `changelog` service, compiled
/// from `../api-protos/changelog.proto` by `build.rs`.
mod proto {
    tonic::include_proto!("changelog");
}

pub(super) use proto::ChangelogEvent;

use proto::changelog_client::ChangelogClient;
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
    /// Submits a new, already-encrypted event, returning its assigned
    /// offset.
    async fn record_event(&self, encrypted_content: Vec<u8>) -> Result<i64>;

    /// Streams every entry from `offset` onward, oldest first: replays
    /// persisted history, then continues with the live tail. An `offset`
    /// of 0 means "from the beginning of the changelog".
    async fn consume_since(&self, offset: i64) -> Result<EventStream>;
}

/// The production [`FydeClient`] implementation, backed by a real tonic
/// connection. Every call is authenticated by attaching the session token
/// currently persisted in settings (if any) as a bearer `authorization`
/// header (see
/// [`crate::domains::sessions::Service::authenticated_request`]).
pub(super) struct GrpcClient {
    client: ChangelogClient<Channel>,
    sessions: Arc<SessionsClient>,
}

impl GrpcClient {
    /// Connects to the changelog service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`), authenticating
    /// every call via `sessions`.
    pub async fn new(base_url: impl AsRef<str>, sessions: Arc<SessionsClient>) -> Result<Self> {
        let endpoint = Channel::from_shared(base_url.as_ref().to_string())
            .map_err(|err| Error::InvalidEndpoint(err.to_string()))?;
        let channel = endpoint
            .connect()
            .await
            .context("failed to connect to changelog grpc endpoint")?;

        Ok(Self {
            client: ChangelogClient::new(channel),
            sessions,
        })
    }
}

#[async_trait]
impl FydeClient for GrpcClient {
    async fn record_event(&self, encrypted_content: Vec<u8>) -> Result<i64> {
        let request = self
            .sessions
            .authenticated_request(RecordEventRequest { encrypted_content })
            .await?;

        // The generated client's RPC methods take `&mut self`, but the
        // underlying `Channel` is cheap to clone and safe to use
        // concurrently, so we clone it per call to expose `&self` here.
        let response = self
            .client
            .clone()
            .record_event(request)
            .await
            .context("failed to record changelog event")?
            .into_inner();

        Ok(response.offset)
    }

    async fn consume_since(&self, offset: i64) -> Result<EventStream> {
        let request = self
            .sessions
            .authenticated_request(ConsumeSinceRequest { offset })
            .await?;

        let response = self
            .client
            .clone()
            .consume_since(request)
            .await
            .with_context(|| format!("failed to consume changelog events since offset {offset}"))?;
        Ok(Box::pin(response.into_inner()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::settings::MockService as MockSettingsService;

    #[tokio::test]
    async fn new_rejects_a_malformed_base_url() {
        let sessions = Arc::new(SessionsClient::new(Arc::new(MockSettingsService::new())));
        let result = GrpcClient::new("not a valid uri", sessions).await;

        let err = match result {
            Ok(_) => panic!("a malformed base url must be rejected"),
            Err(err) => err,
        };
        assert!(matches!(err, Error::InvalidEndpoint(_)));
    }
}
