use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use uuid::Uuid;

use crate::services::documents::Service as DocumentsService;
use crate::{Error, Result};

use super::Service;
use super::grpc_client::{
    ChangelogEvent as ProtoChangelogEvent, EventStream, EventType as ProtoEventType, FydeClient,
    GrpcClient,
};
use super::storage::Storage;

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

/// A live subscription to changelog events, opened internally by
/// [`Service::consume`]. Streams a [`ChangelogEvent`] for every new entry
/// recorded from the moment the subscription was opened; it does not replay
/// past entries.
pub struct ChangelogSubscription {
    stream: EventStream,
}

impl ChangelogSubscription {
    /// Waits for and returns the next event, or `None` once the server
    /// closes the stream.
    pub async fn next(&mut self) -> Result<Option<ChangelogEvent>> {
        match self.stream.next().await {
            Some(Ok(proto_event)) => Ok(Some(proto_event.try_into()?)),
            Some(Err(status)) => Err(status.into()),
            None => Ok(None),
        }
    }
}

/// A client for the fyde server's changelog service, used to subscribe to
/// document write events over gRPC. Generic over the [`Storage`]
/// implementation used by [`Service::consume`] to persist its cursor.
/// [`Service::consume`] also fetches and caches every document referenced by
/// the events it encounters, via an injected documents [`DocumentsService`].
pub struct ChangelogClient<S: Storage> {
    grpc: Box<dyn FydeClient>,
    storage: S,
    documents: Arc<dyn DocumentsService>,
}

impl<S: Storage> ChangelogClient<S> {
    /// Creates a client for the changelog service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`), using `storage`
    /// to persist [`Service::consume`]'s cursor, and `documents` to fetch and
    /// cache the documents referenced by encountered events.
    pub async fn new(
        base_url: impl AsRef<str>,
        storage: S,
        documents: Arc<dyn DocumentsService>,
    ) -> Result<Self> {
        Ok(Self {
            grpc: Box::new(GrpcClient::new(base_url).await?),
            storage,
            documents,
        })
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of a
    /// live server.
    #[cfg(test)]
    fn with_grpc(
        grpc: impl FydeClient + 'static,
        storage: S,
        documents: Arc<dyn DocumentsService>,
    ) -> Self {
        Self {
            grpc: Box::new(grpc),
            storage,
            documents,
        }
    }

    /// Fetches and caches every document referenced by `events` via
    /// [`DocumentsService::fetch_many`], skipping the call entirely when
    /// `events` is empty.
    async fn fetch_documents(&self, events: &[ChangelogEvent]) -> Result<()> {
        let ids: Vec<Uuid> = events.iter().map(|event| event.document_id).collect();

        if ids.is_empty() {
            return Ok(());
        }

        self.documents.fetch_many(ids).await?;

        Ok(())
    }

    /// Catches up on every entry recorded since the offset persisted in
    /// `storage`, advancing that offset as it goes, until none remain.
    /// Invokes `callback` once for each event encountered.
    async fn catch_up(&self, callback: &mut (dyn FnMut(ChangelogEvent) + Send)) -> Result<()> {
        loop {
            let offset = self.storage.read_offset().await?;
            let page = self.list_since(offset, 0).await?;

            if page.events.is_empty() {
                break;
            }

            self.fetch_documents(&page.events).await?;

            for event in page.events {
                callback(event);
            }

            self.storage.write_offset(page.next_offset).await?;
        }

        Ok(())
    }

    /// Lists entries recorded after `offset`, oldest first, one page at a
    /// time. An `offset` of 0 means "from the beginning of the changelog".
    /// A `limit` of 0 selects a server-side default.
    ///
    /// Paginate by repeatedly calling this with the previous page's
    /// `next_offset` until the returned page is empty.
    async fn list_since(&self, offset: i64, limit: i32) -> Result<EventsSincePage> {
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

#[async_trait]
impl<S: Storage> Service for ChangelogClient<S> {
    /// First catches up on every entry already recorded since the offset
    /// persisted in `storage`, via [`Self::catch_up`]. Then opens a
    /// subscription streaming a [`ChangelogEvent`] every time a new entry is
    /// recorded (starting from the moment the call is made, it does not
    /// replay past entries), and for each one received, catches up again via
    /// [`Self::list_since`], advancing the persisted offset afterwards.
    ///
    /// `callback` is invoked once for every event encountered, during both
    /// the initial catch-up and the live subscription.
    ///
    /// Runs until the server closes the watch stream or an error occurs.
    async fn consume(&self, mut callback: Box<dyn FnMut(ChangelogEvent) + Send>) -> Result<()> {
        self.catch_up(&mut *callback).await?;

        let stream = self.grpc.watch_events().await?;
        let mut subscription = ChangelogSubscription { stream };

        while subscription.next().await?.is_some() {
            let offset = self.storage.read_offset().await?;
            let page = self.list_since(offset, 0).await?;

            self.fetch_documents(&page.events).await?;

            for event in page.events {
                callback(event);
            }

            self.storage.write_offset(page.next_offset).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use futures::StreamExt as _;

    use super::super::grpc_client::{EventsSincePage as GrpcEventsSincePage, MockFydeClient};
    use super::*;
    use crate::services::documents::{Document, NewDocument};

    /// A [`Storage`] backed by an in-memory cursor, for tests that don't
    /// need to touch SQLite.
    #[derive(Default)]
    struct InMemoryStorage {
        offset: AtomicI64,
    }

    impl Storage for InMemoryStorage {
        async fn read_offset(&self) -> Result<i64> {
            Ok(self.offset.load(Ordering::SeqCst))
        }

        async fn write_offset(&self, offset: i64) -> Result<()> {
            self.offset.store(offset, Ordering::SeqCst);
            Ok(())
        }
    }

    /// A [`DocumentsService`] that does nothing, for tests that only care
    /// about changelog behavior.
    struct NoopDocuments;

    #[async_trait]
    impl DocumentsService for NoopDocuments {
        async fn upload(&self, _document: NewDocument) -> Result<Uuid> {
            unimplemented!("not used by these tests")
        }

        async fn fetch(&self, _id: Uuid) -> Result<Option<Document>> {
            Ok(None)
        }

        async fn get(&self, _id: Uuid) -> Result<Option<Document>> {
            Ok(None)
        }

        async fn fetch_many(&self, _ids: Vec<Uuid>) -> Result<Vec<Document>> {
            Ok(Vec::new())
        }
    }

    fn proto_event(offset: i64, document_id: Uuid) -> ProtoChangelogEvent {
        ProtoChangelogEvent {
            offset,
            document_id: document_id.to_string(),
            event_type: ProtoEventType::Created as i32,
            created_at: 1_700_000_000,
        }
    }

    #[tokio::test]
    async fn list_since_converts_proto_events_into_domain_events() {
        let document_id = Uuid::new_v4();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_list_events_since()
            .withf(|offset, limit| *offset == 42 && *limit == 10)
            .returning(move |_, _| {
                Ok(GrpcEventsSincePage {
                    events: vec![proto_event(43, document_id)],
                    next_offset: 43,
                })
            });

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            InMemoryStorage::default(),
            Arc::new(NoopDocuments),
        );

        let page = client.list_since(42, 10).await.unwrap();

        assert_eq!(page.next_offset, 43);
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].offset, 43);
        assert_eq!(page.events[0].document_id, document_id);
        assert_eq!(page.events[0].event_type, EventType::Created);
    }

    #[tokio::test]
    async fn catch_up_pages_through_events_and_advances_the_offset() {
        let document_id = Uuid::new_v4();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_list_events_since()
            .withf(|offset, _| *offset == 0)
            .returning(move |_, _| {
                Ok(GrpcEventsSincePage {
                    events: vec![proto_event(1, document_id)],
                    next_offset: 1,
                })
            });
        mock_grpc
            .expect_list_events_since()
            .withf(|offset, _| *offset == 1)
            .returning(|_, _| {
                Ok(GrpcEventsSincePage {
                    events: vec![],
                    next_offset: 1,
                })
            });

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            InMemoryStorage::default(),
            Arc::new(NoopDocuments),
        );

        let mut received = Vec::new();
        client
            .catch_up(&mut |event| received.push(event))
            .await
            .unwrap();

        assert_eq!(received.len(), 1);
        assert_eq!(received[0].document_id, document_id);
    }

    #[tokio::test]
    async fn consume_catches_up_then_reacts_to_live_events() {
        let document_id = Uuid::new_v4();

        let mut mock_grpc = MockFydeClient::new();
        // Nothing to catch up on.
        mock_grpc
            .expect_list_events_since()
            .withf(|offset, _| *offset == 0)
            .times(1)
            .returning(|_, _| {
                Ok(GrpcEventsSincePage {
                    events: vec![],
                    next_offset: 0,
                })
            });
        // One live notification, then the stream closes.
        mock_grpc.expect_watch_events().times(1).returning(move || {
            Ok(futures::stream::iter(vec![Ok(proto_event(1, document_id))]).boxed())
        });
        // Catching up after the live notification finds the new event.
        mock_grpc
            .expect_list_events_since()
            .withf(|offset, _| *offset == 0)
            .times(1)
            .returning(move |_, _| {
                Ok(GrpcEventsSincePage {
                    events: vec![proto_event(1, document_id)],
                    next_offset: 1,
                })
            });

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            InMemoryStorage::default(),
            Arc::new(NoopDocuments),
        );

        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        client
            .consume(Box::new(move |event| {
                received_in_callback.lock().unwrap().push(event);
            }))
            .await
            .unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].document_id, document_id);
    }
}
