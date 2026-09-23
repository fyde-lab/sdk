use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{Error, ErrorContext as _, Result};

use super::changelog::{ChangelogEvent, EventType, Service as ChangelogService};
use super::storage::Storage;
use super::{Document, Metadata, Service, transcript};

/// The only content type [`Service::upload`] currently accepts.
pub(super) const PDF_CONTENT_TYPE: &str = "application/pdf";

/// The default [`Service`] implementation: publishes new documents as
/// encrypted changelog events via an injected [`ChangelogService`], and
/// delegates local, unencrypted caching to an injected [`Storage`].
pub(super) struct DocumentsClient<S: Storage> {
    storage: S,
    changelog: Arc<dyn ChangelogService>,
}

impl<S: Storage> DocumentsClient<S> {
    /// Creates a documents client using `storage` to cache documents
    /// locally, and `changelog` to publish new ones.
    pub(super) fn new(storage: S, changelog: Arc<dyn ChangelogService>) -> Self {
        Self { storage, changelog }
    }
}

#[async_trait]
impl<S: Storage> Service for DocumentsClient<S> {
    /// Reads the PDF file at `path`, encrypts it and its metadata, then
    /// publishes it as a "created" changelog event, returning its
    /// generated id.
    ///
    /// This follows an envelope encryption scheme (see
    /// `changelog::crypto::encrypt_event`): a fresh, random data encryption
    /// key (DEK) is generated for the event, used to encrypt it with
    /// AES-256-GCM, and is itself wrapped under a key-encryption-key before
    /// being sent alongside the ciphertext. Only the wrapped DEK and
    /// ciphertext ever leave this process.
    async fn upload(&self, path: &Path) -> Result<Uuid> {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if extension != "pdf" {
            return Err(Error::UnsupportedDocumentExtension(extension));
        }

        let content = tokio::fs::read(path)
            .await
            .with_context(|| format!("failed to read document at {}", path.display()))?;

        let original_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();

        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let mut hasher = Sha256::new();
        hasher.update(&content);

        let doc_transcript =
            transcript::extract(&content).context("failed to extract document transcript")?;

        let id = Uuid::now_v7();

        let metadata = Metadata {
            id,
            original_name,
            name,
            content_type: PDF_CONTENT_TYPE.to_string(),
            created_at,
            size: content.len() as u64,
            checksum: hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            transcript: doc_transcript,
        };

        self.changelog
            .send(EventType::Created, id, Some(&content), Some(&metadata))
            .await
            .with_context(|| format!("failed to publish changelog event for document {id}"))?;

        Ok(id)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Document>> {
        self.storage.get_document(id).await
    }

    async fn list(&self, offset: i64, limit: i64) -> Result<Vec<Document>> {
        self.storage.list_documents(offset, limit).await
    }

    async fn update_name(&self, metadata: Metadata, new_name: String) -> Result<()> {
        let id = metadata.id;
        let metadata = Metadata {
            name: new_name,
            ..metadata
        };

        self.changelog
            .send(EventType::UpdateMetadata, id, None, Some(&metadata))
            .await
            .with_context(|| format!("failed to publish changelog event for document {id}"))?;

        Ok(())
    }

    async fn sync(&self, callback: Box<dyn FnMut(ChangelogEvent) + Send>) -> Result<()> {
        self.changelog.consume(callback).await
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::Mutex;

    use tempfile::NamedTempFile;

    use super::super::storage::MockStorage;
    use super::*;

    /// A single `send` call recorded by [`RecordingChangelog`].
    type SentEvent = (EventType, Uuid, Option<Vec<u8>>, Option<Metadata>);

    /// A [`ChangelogService`] fake recording every event passed to
    /// [`ChangelogService::send`], and replaying `to_consume` through
    /// [`ChangelogService::consume`]'s callback, for tests that don't need
    /// a live server.
    #[derive(Default)]
    struct RecordingChangelog {
        sent: Mutex<Vec<SentEvent>>,
        to_consume: Mutex<Vec<ChangelogEvent>>,
    }

    #[async_trait]
    impl ChangelogService for RecordingChangelog {
        async fn send(
            &self,
            event_type: EventType,
            document_id: Uuid,
            content: Option<&[u8]>,
            metadata: Option<&Metadata>,
        ) -> Result<()> {
            self.sent.lock().unwrap().push((
                event_type,
                document_id,
                content.map(<[u8]>::to_vec),
                metadata.cloned(),
            ));
            Ok(())
        }

        async fn consume(&self, mut callback: Box<dyn FnMut(ChangelogEvent) + Send>) -> Result<()> {
            for event in self.to_consume.lock().unwrap().drain(..) {
                callback(event);
            }
            Ok(())
        }
    }

    /// Writes `content` to a temporary file with the given `extension`, for
    /// tests that need a real path to pass to [`Service::upload`].
    fn write_temp_file(extension: &str, content: &[u8]) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{extension}"))
            .tempfile()
            .unwrap();
        file.write_all(content).unwrap();
        file
    }

    #[tokio::test]
    async fn upload_publishes_a_created_event_for_the_document() {
        let pdf = super::super::transcript::tests::build_pdf("hello world");
        let file = write_temp_file("pdf", &pdf);

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(MockStorage::new(), changelog.clone());

        client.upload(file.path()).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, _id, content, metadata) = &sent[0];
        assert_eq!(*event_type, EventType::Created);
        assert_eq!(content.as_deref(), Some(pdf.as_slice()));
        assert_eq!(
            metadata
                .as_ref()
                .map(|metadata| metadata.transcript.as_str()),
            Some("hello world\n")
        );
        let file_stem = file.path().file_stem().unwrap().to_str().unwrap();
        let file_name = file.path().file_name().unwrap().to_str().unwrap();
        assert_eq!(
            metadata.as_ref().map(|metadata| metadata.name.as_str()),
            Some(file_stem)
        );
        assert_eq!(
            metadata
                .as_ref()
                .map(|metadata| metadata.original_name.as_str()),
            Some(file_name)
        );
    }

    #[tokio::test]
    async fn upload_rejects_non_pdf_extensions() {
        let file = write_temp_file("txt", b"hello world");
        let client =
            DocumentsClient::new(MockStorage::new(), Arc::new(RecordingChangelog::default()));

        let result = client.upload(file.path()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_returns_a_document_previously_cached_locally() {
        let id = Uuid::now_v7();
        let document = Document {
            id,
            content: b"hello".to_vec(),
            metadata: Metadata {
                id,
                name: "report.pdf".to_string(),
                original_name: "report.pdf".to_string(),
                content_type: PDF_CONTENT_TYPE.to_string(),
                created_at: 1_700_000_000,
                size: 5,
                checksum: "deadbeef".to_string(),
                transcript: String::new(),
            },
        };

        let mut storage = MockStorage::new();
        let expected = document.clone();
        storage
            .expect_get_document()
            .withf(move |queried_id| *queried_id == id)
            .times(1)
            .returning(move |_| Ok(Some(expected.clone())));

        let client = DocumentsClient::new(storage, Arc::new(RecordingChangelog::default()));

        assert_eq!(client.get(id).await.unwrap(), Some(document));
    }

    #[tokio::test]
    async fn list_pages_through_documents_previously_cached_locally() {
        let (id_two, id_three) = (Uuid::now_v7(), Uuid::now_v7());
        let page = vec![
            Document {
                id: id_two,
                content: Vec::new(),
                metadata: Metadata {
                    id: id_two,
                    name: "two".to_string(),
                    original_name: "two".to_string(),
                    content_type: PDF_CONTENT_TYPE.to_string(),
                    created_at: 1_700_000_001,
                    size: 0,
                    checksum: String::new(),
                    transcript: String::new(),
                },
            },
            Document {
                id: id_three,
                content: Vec::new(),
                metadata: Metadata {
                    id: id_three,
                    name: "three".to_string(),
                    original_name: "three".to_string(),
                    content_type: PDF_CONTENT_TYPE.to_string(),
                    created_at: 1_700_000_002,
                    size: 0,
                    checksum: String::new(),
                    transcript: String::new(),
                },
            },
        ];

        let mut storage = MockStorage::new();
        let expected_page = page.clone();
        storage
            .expect_list_documents()
            .withf(|offset, limit| *offset == 1 && *limit == 2)
            .times(1)
            .returning(move |_, _| Ok(expected_page.clone()));

        let client = DocumentsClient::new(storage, Arc::new(RecordingChangelog::default()));

        let page = client.list(1, 2).await.unwrap();

        assert_eq!(
            page.into_iter()
                .map(|document| document.metadata.name)
                .collect::<Vec<_>>(),
            vec!["two", "three"]
        );
    }

    #[tokio::test]
    async fn update_name_publishes_an_update_metadata_event_with_the_new_name() {
        let id = Uuid::now_v7();
        let metadata = Metadata {
            id,
            name: "old.pdf".to_string(),
            original_name: "old.pdf".to_string(),
            content_type: PDF_CONTENT_TYPE.to_string(),
            created_at: 1_700_000_000,
            size: 5,
            checksum: "deadbeef".to_string(),
            transcript: String::new(),
        };

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(MockStorage::new(), changelog.clone());

        client
            .update_name(metadata, "new.pdf".to_string())
            .await
            .unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, document_id, content, metadata) = &sent[0];
        assert_eq!(*event_type, EventType::UpdateMetadata);
        assert_eq!(*document_id, id);
        assert_eq!(*content, None);
        let metadata = metadata.as_ref().unwrap();
        assert_eq!(metadata.name, "new.pdf");
        assert_eq!(metadata.original_name, "old.pdf");
        assert_eq!(metadata.checksum, "deadbeef");
    }

    #[tokio::test]
    async fn sync_delegates_to_the_changelog_services_consume() {
        let event = super::super::FakeChangelogEvent::new().build();

        let changelog = Arc::new(RecordingChangelog {
            to_consume: Mutex::new(vec![event.clone()]),
            ..Default::default()
        });
        let client = DocumentsClient::new(MockStorage::new(), changelog);

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        client
            .sync(Box::new(move |event| {
                received_in_callback.lock().unwrap().push(event)
            }))
            .await
            .unwrap();

        assert_eq!(*received.lock().unwrap(), vec![event]);
    }
}
