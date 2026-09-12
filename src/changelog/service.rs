use tonic::Streaming;
use uuid::Uuid;

use crate::{Error, Result};

use super::grpc_client::{
    ChangelogEvent as ProtoChangelogEvent, EventType as ProtoEventType, GrpcClient,
};

/// The kind of write recorded by a [`ChangelogEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    Created,
    Updated,
    Deleted,
}

impl TryFrom<ProtoEventType> for EventType {
    type Error = Error;

    fn try_from(value: ProtoEventType) -> Result<Self> {
        match value {
            ProtoEventType::Created => Ok(EventType::Created),
            ProtoEventType::Updated => Ok(EventType::Updated),
            ProtoEventType::Deleted => Ok(EventType::Deleted),
            ProtoEventType::Unspecified => Err(Error::InvalidChangelogEvent(
                "server sent an unspecified event type".into(),
            )),
        }
    }
}

/// A single recorded write against a document, as streamed by
/// [`ChangelogSubscription::next`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogEvent {
    pub offset: i64,
    pub document_id: Uuid,
    pub event_type: EventType,
    pub created_at: i64,
}

impl TryFrom<ProtoChangelogEvent> for ChangelogEvent {
    type Error = Error;

    fn try_from(proto: ProtoChangelogEvent) -> Result<Self> {
        let event_type = ProtoEventType::try_from(proto.event_type).map_err(|_| {
            Error::InvalidChangelogEvent(format!("unknown event type: {}", proto.event_type))
        })?;

        Ok(Self {
            offset: proto.offset,
            document_id: Uuid::parse_str(&proto.document_id)?,
            event_type: event_type.try_into()?,
            created_at: proto.created_at,
        })
    }
}

/// A page of changelog events, as returned by [`ChangelogClient::list_since`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventsSincePage {
    pub events: Vec<ChangelogEvent>,
    /// Offset to pass as `offset` on the next call to keep paginating.
    /// Equal to the last returned event's offset, or to the request's
    /// `offset` if this page was empty.
    pub next_offset: i64,
}

/// A live subscription to changelog events, opened by
/// [`ChangelogClient::watch`]. Streams a [`ChangelogEvent`] for every new
/// entry recorded from the moment the subscription was opened; it does not
/// replay past entries.
pub struct ChangelogSubscription {
    stream: Streaming<ProtoChangelogEvent>,
}

impl ChangelogSubscription {
    /// Waits for and returns the next event, or `None` once the server
    /// closes the stream.
    pub async fn next(&mut self) -> Result<Option<ChangelogEvent>> {
        match self.stream.message().await? {
            Some(proto_event) => Ok(Some(proto_event.try_into()?)),
            None => Ok(None),
        }
    }
}

/// A client for the fyde server's changelog service, used to subscribe to
/// document write events over gRPC.
pub struct ChangelogClient {
    grpc: GrpcClient,
}

impl ChangelogClient {
    /// Creates a client for the changelog service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`).
    pub async fn new(base_url: impl AsRef<str>) -> Result<Self> {
        Ok(Self {
            grpc: GrpcClient::new(base_url).await?,
        })
    }

    /// Opens a subscription streaming a [`ChangelogEvent`] every time a new
    /// entry is recorded, starting from the moment the call is made (it
    /// does not replay past entries).
    pub async fn watch(&mut self) -> Result<ChangelogSubscription> {
        let stream = self.grpc.watch_events().await?;
        Ok(ChangelogSubscription { stream })
    }

    /// Lists entries recorded after `offset`, oldest first, one page at a
    /// time. An `offset` of 0 means "from the beginning of the changelog".
    /// A `limit` of 0 selects a server-side default.
    ///
    /// Paginate by repeatedly calling this with the previous page's
    /// `next_offset` until the returned page is empty.
    pub async fn list_since(&mut self, offset: i64, limit: i32) -> Result<EventsSincePage> {
        let page = self.grpc.list_events_since(offset, limit).await?;

        let events = page
            .events
            .into_iter()
            .map(ChangelogEvent::try_from)
            .collect::<Result<_>>()?;

        Ok(EventsSincePage {
            events,
            next_offset: page.next_offset,
        })
    }
}
