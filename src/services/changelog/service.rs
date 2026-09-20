use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::services::documents::{Document, Metadata, Storage as DocumentStorage};
use crate::{Error, ErrorContext as _, Result};

use super::Service;
use super::crypto;
use super::grpc_client::{ChangelogEvent as ProtoChangelogEvent, FydeClient, GrpcClient};
use super::storage::OffsetStorage;

/// The kind of write recorded by a [`ChangelogEvent`]. Only [`Self::Created`]
/// is producible today (there is no update/delete flow yet), but the shape
/// is forward-compatible with the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    Created,
    UpdateMetadata,
    Deleted,
}

/// A single recorded write against a document, decrypted from the server's
/// changelog. Inlines the document's fields rather than wrapping a
/// [`Document`], since not every event type carries all of them: `content`
/// is `None` for a metadata-only update, and both `content` and `metadata`
/// are `None` for a deletion (only `document_id` is meaningful then).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogEvent {
    pub offset: i64,
    pub event_type: EventType,
    pub document_id: Uuid,
    pub content: Option<Vec<u8>>,
    pub metadata: Option<Metadata>,
}

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
        tokens: crate::session::SessionTokenStore,
    ) -> Result<Self> {
        Ok(Self {
            grpc: Box::new(
                GrpcClient::new(base_url, tokens)
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

                let document = Document {
                    id: event.document_id,
                    content: content.clone(),
                    metadata: metadata.clone(),
                };
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
    use super::*;

    /// An in-memory [`DocumentStorage`], for tests that don't need to touch
    /// SQLite.
    #[derive(Default)]
    struct InMemoryDocumentStorage {
        documents: Mutex<Vec<Document>>,
    }

    impl DocumentStorage for InMemoryDocumentStorage {
        async fn save_document(&self, document: &Document) -> Result<()> {
            self.documents.lock().unwrap().push(document.clone());
            Ok(())
        }

        async fn get_document(&self, id: Uuid) -> Result<Option<Document>> {
            Ok(self
                .documents
                .lock()
                .unwrap()
                .iter()
                .find(|document| document.id == id)
                .cloned())
        }

        async fn list_documents(&self, _offset: i64, _limit: i64) -> Result<Vec<Document>> {
            Ok(self.documents.lock().unwrap().clone())
        }
    }

    /// An in-memory [`OffsetStorage`], for tests that don't need to touch
    /// SQLite. Starts at offset 0, like a database that's never been
    /// written to.
    #[derive(Default)]
    struct InMemoryOffsetStorage {
        offset: Mutex<i64>,
    }

    impl OffsetStorage for InMemoryOffsetStorage {
        async fn get_offset(&self) -> Result<i64> {
            Ok(*self.offset.lock().unwrap())
        }

        async fn save_offset(&self, offset: i64) -> Result<()> {
            *self.offset.lock().unwrap() = offset;
            Ok(())
        }
    }

    fn sample_metadata() -> Metadata {
        Metadata {
            name: "report.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            created_at: 1_700_000_000,
            size: 4,
            checksum: "checksum-value".to_string(),
            transcript: String::new(),
        }
    }

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
            Some(&sample_metadata()),
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

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            InMemoryDocumentStorage::default(),
            InMemoryOffsetStorage::default(),
        );

        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        client
            .consume(Box::new(move |event| {
                received_in_callback.lock().unwrap().push(event)
            }))
            .await
            .unwrap();

        {
            let received = received.lock().unwrap();
            assert_eq!(received.len(), 1);
            assert_eq!(received[0].offset, 1);
            assert_eq!(received[0].document_id, document_id);
            assert_eq!(received[0].event_type, EventType::Created);
        }

        let cached = client
            .document_storage
            .get_document(document_id)
            .await
            .unwrap();
        assert_eq!(cached.unwrap().content, b"content");
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

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            InMemoryDocumentStorage::default(),
            InMemoryOffsetStorage::default(),
        );

        client.consume(Box::new(|_| {})).await.unwrap();

        let cached = client
            .document_storage
            .get_document(document_id)
            .await
            .unwrap();
        assert_eq!(cached, None);
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

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            InMemoryDocumentStorage::default(),
            InMemoryOffsetStorage::default(),
        );

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

        let offset_storage = InMemoryOffsetStorage::default();
        offset_storage.save_offset(5).await.unwrap();

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            InMemoryDocumentStorage::default(),
            offset_storage,
        );

        client.consume(Box::new(|_| {})).await.unwrap();

        assert_eq!(client.offset_storage.get_offset().await.unwrap(), 6);
    }

    #[tokio::test]
    async fn send_encrypts_the_event_before_it_reaches_the_transport_layer() {
        let document_id = Uuid::new_v4();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_record_event()
            .withf(move |encrypted_content| {
                let (event_type, id, content, metadata) =
                    crypto::decrypt_event(encrypted_content).unwrap();
                event_type == EventType::Created
                    && id == document_id
                    && content == Some(b"body".to_vec())
                    && metadata == Some(sample_metadata())
            })
            .returning(|_| Ok(1));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            InMemoryDocumentStorage::default(),
            InMemoryOffsetStorage::default(),
        );

        client
            .send(
                EventType::Created,
                document_id,
                Some(b"body"),
                Some(&sample_metadata()),
            )
            .await
            .unwrap();
    }
}
