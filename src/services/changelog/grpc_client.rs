use async_trait::async_trait;
use futures::stream::BoxStream;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;

use crate::{Error, Result};

/// Generated protobuf/gRPC bindings for the `changelog` service, compiled
/// from `../api-protos/changelog.proto` by `build.rs`.
mod proto {
    tonic::include_proto!("changelog");
}

pub(super) use proto::{ChangelogEvent, EventType};

use proto::changelog_client::ChangelogClient;
use proto::{ListEventsSinceRequest, WatchEventsRequest};

/// A page of changelog events, as returned by [`FydeClient::list_events_since`].
pub(super) struct EventsSincePage {
    pub events: Vec<ChangelogEvent>,
    pub next_offset: i64,
}

/// A stream of raw changelog events as received from the server, opened by
/// [`FydeClient::watch_events`].
pub(super) type EventStream =
    BoxStream<'static, std::result::Result<ChangelogEvent, tonic::Status>>;

/// A gRPC transport for talking to the fyde server's changelog service.
/// Knows nothing about changelog events themselves beyond the raw proto
/// types; just opens the stream and hands back raw responses. Abstracted as
/// a trait so callers can be tested against [`MockFydeClient`] instead of a
/// live server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Opens a stream of `ChangelogEvent`s, starting from the moment the
    /// call is made (it does not replay past entries).
    async fn watch_events(&self) -> Result<EventStream>;

    /// Lists entries recorded after `offset`, oldest first, one page at a
    /// time. An `offset` of 0 means "from the beginning of the changelog". A
    /// `limit` of 0 selects a server-side default.
    async fn list_events_since(&self, offset: i64, limit: i32) -> Result<EventsSincePage>;
}

/// The production [`FydeClient`] implementation, backed by a real tonic connection.
pub(super) struct GrpcClient {
    client: ChangelogClient<Channel>,
}

impl GrpcClient {
    /// Connects to the changelog service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`).
    pub async fn new(base_url: impl AsRef<str>) -> Result<Self> {
        let endpoint = Channel::from_shared(base_url.as_ref().to_string())
            .map_err(|err| Error::InvalidEndpoint(err.to_string()))?;
        let channel = endpoint.connect().await?;

        Ok(Self {
            client: ChangelogClient::new(channel),
        })
    }
}

#[async_trait]
impl FydeClient for GrpcClient {
    async fn watch_events(&self) -> Result<EventStream> {
        // The generated client's RPC methods take `&mut self`, but the
        // underlying `Channel` is cheap to clone and safe to use
        // concurrently, so we clone it per call to expose `&self` here.
        let response = self
            .client
            .clone()
            .watch_events(WatchEventsRequest {})
            .await?;
        Ok(Box::pin(response.into_inner()))
    }

    async fn list_events_since(&self, offset: i64, limit: i32) -> Result<EventsSincePage> {
        let response = self
            .client
            .clone()
            .list_events_since(ListEventsSinceRequest { offset, limit })
            .await?
            .into_inner();

        Ok(EventsSincePage {
            events: response.events,
            next_offset: response.next_offset,
        })
    }
}
