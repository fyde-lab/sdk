use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::domains::documents::{Document, Metadata, Storage as DocumentStorage};
use crate::domains::server_state::Service as ServerStateService;
use crate::domains::sessions::SessionsClient;
use crate::{Error, ErrorContext as _, Result};

use super::Service;
use super::crypto;
use super::grpc_client::{ChangelogEvent as ProtoChangelogEvent, FydeClient, GrpcClient};
use super::models::{ChangelogEvent, EventType};
use super::storage::CursorStorage;

impl TryFrom<ProtoChangelogEvent> for ChangelogEvent {
    type Error = Error;

    fn try_from(proto: ProtoChangelogEvent) -> Result<Self> {
        let id = Uuid::parse_str(&proto.id).with_context(|| {
            format!(
                "invalid changelog event id {:?} returned by server",
                proto.id
            )
        })?;

        let (event_type, document_id, content, metadata) =
            crypto::decrypt_event(&proto.encrypted_content)
                .with_context(|| format!("failed to decrypt changelog event {id}"))?;

        Ok(Self {
            id,
            event_type,
            document_id,
            content,
            metadata,
        })
    }
}

/// Returns the smallest id strictly greater than `id`, used to advance the
/// consumption cursor past an already-processed entry: ids are unique and
/// compared byte-for-byte, so the 128-bit successor of `id` is guaranteed
/// to be strictly greater than it and to skip no real id in between — it
/// doesn't need to be a valid UUIDv7 itself, it's only ever used as a query
/// bound (mirrors `DefaultService::consume_since` on the server).
fn successor(id: Uuid) -> Uuid {
    Uuid::from_u128(id.as_u128().wrapping_add(1))
}

/// A client for the fyde server's changelog service. Generic over the
/// [`DocumentStorage`] implementation used by [`Service::consume`] to cache
/// documents materialized from consumed events, and the [`CursorStorage`]
/// implementation used to track how far the changelog has been consumed.
pub(super) struct ChangelogClient<D: DocumentStorage, O: CursorStorage> {
    grpc: Box<dyn FydeClient>,
    document_storage: D,
    cursor_storage: O,
    server_state: Arc<dyn ServerStateService>,
}

/// Delay between reachability checks in [`ChangelogClient::consume_once`]
/// while waiting for the server to come back up.
const SERVER_REACHABILITY_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(200);

impl<D: DocumentStorage, O: CursorStorage> ChangelogClient<D, O> {
    /// Creates a client for the changelog service using the shared
    /// `channel` connection to the fyde server, using `document_storage` to
    /// cache documents materialized by [`Service::consume`],
    /// `cursor_storage` to track its progress through the changelog, and
    /// `server_state` to wait for the server to be reachable before opening
    /// a changelog stream.
    pub(super) async fn new(
        channel: Channel,
        document_storage: D,
        cursor_storage: O,
        sessions: Arc<SessionsClient>,
        server_state: Arc<dyn ServerStateService>,
    ) -> Result<Self> {
        Ok(Self {
            grpc: Box::new(GrpcClient::new(channel, sessions)),
            document_storage,
            cursor_storage,
            server_state,
        })
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of a
    /// live server.
    #[cfg(test)]
    fn with_grpc(
        grpc: impl FydeClient + 'static,
        document_storage: D,
        cursor_storage: O,
        server_state: Arc<dyn ServerStateService>,
    ) -> Self {
        Self {
            grpc: Box::new(grpc),
            document_storage,
            cursor_storage,
            server_state,
        }
    }

    /// One pass of [`Service::consume`]: opens a stream from the persisted
    /// cursor and processes events until the stream ends or an error
    /// occurs. Split out as an inherent method (rather than inlined in
    /// `consume`) so it can be retried in a loop without re-implementing
    /// the retry itself, and so tests can exercise a single pass directly.
    async fn consume_once(&self, callback: &mut (dyn FnMut(ChangelogEvent) + Send)) -> Result<()> {
        let cursor = self
            .cursor_storage
            .get_cursor()
            .await
            .context("failed to read the local changelog cursor")?;

        let since = if cursor.is_nil() { None } else { Some(cursor) };

        while !self
            .server_state
            .is_server_reachable()
            .await
            .context("failed to check server reachability")?
        {
            tokio::time::sleep(SERVER_REACHABILITY_POLL_INTERVAL).await;
        }

        let mut stream = self
            .grpc
            .consume_since(since)
            .await
            .with_context(|| format!("failed to open changelog stream from id {since:?}"))?;

        while let Some(proto_event) = stream.next().await {
            let proto_event = proto_event.context("failed to read next changelog event")?;
            let event = ChangelogEvent::try_from(proto_event)?;

            match event.event_type {
                EventType::Created => {
                    let (Some(content), Some(metadata)) = (&event.content, &event.metadata) else {
                        return Err(Error::InvalidChangelogEvent(format!(
                            "created event {} for document {} is missing content or metadata",
                            event.id, event.document_id
                        )));
                    };

                    let document =
                        Document::new(event.document_id, content.clone(), metadata.clone());
                    self.document_storage
                        .save_document(&document)
                        .await
                        .with_context(|| {
                            format!("failed to cache document {} locally", event.document_id)
                        })?;
                }
                EventType::UpdateMetadata => {
                    let Some(metadata) = &event.metadata else {
                        return Err(Error::InvalidChangelogEvent(format!(
                            "update metadata event {} for document {} is missing metadata",
                            event.id, event.document_id
                        )));
                    };

                    self.document_storage
                        .update_metadata(event.document_id, metadata)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to update cached document {} locally",
                                event.document_id
                            )
                        })?;
                }
                EventType::Deleted => {}
            }

            let next_cursor = successor(event.id);
            self.cursor_storage
                .save_cursor(next_cursor)
                .await
                .with_context(|| format!("failed to persist changelog cursor {next_cursor}"))?;

            callback(event);
        }

        Ok(())
    }
}

#[async_trait]
impl<D: DocumentStorage, O: CursorStorage> Service for ChangelogClient<D, O> {
    async fn send(
        &self,
        event_type: EventType,
        document_id: Uuid,
        content: Option<&[u8]>,
        metadata: Option<&Metadata>,
    ) -> Result<()> {
        let encrypted_content = crypto::encrypt_event(event_type, document_id, content, metadata)
            .context("failed to encrypt changelog event")?;

        self.grpc
            .record_event(encrypted_content)
            .await
            .context("failed to send changelog event to server")?;

        Ok(())
    }

    async fn consume(&self, mut callback: Box<dyn FnMut(ChangelogEvent) + Send>) -> Result<()> {
        loop {
            if let Err(err) = self.consume_once(&mut *callback).await {
                tracing::error!("changelog consume failed, retrying in 1s: {err:?}");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::StreamExt as _;

    use super::super::grpc_client::MockFydeClient;
    use super::super::storage::MockCursorStorage;
    use super::*;
    use crate::domains::documents::{FakeMetadata, MockStorage};
    use crate::domains::server_state::MockService as MockServerState;

    /// A [`ServerStateService`] mock reporting the server as always
    /// reachable, for tests where reachability isn't under test.
    fn reachable_server_state() -> Arc<dyn ServerStateService> {
        let mut mock = MockServerState::new();
        mock.expect_is_server_reachable().returning(|| Ok(true));
        Arc::new(mock)
    }

    fn proto_event(id: Uuid, document_id: Uuid) -> ProtoChangelogEvent {
        proto_event_with_type(id, document_id, EventType::Created)
    }

    fn proto_event_with_type(
        id: Uuid,
        document_id: Uuid,
        event_type: EventType,
    ) -> ProtoChangelogEvent {
        let encrypted_content = crypto::encrypt_event(
            event_type,
            document_id,
            Some(b"content"),
            Some(&FakeMetadata::new().build()),
        )
        .unwrap();
        ProtoChangelogEvent {
            id: id.to_string(),
            encrypted_content,
        }
    }

    #[tokio::test]
    async fn consume_decrypts_events_and_caches_the_document() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .withf(|id| id.is_none())
            .returning(move |_| {
                Ok(futures::stream::iter(vec![Ok(proto_event(event_id, document_id))]).boxed())
            });

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_save_document()
            .withf(move |document| document.id() == document_id && document.content() == b"content")
            .times(1)
            .returning(|_| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == successor(event_id))
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
        );

        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        let mut callback = move |event| received_in_callback.lock().unwrap().push(event);
        client.consume_once(&mut callback).await.unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].id, event_id);
        assert_eq!(received[0].document_id, document_id);
        assert_eq!(received[0].event_type, EventType::Created);
    }

    #[tokio::test(start_paused = true)]
    async fn consume_once_waits_for_the_server_to_become_reachable_before_streaming() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(proto_event(event_id, document_id))]).boxed())
        });

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_save_document()
            .returning(|_| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage.expect_save_cursor().returning(|_| Ok(()));

        let reachability_checks = Arc::new(AtomicU32::new(0));
        let reachability_checks_in_mock = reachability_checks.clone();
        let mut server_state = MockServerState::new();
        server_state
            .expect_is_server_reachable()
            .returning(move || {
                let attempt = reachability_checks_in_mock.fetch_add(1, Ordering::SeqCst);
                Ok(attempt >= 2)
            });

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            Arc::new(server_state),
        );

        let mut callback = |_| {};
        let consume_once_fut = client.consume_once(&mut callback);
        tokio::pin!(consume_once_fut);

        // Not yet reachable on the first two checks, so the stream must not
        // have been opened yet.
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_once_fut)
            .await
            .unwrap_err();
        assert_eq!(reachability_checks.load(Ordering::SeqCst), 1);

        tokio::time::advance(SERVER_REACHABILITY_POLL_INTERVAL).await;
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_once_fut)
            .await
            .unwrap_err();
        assert_eq!(reachability_checks.load(Ordering::SeqCst), 2);

        // Third check reports reachable, so the pass should now complete.
        tokio::time::advance(SERVER_REACHABILITY_POLL_INTERVAL).await;
        consume_once_fut.await.unwrap();
        assert_eq!(reachability_checks.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn consume_does_not_cache_the_document_for_a_deleted_event() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(proto_event_with_type(
                event_id,
                document_id,
                EventType::Deleted,
            ))])
            .boxed())
        });

        // No `expect_save_document()`/`expect_get_document()` set up: the
        // mock panics if either is called, which is how this test proves
        // neither is.
        let document_storage = MockStorage::new();

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == successor(event_id))
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn consume_updates_the_cached_documents_metadata_for_an_update_metadata_event() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(proto_event_with_type(
                event_id,
                document_id,
                EventType::UpdateMetadata,
            ))])
            .boxed())
        });

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_update_metadata()
            .withf(move |id, _metadata| *id == document_id)
            .times(1)
            .returning(|_, _| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == successor(event_id))
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn consume_errors_on_a_created_event_missing_content_or_metadata() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let encrypted_content =
            crypto::encrypt_event(EventType::Created, document_id, None, None).unwrap();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(ProtoChangelogEvent {
                id: event_id.to_string(),
                encrypted_content: encrypted_content.clone(),
            })])
            .boxed())
        });

        // Decryption fails before either storage is touched, so neither
        // mock needs `save_document`/`save_cursor` expectations: the mock
        // panics if it's called, which is how this test proves it isn't.
        let document_storage = MockStorage::new();
        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
        );

        let result = client.consume_once(&mut |_| {}).await;

        assert!(matches!(result, Err(Error::InvalidChangelogEvent(_))));
    }

    #[tokio::test]
    async fn consume_resumes_from_the_persisted_cursor() {
        let document_id = Uuid::now_v7();
        let cursor = Uuid::now_v7();
        let event_id = successor(cursor);

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .withf(move |id| *id == Some(cursor))
            .returning(move |_| {
                Ok(futures::stream::iter(vec![Ok(proto_event(event_id, document_id))]).boxed())
            });

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_save_document()
            .returning(|_| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(move || Ok(cursor));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == successor(event_id))
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn consume_never_returns_and_retries_after_an_error() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_in_mock = attempts.clone();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            if attempts_in_mock.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(Error::InvalidChangelogEvent("boom".to_string()))
            } else {
                // A never-ending stream, mirroring the real `WatchEvents`
                // RPC staying open: this keeps the retried attempt parked
                // awaiting the next event, rather than looping back into
                // `consume_since` in a tight, unyielding busy loop the way
                // an immediately-exhausted stream would.
                Ok(futures::stream::pending().boxed())
            }
        });

        let document_storage = MockStorage::new();

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
        );

        let consume_fut = client.consume(Box::new(|_| {}));
        tokio::pin!(consume_fut);

        // First attempt fails immediately, so `consume` should be waiting
        // in the 1s retry sleep without a second attempt yet.
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // Once the retry sleep elapses, `consume` should retry (and keep
        // looping forever rather than returning).
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert!(attempts.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn send_encrypts_the_event_before_it_reaches_the_transport_layer() {
        let document_id = Uuid::now_v7();
        let metadata = FakeMetadata::new().build();

        let mut mock_grpc = MockFydeClient::new();
        let expected_metadata = metadata.clone();
        mock_grpc
            .expect_record_event()
            .withf(move |encrypted_content| {
                let (event_type, id, content, decrypted_metadata) =
                    crypto::decrypt_event(encrypted_content).unwrap();
                event_type == EventType::Created
                    && id == document_id
                    && content == Some(b"body".to_vec())
                    && decrypted_metadata == Some(expected_metadata.clone())
            })
            .returning(|_| Ok(Uuid::now_v7()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            MockStorage::new(),
            MockCursorStorage::new(),
            reachable_server_state(),
        );

        client
            .send(
                EventType::Created,
                document_id,
                Some(b"body"),
                Some(&metadata),
            )
            .await
            .unwrap();
    }
}
