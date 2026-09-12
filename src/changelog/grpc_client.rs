use tonic::Streaming;
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

/// A page of changelog events, as returned by [`GrpcClient::list_events_since`].
pub(super) struct EventsSincePage {
    pub events: Vec<ChangelogEvent>,
    pub next_offset: i64,
}

/// A thin gRPC transport for talking to the fyde server's changelog
/// service. Knows nothing about changelog events themselves beyond the raw
/// proto types; just opens the stream and hands back raw responses.
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

    /// Opens a stream of `ChangelogEvent`s, starting from the moment the
    /// call is made (it does not replay past entries).
    pub async fn watch_events(&mut self) -> Result<Streaming<ChangelogEvent>> {
        let response = self.client.watch_events(WatchEventsRequest {}).await?;
        Ok(response.into_inner())
    }

    /// Lists entries recorded after `offset`, oldest first, one page at a
    /// time. An `offset` of 0 means "from the beginning of the changelog". A
    /// `limit` of 0 selects a server-side default.
    pub async fn list_events_since(&mut self, offset: i64, limit: i32) -> Result<EventsSincePage> {
        let response = self
            .client
            .list_events_since(ListEventsSinceRequest { offset, limit })
            .await?
            .into_inner();

        Ok(EventsSincePage {
            events: response.events,
            next_offset: response.next_offset,
        })
    }
}
