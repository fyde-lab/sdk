use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use uuid::Uuid;

use crate::services::documents::{Document, Metadata, Storage as DocumentStorage};
use crate::services::sessions::SessionsClient;
use crate::{Error, ErrorContext as _, Result};

use super::Service;
use super::crypto;
use super::grpc_client::{ChangelogEvent as ProtoChangelogEvent, FydeClient, GrpcClient};
use super::models::{ChangelogEvent, EventType};
use super::storage::OffsetStorage;

impl TryFrom<ProtoChangelogEvent> for ChangelogEvent {
    type Error = Error;

    fn try_from(proto: ProtoChangelogEvent) -> Result<Self> {
        let (event_type, document_id, content, metadata) =
            crypto::decrypt_event(&proto.encrypted_content).with_context(|| {
                format!(
                    "failed to decrypt changelog event at offset {}",
                    proto.offset
                )
            })?;

        Ok(Self {
            offset: proto.offset,
            event_type,
            document_id,
            content,
            metadata,
        })
    }
}

/// A client for the fyde server's changelog service. Generic over the
/// [`DocumentStorage`] implementation used by [`Service::consume`] to cache
/// documents materialized from consumed events, and the [`OffsetStorage`]
/// implementation used to track how far the changelog has been consumed.
pub(super) struct ChangelogClient<D: DocumentStorage, O: OffsetStorage> {
    grpc: Box<dyn FydeClient>,
    document_storage: D,
    offset_storage: O,
}

impl<D: DocumentStorage, O: OffsetStorage> ChangelogClient<D, O> {
    /// Creates a client for the changelog service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`), using
    /// `document_storage` to cache documents materialized by
    /// [`Service::consume`] and `offset_storage` to track its progress
    /// through the changelog.
    pub(super) async fn new(
        base_url: impl AsRef<str>,
        document_storage: D,
        offset_storage: O,
        sessions: Arc<SessionsClient>,
    ) -> Result<Self> {
        Ok(Self {
            grpc: Box::new(
                GrpcClient::new(base_url, sessions)
                    .await
                    .context("failed to create changelog grpc client")?,
            ),
            document_storage,
            offset_storage,
        })
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of a
    /// live server.
    #[cfg(test)]
    fn with_grpc(grpc: impl FydeClient + 'static, document_storage: D, offset_storage: O) -> Self {
        Self {
            grpc: Box::new(grpc),
            document_storage,
            offset_storage,
        }
    }
}

#[async_trait]
impl<D: DocumentStorage, O: OffsetStorage> Service for ChangelogClient<D, O> {
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
        let offset = self
            .offset_storage
            .get_offset()
            .await
            .context("failed to read the local changelog offset")?;

        let mut stream = self
            .grpc
            .consume_since(offset)
            .await
            .with_context(|| format!("failed to open changelog stream from offset {offset}"))?;

        while let Some(proto_event) = stream.next().await {
            let proto_event = proto_event.context("failed to read next changelog event")?;
            let event = ChangelogEvent::try_from(proto_event)?;

            if event.event_type == EventType::Created {
                let (Some(content), Some(metadata)) = (&event.content, &event.metadata) else {
                    return Err(Error::InvalidChangelogEvent(format!(
                        "created event at offset {} for document {} is missing content or metadata",
                        event.offset, event.document_id
                    )));
                };

                let document = Document::new(event.document_id, content.clone(), metadata.clone());
                self.document_storage
                    .save_document(&document)
                    .await
                    .with_context(|| {
                        format!("failed to cache document {} locally", event.document_id)
                    })?;
            }

            self.offset_storage
                .save_offset(event.offset + 1)
                .await
                .with_context(|| {
                    format!("failed to persist changelog offset {}", event.offset + 1)
                })?;

            callback(event);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::StreamExt as _;

    use super::super::grpc_client::MockFydeClient;
    use super::super::storage::MockOffsetStorage;
    use super::*;
    use crate::services::documents::{FakeMetadata, MockStorage};

    fn proto_event(offset: i64, document_id: Uuid) -> ProtoChangelogEvent {
        proto_event_with_type(offset, document_id, EventType::Created)
    }

    fn proto_event_with_type(
        offset: i64,
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
            offset,
            encrypted_content,
        }
    }

    #[tokio::test]
    async fn consume_decrypts_events_and_caches_the_document() {
        let document_id = Uuid::new_v4();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .withf(|offset| *offset == 0)
            .returning(move |_| {
                Ok(futures::stream::iter(vec![Ok(proto_event(1, document_id))]).boxed())
            });

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_save_document()
            .withf(move |document| document.id() == document_id && document.content() == b"content")
            .times(1)
            .returning(|_| Ok(()));

        let mut offset_storage = MockOffsetStorage::new();
        offset_storage.expect_get_offset().returning(|| Ok(0));
        offset_storage
            .expect_save_offset()
            .withf(|offset| *offset == 2)
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(mock_grpc, document_storage, offset_storage);

        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        client
            .consume(Box::new(move |event| {
                received_in_callback.lock().unwrap().push(event)
            }))
            .await
            .unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].offset, 1);
        assert_eq!(received[0].document_id, document_id);
        assert_eq!(received[0].event_type, EventType::Created);
    }

    #[tokio::test]
    async fn consume_does_not_cache_the_document_for_a_non_created_event() {
        let document_id = Uuid::new_v4();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(proto_event_with_type(
                1,
                document_id,
                EventType::UpdateMetadata,
            ))])
            .boxed())
        });

        // No `expect_save_document()` set up: the mock panics if it's
        // called, which is how this test proves it isn't.
        let document_storage = MockStorage::new();

        let mut offset_storage = MockOffsetStorage::new();
        offset_storage.expect_get_offset().returning(|| Ok(0));
        offset_storage
            .expect_save_offset()
            .withf(|offset| *offset == 2)
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(mock_grpc, document_storage, offset_storage);

        client.consume(Box::new(|_| {})).await.unwrap();
    }

    #[tokio::test]
    async fn consume_errors_on_a_created_event_missing_content_or_metadata() {
        let document_id = Uuid::new_v4();

        let encrypted_content =
            crypto::encrypt_event(EventType::Created, document_id, None, None).unwrap();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(ProtoChangelogEvent {
                offset: 1,
                encrypted_content: encrypted_content.clone(),
            })])
            .boxed())
        });

        // Decryption fails before either storage is touched, so neither
        // mock needs `save_document`/`save_offset` expectations: the mock
        // panics if it's called, which is how this test proves it isn't.
        let document_storage = MockStorage::new();
        let mut offset_storage = MockOffsetStorage::new();
        offset_storage.expect_get_offset().returning(|| Ok(0));

        let client = ChangelogClient::with_grpc(mock_grpc, document_storage, offset_storage);

        let result = client.consume(Box::new(|_| {})).await;

        assert!(matches!(result, Err(Error::InvalidChangelogEvent(_))));
    }

    #[tokio::test]
    async fn consume_resumes_from_the_persisted_offset() {
        let document_id = Uuid::new_v4();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .withf(|offset| *offset == 5)
            .returning(move |_| {
                Ok(futures::stream::iter(vec![Ok(proto_event(5, document_id))]).boxed())
            });

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_save_document()
            .returning(|_| Ok(()));

        let mut offset_storage = MockOffsetStorage::new();
        offset_storage.expect_get_offset().returning(|| Ok(5));
        offset_storage
            .expect_save_offset()
            .withf(|offset| *offset == 6)
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(mock_grpc, document_storage, offset_storage);

        client.consume(Box::new(|_| {})).await.unwrap();
    }

    #[tokio::test]
    async fn send_encrypts_the_event_before_it_reaches_the_transport_layer() {
        let document_id = Uuid::new_v4();
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
            .returning(|_| Ok(1));

        let client =
            ChangelogClient::with_grpc(mock_grpc, MockStorage::new(), MockOffsetStorage::new());

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
